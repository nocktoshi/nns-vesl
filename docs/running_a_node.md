# Running an NNS Node

Bare-minimum operator runbook — how to install, configure, run, and
monitor an NNS node. Everything below assumes a recent macOS or Linux
host with `cargo +nightly` available.

For the full architecture and design rationale, see
[`../ARCHITECTURE.md`](../ARCHITECTURE.md).

---

## Quick Start

```bash
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

| Tool | Why |
|---|---|
| Rust nightly | kernel uses nightly features in upstream crates |
| `jq` | you'll want to pipe JSON responses through it |
| `curl` or HTTPie | query the local HTTP API |
| 2 GB free RAM | kernel + mirror + axum |
| 1 GB free disk | per-poke checkpoints in `.nns-data/` |

Check Rust:

```bash
rustup toolchain list | grep nightly
# install if missing:  rustup toolchain install nightly
```

## 2. Install

```bash
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

```toml
settlement_mode = "chain"
chain_endpoint  = "http://localhost:50051"  # your Nockchain gRPC
tx_fee          = 100                        # optional, nicks per settle tx
```

With these three lines the follower's two loops start actually doing
work — anchor advance every 10 s, claim replay every 2 s.

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

| Var | Default | What |
|---|---|---|
| `BIND_ADDR` | `127.0.0.1` | interface to bind |
| `API_PORT` | `3000` | HTTP port |
| `VESL_TOML` | `vesl.toml` | config file path |
| `NNS_DATA_DIR` | `.` | where `.nns-data/` is created |
| `NNS_KERNEL_JAM` | `out.jam` | kernel jam path |
| `RUST_LOG` | `info` | log verbosity — see [§ Logs](#logs) |
| `NNS_ENABLE_ADMIN` | unset | enable admin routes (debug only) |

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
    "max_advance_batch":           64
  }
}
```

Interpret:

- **`is_caught_up: true`** and **`last_error: null`** → follower healthy.
- **`is_caught_up: false` and growing `anchor_lag_blocks`** → follower
  falling behind. Check chain endpoint health, inspect `last_error`.
- **`last_advance_age_seconds` > 60** → follower hasn't moved in a
  minute even though the anchor tick runs every 10 s. Probably stuck
  on a Nockchain RPC or the kernel is rejecting advances.
- **`chain_tip_height: null`** → follower has never reached the chain.
  Check `settlement_mode` and `chain_endpoint`.

### `/anchor` — just the anchor surface

```bash
curl -s http://127.0.0.1:3000/anchor | jq
```

Same shape as `/status.follower` + `/status.anchor` merged. 503 when
the node has no kernel anchor peek AND no follower observations
(completely blind).

```bash
# Is my node caught up?
curl -s http://127.0.0.1:3000/anchor | jq .is_caught_up
```

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
INFO phase=anchor_advance tip_height=120 count=5 chain follower advanced anchor
WARN phase=anchor_tick err="header chain fetch failed [111..120]: gRPC unreachable" chain follower anchor tick failed
WARN phase=claim_tick err="chain position lookup failed: ..." chain follower claim tick failed
TRACE phase=anchor_advance anchor tick no-op
```

Grep-friendly — `rg 'phase=anchor_tick.*WARN'` catches anchor-loop
failures without matching claim-loop noise.

### Alerting (suggested)

| Signal | Threshold | Action |
|---|---|---|
| `follower.is_caught_up == false` | sustained > 5 min | investigate chain endpoint |
| `follower.anchor_lag_blocks` | > 50 | alert, check `last_error_phase` |
| `follower.last_advance_age_seconds` | > 120 in chain mode | anchor stuck |
| `follower.last_error_phase == "advance_poke"` | any | kernel rejected an advance — possible reorg |
| Process missing | n/a | supervise with systemd/launchd |

## 6. Follower debugging

If `/status.follower.chain_tip_height == null` in chain mode:

```bash
# 1. confirm chain endpoint reachable
grpcurl -plaintext localhost:50051 list | head -5

# 2. crank tracing to see what the follower attempts
RUST_LOG=info,nns_vesl::chain_follower=trace,nns_vesl::chain=debug nns

# 3. force a single advance to bypass the 10s tick
export NNS_ENABLE_ADMIN=1        # enable admin routes
nns &                             # or restart the node
curl -X POST http://127.0.0.1:3000/admin/advance-tip-now
# → {"advanced": true, "tip_height": 120, "count": 5}
# or
# → {"advanced": false, "reason": "no-op (local mode, endpoint missing, or within finality depth)"}
```

