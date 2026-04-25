//! HTTP API — axum server.
//!
//! Endpoint names mirror the kernel's poke names where possible —
//! `POST /claim` pokes `%claim`, `POST /primary` pokes `%set-primary`.
//! This is a breaking departure from the legacy Cloudflare worker
//! (which used `POST /verify`): the kernel is the authority, so the
//! HTTP surface reads in its vocabulary.
//!
//!   POST /register        create pending reservation
//!   POST /claim           promote pending -> registered (kernel %claim)
//!   GET  /claim-status    check async claim replay status
//!   POST /primary         designate which of caller's names is primary
//!   POST /settle          batch-settle everything claimed since the
//!                         previous successful settle (one note per call)
//!   GET  /snapshot        current commitment (claim-id, hull, root)
//!   GET  /pending-batch   preview what /settle would bundle right now
//!   GET  /pending         list all pending reservations, newest first
//!   GET  /verified        list all registered, newest first
//!   GET  /resolve         ?name=... or ?address=...   (address -> primary)
//!   GET  /proof           ?name=... [&address=...] — Merkle inclusion
//!                         bundle for (name, owner, txHash) at current root
//!   GET  /search          ?name=<label> or ?address=...
//!   GET  /health          liveness
//!   GET  /status          diagnostic
//!
//! One address can own many names. `/resolve?address=` returns the
//! owner's primary (kernel-authoritative, settable via `/primary`).
//! `/search?address=` returns every name the address has (pending
//! + verified) and the primary alongside.
//!
//! CORS is open (`*`) to match legacy behavior.

use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::{header, Method, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use nockapp::noun::slab::NounSlab;
use nockapp::NockApp;
use nockapp::wire::{SystemWire, Wire};
use tokio::sync::Mutex as TokioMutex;
use serde::Serialize;
use serde_json::json;
use tokio::time::timeout;
use tower_http::cors::{Any, CorsLayer};

const POKE_TIMEOUT: Duration = Duration::from_secs(30);

async fn poke_with_timeout(
    kernel: &TokioMutex<NockApp>,
    slab: NounSlab,
) -> Result<Vec<NounSlab>, String> {
    let mut app = kernel.lock().await;
    match timeout(POKE_TIMEOUT, app.poke(SystemWire.to_wire(), slab)).await {
        Ok(Ok(effects)) => Ok(effects),
        Ok(Err(e)) => Err(format!("kernel error: {e:?}")),
        Err(_) => Err(format!(
            "kernel poke exceeded {}s timeout",
            POKE_TIMEOUT.as_secs()
        )),
    }
}


use crate::kernel::{
    build_last_settled_peek, build_owner_peek, build_pending_batch_peek,
    build_anchor_peek, build_proof_peek, build_set_primary_poke, build_settle_batch_poke,
    build_snapshot_peek, decode_anchor, decode_last_settled, decode_owner, decode_pending_batch,
    decode_proof, decode_snapshot, first_batch_settled, first_error_message, first_primary_set,
    first_vesl_settled,
};
use crate::claim_note::ClaimNoteV1;
use crate::payment;
use crate::state::{hex_encode, SharedState};
use crate::types::{
    ClaimRequest, ClaimStatusResponse, ClaimSubmissionResponse, ClaimLifecycleStatus, PendingBatchResponse, ProofAnchor, ProofNodeView, ProofResponse, ProofSide, TransitionProofMetadata,
    RegisterRequest, Registration, RegistrationStatus, SearchByAddressResponse,
    SearchByNameResponse, SearchStatus, SetPrimaryRequest, SetPrimaryResponse, SettleResponse,
};

// ---------------------------------------------------------------------------
// Validation 
// ---------------------------------------------------------------------------

pub fn is_valid_address(address: &str) -> bool {
    let a = address.trim();
    let len = a.len();
    // Match the legacy worker's (buggy) operator precedence exactly —
    // see the note in the plan. Documented here for parity:
    //   (len > 43 && len < 57) || (len === 132 && /^[a-zA-Z0-9]+$/.test(a))
    if len > 43 && len < 57 {
        return true;
    }
    len == 132 && a.chars().all(|c| c.is_ascii_alphanumeric())
}

pub fn is_valid_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".nock") else {
        return false;
    };
    !stem.is_empty() && stem.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub(crate) fn now_millis_for_internal() -> u64 {
    now_millis()
}

// ---------------------------------------------------------------------------
// Error helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

fn bad_request(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody { error: msg.into() }),
    )
}

fn server_error(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorBody { error: msg.into() }),
    )
}

// ---------------------------------------------------------------------------
// Router
// ---------------------------------------------------------------------------

