//! Hull application state.
//!
//! Split of authority:
//!
//!   - The **kernel** is the authoritative registry. It holds
//!     `names=(map @t [owner tx-hash])`, `tx-hashes=(set @t)`, and
//!     `primaries=(map @t @t)` (owner -> primary name). `%claim`
//!     enforces name- and payment-uniqueness and auto-assigns a
//!     primary on first claim; `%set-primary` enforces owner-gated
//!     primary updates.
//!
//!   - The **hull mirror** is a denormalized read cache for the
//!     HTTP API: pending reservations (which never hit the kernel)
//!     plus a reverse `address -> primary name` index so
//!     `/resolve?address=` is an O(1) lookup.
//!
//! One address can own many names. The `names` field carries every
//! registration (and is scanned for `/search?address=`); the
//! `primaries` field is the single reverse-lookup target. The mirror
//! only writes `primaries` in response to a `%primary-set` effect
//! from the kernel — never via blind "last write wins" on insert.
//!
//! The mirror is an in-memory cache persisted as JSON after every
//! mutation. It is rebuildable from the kernel (for registered
//! entries) plus nothing (for pending), so deleting the mirror only
//! loses pending reservations. Payment-replay protection lives in
//! the kernel's `tx-hashes` set, not here — there is no hull-side
//! used-tx-hash cache to get out of sync.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use nockapp::NockApp;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::types::{ClaimLifecycleStatus, ClaimStatusResponse, Registration, RegistrationStatus};

/// Hull-side state: mirror JSON, paths, settlement config, follower
/// telemetry. **Lock ordering:** never acquire [`AppState::hull`] while
/// holding [`AppState::kernel`]. Kernel work (`peek`/`poke`) may run
/// concurrently with short hull reads (`GET /status`) as long as callers
/// `drop(kernel)` before locking hull.
pub struct HullState {
    pub mirror: Mirror,
    pub output_dir: PathBuf,
    pub settlement: vesl_core::SettlementConfig,
    pub follower: FollowerObservability,
}

pub const MIRROR_FILE: &str = ".nns-mirror.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Mirror {
    /// name -> registration. Source of truth for GET handlers that
    /// list everything (`/verified`, `/pending`) or look a name up
    /// directly (`/resolve?name=`, `/search?name=`).
    pub names: HashMap<String, Registration>,
    /// address -> primary name. One entry per owner; always the
    /// designated reverse-lookup target. Populated from kernel
    /// `%primary-set` effects only — never from blind inserts —
    /// so it does not drift when one address owns many names.
    #[serde(alias = "addresses")]
    pub primaries: HashMap<String, String>,
    /// Latest commitment snapshot reported by the kernel (via
    /// `%claim-count-bumped` effects on `%claim`). Cached so `/status`
    /// and `/snapshot` can answer without a peek and so clients
    /// can correlate a claim response with the settlement hull.
    /// `None` until the first successful `%claim`.
    #[serde(default)]
    pub snapshot: Option<SnapshotView>,
    /// Highest `claim-id` whose batch has been successfully settled.
    /// Advanced from `%batch-settled` effects on `POST /settle`.
    /// `0` means "nothing settled yet"; this is also the kernel's
    /// default, so a fresh mirror is consistent with a fresh kernel.
    #[serde(default)]
    pub last_settled_claim_id: u64,
    /// Queue of submitted claims awaiting follower replay.
    #[serde(default)]
    pub submitted_claims: HashMap<String, QueuedClaim>,
    /// Monotonic local submission sequence used for deterministic replay
    /// when multiple claims are pending.
    #[serde(default)]
    pub claim_submit_seq: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueuedClaim {
    pub claim_id: String,
    pub submit_seq: u64,
    pub address: String,
    pub name: String,
    pub fee: u64,
    pub tx_hash: String,
    pub status: ClaimLifecycleStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// JSON-friendly view of the kernel's current commitment snapshot.
/// `hull` and `root` are the raw atom bytes the kernel emitted,
/// hex-encoded for transport.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SnapshotView {
    pub claim_id: u64,
    /// Hex-encoded hull-id (little-endian atom bytes).
    pub hull: String,
    /// Hex-encoded Merkle root (little-endian atom bytes).
    pub root: String,
}

