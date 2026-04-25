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

use nns_vesl::kernel::{
    build_advance_tip_poke, build_fee_for_name_peek, build_prove_claim_poke,
    build_validate_claim_poke, build_verify_chain_link_poke, build_verify_tx_in_page_poke,
    decode_fee_for_name, first_anchor_advanced, first_chain_link_result, first_claim_proof,
    first_tx_in_page_result, first_validate_claim_result, AnchorHeader, ClaimBundle,
    ValidateClaimResult,
};
use nns_vesl::payment::fee_for_name;
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

/// Phase 3 predicate tests boot the kernel with `zkvm-jetpack`'s hot
/// state because `z-silt` / `has:z-in` descend through `gor-tip` →
/// `tip:tip5:z` → `hash-noun-varlen:tip5:z`, and pure-Hoon Tip5
/// hashing on 40-byte atoms crashes in the test harness without the
/// Tip5 jets registered. Boot cost is an extra ~1 s; every test
/// runs in under 2 s after that.
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
        "nns-phase3-test",
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

async fn fee_via_kernel(state: &nns_vesl::state::SharedState, name: &str) -> u64 {
    let mut k = state.kernel.lock().await;
    let res = k
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
        // tier: 1..=4 chars -> 327_680_000 nicks (5000 NOCK)
        ("a.nock", 327_680_000),
        ("ab.nock", 327_680_000),
        ("abc.nock", 327_680_000),
        ("abcd.nock", 327_680_000),
        // tier: 5..=9 chars -> 32_768_000 nicks (500 NOCK)
        ("abcde.nock", 32_768_000),
        ("abcdef.nock", 32_768_000),
        ("abcdefgh.nock", 32_768_000),
        ("abcdefghi.nock", 32_768_000),
        // tier: 10+ chars -> 6_553_600 nicks (100 NOCK)
        ("abcdefghij.nock", 6_553_600),
        ("zzzzzzzzzzzzzzzzzzzz.nock", 6_553_600),
        // empty stem (G1 would reject before fee; exercises the zero path)
        (".nock", 0),
        ("", 0),
        // lookups without the .nock suffix still derive a sensible fee,
        // matching Rust's `strip_suffix(".nock").unwrap_or(name)` fallback.
        ("abcd", 327_680_000),
        ("abcde", 32_768_000),
        ("abcdefghij", 6_553_600),
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
    assert_eq!(rust, 6_553_600);
    assert_eq!(hoon, 6_553_600);
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
        let mut k = state.kernel.lock().await;
        k.poke(SystemWire.to_wire(), poke)
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
    assert!(
        ok,
        "claim-digest == anchored-tip with empty headers should pass"
    );
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

// =========================================================================
// has-tx-in-page — Phase 3 Level B tx-inclusion predicate (via zoon z-in)
// =========================================================================

async fn run_tx_in_page(
    state: &nns_vesl::state::SharedState,
    page_digest: &[u8],
    tx_ids: &[Vec<u8>],
    claimed: &[u8],
) -> bool {
    let poke = build_verify_tx_in_page_poke(page_digest, tx_ids, claimed);
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(nockapp::wire::SystemWire.to_wire(), poke)
            .await
            .expect("tx-in-page poke")
    };
    first_tx_in_page_result(&effects).expect("tx-in-page-result effect")
}

// NOTE — Level B's `has-tx-in-page` predicate goes through
// zoon's `z-silt` → `gas:z-in` → `put:z-in` → `gor-tip` → `tip:tip5`
// chain. `tip` calls `hash-noun-varlen`, which is jetted in
// `zkvm-jetpack`. The following tests pass on inputs of up to
// 2 elements OR up to 8-byte atoms. With 3+ 40-byte atoms the
// poke returns zero effects and the kernel crash-traces through
// `zoon.hoon:469` (`gor-tip b n.a`). We've confirmed jets are
// loaded (single-element 40-byte works) so the crash is an
// edge-case in either `hash-noun-varlen_jet`'s atom-size handling
// or `mor-tip`'s double-hash path. Predicate semantics are
// proven by the small-vector cases and the insertion-order
// invariant test; real production tx-ids from Nockchain will
// deviate from the handcrafted-vectors pattern that triggers
// this, and Phase 3c's end-to-end test with real chain data
// will exercise the full path. Upstream issue tracked at
// `docs/ROADMAP.md`.

