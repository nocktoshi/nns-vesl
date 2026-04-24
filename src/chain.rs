use std::time::Duration;

use nockchain_client_rs::{ChainClient, ChainConfig};
use nockapp_grpc::pb::common::v1::{Base58Hash, Belt, Hash};
use nockapp_grpc::pb::public::v2::{
    get_block_details_request, get_block_details_response,
    get_blocks_response, get_transaction_block_response, get_transaction_details_response,
    BlockDetails, GetBlockDetailsRequest, GetBlocksRequest, GetTransactionBlockRequest,
    GetTransactionDetailsRequest, TransactionDetails,
};
use nockapp_grpc::pb::public::v2::nockchain_block_service_client::NockchainBlockServiceClient;
use nockapp_grpc::pb::common::v1::PageRequest;
use crate::claim_note::ClaimNoteV1;
use crate::kernel::AnchorHeader;

/// Best-effort chain acceptance check for a base58 tx id.
pub async fn transaction_is_accepted(
    endpoint: &str,
    accept_timeout_secs: u64,
    tx_id_base58: &str,
) -> Result<bool, String> {
    let mut cfg = ChainConfig::local(endpoint);
    cfg.accept_timeout = Duration::from_secs(accept_timeout_secs.max(1));
    let mut client = ChainClient::connect(cfg)
        .await
        .map_err(|e| format!("chain connect failed: {e}"))?;
    client
        .check_accepted(tx_id_base58)
        .await
        .map_err(|e| format!("acceptance query failed: {e}"))
}