impl Mirror {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join(MIRROR_FILE);
        match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
            Err(_) => Mirror::default(),
        }
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        let path = dir.join(MIRROR_FILE);
        let tmp = dir.join(format!("{MIRROR_FILE}.tmp"));
        let bytes = serde_json::to_vec_pretty(self).unwrap();
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(tmp, path)
    }

    /// Insert / replace a name row. Does **not** touch `primaries`
    /// — that index is driven by kernel `%primary-set` effects via
    /// [`Mirror::set_primary`], not by per-row writes. Multiple
    /// names for the same address coexist naturally here.
    pub fn insert(&mut self, reg: Registration) {
        self.names.insert(reg.name.clone(), reg);
    }

    /// Record that `address` wants reverse-lookup to resolve to
    /// `name`. Called in response to a kernel `%primary-set` effect
    /// (from either a first `%claim` or an explicit `%set-primary`).
    pub fn set_primary(&mut self, address: String, name: String) {
        self.primaries.insert(address, name);
    }

    /// Record a new commitment snapshot reported by the kernel via
    /// `%claim-count-bumped`. Overwrites the previous snapshot — the
    /// authoritative history lives in the graft state, not here.
    pub fn set_snapshot(&mut self, claim_id: u64, hull: &[u8], root: &[u8]) {
        self.snapshot = Some(SnapshotView {
            claim_id,
            hull: hex_encode(hull),
            root: hex_encode(root),
        });
    }

    /// Record the kernel's new `last-settled-claim-id` after a
    /// successful `%settle-batch`. Monotonic: only moves forward.
    pub fn set_last_settled_claim_id(&mut self, claim_id: u64) {
        if claim_id > self.last_settled_claim_id {
            self.last_settled_claim_id = claim_id;
        }
    }

    pub fn by_status(&self, status: RegistrationStatus) -> Vec<Registration> {
        let mut v: Vec<Registration> = self
            .names
            .values()
            .filter(|r| r.status == status)
            .cloned()
            .collect();
        v.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        v
    }

    pub fn enqueue_claim(
        &mut self,
        claim_id: String,
        address: String,
        name: String,
        fee: u64,
        tx_hash: String,
    ) {
        self.claim_submit_seq = self.claim_submit_seq.saturating_add(1);
        let queued = QueuedClaim {
            claim_id: claim_id.clone(),
            submit_seq: self.claim_submit_seq,
            address,
            name,
            fee,
            tx_hash,
            status: ClaimLifecycleStatus::Submitted,
            reason: None,
        };
        self.submitted_claims.insert(claim_id, queued);
    }

    pub fn pending_claims_in_order(&self) -> Vec<QueuedClaim> {
        let mut out: Vec<QueuedClaim> = self
            .submitted_claims
            .values()
            .filter(|c| matches!(c.status, ClaimLifecycleStatus::Submitted | ClaimLifecycleStatus::Confirmed))
            .cloned()
            .collect();
        out.sort_by_key(|c| c.submit_seq);
        out
    }

    pub fn update_claim_status(
        &mut self,
        claim_id: &str,
        status: ClaimLifecycleStatus,
        reason: Option<String>,
    ) {
        if let Some(c) = self.submitted_claims.get_mut(claim_id) {
            c.status = status;
            c.reason = reason;
        }
    }

    pub fn claim_status(&self, claim_id: &str) -> Option<ClaimStatusResponse> {
        let claim = self.submitted_claims.get(claim_id)?;
        let registration = self.names.get(&claim.name).cloned();
        Some(ClaimStatusResponse {
            claim_id: claim.claim_id.clone(),
            status: claim.status,
            reason: claim.reason.clone(),
            registration,
            tx_hash: Some(claim.tx_hash.clone()),
        })
    }
}

