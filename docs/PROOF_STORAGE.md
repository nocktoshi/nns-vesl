# Proof storage: how NNS verifies on-chain facts without storing the chain

**Status**: architecture reference. Pairs with [CONSENSUS.md](CONSENSUS.md)
(why Nockchain is the sequencer) and
[research/recursive-payment-proof.md](research/recursive-payment-proof.md)
(how the recursive STARK is built).

The short answer: **NNS never verifies the chain itself**. It verifies
that claims *link into* a chain the wallet already trusts. Chain
verification is delegated to the wallet's own Nockchain view; NNS
just provides a cryptographic proof that its registry attaches at a
specific point in that chain.

This doc walks through that claim concretely with a worked example.

## Scenario

Alice registered `alice.nock` on NNS. Bob's wallet wants to verify:
"did `alice.nock` really get registered by `alice-addr`, and did that
registration actually pay?"

## The data involved

Four pieces, stored in four different places:

| Data | Size | Stored by | Trust |
|---|---|---|---|
| Alice's registry row `(alice.nock, alice-addr, tx-hash=X)` | ~120 B | NNS kernel (in `names` map) | kernel-owned |
| Nockchain's block B containing tx X | ~100 KB | Nockchain nodes | chain-owned |
| NNS's current anchored tip T | **40 B** | NNS kernel (`tip-digest` field) | kernel-owned |
| Nockchain's canonical tip T' | varies | wallet's Nockchain view | wallet-owned |

**Key observation**: NNS stores *120 bytes about Alice's claim* and
*40 bytes about the Nockchain tip it anchors to*. It never stores
block B, X's transaction data, or any of Nockchain's tables. All of
those live at Nockchain; all of those travel *with the proof* when
NNS ships it.

## What happens when Bob's wallet verifies

Bob's wallet asks *any* NNS server for `alice.nock`. The server
replies with a bundle:

```
{
  claim:           { name: "alice.nock", owner: "alice-addr", tx_hash: X },
  registry_proof:  <Merkle proof of claim in NNS's registry root R>,
  page:            <Nockchain block B — digest, height, parent, tx-ids>,
  block_proof:     <B's PoW STARK, ~75 KiB>,
  header_chain:    <N parent headers from B up to tip T>,
  raw_tx:          <the actual transaction noun at X>,
  stark:           <one recursive STARK proof covering all of the above>,
}
```

The wallet runs **two verifications**:

### 1. Verify the STARK (cryptography, no trust)

The STARK is a self-verifying object. Running `verify:vesl-verifier`
attests to a compound statement:

```
stark ⇒ ∃ (claim, R, page, block_proof, header_chain, raw_tx):
  claim ∈ registry_with_root(R)              -- Merkle inclusion (G2)
  ∧ verify_pow(block_proof) = ok              -- block B is valid PoW
  ∧ block_commitment(page) = block_proof.commitment  -- proof is about this page
  ∧ compute_id(raw_tx) = claim.tx_hash        -- raw_tx is the one claim refers to
  ∧ claim.tx_hash ∈ page.tx_ids               -- tx landed in page
  ∧ chain_links(page, header_chain, T_nns)    -- page chains to NNS's anchor T_nns
  ∧ pays_sender(raw_tx) = claim.owner         -- Alice paid, not someone else (C5a)
  ∧ pays_amount(raw_tx, treasury) ≥ fee       -- she paid enough (C5c)
```

Every step uses data shipped **inside the proof bundle** or provable
by the STARK itself. None of them read NNS kernel state. None of
them read Nockchain's chain.

### 2. Verify the anchor point (one chain read, wallet-owned)

The STARK proved "claim anchors to `T_nns`". Bob's wallet still
needs to know: *is `T_nns` in the Nockchain chain I trust?*

The wallet has **its own** Nockchain view — from a full node it
runs, a gRPC endpoint it trusts, or a light-client proof. It asks:

> "Is the block with digest `T_nns` at height `H_nns` in your
> canonical chain?"

If yes → accept the name. If no → reject (either NNS server is
lying or Nockchain reorg'd past NNS's anchor).

