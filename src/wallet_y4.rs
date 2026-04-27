//! Path Y4 offline wallet verification helpers.
//!
//! Verifies a lookup bundle against a **pinned Nockchain checkpoint** (no live
//! RPC). Non-empty `recursive_proof_hex` is verified by the hull kernel via
//! [`crate::kernel::verify_stark_explicit_blocking`] (`%verify-stark-explicit`)
//! when the bundle includes matching `recursive_*_jam_hex` fields.
//! **Z-map membership** for a `value` row uses `accumulator_snapshot_jam_hex`
//! (JAM of the full accumulator from `/accumulator-jam`) plus
//! [`crate::kernel::verify_accumulator_snapshot_blocking`].
//! Legacy `z_in_proof` JSON is rejected — use the snapshot field instead.

use serde::Deserialize;

/// Wallet-trusted Nockchain anchor: digest at `height` on the canonical chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointConfig {
    pub height: u64,
    pub digest: Vec<u8>,
}

/// One block header segment used to walk parent links toward a checkpoint.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct NockHeaderLink {
    pub height: u64,
    #[serde(rename = "digest_hex")]
    pub digest_hex: String,
    #[serde(rename = "parent_hex")]
    pub parent_hex: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct AccumulatorEntryJson {
    pub owner: String,
    #[serde(rename = "tx_hash_hex")]
    pub tx_hash_hex: String,
    pub claim_height: u64,
    #[serde(rename = "block_digest_hex")]
    pub block_digest_hex: String,
}

/// Deprecated JSON shape — do not use; `light_verify` rejects non-empty lists.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct ZInProofNodeJson {
    pub hash: String,
    pub side: String,
}

/// JSON envelope read by `light_verify` (Path Y4).
#[derive(Debug, Clone, Deserialize)]
pub struct PathY4LookupBundle {
    pub name: String,
    #[serde(default)]
    pub value: Option<AccumulatorEntryJson>,
    pub last_proved_height: u64,
    #[serde(rename = "last_proved_digest_hex")]
    pub last_proved_digest_hex: String,
    #[serde(rename = "accumulator_root_hex")]
    pub accumulator_root_hex: String,
    /// Jam of a Vesl `proof:sp`, hex-encoded. Empty / absent until Y3.
    #[serde(default, rename = "recursive_proof_hex")]
    pub recursive_proof_hex: Option<String>,
    /// JAM bytes of the traced `subject` noun (hex), paired with
    /// `recursive_formula_jam_hex` when `recursive_proof_hex` is non-empty.
    #[serde(default, rename = "recursive_subject_jam_hex")]
    pub recursive_subject_jam_hex: Option<String>,
    /// JAM bytes of the traced `formula` noun (hex).
    #[serde(default, rename = "recursive_formula_jam_hex")]
    pub recursive_formula_jam_hex: Option<String>,
    /// JAM of the full `nns-accumulator` (hex), from `GET /accumulator/:name?wallet_export=1`
    /// or `/accumulator-jam` peek. Required in strict mode when `value` is present
    /// (unless `--allow-missing-z-in-proof`).
    #[serde(default, rename = "accumulator_snapshot_jam_hex")]
    pub accumulator_snapshot_jam_hex: Option<String>,
    /// Deprecated — must be absent or empty.
    #[serde(default, rename = "z_in_proof")]
    pub z_in_proof: Option<Vec<ZInProofNodeJson>>,
    #[serde(default, rename = "headers_to_checkpoint")]
    pub headers_to_checkpoint: Vec<NockHeaderLink>,
}

