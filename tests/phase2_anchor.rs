//! Path Y2 integration tests — `%scan-block` chain cursor.
//!
//! Replaces the old Phase 2 `%advance-tip` suite (removed in Path Y).
//! Exercises parent linkage, strict height+1 monotonicity, and
//! `/scan-state` peeks against a booted kernel (no HTTP).

use std::sync::Arc;

use nns_vesl::kernel::{
    build_scan_block_poke, build_scan_state_peek, decode_scan_state, first_scan_block_done,
    first_scan_block_error,
};
use nns_vesl::state::AppState;
use nockapp::kernel::boot;
use nockapp::kernel::boot::NockStackSize;
use nockapp::wire::{SystemWire, Wire};
use nockapp::NockApp;
use vesl_core::SettlementConfig;

fn kernel_jam() -> Vec<u8> {
    let path = std::env::var("NNS_KERNEL_JAM").unwrap_or_else(|_| "out.jam".to_string());
    std::fs::read(&path)
        .or_else(|_| std::fs::read("../out.jam"))
        .unwrap_or_else(|e| panic!("could not read kernel jam at {path} or ../out.jam: {e}"))
}

static TRACING_INIT: std::sync::Once = std::sync::Once::new();

async fn boot_kernel() -> (tempfile::TempDir, nns_vesl::state::SharedState) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut cli = boot::default_boot_cli(true);
    cli.stack_size = NockStackSize::Large;
    TRACING_INIT.call_once(|| {
        let _ = boot::init_default_tracing(&cli);
    });
    let prover_hot_state = zkvm_jetpack::hot::produce_prover_hot_state();
    let app: NockApp = boot::setup(
        &kernel_jam(),
        cli,
        prover_hot_state.as_slice(),
        "nns-scan-block-test",
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

fn digest(seed: u8) -> Vec<u8> {
    vec![seed; 40]
}

async fn peek_scan(state: &nns_vesl::state::SharedState) -> nns_vesl::kernel::ScanState {
    let mut k = state.kernel.lock().await;
    let slab = k
        .peek(build_scan_state_peek())
        .await
        .expect("scan-state peek");
    decode_scan_state(&slab).expect("decode scan-state")
}

#[tokio::test]
async fn scan_block_bootstrap_accepts_height_1() {
    let (_tmp, state) = boot_kernel().await;
    let d1 = digest(1);
    let poke = build_scan_block_poke(&digest(0xCD), 1, &d1, &[], &[]);
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), poke)
            .await
            .expect("scan-block poke")
    };
    let done = first_scan_block_done(&effects).expect("scan-block-done");
    assert_eq!(done.height, 1);
    assert_eq!(done.digest, d1);
    assert!(first_scan_block_error(&effects).is_none());

    let s = peek_scan(&state).await;
    assert_eq!(s.last_proved_height, 1);
    assert_eq!(s.last_proved_digest, d1);
}

#[tokio::test]
async fn scan_block_extends_after_bootstrap() {
    let (_tmp, state) = boot_kernel().await;
    let d1 = digest(1);
    let d2 = digest(2);
    {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_scan_block_poke(&digest(9), 1, &d1, &[], &[]),
        )
        .await
        .expect("first scan");
    }
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_scan_block_poke(&d1, 2, &d2, &[], &[]),
        )
        .await
        .expect("second scan")
    };
    let done = first_scan_block_done(&effects).expect("scan-block-done");
    assert_eq!(done.height, 2);
    assert_eq!(done.digest, d2);

    let s = peek_scan(&state).await;
    assert_eq!(s.last_proved_height, 2);
    assert_eq!(s.last_proved_digest, d2);
}

#[tokio::test]
async fn scan_block_rejects_parent_mismatch() {
    let (_tmp, state) = boot_kernel().await;
    let d1 = digest(1);
    {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_scan_block_poke(&digest(0), 1, &d1, &[], &[]),
        )
        .await
        .expect("bootstrap scan");
    }
    let bad_parent = digest(0x99);
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_scan_block_poke(&bad_parent, 2, &digest(3), &[], &[]),
        )
        .await
        .expect("poke")
    };
    assert!(first_scan_block_done(&effects).is_none());
    let err = first_scan_block_error(&effects).expect("scan-block-error");
    assert!(
        err.contains("parent-mismatch"),
        "expected parent-mismatch, got: {err}"
    );
    let s = peek_scan(&state).await;
    assert_eq!(s.last_proved_height, 1);
    assert_eq!(s.last_proved_digest, d1);
}

#[tokio::test]
async fn scan_block_rejects_height_not_successor() {
    let (_tmp, state) = boot_kernel().await;
    let d1 = digest(1);
    {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_scan_block_poke(&digest(0), 1, &d1, &[], &[]),
        )
        .await
        .expect("bootstrap scan");
    }
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_scan_block_poke(&d1, 3, &digest(3), &[], &[]),
        )
        .await
        .expect("gap poke")
    };
    let err = first_scan_block_error(&effects).expect("scan-block-error");
    assert!(
        err.contains("height-not-successor"),
        "expected height-not-successor, got: {err}"
    );
}

#[tokio::test]
async fn scan_block_rejects_bad_first_height() {
    let (_tmp, state) = boot_kernel().await;
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_scan_block_poke(&digest(0), 2, &digest(2), &[], &[]),
        )
        .await
        .expect("poke")
    };
    let err = first_scan_block_error(&effects).expect("scan-block-error");
    assert!(
        err.contains("height-not-successor"),
        "first block must be height 1: {err}"
    );
}