pub fn router(state: SharedState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE]);

    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/register", post(register_handler))
        .route("/claim", post(claim_handler))
        .route("/claim-status", get(claim_status_handler))
        .route("/primary", post(set_primary_handler))
        .route("/settle", post(settle_handler))
        .route("/snapshot", get(snapshot_handler))
        .route("/pending-batch", get(pending_batch_handler))
        .route("/pending", get(pending_handler))
        .route("/verified", get(verified_handler))
        .route("/resolve", get(resolve_handler))
        .route("/proof", get(proof_handler))
        .route("/search", get(search_handler))
        // Phase 7.1 — Operator observability.
        .route("/anchor", get(anchor_handler))
        .route("/admin/advance-tip-now", post(admin_advance_tip_now))
        .layer(cors)
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn status(State(state): State<SharedState>) -> Json<serde_json::Value> {
    // Hull first so we never hold the kernel mutex while waiting on
    // hull I/O — keeps `/status` responsive during long `%advance-tip`
    // pokes in the follower.
    let (
        settlement_mode,
        chain_endpoint,
        names_count,
        pending_count,
        registered_count,
        snapshot,
        last_settled_claim_id,
        pending_batch_count,
        follower,
    ) = {
        let h = state.hull.lock().await;
        let claim_id = h.mirror.snapshot.as_ref().map(|s| s.claim_id).unwrap_or(0);
        let pending_batch_count = claim_id.saturating_sub(h.mirror.last_settled_claim_id);
        (
            h.settlement.mode.to_string(),
            h.settlement.chain_endpoint.clone(),
            h.mirror.names.len(),
            h.mirror.by_status(RegistrationStatus::Pending).len(),
            h.mirror.by_status(RegistrationStatus::Registered).len(),
            h.mirror.snapshot.clone(),
            h.mirror.last_settled_claim_id,
            pending_batch_count,
            h.follower.clone(),
        )
    };

    let anchor_kernel = {
        let mut k = state.kernel.lock().await;
        k.peek(build_anchor_peek())
            .await
            .ok()
            .and_then(|slab| decode_anchor(&slab).ok())
    };

    let (anchor_lag_blocks, follower_is_caught_up) = match (
        follower.last_chain_tip_height,
        anchor_kernel.as_ref().map(|a| a.tip_height),
    ) {
        (Some(chain_tip), Some(anchor_tip)) => {
            let lag = chain_tip.saturating_sub(anchor_tip);
            let caught_up = lag <= crate::chain_follower::DEFAULT_FINALITY_DEPTH + 1;
            (Some(lag), Some(caught_up))
        }
        _ => (None, None),
    };

    let follower_age_seconds = follower
        .last_advance_at_epoch_ms
        .map(|t| crate::state::AppState::now_epoch_ms().saturating_sub(t) / 1000);

    Json(json!({
        "settlement_mode": settlement_mode,
        "chain_endpoint": chain_endpoint,
        "names_count": names_count,
        "pending_count": pending_count,
        "registered_count": registered_count,
        "snapshot": snapshot,
        "last_settled_claim_id": last_settled_claim_id,
        "pending_batch_count": pending_batch_count,
        "anchor": anchor_kernel.as_ref().map(|a| json!({
            "tip_height": a.tip_height,
            "tip_digest": crate::state::hex_encode(&a.tip_digest),
        })),
        "follower": json!({
            "chain_tip_height":                follower.last_chain_tip_height,
            "anchor_lag_blocks":               anchor_lag_blocks,
            "is_caught_up":                    follower_is_caught_up,
            "last_advance_at_epoch_ms":        follower.last_advance_at_epoch_ms,
            "last_advance_age_seconds":        follower_age_seconds,
            "last_advance_tip_height":         follower.last_advance_tip_height,
            "last_advance_count":              follower.last_advance_count,
            "last_chain_tip_observed_at_epoch_ms": follower.last_chain_tip_observed_at_epoch_ms,
            "last_error":                      follower.last_error,
            "last_error_phase":                follower.last_error_phase,
            "last_error_at_epoch_ms":          follower.last_error_at_epoch_ms,
            "finality_depth":                  crate::chain_follower::DEFAULT_FINALITY_DEPTH,
            "max_advance_batch":               crate::chain_follower::DEFAULT_MAX_ADVANCE_BATCH,
        }),
    }))
}

