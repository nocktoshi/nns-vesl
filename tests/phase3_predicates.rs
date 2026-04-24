//! Phase 3 Level A predicate tests.
//!
//! Pins two invariants that Phase 3c's recursive gate will depend on:
//!
//!   - `fee-for-name:nns-predicates` in Hoon matches
//!     `payment::fee_for_name` in Rust, bit-for-bit, across the full
//!     fee-tier boundary surface. If either side drifts, Phase 3's
//!     C2 check inside the gate will start rejecting claims the hull
//!     pre-approved (or accepting ones it should have rejected).
//!
//!   - `chain-links-to:nns-predicates` accepts a valid
//!     `AnchorHeader` chain from a claim digest to the follower's
//!     anchored tip, and rejects every failure mode (parent break,
//!     height gap, wrong tip, empty chain that doesn't terminate at
//!     the tip).
//!
//! These are pure-kernel peeks driven from Rust — no STARK prove, no
//! tx-engine dependency, so they run in seconds on every
//! `cargo test`.

use std::sync::Arc;

use nockapp::kernel::boot;
use nockapp::wire::{SystemWire, Wire};
use nockapp::NockApp;
use nns_vesl::kernel::{
    build_fee_for_name_peek, build_verify_chain_link_poke, decode_fee_for_name,
    first_chain_link_result, AnchorHeader,
};
use nns_vesl::payment::fee_for_name;
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
        "nns-phase3-test",
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

async fn fee_via_kernel(
    state: &nns_vesl::state::SharedState,
    name: &str,
) -> u64 {
    let mut st = state.lock().await;
    let res = st
        .app
        .peek(build_fee_for_name_peek(name))
        .await
        .expect("fee peek");
    decode_fee_for_name(&res).expect("decode fee")
}

/// Cross-repo parity: every Rust `payment::fee_for_name` input must
/// return the same u64 from the kernel's `fee-for-name:nns-predicates`.
///
/// Covers each tier plus boundaries, names with and without the
/// `.nock` suffix, and the "too-long" bucket.
#[tokio::test]
async fn fee_for_name_parity_hoon_rust() {
    let (_tmp, state) = boot_kernel().await;

    let cases = [
        // tier: 1..=4 chars -> 5000
        ("a.nock", 5_000),
        ("ab.nock", 5_000),
        ("abc.nock", 5_000),
        ("abcd.nock", 5_000),
        // tier: 5..=9 chars -> 500
        ("abcde.nock", 500),
        ("abcdef.nock", 500),
        ("abcdefgh.nock", 500),
        ("abcdefghi.nock", 500),
        // tier: 10+ chars -> 100
        ("abcdefghij.nock", 100),
        ("zzzzzzzzzzzzzzzzzzzz.nock", 100),
        // empty stem (G1 would reject before fee; exercises the zero path)
        (".nock", 0),
        ("", 0),
        // lookups without the .nock suffix still derive a sensible fee,
        // matching Rust's `strip_suffix(".nock").unwrap_or(name)` fallback.
        ("abcd", 5_000),
        ("abcde", 500),
        ("abcdefghij", 100),
    ];

    for (name, expected) in cases {
        let rust = fee_for_name(name);
        let hoon = fee_via_kernel(&state, name).await;
        assert_eq!(
            rust, expected,
            "Rust fee_for_name({name:?}) = {rust}, expected {expected}",
        );
        assert_eq!(
            hoon, expected,
            "Hoon /fee-for-name/{name:?} = {hoon}, expected {expected}",
        );
        assert_eq!(
            rust, hoon,
            "Hoon/Rust fee drift on {name:?}: Rust={rust} Hoon={hoon}",
        );
    }
}

/// Sanity: long names (100+ bytes) don't blow up the peek path.
#[tokio::test]
async fn fee_for_name_accepts_long_names() {
    let (_tmp, state) = boot_kernel().await;
    let long_name = format!("{}.nock", "z".repeat(200));
    let rust = fee_for_name(&long_name);
    let hoon = fee_via_kernel(&state, &long_name).await;
    assert_eq!(rust, 100);
    assert_eq!(hoon, 100);
}

