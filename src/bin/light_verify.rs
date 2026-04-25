//! NNS light-client verifier.
//!
//! Reads a `ProofResponse` JSON on stdin and verifies:
//!
//! 1. **Merkle inclusion** — `(name, owner, tx_hash)` is a leaf in
//!    the Merkle tree whose root is `response.root`, under the
//!    sibling chain in `response.proof`.
//!
//! 2. **Anchor freshness** (Phase 7) — the `anchor.tip_height`
//!    committed in the response is within `--max-staleness` blocks
//!    of the wallet's view of the canonical Nockchain tip,
//!    supplied via `--chain-tip <H>`. Default staleness window is
//!    20 blocks; match to your deployment's finality depth + margin.
//!
//! 3. **Anchor binding** (optional) — when `--chain-tip-digest
//!    <hex>` is supplied, check that `response.anchor.tip_digest`
//!    equals the wallet's view of the digest at `tip_height`.
//!    Detects fork attacks where a malicious server proves against
//!    a non-canonical side-chain at the same height.
//!
//! NOT YET implemented here:
//!
//! - STARK proof verification (needs Rust bindings to Vesl's
//!   `verify:vesl-verifier`; deferred — today NNS clients run
//!   `%verify-stark` via a kernel peek or delegate to a trusted
//!   verifier service).
//!
//! ## CLI
//!
//! ```text
//! light_verify \
//!     --chain-tip <HEIGHT> \
//!     [--chain-tip-digest <HEX>] \
//!     [--max-staleness 20] \
//!     [--no-freshness]  (skip freshness; legacy NNS servers without anchor)
//! ```
//!
//! Reads `ProofResponse` JSON on stdin. Exit code 0 = all checks
//! passed; non-zero = specific failure.

use std::io::Read;

use nock_noun_rs::{jam_to_bytes, make_cord, new_stack, T};
use nockchain_tip5_rs::{verify_proof, ProofNode as Tip5ProofNode, Tip5Hash};
use nns_vesl::freshness::{check_anchor_binding, Freshness, DEFAULT_MAX_STALENESS};
use nns_vesl::types::{ProofResponse, ProofSide};

const GOLDILOCKS_PRIME: u64 = 18_446_744_069_414_584_321;

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err("hex length must be even".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

fn le_bytes_to_tip5(bytes: &[u8]) -> Tip5Hash {
    let mut n: Vec<u8> = bytes.to_vec();
    while n.last() == Some(&0) {
        n.pop();
    }
    let mut limbs: Tip5Hash = [0u64; 5];
    for limb in &mut limbs {
        if n.is_empty() {
            break;
        }
        let mut rem: u128 = 0;
        let mut quot = vec![0u8; n.len()];
        for i in (0..n.len()).rev() {
            let v = (rem << 8) | (n[i] as u128);
            quot[i] = (v / GOLDILOCKS_PRIME as u128) as u8;
            rem = v % GOLDILOCKS_PRIME as u128;
        }
        *limb = rem as u64;
        while quot.last() == Some(&0) {
            quot.pop();
        }
        n = quot;
    }
    limbs
}

fn jam_leaf(name: &str, owner: &str, tx_hash: &str) -> Vec<u8> {
    let mut stack = new_stack();
    let n = make_cord(&mut stack, name);
    let o = make_cord(&mut stack, owner);
    let t = make_cord(&mut stack, tx_hash);
    let noun = T(&mut stack, &[n, o, t]);
    jam_to_bytes(&mut stack, noun)
}

#[derive(Debug, Clone)]
struct Cli {
    chain_tip_height: Option<u64>,
    chain_tip_digest: Option<Vec<u8>>,
    max_staleness: u64,
    require_freshness: bool,
}

impl Cli {
    fn parse() -> Result<Self, String> {
        let mut chain_tip_height: Option<u64> = None;
        let mut chain_tip_digest: Option<Vec<u8>> = None;
        let mut max_staleness = DEFAULT_MAX_STALENESS;
        let mut require_freshness = true;

        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--chain-tip" => {
                    let v = args
                        .next()
                        .ok_or_else(|| "--chain-tip requires a value".to_string())?;
                    chain_tip_height = Some(
                        v.parse::<u64>()
                            .map_err(|e| format!("--chain-tip must be u64: {e}"))?,
                    );
                }
                "--chain-tip-digest" => {
                    let v = args
                        .next()
                        .ok_or_else(|| "--chain-tip-digest requires a value".to_string())?;
                    chain_tip_digest =
                        Some(hex_decode(&v).map_err(|e| format!("bad --chain-tip-digest: {e}"))?);
                }
                "--max-staleness" => {
                    let v = args
                        .next()
                        .ok_or_else(|| "--max-staleness requires a value".to_string())?;
                    max_staleness = v
                        .parse::<u64>()
                        .map_err(|e| format!("--max-staleness must be u64: {e}"))?;
                }
                "--no-freshness" => {
                    require_freshness = false;
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
            chain_tip_height,
            chain_tip_digest,
            max_staleness,
            require_freshness,
        })
    }
}