pub fn hex_decode_even(s: &str) -> Result<Vec<u8>, String> {
    let s = s.trim();
    if s.len() % 2 != 0 {
        return Err("hex length must be even".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Verify `headers_to_checkpoint` links `last_proved_*` down to `checkpoint`.
///
/// * The first header (index 0) must match `(last_proved_height, last_digest)`.
/// * Consecutive rows: `headers[i].parent` equals `headers[i+1].digest` (parent
///   pointer of the higher block is the digest of the block below).
/// * The last row must match `(checkpoint.height, checkpoint.digest)`.
/// * Heights must decrease by exactly one per step.
/// * If `last_proved_height == checkpoint.height`, `headers` must be empty and
///   digests must match directly.
pub fn verify_header_chain_to_checkpoint(
    last_height: u64,
    last_digest: &[u8],
    checkpoint: &CheckpointConfig,
    headers: &[NockHeaderLink],
) -> Result<(), String> {
    if last_height == checkpoint.height {
        if !headers.is_empty() {
            return Err(format!(
                "expected empty headers when last_proved_height ({last_height}) equals checkpoint height, got {} headers",
                headers.len()
            ));
        }
        if last_digest != checkpoint.digest.as_slice() {
            return Err(
                "last_proved_digest does not match checkpoint digest at same height".into(),
            );
        }
        return Ok(());
    }

    if last_height < checkpoint.height {
        return Err(format!(
            "last_proved_height {last_height} is below checkpoint height {}",
            checkpoint.height
        ));
    }

    if headers.is_empty() {
        return Err(
            "headers_to_checkpoint is empty but last_proved_height is above checkpoint height"
                .into(),
        );
    }

    let first_d = hex_decode_even(&headers[0].digest_hex)?;
    if first_d != last_digest {
        return Err("first header digest does not match last_proved_digest".into());
    }
    if headers[0].height != last_height {
        return Err(format!(
            "first header height {} does not match last_proved_height {}",
            headers[0].height, last_height
        ));
    }

    for i in 0..headers.len() {
        let d_i = hex_decode_even(&headers[i].digest_hex)?;
        let p_i = hex_decode_even(&headers[i].parent_hex)?;
        if i + 1 < headers.len() {
            let d_next = hex_decode_even(&headers[i + 1].digest_hex)?;
            if p_i != d_next {
                return Err(format!(
                    "header chain break at index {i}: parent does not match next digest"
                ));
            }
            let h = headers[i].height;
            let h_next = headers[i + 1].height;
            if h_next + 1 != h {
                return Err(format!(
                    "header height gap at index {i}: expected height {}, got {}",
                    h - 1,
                    h_next
                ));
            }
        } else {
            // Last header is the pinned checkpoint block (may be genesis).
            if headers[i].height != checkpoint.height {
                return Err(format!(
                    "terminal header height {} does not match checkpoint height {}",
                    headers[i].height, checkpoint.height
                ));
            }
            if d_i != checkpoint.digest {
                return Err("terminal header digest does not match checkpoint digest".into());
            }
            // `parent_hex` points at height-1 (often not included in this list).
            let _ = p_i;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(seed: u8) -> Vec<u8> {
        vec![seed; 40]
    }

    fn hex40(v: &[u8]) -> String {
        v.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn same_height_no_headers_ok() {
        let cp = CheckpointConfig {
            height: 5,
            digest: d(9),
        };
        assert!(verify_header_chain_to_checkpoint(5, &d(9), &cp, &[]).is_ok());
    }

    #[test]
    fn same_height_digest_mismatch_fails() {
        let cp = CheckpointConfig {
            height: 5,
            digest: d(9),
        };
        assert!(verify_header_chain_to_checkpoint(5, &d(1), &cp, &[]).is_err());
    }

    #[test]
    fn three_block_chain_ok() {
        // h=3 digest A, parent B; h=2 digest B parent C; h=1 digest C matches checkpoint
        let d3 = d(3);
        let d2 = d(2);
        let d1 = d(1);
        let cp = CheckpointConfig {
            height: 1,
            digest: d1.clone(),
        };
        let headers = vec![
            NockHeaderLink {
                height: 3,
                digest_hex: hex40(&d3),
                parent_hex: hex40(&d2),
            },
            NockHeaderLink {
                height: 2,
                digest_hex: hex40(&d2),
                parent_hex: hex40(&d1),
            },
            NockHeaderLink {
                height: 1,
                digest_hex: hex40(&d1),
                parent_hex: hex40(&[0u8; 40]),
            },
        ];
        verify_header_chain_to_checkpoint(3, &d3, &cp, &headers).expect("chain ok");
    }

    #[test]
    fn broken_parent_fails() {
        let d3 = d(3);
        let d2 = d(2);
        let d1 = d(1);
        let bad = d(7);
        let cp = CheckpointConfig {
            height: 1,
            digest: d1.clone(),
        };
        let headers = vec![
            NockHeaderLink {
                height: 3,
                digest_hex: hex40(&d3),
                parent_hex: hex40(&bad), // should be d2
            },
            NockHeaderLink {
                height: 2,
                digest_hex: hex40(&d2),
                parent_hex: hex40(&d1),
            },
            NockHeaderLink {
                height: 1,
                digest_hex: hex40(&d1),
                parent_hex: hex40(&[0u8; 40]),
            },
        ];
        assert!(verify_header_chain_to_checkpoint(3, &d3, &cp, &headers).is_err());
    }
}
