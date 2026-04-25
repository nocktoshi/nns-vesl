//! Phase 7 — wallet-side freshness enforcement on claim proofs.
//!
//! ## The attack this closes
//!
//! A malicious NNS operator can freeze the chain follower at an old
//! height and hand-poke arbitrary state into the kernel. The kernel
//! will happily emit a cryptographically valid STARK proof — the
//! proof is *honest* about stale state. A naive wallet that only
//! verifies the STARK + checks digest equality would accept the
//! stale attestation and get stolen names.
//!
//! The defense is a **wallet-side freshness check on `t_nns_height`**:
//! the wallet compares the anchor height committed *inside* the proof
//! (via the bundle's `anchored_tip_height` field) against its own
//! view of the Nockchain tip height, and rejects proofs anchored too
//! far in the past.
//!
//! ## Layering
//!
//! This module is layer 3 of a three-layer defense
//! (see `ARCHITECTURE.md` §7):
//!
//! 1. **Chain-ordered replay** (hull): claims are applied in
//!    `(block_height, tx_index)` order, so honest slow servers
//!    converge.
//! 2. **Frozen-follower no-proof** (kernel): `%prove-claim` refuses
//!    to emit when the bundle's `anchored_tip_height` doesn't match
//!    the kernel's current anchor — see `%anchor-mismatch` in
//!    `hoon/lib/nns-predicates.hoon`.
//! 3. **Wallet freshness check** (this module): closes the manual
//!    operator-pokes-stale-kernel bypass.
//!
//! Only layer 3 is in this file; layer 1 lives in the follower,
//! layer 2 lives in the kernel's `%prove-claim` cause.
//!
//! ## Rule
//!
//! ```text
//! proof.t_nns_height >= wallet.chain_tip_height - max_staleness
//! ```
//!
//! Default `max_staleness = 20` blocks, matching the kernel's
//! `DEFAULT_FINALITY_DEPTH = 10` plus a 10-block margin.
//!
//! Underflow when `chain_tip_height < max_staleness` (very early
//! chain, tests) is interpreted as "everything fresh" — the wallet
//! doesn't care about staleness below the staleness window itself.

use std::fmt;

/// Default staleness window in blocks. Intentionally 2× the default
/// follower finality depth so an honest but temporarily-slow server
/// still produces acceptable proofs. Tighten only if higher-value
/// decisions are being made on proof freshness.
pub const DEFAULT_MAX_STALENESS: u64 = 20;

/// Wallet-side freshness policy.
///
/// Create once at wallet boot with
/// `Freshness::new(DEFAULT_MAX_STALENESS)` (or a tighter value for
/// high-value flows) and pass into every proof verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Freshness {
    max_staleness: u64,
}

impl Default for Freshness {
    fn default() -> Self {
        Self {
            max_staleness: DEFAULT_MAX_STALENESS,
        }
    }
}

impl Freshness {
    /// Construct a policy with the given staleness window (blocks).
    pub const fn new(max_staleness: u64) -> Self {
        Self { max_staleness }
    }

    /// Staleness window in blocks.
    pub const fn max_staleness(&self) -> u64 {
        self.max_staleness
    }

    /// Check a proof's `t_nns_height` against the wallet's current
    /// view of the canonical Nockchain tip height.
    ///
    /// Returns `Ok(())` when `proof_tip_height + max_staleness >=
    /// chain_tip_height`. Rejects with `FreshnessError::ProofTooStale`
    /// otherwise. A proof whose height is *ahead* of the wallet's
    /// chain view is accepted (the wallet is the stale one).
    ///
    /// Note: this does **not** verify that `(t_nns_digest,
    /// t_nns_height)` is actually a prefix of the wallet's canonical
    /// chain. That's a separate check — query Nockchain for the
    /// header at `t_nns_height` and compare digests. See
    /// [`AnchorBindingError`] for callers that want both.
    pub fn check(
        &self,
        proof_tip_height: u64,
        chain_tip_height: u64,
    ) -> Result<(), FreshnessError> {
        // Proof is ahead of (or equal to) the wallet's view — fine.
        if proof_tip_height >= chain_tip_height {
            return Ok(());
        }
        let lag = chain_tip_height - proof_tip_height;
        if lag <= self.max_staleness {
            Ok(())
        } else {
            Err(FreshnessError::ProofTooStale {
                proof_height: proof_tip_height,
                chain_height: chain_tip_height,
                max_staleness: self.max_staleness,
                lag_blocks: lag,
            })
        }
    }
}

/// Why a freshness check rejected a proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FreshnessError {
    /// `chain_height - proof_height` exceeds `max_staleness`.
    ProofTooStale {
        proof_height: u64,
        chain_height: u64,
        max_staleness: u64,
        lag_blocks: u64,
    },
}

impl fmt::Display for FreshnessError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FreshnessError::ProofTooStale {
                proof_height,
                chain_height,
                max_staleness,
                lag_blocks,
            } => write!(
                f,
                "claim proof anchored at height {proof_height} is {lag_blocks} \
                 block(s) behind the wallet's chain tip at height {chain_height}; \
                 max allowed staleness is {max_staleness} block(s)"
            ),
        }
    }
}

impl std::error::Error for FreshnessError {}

