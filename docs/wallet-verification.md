# Path Y wallet verification

**Submitting a claim (on-chain):** wallets must attach **structured NoteData**
on a tx output (`nns/v1/claim-*` keys + JAM tuple). That is not yet a
first-class flow in generic wallets — see
[`docs/claim-note-wallet-support.md`](claim-note-wallet-support.md) and
[nockchain#85](https://github.com/nockchain/nockchain/pull/85).

Path Y4 verifies a **lookup bundle** offline: **no live Nockchain RPC**, no
`--chain-tip` / staleness flags (those were Phase 7). Trust is pinned to a
**checkpoint** `(height, digest)` you ship out-of-band plus Vesl soundness.

## What the wallet trusts

1. **Nockchain consensus at a checkpoint** — CLI `--checkpoint-height` and
   `--checkpoint-digest-hex` (or equivalent in your wrapper).
2. **Vesl STARK soundness** — non-empty `recursive_proof_hex` is checked via
   the NNS kernel poke **`%verify-stark-explicit`** (same jets as the hull),
   with `recursive_subject_jam_hex` and `recursive_formula_jam_hex` (raw JAM
   hex of the traced nouns). Kernel JAM: `--kernel-jam` / `$NNS_KERNEL_JAM` /
   `./out.jam`.
3. **Accumulator snapshot** — when `value` is present, `accumulator_snapshot_jam_hex`
   must carry **`jam(nns-accumulator)`** for the claimed state. The hull exposes
   it as hex on **`GET /accumulator/:name?wallet_export=true`** (`accumulator_snapshot_hex`).
   `light_verify` pokes **`%verify-accumulator-snapshot`**: `root-atom` from
   `accumulator_root_hex` must match the snapshot, and `(get acc name)` must
   match `value`. Legacy **`z_in_proof`** JSON is rejected (exit **2**).

## Check order in `light_verify`

1. Reject non-empty deprecated **`z_in_proof`** (exit **2**).
2. **Recursive STARK** (exit **7** on failure; exit **8** if empty in strict mode).
3. **Accumulator binding** when **`value`** is set (exit **9** without snapshot in strict mode;
   exit **10** on mismatch; exit **7** on kernel/JAM errors).
4. **`headers_to_checkpoint`** — walk parent links from `last_proved_digest_hex` down to the checkpoint (exit **1**).

## JSON bundle (`PathY4LookupBundle`)

| Field | Role |
| --- | --- |
| `name` | `.nock` key |
| `value` | Optional row: `owner`, `tx_hash_hex`, `claim_height`, `block_digest_hex` |
| `last_proved_height` / `last_proved_digest_hex` | Scan cursor (Nockchain block) |
| `accumulator_root_hex` | `root-atom` from `/scan-state` (must match snapshot) |
| `recursive_proof_hex` | Vesl proof JAM (hex); may be empty pre-Y3 |
| `recursive_subject_jam_hex` / `recursive_formula_jam_hex` | Required if proof non-empty |
| `accumulator_snapshot_jam_hex` | `jam(accumulator)` from `?wallet_export=true` |
| `z_in_proof` | Must be absent or empty (deprecated) |
| `headers_to_checkpoint` | Block header segments linking `last_proved_digest` down to the checkpoint |

## CLI flags

| Flag | Role |
| --- | --- |
| `--checkpoint-height` / `--checkpoint-digest-hex` | Required pinned anchor |
| `--kernel-jam` | Kernel JAM for `%verify-stark-explicit` and `%verify-accumulator-snapshot` |
| `--allow-empty-recursive-proof` | Allow empty recursive proof (Y2) |
| `--allow-missing-z-in-proof` | Allow `value` without `accumulator_snapshot_jam_hex` (Y2) |
| `--path-y2-dev` | Sets both relax flags |

## Exit codes

| Code | Meaning |
| --- | --- |
| `0` | OK |
| `1` | Header chain failed |
| `2` | Bad JSON/hex or deprecated `z_in_proof` |
| `5` | Missing checkpoint CLI flags |
| `7` | STARK or accumulator verify kernel error |
| `8` | Strict: empty recursive proof |
| `9` | Strict: `value` without snapshot |
| `10` | Accumulator root / entry mismatch |

## Example (Y2 dev, no `value`)

```bash
echo '{
  "name": "alice.nock",
  "value": null,
  "last_proved_height": 100,
  "last_proved_digest_hex": "'"$(python3 -c 'print("07"*40)')"'",
  "accumulator_root_hex": "'"$(python3 -c 'print("07"*40)')"'",
  "recursive_proof_hex": "",
  "headers_to_checkpoint": []
}' | light_verify \
  --checkpoint-height 100 \
  --checkpoint-digest-hex "$(python3 -c 'print("07"*40)')" \
  --allow-empty-recursive-proof \
  --allow-missing-z-in-proof
```

Rebuild **`out.jam`** after pulling Hoon changes for **`%accumulator-jam`** peek
and **`%verify-accumulator-snapshot`** cause.