/// **Phase 7.1 — Operator observability.** Runtime follower
/// telemetry exposed through `/status` and `/anchor` so operators
/// can answer "is the follower stuck?" with a single HTTP call.
///
/// Not persisted. Resets on process restart — which is the right
/// behaviour, because staleness of a "last advance at T" timestamp
/// across restarts would be misleading. The authoritative kernel
/// anchor is still in kernel state; this only tracks what the
/// follower process observed during its lifetime.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FollowerObservability {
    /// Most recent chain-tip height the follower learned from the
    /// chain endpoint. `None` in local mode (no endpoint) or before
    /// the first successful `fetch_current_tip_height`.
    pub last_chain_tip_height: Option<u64>,
    /// Epoch-millis timestamp of [`last_chain_tip_height`].
    pub last_chain_tip_observed_at_epoch_ms: Option<u64>,
    /// Epoch-millis timestamp of the most recent successful
    /// `%advance-tip` poke. `None` until the first advance completes.
    pub last_advance_at_epoch_ms: Option<u64>,
    /// Tip height reached by the last successful advance.
    pub last_advance_tip_height: Option<u64>,
    /// Number of headers ingested by the last successful advance
    /// (as reported by the kernel's `%anchor-advanced` effect).
    pub last_advance_count: Option<u64>,
    /// Most recent follower-tick failure message. Cleared on the
    /// next successful tick so stale errors don't confuse operators.
    pub last_error: Option<String>,
    /// Epoch-millis timestamp of [`last_error`].
    pub last_error_at_epoch_ms: Option<u64>,
    /// Which follower phase the last error came from. One of
    /// `"anchor_peek"`, `"plan"`, `"header_fetch"`, `"advance_poke"`,
    /// or `"claim_tick"`. Strongly typed as a static string so log
    /// aggregators can histogram on it.
    pub last_error_phase: Option<&'static str>,
}

impl FollowerObservability {
    pub fn record_advance(&mut self, tip_height: u64, count: u64, now_ms: u64) {
        self.last_advance_at_epoch_ms = Some(now_ms);
        self.last_advance_tip_height = Some(tip_height);
        self.last_advance_count = Some(count);
        self.last_error = None;
        self.last_error_at_epoch_ms = None;
        self.last_error_phase = None;
    }

    pub fn record_chain_tip(&mut self, tip: u64, now_ms: u64) {
        self.last_chain_tip_height = Some(tip);
        self.last_chain_tip_observed_at_epoch_ms = Some(now_ms);
    }

    pub fn record_error(&mut self, phase: &'static str, err: String, now_ms: u64) {
        self.last_error = Some(err);
        self.last_error_at_epoch_ms = Some(now_ms);
        self.last_error_phase = Some(phase);
    }
}

/// Shared hull + kernel state. The kernel mutex serializes all Nock I/O;
/// the hull mutex covers the mirror, settlement snapshot, and follower
/// telemetry so `GET /status` never waits on a long `%advance-tip` poke.
pub struct AppState {
    pub kernel: Mutex<NockApp>,
    pub hull: Mutex<HullState>,
}

pub type SharedState = Arc<AppState>;

impl HullState {
    pub fn persist_mirror(&self) {
        if let Err(e) = self.mirror.save(&self.output_dir) {
            tracing::error!("failed to persist mirror: {e}");
        }
    }
}

impl AppState {
    pub fn new(
        app: NockApp,
        output_dir: PathBuf,
        settlement: vesl_core::SettlementConfig,
    ) -> Self {
        let mirror = Mirror::load(&output_dir);
        Self {
            kernel: Mutex::new(app),
            hull: Mutex::new(HullState {
                mirror,
                output_dir,
                settlement,
                follower: FollowerObservability::default(),
            }),
        }
    }

    /// Current monotonic-ish timestamp for telemetry. Wall-clock
    /// `SystemTime` is fine here because we use it for human-readable
    /// "last advanced N seconds ago" math, not anything requiring
    /// strict ordering. Falls back to `0` if the system clock is
    /// before 1970 (shouldn't happen, but don't panic the follower).
    pub fn now_epoch_ms() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    /// Full flush: kernel checkpoint then mirror JSON. **Lock order:**
    /// kernel is acquired and released before touching `hull`, so no
    /// code waits on `hull` while holding `kernel`.
    pub async fn persist_all(&self) {
        if let Err(e) = self.kernel.lock().await.save_blocking().await {
            tracing::error!("failed to save kernel checkpoint: {e:?}");
        }
        let h = self.hull.lock().await;
        if let Err(e) = h.mirror.save(&h.output_dir) {
            tracing::error!("failed to persist mirror: {e}");
        }
    }
}

/// Encode raw atom bytes as lowercase hex. Kept local (no extra
/// dep) because we only need this for the snapshot + settlement
/// JSON surfaces.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}