/// Optional deeper check: verify the proof's committed anchor matches
/// the wallet's canonical Nockchain view at that height.
///
/// Given the proof's `(t_nns_digest, t_nns_height)` and the digest
/// the wallet retrieves from Nockchain at `t_nns_height`, compare
/// byte-for-byte. A mismatch means either the proof was produced
/// against a fork that isn't canonical, or the wallet and the NNS
/// follower disagree about canonical chain at that height — either
/// way, reject.
///
/// This is separate from [`Freshness::check`] because most wallets
/// want freshness strictly and anchor-binding opportunistically
/// (the extra Nockchain query has latency cost). Compose them as
/// needed.
pub fn check_anchor_binding(
    proof_tip_digest: &[u8],
    wallet_view_digest: &[u8],
) -> Result<(), AnchorBindingError> {
    if proof_tip_digest == wallet_view_digest {
        Ok(())
    } else {
        Err(AnchorBindingError::DigestMismatch {
            proof_digest: proof_tip_digest.to_vec(),
            wallet_digest: wallet_view_digest.to_vec(),
        })
    }
}

/// Why [`check_anchor_binding`] rejected a proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorBindingError {
    /// The proof's anchor digest doesn't match the wallet's
    /// canonical-chain view at the same height.
    DigestMismatch {
        proof_digest: Vec<u8>,
        wallet_digest: Vec<u8>,
    },
}

impl fmt::Display for AnchorBindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnchorBindingError::DigestMismatch { .. } => {
                write!(
                    f,
                    "proof's committed tip digest does not match wallet's \
                     canonical-chain view at the same height (possible fork \
                     or malicious NNS)"
                )
            }
        }
    }
}

impl std::error::Error for AnchorBindingError {}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Freshness::check boundary tests -------------------------------

    #[test]
    fn exact_boundary_accepts() {
        // proof height exactly at chain_tip - max_staleness is OK.
        let fresh = Freshness::new(20);
        assert!(fresh.check(100, 120).is_ok(), "lag==max_staleness must pass");
    }

    #[test]
    fn one_block_inside_boundary_accepts() {
        let fresh = Freshness::new(20);
        assert!(fresh.check(101, 120).is_ok(), "lag<max_staleness must pass");
    }

    #[test]
    fn one_block_past_boundary_rejects() {
        let fresh = Freshness::new(20);
        let err = fresh.check(99, 120).unwrap_err();
        match err {
            FreshnessError::ProofTooStale {
                proof_height,
                chain_height,
                max_staleness,
                lag_blocks,
            } => {
                assert_eq!(proof_height, 99);
                assert_eq!(chain_height, 120);
                assert_eq!(max_staleness, 20);
                assert_eq!(lag_blocks, 21);
            }
        }
    }

    #[test]
    fn proof_at_chain_tip_accepts() {
        let fresh = Freshness::new(20);
        assert!(fresh.check(120, 120).is_ok());
    }

    #[test]
    fn proof_ahead_of_wallet_accepts() {
        let fresh = Freshness::new(20);
        assert!(fresh.check(130, 120).is_ok(), "wallet is behind; accept");
    }

    #[test]
    fn genesis_window_accepts_everything() {
        // Before the chain has even reached max_staleness, the rule
        // collapses — everything is "fresh enough".
        let fresh = Freshness::new(20);
        assert!(fresh.check(0, 5).is_ok());
        assert!(fresh.check(0, 20).is_ok(), "chain_height == max_staleness boundary");
    }

    #[test]
    fn default_policy_uses_20() {
        assert_eq!(Freshness::default().max_staleness(), DEFAULT_MAX_STALENESS);
        assert_eq!(DEFAULT_MAX_STALENESS, 20);
    }

    #[test]
    fn tight_policy_rejects_close_lag() {
        let strict = Freshness::new(2);
        assert!(strict.check(100, 102).is_ok());
        assert!(strict.check(100, 103).is_err());
    }

    #[test]
    fn very_tight_policy_requires_exact_tip() {
        let zero = Freshness::new(0);
        assert!(zero.check(100, 100).is_ok(), "equal is always ok");
        assert!(zero.check(99, 100).is_err(), "any lag rejects with window=0");
    }

    #[test]
    fn freshness_error_display_is_informative() {
        let fresh = Freshness::new(20);
        let err = fresh.check(50, 100).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("50"));
        assert!(msg.contains("100"));
        assert!(msg.contains("20"));
        assert!(msg.contains("50")); // lag blocks
    }

    // --- check_anchor_binding ------------------------------------------

    #[test]
    fn matching_anchor_binding_accepts() {
        let digest = vec![0xAA; 40];
        assert!(check_anchor_binding(&digest, &digest).is_ok());
    }

    #[test]
    fn mismatched_anchor_binding_rejects() {
        let a = vec![0xAA; 40];
        let b = vec![0xBB; 40];
        let err = check_anchor_binding(&a, &b).unwrap_err();
        match err {
            AnchorBindingError::DigestMismatch {
                proof_digest,
                wallet_digest,
            } => {
                assert_eq!(proof_digest, a);
                assert_eq!(wallet_digest, b);
            }
        }
    }

    #[test]
    fn empty_vs_digest_rejects() {
        let a: Vec<u8> = Vec::new();
        let b = vec![0xAA; 40];
        assert!(check_anchor_binding(&a, &b).is_err());
    }
}
