# NNS claim notes and wallet support

Path Y registers names **only** when a valid **`nns/v1/claim`** payload appears on a **transaction output’s NoteData**. The NNS hull’s follower decodes every output’s `note_data` and runs `ClaimNoteV1::from_note_data` (`src/claim_note.rs`). There is **no** `POST /claim` HTTP path on this branch.

## Major limitation: structured NoteData on outputs

Generic wallets today often expose **amount + recipient** flows only. NNS requires **arbitrary NoteData**: multiple string keys, each mapped to an opaque blob (including a **JAM**’d triple for `nns/v1/claim`). That is a **non-trivial product and protocol change** for wallet vendors:

- The client must build **NoteData entries** exactly as `ClaimNoteV1::to_note_data` does (see `src/claim_note.rs`).
- The claim must sit on an **output** of the registering transaction; the follower walks `details.outputs` only.
- **`owner`** in the claim tuple should align with how the hull builds the payment witness (see `claim_witness_from_transaction` in `src/chain_follower.rs`): typically the spending note name matches `owner` for correct Level C-A checks.

Until wallets ship first-class “attach NoteData / app-specific payload” UX, operators rely on **custom tx builders** or patched **nockchain-wallet** tooling.

## Wire format (summary)

| Key | Value |
|-----|--------|
| `nns/v1/claim-version` | `u64`, must be `1` |
| `nns/v1/claim-id` | opaque bytes (UTF-8 string in practice) |
| `nns/v1/claim` | JAM of `[name=cord owner=cord tx_hash=cord]` |

Optional Phase 2d keys (`nns/v1/raw-tx`, `nns/v1/page`, …) are documented in `src/claim_note.rs`; the Path Y follower does not require them for the current scanner path.

## Nockchain PR: `create-tx --memo-data`

Upstream work to make the **official wallet CLI** attach opaque payload data when creating transactions is tracked here:

**[nockchain/nockchain#85 — feat: add `--memo-data` to `create-tx` command](https://github.com/nockchain/nockchain/pull/85)**

That PR is the natural place to converge with **structured** NoteData once reviewers align it with `RecipientSpec` / `$order` (see PR discussion: memo on the recipient order, `seeds-from-specs`, etc.). Until something equivalent lands, expect **manual** encoding or **fork-local** wallet / CLI changes that satisfy both:

- **gRPC (or indexer) paths that return `note_data` on outputs** — required for NNS to see claims.  
- **Transaction construction that attaches the NNS key layout** — see `ClaimNoteV1::to_note_data` in this repo.

`Cargo.toml` uses **`../nockchain`**; keep that checkout compatible with the contract in [`docs/nockchain-fork-for-nns.md`](nockchain-fork-for-nns.md). Nockchain fork work is **out of tree** for nns-vesl.

## See also

- [`docs/wallet-verification.md`](wallet-verification.md) — Path Y4 offline verification (after a claim is on-chain and indexed).
- [`docs/running_a_node.md`](running_a_node.md) — operator setup and chain-mode notes.
- [`docs/nockchain-fork-for-nns.md`](nockchain-fork-for-nns.md) — **contract** your `../nockchain` checkout must satisfy (no fork branching instructions here).
