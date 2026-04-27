//! Path Y4 `light_verify` CLI integration tests.
//!
//! Drives the `light_verify` binary with [`PathY4LookupBundle`](nns_vesl::wallet_y4::PathY4LookupBundle)
//! JSON on stdin and pinned `--checkpoint-*` flags.

use std::io::Write;
use std::process::{Command, Stdio};

fn binary() -> String {
    std::env::var("CARGO_BIN_EXE_light_verify")
        .unwrap_or_else(|_| "target/debug/light_verify".to_string())
}

fn hex40(seed: u8) -> String {
    vec![seed; 40].iter().map(|b| format!("{b:02x}")).collect()
}

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

fn checkpoint_args(height: u64, digest_seed: u8) -> Vec<String> {
    vec![
        "--checkpoint-height".into(),
        height.to_string(),
        "--checkpoint-digest-hex".into(),
        hex40(digest_seed),
    ]
}

/// Bundle: last proved equals checkpoint (no intermediate headers).
fn bundle_same_as_checkpoint(name: &str, height: u64, digest_seed: u8) -> String {
    format!(
        r#"{{
  "name": "{name}",
  "value": null,
  "last_proved_height": {height},
  "last_proved_digest_hex": "{dig}",
  "accumulator_root_hex": "{dig}",
  "recursive_proof_hex": "",
  "headers_to_checkpoint": []
}}"#,
        dig = hex40(digest_seed)
    )
}

#[test]
fn y4_same_height_chain_passes_with_y2_dev() {
    let cp = 100u64;
    let seed = 7u8;
    let mut args: Vec<String> = checkpoint_args(cp, seed);
    args.push("--path-y2-dev".into());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let body = bundle_same_as_checkpoint("alice.nock", cp, seed);
    let (code, out, err) = run(&arg_refs, &body);
    assert_eq!(code, 0, "stderr: {err}");
    assert!(out.contains("verified: alice.nock"), "stdout: {out}");
}

#[test]
fn y4_strict_rejects_empty_recursive_without_path_y2_dev() {
    let cp = 5u64;
    let seed = 3u8;
    let args = checkpoint_args(cp, seed);
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let body = bundle_same_as_checkpoint("x.nock", cp, seed);
    let (code, _out, err) = run(&arg_refs, &body);
    assert_eq!(code, 8, "expected exit 8, stderr: {err}");
    assert!(
        err.contains("path-y2-dev") || err.contains("--path-y2-dev"),
        "stderr: {err}"
    );
}

#[test]
fn y4_rejects_bad_header_chain() {
    let cp_h = 1u64;
    let d1 = hex40(1);
    let d2 = hex40(2);
    let d3 = hex40(3);
    let bad_parent = hex40(0xee);
    let body = format!(
        r#"{{
  "name": "a.nock",
  "last_proved_height": 3,
  "last_proved_digest_hex": "{d3}",
  "accumulator_root_hex": "{d3}",
  "recursive_proof_hex": "",
  "headers_to_checkpoint": [
    {{"height":3,"digest_hex":"{d3}","parent_hex":"{bad_parent}"}},
    {{"height":2,"digest_hex":"{d2}","parent_hex":"{d1}"}},
    {{"height":1,"digest_hex":"{d1}","parent_hex":"00000000000000000000000000000000000000000000000000000000000000000000000000000000"}}
  ]
}}"#
    );
    let mut args = checkpoint_args(cp_h, 1);
    args.push("--path-y2-dev".into());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let (code, _out, err) = run(&arg_refs, &body);
    assert_eq!(code, 1, "expected header failure exit 1, stderr: {err}");
}

#[test]
fn y4_value_without_z_in_requires_allow_missing_z() {
    let cp = 2u64;
    let seed = 9u8;
    let mut args = checkpoint_args(cp, seed);
    args.push("--allow-empty-recursive-proof".into());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let body = format!(
        r#"{{
  "name": "b.nock",
  "value": {{"owner":"o","tx_hash_hex":"{}","claim_height":2,"block_digest_hex":"{}"}},
  "last_proved_height": {cp},
  "last_proved_digest_hex": "{dig}",
  "accumulator_root_hex": "{dig}",
  "recursive_proof_hex": "",
  "headers_to_checkpoint": []
}}"#,
        hex40(8),
        hex40(8),
        dig = hex40(seed)
    );
    let (code, _out, err) = run(&arg_refs, &body);
    assert_eq!(code, 9, "stderr: {err}");
    assert!(
        err.contains("accumulator_snapshot_jam_hex"),
        "stderr: {err}"
    );
}

#[test]
fn y4_non_empty_recursive_missing_sf_jams_exits_7() {
    let cp = 1u64;
    let seed = 4u8;
    let mut args = checkpoint_args(cp, seed);
    args.push("--path-y2-dev".into());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let body = format!(
        r#"{{
  "name": "c.nock",
  "last_proved_height": {cp},
  "last_proved_digest_hex": "{dig}",
  "accumulator_root_hex": "{dig}",
  "recursive_proof_hex": "cafe00",
  "headers_to_checkpoint": []
}}"#,
        dig = hex40(seed)
    );
    let (code, _out, err) = run(&arg_refs, &body);
    assert_eq!(code, 7, "stderr: {err}");
    assert!(
        err.contains("recursive_subject_jam_hex") || err.contains("recursive_formula_jam_hex"),
        "stderr: {err}"
    );
}

#[test]
fn y4_non_empty_recursive_bad_subject_hex_exits_2() {
    let cp = 1u64;
    let seed = 4u8;
    let mut args = checkpoint_args(cp, seed);
    args.push("--path-y2-dev".into());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let body = format!(
        r#"{{
  "name": "c2.nock",
  "last_proved_height": {cp},
  "last_proved_digest_hex": "{dig}",
  "accumulator_root_hex": "{dig}",
  "recursive_proof_hex": "cafe00",
  "recursive_subject_jam_hex": "gg",
  "recursive_formula_jam_hex": "00",
  "headers_to_checkpoint": []
}}"#,
        dig = hex40(seed)
    );
    let (code, _out, err) = run(&arg_refs, &body);
    assert_eq!(code, 2, "stderr: {err}");
}

#[test]
fn y4_deprecated_non_empty_z_in_proof_exits_2() {
    let cp = 1u64;
    let seed = 2u8;
    let mut args = checkpoint_args(cp, seed);
    args.push("--path-y2-dev".into());
    let arg_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let body = format!(
        r#"{{
  "name": "d.nock",
  "last_proved_height": {cp},
  "last_proved_digest_hex": "{dig}",
  "accumulator_root_hex": "{dig}",
  "recursive_proof_hex": "",
  "z_in_proof": [{{"hash":"00","side":"left"}}],
  "headers_to_checkpoint": []
}}"#,
        dig = hex40(seed)
    );
    let (code, _out, err) = run(&arg_refs, &body);
    assert_eq!(code, 2, "stderr: {err}");
    assert!(err.contains("deprecated"), "stderr: {err}");
}

#[test]
fn y4_missing_checkpoint_flags_exit_5() {
    let body = bundle_same_as_checkpoint("z.nock", 1, 1);
    let (code, _out, err) = run(&["--path-y2-dev"], &body);
    assert_eq!(code, 5, "stderr: {err}");
}