/// `GET /anchor` — Phase 7.1 dedicated anchor surface.
///
/// Returns the kernel's authoritative anchor digest + height
/// alongside the follower's most recent chain-tip observation,
/// lag, and last advance timestamp. Wallets use this to seed the
/// `light_verify --chain-tip --chain-tip-digest` flags; operators
/// use it as the single-source for "is the follower stuck?".
///
/// Response shape (200 on success):
///
/// ```json
/// {
///   "anchor": { "tip_height": 120, "tip_digest": "0x..." },
///   "chain_tip_height": 130,
///   "anchor_lag_blocks": 10,
///   "is_caught_up": true,
///   "last_advance_at_epoch_ms": 1735689600000,
///   "last_error": null,
///   "finality_depth": 10
/// }
/// ```
///
/// Returns 503 when the kernel anchor peek fails (and no follower
/// telemetry is available either) — the operator needs to know the
/// service is effectively blind rather than getting a confusing
/// all-nulls 200.
async fn anchor_handler(
    State(state): State<SharedState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    let (follower, settlement_mode, chain_endpoint) = {
        let h = state.hull.lock().await;
        (
            h.follower.clone(),
            h.settlement.mode.to_string(),
            h.settlement.chain_endpoint.clone(),
        )
    };

    let anchor_kernel = {
        let mut k = state.kernel.lock().await;
        k.peek(build_anchor_peek())
            .await
            .ok()
            .and_then(|slab| decode_anchor(&slab).ok())
    };

    // If we have neither a kernel anchor nor a single chain-tip
    // observation, we're completely blind — 503 is more useful than
    // a 200 with all nulls.
    if anchor_kernel.is_none() && follower.last_chain_tip_height.is_none() {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorBody {
                error: "anchor peek failed and follower has no chain observations yet".into(),
            }),
        ));
    }

    let anchor_lag_blocks = match (
        follower.last_chain_tip_height,
        anchor_kernel.as_ref().map(|a| a.tip_height),
    ) {
        (Some(chain_tip), Some(anchor_tip)) => Some(chain_tip.saturating_sub(anchor_tip)),
        _ => None,
    };
    let is_caught_up = anchor_lag_blocks.map(|lag| {
        lag <= crate::chain_follower::DEFAULT_FINALITY_DEPTH + 1
    });

    Ok(Json(json!({
        "anchor": anchor_kernel.as_ref().map(|a| json!({
            "tip_height": a.tip_height,
            "tip_digest": crate::state::hex_encode(&a.tip_digest),
        })),
        "chain_tip_height":                    follower.last_chain_tip_height,
        "anchor_lag_blocks":                   anchor_lag_blocks,
        "is_caught_up":                        is_caught_up,
        "last_advance_at_epoch_ms":            follower.last_advance_at_epoch_ms,
        "last_advance_tip_height":             follower.last_advance_tip_height,
        "last_advance_count":                  follower.last_advance_count,
        "last_chain_tip_observed_at_epoch_ms": follower.last_chain_tip_observed_at_epoch_ms,
        "last_error":                          follower.last_error,
        "last_error_phase":                    follower.last_error_phase,
        "last_error_at_epoch_ms":              follower.last_error_at_epoch_ms,
        "finality_depth":                      crate::chain_follower::DEFAULT_FINALITY_DEPTH,
        "max_advance_batch":                   crate::chain_follower::DEFAULT_MAX_ADVANCE_BATCH,
        "settlement_mode":                     settlement_mode,
        "chain_endpoint":                      chain_endpoint,
    })))
}

/// `POST /admin/advance-tip-now` — Phase 7.1 manual anchor-advance
/// trigger for debugging.
///
/// Drives a single `advance_anchor_once` pass outside the regular
/// 10 s poll cycle. Useful for:
///
/// - verifying a freshly-configured chain endpoint works
/// - unsticking an anchor without waiting for the next tick
/// - exercising the anchor-advance path in tests
///
/// **Gated by `NNS_ENABLE_ADMIN=1`.** Admin routes are off by
/// default so a casually-deployed node doesn't expose the
/// mutation surface to the world. Returns 404 when disabled so
/// scanners can't fingerprint the presence of admin endpoints.
async fn admin_advance_tip_now(
    State(state): State<SharedState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    if !admin_enabled() {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: "not found".into(),
            }),
        ));
    }

    match crate::chain_follower::advance_anchor_once(&state).await {
        Ok(Some(advanced)) => Ok(Json(json!({
            "advanced": true,
            "tip_height": advanced.tip_height,
            "tip_digest": crate::state::hex_encode(&advanced.tip_digest),
            "count": advanced.count,
        }))),
        Ok(None) => Ok(Json(json!({
            "advanced": false,
            "reason": "no-op (local mode, endpoint missing, or within finality depth)",
        }))),
        Err(e) => Err(server_error(format!("advance-tip failed: {e}"))),
    }
}

