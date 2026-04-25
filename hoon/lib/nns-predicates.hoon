::  lib/nns-predicates.hoon — shared predicates used by the NNS kernel
::  and (eventually) by the Phase 3 recursive `nns-gate` circuit.
::
::  DESIGN CONSTRAINT: this library is deliberately dep-light.
::
::  The full Nockchain `tx-engine` family (`tx-engine.hoon`,
::  `tx-engine-0.hoon`, `tx-engine-1.hoon`, and their transitive
::  `/common/{pow,nock-prover,schedule,zose}` cone) gives us
::  `block-commitment:page:t`, `compute-id:raw-tx:t`, and the
::  `spends:raw-tx:v1` / `outputs:tx:v1` shapes we eventually want.
::  Unfortunately, pulling tx-engine into the same compilation unit
::  as `lib/vesl-prover.hoon` + `lib/vesl-stark-verifier.hoon` wedges
::  hoonc — both paths transitively import `/common/stark/prover`
::  through a different `=> stark-engine` context, and the dep graph
::  loops on shared `/common/zeke` / `/common/ztd/*` resolution.
::
::  `/common/zoon.hoon` (z-set + has:z-in) is an exception: its only
::  import is `/common/zeke`, so it's safe to pull in. We use it for
::  the Level B `has-tx-in-page` predicate. Level C `matches-treasury`
::  compares the witness to the canonical NNS treasury lock root b58
::  (same as on-chain `note_name_b58`; see nockblocks lockroot link in
::  `src/config.rs::DEFAULT_TREASURY_LOCK_ROOT_B58`).
::
::  Rather than fight the build-system, Phase 3 stages these
::  predicates in three levels:
::
::    Level A (landed 2026-04-24):
::      - `fee-for-name`          (pure Hoon, no external deps)
::      - `chain-links-to`        (works on an `anchor-header` triple,
::                                 not on a full `page:t` noun)
::
::    Level B (landed 2026-04-24, partial):
::      - `has-tx-in-page`        (zoon's `has:z-in` over a claimed
::                                 `(z-set @ux)` of tx-ids)
::      - `matches-block-digest`  (equality check against a claimed
::                                 block-id — the hull is trusted to
::                                 have re-derived it from chain data)
::
::    Level C (pending — needs narrow vendored
::     `hoon/lib/tx-witness.hoon` that reproduces only the tx-engine
::     arms we touch):
::      - `matches-block-commitment`   (recompute commitment from full
::                                      page noun, no hull trust)
::      - `sender-is-owner` / `pays-amount` (over real `raw-tx:v1`)
::
::  Levels A+B let the Phase 3 gate enforce
::  `tx-id ∈ page.tx-ids ∧ page-digest ∈ anchored-chain` without
::  importing the full tx-engine cone; Level C closes the remaining
::  "hull derived the page summary honestly" trust gap.
::
/=  *  /common/zoon
|%
::
::  +$anchor-header: same shape as the kernel's Phase 2a type in
::  `hoon/app/app.hoon`. Keep these in lockstep — or promote to
::  a shared types module if a second caller ever grows here.
::
+$  anchor-header
  $:  digest=@ux
      height=@ud
      parent=@ux
  ==
::
::  +$nns-page-summary: minimal handle on a Nockchain `page:t` that
::  carries just what the gate needs — the block's own digest and a
::  z-set of the tx-ids the block included. The hull (Phase 2c
::  `fetch_page_for_tx`) builds this from `BlockDetails` proto data;
::  we explicitly do NOT carry `parent`, `coinbase`, `accumulated-work`,
::  or any other page field, because their only purpose in the real
::  tx-engine is to feed `hashable-block-commitment` — and that check
::  is Level C's job (see `matches-block-commitment`).
::
::  `tx-ids` is a `(z-set @ux)` = balanced BST keyed by Tip5 hash,
::  ordering matching Nockchain's `tx-ids.page` bit-for-bit. `has:z-in`
::  walks it in O(log n).
::
+$  nns-page-summary
  $:  digest=@ux               :: block-id
      tx-ids=(z-set @ux)       :: ordered set of tx-ids in this block
  ==
