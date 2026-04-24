use std::time::Duration;
use std::future::Future;

use nockapp::wire::Wire;
use tokio::task::JoinHandle;

use crate::chain::{confirmed_tx_position, ConfirmedTxPosition};
use crate::kernel::{build_claim_poke, first_claim_id_bumped, first_error_message, first_primary_set, has_effect};
use crate::state::SharedState;
use crate::types::{ClaimLifecycleStatus, Registration, RegistrationStatus};

const FOLLOWER_POLL: Duration = Duration::from_secs(2);

/// Spawn a lightweight follower loop.
///
/// Current implementation replays submitted claim notes from the mirror
/// queue after they are accepted on-chain (or immediately in local mode).
pub fn spawn(state: SharedState) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            if let Err(err) = tick_once(&state).await {
                tracing::warn!("chain follower tick failed: {err}");
            }
            tokio::time::sleep(FOLLOWER_POLL).await;
        }
    })
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
        if let Some(bumped) = first_claim_id_bumped(&effects) {
            st.mirror
                .set_snapshot(bumped.claim_id, &bumped.hull, &bumped.root);
        }
        st.mirror
            .update_claim_status(&claim.claim_id, ClaimLifecycleStatus::Applied, None);
        st.persist_all().await;
    }
    Ok(())
}