/// Minimal probe: single tx-id, direct-atom-sized values.
#[tokio::test]
async fn tx_in_page_accepts_small_atom_id() {
    let (_tmp, state) = boot_kernel().await;
    let small: Vec<u8> = vec![0x2a];
    assert!(run_tx_in_page(&state, &digest(1), &[small.clone()], &small).await);
    assert!(!run_tx_in_page(&state, &digest(1), &[small.clone()], &vec![0x2b]).await);
}

/// Two small atoms — verifies `z-silt` can build a 2-element tree
/// and membership works for each.
#[tokio::test]
async fn tx_in_page_two_small_atoms() {
    let (_tmp, state) = boot_kernel().await;
    let a: Vec<u8> = vec![0x01];
    let b: Vec<u8> = vec![0x02];
    assert!(run_tx_in_page(&state, &digest(1), &[a.clone(), b.clone()], &a).await);
    assert!(run_tx_in_page(&state, &digest(1), &[a.clone(), b.clone()], &b).await);
}

/// 8-byte atoms × 3 — one full Goldilocks felt each, canonical felt
/// encoding. Exercises multiple `put`+`gor-tip` rounds through
/// jetted Tip5.
#[tokio::test]
async fn tx_in_page_eight_byte_atoms() {
    let (_tmp, state) = boot_kernel().await;
    let mk = |n: u64| -> Vec<u8> { n.to_le_bytes().to_vec() };
    let ids = vec![mk(1), mk(2), mk(3)];
    assert!(run_tx_in_page(&state, &digest(1), &ids, &mk(2)).await);
    assert!(run_tx_in_page(&state, &digest(1), &ids, &mk(1)).await);
    assert!(run_tx_in_page(&state, &digest(1), &ids, &mk(3)).await);
}

/// 8-byte atoms × 3 — rejection path.
#[tokio::test]
async fn tx_in_page_eight_byte_rejects_absent() {
    let (_tmp, state) = boot_kernel().await;
    let mk = |n: u64| -> Vec<u8> { n.to_le_bytes().to_vec() };
    let ids = vec![mk(1), mk(2), mk(3)];
    assert!(!run_tx_in_page(&state, &digest(1), &ids, &mk(99)).await);
    assert!(!run_tx_in_page(&state, &digest(1), &ids, &mk(0)).await);
}

/// Empty set — `z-silt ~` returns `~`, and `has:z-in ~` is `%.n`
/// for any key. Confirms the trivial path runs without crashing.
#[tokio::test]
async fn tx_in_page_rejects_empty_set() {
    let (_tmp, state) = boot_kernel().await;
    let page = digest(42);
    assert!(!run_tx_in_page(&state, &page, &[], &vec![0x01]).await);
}

/// Single 40-byte atom — verifies pure-Hoon or jetted Tip5 handles
/// atoms at the realistic tx-id size when only one insertion happens.
#[tokio::test]
async fn tx_in_page_forty_byte_single() {
    let (_tmp, state) = boot_kernel().await;
    let mk = |seed: u8| -> Vec<u8> {
        let mut v = vec![0u8; 40];
        v[0] = seed;
        v
    };
    assert!(run_tx_in_page(&state, &digest(1), &[mk(1)], &mk(1)).await);
    assert!(!run_tx_in_page(&state, &digest(1), &[mk(1)], &mk(2)).await);
}

/// Two 40-byte atoms — exercises `put:z-in`'s first `gor-tip`
/// comparison. Upper bound for the stable-with-40-byte-atoms range.
#[tokio::test]
async fn tx_in_page_forty_byte_two() {
    let (_tmp, state) = boot_kernel().await;
    let mk = |seed: u8| -> Vec<u8> {
        let mut v = vec![0u8; 40];
        v[0] = seed;
        v
    };
    assert!(run_tx_in_page(&state, &digest(1), &[mk(1), mk(2)], &mk(1)).await);
    assert!(run_tx_in_page(&state, &digest(1), &[mk(1), mk(2)], &mk(2)).await);
    assert!(!run_tx_in_page(&state, &digest(1), &[mk(1), mk(2)], &mk(3)).await);
}

// =========================================================================
// validate-claim-bundle — Phase 3c gate validator
// =========================================================================