::
::  +$claim-bundle: the complete per-claim input the Phase 3c gate
::  consumes. The hull assembles this from the HTTP `/claim` request
::  (claim tuple) + the Phase 2c chain fetchers (page-summary,
::  anchor-headers) + the Phase 2a kernel anchor (anchored-tip).
::
::  Deliberately NOT included:
::    - raw-tx noun (Level C — needs `tx-witness.hoon` vendor)
::    - block PoW STARK proof (Level C+ — needs recursive-gate work)
::
::  Once Level C lands these get added as extra fields and the gate
::  composes `sender-is-owner` / `pays-amount` / `verify:sp-verifier`
::  alongside the Level A/B predicates.
::
+$  claim-bundle
  $:  name=@t                          :: the .nock name
      owner=@t                         :: base58 Nockchain address
      fee=@ud                          :: declared fee in nicks
      tx-hash=@ux                      :: Tip5 tx-id of the paying tx
      claim-block-digest=@ux           :: block where the paying tx landed
      anchor-headers=(list anchor-header)
      page=nns-page-summary            :: the claim's block summary
      anchored-tip=@ux                 :: kernel's current anchor tip
      anchored-tip-height=@ud          :: Phase 7: tip height at prove time
      witness=nns-raw-tx-witness       :: Level C-A: payment semantics
  ==
::
::  +$claim-bundle-linear: Phase 3c step 3 variant of `claim-bundle`
::  whose tx-ids field is a flat `(list @ux)` instead of a `(z-set
::  @ux)`. Lets the validator check tx-inclusion via `has-tx-in-list`
::  (pure Nock) rather than `has:z-in` (needs Tip5 jets).
::
::  Semantically equivalent to `claim-bundle` when the list contains
::  the same elements as the z-set. The hull converts between the two
::  shapes at poke-build time — for `%validate-claim` / `%prove-claim`
::  we use the z-set form (z-silt-canonicalized on the kernel side);
::  for `%prove-claim-in-stark` (step 3) we pass the list through
::  unchanged so the trace stays Tip5-free.
::
+$  claim-bundle-linear
  $:  name=@t
      owner=@t
      fee=@ud
      tx-hash=@ux
      claim-block-digest=@ux
      anchor-headers=(list anchor-header)
      page-digest=@ux
      page-tx-ids=(list @ux)
      anchored-tip=@ux
      anchored-tip-height=@ud            :: Phase 7: tip height at prove time
  ==
::
::  +$validation-error: tag-only union that names which predicate
::  rejected a bundle. The kernel surfaces these as
::  `[%validate-error <tag>]` effects so the hull + wallet can
::  distinguish "malformed name" from "chain linkage broken" without
::  parsing a cord.
::
+$  validation-error
  $?  %invalid-name                    :: G1 — name format
      %fee-below-schedule              :: C2 — fee < fee-for-name
      %page-digest-mismatch            :: claim-block-digest ≠ page.digest
      %tx-not-in-page                  :: tx-hash not in page.tx-ids
      %chain-broken                    :: anchor-headers don't link page to tip
      %anchor-mismatch                 :: Phase 7: bundle anchor ≠ kernel state
      %witness-tx-id-mismatch          :: Level C: witness.tx-id ≠ claim.tx-hash
      %witness-sender-mismatch         :: Level C: witness.spender-pkh ≠ claim.owner
      %witness-underpaid               :: Level C: witness.treasury-amount < fee
      %witness-wrong-treasury          :: Level C: witness output lock root ≠ canonical
  ==
