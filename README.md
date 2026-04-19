# nns-vesl

<img width="1416" height="540" alt="NNS - Nockchain Name Service" src="https://github.com/user-attachments/assets/7793ed6f-766a-4e93-a2b4-834c04843c37" /> <br />

NNS — the Nockchain Name Service. On-chain `.nock` name registrar,
ported from the centralized Cloudflare Worker at `api.nocknames.com`
to a Vesl-grafted NockApp.

This follows the `data-registry` pattern from the Vesl templates: a
small kernel holding the authoritative registry
(`names=(map @t [owner tx-hash])`, `tx-hashes=(set @t)`, and
`primaries=(map @t @t)`) with the Vesl graft wired in for on-demand
settlement. Split of authority:

- **Hot path = `%claim` poke.** The kernel enforces four domain
  rules directly; no graft involvement, no STARK per registration:
    - **C1 — format**: stem is non-empty, `[a-z0-9]+.nock`.
    - **C2 — fee adequacy**: declared fee ≥ the tier for the stem's
      length (`5000 / 500 / 100` nicks, matching the legacy worker).
    - **C3 — name uniqueness**: `name` must not already be in
      `names`. On duplicate the kernel emits `[%claim-error 'name
      already registered']` without mutating state; the hull turns
      that into a `400`.
    - **C4 — payment uniqueness**: `tx-hash` must not already be in
      `tx-hashes`. On duplicate the kernel emits `[%claim-error
      'payment already used']` without mutating state; the hull
      turns that into a `400`. One payment, one name, enforced by
      the same authority that enforces name uniqueness.
    - C1/C2 violations crash (`?>`) — an honest hull never sends
      those, so the crash signals a bug, not a user error.
    - On a successful claim the kernel also emits
      `[%primary-set owner name]` iff the owner had no primary yet.
      This makes the first-claimed name the auto-default for
      `/resolve?address=`.
- **Reverse-lookup path = `%set-primary` poke.** An owner can own
  any number of names; `primaries` stores the one they want
  `/resolve?address=` to return. The kernel enforces two rules on
  `%set-primary`:
    - **P1 — name exists**: target name must be in `names`.
    - **P2 — ownership**: `names[name].owner == address`. Violations
      emit `[%primary-error <msg>]`; the hull turns them into `400`.
- **Commitment path = claim-id-bumped auto-register.** Every
  successful `%claim` bumps a `claim-id` counter, recomputes the
  Merkle root over the full `names` map (canonical: keys sorted
  with `aor`, leaves = `jam([name owner tx-hash])`), derives a
  fresh `hull-id = hash-pair(hash-leaf('nns'), hash-leaf(claim-id))`,
  and internally pokes `%vesl-register hull root` against the
  graft. Hulls are **commitments, not mutable pointers**: each
  claim-id gets its own permanent row in `registered`. The kernel
  emits `[%claim-id-bumped claim-id hull root]` so the hull can
  cache the current snapshot without peeking.
- **Settlement path = `%settle-batch` poke.** Settlement is
  batched: a single `POST /settle` rolls up every name claimed
  since the last successful settle into one STARK-backed graft
  note. The kernel:
    1. Walks `names` in canonical (`aor`-sorted) order and picks
       every entry whose `entry.claim-id > last-settled-claim-id`
       — one contiguous window, no explicit list to maintain.
    2. Builds a Merkle inclusion proof for each selected name
       against the *current* `root` (which commits to the full
       `names` map, not the batch subset).
    3. Jams `[[note-id hull root [%pending ~]] batch root]` and
       pokes `%vesl-settle jammed`, where
       `note-id = hash-leaf(jam(batch))` — a deterministic,
       content-addressed batch id. The graft verifies with
       `nns-gate`, which checks each `(name, owner, tx-hash,
       proof)` in turn:
         - **G1 — format**: `is-valid-name` for every leaf.
         - **G2 — Merkle inclusion**: every leaf's
           `verify-chunk(jam([name owner tx-hash]), proof, root)`
           succeeds.
    4. On `%vesl-settled`, advances `last-settled-claim-id` to
       the current `claim-id.state` (invariant: it equals the
       highest `entry.claim-id` in the batch), emits
       `[%batch-settled claim-id count note-id]` alongside the
       graft's receipt, and drains the pending window.
  Replay protection is *per-batch*: the same leaf set produces
  the same `note-id`, so the graft rejects resubmitting an
  identical batch. Any individual name can still be re-settled
  later as part of a different batch (the `note-id` will differ
  because `batch` differs).
  Empty windows short-circuit to `[%batch-error 'nothing to
  settle']` without a graft poke.

