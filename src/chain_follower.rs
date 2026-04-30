use std::time::Duration;

use nockapp::wire::{SystemWire, Wire};
use nockapp_grpc::pb::common::v2::NoteData as PbNoteData;
use nockapp_grpc::pb::public::v2::TransactionDetails;
use nockchain_client_rs::{NoteData, NoteDataEntry};
use tokio::task::JoinHandle;

use crate::chain::{
    base58_hash_to_atom_bytes, fetch_current_tip_height, prefetch_scan_blocks_for_heights,
    validate_scan_block_chain,
};
use crate::claim_note::ClaimNoteV1;
use crate::kernel::{
    build_scan_block_poke, build_scan_state_peek, decode_scan_state, first_error_message,
    first_scan_block_done, first_scan_block_error, format_effect_tags, has_effect,
    ClaimCandidate, ClaimWitness,
};
use crate::payment::{fee_for_name, sum_treasury_outputs_v1, TREASURY_LOCK_ROOT_B58};
use crate::state::SharedState;

/// Hex prefix for Tip5 atoms in logs (`RUST_LOG=nns_vesl::chain_follower=debug`).
fn atom_hex_preview(bytes: &[u8], prefix_len: usize) -> String {
    let n = prefix_len.min(bytes.len());
    let hex: String = bytes[..n].iter().map(|b| format!("{b:02x}")).collect();
    if bytes.len() > n {
        format!("{hex}…({}B)", bytes.len())
    } else {
        format!("{hex}({}B)", bytes.len())
    }
}

fn format_tx_id_previews(ids: &[Vec<u8>], max: usize) -> String {
    let mut out = Vec::new();
    for id in ids.iter().take(max) {
        out.push(atom_hex_preview(id, 8));
    }
    let suffix = if ids.len() > max {
        format!(" …(+{} more)", ids.len() - max)
    } else {
        String::new()
    };
    format!("[{}]{suffix}", out.join(", "))
}

fn format_candidates_for_log(candidates: &[ClaimCandidate]) -> String {
    const MAX: usize = 12;
    let mut parts = Vec::new();
    for c in candidates.iter().take(MAX) {
        parts.push(format!(
            "(name={:?} owner={:?} fee={} tx_hash={} wit.tx={} wit.spender={} wit.amt={} wit.treas={:?})",
            c.name,
            c.owner,
            c.fee,
            atom_hex_preview(&c.tx_hash, 6),
            atom_hex_preview(&c.witness.tx_id, 6),
            atom_hex_preview(&c.witness.spender_pkh, 6),
            c.witness.treasury_amount,
            c.witness.output_lock_root,
        ));
    }
    if candidates.len() > MAX {
        parts.push(format!("…(+{} more candidates)", candidates.len() - MAX));
    }
    parts.join(" ")
}

/// Sleep between ticks **only** when there is nothing to scan (caught up
/// within finality) or after an error — avoids gRPC busy-loops. While a
/// finalized backlog exists, consecutive `%scan-block` steps run back-to-back.
const FOLLOWER_POLL: Duration = Duration::from_secs(2);

/// How far behind the chain tip the follower waits before committing a
/// block to the kernel scan cursor. Keeps Path Y scans free of short
/// reorgs without waiting on economic finality.
pub const DEFAULT_FINALITY_DEPTH: u64 = 10;

/// Transitional compatibility for status/admin JSON while the API is renamed
/// from "anchor advance" to "block scan".
pub const DEFAULT_MAX_ADVANCE_BATCH: u64 = 1;

/// How many consecutive finalized blocks to prefetch and `%scan-block` apply
/// per idle tick when catching up. Override with `NNS_FOLLOWER_BATCH_BLOCKS`.
pub const DEFAULT_SCAN_BATCH_BLOCKS: u64 = 16;