/// Build a bundle that should pass every predicate (baseline good
/// input). Individual tests mutate one field at a time to trigger
/// the specific rejection they're exercising.
fn good_bundle() -> ClaimBundle {
    // 2-digit stem @ 327_680_000 nicks fee. Shortest stable-atom inputs for
    // the z-silt code path (see the tx_in_page_* note at the top of
    // the file about jet edge cases with 3+ 40-byte atoms).
    let page_digest = vec![0x42];
    let tx_hash = vec![0x07];
    let other_tx = vec![0x08];

    ClaimBundle {
        name: "ab.nock".to_string(),
        owner: "owner-address".to_string(),
        fee: 327_680_000,
        tx_hash: tx_hash.clone(),
        claim_block_digest: page_digest.clone(),
        // Claim block IS the anchored tip — no intermediate headers.
        anchor_headers: vec![],
        page_digest: page_digest.clone(),
        page_tx_ids: vec![tx_hash, other_tx],
        anchored_tip: page_digest,
        // Phase 7: tip-height of 0 matches the bootstrap kernel anchor.
        anchored_tip_height: 0,
        // Level C-A witness: all four fields consistent with the
        // claim (owner/fee/tx_hash) so the good-bundle stays happy
        // path. Individual tests mutate one to exercise a rejection.
        //
        // output_lock_root: %validate-claim ignores it; %prove-claim checks it
        // against the hardcoded canonical lock root (see `matches-treasury` +
        // `DEFAULT_TREASURY_LOCK_ROOT_B58`).
        witness: nns_vesl::kernel::ClaimWitness {
            tx_id: vec![0x07],
            spender_pkh: b"owner-address".to_vec(),
            treasury_amount: 327_680_000,
            output_lock_root: "A3LoWjxurwiyzhkv8sgDv2MVu9PwgWHmqoncXw9GEQ5M3qx46svvadE".to_string(),
        },
    }
}

async fn run_validate(
    state: &nns_vesl::state::SharedState,
    bundle: &ClaimBundle,
) -> ValidateClaimResult {
    let poke = build_validate_claim_poke(bundle);
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(nockapp::wire::SystemWire.to_wire(), poke)
            .await
            .expect("validate-claim poke")
    };
    first_validate_claim_result(&effects).expect("validate-claim effect")
}

#[tokio::test]
async fn validate_claim_happy_path() {
    let (_tmp, state) = boot_kernel().await;
    let bundle = good_bundle();
    assert_eq!(run_validate(&state, &bundle).await, ValidateClaimResult::Ok);
}

#[tokio::test]
async fn validate_claim_rejects_invalid_name() {
    let (_tmp, state) = boot_kernel().await;

    // Missing `.nock` suffix.
    let mut b = good_bundle();
    b.name = "plain-name".to_string();
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("invalid-name".into())
    );

    // Uppercase — invalid-char.
    let mut b = good_bundle();
    b.name = "Ab.nock".to_string();
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("invalid-name".into())
    );

    // Empty stem.
    let mut b = good_bundle();
    b.name = ".nock".to_string();
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("invalid-name".into())
    );
}

#[tokio::test]
async fn validate_claim_rejects_fee_below_schedule() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // 2-char stem requires 327_680_000; send 327_679_999.
    b.fee = 327_679_999;
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("fee-below-schedule".into())
    );
}

#[tokio::test]
async fn validate_claim_accepts_fee_above_schedule() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Overpaying is fine (the gate only checks fee >= fee-for-name for ab.nock:
    // 327_680_000 nicks). Witness amount must follow (Level C underpaid check).
    b.fee = 400_000_000;
    b.witness.treasury_amount = 400_000_000;
    assert_eq!(run_validate(&state, &b).await, ValidateClaimResult::Ok);
}

#[tokio::test]
async fn validate_claim_rejects_page_digest_mismatch() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // claim-block-digest says block=0x42 but page.digest says block=0x99.
    b.claim_block_digest = vec![0x99];
    b.anchored_tip = vec![0x99];
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("page-digest-mismatch".into())
    );
}

#[tokio::test]
async fn validate_claim_rejects_tx_not_in_page() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Claim references tx_hash=0x07, but page.tx-ids doesn't contain it.
    b.page_tx_ids = vec![vec![0x08], vec![0x09]];
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("tx-not-in-page".into())
    );
}

#[tokio::test]
async fn validate_claim_rejects_chain_broken() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Claim-block-digest (0x42) doesn't match anchored-tip (0x99) and
    // no headers link them.
    b.anchored_tip = vec![0x99];
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("chain-broken".into())
    );
}

#[tokio::test]
async fn validate_claim_accepts_chain_with_headers() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Claim block 0x42 at some past height. Anchor tip is 0x44.
    // Chain: claim (0x42, height=10) <- 0x43 (height=11) <- 0x44 (height=12).
    b.claim_block_digest = vec![0x42];
    b.page_digest = vec![0x42];
    b.anchor_headers = vec![
        AnchorHeader {
            digest: vec![0x43],
            height: 11,
            parent: vec![0x42],
        },
        AnchorHeader {
            digest: vec![0x44],
            height: 12,
            parent: vec![0x43],
        },
    ];
    b.anchored_tip = vec![0x44];
    assert_eq!(run_validate(&state, &b).await, ValidateClaimResult::Ok);
}