Everything user-visible but not load-bearing for the chain (pending
reservations, timestamps, reverse-lookup index mirror,
search-response shaping) lives in the Rust hull.

- The Rust hull serves the same HTTP API the old worker served —
  existing clients do not need changes.
- Pending reservations live only in the hull mirror; a name hits the
  kernel exactly once, via `%claim`, at `/claim` time.
- Payment-replay protection is *on-kernel*: the `tx-hashes` set is
  part of the same jammed state a STARK attests to, so there is no
  hull-side cache to fall out of sync or get wiped across restarts.
- The Vesl graft (`registered`, `settled`) is written to on every
  `%claim` (fresh hull per claim-id) and on every `%vesl-settle`
  (replay-protected by batch `note-id`). `/settle` produces a
  single receipt covering every name claimed since the last
  settle, against the current commitment on demand; no poke to
  Nockchain yet — that's a follow-up.

## Architecture

Three layers, with a hard trust boundary between the hull and the
kernel. The hull is rebuildable; the kernel is the system of
record. The graft is embedded inside the kernel as a state
fragment, not a separate process.

```
+---------------------------------------------------------------------+
|                           HTTP client                               |
+---------------------------------+-----------------------------------+
                                  |  JSON over HTTP
                                  v
+---------------------------------------------------------------------+
|  Rust hull (axum)       advisory, rebuildable                       |
|                                                                     |
|    handlers -------> payment::verify                                |
|      (api.rs)          (stub today; chain client later)             |
|       |                                                             |
|       +---> NockApp.poke / .peek  (holds kernel via tokio Mutex)    |
|       |                                                             |
|       +---> Mirror cache  ----> .nns-data/.nns-mirror.json          |
|               . pending reservations   (hull-only)                  |
|               . primaries index        (driven by kernel effects)   |
|               . snapshot cache         (claim-id, hull, root)       |
+---------------------------------+-----------------------------------+
                                  |  pokes:  %claim, %set-primary,
                                  |           %settle-batch
                                  |  peeks:  /owner/<n>, /snapshot,
                                  |           /pending-batch, /last-settled
                                  v
+---------------------------------------------------------------------+
|  Hoon kernel (app.hoon)  authoritative, STARK-provable              |
|                                                                     |
|  versioned-state                                                    |
|  +-- names          = (map @t [owner tx-hash claim-id])             |
|  +-- tx-hashes      = (set @t)          payment-replay guard        |
|  +-- primaries      = (map @t @t)       owner -> primary name       |
|  +-- claim-id       = @ud               monotonic counter           |
|  +-- last-settled   = @ud               settle cursor               |
|  +-- root, hull     = @                 latest commitment cache     |
|  +-- vesl-state  (vesl-graft, embedded)                             |
|         +-- registered = (map hull root)   append-only hull history |
|         +-- settled    = (set note-id)     batch-replay guard       |
|                                                                     |
|  nns-gate: G1 name format + G2 batch Merkle inclusion               |
|            (what the STARK proves; see "Proof scope")               |
+---------------------------------+-----------------------------------+
                                  |  whole kernel state, jammed
                                  v
                   $NNS_DATA_DIR/.nns-data/checkpoints/{0,1}.chkjam
                   (written inline after every mutating poke,
                    plus once more on SIGINT/SIGTERM)
```

Two properties worth naming explicitly:

- **Trust boundary is between hull and kernel, not between kernel
  and graft.** The graft is Hoon code living in the same jammed
  state a STARK attests to; pokes from the hull cross the
  boundary, pokes from the kernel to the graft don't. That's why
  payment-replay (`tx-hashes`) and name-uniqueness (`names`) can
  sit kernel-side with the same integrity guarantees as the graft's
  own `registered` / `settled` maps.
