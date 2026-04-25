//! Phase 7 two-server stale-proof integration test.
//!
//! Acceptance-criteria item from `ARCHITECTURE.md` §7.6:
//!
//! > **Integration test**: spin up two NNS servers, freeze one
//! > follower, have it issue a stale proof, advance the chain past
//! > the staleness window, assert `light_verify` rejects.
//!
//! This test drives two **in-process** NockApp kernels rather than
//! two separate HTTP servers — the freshness check is kernel-state
//! based, not wire-protocol based, so the simulation is equivalent
//! and ~100× cheaper to run in CI.
//!
//! Scenario:
//!
//! 1. Boot two kernels (A, B).
//! 2. Kernel A advances its anchor to height 120. Kernel B stays
//!    frozen at the bootstrap anchor (`tip_digest=0x0`, height 0).
//! 3. Peek `/anchor` on each — confirms each kernel's view matches
//!    intent.
//! 4. Build a `ProofResponse` envelope for each, carrying its
//!    kernel's anchor as the `anchor` field (this is what the
//!    production `GET /proof` handler does under
//!    `src/api.rs::proof_handler`).
//! 5. Wallet simulates canonical chain at height 130.
//! 6. `light_verify --chain-tip 130 --max-staleness 20`:
//!    - Kernel A's proof: anchor=120, lag=10, within window → OK
//!      (Merkle then fails on the synthetic proof envelope, giving
//!      exit 1 — confirms freshness passed).
//!    - Kernel B's proof: anchor=0, lag=130, past window → exit 2.
//!
//! The two-kernel setup is what distinguishes this from the unit
//! tests in `phase7_light_verify` — a real kernel's anchor state
//! drives the metadata, not crafted JSON.

use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::Arc;

use nockapp::kernel::boot;
use nockapp::kernel::boot::NockStackSize;
use nockapp::wire::{SystemWire, Wire};
use nockapp::NockApp;
use nns_vesl::kernel::{
    build_advance_tip_poke, build_anchor_peek, decode_anchor, first_anchor_advanced,
    AnchorHeader,
};
use nns_vesl::state::{hex_encode, AppState, SharedState};
use nns_vesl::types::{ProofAnchor, ProofResponse, TransitionProofMetadata};
use tokio::sync::Mutex;
use vesl_core::SettlementConfig;

fn kernel_jam() -> Vec<u8> {
    let path = std::env::var("NNS_KERNEL_JAM").unwrap_or_else(|_| "out.jam".to_string());
    std::fs::read(&path)
        .or_else(|_| std::fs::read("../out.jam"))
        .unwrap_or_else(|e| panic!("could not read kernel jam at {path} or ../out.jam: {e}"))
}

static TRACING_INIT: std::sync::Once = std::sync::Once::new();

async fn boot_kernel(name: &str) -> (tempfile::TempDir, SharedState) {
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
        name,
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

async fn advance_to(state: &SharedState, digest: Vec<u8>, height: u64) {
    let header = AnchorHeader {
        digest,
        height,
        parent: vec![],
    };
    let poke = build_advance_tip_poke(&[header]);
    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), poke)
            .await
            .expect("advance-tip poke")
    };
    let advanced = first_anchor_advanced(&effects).expect("anchor-advanced");
    assert_eq!(advanced.tip_height, height);
}

async fn peek_anchor(state: &SharedState) -> (Vec<u8>, u64) {
    let mut st = state.lock().await;
    let slab = st.app.peek(build_anchor_peek()).await.expect("anchor peek");
    let view = decode_anchor(&slab).expect("decode anchor");
    (view.tip_digest, view.tip_height)
}

/// Build the `ProofResponse` JSON shape the production
/// `GET /proof` handler would emit, with the supplied anchor as
/// the Phase 7 freshness metadata. The Merkle portion is
/// synthetic — we pipe nonsense root/proof bytes because this
/// test validates the freshness gate, not Merkle inclusion
/// (which has its own test coverage).
fn synthetic_proof_json(tip_digest: &[u8], tip_height: u64) -> String {
    let p = ProofResponse {
        name: "alice.nock".into(),
        owner: "alice-owner".into(),
        tx_hash: "aa".into(),
        claim_id: 1,
        root: "00".into(),
        hull: "00".into(),
        proof: vec![],
        transition: TransitionProofMetadata {
            mode: "claim-window-anchor".into(),
            settled_claim_id: 0,
        },
        transition_proof: None,
        anchor: Some(ProofAnchor {
            tip_digest: hex_encode(tip_digest),
            tip_height,
        }),
    };
    serde_json::to_string(&p).unwrap()
}

