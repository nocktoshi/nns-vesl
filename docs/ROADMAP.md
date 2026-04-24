# NNS zkRollup Roadmap

This document turns the decision in `docs/CONSENSUS.md` into an
implementation sequence.

## Today update (2026-04-24 pm) — Phase 3 Level B tx-inclusion

- ~~Verified `zoon.hoon` is safe to symlink standalone (it only
  imports `/common/zeke`, no stark-engine cone). Added to
  `scripts/setup-hoon-tree.sh` alongside the existing nockchain
  links.~~
- ~~`hoon/lib/nns-predicates.hoon` grew two Level B arms: `has-tx-in-page`
  (walks `tx-ids.page` as a `(z-set @ux)` via zoon's `has:z-in`) and
  `matches-block-digest` (cheap equality check against the hull's
  `BlockDetails.block_id`). Both operate on a minimal
  `+$nns-page-summary [digest=@ux tx-ids=(z-set @ux)]` type so we
  avoid `block-commitment:page:t` (which requires the blocked
  `tx-engine` cone). The hull is trusted to have built the page
  summary from real chain data; Phase 3 Level C (pending) replaces
  that with an in-STARK recompute via vendored `tx-witness.hoon`.~~
- ~~New kernel cause `%verify-tx-in-page digest tx-ids claimed-tx-id`
  builds the canonical z-set via `z-silt` (so `gor-tip` ordering is
  correct) and emits `[%tx-in-page-result ok=?]`. Rust
  `build_verify_tx_in_page_poke` + `first_tx_in_page_result` wired
  through `src/kernel.rs`.~~
- ~~7 new integration tests: 1-byte single, 1-byte two-element,
  8-byte triples (accept + reject), empty-set, 40-byte single,
  40-byte two-element. **16 Phase 3 tests total (9 Level A + 7
  Level B), all passing.**~~
- **Known edge case**: jetted `hash-noun-varlen` in
  `zkvm-jetpack::tip5_jets` crashes when `z-silt` is handed 3+
  40-byte atoms with certain patterns (the `mor-tip` double-hash
  path). Smaller vectors and real Tip5 digests (which never have
  the handcrafted-test patterns) work fine. Upstream jet issue —
  not a blocker for Phase 3c since real chain tx-ids will hit the
  same jet with well-distributed inputs. See the note block at the
  top of the `tx_in_page_*` tests in
  `tests/phase3_predicates.rs`.
- **65 tests total, all green** (10 lib + 30 handlers + 9 phase2 +
  16 phase3 + 3 ignored prover).

### Phase 3 status

| Level | Arms | Status |
|---|---|---|
| A | `fee-for-name`, `chain-links-to` | **shipped** |
| B | `has-tx-in-page`, `matches-block-digest` | **shipped** (hull-trusted page summary) |
| C | `matches-block-commitment`, `pays-sender`, `pays-amount` | pending narrow `tx-witness.hoon` vendor |
| Gate rewrite (3c) | Compose all predicates under `prove-computation` | pending Level C |

## Today update (2026-04-24 pm) — Phase 3 Level A predicates

- ~~New shared predicate library `hoon/lib/nns-predicates.hoon` with
  two arms the Phase 3 recursive gate will consume:
  `fee-for-name` (mirror of Rust `payment::fee_for_name`) and
  `chain-links-to` (walks an oldest-first `(list anchor-header)` by
  parent-pointer from a claim's block digest to the follower's
  anchored tip).~~
- ~~Kernel exposes them as `peek /fee-for-name/<name>` (read-only
  @ud) and cause `%verify-chain-link claim-digest headers
  anchored-tip` → `[%chain-link-result ok=?]`. Matching Rust poke
  builders + decoders in `src/kernel.rs`.~~
- ~~9 new integration tests in `tests/phase3_predicates.rs`:
  2 fee-for-name cases (15-row parity table + long-name sanity)
  and 7 chain-link cases (claim-is-tip, empty-chain-wrong-tip,
  happy 3-header chain, first-parent-mismatch, internal-break,
  height-gap, wrong-final-tip). All green.~~
- ~~Grand total: **58 tests**, all passing (10 lib unit + 30 HTTP +
  9 phase2 + 9 phase3 + 3 ignored prover).~~

### Phase 3 Level B / Level C deferred

The plan's original Phase 3 also called for `matches-block-commitment`,
`has-tx-in-page`, and `pays-sender`/`pays-amount` predicates over
full `page:t` / `raw-tx:v1` nouns. Landing those requires either:

1. Symlinking `/common/tx-engine{,-0,-1}.hoon` +
   `{pow,nock-prover,schedule,zoon,zose}.hoon` from `$NOCK_HOME` — we
   tried this and hit a **hoonc dep-loop**: `tx-engine-0.hoon` hangs
   for ~4 minutes during compile and eventually OOMs. Root cause is
   the shared `/common/stark/prover` resolution through two different
   `=> stark-engine` contexts (one from `vesl-prover`, one from
   `nock-prover` pulled in transitively by `pow`).
2. Vendoring a narrow subset of tx-engine arms into
   `hoon/lib/tx-witness.hoon` with hand-picked re-exports. This is
   the plan's "controlled upgrade" path; we ran out of session time
   before getting to it.

Either way, `nns-gate` v2 and the `%prove-claim` cause wait on the
tx-witness module.

**Status**: Phase 3 Level A shipped + tested; Phase 3 Level B tracked
under follow-up todos. The compile-cycle finding is preserved inline
in `scripts/setup-hoon-tree.sh` so nobody re-adds the tx-engine
symlinks without first redesigning the shared stark-engine import.

## Today update (2026-04-24 pm) — un-vendor vesl hoon

- ~~Moved the two downstream patches we carried on `vesl-stark-verifier.hoon`
  and `vesl-verifier.hoon` upstream on a local vesl feature branch
  `phase1-verifier-debug-and-type-fix` (commit `dc9382c`):~~
    - ~~type-narrow `verify-settlement`'s `mule` unwrap
      (`?.  -.result` → `?.  ?=(%& -.result)`) so stricter hoonc compiles
      it without a nest-fail;~~
    - ~~add `++verify-raw` (top-level + wrapper) that calls `verify-inner`
      directly so assert crashes bubble up as kernel traces — intended
      for prover/verifier integration debugging, not production.~~
- ~~Deleted the five vendored copies under `hoon/lib/vesl-*.hoon` (total
  1 438 lines) and extended `scripts/setup-hoon-tree.sh` to symlink
  from `$VESL_HOME/protocol/lib/` (with the same CLI-env-TOML
  resolution story as `$NOCK_HOME`). NNS now tracks vesl master via
  symlink — a `git pull` in the vesl clone propagates into the next
  `make install`.~~
- ~~Kernel recompiles unchanged (19 MB jam); all 49 active tests still
  pass; the three ignored prover tests still record ~4.7 s prove,
  ~0.61 s verify (ok=true).~~

## Today update (2026-04-24 pm) — Phase 2 chain-input plumbing

- ~~Kernel state grew an `anchored-chain` field (tip digest/height + a
  1 024-entry `recent-headers` deque) and a frozen `payment-address=(unit @t)`.
  New causes `%advance-tip headers=(list anchor-header)` and
  `%set-payment-address address=@t` enforce parent-chain integrity and
  payment-address freeze-after-first-claim. New peeks `/anchor` and
  `/payment-address`.~~
- ~~Rust poke builders + decoders for all four (`build_advance_tip_poke`,
  `build_set_payment_address_poke`, `build_anchor_peek`,
  `build_payment_address_peek`, `decode_anchor`, `decode_payment_address`)
  plus effect extractors (`first_anchor_advanced`,
  `first_payment_address_set`, extended `error_message` coverage).~~
- ~~New NNS-local config module (`src/config.rs`) with CLI > env >
  TOML > compiled-in default resolution for `payment_address`.
  `main.rs` issues the bootstrap `%set-payment-address` poke before
  HTTP serves, so the kernel always starts with a bound payment
  target (frozen on the first successful claim).~~
- ~~Chain fetchers in `src/chain.rs`: `fetch_block_details_by_height`,
  `fetch_block_proof_bytes`, `fetch_transaction_details`,
  `fetch_page_for_tx`, `fetch_header_chain`, `fetch_current_tip_height`,
  `plan_anchor_advance`. `hash_to_atom_bytes` / `atom_bytes_to_hash`
  convert between gRPC `common.v1.Hash` (5 × u64 Belts) and the
  kernel's 40-byte LE-packed `noun-digest:tip5` atom shape.~~
- ~~Follower gained a second background task (`advance_anchor_once`)
  that peeks `/anchor`, asks the chain for the next batch of headers
  up to the finality horizon (`DEFAULT_FINALITY_DEPTH = 10`,
  `DEFAULT_MAX_ADVANCE_BATCH = 64`), and drives `%advance-tip`. Runs
  every 10 s; logs the tip on every advance.~~
- ~~`ClaimNoteV1` got an optional `ClaimChainBundle` with four new
  `nns/v1/*` keys (`raw-tx`, `page`, `block-proof`, `header-chain`).
  Missing fields decode as `None` so local-mode notes stay
  backward-compatible; Phase 3/4 strict mode uses
  `chain_bundle.is_complete()` to reject pre-anchor claims.~~
- ~~9 new integration tests in `tests/phase2_anchor.rs` cover
  bootstrap, extend, parent-mismatch, height-gap, internal-break,
  empty-advance, one-shot address bind, pre-claim re-bind, and
  post-claim freeze. 3 new unit tests cover the claim-note
  chain-bundle roundtrip.~~

Everything green: 10 unit + 30 HTTP handler + 9 Phase 2 = **49 tests passing**. Phase 2 closes; Phase 3 (recursive `nns-gate` circuit) is next.

## Today update (2026-04-24) — recursive-STARK plan

- ~~Phase 0: wired the baseline STARK prover into NNS. `%prove-batch`
  produces a real vesl-style proof (~4.7 s, ~1.2 GB peak RSS, ~75 KiB
  JAM).~~
- ~~Phase 1-redo: vendored `vesl-stark-verifier` + `vesl-verifier` into
  `hoon/lib/`; new `%verify-stark` and `%prove-identity` kernel arms.
  Verified a real NNS batch proof end-to-end in **~0.61 s**
  (verify/prove ratio **0.13×**). Discovered that vesl-style proofs
  and Nockchain `sp-verifier` are not interchangeable — Phase 3's
  recursion still targets `sp-verifier` because the recursed proofs
  are block PoW proofs (puzzle-nock-derived), which is the correct
  compatibility boundary.~~ Full memo in
  [docs/research/recursive-payment-proof.md](research/recursive-payment-proof.md).
- Revised per-batch recursion projection: **~8–25 s wall-clock, ~2–5 GB
  peak memory** on Apple Silicon; per-claim recursion no longer ruled
  out by cost. Plan target (per-batch) unchanged.

Next up: Phase 2 — chain input plumbing (kernel anchored-chain cursor,
hull fetchers for page + block-proof + header chain, extended
settlement-batch schema).

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

- ~~Phase 2: kernel anchored-chain cursor (`anchored-chain`,
  `%advance-tip`, `%set-payment-address`).~~
- ~~Phase 2: hull fetchers for raw-tx, page, block-proof, header chain
  via `nockapp-grpc` public peeks.~~
- ~~Phase 2: extend `ClaimNoteV1` with `nns/v1/raw-tx`, `/page`,
  `/block-proof`, `/header-chain` fields.~~
- [ ] Phase 3 Level B: vendor a narrow `hoon/lib/tx-witness.hoon`
  that re-exports just the arms we need from tx-engine / tx-engine-1
  (`block-commitment:page:t`, `has:z-in` on `tx-ids`,
  `compute-id:raw-tx:t`, `spends:raw-tx:v1`, `outputs:tx:v1`) without
  pulling in the full `pow → nock-prover → stark/prover` cone that
  wedges hoonc.
- [ ] Phase 3 Level B: `matches-block-commitment`, `has-tx-in-page`,
  `pays-sender`, `pays-amount` predicates in `nns-predicates.hoon`
  once `tx-witness.hoon` lands.
- [ ] Phase 3c: recursive `nns-gate` circuit — `verify:sp-verifier`
  on the batch's block PoW proof, `has:z-in` for tx inclusion, C5
  payment predicates (sender pkh, amount >= fee), C1-C4 transitions.
- [ ] Phase 3c: `%prove-claim` kernel cause + Rust poke builder +
  recursive-proof integration test.
- [ ] Phase 4: hull + follower wiring for block-bundle fetch +
  injection into `%settle-batch`; map new kernel error tags.
- [ ] Phase 5: rewrite `src/bin/light_verify.rs` to verify the single
  recursive settlement proof end-to-end; docs + wallet integration
  guide.
- [ ] Wire follower `AnchorAdvance` path into `%claim` so each claim
  also attaches a `ClaimChainBundle` (blocks Phase 3 consumption).
- [ ] Implement chain-native claim-note discovery path (replace reliance on
  local submission queue for canonical replay input).
- [ ] Add follower cursor persistence + reorg replay mechanics.
- [ ] Complete payment attestation semantics (`sender`, `recipient`,
  `amount >= fee`) instead of acceptance-only checks.
- [ ] Anchor settlement/transition outputs on chain and make wallet
  verification fully trustless by default.
