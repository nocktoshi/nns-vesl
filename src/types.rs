//! Wire types shared between handlers, state, and kernel builders.
//!
//! JSON shapes mirror the legacy worker at
//! `~/nock-names-worker/src/types/index.ts` so existing
//! clients can move to this hull without changes.

use serde::{Deserialize, Serialize};

/// The registration record, matching the legacy `Registration`
/// interface exactly (field names and casing).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Registration {
    pub address: String,
    pub name: String,
    pub status: RegistrationStatus,
    /// Unix-millis timestamp (matches legacy `Date.now()` semantics).
    pub timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
    #[serde(rename = "txHash", skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RegistrationStatus {
    Pending,
    Registered,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ClaimLifecycleStatus {
    Submitted,
    Confirmed,
    Finalized,
    Rejected,
}

/// Request body for `POST /register`.
#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub address: String,
    pub name: String,
}

/// Request body for `POST /claim`: promote a pending reservation
/// to a registered name by exchanging a valid payment for a kernel
/// `%claim` poke. Named to mirror the kernel's `%claim` cause.
#[derive(Debug, Deserialize)]
pub struct ClaimRequest {
    pub address: String,
    pub name: String,
    #[serde(rename = "txHash", default)]
    pub tx_hash: Option<String>,
}

/// Request body for `POST /primary`: designate `name` as the
/// reverse-lookup target for `address`. The kernel rejects if the
/// address does not own the name.
#[derive(Debug, Deserialize)]
pub struct SetPrimaryRequest {
    pub address: String,
    pub name: String,
}

/// `POST /claim` success body.
#[derive(Debug, Serialize)]
pub struct ClaimSubmissionResponse {
    pub message: String,
    pub claim_id: String,
    pub status: ClaimLifecycleStatus,
    #[serde(rename = "txHash", skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration: Option<Registration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimStatusResponse {
    pub claim_id: String,
    pub status: ClaimLifecycleStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration: Option<Registration>,
    #[serde(rename = "txHash", skip_serializing_if = "Option::is_none")]
    pub tx_hash: Option<String>,
}

/// `POST /primary` success body.
#[derive(Debug, Serialize)]
pub struct SetPrimaryResponse {
    pub address: String,
    pub name: String,
}

/// `GET /search?address=...` result. `primary` is the name this
/// address currently reverse-lookups to, or `None` if it owns
/// nothing registered yet.
#[derive(Debug, Serialize)]
pub struct SearchByAddressResponse {
    pub address: String,
    pub pending: Vec<Registration>,
    pub verified: Vec<Registration>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
}

/// `GET /search?name=...` result. The legacy worker returns
/// `{name, price, status, owner?, registeredAt?}` and status is
/// one of `registered | pending | available`.
/// `price` is denominated in whole NOCK (`65536` nicks = `1` NOCK on chain).
#[derive(Debug, Serialize)]
pub struct SearchByNameResponse {
    pub name: String,
    pub price: u64,
    pub status: SearchStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(rename = "registeredAt", skip_serializing_if = "Option::is_none")]
    pub registered_at: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchStatus {
    Registered,
    Pending,
    Available,
}

/// `POST /settle` success body.
///
/// Batched: one settle call produces one graft note covering every
/// name claimed since the previous successful settle. The raw atom
/// bytes are surfaced as lowercase hex strings so the response is
/// JSON-safe without leaking Nock encoding details. `note_id` is a
/// deterministic hash of the sorted batch contents, so resubmitting
/// the exact same batch returns the same id and the graft rejects
/// it as a replay (surfaced here as a 400).
#[derive(Debug, Serialize)]
pub struct SettleResponse {
    /// Commitment claim-id at the time the batch was packaged. This
    /// is also the hull's new `last-settled-claim-id`.
    pub claim_id: u64,
    /// Number of names in the batch.
    pub count: u64,
    /// Sorted (canonical `aor` order) names that were settled.
    pub names: Vec<String>,
    /// Hull-id the batch was settled against (hex of little-endian
    /// atom bytes).
    pub hull: String,
    /// Merkle root the batch's inclusion proofs were checked against
    /// (hex of little-endian atom bytes).
    pub root: String,
    /// `hash-leaf(jam sorted-batch)` — batch-level replay key.
    pub note_id: String,
    #[serde(rename = "settlementTx", skip_serializing_if = "Option::is_none")]
    pub settlement_tx: Option<String>,
}