::
::  +$nns-raw-tx-witness: **Level C-A** — narrow view of a
::  Nockchain v1 raw-tx's payment semantics. The hull extracts
::  these four atoms from the full `raw-tx:v1:t` noun fetched from
::  Nockchain and packs them into every `%prove-claim` poke; the
::  kernel then enforces consistency between the witness and the
::  claim tuple (`%prove-claim` also checks output lock root vs
::  canonical treasury lock root b58).
::
::  Why four atoms and not the full raw-tx:
::    - `tx-engine-1` pulls in `tx-engine-0` transitively, which
::      hits a hoonc dep-cycle with `/common/stark/prover`
::      (tracked in ARCHITECTURE.md §9.3). A narrow witness keeps
::      `nns-predicates.hoon` dep-light — it still depends only on
::      `/common/zeke`.
::    - All four predicates below collapse to atom-equality or
::      `gte`; no tx-engine arms needed at the kernel.
::
::  Trust model (Level C-A):
::    - Hull trusted to extract `spender-pkh` / `treasury-amount`
::      / `output-lock-root` from the raw-tx correctly. A hostile
::      hull can lie about these, so the wallet must re-verify by
::      fetching `tx-id` from Nockchain and re-parsing.
::    - Kernel cryptographically enforces: `tx-id` = claim's
::      `tx-hash`; `spender-pkh` = claim.owner; treasury amount >=
::      fee schedule; witness `output-lock-root` = canonical NNS
::      treasury lock-root b58 (must match `DEFAULT_TREASURY_LOCK_ROOT_B58`
::      in Rust / NockBlocks lockroot for this deployment).
::    - Wallet verifies STARK + re-parses raw-tx from chain to
::      confirm witness extraction was honest. One extra chain
::      query vs. today; zero extra trust.
::
::  Level C-B (future) adds `compute-id:raw-tx` re-hashing inside
::  the kernel to eliminate the raw-tx extraction trust entirely.
::  Blocked on a narrow `tx-engine` vendor that breaks the dep
::  cycle. Tracked in ARCHITECTURE.md §9.3.
::
+$  nns-raw-tx-witness
  $:  tx-id=@ux              :: must equal claim.tx-hash
      spender-pkh=@          :: paying signer's pkh (atom form)
      treasury-amount=@ud    :: nicks paid to the NNS treasury
      output-lock-root=@t    :: v1: b58 lock root of treasury note output
                             ::   (GRPC `note_name_b58`); must equal
                             ::   canonical NNS lock (see `matches-treasury`
                             ::   + Rust `DEFAULT_TREASURY_LOCK_ROOT_B58`)
  ==
::
::  +fee-for-name: NNS fee schedule, keyed on the stem length of a
::  `<stem>.nock` name. Mirror of [src/payment.rs::fee_for_name] —
::  when changing either side, update both and run the cross-repo
::  parity test at `tests/phase3_predicates.rs`.
::
::    0 chars        -> 0    (invalid; gate rejects via G1 first)
::    1..4 chars     -> 5000
::    5..9 chars     -> 500
::    10+ chars      -> 100
::
++  fee-for-name
  |=  name=@t
  ^-  @ud
  =/  bytes=@ud  (met 3 name)
  =/  suffix-len=@ud  5
  =/  has-suffix=?
    ?:  (lth bytes suffix-len)  %.n
    =((cut 3 [(sub bytes suffix-len) suffix-len] name) '.nock')
  =/  stem-len=@ud
    ?:  has-suffix  (sub bytes suffix-len)
    bytes
  ::  Atomic fee units: nicks (65.536 nicks = 1 NOCK)
  ::    1..4 chars  -> 327.680.000
  ::    5..9 chars  -> 32.768.000
  ::    10+ chars   -> 6.553.600
  ?:  =(stem-len 0)            0
  ?:  (gte stem-len 10)        6.553.600
  ?:  (gte stem-len 5)         32.768.000
  327.680.000
::
::  +chain-links-to: given a claim's block digest, a list of
::  `anchor-header` triples (oldest-first) extending toward the
::  current tip, and the follower-anchored tip digest, verify that
::  every link's `parent` matches the previous link's `digest` and
::  the last digest equals `anchored-tip`.
::
::  Preconditions (enforced here):
::    - The first header's `parent` MUST equal `claim-digest`.
::    - Consecutive headers MUST satisfy `header[i].parent ==
::      header[i-1].digest` and `header[i].height == header[i-1].height + 1`.
::    - The last header's `digest` MUST equal `anchored-tip`.
::
::  The degenerate case `headers = ~` is accepted only if
::  `claim-digest == anchored-tip` (claim's block is itself the tip
::  and no chain walk is needed).
::
::  Phase 3 Level B will replace this with a form that takes
::  `(list page:t)` and re-derives each digest via
::  `compute-digest:page:t`, eliminating the "trust the follower got
::  the digests right" step. For now the follower is trusted on
::  header digests and the gate trusts the follower. That's fine
::  pre-Phase-3 since Phase 2 is pure plumbing; untrusted once Level
::  B lands.
::
++  chain-links-to
  |=  [claim-digest=@ux headers=(list anchor-header) anchored-tip=@ux]
  ^-  ?
  ?~  headers
    =(claim-digest anchored-tip)
  ::  First link must chain from the claim's block.
  ::
  ?.  =(parent.i.headers claim-digest)  %.n
  ::  Walk the rest: each header's parent is the previous digest,
  ::  and heights must increment by 1.
  ::
  =/  prev=anchor-header  i.headers
  =/  rest=(list anchor-header)  t.headers
  |-  ^-  ?
  ?~  rest
    =(digest.prev anchored-tip)
  ?.  =(parent.i.rest digest.prev)  %.n
  ?.  =(height.i.rest +(height.prev))  %.n
  $(rest t.rest, prev i.rest)
