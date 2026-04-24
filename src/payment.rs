//! Payment verification.
//!
//! v1 behavior is chain-aware when a caller supplies `txHash` on
//! `POST /claim`: the hull checks whether that tx is accepted on the
//! configured chain endpoint. In local mode (or when no `txHash` is
//! provided) we fall back to a synthetic tx id for development.

use uuid::Uuid;

use crate::chain::transaction_is_accepted;

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
        // Local/dev mode fallback: keep deterministic shape while
        // avoiding mandatory chain integration in tests.
        None => Ok(format!("stub-{}", Uuid::new_v4())),
        Some(tx_hash) => {
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