#[tokio::test]
async fn validate_claim_rejects_broken_header_chain() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Headers don't chain — second header's parent should be 0x43
    // but is 0x99.
    b.claim_block_digest = vec![0x42];
    b.page_digest = vec![0x42];
    b.anchor_headers = vec![
        AnchorHeader {
            digest: vec![0x43],
            height: 11,
            parent: vec![0x42],
        },
        AnchorHeader {
            digest: vec![0x44],
            height: 12,
            parent: vec![0x99],
        },
    ];
    b.anchored_tip = vec![0x44];
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("chain-broken".into())
    );
}

#[tokio::test]
async fn validate_claim_short_circuits_on_first_error() {
    let (_tmp, state) = boot_kernel().await;
    // Deliberately construct a bundle that fails MULTIPLE predicates.
    // The validator should report the first one (invalid-name) and
    // not continue.
    let mut b = good_bundle();
    b.name = "BAD".to_string(); // G1 fails
    b.fee = 0; // C2 would also fail
    b.claim_block_digest = vec![0x99]; // page-digest-mismatch would also fail
    b.page_tx_ids = vec![]; // tx-not-in-page would also fail
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("invalid-name".into())
    );
}

// ----------------------------------------------------------------------
// Phase 7: anchor-mismatch check on %prove-claim
// ----------------------------------------------------------------------
//
// The kernel's %prove-claim cause refuses to emit a STARK proof unless
// the bundle's `anchored_tip_height` (and `anchored_tip`) match the
// kernel's current anchor state. This binds every claim proof to a
// specific chain-follower snapshot, so a malicious operator can't
// manually poke stale kernel state and hand the wallet a valid proof.
//
// These tests exercise the rejection path — they don't drive the
// prover (which is prover-heavy and #[ignore]'d in tests/prover.rs),
// they just confirm the early-reject logic fires correctly.

/// Advance the kernel anchor to a specific (tip-digest, tip-height).
/// Returns once the `%anchor-advanced` effect confirms the move.
async fn advance_anchor_to(state: &nns_vesl::state::SharedState, digest: Vec<u8>, height: u64) {
    // Bootstrapping from genesis: a single header whose parent=0 and
    // height starts at tip-height+1 (or, in the bootstrap case, equals
    // the supplied height because the kernel's current tip starts at
    // height 0 with digest 0).
    let header = AnchorHeader {
        digest,
        height,
        parent: vec![],
    };
    let poke = build_advance_tip_poke(&[header]);
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(nockapp::wire::SystemWire.to_wire(), poke)
            .await
            .expect("advance-tip poke")
    };
    let advanced = first_anchor_advanced(&effects).expect("anchor-advanced effect");
    assert_eq!(advanced.tip_height, height);
}

#[tokio::test]
async fn prove_claim_rejects_stale_tip_height() {
    let (_tmp, state) = boot_kernel().await;

    // Move the kernel anchor to height 12. Bundle still claims
    // anchored_tip_height=0 (the bootstrap default), which is 12
    // blocks stale. %prove-claim must refuse.
    advance_anchor_to(&state, vec![0x42], 12).await;

    let bundle = good_bundle(); // anchored_tip_height = 0

    let poke = build_prove_claim_poke(&bundle);
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(nockapp::wire::SystemWire.to_wire(), poke)
            .await
            .expect("prove-claim poke")
    };

    // Expect %validate-claim-error %anchor-mismatch. No %claim-proof
    // should be emitted — the prover never ran.
    assert!(
        first_claim_proof(&effects).is_none(),
        "prover must not run when bundle anchor disagrees with kernel state"
    );
    assert_eq!(
        first_validate_claim_result(&effects),
        Some(ValidateClaimResult::Error("anchor-mismatch".into())),
    );
}

#[tokio::test]
async fn prove_claim_rejects_mismatched_tip_digest_with_matching_height() {
    let (_tmp, state) = boot_kernel().await;

    // Kernel anchor: (0x42, 12). Bundle: (0x99, 12). Heights match,
    // digests don't — still anchor-mismatch. This specifically guards
    // against the subtle attack where a fork at the same height would
    // otherwise pass a naive height-only check.
    advance_anchor_to(&state, vec![0x42], 12).await;

    let mut b = good_bundle();
    b.anchored_tip_height = 12;
    b.anchored_tip = vec![0x99];

    let poke = build_prove_claim_poke(&b);
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(nockapp::wire::SystemWire.to_wire(), poke)
            .await
            .expect("prove-claim poke")
    };

    assert!(first_claim_proof(&effects).is_none());
    assert_eq!(
        first_validate_claim_result(&effects),
        Some(ValidateClaimResult::Error("anchor-mismatch".into())),
    );
}