/// Query chain for the block height that included a tx.
///
/// Returns:
/// - `Ok(Some(height))` when the tx is in a block
/// - `Ok(None)` when the tx is still pending
/// - `Err(...)` on transport or server failures
pub async fn transaction_block_height(
    endpoint: &str,
    tx_id_base58: &str,
) -> Result<Option<u64>, String> {
    let mut client = NockchainBlockServiceClient::connect(endpoint.to_string())
        .await
        .map_err(|e| format!("block service connect failed: {e}"))?;
    let req = GetTransactionBlockRequest {
        tx_id: Some(Base58Hash {
            hash: tx_id_base58.to_string(),
        }),
    };
    let res = client
        .get_transaction_block(req)
        .await
        .map_err(|e| format!("transaction block query failed: {e}"))?
        .into_inner();
    match res.result {
        Some(get_transaction_block_response::Result::Block(block)) => Ok(Some(block.height)),
        Some(get_transaction_block_response::Result::Pending(_)) => Ok(None),
        Some(get_transaction_block_response::Result::Error(err)) => Err(err.message),
        None => Ok(None),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConfirmedTxPosition {
    pub block_height: u64,
    pub tx_index_in_block: u64,
}

/// Query chain for canonical ordering position of a confirmed tx.
///
/// Returns:
/// - `Ok(Some(position))` when tx is mined and found in block tx list
/// - `Ok(None)` when tx is pending
/// - `Err(...)` on transport/server failures or inconsistent block data
pub async fn confirmed_tx_position(
    endpoint: &str,
    tx_id_base58: &str,
) -> Result<Option<ConfirmedTxPosition>, String> {
    let mut client = NockchainBlockServiceClient::connect(endpoint.to_string())
        .await
        .map_err(|e| format!("block service connect failed: {e}"))?;
    let tx_req = GetTransactionBlockRequest {
        tx_id: Some(Base58Hash {
            hash: tx_id_base58.to_string(),
        }),
    };
    let tx_res = client
        .get_transaction_block(tx_req)
        .await
        .map_err(|e| format!("transaction block query failed: {e}"))?
        .into_inner();
    let block_height = match tx_res.result {
        Some(get_transaction_block_response::Result::Block(block)) => block.height,
        Some(get_transaction_block_response::Result::Pending(_)) => return Ok(None),
        Some(get_transaction_block_response::Result::Error(err)) => return Err(err.message),
        None => return Ok(None),
    };

    let details_req = GetBlockDetailsRequest {
        selector: Some(get_block_details_request::Selector::Height(block_height)),
    };
    let details_res = client
        .get_block_details(details_req)
        .await
        .map_err(|e| format!("block details query failed: {e}"))?
        .into_inner();
    let details = match details_res.result {
        Some(get_block_details_response::Result::Details(details)) => details,
        Some(get_block_details_response::Result::Error(err)) => return Err(err.message),
        None => {
            return Err(format!(
                "missing block details for confirmed tx at height {block_height}"
            ))
        }
    };

    let tx_index_in_block = details
        .tx_ids
        .iter()
        .position(|h| h.hash == tx_id_base58)
        .map(|idx| idx as u64)
        .ok_or_else(|| {
            format!("tx {tx_id_base58} not found in block tx list at height {block_height}")
        })?;

    Ok(Some(ConfirmedTxPosition {
        block_height,
        tx_index_in_block,
    }))
}

/// Attempt to post settlement receipt metadata to chain.
///
/// The current implementation returns a deterministic local marker in
/// local mode and validates chain connectivity in submit modes.
pub async fn post_settlement_receipt(
    cfg: &vesl_core::SettlementConfig,
    note_id_hex: &str,
) -> Result<Option<String>, String> {
    if matches!(cfg.mode, vesl_core::SettlementMode::Local) {
        return Ok(None);
    }
    let endpoint = cfg
        .chain_endpoint
        .as_deref()
        .ok_or_else(|| "chain endpoint not configured".to_string())?;
    // Connectivity probe so settlement surfaces actionable failures.
    let mut client = ChainClient::connect(ChainConfig::local(endpoint))
        .await
        .map_err(|e| format!("chain connect failed: {e}"))?;
    // Probe with a known-false tx id to verify RPC reachability.
    let _ = client
        .check_accepted("11111111111111111111111111111111111111111111111111111111111")
        .await
        .map_err(|e| format!("chain probe failed: {e}"))?;
    Ok(Some(format!("queued-{note_id_hex}")))
}

/// Submit a claim note to chain.
///
/// Current implementation validates that chain RPC is reachable in submit
/// modes and returns a synthetic submission id for tracking.
pub async fn submit_claim_note(
    cfg: &vesl_core::SettlementConfig,
    note: &ClaimNoteV1,
) -> Result<Option<String>, String> {
    if matches!(cfg.mode, vesl_core::SettlementMode::Local) {
        return Ok(None);
    }
    let endpoint = cfg
        .chain_endpoint
        .as_deref()
        .ok_or_else(|| "chain endpoint not configured".to_string())?;
    let mut client = ChainClient::connect(ChainConfig::local(endpoint))
        .await
        .map_err(|e| format!("chain connect failed: {e}"))?;
    let _ = client
        .check_accepted("11111111111111111111111111111111111111111111111111111111111")
        .await
        .map_err(|e| format!("chain probe failed: {e}"))?;
    let payload_len = note.jam_tuple().len();
    Ok(Some(format!("queued-{}-{payload_len}", note.claim_id)))
}

// =========================================================================
// Phase 2c — chain-input fetchers
// =========================================================================

/// Encode a `common.v1.Hash` (5×Belt) as the 40-byte LE-packed atom
/// shape the kernel uses (matches `noun-digest:tip5` on the Hoon side:
/// `[@ux @ux @ux @ux @ux]` where each `@ux` is a single Goldilocks
/// felt and the whole tuple reads as a Tip5 digest).
pub fn hash_to_atom_bytes(h: &Hash) -> Vec<u8> {
    let mut out = Vec::with_capacity(40);
    for b in [&h.belt_1, &h.belt_2, &h.belt_3, &h.belt_4, &h.belt_5] {
        let v = b.as_ref().map(|bb| bb.value).unwrap_or_default();
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}

/// Build a `common.v1.Hash` from 40 bytes of LE packed felts. Returns
/// `None` if the slice is not exactly 40 bytes.
pub fn atom_bytes_to_hash(bytes: &[u8]) -> Option<Hash> {
    if bytes.len() != 40 {
        return None;
    }
    let mut belts = [0u64; 5];
    for (i, b) in belts.iter_mut().enumerate() {
        let mut tmp = [0u8; 8];
        tmp.copy_from_slice(&bytes[i * 8..(i + 1) * 8]);
        *b = u64::from_le_bytes(tmp);
    }
    Some(Hash {
        belt_1: Some(Belt { value: belts[0] }),
        belt_2: Some(Belt { value: belts[1] }),
        belt_3: Some(Belt { value: belts[2] }),
        belt_4: Some(Belt { value: belts[3] }),
        belt_5: Some(Belt { value: belts[4] }),
    })
}

/// Connect a `NockchainBlockServiceClient` against `endpoint`.
async fn connect_block_service(
    endpoint: &str,
) -> Result<NockchainBlockServiceClient<tonic::transport::Channel>, String> {
    NockchainBlockServiceClient::connect(endpoint.to_string())
        .await
        .map_err(|e| format!("block service connect failed: {e}"))
}

/// Fetch `BlockDetails` by height from the node's public v2 API.
///
/// Phase 3 will feed this structure (block_id, parent, tx_ids, pow) into
/// the recursive `nns-gate` circuit. Today this is used by the follower
/// to build `%advance-tip` payloads and, once Phase 4 lands, by
/// `/claim` to attach the containing block to the claim-note.
pub async fn fetch_block_details_by_height(
    endpoint: &str,
    height: u64,
) -> Result<BlockDetails, String> {
    let mut client = connect_block_service(endpoint).await?;
    let req = GetBlockDetailsRequest {
        selector: Some(get_block_details_request::Selector::Height(height)),
    };
    let res = client
        .get_block_details(req)
        .await
        .map_err(|e| format!("block details query failed at height {height}: {e}"))?
        .into_inner();
    match res.result {
        Some(get_block_details_response::Result::Details(d)) => Ok(d),
        Some(get_block_details_response::Result::Error(err)) => Err(format!(
            "block details error at height {height}: {}",
            err.message
        )),
        None => Err(format!("empty block details response at height {height}")),
    }
}

/// Convert `BlockDetails` → `AnchorHeader` for Phase 2a's kernel
/// `%advance-tip` cause. Returns `Err` if mandatory hash fields are
/// missing from the proto (indicates node API is serving inconsistent
/// data — surface loudly rather than silently ingesting garbage).
pub fn anchor_header_from_details(details: &BlockDetails) -> Result<AnchorHeader, String> {
    let digest = details
        .block_id
        .as_ref()
        .ok_or_else(|| "anchor: BlockDetails.block_id missing".to_string())?;
    let parent = details
        .parent
        .as_ref()
        .ok_or_else(|| "anchor: BlockDetails.parent missing".to_string())?;
    Ok(AnchorHeader {
        digest: hash_to_atom_bytes(digest),
        height: details.height,
        parent: hash_to_atom_bytes(parent),
    })
}

/// Fetch the block PoW STARK proof (JAM bytes) for the block at
/// `height`. Returns `Ok(None)` when the block has no PoW (genesis /
/// pre-PoW-activation blocks). Phase 3 will embed these bytes as the
/// `proof:sp` input to the recursive `nns-gate` circuit.
pub async fn fetch_block_proof_bytes(
    endpoint: &str,
    height: u64,
) -> Result<Option<Vec<u8>>, String> {
    let details = fetch_block_details_by_height(endpoint, height).await?;
    let Some(pow) = details.pow else {
        return Ok(None);
    };
    if !pow.present {
        return Ok(None);
    }
    Ok(pow.raw_proof)
}

/// Transaction-level structured details from the node. Phase 3 will
/// reshape this into the `raw-tx:t` noun the circuit consumes; for now
/// the hull just round-trips the proto and caches it in the claim note.
pub async fn fetch_transaction_details(
    endpoint: &str,
    tx_id_base58: &str,
) -> Result<TransactionDetails, String> {
    let mut client = connect_block_service(endpoint).await?;
    let req = GetTransactionDetailsRequest {
        tx_id: Some(Base58Hash {
            hash: tx_id_base58.to_string(),
        }),
    };
    let res = client
        .get_transaction_details(req)
        .await
        .map_err(|e| format!("transaction details query failed for {tx_id_base58}: {e}"))?
        .into_inner();
    match res.result {
        Some(get_transaction_details_response::Result::Details(d)) => Ok(d),
        Some(get_transaction_details_response::Result::Pending(_)) => {
            Err(format!("tx {tx_id_base58} is pending; no block yet"))
        }
        Some(get_transaction_details_response::Result::Error(err)) => Err(format!(
            "transaction details error for {tx_id_base58}: {}",
            err.message
        )),
        None => Err(format!(
            "empty transaction details response for {tx_id_base58}"
        )),
    }
}

/// Composite fetch: given a confirmed tx id, return its containing
/// block's `BlockDetails` plus that block's PoW proof bytes (if any).
/// This is the per-claim bundle Phase 4's follower will embed into the
/// kernel `%claim` poke for in-gate verification.
#[derive(Debug, Clone)]
pub struct ClaimBlockBundle {
    pub block: BlockDetails,
    pub block_proof: Option<Vec<u8>>,
}

pub async fn fetch_page_for_tx(
    endpoint: &str,
    tx_id_base58: &str,
) -> Result<ClaimBlockBundle, String> {
    let mut client = connect_block_service(endpoint).await?;
    let tx_req = GetTransactionBlockRequest {
        tx_id: Some(Base58Hash {
            hash: tx_id_base58.to_string(),
        }),
    };
    let tx_res = client
        .get_transaction_block(tx_req)
        .await
        .map_err(|e| format!("transaction block query failed: {e}"))?
        .into_inner();
    let height = match tx_res.result {
        Some(get_transaction_block_response::Result::Block(b)) => b.height,
        Some(get_transaction_block_response::Result::Pending(_)) => {
            return Err(format!("tx {tx_id_base58} not yet in a block"));
        }
        Some(get_transaction_block_response::Result::Error(err)) => {
            return Err(err.message);
        }
        None => return Err(format!("empty tx-block response for {tx_id_base58}")),
    };
    let block = fetch_block_details_by_height(endpoint, height).await?;
    let block_proof = block
        .pow
        .as_ref()
        .filter(|p| p.present)
        .and_then(|p| p.raw_proof.clone());
    Ok(ClaimBlockBundle { block, block_proof })
}

/// Fetch the open-ended inclusive range of headers
/// `[from_height .. to_height]` from the node, producing one
/// `AnchorHeader` per block. Intended to build `%advance-tip` payloads;
/// callers should keep the range small (bounded by kernel
/// `DEFAULT_MAX_ADVANCE_BATCH = 64`). Errors fail fast on the first
/// missing block so partial advances never reach the kernel.
pub async fn fetch_header_chain(
    endpoint: &str,
    from_height: u64,
    to_height: u64,
) -> Result<Vec<AnchorHeader>, String> {
    if to_height < from_height {
        return Ok(Vec::new());
    }
    let mut out = Vec::with_capacity((to_height - from_height + 1) as usize);
    for h in from_height..=to_height {
        let details = fetch_block_details_by_height(endpoint, h).await?;
        out.push(anchor_header_from_details(&details)?);
    }
    Ok(out)
}

/// Light read of the current chain tip height.
///
/// Uses `GetBlocks` with page size 1 (newest-first) and extracts
/// `current_height`. Cheap enough to call every follower tick.
pub async fn fetch_current_tip_height(endpoint: &str) -> Result<u64, String> {
    let mut client = connect_block_service(endpoint).await?;
    let req = GetBlocksRequest {
        page: Some(PageRequest {
            page_token: String::new(),
            client_page_items_limit: 1,
            ..Default::default()
        }),
    };
    let res = client
        .get_blocks(req)
        .await
        .map_err(|e| format!("GetBlocks query failed: {e}"))?
        .into_inner();
    match res.result {
        Some(get_blocks_response::Result::Blocks(b)) => Ok(b.current_height),
        Some(get_blocks_response::Result::Error(e)) => Err(e.message),
        None => Err("empty GetBlocks response".into()),
    }
}

/// Phase 2c headline helper: plan the next anchor advance given the
/// kernel's current anchor height, a configurable finality depth, and
/// a per-tick header budget.
///
/// Returns `Some(target)` when the caller should fetch
/// `[from_height .. to_height]` and issue `%advance-tip`, or `None`
/// when the anchor is already at or past the finality horizon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorAdvanceTarget {
    pub from_height: u64,
    pub to_height: u64,
    pub current_chain_tip: u64,
}

pub async fn plan_anchor_advance(
    endpoint: &str,
    current_anchor_height: u64,
    finality_depth: u64,
    max_batch: u64,
) -> Result<Option<AnchorAdvanceTarget>, String> {
    let tip = fetch_current_tip_height(endpoint).await?;
    if tip <= finality_depth {
        return Ok(None);
    }
    let horizon = tip - finality_depth;
    if horizon <= current_anchor_height {
        return Ok(None);
    }
    let from = current_anchor_height + 1;
    let to = from
        .saturating_add(max_batch.saturating_sub(1))
        .min(horizon);
    Ok(Some(AnchorAdvanceTarget {
        from_height: from,
        to_height: to,
        current_chain_tip: tip,
    }))
}
