//! Stubbed payment verification.
//!
//! The legacy worker enforced on-chain payment via nockblocks RPC
//! before a name would move from `%pending` to `%registered`. For the
//! v1 Vesl migration we keep the two-step `register` → `claim` shape
//! but stub this check: every call returns `Ok` with a synthetic
//! tx hash.
//!
//! To re-enable real payment verification, port the logic from:
//!   - `~/nock-names-worker/src/handlers/verify.ts`
//!   - `~/nock-names-worker/src/services/blockchain.ts`
//! The kernel interface does not change: the hull keeps poking
//! `%claim` with a tx hash after a successful check.

use uuid::Uuid;

/// Returns a synthetic tx hash. Replace with real verification when
/// enabling fee enforcement.
pub fn verify(_address: &str, _name: &str, _required_fee: u64) -> Result<String, PaymentError> {
    Ok(format!("stub-{}", Uuid::new_v4()))
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