## Where each trust boundary lands

```
┌─────────────────┐
│  Bob's wallet   │ ──── trusts cryptography (STARK is self-verifying)
└────────┬────────┘
         │
         │   1. verify STARK (one ~100 KiB artifact)
         ▼
┌─────────────────┐
│  NNS STARK proof│ ──── attests "claim anchors to tip T_nns"
│  (shipped by    │
│   any server)   │
└────────┬────────┘
         │
         │   2. wallet independently asks its own Nockchain source:
         │      "is T_nns in your canonical chain?"
         ▼
┌─────────────────┐
│   Nockchain     │ ──── wallet trusts this anyway (for UTXOs)
│  (wallet's own  │
│   chain view)   │
└─────────────────┘
```

**NNS is nowhere in the trust path for chain data.** The wallet goes
directly to its own Nockchain view. NNS just provides a
cryptographic commitment that bridges "claim" to "Nockchain tip".

## Contrast with "store everything"

If NNS *did* store chain state (the 1024-header deque we removed in
the slim-anchor refactor, or more extreme variants), verification
would look like:

```
wallet → NNS: "show me the claim"
wallet → NNS: "show me your chain history so I can trust your anchor"   ← NEW
wallet: verify chain history against its own view                       ← NEW work
wallet: verify claim linkage into that chain                            ← still needed
```

Two problems:

1. The wallet now has to trust NNS's chain history — but NNS's
   chain history is redundant because the wallet has its own
   Nockchain view.
2. The wallet's workload doubled: verify NNS's chain cache *and*
   verify the claim.

The current design short-circuits both: NNS provides **one**
cryptographic bridge, the wallet uses **one** trusted source
(Nockchain), no replication.

## The three places chain state actually gets used

Let me be explicit about who reads what, and why "storing chain
state in NNS" is never needed:

**A. To prove chain linkage inside the STARK** → uses `header_chain`
from the **proof bundle**. Each claim ships its own chain walk. The
gate walks it inside the trace. No kernel state touched.

**B. To verify Nockchain block PoW inside the STARK** → uses
`block_proof` from the **proof bundle**. `verify:sp-verifier` runs
on it. No kernel state touched.

**C. To anchor the whole proof to a specific Nockchain tip** →
kernel stores `tip-digest + tip-height` (48 bytes). The gate asserts
"header_chain terminates at this tip". The wallet then validates
this tip against its own Nockchain view.

A and B are per-claim data (ships with the proof). C is 48 bytes.

## What the follower *actually* does

The hull's background follower ([src/chain_follower.rs](../src/chain_follower.rs)):

1. Every 10 s, asks Nockchain's gRPC: "what's the current tip height?"
2. Fetches the ~64 headers between NNS's current anchor and the
   chain tip minus 10 blocks of finality.
3. Pokes `%advance-tip` with those headers.
4. Kernel **validates** the chain (every parent link), then **throws
   away** the intermediate headers. Only `tip-digest + tip-height`
   survives.

So even the *advance* path doesn't accumulate state. Validation is
transient; only the tip persists.

## Why this is the right design

Every node-like system on every blockchain ultimately has to answer:
"how do I prove claims about chain-anchored facts without being a
full chain indexer?"

- **Bitcoin SPV wallets**: store ~80-byte block headers only; rely
  on PoW for trust; never store transactions.
- **Ethereum zkRollups** (Optimism, Arbitrum, ZKsync): store one
  state root on L1; never store L1's block data.
- **Cosmos IBC**: light-client proofs reference a counterparty
  chain's tip; never replicate chain state.
- **Nockchain light clients** (the intended Phase 5 wallet
  verifier): verify NNS STARK + check anchor against their own
  Nockchain view.

We're in good company. The pattern is **"pointer + proof, not copy"**.

## One nuance worth surfacing

**Liveness assumption**: the wallet's own Nockchain view must be
recent enough to still have `T_nns` in its canonical chain. If
NNS's anchor is 10 blocks deep and the wallet is 1000 blocks
behind, the wallet has to sync forward before it can verify the
claim. This is identical to the SPV "wallet must be synced to see
recent txs" assumption — nothing NNS-specific.

