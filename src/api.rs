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
use nockapp::wire::{SystemWire, Wire};
use serde::Serialize;
use serde_json::json;
use tokio::time::timeout;
use tower_http::cors::{Any, CorsLayer};

const POKE_TIMEOUT: Duration = Duration::from_secs(30);

async fn poke_with_timeout(
    app: &mut nockapp::NockApp,
    slab: NounSlab,
) -> Result<Vec<NounSlab>, String> {
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
    build_proof_peek, build_set_primary_poke, build_settle_batch_poke, build_snapshot_peek,
    decode_last_settled, decode_owner, decode_pending_batch, decode_proof, decode_snapshot,
    first_batch_settled, first_error_message, first_primary_set, first_vesl_settled,
};
use crate::claim_note::ClaimNoteV1;
use crate::payment;
use crate::state::{hex_encode, SharedState};
use crate::types::{
    ClaimRequest, ClaimStatusResponse, ClaimSubmissionResponse, ClaimLifecycleStatus, PendingBatchResponse, ProofNodeView, ProofResponse, ProofSide, TransitionProofMetadata,
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
    let st = state.lock().await;
    // `pending_batch_count` is the size of the settlement pending
    // window — what /pending-batch would return — and is distinct
    // from `pending_count` (pending *reservations* that haven't been
    // %claim'd yet). Both are useful surfaces.
    let claim_id = st.mirror.snapshot.as_ref().map(|s| s.claim_id).unwrap_or(0);
    let pending_batch_count = claim_id.saturating_sub(st.mirror.last_settled_claim_id);
    Json(json!({
        "settlement_mode": st.settlement.mode.to_string(),
        "names_count": st.mirror.names.len(),
        "pending_count": st.mirror.by_status(RegistrationStatus::Pending).len(),
        "registered_count": st.mirror.by_status(RegistrationStatus::Registered).len(),
        "snapshot": st.mirror.snapshot,
        "last_settled_claim_id": st.mirror.last_settled_claim_id,
        "pending_batch_count": pending_batch_count,
    }))
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

    let mut st = state.lock().await;

    // Pending name reservations live entirely in the hull mirror — the
    // kernel only knows about claimed (registered) names. This keeps
    // the on-kernel state, and thus the Merkle root the graft commits
    // to, limited to the canonical registry.
    if let Some(existing) = st.mirror.names.get(&name) {
        match existing.status {
            RegistrationStatus::Registered => {
                return Err(bad_request("Name already registered"));
            }
            RegistrationStatus::Pending => {
                // Legacy worker returns the full pending object with 200.
                return Ok(Json(serde_json::to_value(existing.clone()).unwrap()));
            }
        }
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
    st.mirror.insert(reg.clone());
    st.persist();

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

    let mut st = state.lock().await;

    let pending = st
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

    let fee = payment::fee_for_name(&name);
    let tx_hash = payment::verify(&st.settlement, &address, &name, fee, req.tx_hash.as_deref())
        .await
        .map_err(|e| bad_request(format!("no valid payment: {e}")))?;
    let note = ClaimNoteV1::new(name.clone(), address.clone(), tx_hash.clone());
    let _chain_submit = crate::chain::submit_claim_note(&st.settlement, &note)
        .await
        .map_err(|e| server_error(format!("claim note submit failed: {e}")))?;
    st.mirror.enqueue_claim(
        note.claim_id.clone(),
        address,
        name,
        fee,
        tx_hash.clone(),
    );
    st.persist();

    if matches!(st.settlement.mode, vesl_core::SettlementMode::Local) {
        drop(st);
        crate::chain_follower::process_once(&state)
            .await
            .map_err(|e| server_error(format!("local replay failed: {e}")))?;
        st = state.lock().await;
    }

    let status = st
        .mirror
        .claim_status(&note.claim_id)
        .map(|s| s.status)
        .unwrap_or(ClaimLifecycleStatus::Submitted);
    if matches!(status, ClaimLifecycleStatus::Rejected) {
        let reason = st
            .mirror
            .claim_status(&note.claim_id)
            .and_then(|s| s.reason)
            .unwrap_or_else(|| "claim rejected".into());
        if reason.contains("name already registered") {
            return Err(bad_request("Name already registered"));
        }
        if reason.contains("payment already used") {
            return Err(bad_request("Payment already consumed"));
        }
        return Err(bad_request(reason));
    }
    let registration = st.mirror.names.get(&note.name).cloned();

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

    let mut st = state.lock().await;

    // The kernel is the source of truth for ownership. We don't
    // short-circuit on mirror state — if the mirror is stale we'd
    // rather let the kernel decide and trust its %primary-error.
    let effects = poke_with_timeout(&mut st.app, build_set_primary_poke(&address, &name))
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

    st.mirror.set_primary(ok_addr.clone(), ok_name.clone());
    st.persist_all().await;

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
    let st = state.lock().await;
    let status = st.mirror.claim_status(&claim_id).ok_or_else(|| {
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
    let mut st = state.lock().await;

    // Try the peek-backed authoritative path first so the response
    // is never stale even if the mirror cache was wiped. On a kernel
    // error fall back to the cached mirror snapshot — this endpoint
    // is diagnostic and should not 500 on transient peek failures.
    let peek_slab = build_snapshot_peek();
    let result = st
        .app
        .peek(peek_slab)
        .await
        .map_err(|e| format!("{e:?}"))
        .and_then(|slab| decode_snapshot(&slab));

    let snap = match result {
        Ok(s) => s,
        Err(_) => {
            return match st.mirror.snapshot.clone() {
                Some(cached) => Ok(Json(serde_json::to_value(cached).unwrap())),
                None => Err((
                    StatusCode::NOT_FOUND,
                    Json(ErrorBody {
                        error: "no commitment yet — registry is empty".into(),
                    }),
                )),
            }
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

    // Refresh the mirror cache opportunistically so subsequent
    // /status calls don't peek.
    st.mirror.set_snapshot(snap.claim_id, &snap.hull, &snap.root);
    st.persist();

    Ok(Json(json!({
        "claim_id": snap.claim_id,
        "hull": hex_encode(&snap.hull),
        "root": hex_encode(&snap.root),
    })))
}

async fn settle_handler(
    State(state): State<SharedState>,
) -> Result<Json<SettleResponse>, (StatusCode, Json<ErrorBody>)> {
    let mut st = state.lock().await;

    // Peek the pending batch first — the kernel-side `%settle-batch`
    // arm walks the same window internally, but we snapshot it here
    // so the HTTP response can tell the client exactly which names
    // were packaged. Doing the peek before the poke is safe: nothing
    // else mutates the kernel while the lock is held.
    let pending_slab = st
        .app
        .peek(build_pending_batch_peek())
        .await
        .map_err(|e| server_error(format!("pending-batch peek failed: {e:?}")))?;
    let names = decode_pending_batch(&pending_slab)
        .map_err(|e| server_error(format!("pending-batch decode failed: {e}")))?;

    // Dispatch the single %settle-batch poke. The kernel handles
    // batch selection, proof generation, note-id derivation, and
    // graft dispatch in one atomic step.
    let effects = poke_with_timeout(&mut st.app, build_settle_batch_poke())
        .await
        .map_err(|msg| server_error(format!("kernel settle-batch poke failed: {msg}")))?;

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

    // Advance the mirror's last-settled cache. The kernel already
    // bumped its own counter — this is just a fast-read cache for
    // /status and /pending-batch.
    st.mirror.set_last_settled_claim_id(batch.claim_id);
    st.mirror
        .set_snapshot(batch.claim_id, &settled.hull, &settled.root);
    let note_id_hex = hex_encode(&batch.note_id);
    let settlement_tx = match crate::chain::post_settlement_receipt(&st.settlement, &note_id_hex).await {
        Ok(tx) => tx,
        Err(e) => {
            tracing::warn!("settlement chain post skipped: {e}");
            None
        }
    };
    st.persist_all().await;

    Ok(Json(SettleResponse {
        claim_id: batch.claim_id,
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
    let mut st = state.lock().await;

    let pending_slab = st
        .app
        .peek(build_pending_batch_peek())
        .await
        .map_err(|e| server_error(format!("pending-batch peek failed: {e:?}")))?;
    let names = decode_pending_batch(&pending_slab)
        .map_err(|e| server_error(format!("pending-batch decode failed: {e}")))?;

    let snap_slab = st
        .app
        .peek(build_snapshot_peek())
        .await
        .map_err(|e| server_error(format!("snapshot peek failed: {e:?}")))?;
    let claim_id = decode_snapshot(&snap_slab).map(|s| s.claim_id).unwrap_or(0);

    let last_slab = st
        .app
        .peek(build_last_settled_peek())
        .await
        .map_err(|e| server_error(format!("last-settled peek failed: {e:?}")))?;
    let last_settled_claim_id = decode_last_settled(&last_slab)
        .map_err(|e| server_error(format!("last-settled decode failed: {e}")))?;

    Ok(Json(PendingBatchResponse {
        claim_id,
        last_settled_claim_id,
        count: names.len() as u64,
        names,
    }))
}

async fn pending_handler(State(state): State<SharedState>) -> Json<Vec<Registration>> {
    let st = state.lock().await;
    Json(st.mirror.by_status(RegistrationStatus::Pending))
}

async fn verified_handler(State(state): State<SharedState>) -> Json<Vec<Registration>> {
    let st = state.lock().await;
    Json(st.mirror.by_status(RegistrationStatus::Registered))
}

async fn resolve_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, (StatusCode, Json<ErrorBody>)> {
    let st = state.lock().await;

    if let Some(name) = params.get("name") {
        if !is_valid_name(name) {
            return Err(bad_request("invalid name"));
        }
        let existing = st
            .mirror
            .names
            .get(name)
            .filter(|r| r.status == RegistrationStatus::Registered);
        match existing {
            Some(r) => Ok(Json(json!({ "address": r.address }))),
            None => Err((
                StatusCode::NOT_FOUND,
                Json(ErrorBody { error: "not found".into() }),
            )),
        }
    } else if let Some(address) = params.get("address") {
        if !is_valid_address(address) {
            return Err(bad_request("invalid address"));
        }
        // One address may own many names — return its designated
        // primary, not "whichever was registered last". Populated
        // from kernel %primary-set effects.
        match st.mirror.primaries.get(address) {
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

    let mut st = state.lock().await;

    // Three peeks under one mutex guard — nothing else can mutate
    // the kernel in between, so `entry`, `proof`, and `snapshot` all
    // observe the same `claim-id`.
    let owner_slab = st
        .app
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

    let proof_slab = st
        .app
        .peek(build_proof_peek(&name))
        .await
        .map_err(|e| server_error(format!("proof peek failed: {e:?}")))?;
    let proof = decode_proof(&proof_slab)
        .map_err(|e| server_error(format!("proof decode failed: {e}")))?;

    let snap_slab = st
        .app
        .peek(build_snapshot_peek())
        .await
        .map_err(|e| server_error(format!("snapshot peek failed: {e:?}")))?;
    let snap = decode_snapshot(&snap_slab)
        .map_err(|e| server_error(format!("snapshot decode failed: {e}")))?;

    Ok(Json(ProofResponse {
        name,
        owner: entry.owner,
        tx_hash: entry.tx_hash,
        claim_id: entry.claim_id,
        root: hex_encode(&snap.root),
        hull: hex_encode(&snap.hull),
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
            settled_claim_id: st.mirror.last_settled_claim_id,
        },
        transition_proof: None,
    }))
}

async fn search_handler(
    State(state): State<SharedState>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<axum::response::Response, (StatusCode, Json<ErrorBody>)> {
    let st = state.lock().await;

    if let Some(address) = params.get("address") {
        if !is_valid_address(address) {
            return Err(bad_request("invalid address"));
        }
        let pending: Vec<Registration> = st
            .mirror
            .by_status(RegistrationStatus::Pending)
            .into_iter()
            .filter(|r| r.address == *address)
            .collect();
        let verified: Vec<Registration> = st
            .mirror
            .by_status(RegistrationStatus::Registered)
            .into_iter()
            .filter(|r| r.address == *address)
            .collect();
        let primary = st.mirror.primaries.get(address).cloned();
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
        let existing = st.mirror.names.get(&name).cloned();
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
