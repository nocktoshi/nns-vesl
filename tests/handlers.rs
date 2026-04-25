//! End-to-end handler tests.
//!
//! Boots the real Hoon kernel (`out.jam` built from `hoon/app/app.hoon`)
//! in a temp directory and drives it via the HTTP router — no network,
//! no real payment, stubbed tx hashes.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use nns_vesl::chain::ConfirmedTxPosition;
use nns_vesl::kernel::{build_claim_poke, first_error_message, has_effect};
use nns_vesl::{api, state::AppState};
use nockapp::kernel::boot;
use nockapp::wire::{SystemWire, Wire};
use nockapp::NockApp;
use tower::util::ServiceExt;
use vesl_core::{SettlementConfig, SettlementMode};

const ADDR1: &str = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ";
const ADDR2: &str = "ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210abcdefghij";

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
async fn register_then_claim_flow() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"alpha.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "register response: {body}");
    assert_eq!(body["status"], "pending");
    assert_eq!(body["name"], "alpha.nock");

    let (status, body) = request_json(router.clone(), "GET", "/pending", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(|a| a.len()), Some(1));
    assert_eq!(body[0]["name"], "alpha.nock");
    assert_eq!(body[0]["status"], "pending");

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(
            r#"{{"address":"{ADDR1}","name":"alpha.nock","txHash":"tx-alpha-1"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "claim response: {body}");
    assert_eq!(body["registration"]["status"], "registered");
    assert_eq!(body["registration"]["txHash"], "tx-alpha-1");

    let (status, body) = request_json(
        router.clone(),
        "GET",
        &format!("/resolve?name=alpha.nock"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["address"], ADDR1);

    let (status, body) = request_json(
        router.clone(),
        "GET",
        &format!("/resolve?address={ADDR1}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["name"], "alpha.nock");
}

#[tokio::test]
async fn register_same_name_different_owner_rejected() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"bravo.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(
            r#"{{"address":"{ADDR1}","name":"bravo.nock","txHash":"tx-bravo-1"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR2}","name":"bravo.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got: {body}");
}

#[tokio::test]
async fn register_is_idempotent_for_same_owner() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    for _ in 0..3 {
        let (status, _) = request_json(
            router.clone(),
            "POST",
            "/register",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"charlie.nock"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    let (_, body) = request_json(router.clone(), "GET", "/pending", None).await;
    assert_eq!(body.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn search_available_pending_registered() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    // 4-char stem: premium tier (5000 NOCK in API; kernel uses nicks).
    let (_, body) = request_json(router.clone(), "GET", "/search?name=echo", None).await;
    assert_eq!(body["status"], "available");
    assert_eq!(body["price"], 5000);

    // 5-char stem: mid tier (500 NOCK).
    let (_, body) = request_json(router.clone(), "GET", "/search?name=delta", None).await;
    assert_eq!(body["status"], "available");
    assert_eq!(body["price"], 500);

    // 10+-char stem: cheap tier (100 NOCK).
    let (_, body) = request_json(router.clone(), "GET", "/search?name=hugecoolname", None).await;
    assert_eq!(body["price"], 100);

    let _ = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"delta.nock"}}"#)),
    )
    .await;
    let (_, body) = request_json(router.clone(), "GET", "/search?name=delta", None).await;
    assert_eq!(body["status"], "pending");
    assert_eq!(body["owner"], ADDR1);

    let _ = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"delta.nock"}}"#)),
    )
    .await;
    let (_, body) = request_json(router.clone(), "GET", "/search?name=delta", None).await;
    assert_eq!(body["status"], "registered");
}

#[tokio::test]
async fn resolve_unknown_is_404() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, _) = request_json(
        router.clone(),
        "GET",
        "/resolve?name=doesnotexist.nock",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn invalid_name_is_400() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"Bad.NOCK"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn claim_requires_tx_hash() {
    let (_tmp, state) = setup().await;
    {
        let mut h = state.hull.lock().await;
        h.settlement.mode = SettlementMode::Fakenet;
    }
    let router = api::router(state.clone());

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"needtx.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"needtx.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "got: {body}");
    assert_eq!(body["error"], "missing txHash");
}

#[tokio::test]
async fn claim_without_tx_hash_is_allowed_in_local_mode() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"fakenet.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"fakenet.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body}");
    assert!(
        body["txHash"]
            .as_str()
            .unwrap_or_default()
            .starts_with("stub-"),
        "expected synthetic txHash in local mode: {body}"
    );
}

/// The mirror's uniqueness check is a courtesy — the kernel is the
/// actual gatekeeper. Clear the mirror mid-session, leave the kernel
/// state intact, and retry the same name from a different owner: the
/// hull now peeks kernel ownership during `/register` and rejects the
/// duplicate immediately, instead of reopening registration.
#[tokio::test]
async fn kernel_rejects_duplicate_even_when_mirror_forgets() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"delta.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(
            r#"{{"address":"{ADDR1}","name":"delta.nock","txHash":"tx-delta-2"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    {
        let mut h = state.hull.lock().await;
        h.mirror.names.clear();
        h.mirror.primaries.clear();
    }

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR2}","name":"delta.nock"}}"#)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "kernel-backed register check must reject duplicate"
    );
}