If NNS's anchor were *ahead* of the wallet (possible during
reorgs), the wallet waits for its view to catch up. If NNS's anchor
is forever unreachable (reorg orphaned it), the follower detects
this on the next `%advance-tip` attempt because new headers won't
chain to our stale tip; the operator notices and either waits for
re-convergence or corrects state.

## Concrete byte budget

For verification of `alice.nock`:

| Artifact | Size | Source |
|---|---|---|
| STARK proof | ~100 KiB | NNS server |
| Block B page header | ~200 B | NNS server (bundled) |
| Block B PoW proof | ~75 KiB | NNS server (bundled) |
| Header chain (avg 5 headers) | ~440 B | NNS server (bundled) |
| Raw tx noun | ~300 B | NNS server (bundled) |
| Merkle inclusion proof | ~200 B | NNS server (bundled) |
| **One chain-view query to wallet's Nockchain** | ~1 RTT | wallet |
| **Bytes NNS stores to support this**: | **~160 B** (claim row + 48 B tip) | kernel |

NNS kernel storage is **~160 bytes per claim + 48 bytes of chain
anchor total**. Nockchain stores everything else it already stores.
The wallet holds its own Nockchain view. No duplication.

## Relationship to other docs

- [CONSENSUS.md](CONSENSUS.md) explains *why* NNS uses Nockchain as
  its sequencer (double-spend resistance for name registration).
  That doc describes the trust architecture at a higher level; this
  doc zooms in on the verification flow and byte accounting.
- [ROADMAP.md](ROADMAP.md) tracks the phased implementation.
  Phase 2 landed the anchor cursor and follower; the slim-anchor
  refactor recorded on 2026-04-24 is what made the storage story
  honest (48 B instead of ~90 KB).
- [research/recursive-payment-proof.md](research/recursive-payment-proof.md)
  covers the recursive-STARK spike that validated the "gate
  verifies block-PoW inside its own trace" step.

## What the current proof attests to (Phase 3c option-B recursion)

Phase 3c landed the proof scaffolding in two steps: a `validate-claim`
cause that runs Level A + Level B + G1/C2 predicates on a bundle, and
a `prove-claim` cause that validates and then emits a STARK
committing to `(bundle-digest, root, hull)`. Full tests land in
`tests/prover.rs::phase3c_prove_claim_roundtrip` (#[ignore], ~5 s
prove, ~615 ms verify).

This is **option-B recursion** — the validator runs *outside* the
STARK trace, and the STARK serves as a tamper-evident commitment to
"this kernel, at this (root, hull), asserted this specific
bundle-hash". A full Phase 3c step 3 (validator execution *inside*
the trace) waits on Level C's tx-witness vendor + Nock-formula
encoding of the predicate composition.

**Wallet verification flow today**:

1. Receive `(bundle, proof, bundle_digest)` from any NNS server.
2. Verify the STARK via `verify:vesl-verifier` → confirms some
   kernel at some `(root, hull)` committed `bundle_digest`.
3. Re-fold `(jam bundle)` to belt-digest locally → must equal
   `bundle_digest`. (Blocks a server from shipping a proof over a
   different bundle than the one it sent you.)
4. Re-run `validate_claim_bundle(bundle)` locally → must return
   `Ok`. (Blocks a server from lying about the bundle passing the
   gate.)
