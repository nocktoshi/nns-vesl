::  lib/nns-predicates.hoon — shared predicates used by the NNS kernel
::  and (eventually) by the Phase 3 recursive `nns-gate` circuit.
::
::  DESIGN CONSTRAINT: this library is deliberately dep-light.
::
::  The full Nockchain `tx-engine` family (`tx-engine.hoon`,
::  `tx-engine-0.hoon`, `tx-engine-1.hoon`, and their transitive
::  `/common/{pow,nock-prover,schedule,zoon,zose}` cone) gives us
::  `block-commitment:page:t`, `has:z-in`, `compute-id:raw-tx:t`, and
::  the `spends:raw-tx:v1` / `outputs:tx:v1` shapes we eventually
::  want. Unfortunately, pulling tx-engine into the same compilation
::  unit as `lib/vesl-prover.hoon` + `lib/vesl-stark-verifier.hoon`
::  wedges hoonc — both paths transitively import `/common/stark/prover`
::  through a different `=> stark-engine` context, and the dep graph
::  loops on shared `/common/zeke` / `/common/ztd/*` resolution.
::
::  Rather than fight the build-system, Phase 3 stages these
::  predicates in two levels:
::
::    Level A (this file, landed 2026-04-24):
::      - `fee-for-name`          (pure Hoon, no external deps)
::      - `chain-links-to`        (works on an `anchor-header` triple,
::                                 not on a full `page:t` noun)
::
::    Level B (pending — requires a narrow vendored
::     `hoon/lib/tx-witness.hoon` that reproduces only the tx-engine
::     arms we touch, without importing the full cone):
::      - `matches-block-commitment`
::      - `has-tx-in-page`
::      - `pays-sender` / `pays-amount`
::
::  Level A is enough to land Phase 2d's claim-note chain-bundle
::  checks against the kernel's anchor — we walk the declared
::  header chain by parent pointer and confirm it chains to the
::  follower-anchored tip. Payment attestation waits on Level B.
::
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
--