::
::  +has-tx-in-page: is `claimed-tx-id` a member of `page.tx-ids`?
::
::  Uses zoon's `has:z-in` directly — O(log n) BST walk, same
::  ordering the real `tx-ids.page` is built with on the Nockchain
::  side. Level C's `matches-block-commitment` will eventually bind
::  the page summary to the block-proof's committed commitment; for
::  now the hull is trusted to have populated `tx-ids` from
::  `BlockDetails.tx_ids` without tampering.
::
++  has-tx-in-page
  |=  [pag=nns-page-summary claimed-tx-id=@ux]
  ^-  ?
  (~(has z-in tx-ids.pag) claimed-tx-id)
::
::  +matches-block-digest: does the claimed block's digest equal the
::  on-chain page's digest? Trivial equality check on two @ux atoms.
::
::  This is the Level B stand-in for
::  `matches-block-commitment:page:t` — the hull reads the block-id
::  from Nockchain via `fetch_block_details_by_height` and passes it
::  into the gate; the gate confirms the same block-id appears in the
::  kernel's anchored-chain (Phase 2). Trust boundary: the kernel
::  trusts the *chain* on digests (via anchor linkage) but not the
::  hull, which is exactly the property Phase 3 wants.
::
::  Level C replaces this with the full `hash-hashable:tip5` over
::  `hashable-block-commitment(page)`, at which point the hull can't
::  lie about the digest even if it wanted to — the gate derives it
::  itself.
::
++  matches-block-digest
  |=  [pag=nns-page-summary claimed-digest=@ux]
  ^-  ?
  =(digest.pag claimed-digest)
::
::  --- Level C-A payment-semantic predicates ---------------------
::
::  Each is a thin equality/arithmetic check on the
::  `nns-raw-tx-witness` that the hull extracts from the on-chain
::  raw-tx. See the type comment on `+$nns-raw-tx-witness` for the
::  trust model.
::
::  +matches-tx-id: witness claims this is the same tx the hull
::  claims the user paid with. Catches a hostile hull trying to
::  swap one tx's payment for another's.
::
++  matches-tx-id
  |=  [witness=nns-raw-tx-witness claim-tx-hash=@ux]
  ^-  ?
  =(tx-id.witness claim-tx-hash)
::
::  +sender-is-owner: the raw-tx was signed by the claim's owner. The
::  hull extracts the spender's pkh; the kernel enforces that it
::  equals `claim.owner`. Atom-equality is used because both sides
::  are pre-canonicalised to the same representation at bundle
::  construction — the hull bundles a pkh-form owner string.
::
++  sender-is-owner
  |=  [witness=nns-raw-tx-witness claim-owner=@t]
  ^-  ?
  =(`@`claim-owner spender-pkh.witness)
::
::  +pays-amount: the treasury received at least the fee-schedule
::  minimum for this name. The witness carries a pre-summed
::  `treasury-amount`; the kernel re-applies the schedule. A
::  hostile hull can't lie low because the wallet re-parses the
::  chain raw-tx.
::
++  pays-amount
  |=  [witness=nns-raw-tx-witness min-fee=@ud]
  ^-  ?
  (gte treasury-amount.witness min-fee)
::
::  +matches-treasury: treasury payment output's lock root (v1
::  `note_name_b58` / NockBlocks lockroot) must be the canonical NNS
::  treasury lock.
::
++  matches-treasury
  |=  witness=nns-raw-tx-witness
  ^-  ?
  ::  lock hash for treasury - TREASURY_LOCK_ROOT_B58 from src/payment.rs
  =/  expected=@t
    'A3LoWjxurwiyzhkv8sgDv2MVu9PwgWHmqoncXw9GEQ5M3qx46svvadE'
  =(output-lock-root.witness expected)
