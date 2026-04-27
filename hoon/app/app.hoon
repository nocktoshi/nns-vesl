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
/+  na=nns-accumulator
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
::  +$anchored-chain: kernel's view of the Nockchain header chain,
::  trimmed to the minimum a zkRollup-style design needs.
::
::  We store ONLY the current follower-anchored tip. Per-claim chain
::  linkage is proved by the gate against headers carried in the
::  claim-note's `ClaimChainBundle.header_chain_jam` (Phase 2d) — the
::  kernel is not a Nockchain-replica and does not cache the
::  canonical chain.
::
::  Analog: Optimism stores a state root on L1, not L1's headers. The
::  wallet independently trusts Nockchain (for UTXOs anyway); all we
::  need to commit to is "this is the Nockchain tip NNS claims anchor
::  to", and the STARK attests to parent-chain linkage up to it.
::
+$  anchored-chain
  $:  tip-digest=@ux    :: follower-advanced canonical tip (0 = uninitialised)
      tip-height=@ud    :: page-number of tip
  ==
::
::  +$v0-state: Path Y prerelease kernel — z-map accumulator + chain-scan
::  cursor.  Tag `%v0` is the on-disk / jam identity for this shape; it
::  does not refer to the old HTTP-era names map (that stack is gone).
::
+$  v0-state
  $:  %v0
      accumulator=nns-accumulator:na
      last-proved-height=@ud
      last-proved-digest=@ux
      vesl=vesl-state
      ::  Cached (subject, formula) for the most recent successful
      ::  `prove-computation` (%prove-arbitrary, %prove-claim-in-stark,
      ::  %prove-recursive-step). `~` until first prove.
      ::
      last-proved=(unit [subject=* formula=*])
  ==