fn follower_scan_batch_blocks() -> u64 {
    match std::env::var("NNS_FOLLOWER_BATCH_BLOCKS") {
        Ok(s) => match s.parse::<u64>() {
            Ok(n) if n >= 1 => n,
            _ => DEFAULT_SCAN_BATCH_BLOCKS,
        },
        Err(_) => DEFAULT_SCAN_BATCH_BLOCKS,
    }
}

/// Spawn the Path Y block scanner. It advances the kernel with `%scan-block`,
/// prefetching a batch of blocks in parallel when behind finality.
pub fn spawn(state: SharedState) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let idle = match scan_once(&state).await {
                Ok(Some(scanned)) => {
                    tracing::info!(
                        height_end = scanned.height,
                        blocks = scanned.blocks_applied,
                        phase = "scan_block",
                        "chain follower scanned blocks"
                    );
                    false
                }
                Ok(None) => {
                    tracing::trace!(phase = "scan_block", "scan tick no-op");
                    true
                }
                Err(err) => {
                    let ts = crate::state::AppState::now_epoch_ms();
                    let mut h = state.hull.lock().await;
                    h.follower.record_error("scan_block", err.clone(), ts);
                    drop(h);
                    tracing::warn!(
                        err = %err,
                        phase = "scan_block",
                        "chain follower scan tick failed"
                    );
                    true
                }
            };
            if idle {
                tokio::time::sleep(FOLLOWER_POLL).await;
            } else {
                tokio::task::yield_now().await;
            }
        }
    })
}

pub async fn process_once(state: &SharedState) -> Result<(), String> {
    scan_once(state).await.map(|_| ())
}

/// Outcome of one block-scan pass (last block in the batch).
#[derive(Debug, Clone)]
pub struct ScanBlockOutcome {
    pub height: u64,
    pub digest: Vec<u8>,
    pub accumulator_root: Vec<u8>,
    /// Finalized blocks applied this tick (`1` when only one `%scan-block` ran).
    pub blocks_applied: u64,
}

/// Backwards-compatible shape used by the existing admin handler.
#[derive(Debug, Clone)]
pub struct AnchorAdvanceOutcome {
    pub tip_height: u64,
    pub tip_digest: Vec<u8>,
    pub count: u64,
}

/// Transitional wrapper: old callers that ask to advance the anchor now drive
/// one `%scan-block` step.
pub async fn advance_anchor_once(
    state: &SharedState,
) -> Result<Option<AnchorAdvanceOutcome>, String> {
    Ok(scan_once(state).await?.map(|out| AnchorAdvanceOutcome {
        tip_height: out.height,
        tip_digest: out.digest,
        count: out.blocks_applied,
    }))
}