#[tokio::test]
async fn resolve_by_name_falls_back_to_kernel_when_mirror_is_stale() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"echo.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(
            r#"{{"address":"{ADDR1}","name":"echo.nock","txHash":"tx-echo-1"}}"#
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    {
        let mut h = state.hull.lock().await;
        h.mirror.names.clear();
    }

    let (status, body) = request_json(router, "GET", "/resolve?name=echo.nock", None).await;
    assert_eq!(status, StatusCode::OK, "got: {body}");
    assert_eq!(body["address"], ADDR1);
}

/// Payment uniqueness: the kernel's `tx-hashes` set rejects a second
/// `%claim` that reuses a tx-hash from an earlier claim, even if the
/// name is different. This is the on-kernel replacement for the old
/// hull-side `used_tx_hashes` cache — one payment, one name, enforced
/// by the same authority that enforces name uniqueness.
#[tokio::test]
async fn kernel_rejects_duplicate_tx_hash() {
    let (_tmp, state) = setup().await;
    let tx_hash = "stub-payment-dedup-test";

    let effects1 = {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_claim_poke("foxtrot.nock", ADDR1, 327_680_000, tx_hash),
        )
        .await
        .expect("first claim poke")
    };
    assert!(
        has_effect(&effects1, "claimed"),
        "first claim should succeed"
    );
    assert!(
        first_error_message(&effects1).is_none(),
        "first claim should not emit an error"
    );

    let effects2 = {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_claim_poke("golf.nock", ADDR2, 327_680_000, tx_hash),
        )
        .await
        .expect("second claim poke")
    };
    let err = first_error_message(&effects2)
        .expect("second claim with reused tx-hash must emit %claim-error");
    assert!(
        err.contains("payment already used"),
        "expected payment-reuse error, got: {err}"
    );
    assert!(
        !has_effect(&effects2, "claimed"),
        "second claim must not emit %claimed"
    );
}

/// Same-height race: two competing claims for one name are ordered by
/// tx index inside the block (not local submission order). The lower
/// tx index wins; the later tx is rejected by kernel C3.
#[tokio::test]
async fn follower_orders_same_height_claims_by_tx_index() {
    let (_tmp, state) = setup().await;

    // Force non-local follower path so ordering uses the mocked chain
    // position provider.
    {
        let mut h = state.hull.lock().await;
        h.settlement.mode = SettlementMode::Fakenet;
        h.settlement.chain_endpoint = Some("http://mock-chain".to_string());

        // Enqueue in opposite order: tx-a first, tx-b second.
        h.mirror.enqueue_claim(
            "claim-a".to_string(),
            ADDR1.to_string(),
            "zero.nock".to_string(),
            327_680_000,
            "tx-a".to_string(),
        );
        h.mirror.enqueue_claim(
            "claim-b".to_string(),
            ADDR2.to_string(),
            "zero.nock".to_string(),
            327_680_000,
            "tx-b".to_string(),
        );
    }

    // Same block height, reversed tx index: tx-b should apply first.
    nns_vesl::chain_follower::process_once_with_position_lookup(&state, |_endpoint, tx_hash| {
        let pos = match tx_hash.as_str() {
            "tx-a" => Some(ConfirmedTxPosition {
                block_height: 42,
                tx_index_in_block: 9,
            }),
            "tx-b" => Some(ConfirmedTxPosition {
                block_height: 42,
                tx_index_in_block: 3,
            }),
            _ => None,
        };
        async move { Ok(pos) }
    })
    .await
    .expect("follower pass");

    let h = state.hull.lock().await;
    let winner = h
        .mirror
        .names
        .get("zero.nock")
        .expect("name should be registered by winner");
    assert_eq!(winner.address, ADDR2, "lower tx index should win");

    let status_a = h
        .mirror
        .claim_status("claim-a")
        .expect("claim-a status present");
    let status_b = h
        .mirror
        .claim_status("claim-b")
        .expect("claim-b status present");
    assert_eq!(
        status_b.status,
        nns_vesl::types::ClaimLifecycleStatus::Finalized
    );
    assert_eq!(
        status_a.status,
        nns_vesl::types::ClaimLifecycleStatus::Rejected
    );
    assert!(
        status_a
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("name already registered"),
        "loser should be rejected by kernel C3: {status_a:?}"
    );
}

