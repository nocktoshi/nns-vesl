# Running an NNS Node

Bare-minimum operator runbook — how to install, configure, run, and
monitor an NNS node. Everything below assumes a recent macOS or Linux
host with `cargo +nightly` available.

For the full architecture and design rationale, see
`[../ARCHITECTURE.md](../ARCHITECTURE.md)`.

---

## Quick Start

```bash
# clone vesl
git clone https://github.com/zkvesl/vesl-core.git
cd vesl-core && git checkout dev
cd ~
# clone nns repo
git clone https://github.com/nocktoshi/nns-vesl.git
cd nns-vesl
make install                                  # builds kernel + nns binary
# start server
nns                                           # starts on 127.0.0.1:3000
# check status 
curl -s http://127.0.0.1:3000/status | jq .
```

---

## 1. Prerequisites


| Tool             | Why                                             |
| ---------------- | ----------------------------------------------- |
| Rust nightly     | kernel uses nightly features in upstream crates |
| `jq`             | you'll want to pipe JSON responses through it   |
| `curl` or HTTPie | query the local HTTP API                        |
| 2 GB free RAM    | kernel + mirror + axum                          |
| 1 GB free disk   | per-poke checkpoints in `.nns-data/`            |


Check Rust:

```bash
rustup toolchain list | grep nightly
# install if missing:  rustup toolchain install nightly
```

## 2. Install

```bash
git clone https://github.com/zkvesl/vesl-core.git
cd vesl-core && git checkout dev
cd ~
git clone https://github.com/nocktoshi/nns-vesl.git
cd nns-vesl
make install
```

What `make install` does:

- Runs `scripts/setup-hoon-tree.sh` — symlinks Nockchain + Vesl Hoon
libs into `hoon/common/` and `hoon/lib/`.
- Compiles the kernel: `hoonc --new hoon/app/app.hoon hoon/` → produces
`out.jam` (~18 MB).
- Builds the Rust binary: `cargo +nightly build --release`.
- Installs `nns` and `light_verify` to `$HOME/.local/bin` (updates
`$PATH` in `~/.zshrc` / `~/.bashrc` if needed).

Verify:

```bash
which nns              # → $HOME/.local/bin/nns
nns --help             # (passes through to the underlying cli)
```

## 3. Configure

```bash
nano vesl.toml
```

### Local mode (default)

```toml
settlement_mode = "local"
```