/// One pass of the Path Y block scanner. Returns `Ok(None)` if there is
/// no finalized block beyond the kernel's current `/scan-state`.
pub async fn scan_once(state: &SharedState) -> Result<Option<ScanBlockOutcome>, String> {
    let (is_local_mode, chain_endpoint) = {
        let h = state.hull.lock().await;
        (
            matches!(h.settlement.mode, vesl_core::SettlementMode::Local),
            h.settlement.chain_endpoint.clone(),
        )
    };
    if is_local_mode {
        return Ok(None);
    }
    let Some(endpoint) = chain_endpoint else {
        return Ok(None);
    };

    let scan_state = {
        let peek_result = {
            let mut k = state.kernel.lock().await;
            k.peek(build_scan_state_peek()).await
        };
        match peek_result {
            Ok(result) => {
                decode_scan_state(&result).map_err(|e| format!("scan-state decode failed: {e}"))?
            }
            Err(e) => {
                let msg = format!("scan-state peek failed: {e:?}");
                let ts = crate::state::AppState::now_epoch_ms();
                let mut h = state.hull.lock().await;
                h.follower.record_error("scan_peek", msg.clone(), ts);
                return Err(msg);
            }
        }
    };

    tracing::debug!(
        last_proved_height = scan_state.last_proved_height,
        last_proved_digest = %atom_hex_preview(&scan_state.last_proved_digest, 16),
        accumulator_root = %atom_hex_preview(&scan_state.accumulator_root, 16),
        accumulator_size = scan_state.accumulator_size,
        "chain_follower: /scan-state peek"
    );

    let current_chain_tip = match fetch_current_tip_height(&endpoint).await {
        Ok(p) => p,
        Err(e) => {
            let ts = crate::state::AppState::now_epoch_ms();
            let mut h = state.hull.lock().await;
            h.follower.record_error("plan", e.clone(), ts);
            return Err(e);
        }
    };

    let now = crate::state::AppState::now_epoch_ms();
    {
        let mut h = state.hull.lock().await;
        h.follower.record_chain_tip(current_chain_tip, now);
    }

    if current_chain_tip <= DEFAULT_FINALITY_DEPTH {
        return Ok(None);
    }
    let finalized_height = current_chain_tip.saturating_sub(DEFAULT_FINALITY_DEPTH);
    let next_height = scan_state.last_proved_height.saturating_add(1);
    if next_height > finalized_height {
        return Ok(None);
    }

    let batch_max = follower_scan_batch_blocks();
    let batch_end = next_height
        .saturating_add(batch_max.saturating_sub(1))
        .min(finalized_height);
    let heights: Vec<u64> = (next_height..=batch_end).collect();

    let prefetched = prefetch_scan_blocks_for_heights(&endpoint, &heights).await?;
    validate_scan_block_chain(scan_state.last_proved_digest.as_slice(), &prefetched)?;

    let batch_lo = prefetched
        .first()
        .map(|b| b.height)
        .unwrap_or(next_height);
    let blocks_applied = prefetched.len() as u64;

    let mut last_done = None;

    for block in &prefetched {
        let candidates = extract_claim_candidates(&block.tx_details)?;

        tracing::debug!(
            chain_tip = current_chain_tip,
            finalized_height,
            batch_lo,
            batch_end,
            height = block.height,
            parent = %atom_hex_preview(&block.parent, 16),
            page_digest = %atom_hex_preview(&block.page_digest, 16),
            page_tx_count = block.page_tx_ids.len(),
            page_tx_ids_preview = %format_tx_id_previews(&block.page_tx_ids, 8),
            claim_candidates_count = candidates.len(),
            claim_candidates = %format_candidates_for_log(&candidates),
            "chain_follower: %scan-block poke payload (Tip5 atoms are LE 40B; compare with kernel last_proved_digest)"
        );

        let poke_result = {
            let mut k = state.kernel.lock().await;
            k.poke(
                SystemWire.to_wire(),
                build_scan_block_poke(
                    &block.parent,
                    block.height,
                    &block.page_digest,
                    &block.page_tx_ids,
                    &candidates,
                ),
            )
            .await
        };

        let effects = poke_result.map_err(|e| {
            let msg = format!("scan-block poke failed: {e:?}");
            let ts = crate::state::AppState::now_epoch_ms();
            if let Ok(mut h) = state.hull.try_lock() {
                h.follower.record_error("scan_poke", msg.clone(), ts);
            }
            msg
        })?;

        if let Some(err) = first_scan_block_error(&effects) {
            let msg = format!("kernel rejected %scan-block: {err}");
            let ts = crate::state::AppState::now_epoch_ms();
            let mut h = state.hull.lock().await;
            h.follower.record_error("scan_poke", msg.clone(), ts);
            return Err(msg);
        }
        if let Some(err) = first_error_message(&effects) {
            let msg = format!("kernel rejected %scan-block: {err}");
            let ts = crate::state::AppState::now_epoch_ms();
            let mut h = state.hull.lock().await;
            h.follower.record_error("scan_poke", msg.clone(), ts);
            return Err(msg);
        }
        let Some(done) = first_scan_block_done(&effects) else {
            let tags = format_effect_tags(&effects);
            let msg = if has_effect(&effects, "invalid-cause") {
                "%invalid-cause from kernel — `(soft cause)` failed (mold mismatch). \
                 Rebuild out.jam from current hoon/app/app.hoon so `+$cause` includes `%scan-block`, \
                 point NNS_KERNEL_JAM at it, redeploy."
                    .to_string()
            } else if effects.is_empty() {
                "kernel did not emit %scan-block-done (empty effects — wrapper/nockapp returned no effects; \
                 if stderr shows `nns: invalid cause`, rebuild out.jam; otherwise check nockapp poke wiring)"
                    .to_string()
            } else {
                format!(
                    "kernel did not emit %scan-block-done (effect tags: {tags}; check kernel JAM vs hull or scan-block-done noun shape)"
                )
            };
            let ts = crate::state::AppState::now_epoch_ms();
            let mut h = state.hull.lock().await;
            h.follower.record_error("scan_poke", msg.clone(), ts);
            return Err(msg);
        };

        state.maybe_persist_after_follower_scan().await;

        tracing::debug!(
            height = done.height,
            block_digest = %atom_hex_preview(&done.digest, 16),
            accumulator_root = %atom_hex_preview(&done.accumulator_root, 16),
            "chain_follower: %scan-block-done"
        );

        last_done = Some(done);
    }

    let Some(done) = last_done else {
        return Err("prefetch produced no blocks (internal error)".into());
    };

    let now = crate::state::AppState::now_epoch_ms();
    {
        let mut h = state.hull.lock().await;
        h.follower.record_advance(done.height, blocks_applied, now);
    }

    Ok(Some(ScanBlockOutcome {
        height: done.height,
        digest: done.digest,
        accumulator_root: done.accumulator_root,
        blocks_applied,
    }))
}