fn run_light_verify(args: &[&str], body: &str) -> i32 {
    let binary = std::env::var("CARGO_BIN_EXE_light_verify")
        .unwrap_or_else(|_| "target/debug/light_verify".to_string());
    let mut child = Command::new(&binary)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn light_verify ({binary}): {e}"));
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(body.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait light_verify");
    output.status.code().unwrap_or(-1)
}

#[tokio::test]
async fn two_servers_freshness_gate() {
    // Server A: real anchor advance to height 120 (representing an
    // honest, caught-up NNS server).
    let (_tmp_a, state_a) = boot_kernel("nns-phase7-a").await;
    advance_to(&state_a, vec![0x42], 120).await;

    // Server B: bootstrap anchor only; follower frozen. This models
    // either a buggy operator who never wires the follower, or a
    // malicious one who stopped their follower to mint stale proofs.
    let (_tmp_b, state_b) = boot_kernel("nns-phase7-b").await;

    // --- Observe each kernel's anchor via the production peek path.
    let (digest_a, height_a) = peek_anchor(&state_a).await;
    let (digest_b, height_b) = peek_anchor(&state_b).await;
    assert_eq!(height_a, 120, "server A must be at 120");
    assert_eq!(height_b, 0, "server B must stay at bootstrap");
    // Digest is padded to 8 bytes (u64 atom representation) on decode.
    // Treat as atoms: strip trailing zeros and compare the leading bytes.
    let mut leading = digest_a.clone();
    while leading.last() == Some(&0) {
        leading.pop();
    }
    assert_eq!(leading, vec![0x42], "server A anchor digest should start with 0x42");

    // --- Wallet side: simulate canonical Nockchain tip at 130.
    const WALLET_CHAIN_TIP: u64 = 130;
    const MAX_STALENESS: u64 = 20; // matches production default

    // --- Server A proof: lag = 130 - 120 = 10, within 20-block window.
    // Freshness passes. Merkle fails on synthetic data → exit 1,
    // which CONFIRMS the freshness gate passed (otherwise we'd see
    // exit 2 before Merkle runs).
    let body_a = synthetic_proof_json(&digest_a, height_a);
    let code_a = run_light_verify(
        &[
            "--chain-tip",
            &WALLET_CHAIN_TIP.to_string(),
            "--max-staleness",
            &MAX_STALENESS.to_string(),
        ],
        &body_a,
    );
    assert_eq!(
        code_a, 1,
        "honest server A's proof must pass freshness, then fail Merkle (synthetic envelope)",
    );

    // --- Server B proof: lag = 130 - 0 = 130, well past window.
    // Freshness rejects with exit 2. Merkle never runs.
    let body_b = synthetic_proof_json(&digest_b, height_b);
    let code_b = run_light_verify(
        &[
            "--chain-tip",
            &WALLET_CHAIN_TIP.to_string(),
            "--max-staleness",
            &MAX_STALENESS.to_string(),
        ],
        &body_b,
    );
    assert_eq!(
        code_b, 2,
        "frozen server B's proof must be rejected by freshness gate (exit 2)",
    );
}

#[tokio::test]
async fn server_catches_up_proofs_become_fresh_again() {
    // Start frozen, prove stale, then catch up, re-peek, confirm
    // fresh. Validates that freshness is keyed on *current* kernel
    // state, not a prior snapshot.
    let (_tmp, state) = boot_kernel("nns-phase7-catchup").await;

    // Initially frozen at height 0.
    let (digest, height) = peek_anchor(&state).await;
    assert_eq!(height, 0);

    let stale_body = synthetic_proof_json(&digest, height);
    assert_eq!(
        run_light_verify(&["--chain-tip", "130", "--max-staleness", "20"], &stale_body),
        2,
        "frozen server produces stale proof (reject)",
    );

    // Catch up to 125 — now within 20 blocks of wallet's tip 130.
    advance_to(&state, vec![0x99], 125).await;

    let (digest2, height2) = peek_anchor(&state).await;
    assert_eq!(height2, 125);

    let fresh_body = synthetic_proof_json(&digest2, height2);
    assert_eq!(
        run_light_verify(&["--chain-tip", "130", "--max-staleness", "20"], &fresh_body),
        1,
        "caught-up server's proof passes freshness (Merkle fails on synthetic data)",
    );
}
