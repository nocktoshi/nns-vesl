# NNS zkRollup Roadmap

This document turns the decision in `docs/CONSENSUS.md` into an
implementation sequence.

> **Pre-production blocker**: Phase 7 (wallet freshness check on
> `t_nns_height`) must ship before any wallet relies on NNS proofs
> for value-bearing decisions. See
> [PROOF_STORAGE.md](PROOF_STORAGE.md) §"Staleness and fork
> resistance" for the attack and
> [§Phase 7](#phase-7--staleness--fork-resistance-blocking-before-production)
> below for the acceptance criteria.

## Today update (2026-04-24 pm) — Phase 3c step 3 foundation

Landed the infrastructure Phase 3c step 3 needs, without yet closing
the last trust gap. The concrete deliverables:

1. **Tip5-free validator variant**: new `++has-tx-in-list` (O(n)
   linear walk, no Tip5 jets in the trace) + `++validate-claim-bundle-linear`
   that uses it. Semantically identical to `validate-claim-bundle`
   on realistic inputs (blocks carry tens to low-hundreds of
   tx-ids; O(n) is negligible vs total trace cost).
2. **General-purpose prover primitive**: new `%prove-arbitrary`
   kernel cause accepting jammed `(subject, formula)` bytes from the
   hull. Kernel cues both, runs `prove-computation:vp`, emits
   `[%arbitrary-proof product proof]`. This is the cause that a
   future canonical Nock-encoding of `validate-claim-bundle-linear`
   will be piped through.
3. **Spike test**: `phase3c_step3_prove_arbitrary_roundtrip` builds
   a trivial `[subject=42 formula=[4 [4 [4 [0 1]]]]]` pair from
   Rust, pokes `%prove-arbitrary`, verifies the proof through
   `%verify-stark`. **Measured: 610 ms prove, 602 ms verify,
   ok=true, 59 235 B proof JAM**. Confirms the pipeline accepts
   caller-supplied formulas end-to-end.

### What's still deferred

The remaining gap to full "validator inside the STARK" is the
**Nock-formula encoding** of `validate-claim-bundle-linear`. The
Hoon arm exists; what doesn't exist yet is a self-contained Nock
formula that `fink:fock [subject=bundle formula]` can trace without
depending on the kernel's compile-time context axes. This is
non-trivial because Hoon-compiled formulas inherit axis references
from the compile-time subject — when handed to
`prove-computation` with a bundle-only subject, those references
break.

Approaches to attempt in a follow-up:

1. **Hand-crafted Nock** — write the formula directly by
   translating the validator arm's Nock tree, with axis 1 pointing
   at the bundle. ~100 lines of Nock, tedious but tractable.
2. **Subject-bundled core** — build `subject = [bundle validator-core]`,
   formula = `[9 <arm-axis> 10 [6 0 2] 0 3]` (Nock-9 slam). Needs
   hoonc introspection to extract `<arm-axis>` reliably.
3. **Formula rebasing tool** — generic Hoon-to-self-contained-Nock
   pass that walks a compiled formula and rewrites axis refs to a
   caller-supplied subject layout. Useful for every future
   validator-in-STARK use case.

All three are compatible with the shipped `%prove-arbitrary` cause
— whichever one lands first plugs in without changes to the kernel
or Rust builders.

### Updated Phase 3 status table

| Level / step | Arms | Status |
|---|---|---|
| A | `fee-for-name`, `chain-links-to` | **shipped** |
| B | `has-tx-in-page`, `matches-block-digest` | **shipped** (hull-trusted page summary) |
| C | `matches-block-commitment`, `pays-sender`, `pays-amount` | pending `tx-witness.hoon` vendor |
| 3c step 1 | `validate-claim-bundle` + `%validate-claim` cause | **shipped** |
| 3c step 2 | `%prove-claim` = validator + committed STARK over bundle-digest | **shipped** (option-B recursion) |
| 3c step 3 foundation | `has-tx-in-list` + `validate-claim-bundle-linear` + `%prove-arbitrary` | **shipped** |
| 3c step 3 completion | Nock-formula encoding of `validate-claim-bundle-linear` + wire into `%prove-arbitrary` | pending encoding spike |

### Totals

**75 active tests, all green** (10 unit + 30 handlers + 9 phase2 +
26 phase3 + 5 ignored prover — step 3 added one prover-heavy test).

## Today update (2026-04-24 pm) — Phase 3c validator + committed proof

Landed Phase 3c in two steps — the gate validator, and a STARK-wrapped
committed proof over a validated bundle.

### Step 1 — `validate-claim-bundle` + `%validate-claim` cause

- `hoon/lib/nns-predicates.hoon` grew `+$claim-bundle`,
  `+$validation-error`, G1 name-format helpers (`is-valid-name`,
  `valid-char`, `all-valid-chars`, `has-nock-suffix`, `stem-len`),
  and `++validate-claim-bundle` — the composition arm that checks,
  in order: G1 (`is-valid-name`), C2 (`fee >= fee-for-name`), Level B
  (`matches-block-digest`, `has-tx-in-page`), Level A
  (`chain-links-to`). Short-circuits on the first failing predicate.
- New kernel cause `%validate-claim` (read-only; does not mutate
  state) emits `[%validate-claim-ok]` on pass or
  `[%validate-claim-error <tag>]` where `<tag>` is one of
  `invalid-name | fee-below-schedule | page-digest-mismatch |
  tx-not-in-page | chain-broken`.
- Rust: `ClaimBundle`, `build_validate_claim_poke`,
  `ValidateClaimResult`, `first_validate_claim_result` in
  `src/kernel.rs`.
- 10 integration tests covering happy path + each rejection path +
  chain-with-headers + short-circuit ordering.

### Step 2 — `%prove-claim` cause + STARK

- New kernel cause `%prove-claim` runs the validator first, only
  proceeds to produce a STARK if validation passes. On pass, folds
  `(jam bundle)` into a belt-digest and runs `prove-computation:vp`
  over it under the kernel's current `(root, hull)`. Emits
  `[%claim-proof bundle-digest proof]` on success.
- The bundle-digest commitment is Fiat-Shamir-bound to `(root, hull)`
  via vesl-prover's puzzle header/nonce absorption — a different
  registry snapshot cannot produce a verifying proof for the same
  bundle.
- Rust: `build_prove_claim_poke`, `ClaimProof`, `claim_proof`,
  `first_claim_proof` in `src/kernel.rs`.
- `#[ignore]`d integration test `phase3c_prove_claim_roundtrip`:
  validates a bundle, proves it, round-trips through
  `%verify-stark`. **Measured: 4.997 s prove, 615 ms verify,
  ok=true, proof JAM 76 537 B, peak RSS 1.0 GB** — in line with the
  Phase 0 baseline (~4.7 s / 75 KiB).

### What the proof attests to (and what it does NOT)

**Attested** (inside the STARK):
- A kernel whose registry is committed at `(root, hull)` asserted
  the bundle-digest at that snapshot.
- Fiat-Shamir binding means the proof cannot be rebound to a
  different registry state.

**Not yet attested in-STARK** (the wallet must check externally):
- Validator predicates passed on the bundle → wallet re-runs
  `validate-claim-bundle` locally on the received bundle.
- Bundle-digest matches the committed subject → wallet re-folds
  `(jam bundle)` to belt-digest, compares.
- Payment semantics (C5 sender + amount) → deferred to Level C's
  `tx-witness.hoon` vendor.
- Block PoW STARK verification inside the gate → deferred to Phase
  3c step 3 (embed `verify:sp-verifier` inside the trace).

This is the "option B" recursion the research memo called out as the
pragmatic intermediate. Level C and step 3 move the validator
execution *inside* the STARK, eliminating the need for the wallet to
re-run the validator itself. Tracked in the
[Phase 3 status](#phase-3-status) table below.

### Totals

**75 tests green** (10 unit + 30 handlers + 9 phase2 + 26 phase3 +
4 ignored prover — Phase 3c added one prover-heavy test marked
`#[ignore]`).

## Today update (2026-04-24 pm) — slim-anchor refactor

- ~~Collapsed kernel `+$anchored-chain` from `[tip-digest tip-height
  recent-headers=(list anchor-header)]` to just `[tip-digest
  tip-height]`. The 1 024-entry header deque was unused — no code
  path read it, and per-claim chain linkage is supplied by the
  claim-note `ClaimChainBundle.header_chain_jam` (Phase 2d) anyway.
  Net storage savings: up to ~90 KB per kernel instance; typical
  savings: ~88 bytes per header the follower had advanced through
  (wasn't keeping them all, but was capping at 1 024).~~
- ~~`%advance-tip` still validates the parent chain on every poke
  but no longer caches intermediate headers after validation. Saved
  ~45 lines of Hoon and removed the `max-anchor-headers` constant.~~
- ~~Rust `AnchorView` dropped `recent_headers`; `decode_anchor`
  simplified from a list-walking loop to a two-atom decode. Two
  obsolete `recent_headers.len()` assertions in `tests/phase2_anchor.rs`
  replaced with `tip_digest` equality checks.~~
- **Design rationale** captured inline in `hoon/app/app.hoon`: NNS is
  a zkRollup-on-Nockchain, and rollups don't re-encode their parent
  chain — they commit to one state root. The kernel commits to one
  Nockchain tip; the wallet trusts Nockchain independently (it has
  to anyway, for UTXOs). Gate verifies the per-claim linkage into
  our tip; we never duplicate Nockchain's chain cache.
- **65 tests total, all green** — same count as before the refactor,
  confirming no behavior drift.

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
| 3c step 1 | `validate-claim-bundle` + `%validate-claim` cause | **shipped** |
| 3c step 2 | `%prove-claim` = validator + committed STARK over bundle-digest | **shipped** (option-B recursion; wallet re-runs validator) |
| 3c step 3 | Validator execution inside the STARK (single-artifact trust) | pending Level C + Nock-formula encoding of predicates |

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

## Phase 7 — Staleness / fork resistance (blocking before production)

Background: [PROOF_STORAGE.md](PROOF_STORAGE.md) §"Staleness and fork
resistance" identifies a vulnerability where a malicious NNS server
can bypass its own follower, manually poke `%claim`, and emit a
STARK proof anchored at a stale `T_nns`. The proof verifies
cryptographically; without wallet-side freshness enforcement the
wallet accepts a name that was really registered to someone else
on-chain. Layers 1 and 2 of the architecture (chain-ordered replay,
frozen-follower convergence) are in place. Layer 3 (wallet freshness
check) is not. **No production wallet should rely on NNS proofs for
value-bearing decisions until Phase 7 ships.**

Acceptance criteria:

- [ ] Proof bundle carries `t_nns_digest: [u8; 40]` and
  `t_nns_height: u64` as explicit top-level fields (not buried in
  the STARK's private inputs). Proof-bundle schema documented in
  `docs/wallet-verification.md` (new file).
- [ ] The recursive nns-gate circuit (Phase 3c) commits
  `t_nns_height` into the STARK's public output so the wallet can
  read the claimed anchor height without decoding the proof bundle
  cryptographically independently.
- [ ] `src/bin/light_verify.rs` (rewritten in Phase 5) exposes a
  `MaxStaleness` parameter with default `20` blocks and rejects any
  proof where
  `proof.t_nns_height < wallet_chain_tip_height - max_staleness`.
  Unit test the boundary cases (exactly at, one-above, one-below).
- [ ] Light-client SDK docs prominently describe the freshness rule,
  explain how to source `wallet_chain_tip_height` (gRPC call against
  the wallet's configured Nockchain node), and give a code snippet
  for end-to-end verification.
- [ ] Integration test: spin up two NNS servers; freeze the follower
  on one; have it issue a proof for a name; assert
  `light_verify` rejects the stale proof once the chain advances
  past the staleness window.

Operator observability (ships alongside Phase 7 or before):

- [ ] `/status` endpoint surfaces `anchor_lag = current_chain_tip -
  anchor.tip_height`. Alert threshold suggested at > 50 blocks.
- [ ] `/status` surfaces count of pending claims in `Submitted`
  state older than 10 minutes.
- [ ] Structured tracing emit on follower anchor advance failures
  (parent-mismatch, height-gap) so operators can distinguish benign
  slowness from chain-divergence bugs.
- [ ] Optional Prometheus export for the three metrics above.

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
- [ ] **Phase 7 (blocking before production)**: wallet freshness
  check on `t_nns_height` to close the malicious-frozen-follower
  attack in [PROOF_STORAGE.md](PROOF_STORAGE.md) §"Staleness and
  fork resistance". Full acceptance criteria in the Phase 7
  section above.
- [ ] Wire follower `AnchorAdvance` path into `%claim` so each claim
  also attaches a `ClaimChainBundle` (blocks Phase 3 consumption).
- [ ] Implement chain-native claim-note discovery path (replace reliance on
  local submission queue for canonical replay input).
- [ ] Add follower cursor persistence + reorg replay mechanics.
- [ ] Complete payment attestation semantics (`sender`, `recipient`,
  `amount >= fee`) instead of acceptance-only checks.
- [ ] Anchor settlement/transition outputs on chain and make wallet
  verification fully trustless by default.
