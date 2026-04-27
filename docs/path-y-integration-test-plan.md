# Path Y — first-time integration test plan

This document is a **manual + automated** checklist for validating the
**Nockchain-sequenced scanner** architecture the first time it hits a
real node and real transactions. It complements unit tests (`cargo test`)
and operator docs in [`running_a_node.md`](running_a_node.md).

**In scope:** `%scan-block` follower, gRPC ingestion, accumulator
correctness, HTTP read surface (`/status`, `/accumulator/:name`),
finality behaviour, persistence across restart.

**Explicitly out of scope for this pass:** fork detection, rewind /
checkpoint recovery (Y5), recursive STARK proving (Y3), production
wallet UX beyond what [`claim-note-wallet-support.md`](claim-note-wallet-support.md)
describes.

---

## 0. Preconditions (block all downstream phases until green)

| # | Check | Pass criterion |
|---|--------|------------------|
| 0.1 | Rust + kernel | `make install` succeeds; `out.jam` matches current `hoon/app/app.hoon`. |
| 0.2 | Automated suite | `cargo +nightly test` green on this branch. |
| 0.3 | Nockchain build | Your fork with **`note_data` on v1 outputs** in `GetTransactionDetails` (and consistent indexing after confirm). See [`nockchain-fork-for-nns.md`](nockchain-fork-for-nns.md). |
| 0.4 | Tx builder | A way to attach **NNS-shaped** NoteData on an output ([`claim-note-wallet-support.md`](claim-note-wallet-support.md), e.g. nockchain#85 or equivalent). |
| 0.5 | Economic alignment | Claim tx pays **treasury** matching `TREASURY_LOCK_ROOT_B58` in `src/payment.rs` and **fee ≥ `fee_for_name(name)`** (nicks). Wrong treasury or under-fee → name must **not** appear in the accumulator. |

Record: chain mode (`fakenet` / `dumbnet`), genesis tip height, and
**`DEFAULT_FINALITY_DEPTH`** (10 in `src/chain_follower.rs`) — the follower
only scans heights `≤ chain_tip − finality_depth`.

---

## Phase A — Automated regression (every PR / before manual)

1. `cargo +nightly test` (full tree).
2. Confirm Path Y handler tests still cover:
   - `GET /health`
   - `GET /status` includes `scan_state`, `follower`, no legacy pending/settlement claim fields
   - `GET /accumulator/:unknown` → 200, `value` absent
   - invalid name → 400
   - `?wallet_export=true` returns non-empty `accumulator_snapshot_hex`

---

## Phase B — Local settlement smoke (no chain)

**Config:** `settlement_mode = "local"` (default), no `chain_endpoint`.

| Step | Action | Expected |
|------|--------|----------|
| B.1 | Start `nns`, `curl -s http://127.0.0.1:3000/status \| jq` | HTTP 200; `settlement_mode` is `local`; `chain_endpoint` null or absent. |
| B.2 | Inspect `scan_state` | `last_proved_height` **0** (or documented boot height); `accumulator_size` **0** on fresh `.nns-data/`. |
| B.3 | `GET /accumulator/nonexistent.nock` | 200; `value` absent; coherent `last_proved_height` / `accumulator_root`. |
| B.4 | `POST /claim` or other removed Path A routes | **404** — confirms hull is read-only for claims. |

**Why:** Separates “kernel + HTTP shell works” from gRPC / finality.

---

## Phase C — Chain mode without a claim transaction

**Config:** `settlement_mode = "dumbnet"` or `"fakenet"` (or accepted alias),
`chain_endpoint` pointing at your Nockchain gRPC (TLS/HTTP as supported).

| Step | Action | Expected |
|------|--------|----------|
| C.1 | Empty or fresh `NNS_DATA_DIR` | Clean scan cursor from kernel boot. |
| C.2 | Start `nns`, wait ≥ ~30 s (follower polls every **2 s**) | `GET /status` → `follower.chain_tip_height` eventually **non-null** if the node answers `GetBlocks`. |
| C.3 | Tip height vs finality | If `chain_tip_height ≤ 10`, `last_proved_height` stays **0** — **by design**; do not file a bug until tip exceeds finality depth. |
| C.4 | After tip **> 10** | `last_proved_height` should start advancing **one block per successful tick** (batch size 1); `follower.last_error` null. |
| C.5 | Logs | `phase=scan_block` lines without persistent `err=` after chain is healthy. |

**Failure triage:** `last_error_phase` in `plan` → endpoint / TLS;
`scan_peek` → kernel peek; `scan_poke` → parent digest / height mismatch
or kernel `%scan-block` rejection.

---

## Phase D — First on-chain claim (happy path)

Execute in order; do not skip finality.

| Step | Action | Expected |
|------|--------|----------|
| D.1 | Precompute fee for your name (`fee_for_name` / tiers in `src/payment.rs`). | Document expected nicks. |
| D.2 | Submit **one** valid claim tx with correct NoteData, treasury output(s), and fee. | Tx confirms in block **H**. |
| D.3 | Wait until `chain_tip_height ≥ H + DEFAULT_FINALITY_DEPTH` | Scanner may legally omit block **H** until then. |
| D.4 | Poll `GET /status \| jq .scan_state` | `last_proved_height` ≥ **H** eventually; `accumulator_size` increases if this was the first accepted claim in prefix order. |
| D.5 | `GET /accumulator/<yourname.nock>` | 200; `value` present with **owner**, **tx** binding, height/digest fields consistent with chain (per API schema). |
| D.6 | Optional | `?wallet_export=true` — snapshot hex present; stash for a later `light_verify` experiment ([`wallet-verification.md`](wallet-verification.md)). |

**Concurrency:** If two honest operators run two NNS instances against the
same endpoint and fresh dirs, both should converge to the **same**
`accumulator_root` and `last_proved_height` after long enough — good
“architecture smell test.”

---

## Phase E — Negative and boundary cases

Run after D passes; order flexible.

| ID | Scenario | Expected |
|----|-----------|----------|
| E.1 | NoteData wrong MIME / wrong tuple keys / garbage | No new accumulator row; scan continues; no follower hard-crash. |
| E.2 | Valid-shaped claim but **underpays** fee | Name not in accumulator (predicate drop). |
| E.3 | Pays wrong note (not treasury lock) | Same as E.2. |
| E.4 | **Duplicate** name: second valid tx later on chain | First writer wins; second does **not** replace `value`; size may not grow. |
| E.5 | Two claims **same block**, different tx order | Only one name row; which wins must match **canonical tx-id order** in the block (document observed order for your chain). |
| E.6 | Restart `nns` mid-catch-up | After restart, scanning resumes from persisted cursor; no silent reset to genesis unless data dir wiped. |
| E.7 | Wrong `chain_endpoint` / node down | `last_error` populated; HTTP still serves `/health`; operators use `/status` to see degradation. |

---

## Phase F — Optional stress (schedule after first green)

- Many blocks empty → follower CPU / RPC rate acceptable.
- Rapid tip growth → `anchor_lag_blocks` and `is_caught_up` behave per
  [`running_a_node.md`](running_a_node.md) (lag ≤ `finality_depth + 1`
  when caught up).
- **Reorg:** deferred; when you implement Y5, add a dedicated reorg matrix
  (this plan intentionally skips it).

---

## Sign-off summary

Before calling Path Y “integration verified” for your fork:

- [ ] Phase A green on CI or local.
- [ ] Phase B confirms local vs read-only surface.
- [ ] Phase C proves gRPC + finality gate understood.
- [ ] Phase D shows **at least one** real claim end-to-end in accumulator.
- [ ] Phase E.2–E.5 exercised enough to trust payment + uniqueness rules.

Capture in the ticket / PR: Nockchain commit, `vesl.toml` redacted snippet,
example tx id, block height **H**, and screenshot or `jq` of final
`scan_state` after catch-up.