fn print_help() {
    eprintln!(
        r#"NNS light-client verifier

Reads a ProofResponse JSON from stdin and verifies Merkle inclusion + anchor freshness.

Flags:
  --chain-tip <HEIGHT>         Wallet's view of the canonical Nockchain tip height.
                                Required unless --no-freshness is set.
  --chain-tip-digest <HEX>     Wallet's view of the digest at <HEIGHT>. If set,
                                verifies the proof's anchor digest matches (fork check).
  --max-staleness <N>          Reject proofs anchored more than N blocks stale.
                                Default: {default_staleness}.
  --no-freshness               Skip freshness check. Only use for testing or
                                legacy NNS servers that don't emit anchor metadata.
  -h, --help                   Show this help.

Exit codes:
  0 — all checks passed
  1 — Merkle inclusion failed
  2 — freshness check failed (proof too stale)
  3 — anchor binding check failed (fork / digest mismatch)
  4 — missing anchor metadata and freshness required
  5 — missing CLI args
  6 — server returned an error body, or unparseable input
  other — malformed input

Example:
  curl -s http://nns.example/proof?name=alice.nock \
    | light_verify --chain-tip 12345 --chain-tip-digest abcd...
"#,
        default_staleness = DEFAULT_MAX_STALENESS
    );
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = match Cli::parse() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("error: {e}");
            eprintln!("run with --help for usage.");
            std::process::exit(5);
        }
    };

    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;

    // Try to parse as ProofResponse. If that fails, fall back to
    // the server's ErrorBody shape so a 4xx/5xx response surfaces
    // its message cleanly instead of a cryptic serde trace. Last
    // resort: dump the raw body so the caller can see what the
    // server actually returned.
    let proof: ProofResponse = match serde_json::from_str::<ProofResponse>(&input) {
        Ok(p) => p,
        Err(proof_err) => {
            #[derive(serde::Deserialize)]
            struct ErrorBody {
                error: String,
            }
            match serde_json::from_str::<ErrorBody>(&input) {
                Ok(e) => {
                    eprintln!("server returned error: {}", e.error);
                    std::process::exit(6);
                }
                Err(_) => {
                    eprintln!(
                        "could not parse stdin as ProofResponse: {proof_err}\n\
                         --- raw body ---\n{input}"
                    );
                    std::process::exit(6);
                }
            }
        }
    };

    // --- 1. Anchor freshness (Phase 7) --------------------------------
    //
    // Run BEFORE Merkle inclusion so a stale/forked proof fails fast
    // with a precise exit code rather than an ambiguous merkle-fail
    // signal. Freshness is the cheaper check; Merkle verification is
    // O(log N) hashes.
    //
    // Accumulate per-check status for the final summary so users see
    // exactly what got verified (and what was skipped) rather than a
    // bare "ok".
    let mut freshness_status: CheckStatus = CheckStatus::Skipped("--no-freshness".into());
    let mut binding_status: CheckStatus = CheckStatus::Skipped("no --chain-tip-digest".into());

    if cli.require_freshness {
        let chain_tip_height = match cli.chain_tip_height {
            Some(h) => h,
            None => {
                eprintln!(
                    "error: --chain-tip required for freshness check \
                     (or pass --no-freshness for legacy compatibility)"
                );
                std::process::exit(5);
            }
        };

        let anchor = match proof.anchor.as_ref() {
            Some(a) => a,
            None => {
                eprintln!(
                    "proof response has no anchor metadata; either the NNS server \
                     predates Phase 7 or the operator stripped the field. Refuse \
                     to accept without --no-freshness."
                );
                std::process::exit(4);
            }
        };

        let policy = Freshness::new(cli.max_staleness);
        if let Err(e) = policy.check(anchor.tip_height, chain_tip_height) {
            eprintln!("freshness check failed: {e}");
            std::process::exit(2);
        }
        let lag = chain_tip_height.saturating_sub(anchor.tip_height);
        freshness_status = CheckStatus::Ok(format!(
            "tip {}, lag {}, max {}",
            anchor.tip_height, lag, cli.max_staleness
        ));

        // --- 2. Anchor binding (optional) -----------------------------
        if let Some(ref wallet_digest) = cli.chain_tip_digest {
            let proof_digest = hex_decode(&anchor.tip_digest)
                .map_err(|e| format!("bad anchor.tip_digest hex: {e}"))?;
            if let Err(e) = check_anchor_binding(&proof_digest, wallet_digest) {
                eprintln!("anchor binding check failed: {e}");
                std::process::exit(3);
            }
            binding_status = CheckStatus::Ok("digest matches".into());
        }
    }

    // --- 3. Merkle inclusion ------------------------------------------
    let root = le_bytes_to_tip5(&hex_decode(&proof.root).map_err(|e| format!("bad root hex: {e}"))?);
    let proof_nodes: Vec<Tip5ProofNode> = proof
        .proof
        .iter()
        .map(|n| {
            let hash = le_bytes_to_tip5(
                &hex_decode(&n.hash).map_err(|e| format!("bad proof node hash: {e}"))?,
            );
            let side = matches!(n.side, ProofSide::Left);
            Ok(Tip5ProofNode { hash, side })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let leaf = jam_leaf(&proof.name, &proof.owner, &proof.tx_hash);
    if !verify_proof(&leaf, &proof_nodes, &root) {
        eprintln!("merkle inclusion verification failed");
        std::process::exit(1);
    }
    let merkle_status = CheckStatus::Ok(format!("{} siblings", proof.proof.len()));

    print_verified(&proof, merkle_status, freshness_status, binding_status);
    Ok(())
}

/// Per-check outcome used in the success summary.
enum CheckStatus {
    /// Check passed — string describes *how* (e.g. "lag 10, max 20").
    Ok(String),
    /// Check was skipped — string describes why.
    Skipped(String),
}

impl CheckStatus {
    fn mark(&self) -> &'static str {
        match self {
            CheckStatus::Ok(_) => "PASS",
            CheckStatus::Skipped(_) => "SKIP",
        }
    }
    fn detail(&self) -> &str {
        match self {
            CheckStatus::Ok(s) | CheckStatus::Skipped(s) => s.as_str(),
        }
    }
}

