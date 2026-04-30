//! HTTP API.
//!
//! Path Y makes NNS a read-only scanner/indexer. Users submit tagged
//! `nns/v1/claim` transactions directly to Nockchain; this service follows
//! finalized blocks and serves accumulator state.
//!
//! Exposed routes:
//!
//!   GET /health
//!   GET /status
//!   GET /accumulator/:name — registered names only (`404` if absent);
//!     `?wallet_export=true` adds `accumulator_snapshot_hex`
//!
//! CORS is open (`*`) to match legacy behavior.

use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Path, Query, State};
use axum::http::{header, Method, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::net::TcpListener;
use tower_http::cors::{Any, CorsLayer};

use crate::kernel::{
    build_accumulator_jam_peek, build_accumulator_peek, build_accumulator_proof_peek,
    build_scan_state_peek, decode_accumulator_entry, decode_accumulator_jam,
    decode_accumulator_proof_axis, decode_scan_state,
};
use crate::state::{hex_encode, SharedState};
use crate::types::{AccumulatorLookupResponse, AccumulatorValueResponse};

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

pub fn is_valid_address(address: &str) -> bool {
    let a = address.trim();
    let len = a.len();
    if len > 43 && len < 57 {
        return true;
    }
    len == 132 && a.chars().all(|c| c.is_ascii_alphanumeric())
}

pub fn is_valid_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".nock") else {
        return false;
    };
    !stem.is_empty()
        && stem
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

#[derive(Debug, Deserialize)]
struct AccumulatorQuery {
    #[serde(default)]
    wallet_export: bool,
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
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

fn not_found(msg: impl Into<String>) -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::NOT_FOUND,
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
        .allow_methods([Method::GET, Method::OPTIONS])
        .allow_headers([header::CONTENT_TYPE]);

    Router::new()
        .route("/health", get(health))
        .route("/status", get(status))
        .route("/accumulator/:name", get(accumulator_handler))
        .layer(cors)
        .with_state(state)
}

pub async fn serve(
    state: SharedState,
    port: u16,
    bind: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let addr = format!("{bind}:{port}");
    let listener = TcpListener::bind(&addr).await?;
    println!("Listening on http://{addr}");
    println!("  GET /health");
    println!("  GET /status");
    println!("  GET /accumulator/:name (?wallet_export=1)");
    axum::serve(listener, router(state)).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok" }))
}

async fn accumulator_handler(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Query(q): Query<AccumulatorQuery>,
) -> Result<Json<AccumulatorLookupResponse>, (StatusCode, Json<ErrorBody>)> {
    if !is_valid_name(&name) {
        return Err(bad_request("invalid name"));
    }

    let mut k = state.kernel.lock().await;
    let entry = k
        .peek(build_accumulator_peek(&name))
        .await
        .map_err(|e| server_error(format!("kernel accumulator peek failed: {e:?}")))
        .and_then(|slab| {
            decode_accumulator_entry(&slab)
                .map_err(|e| server_error(format!("decode accumulator failed: {e}")))
        })?;

    let Some(entry) = entry else {
        return Err(not_found("not registered"));
    };

    let (proof_axis, scan_state, accumulator_snapshot_hex) = {
        let proof_axis = k
            .peek(build_accumulator_proof_peek(&name))
            .await
            .map_err(|e| server_error(format!("kernel accumulator-proof peek failed: {e:?}")))
            .and_then(|slab| {
                decode_accumulator_proof_axis(&slab)
                    .map_err(|e| server_error(format!("decode accumulator-proof failed: {e}")))
            })?;
        let scan_state = k
            .peek(build_scan_state_peek())
            .await
            .map_err(|e| server_error(format!("kernel scan-state peek failed: {e:?}")))
            .and_then(|slab| {
                decode_scan_state(&slab)
                    .map_err(|e| server_error(format!("decode scan-state failed: {e}")))
            })?;
        let snap = if q.wallet_export {
            let jam = k
                .peek(build_accumulator_jam_peek())
                .await
                .map_err(|e| server_error(format!("kernel accumulator-jam peek failed: {e:?}")))
                .and_then(|slab| {
                    decode_accumulator_jam(&slab)
                        .map_err(|e| server_error(format!("decode accumulator-jam failed: {e}")))
                })?;
            Some(hex_encode(&jam))
        } else {
            None
        };
        (proof_axis, scan_state, snap)
    };
    drop(k);

    let value = Some(AccumulatorValueResponse {
        owner: entry.owner,
        tx_hash: hex_encode(&entry.tx_hash),
        claim_height: entry.claim_height,
        block_digest: hex_encode(&entry.block_digest),
    });

    Ok(Json(AccumulatorLookupResponse {
        name,
        value,
        proof_axis: proof_axis.map(|axis| hex_encode(&axis)),
        accumulator_snapshot_hex,
        last_proved_height: scan_state.last_proved_height,
        last_proved_digest: hex_encode(&scan_state.last_proved_digest),
        accumulator_root: hex_encode(&scan_state.accumulator_root),
        accumulator_size: scan_state.accumulator_size,
    }))
}

async fn status(State(state): State<SharedState>) -> Json<serde_json::Value> {
    let (settlement_mode, chain_endpoint, follower) = {
        let h = state.hull.lock().await;
        (
            h.settlement.mode.to_string(),
            h.settlement.chain_endpoint.clone(),
            h.follower.clone(),
        )
    };

    let scan_state = {
        let mut k = state.kernel.lock().await;
        k.peek(build_scan_state_peek())
            .await
            .ok()
            .and_then(|slab| decode_scan_state(&slab).ok())
    };

    let (anchor_lag_blocks, follower_is_caught_up) = match (
        follower.last_chain_tip_height,
        scan_state.as_ref().map(|s| s.last_proved_height),
    ) {
        (Some(chain_tip), Some(scan_tip)) => {
            let lag = chain_tip.saturating_sub(scan_tip);
            let caught_up = lag <= crate::chain_follower::DEFAULT_FINALITY_DEPTH + 1;
            (Some(lag), Some(caught_up))
        }
        _ => (None, None),
    };

    let follower_age_seconds = follower
        .last_advance_at_epoch_ms
        .map(|t| now_millis().saturating_sub(t) / 1000);

    Json(json!({
        "settlement_mode": settlement_mode,
        "chain_endpoint": chain_endpoint,
        "anchor": scan_state.as_ref().map(|s| json!({
            "tip_height": s.last_proved_height,
            "tip_digest": hex_encode(&s.last_proved_digest),
        })),
        "scan_state": scan_state.as_ref().map(|s| json!({
            "last_proved_height": s.last_proved_height,
            "last_proved_digest": hex_encode(&s.last_proved_digest),
            "accumulator_root": hex_encode(&s.accumulator_root),
            "accumulator_size": s.accumulator_size,
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
            "scan_batch_blocks_default":       crate::chain_follower::DEFAULT_SCAN_BATCH_BLOCKS,
        }),
    }))
}