::
::  +matches-current-anchor: Phase 7 freshness binding.
::
::  Kernel's `%prove-claim` uses this to confirm the bundle was
::  built against the kernel's *current* anchor tip. A wallet later
::  reads `anchored-tip-height` out of the bundle (cryptographically
::  committed via `bundle-digest`) and checks it against its own
::  chain-tip view. See ARCHITECTURE.md §7 for the attack this
::  closes (malicious operator pokes stale kernel manually).
::
::  A small predicate rather than inline `=` because it's the
::  single conceptual check that binds the *proof* to a *snapshot
::  of the chain-follower state* — naming it makes that explicit.
::
++  matches-current-anchor
  |=  $:  bundle-tip=@ux   bundle-height=@ud
          state-tip=@ux    state-height=@ud
      ==
  ^-  ?
  ?&  =(bundle-tip state-tip)
      =(bundle-height state-height)
  ==
::
::  +has-tx-in-list: Phase 3c step 3 variant of `has-tx-in-page`.
::
::  O(n) linear walk over a flat `(list @ux)`, no Tip5 hashing. Trades
::  `has:z-in`'s O(log n) BST for a simpler Nock trace that stays
::  inside `fink:fock` without any jet calls. Use when the validator
::  needs to run INSIDE the STARK (Phase 3c step 3) — the extra CPU
::  vs z-in is negligible for block sizes ≤ 1000 tx-ids (typical
::  Nockchain blocks) and the reduced trace cost pays for itself.
::
::  Semantics identical to `has-tx-in-page` when the list contains
::  exactly `page.tx-ids` with no duplicates: returns `%.y` iff
::  `claimed-tx-id ∈ list`.
::
++  has-tx-in-list
  |=  [tx-ids=(list @ux) claimed-tx-id=@ux]
  ^-  ?
  |-  ^-  ?
  ?~  tx-ids  %.n
  ?:  =(i.tx-ids claimed-tx-id)  %.y
  $(tx-ids t.tx-ids)
::
::  --- G1 format helpers (duplicated from app.hoon so the gate
::      library is self-contained — keep them in sync) ---
::
++  valid-char
  |=  c=@
  ^-  ?
  ?|  &((gte c 'a') (lte c 'z'))
      &((gte c '0') (lte c '9'))
  ==
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
++  has-nock-suffix
  |=  cord=@t
  ^-  ?
  =/  n  (met 3 cord)
  ?:  (lth n 6)  %.n
  =((cut 3 [(sub n 5) 5] cord) '.nock')
::
++  stem-len
  |=  cord=@t
  ^-  @ud
  (sub (met 3 cord) 5)
::
::  +is-valid-name: G1 — `<nonempty lowercase+digit stem>.nock`.
::
++  is-valid-name
  |=  name=@t
  ^-  ?
  ?.  (has-nock-suffix name)  %.n
  =/  slen  (stem-len name)
  ?:  =(slen 0)  %.n
  (all-valid-chars (cut 3 [0 slen] name))
