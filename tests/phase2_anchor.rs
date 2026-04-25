//! Phase 2 integration tests — kernel anchor behaviour.
//!
//! Exercises the new causes landed in Phase 2a against a real booted
//! kernel, without the HTTP layer. Covers:
//!
//!   - `%advance-tip` bootstrap, extend, and reorg rejection paths
//!   - `/anchor` peek roundtrip
//!
//! These are `#[tokio::test]`s (not `#[ignore]`) — the kernel boots in
//! under a second with prover jets disabled, so they run on every
//! `cargo test`.

use std::sync::Arc;

use nns_vesl::kernel::{
    build_advance_tip_poke, build_anchor_peek, decode_anchor, first_anchor_advanced,
    first_error_message, AnchorHeader,
};
use nns_vesl::state::AppState;
use nockapp::kernel::boot;
use nockapp::wire::{SystemWire, Wire};
use nockapp::NockApp;
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
    let state = Arc::new(AppState::new(
        app,
        tmp.path().to_path_buf(),
        SettlementConfig::local(),
    ));
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

    let headers = vec![header_at(1, 1, 0), header_at(2, 2, 1), header_at(3, 3, 2)];

    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&headers))
            .await
            .expect("advance-tip poke")
    };

    let advanced = first_anchor_advanced(&effects).expect("anchor-advanced effect");
    assert_eq!(advanced.tip_height, 3);
    assert_eq!(advanced.tip_digest, vec![3u8; 40]);
    assert_eq!(advanced.count, 3);
    assert!(first_error_message(&effects).is_none());

    let view = {
        let mut k = state.kernel.lock().await;
        let r = k.peek(build_anchor_peek()).await.expect("anchor peek");
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
async fn advance_tip_bootstrap_accepts_non_zero_parent() {
    // Phase 2c jump-to-tip bootstrap fix: the follower can seed the
    // anchor with a header at arbitrary height whose parent is NOT
    // 0x0. Required for mainnet, where walking from genesis would
    // time out public RPCs long before completing.
    //
    // Trust model: operator is trusted for the bootstrap seed;
    // wallets re-verify `(tip_digest, tip_height)` against their
    // own canonical chain view (Phase 7). Pre-fix, the kernel
    // required `parent=0x0` on first header, which forced a
    // from-genesis walk and broke on mainnet.
    let (_tmp, state) = boot_kernel().await;

    // Single header at height 12345 with a random non-zero parent —
    // simulating the follower jumping straight to `tip - finality_depth`.
    let headers = vec![header_at(12_345, 0xAB, 0xCD)];

    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&headers))
            .await
            .expect("advance-tip poke")
    };

    let advanced = first_anchor_advanced(&effects).expect("anchor-advanced effect");
    assert_eq!(advanced.tip_height, 12_345);
    assert_eq!(advanced.tip_digest, vec![0xABu8; 40]);
    assert_eq!(advanced.count, 1);
    assert!(
        first_error_message(&effects).is_none(),
        "bootstrap with non-zero parent must not emit %anchor-error",
    );
}

#[tokio::test]
async fn advance_tip_extends_after_bootstrap() {
    let (_tmp, state) = boot_kernel().await;

    // Bootstrap to height=2.
    let boot_headers = vec![header_at(1, 1, 0), header_at(2, 2, 1)];
    {
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&boot_headers))
            .await
            .expect("bootstrap");
    }

    // Extend by two blocks whose parent chains back to the current tip.
    let more = vec![header_at(3, 3, 2), header_at(4, 4, 3)];
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&more))
            .await
            .expect("extend")
    };

    let advanced = first_anchor_advanced(&effects).expect("anchor-advanced effect");
    assert_eq!(advanced.tip_height, 4);
    assert_eq!(advanced.count, 2);

    let view = {
        let mut k = state.kernel.lock().await;
        let r = k.peek(build_anchor_peek()).await.expect("anchor peek");
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
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&boot_headers))
            .await
            .expect("bootstrap");
    }

    // Try to extend with a header whose parent is NOT the current tip
    // digest. Simulates a reorg the follower missed.
    let bad = vec![header_at(3, 3, 99)];
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&bad))
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
        let mut k = state.kernel.lock().await;
        let r = k.peek(build_anchor_peek()).await.expect("anchor peek");
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
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&boot_headers))
            .await
            .expect("bootstrap");
    }

    // Jump straight from 2 to 5 (skipping 3 and 4). Even with a matched
    // parent digest the kernel requires strict height+1 linkage.
    let gap = vec![header_at(5, 5, 2)];
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&gap))
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
    let broken = vec![header_at(1, 1, 0), header_at(2, 2, 1), header_at(3, 3, 99)];
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&broken))
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
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), build_advance_tip_poke(&[]))
            .await
            .expect("empty advance")
    };
    let err = first_error_message(&effects).expect("anchor-error");
    assert!(
        err.contains("empty"),
        "expected empty-advance error, got: {err}"
    );
}