- **Durability is handled in two places at once.** The kernel
  state is jammed by `NockApp::save_blocking` after every mutating
  poke (and again on SIGINT/SIGTERM); the hull mirror is JSON-saved
  in the same handler. If either write fails the other still
  completes, and on restart the handler-mirror is rebuildable from
  the kernel so a missing `.nns-mirror.json` loses at most the
  pending reservation set.

The interaction detail — which pokes each endpoint issues, what
effects come back, and how the hull maps them to HTTP status codes
— is in the per-endpoint diagram below.

```
HTTP client
    |
    v
Rust hull (axum) ---- pending-only mirror + primaries mirror
    |
    |  per /claim, one poke:
    |    %claim name=@t owner=@t fee=@ud tx-hash=@t
    |      -> %claimed name owner tx-hash              on success
    |         (+ %primary-set owner name if first claim for this owner)
    |      -> %claim-error 'name already registered'   (hull: 400)
    |      -> %claim-error 'payment already used'      (hull: 400)
    |      -> crash                          on bad format/fee (hull bug: 500)
    |
    |  per /primary, one poke:
    |    %set-primary address=@t name=@t
    |      -> %primary-set address name                on success
    |      -> %primary-error 'name not registered'     (hull: 400)
    |      -> %primary-error 'not the owner'           (hull: 400)
    |
    |  per /settle, one poke (batched):
    |    %settle-batch ~
    |      -> %batch-settled claim-id count note-id    on success
    |         + %vesl-settled [id hull root [%settled ~]]
    |      -> %batch-error 'nothing to settle'         (hull: 400)
    |      -> %vesl-error <msg>                        (hull: 400)
    v
Hoon kernel = names=(map @t name-entry)          <- authoritative registry
                         name-entry = [owner=@t tx-hash=@t claim-id=@ud]
            + tx-hashes=(set @t)                 <- payment replay guard
            + primaries=(map @t @t)              <- reverse-lookup target
            + claim-id=@ud                       <- claim counter (monotonic)
            + last-settled-claim-id=@ud          <- highest claim-id in
                                                    most recently settled batch
            + root=@                             <- Merkle root over names
            + hull=@                             <- current hull-id
            + vesl=vesl-state                    <- graft commitments
```

The Merkle tree deliberately covers *only* `names`. `primaries`
is a nice-to-have reverse-lookup convenience, not a load-bearing
domain invariant, so changing it does NOT bump the claim-id or the
root — `/settle` would otherwise churn a new hull on every primary
flip for no settlement benefit.

## Requirements

- Nightly Rust: `rustup toolchain install nightly`
- Sibling clones on the same parent directory:
  ```
  ~/nockchain      (NockVM)
  ~/vesl           (Vesl SDK — this crate depends on /dev)
  ~/nns-vesl       (this project)
  ```
  (Paths are configurable via `Cargo.toml` — see "Moving the repo" below.)
- `hoonc` on PATH (bundled in the nockchain clone; `cargo install --path
  .../nockchain/crates/hoonc` if needed).

## Quick start

```bash
# 1. compile the kernel (writes out.jam alongside it)
hoonc --new hoon/app/app.hoon hoon/

# 2. build the hull
cargo +nightly build --release

# 3. run
./target/release/nns-vesl
```

Once started:

```bash
curl -X POST http://127.0.0.1:3000/register \
  -H 'content-type: application/json' \
  -d '{"address":"8s29XUK8Do7QWt2MHfPdd1gDSta6db4c3bQrxP1YdJNfXpL3WPzTT5","name":"nns.nock"}'

curl -X POST http://127.0.0.1:3000/claim \
  -H 'content-type: application/json' \
  -d '{"address":"8s29XUK8Do7QWt2MHfPdd1gDSta6db4c3bQrxP1YdJNfXpL3WPzTT5","name":"nns.nock"}'

curl http://127.0.0.1:3000/resolve?name=nns.nock
curl http://127.0.0.1:3000/status
```

## HTTP API

Endpoint names mirror the kernel's pokes (`POST /claim` →
`%claim`, `POST /primary` → `%set-primary`). This is a breaking
departure from the legacy Cloudflare worker, which used
`POST /verify`; migrate clients by renaming the path. CORS is
open (`*`).

