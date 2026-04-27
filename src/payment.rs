//! Payment verification.
//!
//! v1 behavior supports optional `txHash` on `POST /claim`.
//! In non-local modes, txHash is required by the API handler.
//! In local mode, missing txHash falls back to a synthetic id.

use crate::chain::{fetch_transaction_details, transaction_is_accepted};
use nockapp_grpc::pb::public::v2::transaction_output::AmountRequired;
use nockapp_grpc::pb::public::v2::{TransactionDetails, TransactionOutput};
use uuid::Uuid;

pub const TREASURY_LOCK_ROOT_B58: &str = "A3LoWjxurwiyzhkv8sgDv2MVu9PwgWHmqoncXw9GEQ5M3qx46svvadE";

/// Nockchain: `65536` nicks = `1` NOCK (atomic settlement unit on-chain).
pub const NICKS_PER_NOCK: u64 = 65_536;

/// Verify payment and return the tx hash that should be committed in
/// kernel state for C4 payment-replay protection.
pub async fn verify(
    settlement: &vesl_core::SettlementConfig,
    address: &str,
    _name: &str,
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
            let accepted =
                transaction_is_accepted(endpoint, settlement.accept_timeout_secs, tx_hash)
                    .await
                    .map_err(PaymentError::Rpc)?;
            if !accepted {
                return Err(PaymentError::TxNotAccepted {
                    tx_hash: tx_hash.to_string(),
                    endpoint: endpoint.to_string(),
                });
            }
            let details = fetch_transaction_details(endpoint, tx_hash)
                .await
                .map_err(PaymentError::Rpc)?;
            let sender_present = details
                .inputs
                .iter()
                .any(|input| input.note_name_b58.trim() == address);
            if !sender_present {
                return Err(PaymentError::SenderMismatch {
                    tx_hash: tx_hash.to_string(),
                    address: address.to_string(),
                });
            }
            let treasury_paid = sum_treasury_outputs_v1(&details);
            if treasury_paid < required_fee {
                return Err(PaymentError::Underpaid {
                    tx_hash: tx_hash.to_string(),
                    required_fee,
                    treasury_paid,
                });
            }
            Ok(tx_hash.to_string())
        }
        None => Ok(format!("stub-{}", Uuid::new_v4())),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PaymentError {
    /// RPC answered, but the node does not treat this tx as accepted yet
    /// (pending), or it is unknown on this network / endpoint.
    #[error(
        "transaction {tx_hash} not accepted by chain endpoint {endpoint} \
         (same network as the tx? node synced? tx still pending?)"
    )]
    TxNotAccepted { tx_hash: String, endpoint: String },
    #[error("transaction {tx_hash} does not include an input owned by {address}")]
    SenderMismatch { tx_hash: String, address: String },
    #[error(
        "transaction {tx_hash} underpaid treasury: \
         paid {treasury_paid} nicks, required at least {required_fee} nicks"
    )]
    Underpaid {
        tx_hash: String,
        required_fee: u64,
        treasury_paid: u64,
    },
    #[error("blockchain query failed: {0}")]
    Rpc(String),
}

pub(crate) fn output_amount_nicks(out: &TransactionOutput) -> Option<u64> {
    match &out.amount_required {
        Some(AmountRequired::Amount(n)) => Some(n.value),
        _ => None,
    }
}

pub(crate) fn sum_treasury_outputs_v1(details: &TransactionDetails) -> u64 {
    details
        .outputs
        .iter()
        .filter(|o| o.note_name_b58 == TREASURY_LOCK_ROOT_B58.trim())
        .filter_map(output_amount_nicks)
        .fold(0u64, |s, v| s.saturating_add(v))
}

pub fn fee_for_name(name: &str) -> u64 {
    let stem = name.strip_suffix(".nock").unwrap_or(name);
    let len = stem.chars().count();
    if len == 0 {
        0
    } else if len >= 10 {
        6_553_600
    } else if len >= 5 {
        32_768_000
    } else {
        327_680_000
    }
}

/// Same tier as [`fee_for_name`], in whole NOCK (API / user-facing).
///
/// Internal `%claim` and on-chain witness amounts use [`fee_for_name`] (nicks).
pub fn nock_for_name(name: &str) -> u64 {
    fee_for_name(name) / NICKS_PER_NOCK
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fee_for_name_matches_tiers() {
        assert_eq!(fee_for_name("a.nock"), 327_680_000);
        assert_eq!(fee_for_name("abcd.nock"), 327_680_000);
        assert_eq!(fee_for_name("abcde.nock"), 32_768_000);
        assert_eq!(fee_for_name("abcdefghi.nock"), 32_768_000);
        assert_eq!(fee_for_name("abcdefghij.nock"), 6_553_600);
        assert_eq!(fee_for_name(""), 0);
    }

    #[test]
    fn nock_for_name_matches_fee_for_name() {
        assert_eq!(
            nock_for_name("a.nock"),
            fee_for_name("a.nock") / NICKS_PER_NOCK
        );
        assert_eq!(
            nock_for_name("abcd.nock"),
            fee_for_name("abcd.nock") / NICKS_PER_NOCK
        );
        assert_eq!(
            nock_for_name("abcde.nock"),
            fee_for_name("abcde.nock") / NICKS_PER_NOCK
        );
        assert_eq!(
            nock_for_name("abcdefghi.nock"),
            fee_for_name("abcdefghi.nock") / NICKS_PER_NOCK
        );
        assert_eq!(
            nock_for_name("abcdefghij.nock"),
            fee_for_name("abcdefghij.nock") / NICKS_PER_NOCK
        );
        assert_eq!(nock_for_name(""), 0);
    }
}
