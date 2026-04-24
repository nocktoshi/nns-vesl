::  nns — .nock name registrar kernel.
::
::  Pattern: data-registry (direct kernel state) + Vesl graft for
::  settlement. This is the shape used by
::  ~/vesl/templates/data-registry/hoon/app/app.hoon,
::  generalized with the Vesl graft wired in so on-demand settlement
::  proofs are one poke away.
::
::  One address can own any number of names: the `names` map does
::  not constrain owner uniqueness. A separate `primaries` map
::  designates each owner's reverse-lookup target — the name that
::  `GET /resolve?address=<x>` returns.
::
::  Settlement model:
::
::    A hull is an immutable commitment (see vesl-graft: "A given
::    hull-id can hold exactly one root, forever."). The registry
::    state is mutable (names get added). We reconcile these with
::    a claim-count counter: every successful %claim bumps `claim-count`,
::    recomputes the Merkle root over the entire `names` map, and
::    registers a fresh hull with id `hull-for(claim-count)`.  The graft's
::    `registered` map thus becomes an append-only history of
::    claim-count -> root commitments. Any past commitment is still
::    independently settleable as long as the caller still has the
::    leaf and proof from that claim-count.
::
::    Settlement is batched: %settle-batch selects every name claimed
::    since `last-settled-claim-id` (via the per-entry claim-count tag),
::    builds one Merkle-inclusion payload covering all of them, and
::    pokes the graft with a SINGLE %vesl-settle. The graft records one
::    note whose id is a hash of the sorted batch contents, so replay
::    protection is at the batch level.
::
::  Split of authority:
::
::    - names=(map @t [owner=@t tx-hash=@t claim-count=@ud])
::        authoritative registry (name -> {owner, paying tx-hash,
::        claim-count-at-which-added}). %claim writes it; name-uniqueness
::        is enforced here. There is no constraint that a given owner
::        appears only once — one address can own many names. The
::        per-entry `claim-count` is kernel-local bookkeeping only; it
::        is NOT part of the Merkle leaf content.
::    - tx-hashes=(set @t)
::        secondary index of payment tx-hashes that have been used
::        to claim a name. %claim enforces tx-hash uniqueness here,
::        so a single payment can only ever produce one registration.
::    - primaries=(map @t @t)
::        reverse-lookup index (owner-address -> the single name
::        that address wants to resolve to). Written by %claim on
::        first-claim-per-address, and by %set-primary thereafter.
::        Uniqueness is in the map's key: one primary per address.
::    - claim-count=@ud
::        monotonic counter, bumped on every successful %claim.
::        Hull ids are derived from it via `hull-for`, so re-using
::        a hull is structurally impossible as long as we never
::        roll back `claim-count`.
::    - last-settled-claim-id=@ud
::        monotonic counter tracking the highest `claim-count` that has
::        been packaged into a settled batch. `%settle-batch` selects
::        `{entry | entry.claim-count > last-settled-claim-id}` and, on
::        success, advances this to the current `claim-count`. Invariant:
::        `last-settled-claim-id <= claim-count`.
::    - root=@
::        cached Merkle root over `names` at the current `claim-count`.
::        Re-computed on %claim (O(n)); peeks read it in O(1).
::    - hull=@
::        cached hull-id for the current `claim-count`
::        (= `(hull-for claim-count)`). Cached for symmetry with `root`.
::    - vesl=vesl-state
::        graft bookkeeping. `registered` gets one entry per claim
::        (the append-only commitment history); `settled` gets one
::        entry per successful %vesl-settle (one per batch).
::
::  What the STARK-provable gate (nns-gate) enforces on %vesl-settle:
::
::    G1. Valid name format (lowercase/digit stem + .nock suffix) for
::        every leaf in the batch.
::    G2. Merkle inclusion: for every leaf, `jam([name owner tx-hash])`
::        is committed by `expected-root` via that leaf's proof path.
::        This binds a settlement to a specific set of (name, owner,
::        tx-hash) triples at a specific committed registry snapshot.
::
::  Hot-path domain rules enforced by %claim (same rules as G1 plus
::  uniqueness):
::
::    C1. Valid format (== G1). Crash on violation — honest hulls
::        never send malformed names.
::    C2. Fee tier: declared fee >= fee-for(name). Crash on
::        violation (same reason).
::    C3. Name must not already be in `names`. Duplicate emits
::        [%claim-error 'name already registered'] and does not
::        mutate state.
::    C4. Paying tx-hash must not already be in `tx-hashes`.
::        Duplicate emits [%claim-error 'payment already used']
::        and does not mutate state.
::    (On success, if the owner has no primary yet, the newly
::     claimed name becomes their primary — %claim also emits
::     [%primary-set owner name] alongside [%claimed ...].)
::    (On success, the kernel also auto-registers a fresh hull:
::     emits [%claim-count-bumped claim-count hull root] and passes through
::     the graft's [%vesl-registered hull root]. The caller can
::     use those plus `peek /proof/<name>` to build a settle
::     payload any time.)
::
::  %set-primary rules:
::
::    P1. The target `name` must exist in `names`.
::    P2. `names[name].owner` must equal the caller's declared
::        `address`. No one but the owner can designate which of
::        their names is primary.
::    (Violations emit [%primary-error <msg>] without mutating.
::     %set-primary does NOT bump claim-count: the `primaries` map is
::     not part of the committed Merkle tree, only `names` is.)
::
::  Compile: hoonc --new hoon/app/app.hoon hoon/
::
/+  *vesl-graft
/+  *vesl-merkle
/+  vp=vesl-prover
/+  vv=vesl-verifier
/+  np=nns-predicates
/=  sp  /common/stark/prover
/=  nv  /common/nock-verifier
/=  four  /common/ztd/four
/=  *  /common/zoon
/=  *  /common/wrapper
::  nockup:imports
::  NOTE: this marker lets vesl-nockup's `graft-inject` tool locate
::  the vesl import block. We already wire vesl manually above
::  (`*vesl-graft`, `*vesl-merkle`, `vp=vesl-prover`, `vv=vesl-verifier`),
::  so `graft-inject` is idempotent here — it sees the vesl imports
::  already in place and skips injection. See `docs/ROADMAP.md` for
::  the nockup-adoption evaluation.
::
=>
|%
+$  name-entry  [owner=@t tx-hash=@t claim-count=@ud]
::
::  +$anchor-header: minimal header triple sufficient for parent-chain
::  verification. The full Nockchain page header carries a `proof:sp`,
::  tx-ids z-set, coinbase split, etc. — none of which the kernel needs
::  at Phase 2. We only need enough to walk parent pointers and commit
::  to a specific block-id at a specific height for Phase 3's STARK.
::
+$  anchor-header
  $:  digest=@ux     :: Tip5 hash of this header
      height=@ud    :: page-number
      parent=@ux    :: Tip5 hash of parent header (anchor-tip of genesis is 0)
  ==
::
::  +$anchored-chain: kernel's view of the Nockchain header chain. The
::  follower advances this via `%advance-tip`; the Phase 3 recursive
::  `nns-gate` will bind each batched claim to a block-id that must
::  appear in `recent-headers` (or chain-link to `tip-digest`).
::
+$  anchored-chain
  $:  tip-digest=@ux                 :: follower-advanced canonical tip (0 = uninitialised)
      tip-height=@ud                 :: page-number of tip
      recent-headers=(list anchor-header)  :: bounded deque, newest first
  ==
::
+$  versioned-state
  $:  %v2
      vesl=vesl-state
      names=(map @t name-entry)
      tx-hashes=(set @t)
      primaries=(map @t @t)
      claim-count=@ud
      last-settled-claim-id=@ud
      root=@
      hull=@
      ::  Cached (subject, formula) for the most recent %prove-batch.
      ::  Phase 1-redo uses this so %verify-stark can replay the exact
      ::  inputs the prover traced (`batch` contents may change between
      ::  prove and verify). `~` when no batch has been proved yet.
      ::
      last-proved=(unit [subject=@ formula=*])
      ::  Phase 2: follower-advanced anchor + frozen payment address.
      ::
      anchor=anchored-chain
      ::  Payment address (base58 Nockchain address the C5 predicate
      ::  will require payments to land on). `~` before bootstrap. Once
      ::  set and a claim has been accepted (`claim-count > 0`), this
      ::  is frozen — further %set-payment-address pokes emit an error.
      ::
      payment-address=(unit @t)
      ::  nockup:state
      ::  graft-inject would add `vesl=vesl-state` here on a fresh
      ::  kernel. Already present above; marker is idempotent.
      ::
  ==
::
::  Max number of headers kept in `recent-headers`. The kernel stores
::  only a sliding window — older headers are implicit in the chain
::  commitment and don't need to be rebound to new tips.
::
++  max-anchor-headers  ^~((bex 10))
::
+$  effect  *
::
+$  cause
  $%  [%claim name=@t owner=@t fee=@ud tx-hash=@t]
      [%set-primary address=@t name=@t]
      [%settle-batch ~]
      [%prove-batch ~]
      ::  Phase 1-redo: cue JAM and run `verify:nv` (same jets as
      ::  on-chain block PoW STARK verification). Read-only; for
      ::  benchmarking recursion cost — verify is not inside the
      ::  fink-traced `prove-computation` subject.
      ::
      ::  Use `*` (not `@`) so `soft` accepts large JAM atoms; cast
      ::  before `cue`.
      ::
      [%verify-stark blob=*]
      ::  Phase 1-redo sanity: prove `[42 [0 1]]` (identity) with
      ::  vesl-prover, then verify it with vesl-stark-verifier. Uses
      ::  the exact same shape as vesl/protocol/tests/prove-verify.hoon
      ::  so we can confirm prover<->verifier compatibility independent
      ::  of our batch-specific subject/formula.
      ::
      [%prove-identity ~]
      ::  Phase 2: follower advances the anchored-chain tip. `headers`
      ::  is ordered oldest-first; each must chain to the previous one
      ::  by parent pointer and the first must chain to the current
      ::  `tip-digest` (or be `parent=0` when bootstrapping from genesis).
      ::  Fails hard on any inconsistency — indicates a reorg the
      ::  follower did not replay cleanly, which would compromise
      ::  Phase 3's chain-linkage claim.
      ::
      [%advance-tip headers=(list anchor-header)]
      ::  Phase 2: one-shot (until `claim-count > 0`) payment address
      ::  setter. The C5 payment predicate in Phase 3 will require
      ::  claim payments to land on an output locked to this address's
      ::  pkh. Freezing after the first accepted claim prevents a
      ::  kernel operator from moving the payment target retroactively.
      ::
      [%set-payment-address address=@t]
      ::  Phase 3 Level A: exercise `chain-links-to:nns-predicates`
      ::  without going through %claim. Read-only — the cause does not
      ::  mutate state, it just runs the predicate and emits the
      ::  result. Used by tests + ops tooling to verify a claim's
      ::  header chain resolves to the kernel's anchored tip before
      ::  issuing an expensive %claim poke.
      ::
      [%verify-chain-link claim-digest=@ux headers=(list anchor-header) anchored-tip=@ux]
      ::  Phase 3 Level B: drive `has-tx-in-page:nns-predicates`.
      ::  Read-only; emits `[%tx-in-page-result ok=?]` iff
      ::  `claimed-tx-id ∈ page.tx-ids`. Takes a flat list of tx-ids
      ::  so the kernel can build the canonical `(z-set @ux)` via
      ::  `z-silt` — z-in's `has` uses `gor-tip` (Tip5) ordering for
      ::  BST descent, so the caller cannot hand us a tree directly.
      ::  The page summary is hull-provided today (Phase 2c
      ::  `fetch_page_for_tx`); Level C will recompute its
      ::  block-commitment from the full page noun so the hull can't
      ::  lie about `tx-ids`.
      ::
      [%verify-tx-in-page digest=@ux tx-ids=(list @ux) claimed-tx-id=@ux]
      ::  nockup:cause
      ::  graft-inject would add `vesl-cause` here on a fresh
      ::  kernel. Already present below; marker is idempotent.
      ::
      vesl-cause
  ==
::
::  --- domain predicates shared by %claim and nns-gate ---
::
::  +valid-char: lowercase letter (a-z) or ascii digit (0-9).
::
++  valid-char
  |=  c=@
  ^-  ?
  ?|  &((gte c 'a') (lte c 'z'))
      &((gte c '0') (lte c '9'))
  ==
::
::  +all-valid-chars: every byte of the cord satisfies valid-char.
::
++  all-valid-chars
  |=  cord=@t
  ^-  ?
  =/  n  (met 3 cord)
  =/  i=@  0
  |-
  ?:  =(i n)  %.y
  ?.  (valid-char (cut 3 [i 1] cord))  %.n
  $(i +(i))
::
::  +has-nock-suffix: cord ends in the literal bytes ".nock".
::
++  has-nock-suffix
  |=  cord=@t
  ^-  ?
  =/  n  (met 3 cord)
  ?:  (lth n 6)  %.n
  =((cut 3 [(sub n 5) 5] cord) '.nock')
::
::  +stem-len: length of the cord's stem (before ".nock").
::
++  stem-len
  |=  cord=@t
  ^-  @ud
  (sub (met 3 cord) 5)
::
::  +is-valid-name: G1 — format check.
::
++  is-valid-name
  |=  name=@t
  ^-  ?
  ?.  (has-nock-suffix name)  %.n
  =/  slen  (stem-len name)
  ?:  =(slen 0)  %.n
  (all-valid-chars (cut 3 [0 slen] name))
::
::  +fee-for: fee tiers in nicks, ported from the legacy worker
::  (src/utils/constants.ts).
::
::    stem len >= 10    -> 100
::    stem len 5..=9    -> 500
::    stem len 1..=4    -> 5000
::    empty (rejected by is-valid-name first) -> 0
::
++  fee-for
  |=  name=@t
  ^-  @ud
  =/  slen  (stem-len name)
  ?:  =(slen 0)  0
  ?:  (gte slen 10)  100
  ?:  (gte slen 5)   500
  5.000
::
::  --- Merkle tree primitives (duplicate-last convention, matches
::      Rust's nockchain-tip5-rs::MerkleTree) ---
::
::  +leaf-chunk: canonical leaf atom for a single registry row.
::  Jamming the triple is a deterministic encoding: the same
::  (name, owner, tx-hash) always produces the same atom, and any
::  drift anywhere in the triple produces a different leaf hash.
::
++  leaf-chunk
  |=  [name=@t e=name-entry]
  ^-  @
  (jam [name owner.e tx-hash.e])
::
::  +sorted-leaves: all leaf chunks in canonical order.
::  Sort keys (names) with `aor` so the tree shape is a pure
::  function of `names` — independent of insertion order, which
::  is crucial for reproducible Merkle roots across nodes.
::
++  sorted-leaves
  |=  nm=(map @t name-entry)
  ^-  (list @)
  =/  keys=(list @t)  (sort ~(tap in ~(key by nm)) aor)
  %+  turn  keys
  |=  k=@t
  (leaf-chunk k (~(got by nm) k))
::
::  +next-level: reduce one Merkle level. Odd input: duplicate
::  the last element so pairing closes cleanly. Matches
::  nockchain-tip5-rs::MerkleTree::build — do not deviate.
::
++  next-level
  |=  level=(list @)
  ^-  (list @)
  ?~  level  ~
  ?~  t.level
    ~[(hash-pair i.level i.level)]
  [(hash-pair i.level i.t.level) $(level t.t.level)]
::
::  +compute-root: Merkle root over an already-canonicalized leaf
::  list. Hashes each chunk with `hash-leaf` at level 0, then
::  collapses via `next-level` until a single element remains.
::  Empty registry: root = 0.
::
++  compute-root
  |=  leaves=(list @)
  ^-  @
  ?~  leaves  0
  =/  level  (turn leaves hash-leaf)
  |-  ^-  @
  ?:  ?=([@ ~] level)  i.level
  $(level (next-level level))
::
::  +proof-for: Merkle inclusion proof for leaf at index `idx`
::  (into the sorted leaf list). Side convention mirrors
::  Rust's MerkleTree::proof:
::
::    even idx -> sibling on RIGHT -> side=%.n (false)
::    odd  idx -> sibling on LEFT  -> side=%.y (true)
::
::  When the sibling would run past the level length the current
::  element duplicates into the sibling slot — same padding
::  behavior as `next-level` applies during root construction.
::
::  +nth: element at index `i` in `lst`. Crashes on out-of-bounds
::  so we don't silently return a wrong proof node — callers
::  already range-check.
::
++  nth
  |=  [lst=(list @) i=@ud]
  ^-  @
  ?~  lst  ~|('nth: out of bounds' !!)
  ?:  =(i 0)  i.lst
  $(lst t.lst, i (dec i))
::
++  proof-for
  |=  [leaves=(list @) idx=@ud]
  ^-  (list [hash=@ side=?])
  =/  level=(list @)  (turn leaves hash-leaf)
  =|  acc=(list [hash=@ side=?])
  =/  i=@ud  idx
  |-  ^-  (list [hash=@ side=?])
  ?:  ?=([@ ~] level)  (flop acc)
  =/  n=@ud  (lent level)
  =/  sibling-idx=@ud
    ?:  =(0 (mod i 2))  +(i)
    (sub i 1)
  =/  sib=@
    ?:  (lth sibling-idx n)  (nth level sibling-idx)
    (nth level i)
  =/  side=?  =(1 (mod i 2))
  %=  $
    level  (next-level level)
    i      (div i 2)
    acc    [[sib side] acc]
  ==
::
::  +index-of: sorted-position of `name` in `names`. Returns
::  `~` if the name is absent.
::
++  index-of
  |=  [nm=(map @t name-entry) name=@t]
  ^-  (unit @ud)
  =/  keys=(list @t)  (sort ~(tap in ~(key by nm)) aor)
  =|  i=@ud
  |-  ^-  (unit @ud)
  ?~  keys  ~
  ?:  =(name i.keys)  `i
  $(keys t.keys, i +(i))
::
::  +hull-for: hull-id for a given claim-count.
::
::    hull(claim-count) = hash-pair(hash-leaf('nns'), hash-leaf(claim-count))
::
::  Monotonic `claim-count` guarantees structural uniqueness: we can
::  never re-register the same hull-id twice, so the graft's
::  `%vesl-error 'hull already registered'` branch is
::  unreachable on an honest kernel.
::
++  hull-for
  |=  id=@ud
  ^-  @
  (hash-pair (hash-leaf 'nns') (hash-leaf id))
::
::  +nns-gate: verification gate for %vesl-settle / %vesl-verify.
::
::    data:          (list [name=@t owner=@t tx-hash=@t proof=(list proof-node)])
::    expected-root: Merkle root that every `proof` is claimed to cover
::
::  G1: every leaf's name has valid format.
::  G2: for every leaf, `jam [name owner tx-hash]` hashed as a leaf
::      and walked through `proof` equals `expected-root`.
::  G3: no duplicate `name` within this transition batch.
::  G4: no duplicate `tx-hash` within this transition batch.
::
::  The graft supplies `expected-root` from the registered hull
::  root, so a verified `nns-gate` invocation proves: "these
::  (name, owner, tx-hash) triples were all registry rows at the
::  commitment `expected-root`."  An empty leaves list is rejected
::  at the %settle-batch layer before it ever reaches the gate, but
::  the gate itself accepts the vacuous case (nothing to disprove)
::  so a direct %vesl-verify on an empty batch is a no-op success.
::  No payment checking here — that's on the hot path and payment
::  attestation is a separate concern (see README TODO).
::
++  nns-gate
  |=  [data=* expected-root=@]
  ^-  ?
  =/  leaves
    ;;((list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])]) data)
  =|  seen-names=(set @t)
  =|  seen-tx-hashes=(set @t)
  |-  ^-  ?
  ?~  leaves  %.y
  =/  chunk=@  (jam [name.i.leaves owner.i.leaves tx-hash.i.leaves])
  ?&  (is-valid-name name.i.leaves)
      !(~(has in seen-names) name.i.leaves)
      !(~(has in seen-tx-hashes) tx-hash.i.leaves)
      (verify-chunk chunk proof.i.leaves expected-root)
      %=  $
        leaves  t.leaves
        seen-names  (~(put in seen-names) name.i.leaves)
        seen-tx-hashes  (~(put in seen-tx-hashes) tx-hash.i.leaves)
      ==
  ==
--
|%
++  moat  (keep versioned-state)
::
++  inner
  |_  state=versioned-state
  ::
  ++  load
    |=  old-state=versioned-state
    ^-  _state
    old-state
  ::
  ::  +peek: registry + graft state
  ::
  ::    /owner/<name>      -> (unit name-entry)             {owner, tx-hash, claim-count}
  ::    /primary/<addr>    -> (unit @t)                     primary name
  ::    /entries           -> @ud                           total names
  ::    /claim-count       -> @ud                           current claim-count
  ::    /last-settled      -> @ud                           last-settled-claim-id
  ::    /hull              -> @                             current hull-id
  ::    /root              -> @                             current Merkle root
  ::    /snapshot          -> [claim-count=@ud hull=@ root=@] all three at once
  ::    /proof/<name>      -> (unit (list [hash=@ side=?])) proof or ~
  ::    /pending-batch     -> (list @t)                     names with
  ::                          entry.claim-count > last-settled-claim-id,
  ::                          sorted canonically by `aor`
  ::    /anchor            -> anchored-chain               follower-advanced
  ::                          chain tip + recent headers
  ::    /payment-address   -> (unit @t)                    configured payment
  ::                          address (~ before bootstrap)
  ::    [anything else]    -> vesl-peek  (registered / settled / root by hull)
  ::
  ++  peek
    |=  =path
    ^-  (unit (unit *))
    ?+  path  (vesl-peek vesl.state path)
        [%owner name=@t ~]
      =/  key  +<.path
      ``(~(get by names.state) key)
        ::
        [%primary addr=@t ~]
      =/  key  +<.path
      ``(~(get by primaries.state) key)
        ::
        [%entries ~]
      ``~(wyt by names.state)
        ::
        [%claim-count ~]
      ``claim-count.state
        ::
        [%last-settled ~]
      ``last-settled-claim-id.state
        ::
        [%hull ~]
      ``hull.state
        ::
        [%root ~]
      ``root.state
        ::
        [%snapshot ~]
      ``[claim-count=claim-count.state hull=hull.state root=root.state]
        ::
        [%proof name=@t ~]
      =/  key  +<.path
      ?~  (~(get by names.state) key)
        ``~
      =/  idx  (index-of names.state key)
      ?~  idx  ``~
      =/  leaves  (sorted-leaves names.state)
      ``(proof-for leaves u.idx)
        ::
        [%pending-batch ~]
      =/  keys=(list @t)  (sort ~(tap in ~(key by names.state)) aor)
      =/  cutoff=@ud  last-settled-claim-id.state
      =|  out=(list @t)
      |-  ^-  (unit (unit *))
      ?~  keys  ``(flop out)
      =/  e  (~(got by names.state) i.keys)
      ?:  (gth claim-count.e cutoff)
        $(keys t.keys, out [i.keys out])
      $(keys t.keys)
        ::
        [%anchor ~]
      ``anchor.state
        ::
        [%payment-address ~]
      ``payment-address.state
        ::
        ::  /fee-for-name/<name>  -> @ud
        ::
        ::  Phase 3 — shared fee-schedule peek. Identical table to the
        ::  Rust-side `payment::fee_for_name`; a cross-repo parity
        ::  test in `tests/phase3_predicates.rs` pins these in lockstep.
        ::  Exposed as a peek (not just an internal arm) so the gate
        ::  library and the hull both read from one source of truth.
        ::
        [%fee-for-name name=@t ~]
      =/  key=@t  +<.path
      ``(fee-for-name:np key)
        ::
        ::  nockup:peek
        ::  graft-inject would wire `(vesl-peek vesl.state path)` as
        ::  the `?+` default arm here on a fresh kernel. Already
        ::  wired above (the `?+  path  (vesl-peek vesl.state path)`
        ::  head); marker is idempotent.
        ::
    ==
  ::
  ++  poke
    |=  =ovum:moat
    ^-  [(list effect) _state]
    =/  act  ((soft cause) cause.input.ovum)
    ?~  act
      ~>  %slog.[3 'nns: invalid cause']
      [~ state]
    ?-  -.u.act
        ::
        ::  %claim: the hot path. Enforces C1..C4; writes
        ::  `names` and `tx-hashes`; bumps `claim-count` and
        ::  auto-registers a fresh hull in the graft.
        ::
        %claim
      =/  c  u.act
      ::  C1/C2 — format and fee: an honest hull never violates
      ::  these. If it does we crash (unprovable computation)
      ::  rather than silently accepting bad data.
      ?>  (is-valid-name name.c)
      ?>  (gte fee.c (fee-for name.c))
      ::  C3 — name uniqueness: a user-visible error; emit and
      ::  leave state untouched.
      ?:  (~(has by names.state) name.c)
        :_  state
        ~[[%claim-error 'name already registered']]
      ::  C4 — payment uniqueness: one tx-hash, one registration.
      ?:  (~(has in tx-hashes.state) tx-hash.c)
        :_  state
        ~[[%claim-error 'payment already used']]
      ::  Commit the new row. Each entry records the claim-count at
      ::  which it was added so %settle-batch can select "everything
      ::  since the last successful settle" without an auxiliary
      ::  index.
      =/  new-claim-count=@ud  +(claim-count.state)
      =/  entry=name-entry  [owner.c tx-hash.c new-claim-count]
      =.  names.state      (~(put by names.state) name.c entry)
      =.  tx-hashes.state  (~(put in tx-hashes.state) tx-hash.c)
      ::  Compute the fresh snapshot: Merkle root over the updated
      ::  `names`, hull-id derived from the new claim-count.
      =/  leaves=(list @)  (sorted-leaves names.state)
      =/  new-root=@       (compute-root leaves)
      =/  new-hull=@       (hull-for new-claim-count)
      ::  Register the fresh hull in the graft. Because `new-hull`
      ::  is a pure function of a strictly-monotonic `new-claim-count`,
      ::  it is structurally impossible for it to collide with a
      ::  previously-registered hull — if the graft ever returned
      ::  a %vesl-error here our claim-count bookkeeping is broken and
      ::  we crash rather than emit %claimed with an untracked
      ::  commitment.
      =^  reg-efx=(list vesl-effect)  vesl.state
        (vesl-poke vesl.state [%vesl-register new-hull new-root] nns-gate)
      ?>  ?=(^ reg-efx)
      ?>  ?=(%vesl-registered -.i.reg-efx)
      =.  claim-count.state  new-claim-count
      =.  root.state   new-root
      =.  hull.state   new-hull
      ::  Auto-assign primary on first claim for this owner.
      =/  first-claim=?  !(~(has by primaries.state) owner.c)
      =?  primaries.state  first-claim
        (~(put by primaries.state) owner.c name.c)
      =/  primary-efx=(list effect)
        ?:  first-claim
          ~[[%primary-set owner.c name.c]]
        ~
      :_  state
      ;:  weld
        `(list effect)`~[[%claimed name.c owner.c tx-hash.c]]
        primary-efx
        `(list effect)`~[[%claim-count-bumped new-claim-count new-hull new-root]]
        `(list effect)`reg-efx
      ==
      ::
        ::  %set-primary: owner-gated reverse-lookup update.
        ::  Enforces P1/P2; writes `primaries`. Does NOT bump
        ::  `claim-count` — `primaries` is not part of the committed
        ::  Merkle tree.
        ::
        ::  Sanity-check arm: prove `[42 [0 1]]` then verify. Emits
        ::  [%prove-identity-result ok=?] so the test can confirm the
        ::  prover/verifier round-trip works at all.
        ::
        %prove-identity
      =/  subj=*  42
      =/  form=*  [0 1]
      =/  res
        %-  mule  |.
        (prove-computation:vp subj form 1 1)
      ?.  ?=(%& -.res)
        :_  state
        ~[[%prove-identity-result %.n]]
      =/  pr  p.res
      ?.  ?=(%& -.pr)
        :_  state
        ~[[%prove-identity-result %.n]]
      =/  prf=proof:sp  p.pr
      ::  NB: Phase 1-redo finding — vesl-prover bypasses puzzle-nock
      ::  and standard `verify:nv` derives `[s f]` from puzzle-nock,
      ::  so this round-trip currently fails composition eval. The
      ::  matched verifier is `verify:vv` from vendored vesl-verifier,
      ::  but making it accept our proof requires further investigation
      ::  of stark-config injection. Tracked in the research memo.
      ::
      =/  ok=?  (verify:vv prf ~ 0 subj form)
      :_  state
      ~[[%prove-identity-result ok]]
      ::
        %verify-stark
      ?.  ?=(@ blob.u.act)
        :_  state
        ~[[%verify-stark-error 'blob-not-atom']]
      =/  jammy=@  blob.u.act
      =/  cue-res  (mule |.((cue jammy)))
      ?.  -.cue-res
        :_  state
        ~[[%verify-stark-error 'bad-jam']]
      =/  proof=proof:four  ;;(proof:four +.cue-res)
      ::  Replay the exact [s f] the prover traced. vesl-stark-verifier
      ::  takes them externally (bypasses puzzle-nock). We cache them
      ::  in last-proved on every successful %prove-batch.
      ::
      ?~  last-proved.state
        :_  state
        ~[[%verify-stark-error 'no-cached-sf']]
      =/  subject=@  subject.u.last-proved.state
      =/  formula=*  formula.u.last-proved.state
      =/  ok=?  (verify:vv proof ~ 0 subject formula)
      :_  state
      ~[[%verify-stark-result ok]]
      ::
        ::  %advance-tip: follower-advanced anchor update.
        ::
        ::  `headers` is an oldest-first list of anchor-header triples
        ::  that extend the current tip. Kernel enforces:
        ::
        ::    A1. Every header's parent pointer equals the previous
        ::        header's digest (or the pre-existing tip-digest for
        ::        the first header; or 0 when bootstrapping from an
        ::        uninitialised state).
        ::    A2. Heights are strictly increasing by 1. First header's
        ::        height must be tip-height+1 (or equal to the sole
        ::        header's height on bootstrap).
        ::
        ::  Violations emit [%anchor-error <msg>] without mutating
        ::  state. Empty `headers` is a no-op. The full list is
        ::  prepended onto `recent-headers` (newest first), capped at
        ::  max-anchor-headers — older entries drop off the back.
        ::
        %advance-tip
      =/  hs=(list anchor-header)  headers.u.act
      ?~  hs
        :_  state
        ~[[%anchor-error 'empty advance']]
      ::  Validate chain linkage against current tip (or bootstrap).
      ::
      =/  bootstrapping=?  =(0x0 tip-digest.anchor.state)
      =/  expected-parent=@ux
        ?:  bootstrapping
          `@ux`0
        tip-digest.anchor.state
      =/  expected-height=@ud
        ?:  bootstrapping
          height.i.hs
        +(tip-height.anchor.state)
      ?.  =(parent.i.hs expected-parent)
        :_  state
        ~[[%anchor-error 'first header parent mismatch']]
      ?.  =(height.i.hs expected-height)
        :_  state
        ~[[%anchor-error 'first header height off-by-one']]
      ::  Walk the rest: each header's parent must be the prior
      ::  header's digest; height must increment by 1.
      ::
      =/  ok=?
        =|  prev=anchor-header
        =.  prev  i.hs
        =/  rest=(list anchor-header)  t.hs
        |-  ^-  ?
        ?~  rest  %.y
        ?.  =(parent.i.rest digest.prev)  %.n
        ?.  =(height.i.rest +(height.prev))  %.n
        $(rest t.rest, prev i.rest)
      ?.  ok
        :_  state
        ~[[%anchor-error 'header chain mismatch']]
      ::  Prepend headers newest-first onto recent-headers, capped.
      ::
      =/  newest-first=(list anchor-header)  (flop hs)
      ?>  ?=(^ newest-first)
      =/  tip=anchor-header  i.newest-first
      =/  existing=(list anchor-header)  recent-headers.anchor.state
      =/  merged=(list anchor-header)  (weld newest-first existing)
      =/  capped=(list anchor-header)  (scag max-anchor-headers merged)
      =.  tip-digest.anchor.state  digest.tip
      =.  tip-height.anchor.state  height.tip
      =.  recent-headers.anchor.state  capped
      :_  state
      ^-  (list effect)
      ~[[%anchor-advanced digest.tip height.tip (lent hs)]]
      ::
        ::  %set-payment-address: one-shot (until first claim) address
        ::  setter. Freezing after `claim-count > 0` prevents the
        ::  operator from silently moving the payment target once
        ::  users have started paying in.
        ::
        %set-payment-address
      =/  addr=@t  address.u.act
      ?:  (gth claim-count.state 0)
        :_  state
        ~[[%payment-address-error 'already bound; cannot change after first claim']]
      =.  payment-address.state  `addr
      :_  state
      ^-  (list effect)
      ~[[%payment-address-set addr]]
      ::
        ::  %verify-chain-link: read-only Phase 3 Level A predicate
        ::  smoke test. Returns `[%chain-link-result ok=?]` without
        ::  mutating state.
        ::
        %verify-chain-link
      =/  ok=?
        %-  chain-links-to:np
        :*  claim-digest.u.act
            headers.u.act
            anchored-tip.u.act
        ==
      :_  state
      ^-  (list effect)
      ~[[%chain-link-result ok]]
      ::
        ::  %verify-tx-in-page: read-only Phase 3 Level B predicate
        ::  smoke test. Builds a canonical `(z-set @ux)` from the
        ::  provided tx-id list via `z-silt`, then runs
        ::  `has-tx-in-page:np`. Returns `[%tx-in-page-result ok=?]`
        ::  without mutating state.
        ::
        %verify-tx-in-page
      =/  tx-set=(z-set @ux)  (z-silt tx-ids.u.act)
      =/  pag=nns-page-summary:np  [digest.u.act tx-set]
      =/  ok=?  (has-tx-in-page:np pag claimed-tx-id.u.act)
      :_  state
      ^-  (list effect)
      ~[[%tx-in-page-result ok]]
      ::
        %set-primary
      =/  c  u.act
      =/  existing  (~(get by names.state) name.c)
      ::  P1 — name must exist.
      ?~  existing
        :_  state
        ~[[%primary-error 'name not registered']]
      ::  P2 — caller must own the name.
      ?.  =(owner.u.existing address.c)
        :_  state
        ~[[%primary-error 'not the owner']]
      =.  primaries.state  (~(put by primaries.state) address.c name.c)
      :_  state
      ~[[%primary-set address.c name.c]]
      ::
        ::  %settle-batch: bundle every name claimed since the last
        ::  successful settle into a single %vesl-settle poke. One
        ::  batch = one graft note = one note-id. Replay protection is
        ::  at the batch level: the exact same leaf set can't be
        ::  resettled, but the individual names can still be settled
        ::  as part of a future batch that contains different content.
        ::  Empty batches emit %batch-error instead of wasting a poke.
        ::
        %settle-batch
      =/  cutoff=@ud  last-settled-claim-id.state
      =/  all-keys=(list @t)
        (sort ~(tap in ~(key by names.state)) aor)
      =/  leaves=(list @)  (sorted-leaves names.state)
      =/  batch=(list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
        =|  acc=(list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
        =|  i=@ud
        =/  ks=(list @t)  all-keys
        |-  ^-  (list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
        ?~  ks  (flop acc)
        =/  e  (~(got by names.state) i.ks)
      ?:  (gth claim-count.e cutoff)
          =/  pf  (proof-for leaves i)
          $(ks t.ks, i +(i), acc [[i.ks owner.e tx-hash.e pf] acc])
        $(ks t.ks, i +(i))
      ?~  batch
        :_  state
        ~[[%batch-error 'nothing to settle']]
      ::  Deterministic batch id over the sorted batch contents. The
      ::  graft's `settled` set dedupes on this, so two callers racing
      ::  the same pending window can only produce one settled note.
      =/  note-id=@  (hash-leaf (jam batch))
      =/  jammed=@
        %-  jam
        :*  [note-id hull.state root.state [%pending ~]]
            batch
            root.state
        ==
      =^  efx=(list vesl-effect)  vesl.state
        (vesl-poke vesl.state [%vesl-settle jammed] nns-gate)
      ?>  ?=(^ efx)
      ?:  ?=(%vesl-settled -.i.efx)
        ::  Invariant: every %claim increments claim-count.state and
        ::  writes entry.claim-count = new claim-count, `names` is
        ::  append-only, and the batch is non-empty here — so the
        ::  highest entry.claim-count in the batch equals claim-count.state.
        =/  settled-at=@ud  claim-count.state
        =/  count=@ud  (lent batch)
        =.  last-settled-claim-id.state  settled-at
        :_  state
        ^-  (list effect)
        ;:  weld
          `(list effect)`~[[%batch-settled settled-at count note-id]]
          `(list effect)`efx
        ==
      ::  %vesl-error — pass through unchanged; state not mutated.
      :_  state
      ^-  (list effect)
      efx
      ::
        ::  %prove-batch: same shape as %settle-batch, but additionally
        ::  runs the STARK prover over the batch content. Emits a
        ::  [%batch-proof note-id proof] effect carrying the proof noun
        ::  on success, or [%prove-failed trace] on crash (proving
        ::  fails closed: settlement is not applied in that case).
        ::
        ::  Baseline implementation for Phase 0 — it produces a real
        ::  STARK over a canonical Nock computation derived from the
        ::  batch content. The computation itself is the forge-template
        ::  "64 nested Nock-4 increments over belt-digest" pattern; it
        ::  does NOT yet re-run the gate's C1-C4 predicates inside the
        ::  STARK (that is Phase 3 of the payment plan). Phase 0's
        ::  goal is to have a real STARK artifact flowing end-to-end.
        ::
        %prove-batch
      =/  cutoff=@ud  last-settled-claim-id.state
      =/  all-keys=(list @t)
        (sort ~(tap in ~(key by names.state)) aor)
      =/  leaves=(list @)  (sorted-leaves names.state)
      =/  batch=(list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
        =|  acc=(list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
        =|  i=@ud
        =/  ks=(list @t)  all-keys
        |-  ^-  (list [name=@t owner=@t tx-hash=@t proof=(list [hash=@ side=?])])
        ?~  ks  (flop acc)
        =/  e  (~(got by names.state) i.ks)
      ?:  (gth claim-count.e cutoff)
          =/  pf  (proof-for leaves i)
          $(ks t.ks, i +(i), acc [[i.ks owner.e tx-hash.e pf] acc])
        $(ks t.ks, i +(i))
      ?~  batch
        :_  state
        ~[[%batch-error 'nothing to prove']]
      =/  note-id=@  (hash-leaf (jam batch))
      ::  Fold every byte of the jammed batch into a single
      ::  Goldilocks-field belt-digest. This is the subject of the
      ::  STARK computation.
      ::
      =/  batch-bytes=@  (jam batch)
      =/  belt-digest=@
        =/  belts=(list @)  (split-to-belts batch-bytes)
        =/  p=@  (add (sub (bex 64) (bex 32)) 1)
        %+  roll  belts
        |=  [a=@ b=@]
        (mod (add a b) p)
      ::  Deterministic Nock formula: 64 nested [4 f] increments.
      ::
      =/  fs-formula=*
        =/  f=*  [0 1]
        =|  i=@
        |-
        ?:  =(i 64)  f
        $(f [4 f], i +(i))
      =/  proof-attempt
        %-  mule  |.
        (prove-computation:vp belt-digest fs-formula root.state hull.state)
      ?.  ?=(%& -.proof-attempt)
        ::  Prover crashed — settlement NOT applied.
        :_  state
        ^-  (list effect)
        ~[[%prove-failed (jam p.proof-attempt)]]
      ::  The outer mule succeeded, but prove-computation itself can
      ::  return `[%| err]` (e.g. %too-big heights). Unwrap both layers
      ::  and require the inner `%&` success to emit a usable proof.
      ::
      =/  pr  p.proof-attempt
      ?.  ?=(%& -.pr)
        :_  state
        ^-  (list effect)
        ~[[%prove-failed (jam p.pr)]]
      =/  the-proof=proof:sp  p.pr
      ::  Proof generated. Still fire the regular %vesl-settle so the
      ::  graft's `settled` map advances; package both the settlement
      ::  effect and the proof.
      ::
      =/  jammed=@
        %-  jam
        :*  [note-id hull.state root.state [%pending ~]]
            batch
            root.state
        ==
      =^  efx=(list vesl-effect)  vesl.state
        (vesl-poke vesl.state [%vesl-settle jammed] nns-gate)
      ?>  ?=(^ efx)
      ?:  ?=(%vesl-settled -.i.efx)
        =/  settled-at=@ud  claim-count.state
        =/  count=@ud  (lent batch)
        =.  last-settled-claim-id.state  settled-at
        =.  last-proved.state  `[belt-digest fs-formula]
        :_  state
        ^-  (list effect)
        ;:  weld
          `(list effect)`~[[%batch-settled settled-at count note-id]]
          `(list effect)`~[[%batch-proof note-id the-proof]]
          `(list effect)`efx
        ==
      :_  state
      ^-  (list effect)
      efx
      ::
        ::  vesl-cause tags — delegate to the graft with nns-gate.
        ::  %vesl-register is normally driven by %claim above; a
        ::  direct poke is kept for tests / manual re-registration
        ::  of historical roots.
        ::
        %vesl-register
      =^  efx=(list vesl-effect)  vesl.state
        (vesl-poke vesl.state u.act nns-gate)
      :_  state
      ^-  (list effect)
      efx
      ::
        %vesl-verify
      =^  efx=(list vesl-effect)  vesl.state
        (vesl-poke vesl.state u.act nns-gate)
      :_  state
      ^-  (list effect)
      efx
      ::
        %vesl-settle
      =^  efx=(list vesl-effect)  vesl.state
        (vesl-poke vesl.state u.act nns-gate)
      :_  state
      ^-  (list effect)
      efx
      ::
      ::  nockup:poke
      ::  graft-inject would add the three `%vesl-register` /
      ::  `%vesl-verify` / `%vesl-settle` arms here on a fresh
      ::  kernel. Already present above; marker is idempotent.
      ::
    ==
  --
--
((moat |) inner)