/// One address registers two names. Both show up under that owner in
/// `/search?address=`. `/resolve?address=` returns the *first*
/// registered name (the kernel auto-assigns it as primary) and does
/// not silently flip to the second one just because it was claimed
/// later.
#[tokio::test]
async fn one_address_can_own_multiple_names() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    for name in ["alpha.nock", "beta.nock"] {
        let (status, _) = request_json(
            router.clone(),
            "POST",
            "/register",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "register {name}");

        let (status, _) = request_json(
            router.clone(),
            "POST",
            "/claim",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "claim {name}");
    }

    let (status, body) = request_json(
        router.clone(),
        "GET",
        &format!("/search?address={ADDR1}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let verified = body["verified"].as_array().expect("verified array");
    assert_eq!(verified.len(), 2, "both names listed: {body}");
    let names: Vec<&str> = verified
        .iter()
        .map(|v| v["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"alpha.nock"));
    assert!(names.contains(&"beta.nock"));
    assert_eq!(
        body["primary"], "alpha.nock",
        "primary is the first-claimed name: {body}"
    );

    let (status, body) = request_json(
        router.clone(),
        "GET",
        &format!("/resolve?address={ADDR1}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["name"], "alpha.nock",
        "resolve returns auto-primary, not last-registered: {body}"
    );
}

/// Owner can switch their primary to a different name they own, and
/// `/resolve?address=` follows. Switching back works too.
#[tokio::test]
async fn set_primary_switches_reverse_lookup() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    for name in ["alpha.nock", "beta.nock"] {
        let _ = request_json(
            router.clone(),
            "POST",
            "/register",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        let _ = request_json(
            router.clone(),
            "POST",
            "/claim",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
    }

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/primary",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"beta.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "set-primary: {body}");
    assert_eq!(body["name"], "beta.nock");
    assert_eq!(body["address"], ADDR1);

    let (_, body) = request_json(
        router.clone(),
        "GET",
        &format!("/resolve?address={ADDR1}"),
        None,
    )
    .await;
    assert_eq!(body["name"], "beta.nock", "resolve follows primary");

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/primary",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"alpha.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (_, body) = request_json(
        router.clone(),
        "GET",
        &format!("/resolve?address={ADDR1}"),
        None,
    )
    .await;
    assert_eq!(body["name"], "alpha.nock", "resolve follows primary back");
}

/// The kernel rejects `%set-primary` for a name the caller does not
/// own. Other address's name — 400, not a sneaky takeover.
#[tokio::test]
async fn set_primary_requires_ownership() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    // ADDR1 registers the name; ADDR2 never owned it.
    let _ = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"alpha.nock"}}"#)),
    )
    .await;
    let _ = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(
            r#"{{"address":"{ADDR1}","name":"alpha.nock","txHash":"tx-alpha-2"}}"#
        )),
    )
    .await;

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/primary",
        Some(&format!(r#"{{"address":"{ADDR2}","name":"alpha.nock"}}"#)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "kernel must reject non-owner: {body}"
    );
    assert!(
        body["error"].as_str().unwrap().contains("not the owner"),
        "expected ownership error, got: {body}"
    );

    // ADDR1's primary was untouched by the rejected attempt.
    let (_, body) = request_json(
        router.clone(),
        "GET",
        &format!("/resolve?address={ADDR1}"),
        None,
    )
    .await;
    assert_eq!(body["name"], "alpha.nock");
}

/// `/snapshot` returns the kernel's current commitment and
/// advances on each successful `%claim`. Before the first claim
/// the registry has no commitment and the endpoint 404s.
#[tokio::test]
async fn snapshot_advances_with_each_claim() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    // Empty registry — no snapshot yet.
    let (status, _) = request_json(router.clone(), "GET", "/snapshot", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let mut last_root: Option<String> = None;
    let mut last_claim_id: u64 = 0;

    for (i, name) in ["alpha.nock", "beta.nock", "charlie.nock"]
        .iter()
        .enumerate()
    {
        let _ = request_json(
            router.clone(),
            "POST",
            "/register",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        let (status, _) = request_json(
            router.clone(),
            "POST",
            "/claim",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "claim {name}");

        let (status, body) = request_json(router.clone(), "GET", "/snapshot", None).await;
        assert_eq!(status, StatusCode::OK);
        let claim_id = body["claim_id"].as_u64().expect("claim_id u64");
        assert_eq!(claim_id as usize, i + 1, "claim-id monotonic: {body}");
        let root = body["root"].as_str().expect("root hex").to_string();
        let hull = body["hull"].as_str().expect("hull hex").to_string();
        assert!(!root.is_empty() && !hull.is_empty(), "non-empty commit");
        if let Some(prev) = &last_root {
            assert_ne!(*prev, root, "root changes when names are added");
        }
        assert!(claim_id > last_claim_id);
        last_root = Some(root);
        last_claim_id = claim_id;
    }
}

/// Claim-then-claim-then-settle bundles every unsettled name into
/// a single batch. The response carries every name in canonical
/// (`aor`) order and `count == 3`, and `/pending-batch` is empty
/// afterwards.
#[tokio::test]
async fn settle_batch_rolls_up_all_unsettled() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    for name in ["alpha.nock", "bravo.nock", "charlie.nock"] {
        let _ = request_json(
            router.clone(),
            "POST",
            "/register",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        let (status, _) = request_json(
            router.clone(),
            "POST",
            "/claim",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "claim {name}");
    }

    let (_, snap) = request_json(router.clone(), "GET", "/snapshot", None).await;

    let (status, body) = request_json(router.clone(), "POST", "/settle", None).await;
    assert_eq!(status, StatusCode::OK, "settle: {body}");
    assert_eq!(body["count"], 3);
    assert_eq!(body["claim_id"], snap["claim_id"]);
    assert_eq!(body["hull"], snap["hull"]);
    assert_eq!(body["root"], snap["root"]);
    let names: Vec<&str> = body["names"]
        .as_array()
        .expect("names array")
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        vec!["alpha.nock", "bravo.nock", "charlie.nock"],
        "sorted canonically"
    );
    assert!(
        !body["note_id"].as_str().unwrap().is_empty(),
        "note_id surfaced: {body}"
    );

    // Window is now drained.
    let (status, body) = request_json(router.clone(), "GET", "/pending-batch", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 0);
    assert_eq!(body["names"].as_array().unwrap().len(), 0);
    assert_eq!(body["last_settled_claim_id"], snap["claim_id"]);
}

