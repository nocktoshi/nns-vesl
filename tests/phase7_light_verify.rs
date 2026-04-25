//! Phase 7 `light_verify` CLI integration test.
//!
//! Drives the `light_verify` binary with crafted `ProofResponse`
//! JSON on stdin and asserts the expected exit code for each
//! freshness / anchor-binding scenario.
//!
//! Merkle inclusion is exercised implicitly but not the focus —
//! these tests pipe minimal proof envelopes whose Merkle portion
//! is trivially valid (empty proof list, root == leaf hash) or
//! deliberately invalid. The point is the Phase 7 freshness logic.

use std::io::Write;
use std::process::{Command, Stdio};

use nns_vesl::types::{ProofAnchor, ProofResponse, TransitionProofMetadata};

fn binary() -> String {
    std::env::var("CARGO_BIN_EXE_light_verify")
        .unwrap_or_else(|_| "target/debug/light_verify".to_string())
}

/// Run the binary with `args`, feeding `stdin_json` on stdin.
/// Returns (exit_code, stdout, stderr).
fn run(args: &[&str], stdin_json: &str) -> (i32, String, String) {
    let mut child = Command::new(binary())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn light_verify");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin_json.as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait light_verify");
    let code = output.status.code().unwrap_or(-1);
    let out = String::from_utf8_lossy(&output.stdout).to_string();
    let err = String::from_utf8_lossy(&output.stderr).to_string();
    (code, out, err)
}

/// Minimal proof envelope — Merkle verification will fail on any
/// realistic hash, which is fine for tests that exercise earlier
/// (exit-code-2-and-up) or later (exit-code-0) paths.
fn synthetic_proof(anchor: Option<ProofAnchor>) -> String {
    let p = ProofResponse {
        name: "alice.nock".into(),
        owner: "alice-owner".into(),
        tx_hash: "aa".into(),
        claim_id: 1,
        root: "00".into(),
        hull: "00".into(),
        proof: vec![],
        transition: TransitionProofMetadata {
            mode: "x".into(),
            settled_claim_id: 0,
        },
        transition_proof: None,
        anchor,
    };
    serde_json::to_string(&p).unwrap()
}

#[test]
fn rejects_stale_proof_with_exit_2() {
    // Proof anchored at height 100, wallet sees tip 130, staleness=20.
    // Lag 30 > 20 → reject with "freshness check failed" (exit 2).
    let body = synthetic_proof(Some(ProofAnchor {
        tip_digest: "42".into(),
        tip_height: 100,
    }));
    let (code, _out, err) = run(
        &["--chain-tip", "130", "--max-staleness", "20"],
        &body,
    );
    assert_eq!(code, 2, "stale proof must exit 2 (stderr: {err})");
    assert!(
        err.contains("freshness check failed"),
        "stderr should name the failure: {err}"
    );
}

#[test]
fn accepts_at_exact_freshness_boundary() {
    // Proof at 100, wallet at 120, staleness=20 → lag exactly 20 → OK.
    // Merkle still fails (synthetic proof) but freshness passes first.
    // Exit code 1 (merkle fail) confirms we got past the freshness gate.
    let body = synthetic_proof(Some(ProofAnchor {
        tip_digest: "42".into(),
        tip_height: 100,
    }));
    let (code, _out, err) = run(
        &["--chain-tip", "120", "--max-staleness", "20"],
        &body,
    );
    assert_eq!(
        code, 1,
        "at-boundary should pass freshness, then fail on synthetic merkle \
         (stderr: {err})"
    );
    assert!(
        err.contains("merkle"),
        "stderr should indicate merkle failure: {err}"
    );
}

#[test]
fn rejects_missing_anchor_metadata_with_exit_4() {
    // Old NNS server omitting anchor field. Reject unless --no-freshness.
    let body = synthetic_proof(None);
    let (code, _out, err) = run(&["--chain-tip", "100"], &body);
    assert_eq!(
        code, 4,
        "missing anchor must exit 4 when freshness required (stderr: {err})"
    );
    assert!(
        err.contains("anchor metadata"),
        "stderr should name missing anchor: {err}"
    );
}

