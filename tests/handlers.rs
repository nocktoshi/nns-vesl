//! End-to-end handler tests.
//!
//! Boots the real Hoon kernel (`out.jam` built from `hoon/app/app.hoon`)
//! in a temp directory and drives it via the HTTP router — no network,
//! no real payment, stubbed tx hashes.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use nockapp::kernel::boot;
use nockapp::NockApp;
use nockapp::wire::{SystemWire, Wire};
use nns_vesl::kernel::{build_claim_poke, first_error_message, has_effect};
use nns_vesl::{api, state::AppState};
use tokio::sync::Mutex;
use tower::util::ServiceExt;
use vesl_core::SettlementConfig;

const ADDR1: &str = "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFGHIJ";
const ADDR2: &str = "ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210abcdefghij";

fn kernel_jam() -> Vec<u8> {
    let path = std::env::var("NNS_KERNEL_JAM")
        .unwrap_or_else(|_| "out.jam".to_string());
    match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => std::fs::read("../out.jam").unwrap_or_else(|e| {
            panic!("could not read kernel jam at {path} or ../out.jam: {e}")
        }),
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
    let state = Arc::new(Mutex::new(AppState::new(
        app,
        tmp.path().to_path_buf(),
        SettlementConfig::local(),
    )));
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

    let (status, body) = request_json(
        router.clone(),
        "GET",
        "/pending",
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(|a| a.len()), Some(1));
    assert_eq!(body[0]["name"], "alpha.nock");
    assert_eq!(body[0]["status"], "pending");

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(r#"{{"address":"{ADDR1}","name":"alpha.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "claim response: {body}");
    assert_eq!(body["registration"]["status"], "registered");
    assert!(body["registration"]["txHash"].as_str().unwrap().starts_with("stub-"));

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
        Some(&format!(r#"{{"address":"{ADDR1}","name":"bravo.nock"}}"#)),
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

    // 4-char stem exercises the premium tier (5000).
    let (_, body) = request_json(router.clone(), "GET", "/search?name=echo", None).await;
    assert_eq!(body["status"], "available");
    assert_eq!(body["price"], 5000);

    // 5-char stem sits in the mid tier (500).
    let (_, body) = request_json(router.clone(), "GET", "/search?name=delta", None).await;
    assert_eq!(body["status"], "available");
    assert_eq!(body["price"], 500);

    // 10+-char stem sits in the cheap tier (100).
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

/// The mirror's uniqueness check is a courtesy — the kernel is the
/// actual gatekeeper. Clear the mirror mid-session, leave the kernel
/// state intact, and retry the same name from a different owner: the
/// kernel's %claim emits [%claim-error 'name already registered']
/// without mutating state, and the hull surfaces that as 400 "Name
/// already registered". This is the regression test for the silent
/// re-registration bug.
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
        Some(&format!(r#"{{"address":"{ADDR1}","name":"delta.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    {
        let mut st = state.lock().await;
        st.mirror.names.clear();
        st.mirror.primaries.clear();
    }

    let (status, _) = request_json(
        router.clone(),
        "POST",
        "/register",
        Some(&format!(r#"{{"address":"{ADDR2}","name":"delta.nock"}}"#)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "mirror cleared: register reopens");

    let (status, body) = request_json(
        router.clone(),
        "POST",
        "/claim",
        Some(&format!(r#"{{"address":"{ADDR2}","name":"delta.nock"}}"#)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "kernel must reject duplicate: {body}"
    );
    assert_eq!(body["error"], "Name already registered");
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
        let mut st = state.lock().await;
        st.app
            .poke(
                SystemWire.to_wire(),
                build_claim_poke("foxtrot.nock", ADDR1, 5000, tx_hash),
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
        let mut st = state.lock().await;
        st.app
            .poke(
                SystemWire.to_wire(),
                build_claim_poke("golf.nock", ADDR2, 5000, tx_hash),
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
        Some(&format!(r#"{{"address":"{ADDR1}","name":"alpha.nock"}}"#)),
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

    for (i, name) in ["alpha.nock", "beta.nock", "charlie.nock"].iter().enumerate() {
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
        Some(&format!(r#"{{"address":"{ADDR1}","name":"india.nock"}}"#)),
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
            Some(&format!(r#"{{"address":"{ADDR1}","name":"{name}"}}"#)),
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
        let mut st = state.lock().await;
        for (name, addr, tx) in claims.iter() {
            let effects = st
                .app
                .poke(
                    SystemWire.to_wire(),
                    build_claim_poke(name, addr, 5000, tx),
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