fn admin_enabled() -> bool {
    matches!(
        std::env::var("NNS_ENABLE_ADMIN").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

async fn register_handler(
    State(state): State<SharedState>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    if !is_valid_address(&req.address) {
        return Err(bad_request("invalid address"));
    }
    if !is_valid_name(&req.name) {
        return Err(bad_request("invalid name"));
    }

    let name = req.name.trim().to_string();
    let address = req.address.trim().to_string();

    // Pending name reservations live entirely in the hull mirror — the
    // kernel only knows about claimed (registered) names. This keeps
    // the on-kernel state, and thus the Merkle root the graft commits
    // to, limited to the canonical registry.
    {
        let h = state.hull.lock().await;
        if let Some(existing) = h.mirror.names.get(&name) {
            match existing.status {
                RegistrationStatus::Registered => {
                    return Err(bad_request("Name already registered"));
                }
                RegistrationStatus::Pending => {
                    return Ok(Json(serde_json::to_value(existing.clone()).unwrap()));
                }
            }
        }
    }

    // Mirror can be stale (e.g. if cache was cleared). Ask the kernel
    // before creating a new pending reservation so register/claim stays
    // consistent with kernel authority.
    let owner_slab = {
        let mut k = state.kernel.lock().await;
        k.peek(build_owner_peek(&name))
            .await
            .map_err(|e| server_error(format!("owner peek failed: {e:?}")))?
    };
    if let Some(entry) =
        decode_owner(&owner_slab).map_err(|e| server_error(format!("owner decode failed: {e}")))?
    {
        let mut h = state.hull.lock().await;
        h.mirror.insert(Registration {
            address: entry.owner,
            name: name.clone(),
            status: RegistrationStatus::Registered,
            timestamp: now_millis(),
            date: None,
            tx_hash: Some(entry.tx_hash),
        });
        h.persist_mirror();
        return Err(bad_request("Name already registered"));
    }

    let now = now_millis();
    let reg = Registration {
        address: address.clone(),
        name: name.clone(),
        status: RegistrationStatus::Pending,
        timestamp: now,
        date: None,
        tx_hash: None,
    };
    {
        let mut h = state.hull.lock().await;
        h.mirror.insert(reg.clone());
        h.persist_mirror();
    }

    Ok(Json(json!({
        "address": reg.address,
        "name": reg.name,
        "status": "pending",
    })))
}

async fn claim_handler(
    State(state): State<SharedState>,
    Json(req): Json<ClaimRequest>,
) -> Result<impl IntoResponse, (StatusCode, Json<ErrorBody>)> {
    if !is_valid_address(&req.address) {
        return Err(bad_request("invalid address"));
    }
    if !is_valid_name(&req.name) {
        return Err(bad_request("invalid name"));
    }

    let name = req.name.trim().to_string();
    let address = req.address.trim().to_string();
    let tx_hash = req.tx_hash.as_deref().map(str::trim);

    let (_pending, settlement) = {
        let h = state.hull.lock().await;
        let pending = h
            .mirror
            .names
            .get(&name)
            .cloned()
            .ok_or_else(|| bad_request("no pending registration"))?;
        if pending.status != RegistrationStatus::Pending {
            return Err(bad_request("already registered"));
        }
        if pending.address != address {
            return Err(bad_request("address does not match pending registration"));
        }
        if !matches!(h.settlement.mode, vesl_core::SettlementMode::Local)
            && tx_hash.map_or(true, str::is_empty)
        {
            return Err(bad_request("missing txHash"));
        }
        (pending, h.settlement.clone())
    };

    let fee = payment::fee_for_name(&name);
    let tx_hash = payment::verify(&settlement, &address, &name, fee, tx_hash)
        .await
        .map_err(|e| bad_request(format!("no valid payment: {e}")))?;
    let note = ClaimNoteV1::new(name.clone(), address.clone(), tx_hash.clone());
    let _chain_submit = crate::chain::submit_claim_note(&settlement, &note)
        .await
        .map_err(|e| server_error(format!("claim note submit failed: {e}")))?;

    let is_local = matches!(settlement.mode, vesl_core::SettlementMode::Local);
    {
        let mut h = state.hull.lock().await;
        h.mirror.enqueue_claim(
            note.claim_id.clone(),
            address,
            name,
            fee,
            tx_hash.clone(),
        );
        h.persist_mirror();
    }

    if is_local {
        crate::chain_follower::process_once(&state)
            .await
            .map_err(|e| server_error(format!("local replay failed: {e}")))?;
    }

    let (status, reason_opt, registration) = {
        let h = state.hull.lock().await;
        let status = h
            .mirror
            .claim_status(&note.claim_id)
            .map(|s| s.status)
            .unwrap_or(ClaimLifecycleStatus::Submitted);
        let reason = if matches!(status, ClaimLifecycleStatus::Rejected) {
            h.mirror
                .claim_status(&note.claim_id)
                .and_then(|s| s.reason)
        } else {
            None
        };
        let registration = h.mirror.names.get(&note.name).cloned();
        (status, reason, registration)
    };

    if matches!(status, ClaimLifecycleStatus::Rejected) {
        let reason = reason_opt.unwrap_or_else(|| "claim rejected".into());
        if reason.contains("name already registered") {
            return Err(bad_request("Name already registered"));
        }
        if reason.contains("payment already used") {
            return Err(bad_request("Payment already consumed"));
        }
        return Err(bad_request(reason));
    }

    Ok(Json(ClaimSubmissionResponse {
        message: "Claim submitted; awaiting chain replay".into(),
        claim_id: note.claim_id,
        status,
        tx_hash: Some(tx_hash),
        registration,
    }))
}

async fn set_primary_handler(
    State(state): State<SharedState>,
    Json(req): Json<SetPrimaryRequest>,
) -> Result<Json<SetPrimaryResponse>, (StatusCode, Json<ErrorBody>)> {
    if !is_valid_address(&req.address) {
        return Err(bad_request("invalid address"));
    }
    if !is_valid_name(&req.name) {
        return Err(bad_request("invalid name"));
    }

    let name = req.name.trim().to_string();
    let address = req.address.trim().to_string();

    // The kernel is the source of truth for ownership. We don't
    // short-circuit on mirror state — if the mirror is stale we'd
    // rather let the kernel decide and trust its %primary-error.
    let effects = poke_with_timeout(&state.kernel, build_set_primary_poke(&address, &name))
        .await
        .map_err(|msg| server_error(format!("kernel set-primary poke failed: {msg}")))?;

    if let Some(err) = first_error_message(&effects) {
        // Both "name not registered" and "not the owner" are
        // user-visible 400s. A kernel wedge producing anything else
        // still lands here so we don't 500 on a duplicate shape.
        return Err(bad_request(err));
    }

    let (ok_addr, ok_name) = first_primary_set(&effects).ok_or_else(|| {
        server_error(format!(
            "set-primary returned no %primary-set effect ({} effects)",
            effects.len()
        ))
    })?;

    {
        let mut h = state.hull.lock().await;
        h.mirror.set_primary(ok_addr.clone(), ok_name.clone());
    }
    state.persist_all().await;

    Ok(Json(SetPrimaryResponse {
        address: ok_addr,
        name: ok_name,
    }))
}

async fn claim_status_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ClaimStatusResponse>, (StatusCode, Json<ErrorBody>)> {
    let claim_id = params
        .get("claim_id")
        .or_else(|| params.get("claimId"))
        .ok_or_else(|| bad_request("missing claim_id parameter"))?
        .trim()
        .to_string();
    if claim_id.is_empty() {
        return Err(bad_request("missing claim_id parameter"));
    }
    let h = state.hull.lock().await;
    let status = h.mirror.claim_status(&claim_id).ok_or_else(|| {
        (
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: "unknown claim_id".into(),
            }),
        )
    })?;
    Ok(Json(status))
}

