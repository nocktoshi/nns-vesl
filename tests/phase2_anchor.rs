//! Phase 2 integration tests — kernel anchor + payment-address behaviour.
//!
//! Exercises the new causes landed in Phase 2a against a real booted
//! kernel, without the HTTP layer. Covers:
//!
//!   - `%advance-tip` bootstrap, extend, and reorg rejection paths
//!   - `/anchor` peek roundtrip
//!   - `%set-payment-address` single-shot + freeze-after-first-claim
//!   - `/payment-address` peek roundtrip
//!
//! These are `#[tokio::test]`s (not `#[ignore]`) — the kernel boots in
//! under a second with prover jets disabled, so they run on every
//! `cargo test`.

use std::sync::Arc;

use nockapp::kernel::boot;
use nockapp::wire::{SystemWire, Wire};
use nockapp::NockApp;
use nns_vesl::kernel::{
    build_advance_tip_poke, build_anchor_peek, build_claim_poke, build_payment_address_peek,
    build_set_payment_address_poke, decode_anchor, decode_payment_address, first_anchor_advanced,
    first_error_message, first_payment_address_set, has_effect, AnchorHeader,
};
use nns_vesl::state::AppState;
use tokio::sync::Mutex;
use vesl_core::SettlementConfig;

fn kernel_jam() -> Vec<u8> {
    let path = std::env::var("NNS_KERNEL_JAM").unwrap_or_else(|_| "out.jam".to_string());
    std::fs::read(&path)
        .or_else(|_| std::fs::read("../out.jam"))
        .unwrap_or_else(|e| panic!("could not read kernel jam at {path} or ../out.jam: {e}"))
}

async fn boot_kernel() -> (tempfile::TempDir, nns_vesl::state::SharedState) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let cli = boot::default_boot_cli(true);
    let app: NockApp = boot::setup(
        &kernel_jam(),
        cli,
        &[],
        "nns-phase2-test",
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

/// Synthetic anchor header: 40-byte digests built from the seed byte
/// so chain linkage is easy to reason about in tests.
fn header_at(height: u64, seed: u8, parent_seed: u8) -> AnchorHeader {
    AnchorHeader {
        digest: vec![seed; 40],
        height,
        parent: vec![parent_seed; 40],
    }
}

// =========================================================================
// %advance-tip
// =========================================================================

#[tokio::test]
async fn advance_tip_bootstrap_accepts_from_genesis() {
    let (_tmp, state) = boot_kernel().await;

    let headers = vec![
        header_at(1, 1, 0),
        header_at(2, 2, 1),
        header_at(3, 3, 2),
    ];

    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&headers))
            .await
            .expect("advance-tip poke")
    };

    let advanced = first_anchor_advanced(&effects).expect("anchor-advanced effect");
    assert_eq!(advanced.tip_height, 3);
    assert_eq!(advanced.tip_digest, vec![3u8; 40]);
    assert_eq!(advanced.count, 3);
    assert!(first_error_message(&effects).is_none());

    let view = {
        let mut st = state.lock().await;
        let r = st.app.peek(build_anchor_peek()).await.expect("anchor peek");
        decode_anchor(&r).expect("decode anchor")
    };
    assert_eq!(view.tip_height, 3);
    assert_eq!(view.tip_digest, vec![3u8; 40]);
    // Post-refactor: kernel no longer caches intermediate headers,
    // so the only assertion we can make is on the current tip.
    // Intermediate-chain provenance is carried in per-claim note
    // bundles and verified by the gate, not by state inspection.
}

#[tokio::test]
async fn advance_tip_extends_after_bootstrap() {
    let (_tmp, state) = boot_kernel().await;

    // Bootstrap to height=2.
    let boot_headers = vec![header_at(1, 1, 0), header_at(2, 2, 1)];
    {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&boot_headers))
            .await
            .expect("bootstrap");
    }

    // Extend by two blocks whose parent chains back to the current tip.
    let more = vec![header_at(3, 3, 2), header_at(4, 4, 3)];
    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&more))
            .await
            .expect("extend")
    };

    let advanced = first_anchor_advanced(&effects).expect("anchor-advanced effect");
    assert_eq!(advanced.tip_height, 4);
    assert_eq!(advanced.count, 2);

    let view = {
        let mut st = state.lock().await;
        let r = st.app.peek(build_anchor_peek()).await.expect("anchor peek");
        decode_anchor(&r).expect("decode anchor")
    };
    assert_eq!(view.tip_height, 4);
    assert_eq!(view.tip_digest, vec![4u8; 40]);
}

#[tokio::test]
async fn advance_tip_rejects_parent_mismatch() {
    let (_tmp, state) = boot_kernel().await;

    // Bootstrap to height=2.
    let boot_headers = vec![header_at(1, 1, 0), header_at(2, 2, 1)];
    {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&boot_headers))
            .await
            .expect("bootstrap");
    }

    // Try to extend with a header whose parent is NOT the current tip
    // digest. Simulates a reorg the follower missed.
    let bad = vec![header_at(3, 3, 99)];
    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&bad))
            .await
            .expect("bad advance")
    };

    assert!(first_anchor_advanced(&effects).is_none());
    let err = first_error_message(&effects).expect("anchor-error");
    assert!(
        err.contains("parent mismatch"),
        "expected parent-mismatch, got: {err}"
    );

    // Anchor should still be at height=2.
    let view = {
        let mut st = state.lock().await;
        let r = st.app.peek(build_anchor_peek()).await.expect("anchor peek");
        decode_anchor(&r).expect("decode anchor")
    };
    assert_eq!(view.tip_height, 2);
}