// =========================================================================
// chain-links-to — Phase 3 Level A header-chain walker
// =========================================================================

/// Synthetic 40-byte digest built from a seed; lets us reason about
/// parent/digest relationships in tests without a real Tip5 hash.
fn digest(seed: u8) -> Vec<u8> {
    vec![seed; 40]
}

fn hdr(height: u64, seed: u8, parent_seed: u8) -> AnchorHeader {
    AnchorHeader {
        digest: digest(seed),
        height,
        parent: digest(parent_seed),
    }
}

async fn run_chain_link(
    state: &nns_vesl::state::SharedState,
    claim_digest: &[u8],
    headers: &[AnchorHeader],
    anchored_tip: &[u8],
) -> bool {
    let poke = build_verify_chain_link_poke(claim_digest, headers, anchored_tip);
    let effects = {
        let mut st = state.lock().await;
        st.app
            .poke(SystemWire.to_wire(), poke)
            .await
            .expect("chain-link poke")
    };
    first_chain_link_result(&effects).expect("chain-link-result effect")
}

/// Empty header list: claim's own block IS the anchored tip.
#[tokio::test]
async fn chain_link_accepts_claim_is_tip() {
    let (_tmp, state) = boot_kernel().await;
    let ok = run_chain_link(&state, &digest(7), &[], &digest(7)).await;
    assert!(ok, "claim-digest == anchored-tip with empty headers should pass");
}

/// Empty headers + different tip must fail.
#[tokio::test]
async fn chain_link_rejects_empty_chain_wrong_tip() {
    let (_tmp, state) = boot_kernel().await;
    let ok = run_chain_link(&state, &digest(7), &[], &digest(9)).await;
    assert!(!ok, "empty chain from claim=7 to tip=9 must be rejected");
}

/// Happy path: claim at digest=1, chain [2<-1, 3<-2, 4<-3], tip=4.
#[tokio::test]
async fn chain_link_accepts_three_header_chain() {
    let (_tmp, state) = boot_kernel().await;
    let headers = vec![hdr(2, 2, 1), hdr(3, 3, 2), hdr(4, 4, 3)];
    let ok = run_chain_link(&state, &digest(1), &headers, &digest(4)).await;
    assert!(ok);
}

/// First header's parent doesn't match claim-digest.
#[tokio::test]
async fn chain_link_rejects_first_parent_mismatch() {
    let (_tmp, state) = boot_kernel().await;
    let headers = vec![hdr(2, 2, 99)]; // parent = 99, not claim-digest 1
    let ok = run_chain_link(&state, &digest(1), &headers, &digest(2)).await;
    assert!(!ok);
}

/// Internal link break: [2<-1, 3<-99] — second header doesn't chain
/// to first.
#[tokio::test]
async fn chain_link_rejects_internal_break() {
    let (_tmp, state) = boot_kernel().await;
    let headers = vec![hdr(2, 2, 1), hdr(3, 3, 99)];
    let ok = run_chain_link(&state, &digest(1), &headers, &digest(3)).await;
    assert!(!ok);
}

/// Height gap: [2<-1, 5<-2] — height jumps from 2 to 5 instead of 3.
#[tokio::test]
async fn chain_link_rejects_height_gap() {
    let (_tmp, state) = boot_kernel().await;
    let headers = vec![hdr(2, 2, 1), hdr(5, 3, 2)];
    let ok = run_chain_link(&state, &digest(1), &headers, &digest(3)).await;
    assert!(!ok);
}

/// Final digest != anchored tip.
#[tokio::test]
async fn chain_link_rejects_wrong_final_tip() {
    let (_tmp, state) = boot_kernel().await;
    let headers = vec![hdr(2, 2, 1), hdr(3, 3, 2)];
    let ok = run_chain_link(&state, &digest(1), &headers, &digest(99)).await;
    assert!(!ok);
}
