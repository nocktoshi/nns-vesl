# Nockchain sibling checkout (`../nockchain`)

`nns-vesl` uses **path dependencies** on a local **`../nockchain`** tree (`Cargo.toml`: `nockapp`, `nockapp-grpc`, `nockchain-types`, `zkvm-jetpack`, …). You maintain that checkout in your own repo; **this document only states the contract NNS needs** — not how to branch or merge your fork.

## Contract: what Path Y needs

1. **RPC / block explorer surfaces must return `note_data` on v1 outputs**  
   The follower (`src/chain_follower.rs`) only learns about claims from `output.note_data`. If `GetBlockDetails` (or whatever your build wires) omits it, `%scan-block` never sees `nns/v1/claim` payloads.

2. **A way to put that NoteData on-chain**  
   Wallets must be able to attach the keyed blobs NNS defines (`docs/claim-note-wallet-support.md`). Upstream direction: **[nockchain#85](https://github.com/nockchain/nockchain/pull/85)** (`create-tx --memo-data` — API still evolving in review).

## Hoon / `hoonc`

Point **`NOCK_HOME`** at the same tree `scripts/setup-hoon-tree.sh` symlinks from (`../nockchain` by default) so kernel builds see the same Nockchain Hoon as the Rust crates.

## Relation to this repo

NNS does **not** vendor nockchain sources beyond those path deps. Fork-specific notes, merge plans, and wallet patches live **only in your nockchain fork**; keep `../nockchain` on whatever branch satisfies the contract above for your environment.
