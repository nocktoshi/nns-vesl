//! Path Y4 NNS light-client verifier (offline).
//!
//! Verifies a [`nns_vesl::wallet_y4::PathY4LookupBundle`] from stdin in order:
//!
//! 1. **Recursive Vesl STARK** — non-empty `recursive_proof_hex` plus subject /
//!    formula JAMs → `%verify-stark-explicit` via `--kernel-jam` (exit **7** on failure).
//!    Empty proof requires `--allow-empty-recursive-proof` or `--path-y2-dev` (exit **8**).
//!
//! 2. **Z-map row binding** — when `value` is present and `accumulator_snapshot_jam_hex`
//!    is non-empty, `%verify-accumulator-snapshot` checks the JAMmed accumulator's
//!    `root-atom` matches `accumulator_root_hex` and `(get acc name)` matches `value`.
//!    Missing snapshot in strict mode → exit **9**. Mismatch / false → exit **10**.
//!    Legacy non-empty `z_in_proof` → exit **2** (deprecated).
//!
//! 3. **Header chain** — `headers_to_checkpoint` links `last_proved_digest` to the
//!    pinned `--checkpoint-*` (exit **1** on failure).
//!
//! No live Nockchain RPC, `--chain-tip`, or Phase 7 freshness flags.

use std::io::Read;

use nns_vesl::kernel::{
    verify_accumulator_snapshot_blocking, verify_stark_explicit_blocking,
};
use nns_vesl::wallet_y4::{
    hex_decode_even, verify_header_chain_to_checkpoint, AccumulatorEntryJson, CheckpointConfig,
    PathY4LookupBundle,
};

fn kernel_jam_bytes(path: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|e| format!("read kernel jam {path}: {e}"))
}

#[derive(Debug, Clone)]
struct Cli {
    checkpoint_height: Option<u64>,
    checkpoint_digest_hex: Option<String>,
    kernel_jam_path: String,
    allow_empty_recursive: bool,
    allow_missing_z_in: bool,
}

impl Cli {
    fn parse() -> Result<Self, String> {
        let mut checkpoint_height: Option<u64> = None;
        let mut checkpoint_digest_hex: Option<String> = None;
        let mut kernel_jam_path =
            std::env::var("NNS_KERNEL_JAM").unwrap_or_else(|_| "out.jam".to_string());
        let mut allow_empty_recursive = false;
        let mut allow_missing_z_in = false;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--checkpoint-height" => {
                    let v = args
                        .next()
                        .ok_or_else(|| "--checkpoint-height requires a value".to_string())?;
                    checkpoint_height = Some(
                        v.parse::<u64>()
                            .map_err(|e| format!("--checkpoint-height must be u64: {e}"))?,
                    );
                }
                "--checkpoint-digest-hex" => {
                    let v = args
                        .next()
                        .ok_or_else(|| "--checkpoint-digest-hex requires a value".to_string())?;
                    checkpoint_digest_hex = Some(v);
                }
                "--kernel-jam" => {
                    let v = args
                        .next()
                        .ok_or_else(|| "--kernel-jam requires a path".to_string())?;
                    kernel_jam_path = v;
                }
                "--allow-empty-recursive-proof" => {
                    allow_empty_recursive = true;
                }
                "--allow-missing-z-in-proof" => {
                    allow_missing_z_in = true;
                }
                "--path-y2-dev" => {
                    allow_empty_recursive = true;
                    allow_missing_z_in = true;
                }
                "-h" | "--help" => {
                    print_help();
                    std::process::exit(0);
                }
                other => {
                    return Err(format!("unknown argument: {other}"));
                }
            }
        }

        Ok(Cli {
            checkpoint_height,
            checkpoint_digest_hex,
            kernel_jam_path,
            allow_empty_recursive,
            allow_missing_z_in,
        })
    }
}

fn print_help() {
    eprintln!(
        r#"NNS light_verify — Path Y4 offline bundle checker

Reads PathY4LookupBundle JSON from stdin. Requires a pinned Nockchain checkpoint.

Checks (in order):
  1) recursive Vesl STARK (optional until Y3 — see flags)
  2) z-map row via accumulator_snapshot_jam_hex + accumulator_root_hex (when value present)
  3) header chain from last_proved_digest to checkpoint digest

Flags:
  --checkpoint-height <H>       Pinned canonical height.
  --checkpoint-digest-hex <HEX> Digest at that height (even-length hex, typically 40 bytes).
  --kernel-jam <PATH>           NNS kernel JAM (default: $NNS_KERNEL_JAM or ./out.jam).
  --allow-empty-recursive-proof   Accept empty / absent recursive_proof_hex (pre-Y3).
  --allow-missing-z-in-proof      Accept `value` without accumulator_snapshot_jam_hex (Y2).
  --path-y2-dev                   Sets both relax flags above.

