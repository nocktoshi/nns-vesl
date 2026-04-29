# NNS claim notes and wallet support

Path Y registers names **only** when a valid claim appears on a **transaction output’s NoteData**: entry key **`blob`** with wallet-packed payload ([nockchain#116](https://github.com/nockchain/nockchain/pull/116)); see `ClaimNoteV1::from_note_data` (`src/claim_note.rs`). There is **no** `POST /claim` HTTP path on this branch.

## Major limitation: structured NoteData on outputs

Generic wallets today often expose **amount + recipient** flows only. NNS requires **NoteData** with the **`blob`** key encoding described below (matches `wallet-tx-builder`). That is still a **non-trivial** wallet product change for many vendors:

- The claim must sit on an **output** of the registering transaction; the follower walks `details.outputs` only.
- **`owner`** in the claim tuple should match how the hull builds the payment witness (see `claim_witness_from_transaction` in `src/chain_follower.rs`): typically the spending input’s `note_name_b58` equals `owner` for correct Level C-A checks.

Until wallets ship first-class NoteData UX, operators rely on **nockchain-wallet** / **custom tx builders** aligned with upstream packing (`encode_blob_belts` → jam).

## Wire format (summary)

| Key | Value |
|-----|--------|
| `blob` | **Wallet-packed** ([nockchain#116](https://github.com/nockchain/nockchain/pull/116)): JAM of belt list `[byte-len belt …]`; **inner** bytes are JAM of `[name=cord owner=cord tx_hash=cord]`. Decode: `src/packed_blob.rs` (same as `wallet-tx-builder` / `MemoDataPayload::from_blob`). **`memo`** uses the same packing with a different key (human-readable UTF-8 bytes). |

**Trust boundary:** the note **only** commits the claim triple in **`blob`**. The hull **does not** accept or decode extra note-data keys for raw-tx, block pages, PoW proofs, or header chains — those would be user-supplied and forgeable. Path Y **`chain_follower`** loads **`TransactionDetails`** and block metadata from **Nockchain RPC** for each tx and validates predicates + witness against that canonical view (`src/chain_follower.rs`, `hoon/lib/nns-predicates.hoon`).

## Nockchain PRs: `--memo-data`, public `note_data`, `blob` / `memo`

**[nockchain/nockchain#116 — feat(wallet): blobs and memo on transactions](https://github.com/nockchain/nockchain/pull/116)** — canonical **`memo`** (UTF-8) and **`blob`** (programmatic / packed) keys in NoteData; public gRPC exposes `note_data` on outputs so followers can read claims.

**[nockchain/nockchain#85 — feat: add `--memo-data` to `create-tx` command](https://github.com/nockchain/nockchain/pull/85)** — earlier CLI hook for opaque payloads; convergence with structured NoteData / `RecipientSpec` / `$order` is discussed there (`seeds-from-specs`, etc.). Until wallet UX catches up, expect **manual** encoding or **fork-local** CLI changes that satisfy both:

- **gRPC (or indexer) paths that return `note_data` on outputs** — required for NNS to see claims.  
- **Transaction construction** that attaches **`blob`** per upstream packing — mirror [`wallet-tx-builder`](https://github.com/nockchain/nockchain/blob/master/crates/wallet-tx-builder/src/note_data.rs).

`Cargo.toml` uses **`../nockchain`**; keep that checkout compatible with the contract in [`docs/nockchain-fork-for-nns.md`](nockchain-fork-for-nns.md). Nockchain fork work is **out of tree** for nns-vesl.

## See also

- [`docs/wallet-verification.md`](wallet-verification.md) — Path Y4 offline verification (after a claim is on-chain and indexed).
- [`docs/running_a_node.md`](running_a_node.md) — operator setup and chain-mode notes.
- [`docs/nockchain-fork-for-nns.md`](nockchain-fork-for-nns.md) — **contract** your `../nockchain` checkout must satisfy (no fork branching instructions here).