#[tokio::test]
async fn advance_tip_rejects_height_gap() {
    let (_tmp, state) = boot_kernel().await;

    // Bootstrap to height=2.
    let boot_headers = vec![header_at(1, 1, 0), header_at(2, 2, 1)];
    {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&boot_headers))
            .await
            .expect("bootstrap");
    }

    // Jump straight from 2 to 5 (skipping 3 and 4). Even with a matched
    // parent digest the kernel requires strict height+1 linkage.
    let gap = vec![header_at(5, 5, 2)];
    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&gap))
            .await
            .expect("gap advance")
    };
    let err = first_error_message(&effects).expect("anchor-error");
    assert!(
        err.contains("off-by-one"),
        "expected height off-by-one, got: {err}"
    );
}

#[tokio::test]
async fn advance_tip_rejects_internal_break() {
    let (_tmp, state) = boot_kernel().await;

    // Chain with a break in the middle: [1<-0, 2<-1, 3<-99 (bad)].
    let broken = vec![
        header_at(1, 1, 0),
        header_at(2, 2, 1),
        header_at(3, 3, 99),
    ];
    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&broken))
            .await
            .expect("broken advance")
    };
    let err = first_error_message(&effects).expect("anchor-error");
    assert!(
        err.contains("chain mismatch"),
        "expected chain-mismatch, got: {err}"
    );
}

#[tokio::test]
async fn advance_tip_empty_list_is_rejected() {
    let (_tmp, state) = boot_kernel().await;
    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), build_advance_tip_poke(&[]))
            .await
            .expect("empty advance")
    };
    let err = first_error_message(&effects).expect("anchor-error");
    assert!(err.contains("empty"), "expected empty-advance error, got: {err}");
}

// =========================================================================
// %set-payment-address
// =========================================================================

#[tokio::test]
async fn set_payment_address_binds_on_first_poke() {
    let (_tmp, state) = boot_kernel().await;

    // Peek before bootstrap: unit is `~`.
    let before = {
        let mut st = state.lock().await;
        let r = st
            .app
            .peek(build_payment_address_peek())
            .await
            .expect("pre peek");
        decode_payment_address(&r).expect("decode")
    };
    assert_eq!(before, None);

    let addr = "test-address-abc";
    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(
                SystemWire.to_wire(),
                build_set_payment_address_poke(addr),
            )
            .await
            .expect("set address")
    };
    let bound =
        first_payment_address_set(&effects).expect("payment-address-set effect");
    assert_eq!(bound, addr);

    let after = {
        let mut st = state.lock().await;
        let r = st
            .app
            .peek(build_payment_address_peek())
            .await
            .expect("post peek");
        decode_payment_address(&r).expect("decode")
    };
    assert_eq!(after.as_deref(), Some(addr));
}

#[tokio::test]
async fn set_payment_address_can_change_before_first_claim() {
    let (_tmp, state) = boot_kernel().await;

    for addr in ["first-addr", "second-addr"] {
        let effects = {
            let mut st = state.lock().await;
            st.app
                .poke(
                    SystemWire.to_wire(),
                    build_set_payment_address_poke(addr),
                )
                .await
                .expect("set address")
        };
        let bound =
            first_payment_address_set(&effects).expect("payment-address-set");
        assert_eq!(bound, addr);
    }

    let after = {
        let mut st = state.lock().await;
        let r = st
            .app
            .peek(build_payment_address_peek())
            .await
            .expect("peek");
        decode_payment_address(&r).expect("decode")
    };
    assert_eq!(after.as_deref(), Some("second-addr"));
}

#[tokio::test]
async fn set_payment_address_is_frozen_after_first_claim() {
    let (_tmp, state) = boot_kernel().await;

    // Bind initially.
    {
        let mut st = state.lock().await;
        let fx = st
            .app
            .poke(
                SystemWire.to_wire(),
                build_set_payment_address_poke("frozen-addr"),
            )
            .await
            .expect("set");
        assert!(first_payment_address_set(&fx).is_some());
    }

    // Accept a first claim to bump claim-count.
    {
        let mut st = state.lock().await;
        let fx = st
            .app
            .poke(
                SystemWire.to_wire(),
                build_claim_poke("alpha.nock", "owner-zzz", 5000, "tx-freeze-1"),
            )
            .await
            .expect("claim");
        assert!(has_effect(&fx, "claimed"), "first claim should succeed");
    }

    // Further %set-payment-address pokes should error, not mutate.
    let fx = {
        let mut st = state.lock().await;
        st.app
            .poke(
                SystemWire.to_wire(),
                build_set_payment_address_poke("should-be-rejected"),
            )
            .await
            .expect("second set")
    };
    assert!(first_payment_address_set(&fx).is_none());
    let err = first_error_message(&fx).expect("payment-address-error");
    assert!(err.contains("already bound"), "expected freeze error, got: {err}");

    let view = {
        let mut st = state.lock().await;
        let r = st
            .app
            .peek(build_payment_address_peek())
            .await
            .expect("peek");
        decode_payment_address(&r).expect("decode")
    };
    assert_eq!(view.as_deref(), Some("frozen-addr"));
}