::
::  +validate-claim-bundle: Phase 3c gate validator. Composes the
::  Level A + Level B + Level C-A predicates + the cheap G1/C2
::  format/fee checks into a single arm. Returns either `[%.y ~]`
::  on success, or `[%.n err]` naming the first predicate that
::  rejected the bundle.
::
::  Ordering is cheap-to-expensive so we short-circuit quickly on bad
::  input. The expensive step is `chain-links-to`, which walks the
::  full header chain with Tip5 equality comparisons (the follower
::  should never submit a bundle whose chain isn't sound anyway).
::
::  Bundle-only checks (all cryptographically bound via the STARK's
::  bundle-digest commitment). The kernel `%prove-claim` cause adds
::  one *state-relative* check that isn't here:
::    - `matches-current-anchor` (Phase 7): bundle anchor == kernel
::      anchor state.
::  `matches-treasury` (Level C-A) compares the witness to a fixed
::  lock root and lives in `%prove-claim` beside the anchor check.
::
::  Predicates NOT yet enforced (Level C-B, future):
::    - `compute-id:raw-tx:t` inside the kernel (would eliminate the
::      hull's witness-extraction trust).
::    - `verify:sp-verifier` on a block PoW STARK.
::    - `matches-block-commitment` — recompute page commitment
::      from the full page noun.
::  Level C-B needs a narrow `tx-engine` vendor that breaks the
::  hoonc dep-cycle; tracked in ARCHITECTURE.md §9.3. The hull
::  remains trusted for those fields until then, but its
::  trustworthiness is *falsifiable*: a wallet that fetches the
::  raw-tx from Nockchain and re-parses it can independently
::  verify every witness field.
::
++  validate-claim-bundle
  |=  bundle=claim-bundle
  ^-  (each ~ validation-error)
  ::  G1 — name format.
  ?.  (is-valid-name name.bundle)
    [%| %invalid-name]
  ::  C2 — declared fee >= schedule. Belt-and-suspenders with the
  ::  witness-underpaid check below; short-circuits on the cheap
  ::  atom comparison before we start touching witness fields.
  ?.  (gte fee.bundle (fee-for-name name.bundle))
    [%| %fee-below-schedule]
  ::  Level C-A — witness's tx-id == claim.tx-hash.
  ?.  (matches-tx-id witness.bundle tx-hash.bundle)
    [%| %witness-tx-id-mismatch]
  ::  Level C-A — witness's sender pkh == claim.owner.
  ?.  (sender-is-owner witness.bundle owner.bundle)
    [%| %witness-sender-mismatch]
  ::  Level C-A — actual treasury-flowed amount >= fee schedule.
  ::  Stricter than C2: hull can declare any `fee` it likes, but
  ::  the chain tx's treasury flow is falsifiable.
  ?.  (pays-amount witness.bundle (fee-for-name name.bundle))
    [%| %witness-underpaid]
  ::  Level B — claim-block-digest matches page.digest.
  ?.  (matches-block-digest page.bundle claim-block-digest.bundle)
    [%| %page-digest-mismatch]
  ::  Level B — tx-hash is in the claimed block.
  ?.  (has-tx-in-page page.bundle tx-hash.bundle)
    [%| %tx-not-in-page]
  ::  Level A — claim-block chains to the kernel's anchored tip.
  ?.  %-  chain-links-to
      :*  claim-block-digest.bundle
          anchor-headers.bundle
          anchored-tip.bundle
      ==
    [%| %chain-broken]
  [%& ~]
::
::  +validate-claim-bundle-linear: Phase 3c step 3 variant.
::
::  Identical semantics to `validate-claim-bundle`, but with every
::  predicate replaced by a Tip5-jet-free alternative so the function
::  can run INSIDE the STARK trace without needing jet support:
::
::    - `has-tx-in-list` (O(n) list walk) replaces `has-tx-in-page`
::      (O(log n) z-in `has` that calls `gor-tip` → `hash-noun-varlen`).
::    - All other predicates (`is-valid-name`, `fee-for-name`,
::      `matches-block-digest`-as-atom-equality, `chain-links-to`)
::      are already pure Nock.
::
::  Trade-off: linear walk over tx-ids costs O(n) Nock steps per
::  claim vs O(log n) for z-in. Real Nockchain blocks carry tens to
::  a few hundred tx-ids; the extra Nock steps are negligible vs the
::  STARK trace's overall footprint.
::
::  This is the gate body Phase 3c step 3 will eventually trace
::  inside `prove-computation`. See `docs/research/recursive-payment-proof.md`
::  §"Step 3: Nock-formula encoding" for the remaining
::  encoding work required to get this arm's compiled formula
::  embedded in a `fink:fock`-traceable `[subject formula]` pair.
::
++  validate-claim-bundle-linear
  |=  bundle=claim-bundle-linear
  ^-  (each ~ validation-error)
  ?.  (is-valid-name name.bundle)
    [%| %invalid-name]
  ?.  (gte fee.bundle (fee-for-name name.bundle))
    [%| %fee-below-schedule]
  ?.  =(page-digest.bundle claim-block-digest.bundle)
    [%| %page-digest-mismatch]
  ?.  (has-tx-in-list page-tx-ids.bundle tx-hash.bundle)
    [%| %tx-not-in-page]
  ?.  %-  chain-links-to
      :*  claim-block-digest.bundle
          anchor-headers.bundle
          anchored-tip.bundle
      ==
    [%| %chain-broken]
  [%& ~]
::
::  --- Phase 3c step 3 (validator-in-STARK) ---
::
::  HOT PATH: only `validator-arm-axis` and `build-validator-trace-inputs`
::  are reached from the kernel's `%prove-claim-in-stark` cause. They
::  feed the subject-bundled-core encoding into `prove-computation:vp`,
::  which currently traps inside Vesl's STARK prover — see Current
::  upstream blocker in `ARCHITECTURE.md`.
::
::  DORMANT ARTIFACTS: `+normalize-to-0-8`, `+ta-axis`, `+rebuild-at-axis`
::  below are preserved research code from the Path-3 spike
::  (2026-04-24). They implement a structure-preserving Nock-9/10/11
::  rewriter intended to pre-expand Hoon-compiled formulas into the
::  0-8 subset that Vesl's compute table models. They compile cleanly
::  but are not invoked: embedding them inside `fink:fock::interpret`
::  (the equivalent in-prover approach) OOM'd on real validator
::  bodies because each rewritten Nock-9 arm call expands into a
::  full-subtree trace, giving a geometric blowup in trace size. The
::  proper fix is native opcode support in Vesl's compute-table
::  constraints (Phase 8). Kept as a starting point for that work.
::
::  `validator-arm-axis` captures, at compile time, the axis at which
::  hoonc places `validate-claim-bundle-linear` inside this core's
::  battery. `!=(arm)` produces the Nock formula for arm access; for
::  a `|%` arm that formula has shape
::
::      `[11 <hint> [9 <axis> <core-path>]]`
::
::  (hoonc wraps every arm access in a `%fast`/`%mean` hint). We
::  peel the Nock-11 if present and pull out `<axis>`. `^~` forces
::  compile-time evaluation — the result is a literal atom in the
::  compiled kernel, zero poke-time cost, and a Hoon rename turns
::  into a compile error rather than a silent runtime bug.
::
++  validator-arm-axis
  ^~
  =/  probe  !=(validate-claim-bundle-linear)
  =/  inner=*
    ?.  ?=([%11 * *] probe)  probe
    +>.probe
  ?>  ?=([@ @ *] inner)
  +<.inner
::
::  +ta-axis: map axis K of T to its axis inside the post-Nock-8-push
::  subject `[T a]`. Used by `+rebuild-at-axis` to produce correct
::  axis references when reconstructing an edited T.
::
::      ta-axis(1) = 2             (T is at axis 2 of [T a])
::      ta-axis(2) = 4             (head of T = head of head of [T a])
::      ta-axis(3) = 5             (tail of T)
::      ta-axis(6) = 10            (head of tail of T)
::      ta-axis(7) = 11            (tail of tail of T)
::
++  ta-axis
  |=  k=@
  ^-  @
  ?:  =(1 k)  2
  =/  parent  $(k (div k 2))
  ?:  =(0 (mod k 2))
    (mul 2 parent)
  +((mul 2 parent))
::
::  +rebuild-at-axis: produce a Nock formula that — when evaluated
::  against the post-Nock-8-push subject `[T a]` — yields T with
::  `axis` replaced by whatever `current` evaluates to. Walks the
::  axis path from deepest to root, wrapping `current` at each
::  step with an autocons of the unaffected sibling branch.
::
++  rebuild-at-axis
  |=  [axis=@ current=*]
  ^-  *
  ?:  =(1 axis)  current
  =/  parent   (div axis 2)
  =/  is-head  =(0 (mod axis 2))
  =/  sibling  ?:(is-head +(axis) (dec axis))
  =/  sibling-ta-axis  (ta-axis sibling)
  =/  sibling-fetch=*  [%0 sibling-ta-axis]
  =/  new-current=*
    ?:  is-head  [current sibling-fetch]
    [sibling-fetch current]
  $(axis parent, current new-current)
::
::  +normalize-to-0-8: rewrite a Nock formula so it uses only
::  opcodes 0–8 + autocons (cell-headed formula). This lets
::  `fink:fock` (Vesl's STARK prover, which currently traps on
::  Nock 9/10/11) trace Hoon-compiled formulas.
::
::  Rewrites, proven equivalent to the Nock spec:
::
::    [9 b c]          →  [7 (normalize c) [2 [0 1] [0 b]]]
::      (direct: the spec *defines* Nock-9 as this sequence)
::
::    [10 [axis v] t]  →  [8 (normalize t)
::                          (rebuild-at-axis axis [7 [0 3] (normalize v)])]
::      (noun surgery: Nock-8 pushes T onto subject; `rebuild-at-axis`
::      walks the axis path producing nested autocons that
::      reconstructs T with the target axis replaced by V)
::
::    [11 * *]         →  [7 [0 1] (normalize next)]
::      (spec: product is `*[a <next>]`; we wrap in `[7 [0 1] …]`
::      rather than collapsing to `next` so the rewrite stays
::      **structure-preserving** — `[%11 * *]` and `[%7 [0 1] *]`
::      are both 3-slot cells with `next` at axis 7. Critical when
::      normalizing whole cores: arm axes stay put.)
::
::  Constants `[1 c]` are recursed into because hoonc emits gate
::  batteries as `[1 <body-formula>]` constants, where the body
::  formula is later evaluated via Nock-2 after gate construction.
::  Those inner formulas contain hints and cross-arm calls. The
::  theoretical risk: a genuine data atom structured as
::  `[9|10|11 X Y]` would get rewritten, but such data doesn't
::  occur in Hoon-compiled cores (np-core's data is atoms and
::  cores whose batteries are themselves formulas).
::
++  normalize-to-0-8
  |=  f=*
  ^-  *
  ?@  f  f
  ?.  ?=(@ -.f)
    [$(f -.f) $(f +.f)]
  ::
  ::  Each opcode arm defensively pattern-matches the expected
  ::  formula shape before rewriting. If `f` has head=0..11 but
  ::  doesn't match the spec shape (e.g., hoonc-emitted Nock-1
  ::  constants carrying data that happens to look opcode-like —
  ::  `[1 [6 5]]` is a real shape that appears in compiled cores),
  ::  we return `f` unchanged. Malformed formulas stay malformed,
  ::  real data stays real data.
  ::
  ?+  -.f  f
      %0  f
    ::
      %1  [%1 $(f +.f)]
    ::
      %2
    ?.  ?=([@ * *] f)  f
    [%2 $(f -.+.f) $(f +.+.f)]
    ::
      %3  [%3 $(f +.f)]
      %4  [%4 $(f +.f)]
    ::
      %5
    ?.  ?=([@ * *] f)  f
    [%5 $(f -.+.f) $(f +.+.f)]
    ::
      %6
    ?.  ?=([@ * * *] f)  f
    [%6 $(f -.+.f) $(f -.+.+.f) $(f +.+.+.f)]
    ::
      %7
    ?.  ?=([@ * *] f)  f
    [%7 $(f -.+.f) $(f +.+.f)]
    ::
      %8
    ?.  ?=([@ * *] f)  f
    [%8 $(f -.+.f) $(f +.+.f)]
    ::
      %9
    ?.  ?=([@ @ *] f)  f
    =/  axis=@   +<.f
    =/  core=*   $(f +>.f)
    [%7 core [%2 [%0 1] [%0 axis]]]
    ::
      %10
    ?.  ?=([@ [@ *] *] f)  f
    =/  edit-axis=@  -.-.+.f
    =/  value=*      $(f +.-.+.f)
    =/  target=*     $(f +.+.f)
    =/  v-fetch=*    [%7 [%0 3] value]
    =/  rebuild=*    (rebuild-at-axis edit-axis v-fetch)
    [%8 target rebuild]
    ::
      %11
    ?.  ?=([@ * *] f)  f
    =/  next=*  $(f +.+.f)
    [%7 [%0 1] next]
  ==
::
::  +build-validator-trace-inputs: produce `[subject formula]` for
::  `prove-computation:vp` such that `fink:fock [s f]` would run
::  `validate-claim-bundle-linear(bundle)` INSIDE the STARK.
::
::  Subject layout:  `[bundle np-core]`
::  Formula:         `[9 2 10 [6 0 2] 9 <arm-axis> 0 3]`
::
::  `..validate-claim-bundle-linear` gives us the pure enclosing
::  core; a naive `=/ self-core .` would capture
::  `[bundle-arg [gate-battery np-core]]` (the gate's own subject)
::  and break axis resolution.
::
::  STATUS: the raw `[9 ... 10 ... 9 ... 0 3]` formula evaluates
::  correctly on the raw nockvm (dry-run in `%prove-claim-in-stark`
::  returns `[%& ~]` for a valid bundle) but `prove-computation:vp`
::  traps on the Nock-9/10/11 opcodes — see `ARCHITECTURE.md` §
::  Current upstream blocker. We emit the formula as-is and let the
::  prover trap; the `%prove-claim-in-stark` cause captures that
::  trap and emits `%prove-failed` so the blocker-signal test in
::  `tests/prover.rs` can assert the specific failure shape.
::
::  When Vesl's prover ships native Nock 9/10/11 support, this arm
::  continues to work unchanged.
::
++  build-validator-trace-inputs
  |=  bundle=claim-bundle-linear
  ^-  [subject=* formula=*]
  =/  np-core  ..validate-claim-bundle-linear
  :-  [bundle np-core]
  [9 2 10 [6 0 2] 9 validator-arm-axis 0 3]
--