No chain client needed. Claims settle to a local `.nns-data/` store.
Good for development and for testing the HTTP surface. The chain
follower's anchor loop is a no-op in this mode (see
[§ Follower debugging](#follower-debugging)).

### Chain mode

Three flavours of chain mode, chosen by `settlement_mode`:

| Mode | What it means | Typical use |
|---|---|---|
| `fakenet` | Real Nockchain protocol, single-node test fabric, deterministic | dev / CI |
| `dumbnet` | Public dev network with real-ish chain but throwaway value | integration, staging |
| `chain`   | Alias accepted by the config parser, treated as `dumbnet` | scripts that hard-code `"chain"` |
| `mainnet` | Production | real deploys |

Minimum chain-mode config:

```toml
settlement_mode = "dumbnet"                  # or "fakenet", "mainnet"
chain_endpoint  = "https://localhost:5556"   # your Nockchain gRPC (TLS)
tx_fee          = 256                         # optional, nicks per settle tx
```

**`chain_endpoint` can be HTTP or HTTPS.** If HTTPS, the hull's gRPC
client uses rustls with webpki roots; the `aws-lc-rs` provider is
installed at process start (see `src/main.rs`), which avoids the
"Could not automatically determine the process-level CryptoProvider"
panic that rustls 0.23 throws when multiple backends are visible.

With these lines the follower starts scanning blocks into the kernel
(`%scan-block`). Check `curl -s localhost:3000/status | jq .follower`
after ~30 s to confirm `chain_tip_height` is populated.

### Nockchain checkout: NoteData on outputs + wallet tooling

Path Y **requires** block/tx RPC responses that include **`note_data` on
each v1 output**; otherwise the scanner never sees `nns/v1/claim` notes.

- See **[`docs/nockchain-fork-for-nns.md`](nockchain-fork-for-nns.md)** for
  the **`../nockchain`** contract (note_data on outputs, wallet path).
- **Wallet limitation:** most wallets do not yet expose arbitrary
  multi-entry NoteData when building transactions — see
  **[`docs/claim-note-wallet-support.md`](claim-note-wallet-support.md)**.
- Upstream wallet CLI work: **[nockchain#85 — `create-tx --memo-data`](https://github.com/nockchain/nockchain/pull/85)**  
  (in review; may evolve toward recipient-level memo per maintainer feedback).
- **First-time Path Y validation:** use **[`docs/path-y-integration-test-plan.md`](path-y-integration-test-plan.md)** (manual phases + finality / negative cases).

## 4. Run

```bash
nns
```

Log lines on startup (all on stdout + `tracing`):

```
=== nns-vesl ===
  settlement mode: local
  payment address: 8s29X...WPzTT5
I (21:45:31) [no] kernel::boot: kernel: starting
I (21:45:31) [%build-hash 0v1u.44j19]
I (21:45:31) nockapp: Nockapp save interval duration: 120s
  kernel booted (18627784 bytes)
  state dir: ./.nns-data
  payment address bound: 8s29X...WPzTT5
I (21:45:31) api: listening on http://127.0.0.1:3000
Listening on http://127.0.0.1:3000
  POST /register
  POST /claim
  ...
```

Runtime env vars the binary respects:


| Var                | Default     | What                                |
| ------------------ | ----------- | ----------------------------------- |
| `BIND_ADDR`        | `127.0.0.1` | interface to bind                   |
| `API_PORT`         | `3000`      | HTTP port                           |
| `VESL_TOML`        | `vesl.toml` | config file path                    |
| `NNS_DATA_DIR`     | `.`         | where `.nns-data/` is created       |
| `NNS_KERNEL_JAM`   | `out.jam`   | kernel jam path                     |
| `RUST_LOG`         | `info`      | log verbosity — see [§ Logs](#logs) |
| `NNS_ENABLE_ADMIN` | unset       | enable admin routes (debug only)    |


Clean shutdown: `Ctrl-C` or `SIGTERM`. The binary flushes state
(mirror + kernel checkpoint) before exiting.

## 5. Monitoring

### `/status` — the primary health dashboard

```bash
curl -s http://127.0.0.1:3000/status | jq
```

```jsonc
{
  "settlement_mode": "local",
  "chain_endpoint":  null,
  "names_count":     7,
  "pending_count":   0,
  "registered_count": 7,
  "pending_batch_count": 7,
  "last_settled_claim_id": 0,
  "snapshot": { "claim_id": 7, "hull": "...", "root": "..." },
  "anchor":   { "tip_height": 120, "tip_digest": "42..." },
  "follower": {
    "chain_tip_height":            130,   // what the follower last saw
    "anchor_lag_blocks":           10,    // chain_tip - anchor.tip_height
    "is_caught_up":                true,  // lag within finality_depth + 1
    "last_advance_at_epoch_ms":    1735689600000,
    "last_advance_age_seconds":    23,
    "last_advance_tip_height":     120,
    "last_advance_count":          5,
    "last_chain_tip_observed_at_epoch_ms": 1735689610000,
    "last_error":                  null,
    "last_error_phase":            null,
    "last_error_at_epoch_ms":      null,
    "finality_depth":              10,
    "max_advance_batch":           1
  }
}
```

Interpret:

- `**is_caught_up: true**` and `**last_error: null**` → follower healthy.
- `**is_caught_up: false` and growing `anchor_lag_blocks**` → follower
falling behind. Check chain endpoint health, inspect `last_error`.
- `**last_advance_age_seconds` > 60** → follower hasn't moved in a
minute even though the scan tick runs every 2 s. Probably stuck
on a Nockchain RPC or the kernel is rejecting scans.
- `**chain_tip_height: null`** → follower has never reached the chain.
Check `settlement_mode` and `chain_endpoint`.

### `/health` — cheap liveness probe

```bash
curl -s http://127.0.0.1:3000/health
# {"status":"ok"}
```

Always 200 if the HTTP server is up. Doesn't assert anything about
kernel or follower health — use `/status` for that.

### Logs

Default is `RUST_LOG=info`. For follower-specific debugging:

```bash
RUST_LOG=info,nns_vesl::chain_follower=trace,nns_vesl::chain=debug nns
```

Structured fields the follower emits:

```
INFO phase=scan_block height=120 chain follower scanned block
WARN phase=scan_block err="block details query failed at height 120: ..." chain follower scan tick failed
TRACE phase=scan_block scan tick no-op
```

Grep-friendly — `rg 'phase=scan_block.*WARN'` catches scanner failures.

### Alerting (suggested)


| Signal                                        | Threshold           | Action                                      |
| --------------------------------------------- | ------------------- | ------------------------------------------- |
| `follower.is_caught_up == false`              | sustained > 5 min   | investigate chain endpoint                  |
| `follower.anchor_lag_blocks`                  | > 50                | alert, check `last_error_phase`             |
| `follower.last_advance_age_seconds`           | > 120 in chain mode | scanner stuck                               |
| `follower.last_error_phase == "scan_poke"`    | any                 | kernel rejected a scan — possible reorg     |
| Process missing                               | n/a                 | supervise with systemd/launchd              |


## 6. Follower debugging

If `/status.follower.chain_tip_height == null` in chain mode:

```bash
# 1. confirm chain endpoint reachable
#    (-plaintext for http://, omit for https://)
grpcurl -plaintext localhost:5556 list | head -5

# 2. crank tracing to see what the follower attempts
RUST_LOG=info,nns_vesl::chain_follower=trace,nns_vesl::chain=debug nns

# 3. watch the scanner move
watch -n 2 'curl -s http://127.0.0.1:3000/status | jq .scan_state'
```

Common causes of no scan progress:

- `**local mode**` — `settlement_mode = "local"` in vesl.toml. Set to `"chain"`.
- `**endpoint missing**` — `chain_endpoint` not set. Add it.
- `**within finality depth**` — chain tip < NNS scan height + `finality_depth` (default 10). Wait for chain to advance.

## 7. Claim lookup flow

Users submit names by publishing tagged `nns/v1/claim` transactions to
Nockchain. NNS is now a read-only scanner/indexer: it follows finalized
blocks, folds valid claims into the accumulator, and serves accumulator
lookups.

```bash
HOST=http://127.0.0.1:3000
curl -s "$HOST/accumulator/alice.nock" | jq
```

Example response:

```json
{
  "name": "alice.nock",
  "value": {
    "owner": "8s29X...",
    "tx_hash": "42...",
    "claim_height": 120,
    "block_digest": "99..."
  },
  "last_proved_height": 130,
  "last_proved_digest": "aa...",
  "accumulator_root": "bb...",
  "accumulator_size": 1000
}
```

In Y2 this is an honest-indexer response. Y3 adds the recursive proof and
z-map inclusion/non-inclusion proofs needed for offline wallet
verification.

## 8. Data layout

Everything NNS writes at runtime lives under `$NNS_DATA_DIR/.nns-data/`
(default `./.nns-data`):

```
.nns-data/
├── checkpoints/        # kernel state snapshots (periodic + on-poke)
├── pma/                # nockapp persistent memory arena
└── .nns-mirror.json    # hull mirror (denormalized read cache)
```

Backing up:

- **State dir** — the whole `.nns-data/` atomically (the kernel fsyncs
internally on `persist_all`; copying mid-run risks a partial
checkpoint but not corruption of older ones).
- `**vesl.toml`** — required to re-boot against the same treasury.

Restoring: drop the `.nns-data/` back in place and start `nns`. The
kernel prints `Successfully imported kernel state from: ...` when it
picks up a checkpoint.

If the kernel jam changed between backup and restore, you'll see:

```
W checkpoint kernel hash mismatch; loading checkpoint state into current kernel
```

This branch intentionally breaks old kernel state while Path Y is still
pre-release. If you hit a checkpoint shape mismatch, wipe `.nns-data/`
and rescan from chain.

### Troubleshooting: `nest-fail` during `make install`

If `hoonc` aborts a fresh build with a trace like:

```
nest-fail
-have.[i=@tD t=""]
-need.@
/lib/vesl-stark-verifier.hoon::[541 27].[541 35]
```

that used to mean `$VESL_HOME` was checked out to a commit whose
`vesl-stark-verifier.hoon` had drifted from what NNS expected. It is
now auto-resolved: `vesl-stark-verifier.hoon` is **vendored** (checked
into this repo at `hoon/lib/vesl-stark-verifier.hoon` rather than
symlinked). `scripts/setup-hoon-tree.sh` leaves the vendored file in
place even when you re-run it. The header banner at the top of the
file documents the upstream divergence and the refresh procedure for
the day the patch lands in vesl main.

If you still see the nest-fail after pulling NNS, your checkout is
missing the vendored copy — `git status hoon/lib/vesl-stark-verifier.hoon`
and `git checkout` it.

## 9. Chain-replay bootstrap (chain mode)

Fresh chain-mode node picks up existing NNS claims by **replaying
chain-ordered claim notes** from the follower:

```bash
# 1. start fresh
rm -rf .nns-data
nns

# 2. the follower scans finalized blocks and pokes %scan-block into the
#    kernel. Claims are folded into the accumulator first-writer-wins.

# 3. monitor progress
watch -n 5 'curl -s http://127.0.0.1:3000/status | jq "{scan_state, follower: {anchor_lag_blocks, last_advance_tip_height}}"'
```

Replay catches up when `anchor_lag_blocks` drops below `finality_depth`.

## 10. Where to look next

- **Trust model + attack surface** — `ARCHITECTURE.md` §5–§7
- **Path Y4 `light_verify`** (pinned checkpoint, no live chain RPC) — `src/bin/light_verify.rs --help`, `docs/wallet-verification.md`
- **API reference** — `src/api.rs` (each handler's doc-comment)
- **Config surface** — `src/config.rs`, `vesl.toml.example`
- **Roadmap** — `ARCHITECTURE.md` §11