/// Print a structured summary of what the proof attests + which
/// checks ran. Goes to stdout so callers can pipe/parse it (the
/// `verified:` prefix on the first line makes one-line grep easy).
fn print_verified(
    proof: &ProofResponse,
    merkle: CheckStatus,
    freshness: CheckStatus,
    binding: CheckStatus,
) {
    println!("verified: {}", proof.name);
    println!("  owner:      {}", proof.owner);
    println!("  tx_hash:    {}", truncate_middle(&proof.tx_hash, 24));
    println!("  claim_id:   {}", proof.claim_id);
    println!("  root:       {}", truncate_middle(&proof.root, 24));
    println!("  hull:       {}", truncate_middle(&proof.hull, 24));
    if let Some(ref a) = proof.anchor {
        println!(
            "  anchor:     height={} digest={}",
            a.tip_height,
            truncate_middle(&a.tip_digest, 24)
        );
    } else {
        println!("  anchor:     <not provided by server>");
    }
    println!();
    println!("checks:");
    println!("  [{}] merkle inclusion    ({})", merkle.mark(), merkle.detail());
    println!("  [{}] anchor freshness    ({})", freshness.mark(), freshness.detail());
    println!("  [{}] anchor binding      ({})", binding.mark(), binding.detail());
}

/// `"abcdef...9876"` style abbreviation for long hex strings so the
/// summary stays readable. Leaves shorter strings unmodified.
fn truncate_middle(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(3) / 2;
    format!("{}...{}", &s[..keep], &s[s.len() - keep..])
}
