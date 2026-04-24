# NNS zkRollup Roadmap

This document turns the decision in `docs/CONSENSUS.md` into an
implementation sequence.

## Today update (2026-04-23)

- ~~Added canonical same-block ordering in follower replay using
  `(block_height, tx_index_in_block)` via `GetTransactionBlock` +
  `GetBlockDetails`.~~
- ~~Added claim-note `NoteData` schema helpers in `src/claim_note.rs`
  (`nns/v1/claim-version`, `nns/v1/claim-id`, `nns/v1/claim`) with
  roundtrip test coverage.~~
- ~~Added injectable chain-position lookup seam in follower for testing,
  plus focused integration test for same-height race ordering.~~
- ~~Updated README with consensus architecture and security/trust model
  explanations.~~

## Decision memo (Phase 0)

- ~~**Sequencing model**: Path A, Nockchain-ordered claim log.~~
- ~~**Light-client requirement**: required, so we keep the `nns-gate`
  transition-proof track in scope.~~
- ~~**Chain read constraint**: public explorer APIs do not expose note-data;
  follower implementation starts with tx-level tracking and a deterministic
  replay queue, with a future upgrade path to direct note-data scanning.~~
- ~~**Prover constraint**: current Vesl `%prove` path is not yet proving the
  full gate transition; we stage the transition-proof upgrade in a dedicated
  phase and keep it bounded.~~
- ~~**Block-time constraint**: mainnet-style block cadence is slow; API must
  expose pending/confirmed states explicitly.~~

## Phase 1 — Prerequisites

- ~~Wire chain client dependencies and config.~~
- ~~Replace unconditional payment stub with chain-aware verification when
  tx hash is supplied.~~
- ~~Attempt on-chain settlement submission when mode supports it
  (currently best-effort/placeholder path).~~

## Phase 2 — Claim-note schema and submission

- ~~Add a versioned claim-note payload model.~~
- ~~Convert `POST /claim` into chain-first submission producing a pending
  claim handle.~~

## Phase 3 — ChainFollower

- ~~Add background follower that replays pending claim notes into the kernel
  in deterministic order.~~
- ~~Harden deterministic ordering for same-block races using tx index in
  block and test coverage.~~
- [ ] Persist follower cursor + block reference for restart continuity.
- [ ] Add reorg-safe rewind + replay from checkpoints.
- [ ] Upgrade from submission-queue replay to chain-native claim-note
  discovery (direct note-data scan path).

## Phase 4 — Source-of-truth flip

- ~~Remove direct `%claim` poke from request path.~~
- ~~Serve pending/confirmed/rejected through status APIs.~~

## Phase 5 — `nns-gate` transition-proof upgrade

- [ ] Extend gate input shape to carry transition context.
- [ ] Validate per-claim transition predicates in-gate (C1-C4 over ordered
  transitions, not only inclusion).
- [ ] Bind transition proof output to a chain-verifiable anchor so light
  clients can reject non-canonical server roots.

## Phase 6 — Light-client verification surface

- ~~Expand `/proof` payload with transition-proof metadata.~~
- ~~Provide a standalone verifier entrypoint.~~
- [ ] Emit/serve real `transition_proof` bytes (currently metadata-first).
- [ ] Add wallet-ready verification guide + reference flow that verifies:
  inclusion proof + transition proof + chain anchor.

## Next todos (priority order)

- [ ] Implement chain-native claim-note discovery path (replace reliance on
  local submission queue for canonical replay input).
- [ ] Add follower cursor persistence + reorg replay mechanics.
- [ ] Complete payment attestation semantics (`sender`, `recipient`,
  `amount >= fee`) instead of acceptance-only checks.
- [ ] Upgrade `nns-gate` to full transition proofs and ship proof bytes.
- [ ] Anchor settlement/transition outputs on chain and make wallet
  verification fully trustless by default.
