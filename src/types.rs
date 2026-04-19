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
pub struct ClaimResponse {
    pub message: String,
    pub registration: Registration,
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