| Method | Path                     | Purpose                                               |
| ------ | ------------------------ | ----------------------------------------------------- |
| POST   | `/register`              | Create a pending reservation (`{address, name}`)      |
| POST   | `/claim`                 | Promote pending → registered (`{address, name}`), pokes kernel `%claim` |
| POST   | `/primary`               | Designate which of caller's names is primary (`{address, name}`), pokes kernel `%set-primary` |
| POST   | `/settle`                | Produce one batch receipt for every name claimed since the last settle (empty body); pokes kernel `%settle-batch` |
| GET    | `/snapshot`              | Current commitment `{claim_id, hull, root}` (hex-encoded) |
| GET    | `/pending-batch`         | Names waiting to be settled `{count, names[], last_settled_claim_id}` |
| GET    | `/pending`               | All pending entries, newest first                     |
| GET    | `/verified`              | All registered entries, newest first                  |
| GET    | `/resolve?name=X`        | `{address}` or 404                                    |
| GET    | `/resolve?address=X`     | `{name}` — the address's **primary** — or 404         |
| GET    | `/search?name=X`         | `{name, price, status, owner?, registeredAt?}`        |
| GET    | `/search?address=X`      | `{address, pending[], verified[], primary?}`          |
| GET    | `/status`                | Diagnostic: counts, merkle root, settlement mode      |
| GET    | `/health`                | `{status: "ok"}`                                      |

One address can own any number of names. `/resolve?address=` returns
the owner's **primary** name (set by the kernel to the first claim
for that owner, or to whatever the user last sent to `/primary`).
`/primary` is owner-gated in the kernel — a `%set-primary` whose
`address` does not match `names[name].owner` is rejected with a
`%primary-error` and the mirror is not updated.

Fee tiers (ported from `nock-names-worker/src/utils/constants.ts`):

| Stem length | Fee (nocks) |
| ----------- | ----------- |
| 1-4 chars   | 5000        |
| 5-9 chars   | 500         |
| 10+ chars   | 100         |

## Configuration

Three layers, in precedence order (highest wins): CLI flags, env vars,
`vesl.toml`. The hull honors:

| Env var              | Purpose                                            | Default   |
| -------------------- | -------------------------------------------------- | --------- |
| `API_PORT`           | HTTP port                                          | `3000`    |
| `BIND_ADDR`          | HTTP bind address                                  | `127.0.0.1` |
| `NNS_DATA_DIR`       | Root dir for kernel checkpoints + mirror snapshot  | `.`       |
| `NNS_KERNEL_JAM`     | Path to the compiled kernel                        | `out.jam` |
| `VESL_TOML`          | Path to settlement config                          | `vesl.toml` |
| `RUST_LOG`           | Tracing filter (passed to `tracing_subscriber`)    | unset     |

Vesl settlement config in `vesl.toml`:

```toml
# v1: kernel verifies, no chain interaction.
settlement_mode = "local"

# uncomment and flip to "fakenet" or "dumbnet" for real chain:
# chain_endpoint       = "http://localhost:9090"
# tx_fee               = 256
# accept_timeout_secs  = 300
```

## Data layout

Under `$NNS_DATA_DIR` (default CWD):

```
.nns-data/
  checkpoints/                   # kernel jammed-state snapshots (NockApp)
  pma/                           # NockApp persistent memory arena
  .nns-mirror.json               # hull read-cache (names, primaries, pending)
```

Everything the hull writes at runtime lives in the single
`.nns-data/` directory, which is safe to gitignore and safe to
delete between runs (losing all state). `$NNS_DATA_DIR` itself
stays untouched by the hull — only its `.nns-data/` child is.

Split of authority:

- **Kernel jam** — the authoritative `names=(map @t [owner tx-hash])`
  registry, the `tx-hashes=(set @t)` payment-replay index, the
  `primaries=(map @t @t)` reverse-lookup index (owner → designated
  primary name), and `vesl-state` (currently unused; populated on
  settlement). This is the state a STARK proof attests to: every
  `%claim` poke the kernel accepted is, by construction, one that
  passed C1/C2/C3/C4; every `%set-primary` it accepted is one where
  `names[name].owner == address` (rule P2).
