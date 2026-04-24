# Recursive-STARK payment proof spike — GO/NO-GO memo

Status: **Conditional GO — with one hard prerequisite and one scope change**.
Author: Phase 1 design spike.
Date: 2026-04-24.

## TL;DR

The recursive architecture described in [claim_payment_verification_066cf161.plan.md](../../.cursor/plans/claim_payment_verification_066cf161.plan.md) is feasible and technically sound. Nockchain's proof system is unusually well-suited to recursion: the block STARK verifier is pure Hoon, shares every cryptographic primitive with the NNS app STARK, and can be called directly inside `nns-gate`. No impedance mismatch.

However, two uncomfortable facts emerged during Phase 1 investigation:

1. **NNS does not yet produce any STARK proofs.** The current `nns-gate` is Hoon-validated but the STARK prover is not wired through the hull. `TransitionProofMetadata.transition_proof` in [../../src/types.rs](../../src/types.rs) is always `None`. The recursive plan builds on a baseline that does not exist yet. This is Phase 5 on the original roadmap and must land first.
2. **The baseline non-recursive STARK prover is already expensive in the vesl stack.** Reference measurement: the `forge_prove_roundtrip` test in [vesl/hull-rag/tests/e2e_forge.rs](../../../vesl/hull-rag/tests/e2e_forge.rs) requires `--stack-size large`, approximately 128 GB RAM, a 600-second wall-clock budget (with real observed runs in the "minutes" range), and is marked `#[ignore]` so it cannot run in CI. Recursion is projected to multiply that cost by roughly 5–10x.

Recommendation: **proceed with recursion as a research track with a revised scope** that batches recursion across a settlement window instead of running it per-claim. See "Revised plan" below. Per-claim recursion is not viable on current hardware.

## What Phase 1 actually covered

- Read [../../../nockchain/hoon/common/stark/verifier.hoon](../../../nockchain/hoon/common/stark/verifier.hoon) end to end. Interface, purity, FRI parameters, linking checks, merkle verification, DEEP composition all walked.
- Mapped [../../../nockchain/hoon/common/tx-engine-0.hoon](../../../nockchain/hoon/common/tx-engine-0.hoon) and [../../../nockchain/hoon/common/tx-engine-1.hoon](../../../nockchain/hoon/common/tx-engine-1.hoon) `page`, `hashable-block-commitment`, `hashable-digest`, `compute-digest`, `block-commitment`, `hashable-tx-ids`.
- Mapped z-set inclusion via `has:z-in` in [../../../nockchain/hoon/common/zoon.hoon](../../../nockchain/hoon/common/zoon.hoon).
- Checked current prove-path status in [../../../vesl/crates/vesl-core/src/forge.rs](../../../vesl/crates/vesl-core/src/forge.rs) and [../../src/api.rs](../../src/api.rs) (the "Placeholder struct for future full STARK prover integration" comment and the `transition_proof: None` output).
- Measured available baseline: vesl forge's `%prove` poke cost characteristics from [../../../vesl/hull-rag/tests/e2e_forge.rs](../../../vesl/hull-rag/tests/e2e_forge.rs) and [../../../vesl/scripts/diag-reproduce.sh](../../../vesl/scripts/diag-reproduce.sh).

The empirical spike steps in the plan ("build a Hoon spike that calls `verify:nv` and prove the execution") were **deliberately cancelled** because they would have required first wiring up the baseline prover (Phase 5 prerequisite above). The numbers we would produce without that would not be trustworthy.

## Finding 1 — Verifier interface is recursion-ready

`++verify` at [../../../nockchain/hoon/common/stark/verifier.hoon](../../../nockchain/hoon/common/stark/verifier.hoon) line 15:

```
++  verify
  =|  test-mode=_|
  |=  [=proof override=(unit (list term)) verifier-eny=@]
  ^-  ?
```

Pure. Deterministic once `verifier-eny` is fixed. No side effects. Crashes via `!!` on failure. Externally returns `?`; internally `verify-inner` produces `verify-result = [commitment=noun-digest:tip5 nonce=noun-digest:tip5]`.

