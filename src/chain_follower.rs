use std::time::Duration;
use std::future::Future;

use nockapp::wire::{SystemWire, Wire};
use tokio::task::JoinHandle;

use crate::chain::{
    confirmed_tx_position, fetch_header_chain, plan_anchor_advance, AnchorAdvanceTarget,
    ConfirmedTxPosition,
};
use crate::kernel::{
    build_advance_tip_poke, build_anchor_peek, build_claim_poke, decode_anchor,
    first_anchor_advanced, first_claim_count_bumped, first_error_message, first_primary_set,
    has_effect, AnchorHeader,
};
use crate::state::SharedState;
use crate::types::{ClaimLifecycleStatus, Registration, RegistrationStatus};

const FOLLOWER_POLL: Duration = Duration::from_secs(2);
const ANCHOR_POLL: Duration = Duration::from_secs(10);

/// How far behind the chain tip the follower waits before committing a
/// block to the kernel's anchor. Keeps Phase 3's STARK reasoning free
/// of short reorgs without waiting on economic finality.
pub const DEFAULT_FINALITY_DEPTH: u64 = 10;

/// Max headers the follower ingests into a single `%advance-tip` poke.
/// Bounded well under the kernel's `max-anchor-headers = 1024` so the
/// deque never drops an entry mid-advance.
pub const DEFAULT_MAX_ADVANCE_BATCH: u64 = 64;

/// Spawn both follower loops: claim replay + anchor advance. Both run
/// forever on the current tokio runtime; cancel by dropping the task
/// handles (we don't hold onto them in production yet).
pub fn spawn(state: SharedState) -> JoinHandle<()> {
    let claim_task = {
        let s = state.clone();
        tokio::spawn(async move {
            loop {
                if let Err(err) = tick_once(&s).await {
                    tracing::warn!("chain follower claim tick failed: {err}");
                }
                tokio::time::sleep(FOLLOWER_POLL).await;
            }
        })
    };

    let _anchor_task = {
        let s = state.clone();
        tokio::spawn(async move {
            loop {
                match advance_anchor_once(&s).await {
                    Ok(Some(advanced)) => {
                        tracing::info!(
                            tip_height = advanced.tip_height,
                            count = advanced.count,
                            "chain follower advanced anchor"
                        );
                    }
                    Ok(None) => {}
                    Err(err) => {
                        tracing::warn!("chain follower anchor tick failed: {err}");
                    }
                }
                tokio::time::sleep(ANCHOR_POLL).await;
            }
        })
    };

    claim_task
}

pub async fn process_once(state: &SharedState) -> Result<(), String> {
    tick_once(state).await
}

async fn tick_once(state: &SharedState) -> Result<(), String> {
    process_once_with_position_lookup(state, |endpoint, tx_hash| async move {
        confirmed_tx_position(&endpoint, &tx_hash).await
    })
    .await
}

/// Process one follower pass with an injectable chain position lookup.
///
/// This is primarily a testing seam so integration tests can provide a
/// deterministic mocked chain order for pending tx hashes.
pub async fn process_once_with_position_lookup<F, Fut>(
    state: &SharedState,
    mut lookup: F,
) -> Result<(), String>
where
    F: FnMut(String, String) -> Fut,
    Fut: Future<Output = Result<Option<ConfirmedTxPosition>, String>>,
{
    let (pending, is_local_mode, chain_endpoint) = {
        let st = state.lock().await;
        (
            st.mirror.pending_claims_in_order(),
            matches!(st.settlement.mode, vesl_core::SettlementMode::Local),
            st.settlement.chain_endpoint.clone(),
        )
    };
    if pending.is_empty() {
        return Ok(());
    }

    let mut ready = Vec::new();
    for claim in pending {
        if is_local_mode || chain_endpoint.is_none() {
            ready.push((0_u64, 0_u64, claim));
            continue;
        }
        let endpoint = chain_endpoint.clone().unwrap_or_default();
        if let Some(pos) = lookup(endpoint, claim.tx_hash.clone()).await? {
            ready.push((pos.block_height, pos.tx_index_in_block, claim));
        }
    }

    ready.sort_by(|(h1, i1, c1), (h2, i2, c2)| {
        h1.cmp(h2)
            .then_with(|| i1.cmp(i2))
            .then_with(|| c1.submit_seq.cmp(&c2.submit_seq))
            .then_with(|| c1.tx_hash.cmp(&c2.tx_hash))
    });

    for (_height, _index, claim) in ready {
        let mut st = state.lock().await;
        let current = match st.mirror.submitted_claims.get(&claim.claim_id) {
            Some(c) => c.clone(),
            None => continue,
        };
        if !matches!(current.status, ClaimLifecycleStatus::Submitted | ClaimLifecycleStatus::Confirmed) {
            continue;
        }
        st.mirror
            .update_claim_status(&claim.claim_id, ClaimLifecycleStatus::Confirmed, None);

        let effects = match st
            .app
            .poke(
                nockapp::wire::SystemWire.to_wire(),
                build_claim_poke(&claim.name, &claim.address, claim.fee, &claim.tx_hash),
            )
            .await
        {
            Ok(e) => e,
            Err(e) => {
                st.mirror.update_claim_status(
                    &claim.claim_id,
                    ClaimLifecycleStatus::Rejected,
                    Some(format!("kernel claim poke failed: {e:?}")),
                );
                st.persist_all().await;
                continue;
            }
        };

        if let Some(err) = first_error_message(&effects) {
            st.mirror.update_claim_status(
                &claim.claim_id,
                ClaimLifecycleStatus::Rejected,
                Some(err),
            );
            st.persist_all().await;
            continue;
        }
        if !has_effect(&effects, "claimed") {
            st.mirror.update_claim_status(
                &claim.claim_id,
                ClaimLifecycleStatus::Rejected,
                Some("missing %claimed effect".into()),
            );
            st.persist_all().await;
            continue;
        }

        let now = crate::api::now_millis_for_internal();
        let reg = Registration {
            address: claim.address.clone(),
            name: claim.name.clone(),
            status: RegistrationStatus::Registered,
            timestamp: now,
            date: Some(crate::api::iso8601_for_internal(now)),
            tx_hash: Some(claim.tx_hash.clone()),
        };
        st.mirror.insert(reg);
        if let Some((addr, primary_name)) = first_primary_set(&effects) {
            st.mirror.set_primary(addr, primary_name);
        }
        if let Some(bumped) = first_claim_count_bumped(&effects) {
            st.mirror
                .set_snapshot(bumped.claim_count, &bumped.hull, &bumped.root);
        }
        st.mirror
            .update_claim_status(&claim.claim_id, ClaimLifecycleStatus::Finalized, None);
        st.persist_all().await;
    }
    Ok(())
}

