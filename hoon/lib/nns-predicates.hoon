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
--