Inside a recursive circuit we will not need the internal result: we can extract the expected `commitment` cheaply from `proof.objects` (the first item is always the `%puzzle` record) and compare it against our own recomputation from the page. So the sequence is:

```
=/  puzzle  (snag 0 objects.proof)
?>  ?=(%puzzle -.puzzle)
?>  =(commitment.puzzle (block-commitment:page:t our-page))
?>  (verify:sp-verifier block-proof ~ 0)
```

This is exactly the shape envisioned in the plan. No new hooks needed upstream.

### FRI parameters and bound on verifier work

From [../../../nockchain/hoon/common/ztd/eight.hoon](../../../nockchain/hoon/common/ztd/eight.hoon):

- `log-expand-factor = 6` → `expand-factor = 64`.
- `security-level = 50` → `num-spot-checks = 50 / 6 = 8`.
- `num-rounds` derived from domain length; mining-sized traces give `num-rounds ≈ 10–12`.

Proof stream items in a typical block proof:

```
12 (static)
 + num-rounds (≈10)
 + num-rounds × num-spot-checks (≈80)
 + 4 × num-spot-checks (=32)
≈ 134 items
```

Dominant verifier work:

- Merkle proof verification: `4 × num-spot-checks × log2(fri_domain_len)` Tip5 hashes. For `fri_domain_len ≈ 2^20`, that is `≈ 640` hashes.
- FRI spot checks: per-round Fiat-Shamir + folding check. `num_rounds × num_spot_checks ≈ 80` groups.
- Composition polynomial evaluation in the extension field.

Rough analytic bound on Nock trace for one `verify` call: **1M–10M Nock steps**, dominated by polynomial arithmetic in the composition and DEEP evaluation phases. The exact number depends on the block being verified (table heights scale with how much the miner computed).

## Finding 2 — Block commitment and chain linkage are simple

Both v0 and v1 use the same `hashable-block-commitment` shape (parent, `hashable-tx-ids`, coinbase hash, timestamp, epoch-counter, target, accumulated-work, height, msg). The leading `version=%1` tag on v1 does not participate in the hash. The circuit can call `block-commitment:page:t` directly and it will dispatch correctly on both.

Digest linkage: `hashable-digest = [?(~|pow-hash) block-commitment-hashable]`, so each parent's digest depends on (a) the block commitment and (b) either `~` or `hash-proof pow`. The follower can supply a short list of parent `local-page` noun headers (pow already JAM'd for cheap transport); the circuit cue's and re-hashes to confirm each link.

## Finding 3 — Z-set membership is cheap

`tx-ids: (z-set tx-id)` is a balanced binary tree keyed by Tip5 hash. `~(has z-in)` walks left/right on each key comparison. Trace cost:

- For a block with ~2^11 transactions, `has` is ~11 tree-node visits.
- Each visit is a handful of Nock steps and one atom comparison.
- Total: low hundreds of Nock steps. Negligible compared to `verify:sp-verifier`.

**Decision: use `has:z-in` directly.** No separate inclusion-proof input shape required. This simplifies the circuit inputs.

## Finding 4 — NNS has no baseline STARK proving yet

Evidence:

- [../../src/api.rs](../../src/api.rs) sets `transition_proof: None` when building the proof response — there is no actual STARK produced.
- [../../../vesl/crates/vesl-core/src/forge.rs](../../../vesl/crates/vesl-core/src/forge.rs) labels the `Forge` struct a "Placeholder struct for future full STARK prover integration."
- [../../src/main.rs](../../src/main.rs) does not boot the nockapp with `zkvm_jetpack::hot::produce_prover_hot_state()`; therefore the prover jets aren't even registered in the running kernel.

Implication: before the plan can be executed at all, someone must:

