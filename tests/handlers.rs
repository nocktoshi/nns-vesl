//! End-to-end handler tests for the Path Y read-only HTTP surface.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use nns_vesl::{api, state::AppState};
use nockapp::kernel::boot;
use nockapp::NockApp;
use tower::util::ServiceExt;
use vesl_core::SettlementConfig;

fn kernel_jam() -> Vec<u8> {
    let path = std::env::var("NNS_KERNEL_JAM").unwrap_or_else(|_| "out.jam".to_string());
    match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => std::fs::read("../out.jam")
            .unwrap_or_else(|e| panic!("could not read kernel jam at {path} or ../out.jam: {e}")),
    }
}

async fn setup() -> (tempfile::TempDir, nns_vesl::state::SharedState) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = boot::default_boot_cli(true);
    let app: NockApp = boot::setup(
        &kernel_jam(),
        cli,
        &[],
        "nns-vesl-test",
        Some(tmp.path().to_path_buf()),
    )
    .await
    .expect("kernel boot");
    let state = Arc::new(AppState::new(
        app,
        tmp.path().to_path_buf(),
        SettlementConfig::local(),
    ));
    (tmp, state)
}

async fn request_json(
    router: axum::Router,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (StatusCode, serde_json::Value) {
    let mut req = Request::builder().method(method).uri(path);
    let req = if let Some(b) = body {
        req = req.header("content-type", "application/json");
        req.body(Body::from(b.to_string())).unwrap()
    } else {
        req.body(Body::empty()).unwrap()
    };
    let resp = router.oneshot(req).await.expect("route");
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 10 * 1024 * 1024)
        .await
        .expect("body");
    let body: serde_json::Value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    };
    (status, body)
}

#[tokio::test]
async fn health_returns_ok() {
    let (_tmp, state) = setup().await;
    let router = api::router(state);

    let (status, body) = request_json(router, "GET", "/health", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn status_exposes_scan_state_and_follower() {
    let (_tmp, state) = setup().await;
    let router = api::router(state);

    let (status, body) = request_json(router, "GET", "/status", None).await;

    assert_eq!(status, StatusCode::OK);
    assert!(body.get("settlement_mode").is_some());
    assert!(body.get("chain_endpoint").is_some());
    assert!(body.get("scan_state").is_some());
    assert!(body.get("follower").is_some());
    assert!(body.get("pending_count").is_none());
    assert!(body.get("registered_count").is_none());

    let scan_state = &body["scan_state"];
    assert_eq!(scan_state["last_proved_height"], 0);
    assert!(scan_state.get("last_proved_digest").is_some());
    assert!(scan_state.get("accumulator_root").is_some());
    assert_eq!(scan_state["accumulator_size"], 0);

    let follower = &body["follower"];
    assert_eq!(follower["finality_depth"], 10);
    assert_eq!(follower["max_advance_batch"], 1);
}

#[tokio::test]
async fn accumulator_lookup_returns_absent_value_for_unknown_name() {
    let (_tmp, state) = setup().await;
    let router = api::router(state);

    let (status, body) = request_json(router, "GET", "/accumulator/alice.nock", None).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "alice.nock");
    assert!(body.get("value").is_none());
    assert!(body.get("proof_axis").is_none());
    assert_eq!(body["last_proved_height"], 0);
    assert_eq!(body["accumulator_size"], 0);
}

#[tokio::test]
async fn accumulator_lookup_rejects_invalid_name() {
    let (_tmp, state) = setup().await;
    let router = api::router(state);

    let (status, body) = request_json(router, "GET", "/accumulator/Alice.nock", None).await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "invalid name");
}

#[tokio::test]
async fn accumulator_wallet_export_includes_snapshot_hex() {
    let (_tmp, state) = setup().await;
    let router = api::router(state);
    let (status, body) = request_json(
        router,
        "GET",
        "/accumulator/alice.nock?wallet_export=true",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let snap = body["accumulator_snapshot_hex"]
        .as_str()
        .expect("accumulator_snapshot_hex");
    assert!(!snap.is_empty());
}