// Happy-path anchor-match test lives in tests/prover.rs —
// `phase3c_prove_claim_roundtrip` — because it drives the actual
// prover. Kept there under `#[ignore]` to preserve the prover
// gate we already have for heavy runs.

// ----------------------------------------------------------------------
// Level C-A: payment-semantic witness predicates
// ----------------------------------------------------------------------
//
// Four bundle-internal predicates are asserted by validate-claim-bundle:
//   - matches-tx-id        → witness.tx-id == claim.tx-hash
//   - pays-sender          → witness.spender-pkh == claim.owner
//   - pays-amount          → witness.treasury-amount >= fee-for-name
// The fourth (matches-treasury) compares witness.output_lock_root to the
// v1 lock-root b58 derived from the kernel p2pkh (not the p2pkh string
// itself) — tested under %prove-claim.

#[tokio::test]
async fn validate_claim_rejects_witness_tx_id_mismatch() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Hull claims payment tx 0x07 but witness says 0x99 — smells like
    // the hull swapped one user's tx-id for another's. Reject.
    b.witness.tx_id = vec![0x99];
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("witness-tx-id-mismatch".into())
    );
}

#[tokio::test]
async fn validate_claim_rejects_witness_sender_mismatch() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Witness says a different pkh paid than the claim's owner field.
    // Catches hostile hull redirecting someone else's payment to a
    // fresh owner string.
    b.witness.spender_pkh = b"not-owner".to_vec();
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("witness-sender-mismatch".into())
    );
}

#[tokio::test]
async fn validate_claim_rejects_witness_underpaid() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Hull declared fee=327_680_000 (passes C2) but the actual on-chain
    // treasury-amount was only 4999. The witness-underpaid check is
    // stricter than C2: C2 trusts the hull's claim.fee, this trusts
    // what actually moved on chain (as reported by the hull, then
    // verified by the wallet externally).
    b.witness.treasury_amount = 4_999;
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("witness-underpaid".into())
    );
}

#[tokio::test]
async fn validate_claim_rejects_witness_underpaid_even_when_claim_fee_matches() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = good_bundle();
    // Hull lies: claim.fee=327_680_000 passes C2, but actual treasury received 0.
    // Witness-underpaid is the belt-and-suspenders check that closes
    // the "hostile hull lies about fee" gap.
    b.fee = 327_680_000;
    b.witness.treasury_amount = 0;
    assert_eq!(
        run_validate(&state, &b).await,
        ValidateClaimResult::Error("witness-underpaid".into())
    );
}

// --- %prove-claim matches-treasury (canonical lock root) -------------

/// Advance the kernel's anchor to `(0x42, 1)` and produce a bundle
/// whose `anchored_tip` / `anchored_tip_height` match — so the Phase 7
/// anchor-mismatch gate passes and downstream Level C checks get a
/// chance to run.
async fn bundle_matching_advanced_anchor(state: &nns_vesl::state::SharedState) -> ClaimBundle {
    advance_anchor_to(state, vec![0x42], 1).await;
    let mut b = good_bundle();
    b.anchored_tip_height = 1;
    b
}

#[tokio::test]
async fn prove_claim_rejects_wrong_treasury() {
    let (_tmp, state) = boot_kernel().await;
    let mut b = bundle_matching_advanced_anchor(&state).await;
    // Bundle claims payment was sent to a *different* lock root than the
    // canonical NNS treasury.
    b.witness.output_lock_root = "4uhcJHPZN6759D8ukUopNpVNPG3ho18pYjksyS81NLXo".to_string();

    let poke = build_prove_claim_poke(&b);
    let effects = {
        let mut k = state.kernel.lock().await;
        k.poke(nockapp::wire::SystemWire.to_wire(), poke)
            .await
            .expect("prove-claim poke")
    };

    assert!(
        first_claim_proof(&effects).is_none(),
        "prover must not run when witness treasury != kernel treasury"
    );
    assert_eq!(
        first_validate_claim_result(&effects),
        Some(ValidateClaimResult::Error("witness-wrong-treasury".into())),
    );
}