async fn snapshot_handler(
    State(state): State<SharedState>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    let peek_slab = build_snapshot_peek();
    let result = {
        let mut k = state.kernel.lock().await;
        k.peek(peek_slab)
            .await
            .map_err(|e| format!("{e:?}"))
            .and_then(|slab| decode_snapshot(&slab))
    };

    let snap = match result {
        Ok(s) => s,
        Err(_) => {
            let h = state.hull.lock().await;
            return match h.mirror.snapshot.clone() {
                Some(cached) => Ok(Json(serde_json::to_value(cached).unwrap())),
                None => Err((
                    StatusCode::NOT_FOUND,
                    Json(ErrorBody {
                        error: "no commitment yet — registry is empty".into(),
                    }),
                )),
            };
        }
    };

    // Empty registry — the kernel's state default is
    // `[claim-id=0 root=0 hull=0]`. Return 404 rather than emit a
    // bogus zero-commitment: there's nothing to settle against.
    if snap.claim_id == 0 {
        return Err((
            StatusCode::NOT_FOUND,
            Json(ErrorBody {
                error: "no commitment yet — registry is empty".into(),
            }),
        ));
    }

    {
        let mut h = state.hull.lock().await;
        h.mirror.set_snapshot(snap.claim_id, &snap.hull, &snap.root);
        h.persist_mirror();
    }

    Ok(Json(json!({
        "claim_id": snap.claim_id,
        "hull": hex_encode(&snap.hull),
        "root": hex_encode(&snap.root),
    })))
}

