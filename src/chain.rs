use std::time::Duration;

use nockchain_client_rs::{ChainClient, ChainConfig};
use nockapp_grpc::pb::common::v1::Base58Hash;
use nockapp_grpc::pb::public::v2::{
    get_block_details_request, get_block_details_response,
    get_transaction_block_response, GetTransactionBlockRequest,
    GetBlockDetailsRequest,
};
use nockapp_grpc::pb::public::v2::nockchain_block_service_client::NockchainBlockServiceClient;
use crate::claim_note::ClaimNoteV1;

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