Common causes of `advanced: false`:

- **`local mode`** — `settlement_mode = "local"` in vesl.toml. Set to `"chain"`.
- **`endpoint missing`** — `chain_endpoint` not set. Add it.
- **`within finality depth`** — chain tip < NNS anchor + `finality_depth` (default 10). Wait for chain to advance.

Never enable `NNS_ENABLE_ADMIN` on a public-facing node. The admin
routes aren't authenticated. Scanners see 404 when it's disabled —
no fingerprinting.

## 7. Claim flow

Register a name, claim it, fetch a proof. This is what a real wallet
does end-to-end against your node.

```bash
HOST=http://127.0.0.1:3000
ADDR='8s29XUK8Do7QWt2MHfPdd1gDSta6db4c3bQrxP1YdJNfXpL3WPzTT5'

# 1. reserve pending registration (optional)
curl -s -X POST "$HOST/register" \
  -H 'content-type: application/json' \
  -d "{\"address\":\"$ADDR\",\"name\":\"alice.nock\"}"

# 2. claim with a txHash (in local mode, generate a stub;
#    in chain mode, pass the real payment tx-id)
curl -s -X POST "$HOST/claim" \
  -H 'content-type: application/json' \
  -d "{\"address\":\"$ADDR\",\"name\":\"alice.nock\",\"fee\":5000,\"txHash\":\"stub-$(uuidgen)\"}"

# 3. fetch the Merkle + anchor proof
curl -s "$HOST/proof?name=alice.nock" | jq

# 4. verify locally (optional)
curl -s "$HOST/proof?name=alice.nock" \
  | light_verify --chain-tip $(curl -s "$HOST/status" | jq -r '.follower.chain_tip_height // 0') \
                 --max-staleness 20
```

`light_verify` output:

```
verified: alice.nock
  owner:      8s29X...WPzTT5
  tx_hash:    stub-ab12...34cd
  claim_id:   1
  root:       b4b7e1...5dc10
  hull:       0a8137...3d05
  anchor:     height=120 digest=42...

checks:
  [PASS] merkle inclusion    (3 siblings)
  [PASS] anchor freshness    (tip 120, lag 10, max 20)
  [SKIP] anchor binding      (no --chain-tip-digest)
```

Exit codes are scriptable — see `light_verify --help`.

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
- **`vesl.toml`** — required to re-boot against the same treasury.

Restoring: drop the `.nns-data/` back in place and start `nns`. The
kernel prints `Successfully imported kernel state from: ...` when it
picks up a checkpoint.

If the kernel jam changed between backup and restore, you'll see:

```
W checkpoint kernel hash mismatch; loading checkpoint state into current kernel
```

That's usually fine (new kernel can read older state as long as the
`+$state` shape is backwards-compatible), but some peek paths may hit
deterministic exits on exotic edge cases. The `/proof` handler has a
mirror-cache fallback for this specifically.

## 9. Chain-replay bootstrap (chain mode)

Fresh chain-mode node picks up existing NNS claims by **replaying
chain-ordered claim notes** from the follower:

```bash
# 1. start fresh
rm -rf .nns-data
nns

# 2. the follower discovers nns/v1/claim notes on chain and pokes
#    them into the kernel in block order — the kernel re-applies
#    every claim deterministically.

# 3. monitor progress
watch -n 5 'curl -s http://127.0.0.1:3000/status | jq "{names_count, follower: {anchor_lag_blocks, last_advance_tip_height}}"'
```

Replay completes when `anchor_lag_blocks` drops below `finality_depth`
and `names_count` stops changing.

## 10. Where to look next

- **Trust model + attack surface** — `ARCHITECTURE.md` §5–§7
- **Freshness / `light_verify`** — `src/bin/light_verify.rs --help`
- **API reference** — `src/api.rs` (each handler's doc-comment)
- **Config surface** — `src/config.rs`, `vesl.toml.example`
- **Roadmap** — `ARCHITECTURE.md` §11