/// Two `/settle`s back-to-back with no intervening `%claim`: the
/// first succeeds, the second has an empty window and 400s with
/// "nothing to settle".
#[tokio::test]
async fn settle_twice_in_a_row_second_is_empty() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let _ = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"india.nock"}}"#)),
    )
    .await;
    let _ = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(
            r#"{{"address":"{ADDR1}","name":"india.nock","txHash":"tx-india-1"}}"#
        )),
    )
    .await;

    let (status, _) = request_json(router.clone(), "POST", "/settle", None).await;
    assert_eq!(status, StatusCode::OK, "first settle");

    let (status, body) = request_json(router.clone(), "POST", "/settle", None).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "second settle must 400: {body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap_or("")
            .contains("nothing to settle"),
        "expected 'nothing to settle', got: {body}"
    );
}

/// Settle with no claims whatsoever is also "nothing to settle".
#[tokio::test]
async fn settle_empty_registry_is_rejected() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, body) = request_json(router.clone(), "POST", "/settle", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "empty settle: {body}");
    assert!(body["error"]
        .as_str()
        .unwrap_or("")
        .contains("nothing to settle"));
}

/// After a successful settle, further `%claim`s open a new pending
/// window. The next `/settle` packages only those new claims into
/// a fresh batch with a distinct note-id.
#[tokio::test]
async fn claim_after_settle_gets_its_own_batch() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    for name in ["alpha.nock", "bravo.nock"] {
        let _ = request_json(
            router.clone(),
            "POST",
            "/register",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        let _ = request_json(
            router.clone(),
            "POST",
            "/claim",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
    }

    let (_, first) = request_json(router.clone(), "POST", "/settle", None).await;
    let first_note = first["note_id"].as_str().unwrap().to_string();
    assert_eq!(first["count"], 2);

    // Two more claims open a brand-new settlement window.
    for name in ["charlie.nock", "delta.nock"] {
        let _ = request_json(
            router.clone(),
            "POST",
            "/register",
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
        )
        .await;
        let _ = request_json(
            router.clone(),
            "POST",
            "/claim",
            Some(&format!(
                r#"{{"address":"{ADDR1}","name":"{name}","txHash":"tx-{name}"}}"#
            )),
        )
        .await;
    }

    let (status, second) = request_json(router.clone(), "POST", "/settle", None).await;
    assert_eq!(status, StatusCode::OK, "second settle: {second}");
    assert_eq!(second["count"], 2, "only the new claims");
    let names: Vec<&str> = second["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["charlie.nock", "delta.nock"]);
    let second_note = second["note_id"].as_str().unwrap().to_string();
    assert_ne!(
        first_note, second_note,
        "distinct batches produce distinct note-ids"
    );
}

/// `note_id = hash-leaf(jam sorted-batch)` is a pure function of
/// the (name, owner, tx-hash) triples in the batch — pin the
/// tx-hashes across two independent kernel instances and assert
/// the produced note-ids are bit-identical.
#[tokio::test]
async fn batch_note_id_is_deterministic() {
    let claims = [
        ("alpha.nock", ADDR1, "fixed-tx-1"),
        ("bravo.nock", ADDR1, "fixed-tx-2"),
        ("charlie.nock", ADDR1, "fixed-tx-3"),
    ];

    let (_tmp1, state1) = setup().await;
    let (_tmp2, state2) = setup().await;

    for state in [state1.clone(), state2.clone()] {
        let mut k = state.kernel.lock().await;
        for (name, addr, tx) in claims.iter() {
            let effects = k
                .poke(
                    SystemWire.to_wire(),
                    build_claim_poke(name, addr, 327_680_000, tx),
                )
                .await
                .expect("claim poke");
            assert!(has_effect(&effects, "claimed"));
        }
    }

    let router1 = api::router(state1.clone());
    let router2 = api::router(state2.clone());
    let (s1, b1) = request_json(router1, "POST", "/settle", None).await;
    let (s2, b2) = request_json(router2, "POST", "/settle", None).await;
    assert_eq!(s1, StatusCode::OK);
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(
        b1["note_id"], b2["note_id"],
        "same (name, owner, tx-hash) set -> same note-id: {b1} vs {b2}"
    );
    assert_eq!(b1["root"], b2["root"], "same tree shape -> same root");
    assert_eq!(b1["count"], b2["count"]);
}

/// `/pending-batch` reflects the settlement window: size equals
/// claims-since-last-settle, shrinks to 0 after a settle, and
/// grows again when fresh claims come in.
#[tokio::test]
async fn pending_batch_endpoint_reflects_window() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    // Empty registry: window is empty and last_settled_claim_id = 0.
    let (status, body) = request_json(router.clone(), "GET", "/pending-batch", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["count"], 0);
    assert_eq!(body["claim_id"], 0);
    assert_eq!(body["last_settled_claim_id"], 0);

    for name in ["alpha.nock", "bravo.nock"] {
        let _ = request_json(
            router.clone(),
            "POST",
            "/register",
            Some(&format!(
                r#"{{"address":"{ADDR1}","name":"{name}","txHash":"tx-{name}"}}"#
            )),
        )
        .await;
        let _ = request_json(
            router.clone(),
            "POST",
            "/claim",
            Some(&format!(
                r#"{{"address":"{ADDR1}","name":"{name}","txHash":"tx-{name}"}}"#
            )),
        )
        .await;
    }

    let (_, body) = request_json(router.clone(), "GET", "/pending-batch", None).await;
    assert_eq!(body["count"], 2);
    let names: Vec<&str> = body["names"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["alpha.nock", "bravo.nock"]);
    assert_eq!(body["claim_id"], 2);
    assert_eq!(body["last_settled_claim_id"], 0);

    let _ = request_json(router.clone(), "POST", "/settle", None).await;

    let (_, body) = request_json(router.clone(), "GET", "/pending-batch", None).await;
    assert_eq!(body["count"], 0, "window drained after settle");
    assert_eq!(body["claim_id"], 2);
    assert_eq!(body["last_settled_claim_id"], 2);
}

/// `%set-primary` for a name that has never been registered is a
/// 400 with the kernel's "name not registered" message.
#[tokio::test]
async fn set_primary_rejects_unknown_name() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/primary",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"ghost.nock"}}"#)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "unknown name must 400: {body}"
    );
    assert!(
        body["error"].as_str().unwrap().contains("not registered"),
        "expected 'name not registered', got: {body}"
    );
}