/// One sibling node in a Merkle inclusion proof.
///
/// `side` is a string — `"left"` or `"right"` — describing where
/// the sibling hash sits relative to the running hash during
/// verification. This matches the Hoon-side convention in
/// `hoon/lib/vesl-merkle.hoon::verify-chunk`:
///
///   - `"left"`  (Hoon `%.y`): compute `hash-pair(sibling, cur)`
///   - `"right"` (Hoon `%.n`): compute `hash-pair(cur, sibling)`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofNodeView {
    /// Hex-encoded little-endian sibling hash bytes.
    pub hash: String,
    pub side: ProofSide,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProofSide {
    Left,
    Right,
}

/// `GET /proof?name=...` success body. Everything a client needs
/// to independently verify that `(name, owner, txHash)` is a row
/// at the committed Merkle `root`:
///
/// 1. Compute `chunk = jam([name owner txHash])` (Hoon `+jam` on the
///    tuple — Rust port in `nock_noun_rs`).
/// 2. Run `verify-chunk(chunk, proof, root)` from
///    `hoon/lib/vesl-merkle.hoon` (tip5 primitives `hash-leaf` and
///    `hash-pair`).
///
/// `claim_id` is the kernel's `claim-id` counter at the time the
/// row was written; `hull` is the current commitment hull-id. Both
/// are included so a client can cross-reference the graft's
/// `registered` map to establish which commitment the row lives in.
///
/// Caveat: this endpoint only attests Merkle inclusion at the
/// current `root` — not that `txHash` is unique in history (kernel
/// `tx-hashes` set, trusted) nor that the root is a deterministic
/// result of honest claims (see README "Proof scope").
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofResponse {
    pub name: String,
    pub owner: String,
    #[serde(rename = "txHash")]
    pub tx_hash: String,
    pub claim_id: u64,
    /// Hex-encoded Merkle root the proof verifies against.
    pub root: String,
    /// Hex-encoded hull-id this root was committed under.
    pub hull: String,
    /// Sibling chain from leaf to root, leaf-first.
    pub proof: Vec<ProofNodeView>,
    /// Transition-proof anchor metadata for light clients. The
    /// transition proof bytes are surfaced separately as an opaque
    /// hex payload when available.
    pub transition: TransitionProofMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transition_proof: Option<String>,
    /// **Phase 7**: the follower-advanced Nockchain tip at the
    /// moment this proof was generated. Wallets use this together
    /// with their own view of the canonical chain tip to reject
    /// stale proofs:
    ///
    /// ```text
    /// anchor.tip_height >= wallet_chain_tip_height - max_staleness
    /// ```
    ///
    /// Default `max_staleness = 20` blocks. Without this check a
    /// malicious server could freeze its follower and hand-poke
    /// stale state into a cryptographically valid proof. See
    /// `ARCHITECTURE.md` §7 for the full attack analysis.
    ///
    /// Optional for backwards compat — older NNS servers omit it.
    /// New light clients SHOULD require it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anchor: Option<ProofAnchor>,
}

/// Follower-advanced anchor snapshot carried in `ProofResponse`.
/// Phase 7 — see [`ProofResponse::anchor`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProofAnchor {
    /// Hex-encoded 40-byte Tip5 digest of the tip block. Wallets
    /// cross-reference this against their own canonical-chain view
    /// at `tip_height` to detect fork attacks.
    pub tip_digest: String,
    /// Nockchain height of the tip block. Freshness rule compares
    /// against the wallet's canonical tip height.
    pub tip_height: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionProofMetadata {
    pub mode: String,
    pub settled_claim_id: u64,
}

/// `GET /pending-batch` success body.
///
/// Preview of what the next `POST /settle` would bundle, without
/// actually dispatching it.
#[derive(Debug, Serialize)]
pub struct PendingBatchResponse {
    /// Current `claim-id` — the highest the batch could possibly
    /// advance `last-settled-claim-id` to.
    pub claim_id: u64,
    /// Current `last-settled-claim-id`. The pending window is every
    /// `entry.claim-id` strictly greater than this.
    pub last_settled_claim_id: u64,
    /// Number of names in the pending window (= `names.len()`).
    pub count: u64,
    /// Sorted names in the pending window.
    pub names: Vec<String>,
}