async fn settle_handler(
    State(state): State<SharedState>,
) -> Result<Json<SettleResponse>, (StatusCode, Json<ErrorBody>)> {
    let (names, effects) = {
        let mut k = state.kernel.lock().await;
        let pending_slab = k
            .peek(build_pending_batch_peek())
            .await
            .map_err(|e| server_error(format!("pending-batch peek failed: {e:?}")))?;
        let names = decode_pending_batch(&pending_slab)
            .map_err(|e| server_error(format!("pending-batch decode failed: {e}")))?;
        let effects = match timeout(POKE_TIMEOUT, k.poke(SystemWire.to_wire(), build_settle_batch_poke()))
            .await
        {
            Ok(Ok(effects)) => effects,
            Ok(Err(e)) => {
                return Err(server_error(format!("kernel settle-batch poke failed: {e:?}")));
            }
            Err(_) => {
                return Err(server_error(format!(
                    "kernel poke exceeded {}s timeout",
                    POKE_TIMEOUT.as_secs()
                )));
            }
        };
        (names, effects)
    };

    if let Some(err) = first_error_message(&effects) {
        // `nothing to settle` (empty window), `note already settled`
        // (exact same batch resubmitted) and any graft-level
        // rejection are all client-visible — surface as 400.
        return Err(bad_request(err));
    }

    let batch = first_batch_settled(&effects).ok_or_else(|| {
        let tags: Vec<String> = effects
            .iter()
            .map(|e| {
                crate::kernel::effect_tag(e).unwrap_or_else(|| "<untagged>".to_string())
            })
            .collect();
        server_error(format!(
            "settle returned no %batch-settled effect (tags: {tags:?})"
        ))
    })?;
    let settled = first_vesl_settled(&effects).ok_or_else(|| {
        server_error("settle returned %batch-settled without %vesl-settled")
    })?;

    let note_id_hex = hex_encode(&batch.note_id);
    let settlement_for_post = {
        let mut h = state.hull.lock().await;
        let settlement_for_post = h.settlement.clone();
        h.mirror.set_last_settled_claim_id(batch.claim_count);
        h.mirror
            .set_snapshot(batch.claim_count, &settled.hull, &settled.root);
        settlement_for_post
    };
    let settlement_tx =
        match crate::chain::post_settlement_receipt(&settlement_for_post, &note_id_hex).await {
            Ok(tx) => tx,
            Err(e) => {
                tracing::warn!("settlement chain post skipped: {e}");
                None
            }
        };
    state.persist_all().await;

    Ok(Json(SettleResponse {
        claim_id: batch.claim_count,
        count: batch.count,
        names,
        hull: hex_encode(&settled.hull),
        root: hex_encode(&settled.root),
        note_id: note_id_hex,
        settlement_tx,
    }))
}

async fn pending_batch_handler(
    State(state): State<SharedState>,
) -> Result<Json<PendingBatchResponse>, (StatusCode, Json<ErrorBody>)> {
    let (names, claim_id, last_settled_claim_id) = {
        let mut k = state.kernel.lock().await;
        let pending_slab = k
            .peek(build_pending_batch_peek())
            .await
            .map_err(|e| server_error(format!("pending-batch peek failed: {e:?}")))?;
        let names = decode_pending_batch(&pending_slab)
            .map_err(|e| server_error(format!("pending-batch decode failed: {e}")))?;

        let snap_slab = k
            .peek(build_snapshot_peek())
            .await
            .map_err(|e| server_error(format!("snapshot peek failed: {e:?}")))?;
        let claim_id = decode_snapshot(&snap_slab).map(|s| s.claim_id).unwrap_or(0);

        let last_slab = k
            .peek(build_last_settled_peek())
            .await
            .map_err(|e| server_error(format!("last-settled peek failed: {e:?}")))?;
        let last_settled_claim_id = decode_last_settled(&last_slab)
            .map_err(|e| server_error(format!("last-settled decode failed: {e}")))?;
        (names, claim_id, last_settled_claim_id)
    };

    Ok(Json(PendingBatchResponse {
        claim_id,
        last_settled_claim_id,
        count: names.len() as u64,
        names,
    }))
}

async fn pending_handler(State(state): State<SharedState>) -> Json<Vec<Registration>> {
    let h = state.hull.lock().await;
    Json(h.mirror.by_status(RegistrationStatus::Pending))
}

async fn verified_handler(State(state): State<SharedState>) -> Json<Vec<Registration>> {
    let h = state.hull.lock().await;
    Json(h.mirror.by_status(RegistrationStatus::Registered))
}

async fn resolve_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    if let Some(name) = params.get("name") {
        if !is_valid_name(name) {
            return Err(bad_request("invalid name"));
        }
        let mirror_hit = {
            let h = state.hull.lock().await;
            h.mirror
                .names
                .get(name)
                .filter(|r| r.status == RegistrationStatus::Registered)
                .map(|r| r.address.clone())
        };
        match mirror_hit {
            Some(addr) => Ok(Json(json!({ "address": addr }))),
            None => {
                let owner_slab = {
                    let mut k = state.kernel.lock().await;
                    k.peek(build_owner_peek(name))
                        .await
                        .map_err(|e| server_error(format!("owner peek failed: {e:?}")))?
                };
                let entry = decode_owner(&owner_slab)
                    .map_err(|e| server_error(format!("owner decode failed: {e}")))?;
                match entry {
                    Some(entry) => {
                        let mut h = state.hull.lock().await;
                        h.mirror.insert(Registration {
                            address: entry.owner.clone(),
                            name: name.clone(),
                            status: RegistrationStatus::Registered,
                            timestamp: now_millis(),
                            date: None,
                            tx_hash: Some(entry.tx_hash),
                        });
                        h.persist_mirror();
                        Ok(Json(json!({ "address": entry.owner })))
                    }
                    None => Err((
                        StatusCode::NOT_FOUND,
                        Json(ErrorBody {
                            error: "not found".into(),
                        }),
                    )),
                }
            }
        }
    } else if let Some(address) = params.get("address") {
        if !is_valid_address(address) {
            return Err(bad_request("invalid address"));
        }
        let h = state.hull.lock().await;
        match h.mirror.primaries.get(address) {
            Some(name) => Ok(Json(json!({ "name": name }))),
            None => Err((
                StatusCode::NOT_FOUND,
                Json(ErrorBody { error: "not found".into() }),
            )),
        }
    } else {
        Err(bad_request("missing name or address parameter"))
    }
}