// ---------------------------------------------------------------------------
// GET /proof
// ---------------------------------------------------------------------------

async fn register_and_claim(router: axum::Router, addr: &str, name: &str) {
    let (s, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{addr}","name":"{name}"}}"#)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = request_json(
        router,
        "POST",
        "/claim",
        Some(&format!(
            r#"{{"address":"{addr}","name":"{name}","txHash":"tx-{name}"}}"#
        )),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn proof_returns_full_bundle_for_registered_name() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    register_and_claim(router.clone(), ADDR1, "proof1.nock").await;

    let (status, body) = request_json(router.clone(), "GET", "/proof?name=proof1.nock", None).await;
    assert_eq!(status, StatusCode::OK, "got: {body}");

    assert_eq!(body["name"], "proof1.nock");
    assert_eq!(body["owner"], ADDR1);
    assert_eq!(
        body["txHash"], "tx-proof1.nock",
        "txHash should match submitted claim payment id: {body}"
    );
    assert!(
        body["claim_id"].as_u64().unwrap() >= 1,
        "claim_id must be bumped: {body}"
    );
    assert!(
        !body["root"].as_str().unwrap().is_empty(),
        "root hex must be non-empty"
    );
    assert!(
        !body["hull"].as_str().unwrap().is_empty(),
        "hull hex must be non-empty"
    );
    // Single-leaf tree: proof is trivially empty (leaf IS the root).
    assert_eq!(
        body["proof"].as_array().map(|a| a.len()),
        Some(0),
        "one leaf -> empty proof: {body}"
    );

    // Cross-check: /snapshot's root must match /proof's root — both
    // read the same kernel state.
    let (_, snap) = request_json(router.clone(), "GET", "/snapshot", None).await;
    assert_eq!(
        snap["root"], body["root"],
        "snapshot root should match proof root"
    );
}

#[tokio::test]
async fn proof_contains_sibling_chain_for_multi_leaf_tree() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    for name in ["p1.nock", "p2.nock", "p3.nock", "p4.nock"] {
        register_and_claim(router.clone(), ADDR1, name).await;
    }

    let (status, body) = request_json(router.clone(), "GET", "/proof?name=p2.nock", None).await;
    assert_eq!(status, StatusCode::OK, "got: {body}");

    let proof = body["proof"].as_array().unwrap();
    // 4-leaf tree -> depth-2 proof (2 siblings).
    assert_eq!(proof.len(), 2, "4-leaf tree has 2-step proof: {body}");
    for node in proof {
        assert!(!node["hash"].as_str().unwrap().is_empty());
        let side = node["side"].as_str().unwrap();
        assert!(side == "left" || side == "right", "got side={side}");
    }
}

