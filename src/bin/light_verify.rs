use std::io::Read;

use nock_noun_rs::{jam_to_bytes, make_cord, new_stack, T};
use nockchain_tip5_rs::{verify_proof, ProofNode as Tip5ProofNode, Tip5Hash};
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let proof: ProofResponse = serde_json::from_str(&input)?;

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
    let ok = verify_proof(&leaf, &proof_nodes, &root);
    if ok {
        println!("ok");
        return Ok(());
    }
    Err("verification failed".into())
}