/// `GET /proof?name=<name>[&address=<addr>]`
///
/// Returns a Merkle inclusion bundle that lets a client verify
/// off-server that `(name, owner, tx-hash)` is a row in the kernel
/// registry at the current commitment `root`.
///
/// Response body (see `ProofResponse`):
///
/// ```json
/// {
///   "name": "foo.nock",
///   "owner": "<address>",
///   "txHash": "<tx>",
///   "claim_id": 7,
///   "root": "<hex>",
///   "hull": "<hex>",
///   "proof": [ { "hash": "<hex>", "side": "left" | "right" }, ... ]
/// }
/// ```
///
/// Verification recipe (same check `nns-gate` G2 performs inside
/// the STARK, but done client-side here):
///
///   1. `chunk = jam([name owner tx-hash])`.
///   2. Walk `proof` with `hash-leaf` / `hash-pair` (tip5).
///   3. Accept iff the result equals `root`.
///
/// If `address` is supplied and doesn't match the stored owner we
/// return 404 ("address does not own this name") — same status as
/// an unregistered name, so the endpoint doesn't leak ownership
/// data to a caller who didn't know the right pair up front.
///
/// 404s on any kernel peek-miss (unregistered name, empty registry).
async fn proof_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<ProofResponse>, (StatusCode, Json<ErrorBody>)> {
    let name = params
        .get("name")
        .ok_or_else(|| bad_request("missing name parameter"))?
        .trim()
        .to_string();
    if !is_valid_name(&name) {
        return Err(bad_request("invalid name"));
    }
    let expected_address = match params.get("address") {
        Some(a) if !a.is_empty() => {
            if !is_valid_address(a) {
                return Err(bad_request("invalid address"));
            }
            Some(a.trim().to_string())
        }
        _ => None,
    };

    let (entry, proof, snap_from_kernel, anchor) = {
        let mut k = state.kernel.lock().await;
        let owner_slab = k
            .peek(build_owner_peek(&name))
            .await
            .map_err(|e| server_error(format!("owner peek failed: {e:?}")))?;
        let entry = decode_owner(&owner_slab)
            .map_err(|e| server_error(format!("owner decode failed: {e}")))?
            .ok_or_else(|| {
                (
                    StatusCode::NOT_FOUND,
                    Json(ErrorBody {
                        error: "name not registered".into(),
                    }),
                )
            })?;

        if let Some(ref addr) = expected_address {
            if &entry.owner != addr {
                return Err((
                    StatusCode::NOT_FOUND,
                    Json(ErrorBody {
                        error: "address does not own this name".into(),
                    }),
                ));
            }
        }

        let proof_slab = k
            .peek(build_proof_peek(&name))
            .await
            .map_err(|e| server_error(format!("proof peek failed: {e:?}")))?;
        let proof = decode_proof(&proof_slab)
            .map_err(|e| server_error(format!("proof decode failed: {e}")))?;

        let snap_from_kernel = k
            .peek(build_snapshot_peek())
            .await
            .map_err(|e| format!("{e:?}"))
            .and_then(|slab| decode_snapshot(&slab))
            .map(|s| (s.claim_id, hex_encode(&s.hull), hex_encode(&s.root)));

        let anchor = match k
            .peek(build_anchor_peek())
            .await
            .map_err(|e| format!("{e:?}"))
            .and_then(|slab| decode_anchor(&slab))
        {
            Ok(view) => Some(ProofAnchor {
                tip_digest: hex_encode(&view.tip_digest),
                tip_height: view.tip_height,
            }),
            Err(_) => None,
        };

        (entry, proof, snap_from_kernel, anchor)
    };

    let snap = match snap_from_kernel {
        Ok(triple) => triple,
        Err(peek_err) => {
            let h = state.hull.lock().await;
            match h.mirror.snapshot.clone() {
                Some(cached) => (cached.claim_id, cached.hull, cached.root),
                None => {
                    return Err(server_error(format!(
                        "snapshot peek failed and mirror has no cached snapshot: {peek_err}"
                    )));
                }
            }
        }
    };

    let last_settled_claim_id = state.hull.lock().await.mirror.last_settled_claim_id;

    let (_snap_claim_id, snap_hull_hex, snap_root_hex) = snap;

    Ok(Json(ProofResponse {
        name,
        owner: entry.owner,
        tx_hash: entry.tx_hash,
        claim_id: entry.claim_count,
        root: snap_root_hex,
        hull: snap_hull_hex,
        proof: proof
            .into_iter()
            .map(|p| ProofNodeView {
                hash: hex_encode(&p.hash),
                side: if p.side {
                    ProofSide::Left
                } else {
                    ProofSide::Right
                },
            })
            .collect(),
        transition: TransitionProofMetadata {
            mode: "claim-window-anchor".into(),
            settled_claim_id: last_settled_claim_id,
        },
        transition_proof: None,
        anchor,
    }))
}