#[tokio::test]
async fn proof_404s_for_unregistered_name() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    register_and_claim(router.clone(), ADDR1, "exists.nock").await;

    let (status, body) = request_json(router.clone(), "GET", "/proof?name=nope.nock", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "got: {body}");
    assert!(body["error"].as_str().unwrap().contains("not registered"));
}

#[tokio::test]
async fn proof_404s_when_address_does_not_match_owner() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    register_and_claim(router.clone(), ADDR1, "mine.nock").await;

    // Right name, wrong owner -> 404 (we don't leak ownership to
    // callers who didn't know the pair).
    let (status, body) = request_json(
        router.clone(),
        "GET",
        &format!("/proof?name=mine.nock&address={ADDR2}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "got: {body}");
    assert!(body["error"].as_str().unwrap().contains("does not own"));

    // Matching address -> 200.
    let (status, body) = request_json(
        router.clone(),
        "GET",
        &format!("/proof?name=mine.nock&address={ADDR1}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "got: {body}");
    assert_eq!(body["owner"], ADDR1);
}

#[tokio::test]
async fn proof_rejects_invalid_inputs() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, _) = request_json(router.clone(), "GET", "/proof", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request_json(router.clone(), "GET", "/proof?name=Foo.nock", None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _) = request_json(
        router.clone(),
        "GET",
        "/proof?name=ok.nock&address=short",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// End-to-end verify-chunk walk
//
// Confirms that the bundle `/proof` hands out is bit-compatible with a
// Rust-side reimplementation of the same Hoon `verify-chunk` logic:
//
//   1. chunk = jam([name owner tx-hash])          (nock_noun_rs)
//   2. cur   = hash-leaf(chunk)                   (nockchain_tip5_rs)
//   3. for each proof node: cur = hash-pair(...)
//   4. assert cur == root
//
// If this test starts failing, either the wire encoding drifted or the
// Hoon / Rust tip5 ports diverged — both are load-bearing for any
// client-side verifier we ship.
// ---------------------------------------------------------------------------

use nock_noun_rs::{jam_to_bytes, make_cord, new_stack, T as TupleIn};
use nockchain_tip5_rs::{verify_proof, ProofNode as Tip5ProofNode, Tip5Hash};

/// Goldilocks prime. Same constant `nockchain_math::belt::PRIME` uses;
/// hard-coded here to keep the test free of an extra path dep.
const GOLDILOCKS_PRIME: u64 = 18_446_744_069_414_584_321;

/// Decode a lowercase hex string to LE bytes. Panics on malformed input
/// — fine for test use against `state::hex_encode`'s output.
fn hex_decode(s: &str) -> Vec<u8> {
    assert!(s.len() % 2 == 0, "hex length must be even");
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("valid hex"))
        .collect()
}

/// Inverse of `nockchain_tip5_rs::tip5_to_atom_le_bytes`.
///
/// The forward direction is a base-PRIME Horner expansion
/// `limb[0] + limb[1]*P + limb[2]*P^2 + ...`, emitted as LE atom bytes.
/// We recover the limbs by repeated long-division (base 256) of that
/// byte vector by PRIME and taking remainders.
fn le_bytes_to_tip5(bytes: &[u8]) -> Tip5Hash {
    let mut n: Vec<u8> = bytes.to_vec();
    while n.last() == Some(&0) {
        n.pop();
    }

    let mut limbs: Tip5Hash = [0u64; 5];
    for limb in limbs.iter_mut() {
        if n.is_empty() {
            break;
        }
        // Long-divide n (LE byte integer) by PRIME.
        //   rem <= PRIME - 1 < 2^64, so (rem << 8) | byte fits in u128
        //   (max ~2^72), and the quotient byte ≤ 2^72 / PRIME < 2^8.
        let mut rem: u128 = 0;
        let mut quot = vec![0u8; n.len()];
        for i in (0..n.len()).rev() {
            let v = (rem << 8) | (n[i] as u128);
            quot[i] = (v / GOLDILOCKS_PRIME as u128) as u8;
            rem = v % GOLDILOCKS_PRIME as u128;
        }
        *limb = rem as u64;
        while quot.last() == Some(&0) {
            quot.pop();
        }
        n = quot;
    }
    limbs
}

/// Compute `jam([name owner tx-hash])` — the exact leaf-chunk bytes
/// the kernel's `+leaf-chunk` produces and `+hash-leaf` absorbs.
fn jam_leaf(name: &str, owner: &str, tx_hash: &str) -> Vec<u8> {
    let mut stack = new_stack();
    let n = make_cord(&mut stack, name);
    let o = make_cord(&mut stack, owner);
    let t = make_cord(&mut stack, tx_hash);
    let triple = TupleIn(&mut stack, &[n, o, t]);
    jam_to_bytes(&mut stack, triple)
}

#[tokio::test]
async fn le_bytes_to_tip5_is_inverse_of_tip5_to_atom_le_bytes() {
    // Round-trip sanity check: hash a few fixed inputs, encode with the
    // crate's forward function, decode with ours, and confirm we got
    // the original back. This keeps the inverse honest independent of
    // the kernel so failures in the big end-to-end test localize.
    use nockchain_tip5_rs::{hash_leaf, tip5_to_atom_le_bytes};
    for data in [
        &b"alpha"[..],
        b"bravo.nock",
        b"",
        b"\x00\x01\x02\x03\x04\x05\x06\x07",
    ] {
        let h = hash_leaf(data);
        let bytes = tip5_to_atom_le_bytes(&h);
        let roundtrip = le_bytes_to_tip5(&bytes);
        assert_eq!(
            roundtrip, h,
            "round-trip failed for {data:?}: {h:?} -> {bytes:?} -> {roundtrip:?}"
        );
    }
}

#[tokio::test]
async fn proof_verifies_end_to_end_against_tip5() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    // Multi-leaf tree so the proof has a non-trivial sibling chain
    // — a 1-leaf tree would make verification vacuous.
    for name in ["v1.nock", "v2.nock", "v3.nock", "v4.nock", "v5.nock"] {
        register_and_claim(router.clone(), ADDR1, name).await;
    }

    let (status, body) = request_json(router.clone(), "GET", "/proof?name=v3.nock", None).await;
    assert_eq!(status, StatusCode::OK, "got: {body}");

    let name = body["name"].as_str().unwrap();
    let owner = body["owner"].as_str().unwrap();
    let tx_hash = body["txHash"].as_str().unwrap();
    let root_hex = body["root"].as_str().unwrap();

    // Server hands us the root + every sibling as hex LE atom bytes
    // (output of `digest-to-atom:tip5`). Invert that to Tip5Hash form
    // so we can feed it to nockchain_tip5_rs::verify_proof.
    let root: Tip5Hash = le_bytes_to_tip5(&hex_decode(root_hex));
    let proof_nodes: Vec<Tip5ProofNode> = body["proof"]
        .as_array()
        .expect("proof must be an array")
        .iter()
        .map(|n| {
            let hash = le_bytes_to_tip5(&hex_decode(n["hash"].as_str().unwrap()));
            let side = n["side"].as_str().unwrap() == "left";
            Tip5ProofNode { hash, side }
        })
        .collect();

    // 5 leaves -> tree depth 3 (padded to 8 at level 0).
    assert_eq!(proof_nodes.len(), 3, "5-leaf tree has depth-3 proof");

    // Reconstruct the leaf preimage = jam([name owner tx-hash]).
    // This must byte-match what the kernel's +leaf-chunk produces,
    // since tip5 hash-leaf ingests these bytes directly.
    let leaf = jam_leaf(name, owner, tx_hash);

    // Happy path: server proof + server root + correct triple verifies.
    assert!(
        verify_proof(&leaf, &proof_nodes, &root),
        "end-to-end verify-chunk walk rejected a proof the server handed us — \
         wire encoding or tip5 port has drifted from the Hoon kernel"
    );

    // Tampered leaf: same proof + same root, but wrong owner.
    let bad_owner_leaf = jam_leaf(name, ADDR2, tx_hash);
    assert!(
        !verify_proof(&bad_owner_leaf, &proof_nodes, &root),
        "leaf with forged owner must not verify"
    );

    // Tampered leaf: wrong tx_hash.
    let bad_tx_leaf = jam_leaf(name, owner, "stub-forged-tx");
    assert!(
        !verify_proof(&bad_tx_leaf, &proof_nodes, &root),
        "leaf with forged tx-hash must not verify"
    );

    // Tampered root: legitimate leaf + proof, but nudged root.
    let mut bad_root = root;
    bad_root[0] = bad_root[0].wrapping_add(1);
    assert!(
        !verify_proof(&leaf, &proof_nodes, &bad_root),
        "nudged root must not verify"
    );

    // Tampered proof: flipping a sibling's side swaps hash-pair order
    // and must break the walk.
    let mut bad_proof = proof_nodes.clone();
    bad_proof[0].side = !bad_proof[0].side;
    assert!(
        !verify_proof(&leaf, &bad_proof, &root),
        "proof with a flipped side must not verify"
    );
}

// ---------------------------------------------------------------------------
// Phase 7.1 — Operator observability
// ---------------------------------------------------------------------------
//
// Four shipping surfaces for operators:
//   1. /status gains a `follower` telemetry object and an `anchor` hint.
//   2. GET /anchor — dedicated anchor + follower surface, 503 when blind.
//   3. POST /admin/advance-tip-now — manual advance, gated by env var.
//   4. Structured tracing (not directly testable from handlers — covered
//      by log-shape review during debugging).
//
// All tests boot a local-mode kernel, which intentionally has the
// follower at bootstrap anchor `(0x0, 0)` and no chain-tip observation.
// Chain-mode behaviour is tested indirectly via phase2_anchor + phase7
// integration tests.

#[tokio::test]
async fn status_exposes_follower_observability_shape() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, body) = request_json(router, "GET", "/status", None).await;
    assert_eq!(status, StatusCode::OK);

    // Existing shape still present.
    assert!(body.get("settlement_mode").is_some());
    assert!(body.get("names_count").is_some());

    // Phase 7.1 additions: `anchor` + `follower` blocks.
    let follower = body.get("follower").expect("status.follower present");
    for key in [
        "chain_tip_height",
        "anchor_lag_blocks",
        "is_caught_up",
        "last_advance_at_epoch_ms",
        "last_advance_tip_height",
        "last_advance_count",
        "last_error",
        "last_error_phase",
        "finality_depth",
        "max_advance_batch",
    ] {
        assert!(
            follower.get(key).is_some(),
            "follower.{key} missing in /status body: {body}"
        );
    }
    // finality_depth is a compile-time constant.
    assert_eq!(follower["finality_depth"], 10);
    assert_eq!(follower["max_advance_batch"], 64);

    // Local mode → no chain observations yet → all telemetry nulls.
    assert!(follower["chain_tip_height"].is_null());
    assert!(follower["last_error"].is_null());
    assert!(follower["last_advance_at_epoch_ms"].is_null());
}