When `recursive_proof_hex` is non-empty, also supply recursive_subject_jam_hex and
recursive_formula_jam_hex (raw proof JAM + traced nouns). Rebuild out.jam after kernel changes.

When `value` is present, supply accumulator_snapshot_jam_hex (from GET /accumulator/:name?wallet_export=1)
unless using --allow-missing-z-in-proof. Non-empty z_in_proof is rejected (deprecated).

Exit codes:
  0  OK
  1  header chain failed
  2  bad JSON / hex / deprecated z_in_proof
  5  missing CLI flags
  7  STARK or accumulator verify kernel error (boot, bad jam, etc.)
  8  strict: empty recursive proof
  9  strict: value without accumulator snapshot
  10 accumulator root / entry mismatch (verify returned false)

Trust model: docs/wallet-verification.md
"#
    );
}

fn decode_recursive_proof(bundle: &PathY4LookupBundle) -> Option<Vec<u8>> {
    match &bundle.recursive_proof_hex {
        None => None,
        Some(s) => {
            let t = s.trim();
            if t.is_empty() {
                None
            } else {
                match hex_decode_even(t) {
                    Ok(b) => Some(b),
                    Err(e) => {
                        eprintln!("error: recursive_proof_hex: {e}");
                        std::process::exit(2);
                    }
                }
            }
        }
    }
}

fn verify_stark_section(cli: &Cli, bundle: &PathY4LookupBundle, recursive_bytes: &Option<Vec<u8>>) -> bool {
    if let Some(ref b) = recursive_bytes {
        if b.is_empty() {
            return false;
        }
        let sj_hex = bundle
            .recursive_subject_jam_hex
            .as_ref()
            .map(|s| s.trim());
        let fj_hex = bundle
            .recursive_formula_jam_hex
            .as_ref()
            .map(|s| s.trim());
        match (sj_hex, fj_hex) {
            (Some(sj), Some(fj)) if !sj.is_empty() && !fj.is_empty() => {
                let subject_jam = match hex_decode_even(sj) {
                    Ok(x) => x,
                    Err(e) => {
                        eprintln!("error: recursive_subject_jam_hex: {e}");
                        std::process::exit(2);
                    }
                };
                let formula_jam = match hex_decode_even(fj) {
                    Ok(x) => x,
                    Err(e) => {
                        eprintln!("error: recursive_formula_jam_hex: {e}");
                        std::process::exit(2);
                    }
                };
                let kernel_jam = match kernel_jam_bytes(&cli.kernel_jam_path) {
                    Ok(k) => k,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(7);
                    }
                };
                match verify_stark_explicit_blocking(
                    &kernel_jam,
                    b,
                    &subject_jam,
                    &formula_jam,
                ) {
                    Ok(true) => true,
                    Ok(false) => {
                        eprintln!("error: Vesl STARK verify returned false (exit 7)");
                        std::process::exit(7);
                    }
                    Err(msg) => {
                        eprintln!("error: STARK verify / kernel: {msg} (exit 7)");
                        std::process::exit(7);
                    }
                }
            }
            _ => {
                eprintln!(
                    "error: non-empty recursive_proof_hex requires both \
                     recursive_subject_jam_hex and recursive_formula_jam_hex (exit 7)"
                );
                std::process::exit(7);
            }
        }
    } else {
        false
    }
}

fn parse_entry_hex(v: &AccumulatorEntryJson) -> Result<(Vec<u8>, Vec<u8>), String> {
    let tx = hex_decode_even(v.tx_hash_hex.trim())?;
    let bd = hex_decode_even(v.block_digest_hex.trim())?;
    Ok((tx, bd))
}