- **Hull mirror** — denormalized read-cache for HTTP handlers:
  pending reservations with timestamp/date and the reverse
  `address -> primary name` index. The mirror's `primaries` field is
  populated only from kernel `%primary-set` effects — never from
  blind "last write wins" on insert — so an address that owns many
  names has a single, deterministic reverse-lookup target. Nothing
  authoritative lives here.

The hull mirror is advisory, not authoritative: even if it is wiped
mid-session the kernel still rejects duplicate names *and* duplicate
payments. Two regression tests cover this:

- `kernel_rejects_duplicate_even_when_mirror_forgets` — clears the
  mirror after a successful claim and confirms a second claim on the
  same name returns `%claim-error 'name already registered'`.
- `kernel_rejects_duplicate_tx_hash` — pokes the kernel directly
  with the same `tx-hash` under a different name and confirms the
  kernel returns `%claim-error 'payment already used'`.

The mirror is written atomically after every successful mutation so
crashes never leave it ahead of the kernel.

## Testing

```bash
# Hoon compile-time domain tests
hoonc --new --arbitrary hoon/tests/names.hoon hoon/

# Rust unit + handler tests (boots the real kernel per test)
cargo +nightly test

# API parity against the legacy worker
scripts/parity.py \
  --old https://nocknames.nockblocks.workers.dev \
  --new http://127.0.0.1:3000
```

## Settlement

Settlement is **batched** and **on** for commitments and
receipts, **off** for on-chain posting. Every `%claim` bumps
`claim-id` and writes `entry.claim-id = new claim-id`; `/settle`
rolls up *every* name with `entry.claim-id >
last-settled-claim-id` into a single `%vesl-settle` note against
the current commitment. The kernel handles batch selection,
Merkle proof generation (one traversal per name, against the
same `root`), `note-id` derivation (`hash-leaf(jam(sorted
batch))`), and graft dispatch as one atomic poke — no hull-side
coordination needed.

`POST /settle` takes an **empty body** and returns
`{claim_id, count, names[], hull, root, note_id}` where `hull`,
`root`, and `note_id` are hex-encoded raw atom bytes, `names` is
the canonically-sorted list of names covered by this batch, and
`claim_id` is the highest `claim-id` in the batch (which, by
invariant, equals the current `claim-id.state`). No chain poke
is emitted yet; wiring that up is a drop-in at the end of
`settle_handler`.

`GET /pending-batch` returns `{count, names[],
last_settled_claim_id}` for the exact window the next `/settle`
will cover — handy for clients that want to preview or batch
their claims until the window is worth settling.

Replay protection is *per-batch*. The graft's `settled` set
dedupes on `note-id = hash-leaf(jam(sorted-batch))`, so two
callers racing the same pending window produce the same
`note-id` and only one wins — the other gets a `%vesl-error
'note already settled'`. An individual name can still be
re-settled later as part of a *different* batch (containing at
least one additional or missing leaf), because the batch
content — and thus the `note-id` — will differ. An empty
pending window short-circuits to `400 "nothing to settle"`
without a graft poke.

### Proof scope

What the STARK currently attests to (`nns-gate` predicates):

- **G1 — name format.** Every leaf's `name` matches the kernel's
  `is-valid-name` predicate (non-empty `[a-z0-9]+.nock`).
- **G2 — Merkle inclusion.** Every leaf's
  `jam([name owner tx-hash])` hashed at level 0 and walked
  through its `proof` yields `expected-root`, i.e. "these
  `(name, owner, tx-hash)` triples were all registry rows at
  the commitment `root`."

What the STARK does **not** attest to — these are **trusted
kernel code**, not verified by the proof:

- **C3 — name uniqueness.** The kernel's `names` map rejects a
  second `%claim` for the same name, but the proof sees only
  one snapshot and cannot attest to "this name was never
  claimed by a different owner in an earlier commitment."
- **C4 — payment uniqueness.** The kernel's `tx-hashes` set
  rejects duplicate `tx-hash`es across all `%claim`s, but the
  proof does not carry the `tx-hashes` set and so cannot
  attest to "this `tx-hash` is unique in the history."