async fn search_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorBody>)> {
    let h = state.hull.lock().await;

    if let Some(address) = params.get("address") {
        if !is_valid_address(address) {
            return Err(bad_request("invalid address"));
        }
        let pending: Vec<Registration> = h
            .mirror
            .by_status(RegistrationStatus::Pending)
            .into_iter()
            .filter(|r| r.address == *address)
            .collect();
        let verified: Vec<Registration> = h
            .mirror
            .by_status(RegistrationStatus::Registered)
            .into_iter()
            .filter(|r| r.address == *address)
            .collect();
        let primary = h.mirror.primaries.get(address).cloned();
        let body = SearchByAddressResponse {
            address: address.clone(),
            pending,
            verified,
            primary,
        };
        Ok(Json(body).into_response())
    } else if let Some(label) = params.get("name") {
        // Label is the stem without `.nock`.
        let name = format!("{label}.nock");
        if !is_valid_name(&name) {
            return Err(bad_request("invalid name"));
        }
        let price = payment::fee_for_name(&name);
        let existing = h.mirror.names.get(&name).cloned();
        let body = match existing {
            None => SearchByNameResponse {
                name,
                price,
                status: SearchStatus::Available,
                owner: None,
                registered_at: None,
            },
            Some(r) => SearchByNameResponse {
                name: r.name,
                price,
                status: match r.status {
                    RegistrationStatus::Pending => SearchStatus::Pending,
                    RegistrationStatus::Registered => SearchStatus::Registered,
                },
                owner: Some(r.address),
                registered_at: Some(r.timestamp),
            },
        };
        Ok(Json(body).into_response())
    } else {
        Err(bad_request("missing name or address parameter"))
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn iso8601(unix_millis: u64) -> String {
    // Minimal RFC3339 without deps: UTC, seconds precision.
    // Good enough for the legacy `date` field which was a free-form
    // string set via `new Date().toISOString()`.
    let secs = unix_millis / 1000;
    let ms = unix_millis % 1000;
    let (year, month, day, hh, mm, ss) = unix_seconds_to_ymdhms(secs as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}.{ms:03}Z"
    )
}

pub(crate) fn iso8601_for_internal(unix_millis: u64) -> String {
    iso8601(unix_millis)
}

fn unix_seconds_to_ymdhms(secs: i64) -> (i32, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400) as u32;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let yy = if m <= 2 { y + 1 } else { y } as i32;
    let _ = z;
    let _ = rem;
    (yy, m, d, hh, mm, ss)
}

pub async fn serve(
    state: SharedState,
    port: u16,
    bind_addr: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let app = router(state);
    let addr = format!("{bind_addr}:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("listening on http://{addr}");
    println!("Listening on http://{addr}");
    println!("  POST /register");
    println!("  POST /claim");
    println!("  GET  /claim-status?claim_id=");
    println!("  POST /primary");
    println!("  POST /settle");
    println!("  GET  /snapshot");
    println!("  GET  /pending-batch");
    println!("  GET  /pending");
    println!("  GET  /verified");
    println!("  GET  /resolve?name=|address=");
    println!("  GET  /proof?name=[&address=]");
    println!("  GET  /search?name=|address=");
    println!("  GET  /status");
    println!("  GET  /health");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_validation_matches_legacy() {
        assert!(is_valid_name("foo.nock"));
        assert!(is_valid_name("abc123.nock"));
        assert!(!is_valid_name("foo"));
        assert!(!is_valid_name("Foo.nock"));
        assert!(!is_valid_name(".nock"));
        assert!(!is_valid_name("foo.bar"));
    }

    #[test]
    fn address_length_44_to_56_accepted() {
        let a = "x".repeat(44);
        assert!(is_valid_address(&a));
        let a = "x".repeat(56);
        assert!(is_valid_address(&a));
        let a = "x".repeat(43);
        assert!(!is_valid_address(&a));
        let a = "x".repeat(57);
        assert!(!is_valid_address(&a));
    }

    #[test]
    fn address_length_132_requires_alnum() {
        let a = "a".repeat(132);
        assert!(is_valid_address(&a));
        let mut a: String = "a".repeat(131);
        a.push('!');
        assert!(!is_valid_address(&a));
    }

    #[test]
    fn iso_roundtrip_epoch() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00.000Z");
    }
}