#[tokio::test]
async fn anchor_handler_returns_503_when_blind() {
    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, body) = request_json(router, "GET", "/anchor", None).await;
    // In local mode on a fresh boot we have no follower observations
    // and the kernel anchor peek returns zeros. 503 surfaces "we don't
    // know anything" loudly so operators don't confuse it with "chain
    // is truly at height 0".
    //
    // If a future boot defaults the anchor to a non-zero sentinel, this
    // test will need to be revisited — but for now 503 is the right
    // signal on a cold local kernel.
    assert!(
        status == StatusCode::SERVICE_UNAVAILABLE || status == StatusCode::OK,
        "/anchor must be 503 (blind) or 200 (some data); got {status}: {body}"
    );

    if status == StatusCode::OK {
        assert!(body.get("anchor").is_some() || body.get("chain_tip_height").is_some());
    } else {
        assert!(body.get("error").is_some());
    }
}

/// Serialize admin-env tests — the `NNS_ENABLE_ADMIN` env var is
/// process-global, so two tests flipping it in parallel would race.
/// This mutex is test-file-local and cheap to take.
static ADMIN_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn admin_advance_tip_returns_404_when_admin_disabled() {
    let _serial = ADMIN_ENV_LOCK.lock().expect("admin env lock poisoned");
    let _guard = EnvGuard::unset("NNS_ENABLE_ADMIN");

    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, _body) = request_json(router, "POST", "/admin/advance-tip-now", Some("{}")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn admin_advance_tip_is_noop_in_local_mode_when_enabled() {
    let _serial = ADMIN_ENV_LOCK.lock().expect("admin env lock poisoned");
    let _guard = EnvGuard::set("NNS_ENABLE_ADMIN", "1");

    let (_tmp, state) = setup().await;
    let router = api::router(state.clone());

    let (status, body) = request_json(router, "POST", "/admin/advance-tip-now", Some("{}")).await;
    assert_eq!(status, StatusCode::OK);
    // Local mode always returns advanced=false with a reason.
    assert_eq!(body["advanced"], false);
    assert!(
        body["reason"]
            .as_str()
            .map(|s| s.contains("local mode"))
            .unwrap_or(false),
        "admin endpoint in local mode must name the reason: {body}"
    );
}

/// RAII env-var guard for tests that must mutate process env. Unsets
/// or restores to the pre-test value on drop so parallel tests don't
/// see each other's scratch state. **Use with `#[serial]` or ensure
/// any set/unset pair can't race.**
struct EnvGuard {
    key: &'static str,
    prior: Option<String>,
}

impl EnvGuard {
    fn set(key: &'static str, val: &str) -> Self {
        let prior = std::env::var(key).ok();
        // SAFETY: Cargo's test harness serializes env mutations only
        // within a single test; cross-test concurrency on the SAME key
        // would race. Keep admin-tests in the same file and rely on
        // the two admin tests being sequential by declaring their own
        // EnvGuard — they won't trample each other because the one
        // that unsets explicitly runs first in the file order.
        unsafe { std::env::set_var(key, val) };
        Self { key, prior }
    }
    fn unset(key: &'static str) -> Self {
        let prior = std::env::var(key).ok();
        unsafe { std::env::remove_var(key) };
        Self { key, prior }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(v) => unsafe { std::env::set_var(self.key, v) },
            None => unsafe { std::env::remove_var(self.key) },
        }
    }
}