fn main() {
    let cli = match Cli::parse() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("run with --help for usage.");
            std::process::exit(5);
        }
    };

    let cp_height = match cli.checkpoint_height {
        Some(h) => h,
        None => {
            eprintln!("error: --checkpoint-height is required");
            std::process::exit(5);
        }
    };
    let cp_hex = match &cli.checkpoint_digest_hex {
        Some(h) => h,
        None => {
            eprintln!("error: --checkpoint-digest-hex is required");
            std::process::exit(5);
        }
    };

    let checkpoint_digest = match hex_decode_even(cp_hex) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: bad checkpoint digest hex: {e}");
            std::process::exit(5);
        }
    };

    let mut input = String::new();
    if let Err(e) = std::io::stdin().read_to_string(&mut input) {
        eprintln!("error: read stdin: {e}");
        std::process::exit(2);
    }

    let bundle: PathY4LookupBundle = match serde_json::from_str(&input) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: invalid PathY4LookupBundle JSON: {e}");
            std::process::exit(2);
        }
    };

    if bundle
        .z_in_proof
        .as_ref()
        .map(|z| !z.is_empty())
        .unwrap_or(false)
    {
        eprintln!(
            "error: non-empty z_in_proof is deprecated — use accumulator_snapshot_jam_hex \
             from ?wallet_export=1 (exit 2)"
        );
        std::process::exit(2);
    }

    let last_digest = match hex_decode_even(&bundle.last_proved_digest_hex) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: bad last_proved_digest_hex: {e}");
            std::process::exit(2);
        }
    };

    let accumulator_root = match hex_decode_even(&bundle.accumulator_root_hex) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: bad accumulator_root_hex: {e}");
            std::process::exit(2);
        }
    };

    let recursive_bytes = decode_recursive_proof(&bundle);

    let stark_ok = verify_stark_section(&cli, &bundle, &recursive_bytes);

    if recursive_bytes.as_ref().map(|b| b.is_empty()).unwrap_or(true) {
        if recursive_bytes.is_none() && !cli.allow_empty_recursive {
            eprintln!(
                "error: missing or empty recursive_proof_hex — pass \
                 --allow-empty-recursive-proof or --path-y2-dev until Y3 (exit 8)"
            );
            std::process::exit(8);
        }
    }

    let mut acc_ok = false;
    if let Some(ref v) = bundle.value {
        let snap_hex = bundle
            .accumulator_snapshot_jam_hex
            .as_ref()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty());
        match snap_hex {
            Some(sh) => {
                let acc_jam = match hex_decode_even(sh) {
                    Ok(x) => x,
                    Err(e) => {
                        eprintln!("error: accumulator_snapshot_jam_hex: {e}");
                        std::process::exit(2);
                    }
                };
                let (tx_hash, block_digest) = match parse_entry_hex(v) {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("error: value tx/block hex: {e}");
                        std::process::exit(2);
                    }
                };
                let kernel_jam = match kernel_jam_bytes(&cli.kernel_jam_path) {
                    Ok(k) => k,
                    Err(e) => {
                        eprintln!("error: {e}");
                        std::process::exit(7);
                    }
                };
                match verify_accumulator_snapshot_blocking(
                    &kernel_jam,
                    &accumulator_root,
                    &acc_jam,
                    bundle.name.trim(),
                    v.owner.trim(),
                    &tx_hash,
                    v.claim_height,
                    &block_digest,
                ) {
                    Ok(true) => acc_ok = true,
                    Ok(false) => {
                        eprintln!(
                            "error: accumulator snapshot does not match root and/or entry (exit 10)"
                        );
                        std::process::exit(10);
                    }
                    Err(msg) => {
                        eprintln!("error: accumulator verify / kernel: {msg} (exit 7)");
                        std::process::exit(7);
                    }
                }
            }
            None => {
                if !cli.allow_missing_z_in {
                    eprintln!(
                        "error: bundle has `value` but no accumulator_snapshot_jam_hex — pass \
                         --allow-missing-z-in-proof or --path-y2-dev (exit 9)"
                    );
                    std::process::exit(9);
                }
            }
        }
    }

    let checkpoint = CheckpointConfig {
        height: cp_height,
        digest: checkpoint_digest,
    };

    if let Err(e) = verify_header_chain_to_checkpoint(
        bundle.last_proved_height,
        &last_digest,
        &checkpoint,
        &bundle.headers_to_checkpoint,
    ) {
        eprintln!("header chain verification failed: {e}");
        std::process::exit(1);
    }

    println!("verified: {}", bundle.name);
    println!("  last_proved_height: {}", bundle.last_proved_height);
    println!(
        "  checkpoint: height={} ({}…)",
        cp_height,
        truncate_hex(cp_hex, 12)
    );
    if bundle.value.is_some() {
        println!("  value: present");
    } else {
        println!("  value: absent");
    }
    if cli.allow_empty_recursive && cli.allow_missing_z_in {
        println!("  mode: path-y2-dev (both Y2 relax flags set)");
    } else if cli.allow_empty_recursive || cli.allow_missing_z_in {
        println!(
            "  mode: partial Y2 relax (empty_recursive={} missing_z_in={})",
            cli.allow_empty_recursive, cli.allow_missing_z_in
        );
    }
    println!("checks:");
    if stark_ok {
        println!("  [PASS] recursive Vesl STARK (%verify-stark-explicit)");
    } else {
        println!("  [SKIP] recursive Vesl STARK (empty / absent proof)");
    }
    if bundle.value.is_some() {
        if acc_ok {
            println!("  [PASS] z-map row (%verify-accumulator-snapshot)");
        } else {
            println!("  [SKIP] z-map row (no snapshot; Y2 relax)");
        }
    } else {
        println!("  [SKIP] z-map row (no value in bundle)");
    }
    println!("  [PASS] header chain to checkpoint");
}

fn truncate_hex(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1) / 2;
    format!("{}…", &s[..keep])
}