fn extract_claim_candidates(details: &[TransactionDetails]) -> Result<Vec<ClaimCandidate>, String> {
    let mut candidates = Vec::new();
    for tx in details {
        candidates.extend(extract_claim_candidates_from_transaction(tx)?);
    }
    Ok(candidates)
}

fn extract_claim_candidates_from_transaction(
    details: &TransactionDetails,
) -> Result<Vec<ClaimCandidate>, String> {
    let mut candidates = Vec::new();
    for output in &details.outputs {
        let Some(note_data) = output.note_data.as_ref() else {
            continue;
        };
        let note_data = note_data_from_proto(note_data);
        let Ok(note) = ClaimNoteV1::from_note_data(&note_data) else {
            continue;
        };
        let tx_hash = base58_hash_to_atom_bytes(&note.tx_hash)?;
        let actual_tx_hash = base58_hash_to_atom_bytes(&details.tx_id)?;
        let witness = claim_witness_from_transaction(details, &actual_tx_hash, &note.owner);
        candidates.push(ClaimCandidate {
            fee: fee_for_name(&note.name),
            name: note.name,
            owner: note.owner,
            tx_hash,
            witness,
        });
    }
    Ok(candidates)
}

fn claim_witness_from_transaction(
    details: &TransactionDetails,
    tx_hash: &[u8],
    owner: &str,
) -> ClaimWitness {
    let spender = details
        .inputs
        .iter()
        .find(|input| input.note_name_b58.trim() == owner)
        .or_else(|| details.inputs.first())
        .map(|input| input.note_name_b58.trim())
        .unwrap_or_default();
    ClaimWitness {
        tx_id: tx_hash.to_vec(),
        spender_pkh: spender.as_bytes().to_vec(),
        treasury_amount: sum_treasury_outputs_v1(details),
        output_lock_root: TREASURY_LOCK_ROOT_B58.to_string(),
    }
}

fn note_data_from_proto(data: &PbNoteData) -> NoteData {
    NoteData::new(
        data.entries
            .iter()
            .map(|entry| NoteDataEntry::new(entry.key.clone(), entry.blob.clone().into()))
            .collect(),
    )
}