- **Claim-id monotonicity and honest hull/root derivation.**
  The kernel computes `claim-id = claim-id + 1`,
  `root = compute-root(sorted-leaves(names))`, and
  `hull = hull-for(claim-id)` deterministically on every
  `%claim`, but the proof does not retrace those transitions.
- **`last-settled-claim-id` advancement.** The kernel bumps it
  on successful settlement; the proof says nothing about
  whether any particular claim has been settled yet.
- **Payment attestation.** No chain-side check of "address
  paid fee" — see the `/claim` TODO.

This is a known, tracked gap. The upgrade path (future work) is
**provable claim transitions**: widen `nns-gate` to verify a
sequence of `%claim` events and their state transitions, so the
proof attests not just "these rows are in this commitment" but
"this commitment is the deterministic result of applying these
claims in order to the empty registry." That requires either
replaying the claim log inside the gate or adopting an
accumulator (e.g., a sparse Merkle tree + non-membership
proofs) so C3/C4 checks become verifiable instead of trusted.
Tracked as TODO below.

To post settlements to Nockchain once that path is desired:

1. After a successful `%vesl-settle` effect, submit the same
   jammed `graft-payload` as a chain note via
   `nockchain-client-rs`.
2. Guard chain calls with a timeout and surface transient failures
   as `503` — retries are safe because the graft's `settled` set
   already rejects double-submits on the kernel side.
3. Flip `settlement_mode` in `vesl.toml` to `"fakenet"` /
   `"dumbnet"` and populate `chain_endpoint`, `tx_fee`,
   `accept_timeout_secs`, and a signing key (env
   `NOCK_SEED_PHRASE` or CLI flag — see
   `vesl_core::SettlementConfig::resolve`).

## Graduation path to real payment verification

v1 intentionally stubs payment verification. Today
`src/payment.rs::verify` returns `Ok(format!("stub-{uuid}"))`
unconditionally. To wire real payment:

1. Reimplement `verify(address, name, required_fee)` against a real
   chain client (the legacy TypeScript lives at
   `~/nock-names-worker/src/services/blockchain.ts` — port
   that logic).
2. Return the real `tx_hash` on success.
3. Keep the existing interface: the kernel's `tx-hashes` set
   already enforces `tx_hash` uniqueness, so payment-replay
   protection is automatic — no hull bookkeeping required.
4. To move payment attestation into the STARK proof, widen
   `nns-gate`'s per-leaf data shape in `hoon/app/app.hoon` to
   include `tx_hash` (already there) + signature bytes, add
   predicates alongside G1/G2, and extend the payload the
   kernel's `%settle-batch` arm jams as `batch`.

## TODO

- [ ] **Provable claim transitions.** Upgrade `nns-gate` from
  "these rows are in this commitment" (G1/G2) to "this
  commitment is the deterministic result of applying a sequence
  of `%claim` events to the empty registry, each satisfying
  C1/C2/C3/C4." Today C3 (name uniqueness), C4 (payment
  uniqueness), hull/root derivation, and `last-settled-claim-id`
  advancement are trusted kernel code — the STARK proves only
  inclusion and name format (see "Proof scope" under
  Settlement). Options:
    1. Verify a log of `%claim` events inside the gate,
       replaying each into a running `names` / `tx-hashes`
       snapshot and checking uniqueness against that snapshot.
       Linear in claim history; fine as a first pass.
    2. Maintain a sparse Merkle tree (or similar accumulator)
       alongside `names` so uniqueness becomes a verifiable
       non-membership proof on the prior commitment. Constant
       per-claim overhead but more Hoon machinery.
  The `claim-id` ladder (append-only, one hull per claim) is
  already the right shape for option 1 — each prior hull is a
  previous commitment the gate can reference when attesting a
  transition.

