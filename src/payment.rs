//! Payment verification.
//!
//! v1 behavior supports optional `txHash` on `POST /claim`.
//! In non-local modes, txHash is required by the API handler.
//! In local mode, missing txHash falls back to a synthetic id.

use crate::chain::transaction_is_accepted;
use uuid::Uuid;

/// Verify payment and return the tx hash that should be committed in
/// kernel state for C4 payment-replay protection.
pub async fn verify(
    settlement: &vesl_core::SettlementConfig,
    address: &str,
    name: &str,
    required_fee: u64,
    claimed_tx_hash: Option<&str>,
) -> Result<String, PaymentError> {
    match claimed_tx_hash.map(str::trim).filter(|s| !s.is_empty()) {
        Some(tx_hash) => {
            if matches!(settlement.mode, vesl_core::SettlementMode::Local) {
                return Ok(tx_hash.to_string());
            }
            let endpoint = settlement
                .chain_endpoint
                .as_deref()
                .ok_or_else(|| PaymentError::Rpc("chain endpoint not configured".into()))?;
            let accepted = transaction_is_accepted(
                endpoint,
                settlement.accept_timeout_secs,
                tx_hash,
            )
            .await
            .map_err(PaymentError::Rpc)?;
            if !accepted {
                return Err(PaymentError::NotFound {
                    address: address.to_string(),
                    name: name.to_string(),
                });
            }
            // TODO(phase1+): validate sender/recipient/amount against
            // required_fee once tx-details surface is available.
            let _ = required_fee;
            Ok(tx_hash.to_string())
        }
        None => Ok(format!("stub-{}", Uuid::new_v4())),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    #[error("no valid payment found for {address} to register {name}")]
    NotFound { address: String, name: String },
    #[error("blockchain query failed: {0}")]
    Rpc(String),
}

/// Fee schedule, ported from
/// `~/nock-names-worker/src/utils/constants.ts::getFee`.
/// Length refers to the stem (before `.nock`).
pub fn fee_for_name(name: &str) -> u64 {
    let stem = name.strip_suffix(".nock").unwrap_or(name);
    let len = stem.chars().count();
    if len == 0 {
        0
    } else if len >= 10 {
        100
    } else if len >= 5 {
        500
    } else {
        5000
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_matches_legacy() {
        assert_eq!(fee_for_name("a.nock"), 5000);
        assert_eq!(fee_for_name("abcd.nock"), 5000);
        assert_eq!(fee_for_name("abcde.nock"), 500);
        assert_eq!(fee_for_name("abcdefghi.nock"), 500);
        assert_eq!(fee_for_name("abcdefghij.nock"), 100);
        assert_eq!(fee_for_name(""), 0);
    }
}
