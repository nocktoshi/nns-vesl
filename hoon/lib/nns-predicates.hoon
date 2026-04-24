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
::  the Level B `has-tx-in-page` predicate.
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
::      - `pays-sender` / `pays-amount` (over real `raw-tx:v1`)
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
::  composes `pays-sender` / `pays-amount` / `verify:sp-verifier`
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
  ?:  =(stem-len 0)            0
  ?:  (gte stem-len 10)        100
  ?:  (gte stem-len 5)         500
  5.000
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
::  Level A + Level B predicates + the cheap G1/C2 format/fee checks
::  into a single arm. Returns either `[%.y ~]` on success, or
::  `[%.n err]` naming the first predicate that rejected the bundle.
::
::  Ordering is cheap-to-expensive so we short-circuit quickly on bad
::  input. The expensive step is `chain-links-to`, which walks the
::  full header chain with Tip5 equality comparisons (the follower
::  should never submit a bundle whose chain isn't sound anyway).
::
::  Predicates NOT yet enforced (Level C, pending tx-witness vendor):
::    - `verify:sp-verifier` on a block PoW STARK (block-proof field
::      is not yet in the bundle)
::    - `compute-id:raw-tx:t`             — claimed tx-hash really is
::                                          this raw-tx
::    - `pays-sender` / `pays-amount`     — C5 payment semantics
::    - `matches-block-commitment`        — recompute page commitment
::                                          from full page noun
::  Until Level C lands, the hull must be trusted to have built the
::  `page-summary` and `anchor-headers` from real chain data. The
::  wallet-side freshness check (Phase 7) handles the remaining gap;
::  see `docs/PROOF_STORAGE.md`.
::
++  validate-claim-bundle
  |=  bundle=claim-bundle
  ^-  (each ~ validation-error)
  ::  G1 — name format.
  ?.  (is-valid-name name.bundle)
    [%| %invalid-name]
  ::  C2 — fee >= fee-for-name.
  ?.  (gte fee.bundle (fee-for-name name.bundle))
    [%| %fee-below-schedule]
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
--
