use std::time::Duration;

use nockapp::wire::{SystemWire, Wire};
use nockapp_grpc::pb::common::v2::NoteData as PbNoteData;
use nockapp_grpc::pb::public::v2::TransactionDetails;
use nockchain_client_rs::{NoteData, NoteDataEntry};
use tokio::task::JoinHandle;

use crate::chain::{
    base58_hash_to_atom_bytes, fetch_block_details_by_height, fetch_block_transaction_details,
    fetch_current_tip_height, tx_ids_from_block_details,
};
use crate::claim_note::ClaimNoteV1;
use crate::kernel::{
    build_scan_block_poke, build_scan_state_peek, decode_scan_state, first_error_message,
    first_scan_block_done, first_scan_block_error, ClaimCandidate, ClaimWitness,
};
use crate::payment::{fee_for_name, sum_treasury_outputs_v1, TREASURY_LOCK_ROOT_B58};
use crate::state::SharedState;

/// Sleep between ticks **only** when there is nothing to scan (caught up
/// within finality) or after an error — avoids gRPC busy-loops. While a
/// finalized backlog exists, consecutive `%scan-block` steps run back-to-back.
const FOLLOWER_POLL: Duration = Duration::from_secs(2);

/// How far behind the chain tip the follower waits before committing a
/// block to the kernel scan cursor. Keeps Path Y scans free of short
/// reorgs without waiting on economic finality.
pub const DEFAULT_FINALITY_DEPTH: u64 = 10;

/// Transitional compatibility for status/admin JSON while the API is renamed
/// from "anchor advance" to "block scan". Path Y scans one block per tick.
pub const DEFAULT_MAX_ADVANCE_BATCH: u64 = 1;

/// Spawn the Path Y block scanner. It advances the kernel one finalized
/// Nockchain block at a time with `%scan-block`.
pub fn spawn(state: SharedState) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let idle = match scan_once(&state).await {
                Ok(Some(scanned)) => {
                    tracing::info!(
                        height = scanned.height,
                        phase = "scan_block",
                        "chain follower scanned block"
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

/// Outcome of one block-scan pass.
#[derive(Debug, Clone)]
pub struct ScanBlockOutcome {
    pub height: u64,
    pub digest: Vec<u8>,
    pub accumulator_root: Vec<u8>,
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
        count: 1,
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

    let details = fetch_block_details_by_height(&endpoint, next_height).await?;
    let page_digest = details
        .block_id
        .as_ref()
        .ok_or_else(|| format!("block {next_height} missing block_id"))?;
    let parent = details
        .parent
        .as_ref()
        .ok_or_else(|| format!("block {next_height} missing parent"))?;
    let page_digest = crate::chain::hash_to_atom_bytes(page_digest);
    let parent = crate::chain::hash_to_atom_bytes(parent);
    let page_tx_ids = tx_ids_from_block_details(&details)?;
    let tx_details = fetch_block_transaction_details(&endpoint, &details).await?;
    let candidates = extract_claim_candidates(&tx_details)?;

    let poke_result = {
        let mut k = state.kernel.lock().await;
        k.poke(
            SystemWire.to_wire(),
            build_scan_block_poke(
                &parent,
                details.height,
                &page_digest,
                &page_tx_ids,
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
        let msg = "kernel did not emit %scan-block-done".to_string();
        let ts = crate::state::AppState::now_epoch_ms();
        let mut h = state.hull.lock().await;
        h.follower.record_error("scan_poke", msg.clone(), ts);
        return Err(msg);
    };

    let now = crate::state::AppState::now_epoch_ms();
    {
        let mut h = state.hull.lock().await;
        h.follower.record_advance(done.height, 1, now);
    }
    state.persist_all().await;

    Ok(Some(ScanBlockOutcome {
        height: done.height,
        digest: done.digest,
        accumulator_root: done.accumulator_root,
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