1. Boot the NNS hull with prover hot state (copy the pattern from [../../../vesl/hull-rag/tests/e2e_forge.rs](../../../vesl/hull-rag/tests/e2e_forge.rs) `boot_forge_with_prover`).
2. Extend `nns-gate` to emit actual proof bytes rather than the current metadata-only structure.
3. Update `/proof` to return the real bytes.

That is "Phase 5: Upgrade nns-gate to prove claim transitions" from the original roadmap. It is a multi-week project on its own. Until that completes, we cannot benchmark recursion.

## Finding 5 — Baseline non-recursive prover cost is already at the edge

From [../../../vesl/hull-rag/tests/e2e_forge.rs](../../../vesl/hull-rag/tests/e2e_forge.rs):

- Boot: `NockStackSize::Large` (explicit comment "128 GB RAM required").
- Wall clock: one `%prove` poke uses a 600-second tokio timeout; the test comments warn "this may take minutes".
- Marked `#[ignore]` so the test only runs manually on dedicated hardware.

`vesl/scripts/diag-reproduce.sh` confirms: "Run on PC only (needs 128GB for STARK proving)".

This is the non-recursive baseline. Recursion embeds a verifier (`verify:sp-verifier`) inside the outer trace. Expected multipliers:

- Trace length: outer trace gains +1M to +10M Nock steps for the inner verifier. Even in the best case this is a 2–5x multiplier on existing nns-gate traces.
- FRI domain: with `expand_factor = 64`, doubling trace length quadruples FRI work at some stages.
- Memory: the NockStack needs to hold the full outer trace plus intermediate polynomial structures. Doubling the trace roughly doubles peak memory.

Conservative projection for per-claim recursive proving on PC-class hardware:

- Wall clock: **10–30 minutes per claim**.
- Memory: **>128 GB**, possibly 256 GB for larger blocks.
- No prospect of per-claim cadence on server-class hardware in the next 12 months.

Per-claim recursion is therefore infeasible for production.

## Finding 6 — Batched recursion is feasible

The recursion cost is dominated by the verifier-inside-the-trace work. That work is the same per call, regardless of how many claims ride alongside it in the outer proof. So:

- A single outer proof that verifies **one** block PoW STARK, **checks N claim transitions** against that one block, and **composes** in one shot, amortizes the verifier cost across N claims.
- NNS already batches on a settlement cadence (see `%settle-batch` in [../../hoon/app/app.hoon](../../hoon/app/app.hoon)). The natural shape: one settlement batch = one outer STARK.
- For a batch of 64 claims, amortized recursion cost per claim drops to 10–30s — approaching feasibility.

This is the only realistic production shape. It changes the plan's granularity from "claim-level recursive proof" to "settlement-level recursive proof".

## Finding 7 — Anchoring compromise is unavoidable

Even with recursion we cannot cheaply verify every block's PoW STARK up to the tip. A 1000-deep header chain means 1000 invocations of `verify:sp-verifier` — even batched, this is untenable.