5. Cross-reference `(root, hull, t_nns)` against the wallet's own
   view (see [Staleness and fork resistance](#staleness-and-fork-resistance)).

Steps 3 + 4 are cheap (no Tip5 sponge, no FRI). Step 2 is the
~100 KB STARK verify. Steps 1 + 5 are network RTTs. The wallet's
total work is still bounded by one STARK verify.

**What step 3 upgrades**: moves the `validate_claim_bundle`
execution into the STARK trace so the wallet no longer runs it.
Single-artifact trust; smaller wallet SDK. Requires:

- The Hoon predicates to be compilable to a Nock formula the prover
  can embed in `prove-computation`'s `[subject formula]`. Either
  hand-written or generated from the `++validate-claim-bundle` arm.
- Level C predicates (`pays-sender`, `pays-amount`,
  `matches-block-commitment`) to close the payment + page-commitment
  trust gaps.

## Staleness and fork resistance

The "wallet trusts Nockchain independently" story handles honest-but-
out-of-order servers. It does *not* handle the following race on its
own:

1. Alice asks NNS server N1 to register `zero.nock`.
2. N1's follower is stale — anchored at height 1000 while Nockchain is
   at 1020.
3. Between 1000 and 1020 Bob's payment for `zero.nock` already landed
   in a block.
4. N1's kernel doesn't know about Bob's tx yet; its `names` map still
   says `zero.nock` is free.

We have to guarantee that Alice never walks away holding a proof that
`zero.nock` is hers in this situation. The design does so in three
layers of increasing adversarial strength:

### Layer 1 — chain-ordered replay handles honest-but-slow servers

The `POST /claim` HTTP request is **not** a commitment. The handler:

1. Accepts a payment tx the user already sent to Nockchain,
2. Queues a pending claim in the hull mirror,
3. Returns a `claim-id` with status `Submitted`.

The kernel never touches `names` / `root` / `hull` at this point. No
proof can be generated. The actual commitment happens later, when the
follower replays txs **in canonical chain order**
([src/chain_follower.rs](../src/chain_follower.rs) sorts pending claims
by `(block_height, tx_index_in_block)` before issuing `%claim` pokes).

So even if Alice's HTTP request reached N1 before Bob's reached N2,
when N1's follower catches up to height 1020 it processes them in
on-chain order: Bob first, Alice second. Alice's replay fails with
C3 (`"name already registered"`), her `claim-id` flips to `Rejected`,
and no proof is ever issued. The follower cannot reorder on-chain
events — that's the sequencer guarantee from Path A in
[CONSENSUS.md](CONSENSUS.md).

### Layer 2 — frozen followers converge on replay

If N1's follower is *stuck* at a stale tip (operator hasn't restarted,
network partition, bug), N1's kernel stays frozen too. No new claims
get processed. Alice polls `/claim-status` and sees `Submitted`
indefinitely, but she never gets a proof either. This is broken
service, not safety loss.

When the follower restarts, it replays every on-chain tx in order
from its last processed height. Bob's earlier tx gets replayed first;
the kernel's `%claim` arm accepts Bob. Alice's (later) tx fails C3
on replay. State converges to what a never-stale follower would
have produced. No human intervention needed beyond fixing whatever
broke the follower.

### Layer 3 — wallet freshness check handles malicious servers

A malicious operator can bypass the follower entirely: patch the
code to skip the chain-order replay, manually poke `%claim` to
register Alice even though `zero.nock` is Bob's on-chain. The
kernel's C3 check passes because the kernel's `names` map doesn't
know about Bob. A proof can then be emitted.

The proof verifies cryptographically (the STARK is honest about
the kernel's internal state; the kernel's internal state just
happens to be wrong). The wallet's naive "STARK verifies + anchor
is in my Nockchain" check both pass.

**This is where wallet-side freshness closes the gap**:

> A wallet MUST reject any proof where
> `proof.T_nns_height < wallet.current_chain_tip_height - MAX_STALENESS`

With `MAX_STALENESS = finality_depth + margin` (e.g. 20 blocks), a
malicious N1 whose anchor is frozen at the moment of attack can only
trick wallets whose Nockchain view is *also* within `MAX_STALENESS`
of that stale anchor. For any reasonably fresh wallet the forged
proof fails the freshness check and gets rejected.

Phase 3's recursive STARK already commits `T_nns_height` into the
proof as part of the anchor binding, and the `/anchor` peek exposes
the current value. All that's missing is the wallet-side enforcement
at verification time. Tracked in [ROADMAP.md](ROADMAP.md) as a
blocking acceptance criterion before production claims are trusted
for real value.

### Why MAX_STALENESS can be small

A conservative wallet might worry that picking `MAX_STALENESS` too
small rejects proofs from honest-but-lightly-lagging servers. A few
observations make this cheap to tune:

- The NNS follower polls every 10 s and advances up to 64 headers
  per poke, so an honest server's anchor lag is typically a few
  seconds to a minute behind Nockchain tip.
- Nockchain's finality depth is ~10 blocks; anything within that
  window is considered provisional on-chain anyway.
- A `MAX_STALENESS` of 20 blocks (2× finality) rejects only servers
  that are meaningfully degraded *and* catches every malicious
  operator whose follower is older than 20 blocks.

So 20 is a reasonable default. The wallet SDK should surface this
as a user-configurable parameter with a well-documented minimum.

### Scenario walk-through

```
Nockchain height 1000: chain tip T0
NNS server N1 anchored at T0
NNS server N2 anchored at T0

t=0:   Alice sends tx X_A to Nockchain (pays NNS treasury for zero.nock)
t=1:   Bob   sends tx X_B to Nockchain (pays NNS treasury for zero.nock)
t=2:   Nockchain mines block 1005 containing X_B
t=3:   Alice submits POST /claim to N1 — queues (claim_id_A, Submitted)
t=4:   Bob   submits POST /claim to N2 — queues (claim_id_B, Submitted)
t=5:   Nockchain mines block 1010 containing X_A
t=6:   Nockchain tip = 1010

Case A — N1 is honest and synced (Layer 1):
t=7:   N1 follower advances anchor to height 1010
t=8:   N1 follower replays in chain order: X_B (1005) then X_A (1010)
t=9:   Kernel accepts Bob, rejects Alice with C3
t=10:  Alice polls /claim-status?claim_id=A → Rejected, "name already registered"
RESULT: Alice knows. No proof ever issued. Safe.

Case B — N1 frozen follower (Layer 2):
t=7:   N1 follower stuck; anchor stays at 1000
t=8:   Alice polls → Submitted (indefinitely). N1 refuses /proof requests.
RESULT: Alice in limbo, not hurt. Broken service; no safety loss.

Case C — N1 malicious operator bypasses follower (Layer 3):
t=7:   Operator directly pokes %claim for Alice, skipping chain-order replay
t=8:   Kernel C3 passes (kernel unaware of Bob), registers Alice at root R
t=9:   Alice's wallet asks N1 for proof of zero.nock
t=10:  N1 emits proof (claim=Alice, root=R, T_nns_height=1000)
t=11:  Wallet verifies STARK ✓
t=12:  Wallet checks T_nns_height=1000 vs current_tip=1010
       → 1000 < 1010 - 20   NO (margin holds)
       → If current chain has advanced further (t+10min, say to 1025):
         1000 < 1025 - 20 = 1005 → REJECT as stale
RESULT: Wallet rejects once Nockchain advances past MAX_STALENESS
        window past the stale anchor. Safe under the freshness rule.
```

### Operator observability

For operators of NNS servers, three monitors catch each layer's
failure modes before users notice:

1. **Anchor lag**: `current_chain_tip - anchor.tip_height`. Alert at
   > 50 blocks — follower is likely broken.
2. **Pending claim backlog**: count of claims in `Submitted` for
   > 10 minutes. Alert if non-zero — follower isn't replaying.
3. **Claim rejection rate**: spike in `Rejected` claims with reason
   `name already registered`. Indicates either normal contention or
   a multi-node fork worth investigating.

None of these are implemented today; [ROADMAP.md](ROADMAP.md) tracks
them as pre-Phase-5 items.

## Design rule to preserve

Any time someone asks "should we store X from Nockchain in the
kernel to make peek Y faster?", the answer is almost certainly
**no** — that's a hull-side concern (the follower already fetches
everything, and a hull cache has zero consensus implications). The
kernel stores only things that:

1. Get committed into the registry's Merkle tree (names, owners,
   tx-hashes), **or**
2. Form the one-pointer bridge to the outside world
   (`tip-digest`, `payment-address`).

Everything else flows through the proof bundle. That's what makes
the kernel small, the prover fast, and the wallet's trust boundary
clean.