- [ ] **Verify address ownership via payment.** Today any caller can
  claim any name for any address — `src/payment.rs::verify` is a stub
  that returns `Ok("stub-<uuid>")` without checking anything, so the
  `address` field on `/register` and `/claim` is effectively
  self-asserted. Fix: prove ownership *by payment* — if the claimed
  `address` paid the fee for this `name`, it controls that address.
  Wire:
    1. Poke a local `nockchain` instance (via `nockchain-client-rs`,
       already a path-dep in `Cargo.toml`) to look up the payment
       transaction by `tx_hash`.
    2. Check, under one note/transaction: `sender == address`,
       `recipient == <nns treasury address>`, and
       `amount >= fee_for(name)`. Payment-replay ("paid once, claim
       many names") is already blocked by the kernel's
       `tx-hashes` set — C4 in `%claim` — so no memo-binding is
       needed here.
    3. Replace the `payment::verify` stub with this check; surface
       chain-client failures as `503` (transient) and verification
       failures as `400 "payment did not cover fee"` /
       `400 "payment sender does not match address"`.
    4. Follow-up: lift payment attestation into the STARK proof by
       widening `nns-gate`'s data shape to include `tx_hash` +
       signature bytes and adding predicates alongside G1/G2/G3 (see
       "Real payment verification" under Graduation path).

- [ ] **Authenticate the caller of `/primary`.** The kernel's P2
  check (`names[name].owner == address`) is *authorization*, not
  *authentication*: it only guarantees the address declared in the
  poke does own the name. Neither the hull nor the kernel verifies
  that the HTTP client is actually that address, so today anyone
  can `POST /primary {address: "ALICE", name: "alpha.nock"}` and
  flip Alice's reverse-lookup. Same gap as `/register` + `/claim`
  and closes with the same primitive — pick one:
    1. **Couple to a fresh payment** (cheapest, lands with the item
       above). Require a small fee transaction from `address` whose
       existence the hull verifies via the Nockchain client before
       poking `%set-primary`. The payment proves control of the
       sending address. Bind the payment to the intent by reusing
       the kernel's `tx-hashes` set so a single payment can't
       retarget multiple names. Surface chain failures as `503`,
       missing/mismatched payment as `400`.
    2. **Signed-message auth** (canonical ENS-style). Extend
       `SetPrimaryRequest` with `signature` + `nonce`; the hull
       verifies a Nockchain signature over
       `canonical({address, name, nonce})` before poking. Requires
       a Nockchain signature-verification primitive in the hull.
       Track a per-address `nonce` in the mirror to prevent replay.
    3. **STARK-bound auth** (follow-up, maximally trust-minimized).
       Widen the `%set-primary` cause + `nns-gate` data to carry a
       signed attestation and verify it inside the gate, so the
       settlement proof attests "a message signed by the owner
       authorized this primary." Depends on a Hoon-side signature
       verifier and is downstream of real `nns-gate` settlement.

## Project layout

```
hoon/
  app/app.hoon              names registry + %claim/%set-primary/%vesl-* pokes
                            + Merkle helpers (sorted-leaves, next-level,
                            compute-root, proof-for, hull-for) + nns-gate
  lib/vesl-graft.hoon       graft state + dispatcher (copied from vesl)
  lib/vesl-merkle.hoon      merkle primitives (hash-leaf, hash-pair, verify-chunk)
  common/wrapper.hoon       state versioning
  common/zeke.hoon          tip5 hash chain
  common/ztd/               tip5 math tables
  tests/names.hoon          compile-time domain invariant tests
                            (G1 format + G2 Merkle inclusion across tree sizes)
src/
  main.rs                   entrypoint: boot kernel, load config, serve HTTP
  lib.rs                    module wiring
  api.rs                    axum router + handlers + poke/verify timeout guard
  kernel.rs                 NounSlab builders for each poke and peek
  state.rs                  AppState + mirror persistence
  payment.rs                stubbed payment verify + fee tiers
  types.rs                  wire-compat types for the legacy JSON shapes
scripts/
  parity.py                 legacy vs new API diff tool
tests/
  handlers.rs               full HTTP integration tests
vesl.toml                   settlement config
Cargo.toml                  path-deps on ../nockchain and ../vesl
out.jam                     compiled kernel (built by hoonc)
```

## Moving the repo

The sibling path layout (`../nockchain`, `../vesl`) is set in
`Cargo.toml`. If you clone elsewhere, update those four path entries
and you're done. No env vars or build scripts reference the repo
location.