/// Outcome of one anchor-advance pass.
#[derive(Debug, Clone)]
pub struct AnchorAdvanceOutcome {
    pub tip_height: u64,
    pub tip_digest: Vec<u8>,
    pub count: u64,
}

/// One pass of the anchor-advance loop.
///
/// 1. Peek the kernel's `/anchor` to learn the current tip height.
/// 2. Ask the chain (via `plan_anchor_advance`) for the next range to
///    ingest — bounded by `DEFAULT_FINALITY_DEPTH` and `DEFAULT_MAX_ADVANCE_BATCH`.
/// 3. Fetch headers for that range and issue one `%advance-tip` poke.
/// 4. Surface any `%anchor-error` as a follower warning.
///
/// Returns `Ok(None)` when there's nothing to advance (local mode, no
/// chain endpoint, anchor already at the finality horizon, or we're
/// racing a very young chain).
pub async fn advance_anchor_once(
    state: &SharedState,
) -> Result<Option<AnchorAdvanceOutcome>, String> {
    let (is_local_mode, chain_endpoint) = {
        let st = state.lock().await;
        (
            matches!(st.settlement.mode, vesl_core::SettlementMode::Local),
            st.settlement.chain_endpoint.clone(),
        )
    };
    if is_local_mode {
        return Ok(None);
    }
    let Some(endpoint) = chain_endpoint else {
        return Ok(None);
    };

    // Read the kernel's current anchor height via peek. Missing peek
    // responses mean the kernel is not ready; treat as transient.
    let current_anchor_height = {
        let mut st = state.lock().await;
        let result = st
            .app
            .peek(build_anchor_peek())
            .await
            .map_err(|e| format!("anchor peek failed: {e:?}"))?;
        match decode_anchor(&result) {
            Ok(view) => view.tip_height,
            Err(_) => 0,
        }
    };

    let Some(plan) = plan_anchor_advance(
        &endpoint,
        current_anchor_height,
        DEFAULT_FINALITY_DEPTH,
        DEFAULT_MAX_ADVANCE_BATCH,
    )
    .await?
    else {
        return Ok(None);
    };
    let AnchorAdvanceTarget {
        from_height,
        to_height,
        current_chain_tip: _,
    } = plan;

    let headers: Vec<AnchorHeader> = fetch_header_chain(&endpoint, from_height, to_height)
        .await
        .map_err(|e| format!("header chain fetch failed [{from_height}..{to_height}]: {e}"))?;
    if headers.is_empty() {
        return Ok(None);
    }

    let mut st = state.lock().await;
    let effects = st
        .app
        .poke(SystemWire.to_wire(), build_advance_tip_poke(&headers))
        .await
        .map_err(|e| format!("advance-tip poke failed: {e:?}"))?;

    if let Some(err) = first_error_message(&effects) {
        return Err(format!("kernel rejected %advance-tip: {err}"));
    }
    let Some(advanced) = first_anchor_advanced(&effects) else {
        return Err("kernel did not emit %anchor-advanced".into());
    };
    st.persist_all().await;

    Ok(Some(AnchorAdvanceOutcome {
        tip_height: advanced.tip_height,
        tip_digest: advanced.tip_digest,
        count: advanced.count,
    }))
}