::
::
+$  effect  *
::
+$  cause
  $%  ::  Phase 1-redo: cue JAM and run `verify:nv` (same jets as
      ::  on-chain block PoW STARK verification). Read-only; for
      ::  benchmarking recursion cost — verify is not inside the
      ::  fink-traced `prove-computation` subject.
      ::
      ::  Use `*` (not `@`) so `soft` accepts large JAM atoms; cast
      ::  before `cue`.
      ::
      [%verify-stark blob=*]
      ::  Path Y4 / wallet offline: cue proof plus caller-supplied
      ::  `subject-jam` and `formula-jam` atoms (raw JAM bytes of the
      ::  traced nouns), then `verify:vesl-stark-verifier` — same math as
      ::  `%verify-stark` but does not read `last-proved.state`. Read-only.
      ::
      [%verify-stark-explicit blob=* subject-jam=* formula-jam=*]
      ::  Path Y4: offline z-map membership. Cues `acc-jam` into an
      ::  `nns-accumulator`, checks `root-atom` matches `expected-root`,
      ::  and that `(get acc name)` is exactly `entry`. Read-only.
      ::
      $:  %verify-accumulator-snapshot
          expected-root=@
          acc-jam=@
          name=@t
          owner=@t
          tx-hash=@ux
          claim-height=@ud
          block-digest=@ux
      ==
      ::  Phase 1-redo sanity: prove `[42 [0 1]]` (identity) with
      ::  vesl-prover, then verify it with vesl-stark-verifier. Uses
      ::  the exact same shape as vesl/protocol/tests/prove-verify.hoon
      ::  so we can confirm prover<->verifier compatibility independent
      ::  of our batch-specific subject/formula.
      ::
      [%prove-identity ~]
      ::  Path Y2: ingest one Nockchain block worth of `nns/v1/claim`
      ::  candidates. Verifies `parent` links to `last-proved-digest`,
      ::  `height` is strictly the successor of `last-proved-height`,
      ::  then folds valid candidates into the accumulator via
      ::  `+claim-scanner:np`. On success advances the scan cursor to
      ::  this block's digest and emits `[%scan-block-done ...]`.
      ::
      $:  %scan-block
          parent=@ux
          height=@ud
          page-digest=@ux
          page-tx-ids=(list @ux)
          candidates=(list nns-claim-candidate:np)
      ==
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
      ::  Phase 3c: compose all Level A + Level B + G1/C2 predicates
      ::  into one bundled validation call. Read-only — the cause does
      ::  not mutate state. Emits `[%validate-claim-ok]` on success or
      ::  `[%validate-claim-error <tag>]` on the first failing
      ::  predicate. The hull uses this pre-%claim to give users an
      ::  early rejection + structured error tag before committing a
      ::  claim that would only be rejected during chain replay.
      ::
      ::  `tx-ids` is a flat list (kernel canonicalises into a
      ::  `(z-set @ux)` via `z-silt` — see `%verify-tx-in-page` for
      ::  why the caller can't ship a tree directly).
      ::
      $:  %validate-claim
          name=@t
          owner=@t
          fee=@ud
          tx-hash=@ux
          claim-block-digest=@ux
          anchor-headers=(list anchor-header)
          page-digest=@ux
          page-tx-ids=(list @ux)
          anchored-tip=@ux
          anchored-tip-height=@ud
          witness-tx-id=@ux
          witness-spender-pkh=@
          witness-treasury-amount=@ud
          witness-output-lock-root=@t    :: v1 output lock root b58 (note_name)
      ==
      ::  Phase 3c step 2: validated proof of a single claim.
      ::
      ::  Same payload shape as %validate-claim. Kernel runs the
      ::  full validator first; on pass, produces a STARK committing
      ::  to `belt-digest(jam(bundle))` under the current (root, hull)
      ::  registry snapshot. On validator rejection, emits the usual
      ::  `%validate-claim-error <tag>` — no proof is produced.
      ::
      ::  What the STARK attests:
      ::    - a kernel whose registry is committed at (root, hull)
      ::    - asserted the bundle-hash at that snapshot.
      ::
      ::  What the wallet still verifies (out of the STARK):
      ::    - validator passes on the received bundle.
      ::    - bundle-hash matches the STARK's committed hash.
      ::    - (root, hull) match the expected registry anchor.
      ::  This is option-B recursion: the STARK commits to a hashed
      ::  witness, the wallet re-checks the witness's validity. A
      ::  future follow-up (Level C + Phase 3c step 3) will embed the
      ::  validator execution INSIDE the STARK so the wallet needs
      ::  only to verify the proof — see `docs/PROOF_STORAGE.md`.
      ::
      ::  Phase 3c step 3: general-purpose prover primitive. Takes an
      ::  arbitrary `[subject formula]` pair and traces
      ::  `(fink:fock [subject formula])` via `prove-computation:vp`
      ::  bound to the kernel's current `(root, hull)`.
      ::
      ::  The fundamental trust boundary: whatever the formula
      ::  evaluates to becomes the committed product. A caller
      ::  building a formula that computes `validate-claim-bundle`
      ::  gets in-STARK validation; a caller building `[1 42]` gets a
      ::  STARK over the constant 42. This primitive is
      ::  caller-responsibility — the kernel does not inspect or
      ::  constrain the formula's semantics.
      ::
      ::  Wallet verification then runs `verify:vesl-verifier` on the
      ::  emitted proof against the same `[subject formula]` pair.
      ::  The wallet's own cross-check — "does this subject/formula
      ::  actually express the property I care about?" — closes the
      ::  loop. For the NNS use case that's "is this the Nock of
      ::  validate-claim-bundle-linear applied to my bundle?", which
      ::  is tractable to pattern-match once a canonical encoding is
      ::  published (see `docs/research/recursive-payment-proof.md`
      ::  §"Step 3 Nock-formula encoding").
      ::
      ::  `subject-jam` and `formula-jam` are the JAM bytes of the
      ::  two nouns. The kernel cues them before handing to
      ::  `prove-computation`, keeping the Rust poke-builder side
      ::  simple (bytes in, bytes out) and the kernel in charge of
      ::  Nock-noun shape.
      ::
      [%prove-arbitrary subject-jam=@ formula-jam=@]
      ::  Phase 3c step 3 completion: proves a claim bundle by
      ::  tracing `validate-claim-bundle-linear(bundle)` INSIDE the
      ::  STARK. Uses the subject-bundled-core encoding from
      ::  `build-validator-trace-inputs:np`.
      ::
      ::  Emits `[%claim-in-stark-proof product proof]` on success.
      ::  The `product` is `(each ~ validation-error):np` head-tagged:
      ::  `[%& ~]` iff validation passed, `[%| err]` on rejection.
      ::  Wallet reads product and proof — no re-running the
      ::  validator. Single-artifact trust.
      ::
      $:  %prove-claim-in-stark
          name=@t
          owner=@t
          fee=@ud
          tx-hash=@ux
          claim-block-digest=@ux
          anchor-headers=(list anchor-header)
          page-digest=@ux
          page-tx-ids=(list @ux)
          anchored-tip=@ux
          anchored-tip-height=@ud  ::  Phase 7
      ==
      ::  Y0 recursive-composition spike. Given a previously-emitted
      ::  STARK (`prev-proof`) bound to `[prev-subject prev-formula]`,
      ::  build a second [subject formula] pair that runs
      ::  `verify:vesl-stark-verifier` on those three, then trace it
      ::  through `prove-computation:vp`. Success = single-artifact
      ::  recursive rollup is tractable. Trap = Path Y blocked by the
      ::  same Nock 9/10/11 upstream ask as Phase 3c step 3.
      ::
      ::  Emits `[%recursive-step-dry-run-ok product=?]` on dry-run
      ::  success, followed by either `[%recursive-step-proof product
      ::  proof]` on prover success or `[%prove-failed trace]` on
      ::  prover crash. See `y0_recursive_composition_spike` in
      ::  `tests/prover.rs`.
      ::
      [%prove-recursive-step prev-proof-jam=@ prev-subject-jam=@ prev-formula-jam=@]
      ::  nockup:cause
      ::  graft-inject would add `vesl-cause` here on a fresh
      ::  kernel. Already present below; marker is idempotent.
      ::
      vesl-cause
  ==
::
::  --- Y0 recursive-composition helpers ---
::
::  These build the subject+formula pair that traces
::  `verify:vesl-stark-verifier(prev-proof, ~, 0, prev-subject, prev-formula)`
::  under `prove-computation:vp`, using the same subject-bundled-core
::  encoding `build-validator-trace-inputs:nns-predicates` uses for
::  Phase 3c step 3. The Y0 spike's only question is whether Vesl's
::  STARK prover can trace a formula of this shape; a trap here tells
::  us recursive rollup (Path Y) shares the Phase 3c Nock-9/10/11
::  blocker and the upstream ask need not be duplicated.
::
::  +recursive-verify-arm: local proxy arm that slams `verify:vv`.
::  We proxy instead of calling `verify:vv` directly from the trace
::  because `!=(verify:vv)` compiles to
::
::      [11 hint [7 <nav-to-vv> [9 <arm> 0 1]]]
::
::  — a Nock-7 composition that threads the `vv` subject navigation
::  before the arm access, which breaks the subject-bundled-core
::  trick (the `[9 arm 0 3]` slot expects a pure arm access on its
::  core). Staging the call through a local arm keeps the emitted
::  Nock shape at `[11 hint [9 <arm> 0 1]]` — the same shape
::  `validator-arm-axis:nns-predicates` extracts.
::
::  The cast `;;(proof:sp prev-proof)` runs at Nock level and the
::  STARK would have to trace it too; it's cheap and gives us type
::  safety at the wallet/dry-run boundary.
::
++  recursive-verify-arm
  |=  [prev-proof=* override=(unit (list term)) eny=@ prev-subj=* prev-form=*]
  ^-  ?
  =/  prf=proof:sp  ;;(proof:sp prev-proof)
  (verify:vv prf override eny prev-subj prev-form)
::
::  +recursive-verify-arm-axis: compile-time extraction of
::  `recursive-verify-arm`'s axis inside this core's battery. Same
::  `!= + strip-%11` idiom as `validator-arm-axis:nns-predicates`.
::
++  recursive-verify-arm-axis
  ^~
  =/  probe  !=(recursive-verify-arm)
  =/  inner=*
    ?.  ?=([%11 * *] probe)  probe
    +>.probe
  ?>  ?=([@ @ *] inner)
  +<.inner
::
::  +build-recursive-verify-trace-inputs: produce `[subject formula]`
::  for `prove-computation:vp` such that `fink:fock [s f]` would slam
::  `+recursive-verify-arm` on
::  `[prev-proof ~ 0 prev-subj prev-form]` — i.e. run a full
::  `verify:vv` inside the STARK.
::
::  Subject layout:  `[sample self-core]` where
::                   `sample = [prev-proof ~ 0 prev-subj prev-form]`
::                   and `self-core = ..recursive-verify-arm`
::                   (the enclosing kernel `|%`).
::  Formula:         `[9 2 10 [6 0 2] 9 <arm-axis> 0 3]`
::
::  Same shape as `build-validator-trace-inputs:nns-predicates`,
::  swapping `validate-claim-bundle-linear` for
::  `recursive-verify-arm`. When Vesl's prover ships Nock 9/10/11,
::  this traces cleanly and we've graduated out of Y0.
::
++  build-recursive-verify-trace-inputs
  |=  [prev-proof=* prev-subj=* prev-form=*]
  ^-  [subject=* formula=*]
  =/  self-core  ..recursive-verify-arm
  =/  sample=*  [prev-proof ~ 0 prev-subj prev-form]
  :-  [sample self-core]
  [9 2 10 [6 0 2] 9 recursive-verify-arm-axis 0 3]
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
  ::  Atomic fee units: nicks (65.536 nicks = 1 NOCK)
  ::    1..4 chars  -> 327.680.000
  ::    5..9 chars  -> 32.768.000
  ::    10+ chars   -> 6.553.600
  ?:  =(slen 0)  0
  ?:  (gte slen 10)  6.553.600
  ?:  (gte slen 5)   32.768.000
  327.680.000
::
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
::
++  stark-bind
  |=  state=v0-state
  ^-  [@ @]
  :*  (root-atom:na accumulator.state)
      last-proved-height.state
  ==
::
++  moat  (keep v0-state)
::
++  inner
  |_  state=v0-state
  ::
  ++  load
    |=  old-state=v0-state
    ^-  _state
    old-state
  ::
  ::  +peek: Path Y accumulator + graft state
  ::
  ::    /accumulator/<name>  -> (unit nns-accumulator-entry)
  ::    /accumulator-root    -> @ (lossy atom of Tip5 z-map tip)
  ::    /accumulator-jam     -> @ (jam of full nns-accumulator noun)
  ::    /scan-state          -> [height=@ud digest=@ux root=@ size=@ud]
  ::    /fee-for-name/<n>    -> @ud
  ::    [anything else]      -> vesl-peek
  ::
  ++  peek
    |=  =path
    ^-  (unit (unit *))
    ?+  path  (vesl-peek vesl.state path)
        [%accumulator name=@t ~]
      =/  key=@t  +<.path
      ``(get:na [accumulator.state key])
        ::
        [%accumulator-proof name=@t ~]
      =/  key=@t  +<.path
      ``(proof-axis:na [accumulator.state key])
        ::
        [%accumulator-root ~]
      ``(root-atom:na accumulator.state)
        ::
        [%accumulator-jam ~]
      ``(jam accumulator.state)
        ::
        [%scan-state ~]
      ``[ last-proved-height.state
            last-proved-digest.state
            (root-atom:na accumulator.state)
            (size:na accumulator.state)
        ]
        ::
        [%fee-for-name name=@t ~]
      =/  key=@t  +<.path
      ``(fee-for-name:np key)
        ::
    ==
  ::
  ::  Path Y2: %scan-block — parent link + height monotonicity,
  ::  then `+claim-scanner:np` over the supplied candidates.
  ::
  ++  scan-block-poke
    |=  c=cause
    ^-  [(list effect) v0-state]
    ?>  ?=(%scan-block -.c)
    =/  boot=?
      &(=(0 last-proved-height.state) =(0 last-proved-digest.state))
    ?.  ?|(boot =(parent.c last-proved-digest.state))
      :-  ~[[%scan-block-error 'parent-mismatch']]
      state
    ?.  =(height.c +(last-proved-height.state))
      :-  ~[[%scan-block-error 'height-not-successor']]
      state
    ::  Progress on stderr (level 2).
    ::
    =/  ent0
      (weld "nns: scan-block start height=" (trip (scot %ud height.c)))
    =/  ent1  (weld ent0 " prev_height=")
    =/  ent2
      (weld ent1 (trip (scot %ud last-proved-height.state)))
    =/  ent3  (weld ent2 " page_tx_ids=")
    =/  ent4
      (weld ent3 (trip (scot %ud (lent page-tx-ids.c))))
    =/  ent5  (weld ent4 " candidates=")
    =/  ent6
      (weld ent5 (trip (scot %ud (lent candidates.c))))
    =/  slog-enter=@t  (crip ent6)
    ~>  %slog.[2 slog-enter]
    =/  tx-set=(z-set @ux)  (z-silt page-tx-ids.c)
    =/  pag=nns-page-summary:np  [page-digest.c tx-set]
    =/  n-tx-set=@ud  ~(wyt z-in tx-set)
    =/  pg0
      (weld "nns: scan-block page summary built z_set_size=" (trip (scot %ud n-tx-set)))
    =/  pg1  (weld pg0 " block_id=")
    =/  pg2  (weld pg1 (trip (scot %ux page-digest.c)))
    =/  slog-page=@t  (crip pg2)
    ~>  %slog.[2 slog-page]
    =/  new-acc=nns-accumulator:na
      (claim-scanner:np accumulator.state pag height.c candidates.c)
    =/  acc-root=@  (root-atom:na new-acc)
    =/  acc-sz=@ud  (size:na new-acc)
    =/  dn0
      (weld "nns: scan-block done height=" (trip (scot %ud height.c)))
    =/  dn1  (weld dn0 " acc_root=")
    =/  dn2  (weld dn1 (trip (scot %ux acc-root)))
    =/  dn3  (weld dn2 " acc_size=")
    =/  dn4  (weld dn3 (trip (scot %ud acc-sz)))
    =/  dn5  (weld dn4 " block_digest=")
    =/  dn6  (weld dn5 (trip (scot %ux digest.pag)))
    =/  slog-done=@t  (crip dn6)
    ~>  %slog.[2 slog-done]
    =.  accumulator.state  new-acc
    =.  last-proved-height.state  height.c
    =.  last-proved-digest.state  digest.pag
    :-  ~[[%scan-block-done height.c digest.pag acc-root]]
    state
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
        %scan-block
      (scan-block-poke u.act)
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
      ::  in last-proved on every successful prove poke.
      ::
      ?~  last-proved.state
        :_  state
        ~[[%verify-stark-error 'no-cached-sf']]
      =/  subject=*  subject.u.last-proved.state
      =/  formula=*  formula.u.last-proved.state
      =/  ok=?  (verify:vv proof ~ 0 subject formula)
      :_  state
      ~[[%verify-stark-result ok]]
      ::
        %verify-stark-explicit
      ?.  ?=(@ blob.u.act)
        :_  state
        ~[[%verify-stark-error 'blob-not-atom']]
      ?.  ?=(@ subject-jam.u.act)
        :_  state
        ~[[%verify-stark-error 'subject-jam-not-atom']]
      ?.  ?=(@ formula-jam.u.act)
        :_  state
        ~[[%verify-stark-error 'formula-jam-not-atom']]
      =/  jammy=@  blob.u.act
      =/  cue-res  (mule |.((cue jammy)))
      ?.  -.cue-res
        :_  state
        ~[[%verify-stark-error 'bad-jam']]
      =/  proof=proof:four  ;;(proof:four +.cue-res)
      =/  subject-cue  (mule |.((cue subject-jam.u.act)))
      ?.  -.subject-cue
        :_  state
        ~[[%verify-stark-error 'bad-subject-jam']]
      =/  formula-cue  (mule |.((cue formula-jam.u.act)))
      ?.  -.formula-cue
        :_  state
        ~[[%verify-stark-error 'bad-formula-jam']]
      =/  subject=*  p.subject-cue
      =/  formula=*  p.formula-cue
      =/  ok=?  (verify:vv proof ~ 0 subject formula)
      :_  state
      ~[[%verify-stark-result ok]]
      ::
        %verify-accumulator-snapshot
      ::  expected-root / acc-jam are already `@` on the $cause mold; do not
      ::  test `?=(@ ...)` here — mint-vain (dead branch) under current Hoon.
      ::
      =/  acc-cue  (mule |.((cue acc-jam.u.act)))
      ?.  -.acc-cue
        :_  state
        ~[[%accumulator-snapshot-verify-error 'bad-acc-jam']]
      =/  acc=nns-accumulator:na  ;;(nns-accumulator:na +.acc-cue)
      ?.  =((root-atom:na acc) expected-root.u.act)
        :_  state
        ~[[%accumulator-snapshot-verify-result %.n]]
      =/  entry=nns-accumulator-entry:na
        :*  owner=owner.u.act
            tx-hash=tx-hash.u.act
            claim-height=claim-height.u.act
            block-digest=block-digest.u.act
        ==
      =/  got=(unit nns-accumulator-entry:na)  (get:na [acc name.u.act])
      =/  ok=?  =(got [~ entry])
      :_  state
      ~[[%accumulator-snapshot-verify-result ok]]
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
        ::  %validate-claim: Phase 3c gate validator. Composes Level A
        ::  + Level B + G1/C2 predicates on the full claim bundle.
        ::  Read-only; emits `[%validate-claim-ok]` on success or
        ::  `[%validate-claim-error <tag>]` where <tag> names the
        ::  first predicate that rejected. State is not mutated.
        ::
        %validate-claim
      =/  tx-set=(z-set @ux)  (z-silt page-tx-ids.u.act)
      =/  pag=nns-page-summary:np  [page-digest.u.act tx-set]
      =/  wit=nns-raw-tx-witness:np
        :*  witness-tx-id.u.act
            witness-spender-pkh.u.act
            witness-treasury-amount.u.act
            witness-output-lock-root.u.act
        ==
      =/  bundle=claim-bundle:np
        :*  name.u.act
            owner.u.act
            fee.u.act
            tx-hash.u.act
            claim-block-digest.u.act
            anchor-headers.u.act
            pag
            anchored-tip.u.act
            anchored-tip-height.u.act
            wit
        ==
      =/  res=(each ~ validation-error:np)
        (validate-claim-bundle:np bundle)
      ?-  -.res
          %&
        :_  state
        ^-  (list effect)
        ~[[%validate-claim-ok ~]]
      ::
          %|
        :_  state
        ^-  (list effect)
        ~[[%validate-claim-error p.res]]
      ==
      ::
        ::  %prove-arbitrary: trace an arbitrary [subject formula] via
        ::  `prove-computation:vp` and emit a proof bound to
        ::  `+stark-bind` (accumulator root + scan height). No validation
        ::  — caller is responsible for
        ::  constructing the pair.
        ::
        ::  Emits `[%arbitrary-proof product proof]` on prover
        ::  success (product is what the formula evaluated to on the
        ::  subject) or `[%prove-failed trace]` on crash. Caches
        ::  `(subject, formula)` in `last-proved` so subsequent
        ::  `%verify-stark` pokes find the right replay inputs.
        ::
        ::  This is the Phase 3c step 3 primitive — see `docs/PROOF_STORAGE.md`
        ::  §"What the current proof attests to".
        ::
        %prove-arbitrary
      =/  subject-cue  (mule |.((cue subject-jam.u.act)))
      ?.  ?=(%& -.subject-cue)
        :_  state
        ~[[%prove-failed (jam p.subject-cue)]]
      =/  formula-cue  (mule |.((cue formula-jam.u.act)))
      ?.  ?=(%& -.formula-cue)
        :_  state
        ~[[%prove-failed (jam p.formula-cue)]]
      =/  subj=*  p.subject-cue
      =/  form=*  p.formula-cue
      =/  [br=@ bh=@]  (stark-bind state)
      =/  attempt
        %-  mule  |.
        (prove-computation:vp subj form br bh)
      ?.  ?=(%& -.attempt)
        :_  state
        ^-  (list effect)
        ~[[%prove-failed (jam p.attempt)]]
      =/  pr  p.attempt
      ?.  ?=(%& -.pr)
        :_  state
        ^-  (list effect)
        ~[[%prove-failed (jam p.pr)]]
      =/  the-proof=proof:sp  p.pr
      ::  Run the formula directly to capture the evaluated product
      ::  for inclusion in the emitted effect. Same semantics as the
      ::  STARK's trace — `.*` and `fink:fock` agree on products,
      ::  they only differ in whether the execution is traced.
      ::
      =/  product=*  .*(subj form)
      =.  last-proved.state  `[subj form]
      :_  state
      ^-  (list effect)
      ~[[%arbitrary-proof product the-proof]]
      ::
        ::  %prove-claim-in-stark: Phase 3c step 3 completion.
        ::  Builds the subject+formula pair via the nns-predicates
        ::  library, runs prove-computation, emits the trace's
        ::  committed product (the validator's return value) alongside
        ::  the STARK. Wallet verifies proof, reads product — no
        ::  validator re-run required.
        ::
        %prove-claim-in-stark
      =/  bundle=claim-bundle-linear:np
        :*  name.u.act
            owner.u.act
            fee.u.act
            tx-hash.u.act
            claim-block-digest.u.act
            anchor-headers.u.act
            page-digest.u.act
            page-tx-ids.u.act
            anchored-tip.u.act
            anchored-tip-height.u.act
        ==
      =/  [subj=* form=*]  (build-validator-trace-inputs:np bundle)
      ::
      ::  Dry-run outside the STARK to catch validator-level bugs
      ::  before paying for a prover run. `.*` on the raw nockvm
      ::  supports the full Nock opcode set, unlike `fink:fock`
      ::  (which is restricted to opcodes 0-8 for STARK-tractability).
      ::  The validator body uses Nock 9 (slam) and Nock 10 (edit)
      ::  via the subject-bundled-core encoding — those ops are
      ::  currently `!!` in `common/ztd/eight.hoon` under Vesl's
      ::  prover, so the `prove-computation` call below will trap
      ::  until upstream Vesl extends `interpret`.
      ::
      =/  dry-run
        %-  mule  |.  .*(subj form)
      ?.  ?=(%& -.dry-run)
        :_  state
        ^-  (list effect)
        ~[[%prove-failed (jam p.dry-run)]]
      =/  [br2=@ bh2=@]  (stark-bind state)
      =/  attempt
        %-  mule  |.
        (prove-computation:vp subj form br2 bh2)
      ?.  ?=(%& -.attempt)
        :_  state
        ^-  (list effect)
        ~[[%prove-failed (jam p.attempt)]]
      =/  pr  p.attempt
      ?.  ?=(%& -.pr)
        :_  state
        ^-  (list effect)
        ~[[%prove-failed (jam p.pr)]]
      =/  the-proof=proof:sp  p.pr
      =/  product=*  .*(subj form)
      =.  last-proved.state  `[subj form]
      :_  state
      ^-  (list effect)
      ~[[%claim-in-stark-proof product the-proof]]
      ::
        ::  %prove-recursive-step: Y0 recursive-composition spike.
        ::
        ::  1. Cue the three JAM atoms (prev-proof, prev-subject,
        ::     prev-formula). A bad JAM emits %prove-failed with the
        ::     mule trace and does not run the prover.
        ::  2. Build `[subject formula]` via
        ::     `+build-recursive-verify-trace-inputs` — the formula
        ::     slams `verify:vv` on `(prev-proof, ~, 0, prev-subject,
        ::     prev-formula)`.
        ::  3. Dry-run via raw `.*(subj form)`. The raw nockvm supports
        ::     the full opcode set; we expect this to produce a loobean
        ::     (ideally `%.y` — a genuinely recursive verify of a
        ::     correctly-generated inner proof). A dry-run crash means
        ::     the encoding itself is broken (surface a `%prove-failed`
        ::     before paying for the prover).
        ::  4. Emit `[%recursive-step-dry-run-ok product=?]` so the
        ::     test can assert step (3) independently of step (5).
        ::  5. Call `prove-computation:vp`. Expected outcome for now:
        ::     mule-trap inside `common/ztd/eight.hoon::interpret`
        ::     because `verify:vv`'s body uses Nock-9/10/11 opcodes
        ::     that Vesl's STARK compute table does not yet model.
        ::     We emit `%prove-failed` with the captured trace; the
        ::     Y0 blocker-signal test asserts that shape.
        ::  6. If the prover unexpectedly succeeds (e.g. Vesl has
        ::     shipped opcode 9/10/11 support), emit
        ::     `[%recursive-step-proof product the-proof]` and cache
        ::     `(subject, formula)` in `last-proved` so a follow-up
        ::     `%verify-stark` poke can round-trip.
        ::
        %prove-recursive-step
      =/  prev-proof-cue  (mule |.((cue prev-proof-jam.u.act)))
      ?.  ?=(%& -.prev-proof-cue)
        :_  state
        ~[[%prove-failed (jam p.prev-proof-cue)]]
      =/  prev-subject-cue  (mule |.((cue prev-subject-jam.u.act)))
      ?.  ?=(%& -.prev-subject-cue)
        :_  state
        ~[[%prove-failed (jam p.prev-subject-cue)]]
      =/  prev-formula-cue  (mule |.((cue prev-formula-jam.u.act)))
      ?.  ?=(%& -.prev-formula-cue)
        :_  state
        ~[[%prove-failed (jam p.prev-formula-cue)]]
      =/  prev-proof=*  p.prev-proof-cue
      =/  prev-subj=*   p.prev-subject-cue
      =/  prev-form=*   p.prev-formula-cue
      =/  [subj=* form=*]
        (build-recursive-verify-trace-inputs prev-proof prev-subj prev-form)
      =/  dry-run
        %-  mule  |.  .*(subj form)
      ?.  ?=(%& -.dry-run)
        :_  state
        ^-  (list effect)
        ~[[%prove-failed (jam p.dry-run)]]
      =/  dry-product=*  p.dry-run
      =/  dry-ok=?  ?=(%.y dry-product)
      =/  [br3=@ bh3=@]  (stark-bind state)
      =/  attempt
        %-  mule  |.
        (prove-computation:vp subj form br3 bh3)
      ?.  ?=(%& -.attempt)
        :_  state
        ^-  (list effect)
        :~  [%recursive-step-dry-run-ok dry-ok]
            [%prove-failed (jam p.attempt)]
        ==
      =/  pr  p.attempt
      ?.  ?=(%& -.pr)
        :_  state
        ^-  (list effect)
        :~  [%recursive-step-dry-run-ok dry-ok]
            [%prove-failed (jam p.pr)]
        ==
      =/  the-proof=proof:sp  p.pr
      =/  product=*  .*(subj form)
      =.  last-proved.state  `[subj form]
      :_  state
      ^-  (list effect)
      :~  [%recursive-step-dry-run-ok dry-ok]
          [%recursive-step-proof product the-proof]
      ==
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