#[test]
fn no_freshness_flag_skips_anchor_check() {
    // --no-freshness lets legacy proofs through. Merkle still runs.
    let body = synthetic_proof(None);
    let (code, _out, err) = run(&["--no-freshness"], &body);
    // Merkle fails on synthetic input — we expect exit 1, not 4.
    assert_eq!(
        code, 1,
        "--no-freshness should skip to merkle, not bail on missing anchor \
         (stderr: {err})"
    );
}

#[test]
fn rejects_mismatched_anchor_digest_with_exit_3() {
    // Proof's anchor digest differs from wallet's view at that height.
    // This is the fork-attack check.
    let body = synthetic_proof(Some(ProofAnchor {
        tip_digest: "aa".into(),
        tip_height: 100,
    }));
    let (code, _out, err) = run(
        &[
            "--chain-tip",
            "100",
            "--chain-tip-digest",
            "bb",
        ],
        &body,
    );
    assert_eq!(
        code, 3,
        "mismatched anchor digest must exit 3 (stderr: {err})"
    );
    assert!(
        err.contains("anchor binding"),
        "stderr should name anchor binding failure: {err}"
    );
}

#[test]
fn accepts_matching_anchor_digest() {
    // Digests match; freshness ok → freshness gate passes.
    // Exit 1 from synthetic merkle is the expected post-freshness outcome.
    let body = synthetic_proof(Some(ProofAnchor {
        tip_digest: "aa".into(),
        tip_height: 100,
    }));
    let (code, _out, err) = run(
        &[
            "--chain-tip",
            "100",
            "--chain-tip-digest",
            "aa",
        ],
        &body,
    );
    assert_eq!(
        code, 1,
        "matching anchor should pass freshness+binding, then fail merkle \
         (stderr: {err})"
    );
}

#[test]
fn requires_chain_tip_without_no_freshness() {
    // Caller forgot --chain-tip; --no-freshness not set.
    let body = synthetic_proof(Some(ProofAnchor {
        tip_digest: "aa".into(),
        tip_height: 100,
    }));
    let (code, _out, err) = run(&[], &body);
    assert_eq!(
        code, 5,
        "missing --chain-tip must exit 5 (stderr: {err})"
    );
}

#[test]
fn surfaces_server_error_body_with_exit_6() {
    // When the server returns an ErrorBody JSON (e.g. 404/500)
    // instead of a ProofResponse, `light_verify` should print the
    // error message cleanly and exit 6 — not drop a cryptic serde
    // trace on the user.
    let body = r#"{"error":"name not registered"}"#;
    let (code, _out, err) = run(&["--chain-tip", "100"], body);
    assert_eq!(code, 6, "ErrorBody body must exit 6 (stderr: {err})");
    assert!(
        err.contains("name not registered"),
        "stderr should surface the server's error message: {err}"
    );
}

#[test]
fn surfaces_unparseable_body_with_exit_6() {
    // Neither ProofResponse nor ErrorBody shape — print raw body so
    // the user can see what the server actually sent.
    let body = "this is not json";
    let (code, _out, err) = run(&["--chain-tip", "100"], body);
    assert_eq!(code, 6, "unparseable body must exit 6 (stderr: {err})");
    assert!(
        err.contains("raw body"),
        "stderr should include raw body dump: {err}"
    );
}

#[test]
fn rejects_proof_one_block_past_boundary() {
    // Proof at 99, wallet at 120, staleness 20 → lag 21 → exactly one
    // block past boundary → reject.
    let body = synthetic_proof(Some(ProofAnchor {
        tip_digest: "42".into(),
        tip_height: 99,
    }));
    let (code, _out, err) = run(
        &["--chain-tip", "120", "--max-staleness", "20"],
        &body,
    );
    assert_eq!(code, 2, "one-past-boundary must reject (stderr: {err})");
}