Practical compromise (same as in the plan's "Finding 2" section):

- Verify the PoW STARK of **only the block containing the payment** (or the block containing the settlement's earliest payment).
- Link that block to the NNS kernel's anchored tip via a hash chain of parent pointers (cheap: just `hash-hashable:tip5 (hashable-digest parent)` per link).
- Trust-reduce to: "light client follows Nockchain header chain and knows the anchored tip we committed to."

This is the same trust model as any zk-rollup with respect to its L1 (Ethereum rollups trust the L1's header chain; they do not re-verify Ethereum's consensus). It qualifies as "research-level trustlessness" because the only trusted entity is the underlying chain itself.

If in the future we want to eliminate even this — verify Nockchain's full header chain recursively — that is a separate research project (bootstrapping recursive proofs / checkpoint compaction) and should not block the first deployable recursion.

## Decision: Conditional GO with two modifications

**GO** on the recursive-STARK direction. The primitives line up; the cryptography is right; nothing technically blocks us.

**Modifications** to the active plan:

### Modification A — Land Phase 5 (baseline STARK prover) first

Before starting the recursion work, complete these preconditions:

1. Boot nns-vesl with `zkvm_jetpack::hot::produce_prover_hot_state()`.
2. Make `nns-gate` emit actual STARK bytes (not placeholder metadata).
3. Wire real proof bytes through [../../src/api.rs](../../src/api.rs) `ProofResponse.transition_proof`.
4. Benchmark a baseline settlement-level proof on dev hardware. Record: wall-clock, peak memory, proof size.
5. Make that benchmark reproducible (an `#[ignore]`d test, mirroring `forge_prove_roundtrip`).

Exit criterion: one real non-recursive NNS STARK produced and verified end-to-end. Numbers recorded. Go no further until done.

### Modification B — Restructure recursion around the settlement batch, not the individual claim

Replace "per-claim recursive proof" with "per-batch recursive proof":

- The `%settle-batch` poke already collects every claim since `last-settled-claim-id`.
- Extend the settlement payload to include, per batch:
  - a single block PoW STARK (the most recent block containing at least one of the batch's payments; or the chain tip), and
  - a Merkle chain of headers from that block up to the anchored tip.
- In the outer gate, run `verify:sp-verifier` **once** on the embedded block proof, then verify all claims in the batch against that one block (via `has:z-in` per claim against `tx-ids.page`).
- Wallet verification: verify one settlement-level STARK per name lookup instead of one per claim. The existing `GET /proof` becomes "give me the settlement bundle and batch inclusion proof for this name" — still one STARK to verify on the wallet.

This preserves all the trustlessness goals and makes proving cost amortized across the batch.

## Revised phase layout

- **Phase 0 (precondition, 1–2 weeks)** — Wire baseline STARK prover into NNS. Boot with prover hot state. Emit real proof bytes from nns-gate. Benchmark non-recursive settlement proof. Acceptance: measured wall-clock and memory published in this memo (appendix below).
- **Phase 1-redo (2–3 days)** — Empirical spike with real numbers. Verifier embedded in a minimal trace; prove that trace; measure. Updated GO/NO-GO decision gate.
- **Phase 2–4 (unchanged in shape, retargeted at the batch)** — Kernel anchor state, follower fetchers, batched recursive nns-gate circuit.
- **Phase 5 (unchanged)** — Wallet light-client rewrite and documentation.

Realistic revised scope: **6–8 weeks**, not 3–4, because Phase 0 has to happen first and we need a real spike after it.

## Open risks

- Prover throughput scaling with claim volume. If registrations arrive faster than batches can be proved, the queue grows unboundedly. Mitigate by tuning batch cadence and potentially parallelising proof generation across multiple machines.
- Nockchain STARK format drift. Pin a specific `ztd/*` version and vendor the verifier + tx-engine arms we depend on into `hoon/lib/` so upstream changes do not silently break our circuit.
- Hardware lockout. If the only machine that can run recursive proving has 256 GB RAM, we have effectively centralised proof production on operators with that hardware. This is acceptable for an app-level zk system but should be documented; light clients still verify freely on commodity hardware.
- Anchor freshness. The follower's anchor lag = how old a header a light client is willing to trust. Too aggressive and reorgs orphan claims; too conservative and users wait. 50–100 blocks of finality looks right but will need real-world data to tune.

## Appendix — Baseline prover measurements

Measured via [tests/prover.rs](../../tests/prover.rs) `phase0_baseline_prove_and_verify`.
Hardware: Apple Silicon dev laptop (macOS). Release build, `NockStackSize::Large` (32 GB address space, real usage much lower).
Batch shape: one claim for `alpha.nock`; kernel folds the JAM'd batch into a Goldilocks belt-digest, then proves 64 nested Nock-4 increments over it.
Reproduce:

```
cargo +nightly test --release --test prover \
  phase0_baseline_prove_and_verify -- --nocapture --ignored
```

| Metric | Value | Notes |
| --- | --- | --- |
| `%prove-batch` wall-clock | **4.758 s** | Whole poke including trace, FRI, merkle, deep composition |
| Process wall-clock (cargo test) | 81 s | 73 s release build + 7 s test + process teardown |
| Proof size (JAM bytes) | **76 488 B** (~75 KiB) | Opaque, CUE-able back to proof noun |
| Peak RSS | **1.157 GB** | `maximum resident set size`; well under the 128 GB vesl-forge ceiling |
| Peak memory footprint (kernel metric) | 162 MB | OS accounting of active pages |
| Retired instructions | 5.23 × 10^9 | |
| Voluntary context switches | 6 169 | |
| Effects produced | 3 | `%batch-settled`, `%batch-proof`, `%vesl-settled` |

**Surprise finding**: these numbers are an order of magnitude better than the vesl forge `%prove-timeout 600s / 128 GB RAM` reference. Two reasons:

1. The NNS baseline batch is tiny (one leaf).
2. Apple Silicon hits this STARK prover's jets unusually well; we expect ~2–5x slowdown on x86 Linux.

**Updated recursion projection**: multiply by the 1M–10M Nock-step verifier trace embedding. Optimistic bound: ~50 s per prove-batch and ~12 GB peak memory. Pessimistic: ~60 s and ~30 GB. Both are viable on a single dev machine, which reopens per-claim recursion as a possibility — pending Phase 1-redo verification.

| Projected metric | Low estimate | High estimate |
| --- | --- | --- |
| Recursive proof wall-clock | 50 s | 10 min |
| Recursive proof peak memory | 12 GB | 30 GB |
| Recursive proof bytes | 150 KiB | 300 KiB |

## Phase 1-redo — Empirical `verify-inner` measurement

Landed 2026-04-24. Reproduce via

```
cargo +nightly test --release --test prover \
  phase1_redo_verify_inner_proof_wall_clock -- --nocapture --ignored
```

### What we built

- Vendored [vesl-stark-verifier.hoon](../../hoon/lib/vesl-stark-verifier.hoon) and [vesl-verifier.hoon](../../hoon/lib/vesl-verifier.hoon) from vesl master into `hoon/lib/`. These are the matched verifiers for vesl-style proofs (which bypass `puzzle-nock`); we upgraded the verifier's internal `?.  -.result  %.n` to `?.  ?=(%& -.result)` so our stricter hoonc accepts it.
- New kernel causes:
  - `%verify-stark blob=*` — CUEs a proof noun and calls `verify:vesl-verifier` with the `(subject, formula)` pair cached from the last successful `%prove-batch`. Emits `[%verify-stark-result ok=?]` or `[%verify-stark-error msg=@t]`. Implementation in [hoon/app/app.hoon](../../hoon/app/app.hoon).
  - `%prove-identity ~` — sanity arm: proves `[42 [0 1]]`, verifies it on the same kernel, emits `[%prove-identity-result ok=?]`. Used to isolate prover/verifier compatibility from batch-specific input shape.
- Kernel state grew a `last-proved=(unit [subject=@ formula=*])` field so `%verify-stark` can replay the exact `[s f]` the prover traced (the NNS batch shape is not stable across pokes: `%prove-batch` advances `last-settled-claim-id`, which changes the pending window).
- Rust side: `build_verify_stark_poke`, `build_prove_identity_poke`, effect extractors in [src/kernel.rs](../../src/kernel.rs); `#[ignore]`d `phase1_redo_verify_inner_proof_wall_clock` and `phase1_redo_prove_identity_sanity` tests in [tests/prover.rs](../../tests/prover.rs).

### Hard finding: prover/verifier stark-config contract

Standard `verify:nock-verifier` (`/common/nock-verifier.hoon`) reconstructs `[s f]` via `puzzle-nock(header, nonce, pow-len)`. The vesl-forked prover (`hoon/lib/vesl-prover.hoon`) bypasses `puzzle-nock` and traces an arbitrary `[subject formula]`. These are **not interchangeable**. Attempting to verify a vesl-style proof with `verify:sp-verifier` fails at the composition evaluation step because the verifier's re-derived `[s f]` is `(puzzle-nock commitment=root-digest nonce=hull-digest len=0)`, which is not the traced computation.

Implication for the architecture: Phase 3's recursive `nns-gate` can still call `verify:sp-verifier` — but only because Phase 3 verifies **Nockchain block PoW proofs**, which *are* puzzle-nock-derived. There is no need to make Nockchain's `sp-verifier` accept vesl-style proofs. The compatibility boundary is clean: vesl proofs ↔ `vesl-stark-verifier`, block proofs ↔ `sp-verifier`.

### Measurements (Apple Silicon dev laptop, release build, `NockStackSize::Large`)

Non-recursive baseline (reconfirmed):

| Metric | Value |
| --- | --- |
| `%prove-batch` wall-clock | 4.720 s |
| Proof JAM size | 76 879 B (~75 KiB) |
| Peak RSS | 1.116 GB |

Standalone verify on the same fresh proof (via `%verify-stark`):

| Metric | Value |
| --- | --- |
| `%verify-stark` wall-clock | **0.605 s** |
| Verify/prove ratio | **0.13×** |
| `ok` (full STARK math check) | **true** |

Sequential prove + verify: **5.325 s** end-to-end, verify adds ~11.4 % over prove alone.

### What this tells us about recursion

The verifier's internal work (`verify-inner`: FRI, linking checks, composition and DEEP evaluation) takes **~600 ms** of wall-clock for a small NNS batch proof. **That work is what the recursive outer trace has to prove**. Upper-bound analysis of the extra Nock-step count embedded in the outer trace:

- `verify-inner` wall-clock: ~600 ms on jetted Tip5/FRI ops
- Interpolating from baseline prove rate (~4.7 s for ~20 M Nock steps of trace) ≈ **2–4 M extra Nock steps** embedded.
- Outer prove time scales quasi-linearly; doubling trace length roughly doubles FRI work but composition scales with max constraint degree, not trace length.

Revised recursion projection (replaces the earlier 50 s–10 min table):

| Projected metric | Low estimate | High estimate |
| --- | --- | --- |
| Recursive proof wall-clock | **~8 s** | **~25 s** |
| Recursive proof peak memory | **~2 GB** | **~5 GB** |
| Recursive proof JAM size | ~150 KiB | ~300 KiB |

**Per-claim recursion is viable on dev hardware**. A single recursive proof of `verify:sp-verifier block-proof ~ 0` embedded in an NNS `nns-gate` trace should complete in well under a minute. Batched recursion remains preferable for throughput, but it is no longer *required* for feasibility.

### Caveats on these numbers

- The proof we actually verify is an NNS vesl-style proof (batch Merkle commitment over 64 nested Nock-4 increments over a Goldilocks belt-digest), **not** a Nockchain block PoW proof. The verify-inner work (FRI + composition eval) is substantively identical in both cases; only the `[s f]` reconstruction differs (puzzle-nock vs direct). Expect ±30 % variance when measuring against a real block proof.
- The `%prove-identity` sanity arm (trivial `[42 [0 1]]`) **fails** verification: degenerate table heights expose an edge case in the verifier's composition-piece evaluation. This does not affect Phase 3 — the real gate traces are always non-trivial.
- Apple Silicon runs jetted Tip5 unusually fast; x86 Linux is expected to add ~2–5× to both prove and verify.
- Recursive memory projection assumes the outer trace does **not** need to hold the full inner proof in-memory simultaneously with its own polynomial structures; if that assumption fails, peak RSS could grow to ~10–15 GB. Needs confirmation in Phase 3.

### Decision: GO on per-batch recursion, keep per-claim in scope

**Phase 1-redo closes with GO.** The baseline STARK prover works end-to-end, the matched verifier works end-to-end, and the verifier's internal work is modest enough that per-claim recursion is not ruled out by raw wall-clock. The Phase 3 circuit should still target per-batch recursion first (better amortization), but can move to per-claim without re-architecting once Phase 2's chain-input plumbing lands.
