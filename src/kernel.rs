//! NockApp poke construction + effect/peek inspection.
//!
//! The kernel is a `data-registry`-shaped NockApp with the Vesl
//! graft wired in. Four poke shapes are in use:
//!
//!   - `%claim` — hot path. The hull sends one per `POST /claim`
//!     and the kernel writes `names` + `tx-hashes`, bumps the
//!     claim-count counter, recomputes the Merkle root over the full
//!     `names` map, and auto-registers a fresh hull in the graft.
//!     Effects: `%claimed`, optional `%primary-set`,
//!     `%claim-count-bumped`, and graft's `%vesl-registered`. Returns
//!     `%claim-error <msg>` on a user-visible failure without
//!     mutating state.
//!
//!   - `%set-primary` — owner-gated reverse-lookup update. Writes
//!     `primaries` only; does NOT bump the claim-count. Effects:
//!     `%primary-set` on success, `%primary-error <msg>` on
//!     rejection.
//!
//!   - `%settle-batch` — batch settlement. Kernel-side: selects
//!     every name with `entry.claim-count > last-settled-claim-id`,
//!     builds one `graft-payload` holding the whole batch, and
//!     internally dispatches a single `%vesl-settle`. Effects:
//!     `%batch-settled claim-id count note-id` plus graft's
//!     `%vesl-settled` on success, or `%batch-error <msg>` when
//!     there is nothing to settle, or `%vesl-error` passthrough.
//!
//!   - `%vesl-register` — normally driven by `%claim` internally;
//!     kept as a direct poke for manual re-registration of
//!     historical roots.
//!
//! Peek paths (see `hoon/app/app.hoon::peek`):
//!   `/owner/<name>`, `/primary/<addr>`, `/entries`, `/claim-count`,
//!   `/last-settled`, `/hull`, `/root`, `/snapshot`, `/proof/<name>`,
//!   `/pending-batch`, plus the graft's `/registered/<hull>`,
//!   `/settled/<note-id>`, `/root/<hull>`.

use nock_noun_rs::{
    atom_from_u64, jam_to_bytes, make_atom_in, make_cord_in, make_tag_in, new_stack, NounSlab,
};
use nockvm::noun::{Noun, D, T};

use crate::freshness::{AnchorBindingError, Freshness, FreshnessError};

// ---------------------------------------------------------------------------
// Poke builders
// ---------------------------------------------------------------------------

/// Build a `[%claim name=@t owner=@t fee=@ud tx-hash=@t]` poke slab.
///
/// Kernel response:
///
///   - `[%claimed name owner tx-hash]` on success; both `names`
///     and `tx-hashes` get updated. Also emits
///     `[%claim-count-bumped claim-count hull root]` and the graft's
///     `[%vesl-registered hull root]` so the hull can update its
///     snapshot cache without an extra peek. On a first claim for
///     this owner, `[%primary-set owner name]` is emitted too.
///   - `[%claim-error 'name already registered']` on duplicate name.
///   - `[%claim-error 'payment already used']` on duplicate tx-hash.
///   - kernel crash (poke returns `Err`) on invalid format/fee.
pub fn build_claim_poke(name: &str, owner: &str, fee: u64, tx_hash: &str) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "claim");
    let name_atom = make_cord_in(&mut slab, name);
    let owner_atom = make_cord_in(&mut slab, owner);
    let fee_atom = atom_from_u64(&mut slab, fee);
    let tx_hash_atom = make_cord_in(&mut slab, tx_hash);
    let poke = T(
        &mut slab,
        &[tag, name_atom, owner_atom, fee_atom, tx_hash_atom],
    );
    slab.set_root(poke);
    slab
}

/// Build a `[%set-primary address=@t name=@t]` poke slab.
///
/// Kernel response:
///
///   - `[%primary-set address name]` on success; `primaries` updated.
///   - `[%primary-error 'name not registered']` if the name does not
///     exist in the registry.
///   - `[%primary-error 'not the owner']` if the caller does not own
///     the target name.
pub fn build_set_primary_poke(address: &str, name: &str) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "set-primary");
    let address_atom = make_cord_in(&mut slab, address);
    let name_atom = make_cord_in(&mut slab, name);
    let poke = T(&mut slab, &[tag, address_atom, name_atom]);
    slab.set_root(poke);
    slab
}

/// Build a `[%settle-batch ~]` poke slab.
///
/// The kernel does the full batch construction internally: selects
/// the pending window, computes every Merkle proof against the
/// current root, bundles them into a single `graft-payload`, and
/// pokes the graft with one `%vesl-settle`.
///
/// Kernel response:
///
///   - `[%batch-settled claim-id count note-id]` + graft's
///     `[%vesl-settled note=[id hull root [%settled ~]]]` on success.
///     The hull advances its `last-settled-claim-id` cache.
///   - `[%batch-error 'nothing to settle']` when the pending window
///     is empty (nothing new since the previous successful settle).
///   - `[%vesl-error msg]` passthrough if the graft rejected the
///     poke (e.g. the exact same batch was already settled).
pub fn build_settle_batch_poke() -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "settle-batch");
    let poke = T(&mut slab, &[tag, D(0)]);
    slab.set_root(poke);
    slab
}

/// Build a `[%prove-batch ~]` poke slab.
///
/// Same batch-selection semantics as `%settle-batch` but additionally
/// runs `prove-computation` over the jammed batch payload to produce
/// a real STARK. On success the kernel emits a `[%batch-proof note-id
/// proof]` effect alongside the usual `[%batch-settled ...]` + graft
/// `[%vesl-settled ...]` effects. On prover crash the kernel emits
/// `[%prove-failed trace-jam]` and does NOT apply settlement.
pub fn build_prove_batch_poke() -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "prove-batch");
    let poke = T(&mut slab, &[tag, D(0)]);
    slab.set_root(poke);
    slab
}

/// Phase 1-redo sanity: `[%prove-identity ~]` poke. Kernel proves
/// the trivial `[42 [0 1]]` computation and immediately verifies it,
/// emitting `[%prove-identity-result ok=?]`. Used by the spike to
/// confirm the prover/verifier pair is self-consistent before
/// attempting verification of NNS-specific batch proofs.
pub fn build_prove_identity_poke() -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "prove-identity");
    let poke = T(&mut slab, &[tag, D(0)]);
    slab.set_root(poke);
    slab
}

/// `ok` from `[%prove-identity-result ok=?]`.
pub fn prove_identity_result(effect: &NounSlab) -> Option<bool> {
    if effect_tag(effect)? != "prove-identity-result" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let v = cell.tail().as_atom().ok()?.as_u64().ok()?;
    Some(v == 0)
}

pub fn first_prove_identity_result(effects: &[NounSlab]) -> Option<bool> {
    effects.iter().find_map(prove_identity_result)
}

/// Build a `[%verify-stark blob=@]` poke slab. `blob` is the raw JAM
/// bytes of a `proof` noun (e.g. from `%batch-proof`). The kernel cues
/// it and runs `verify:nock-verifier` with the same jets as block PoW
/// verification. Emits `[%verify-stark-result ok=?]` or
/// `[%verify-stark-error msg=@t]`.
pub fn build_verify_stark_poke(proof_jam: &[u8]) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "verify-stark");
    let blob = make_atom_in(&mut slab, proof_jam);
    let poke = T(&mut slab, &[tag, blob]);
    slab.set_root(poke);
    slab
}

/// Build `[%verify-stark-explicit blob=@ subject-jam=@ formula-jam=@]`.
///
/// Each jam slice is the raw JAM bytes of the proof noun and of the
/// traced `subject` / `formula` nouns — the same triple cached in
/// `last-proved` for `%verify-stark`. Path Y4 `light_verify` uses this
/// so wallets verify without mutating kernel state.
pub fn build_verify_stark_explicit_poke(
    proof_jam: &[u8],
    subject_jam: &[u8],
    formula_jam: &[u8],
) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "verify-stark-explicit");
    let blob = make_atom_in(&mut slab, proof_jam);
    let sj = make_atom_in(&mut slab, subject_jam);
    let fj = make_atom_in(&mut slab, formula_jam);
    let poke = T(&mut slab, &[tag, blob, sj, fj]);
    slab.set_root(poke);
    slab
}

/// Boot the NNS kernel from `kernel_jam`, poke `%verify-stark-explicit`,
/// and return the loobean from `[%verify-stark-result ok=?]`.
///
/// Uses the same prover hot-state registration as hull tests so
/// `verify:vesl-stark-verifier` jets match `%prove-*` paths. Requires a
/// kernel JAM built from a Hoon source that includes `%verify-stark-explicit`.
pub async fn verify_stark_explicit_offline(
    kernel_jam: &[u8],
    proof_jam: &[u8],
    subject_jam: &[u8],
    formula_jam: &[u8],
) -> Result<bool, String> {
    use nockapp::kernel::boot;
    use nockapp::kernel::boot::NockStackSize;
    use nockapp::wire::{SystemWire, Wire};
    use nockapp::NockApp;

    let dir = std::env::temp_dir().join(format!(
        "nns-light-stark-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;

    let run = async {
        let mut cli = boot::default_boot_cli(true);
        cli.stack_size = NockStackSize::Large;
        let prover_hot_state = zkvm_jetpack::hot::produce_prover_hot_state();
        let mut app: NockApp = boot::setup(
            kernel_jam,
            cli,
            prover_hot_state.as_slice(),
            "nns-light-verify-stark",
            Some(dir.clone()),
        )
        .await
        .map_err(|e| format!("kernel boot: {e:?}"))?;

        let poke = build_verify_stark_explicit_poke(proof_jam, subject_jam, formula_jam);
        let fx = app
            .poke(SystemWire.to_wire(), poke)
            .await
            .map_err(|e| format!("verify poke: {e:?}"))?;

        if let Some(msg) = first_verify_stark_error(&fx) {
            return Err(msg);
        }
        first_verify_stark_result(&fx)
            .ok_or_else(|| "kernel emitted no %verify-stark-result".to_string())
    };

    let out = run.await;
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// [`verify_stark_explicit_offline`] on a fresh current-thread runtime.
pub fn verify_stark_explicit_blocking(
    kernel_jam: &[u8],
    proof_jam: &[u8],
    subject_jam: &[u8],
    formula_jam: &[u8],
) -> Result<bool, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(verify_stark_explicit_offline(
            kernel_jam,
            proof_jam,
            subject_jam,
            formula_jam,
        ))
}

/// Build `[%verify-accumulator-snapshot ...]`.
///
/// `expected_root` is the raw `@` bytes of `(root-atom:na acc)` (same encoding
/// as `/scan-state` `root` and `accumulator_root_hex` from the hull).
pub fn build_verify_accumulator_snapshot_poke(
    expected_root: &[u8],
    acc_jam: &[u8],
    name: &str,
    owner: &str,
    tx_hash: &[u8],
    claim_height: u64,
    block_digest: &[u8],
) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "verify-accumulator-snapshot");
    let er = make_atom_in(&mut slab, expected_root);
    let aj = make_atom_in(&mut slab, acc_jam);
    let name_n = make_cord_in(&mut slab, name);
    let owner_n = make_cord_in(&mut slab, owner);
    let txh = make_atom_in(&mut slab, tx_hash);
    let ch = atom_from_u64(&mut slab, claim_height);
    let bd = make_atom_in(&mut slab, block_digest);
    let poke = T(
        &mut slab,
        &[tag, er, aj, name_n, owner_n, txh, ch, bd],
    );
    slab.set_root(poke);
    slab
}

/// Boot kernel, poke `%verify-accumulator-snapshot`, return loobean from
/// `[%accumulator-snapshot-verify-result ok=?]`.
pub async fn verify_accumulator_snapshot_offline(
    kernel_jam: &[u8],
    expected_root: &[u8],
    acc_jam: &[u8],
    name: &str,
    owner: &str,
    tx_hash: &[u8],
    claim_height: u64,
    block_digest: &[u8],
) -> Result<bool, String> {
    use nockapp::kernel::boot;
    use nockapp::kernel::boot::NockStackSize;
    use nockapp::wire::{SystemWire, Wire};
    use nockapp::NockApp;

    let dir = std::env::temp_dir().join(format!(
        "nns-light-acc-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("temp dir: {e}"))?;

    let run = async {
        let mut cli = boot::default_boot_cli(true);
        cli.stack_size = NockStackSize::Large;
        let prover_hot_state = zkvm_jetpack::hot::produce_prover_hot_state();
        let mut app: NockApp = boot::setup(
            kernel_jam,
            cli,
            prover_hot_state.as_slice(),
            "nns-light-verify-acc",
            Some(dir.clone()),
        )
        .await
        .map_err(|e| format!("kernel boot: {e:?}"))?;

        let poke = build_verify_accumulator_snapshot_poke(
            expected_root,
            acc_jam,
            name,
            owner,
            tx_hash,
            claim_height,
            block_digest,
        );
        let fx = app
            .poke(SystemWire.to_wire(), poke)
            .await
            .map_err(|e| format!("accumulator verify poke: {e:?}"))?;

        if let Some(msg) = first_accumulator_snapshot_verify_error(&fx) {
            return Err(msg);
        }
        first_accumulator_snapshot_verify_result(&fx).ok_or_else(|| {
            "kernel emitted no %accumulator-snapshot-verify-result".to_string()
        })
    };

    let out = run.await;
    let _ = std::fs::remove_dir_all(&dir);
    out
}

/// [`verify_accumulator_snapshot_offline`] on a fresh current-thread runtime.
pub fn verify_accumulator_snapshot_blocking(
    kernel_jam: &[u8],
    expected_root: &[u8],
    acc_jam: &[u8],
    name: &str,
    owner: &str,
    tx_hash: &[u8],
    claim_height: u64,
    block_digest: &[u8],
) -> Result<bool, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(verify_accumulator_snapshot_offline(
            kernel_jam,
            expected_root,
            acc_jam,
            name,
            owner,
            tx_hash,
            claim_height,
            block_digest,
        ))
}

/// One entry in the `%advance-tip` header list.
///
/// `digest` and `parent` are raw 40-byte Tip5 hashes (5 × 8-byte
/// Goldilocks field elements, LE-packed). `height` is the Nockchain
/// `page-number`. The kernel walks these oldest-first, enforcing
/// `header[n].parent == header[n-1].digest` and monotonic heights.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorHeader {
    pub digest: Vec<u8>,
    pub height: u64,
    pub parent: Vec<u8>,
}

/// Build a `[%advance-tip headers=(list anchor-header)]` poke slab.
///
/// `headers` MUST be oldest-first (same convention as the follower
/// receives blocks from Nockchain). On success the kernel emits
/// `[%anchor-advanced tip-digest=@ux tip-height=@ud count=@ud]`; on
/// validation failure it emits `[%anchor-error msg=@t]` and does NOT
/// mutate state.
pub fn build_advance_tip_poke(headers: &[AnchorHeader]) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "advance-tip");

    let mut list_noun = D(0);
    for h in headers.iter().rev() {
        let digest = make_atom_in(&mut slab, &h.digest);
        let parent = make_atom_in(&mut slab, &h.parent);
        let height = atom_from_u64(&mut slab, h.height);
        let cell = T(&mut slab, &[digest, height, parent]);
        list_noun = T(&mut slab, &[cell, list_noun]);
    }

    let poke = T(&mut slab, &[tag, list_noun]);
    slab.set_root(poke);
    slab
}

/// Build a `[%verify-chain-link claim-digest=@ux headers=(list anchor-header) anchored-tip=@ux]`
/// poke slab. Read-only — runs
/// `chain-links-to:nns-predicates` and emits
/// `[%chain-link-result ok=?]` without mutating state.
///
/// Intended for tests + ops tooling; the live Phase 3 circuit will
/// call `chain-links-to` in-gate, not via this poke.
pub fn build_verify_chain_link_poke(
    claim_digest: &[u8],
    headers: &[AnchorHeader],
    anchored_tip: &[u8],
) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "verify-chain-link");
    let cd = make_atom_in(&mut slab, claim_digest);
    let at = make_atom_in(&mut slab, anchored_tip);

    let mut list_noun = D(0);
    for h in headers.iter().rev() {
        let digest = make_atom_in(&mut slab, &h.digest);
        let parent = make_atom_in(&mut slab, &h.parent);
        let height = atom_from_u64(&mut slab, h.height);
        let cell = T(&mut slab, &[digest, height, parent]);
        list_noun = T(&mut slab, &[cell, list_noun]);
    }

    let poke = T(&mut slab, &[tag, cd, list_noun, at]);
    slab.set_root(poke);
    slab
}

/// `ok` from `[%chain-link-result ok=?]`.
pub fn chain_link_result(effect: &NounSlab) -> Option<bool> {
    if effect_tag(effect)? != "chain-link-result" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let v = cell.tail().as_atom().ok()?.as_u64().ok()?;
    Some(v == 0)
}

pub fn first_chain_link_result(effects: &[NounSlab]) -> Option<bool> {
    effects.iter().find_map(chain_link_result)
}

/// Build a `[%verify-tx-in-page digest=@ux tx-ids=(list @ux) claimed-tx-id=@ux]`
/// poke slab. Read-only — the kernel builds the canonical
/// `(z-set @ux)` via `z-silt` (so `gor-tip` ordering is correct),
/// then runs `has-tx-in-page:nns-predicates` and emits
/// `[%tx-in-page-result ok=?]` without mutating state.
///
/// `page_digest` is the 40-byte LE-packed Tip5 block digest.
/// `tx_ids` is the flat list of 40-byte Tip5 tx-ids the block
/// included; order doesn't matter (kernel canonicalises).
/// `claimed_tx_id` is the tx-id we're checking membership for.
pub fn build_verify_tx_in_page_poke(
    page_digest: &[u8],
    tx_ids: &[Vec<u8>],
    claimed_tx_id: &[u8],
) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "verify-tx-in-page");

    let mut list_noun = D(0);
    for id in tx_ids.iter().rev() {
        let key = make_atom_in(&mut slab, id);
        list_noun = T(&mut slab, &[key, list_noun]);
    }

    let digest = make_atom_in(&mut slab, page_digest);
    let claimed = make_atom_in(&mut slab, claimed_tx_id);
    let poke = T(&mut slab, &[tag, digest, list_noun, claimed]);
    slab.set_root(poke);
    slab
}

/// `ok` from `[%tx-in-page-result ok=?]`.
pub fn tx_in_page_result(effect: &NounSlab) -> Option<bool> {
    if effect_tag(effect)? != "tx-in-page-result" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let v = cell.tail().as_atom().ok()?.as_u64().ok()?;
    Some(v == 0)
}

pub fn first_tx_in_page_result(effects: &[NounSlab]) -> Option<bool> {
    effects.iter().find_map(tx_in_page_result)
}

/// Full per-claim bundle fed to the Phase 3c validator. Mirrors
/// `+$claim-bundle` in `hoon/lib/nns-predicates.hoon`.
///
/// All digests (`tx_hash`, `claim_block_digest`, `page_digest`,
/// `anchored_tip`, and every `AnchorHeader.{digest,parent}`) are raw
/// 40-byte LE-packed Tip5 hashes. `page_tx_ids` is the list of all
/// tx-ids in the claim's block — the kernel canonicalises these
/// into the on-chain `(z-set @ux)` via `z-silt` before calling
/// `has-tx-in-page`, so insertion order here doesn't matter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimBundle {
    pub name: String,
    pub owner: String,
    pub fee: u64,
    pub tx_hash: Vec<u8>,
    pub claim_block_digest: Vec<u8>,
    pub anchor_headers: Vec<AnchorHeader>,
    pub page_digest: Vec<u8>,
    pub page_tx_ids: Vec<Vec<u8>>,
    /// Follower-advanced canonical tip digest the bundle's chain
    /// link should resolve to. Hull must set this to
    /// `AnchorView::tip_digest` at bundle-build time.
    pub anchored_tip: Vec<u8>,
    /// Follower-advanced canonical tip height at bundle-build
    /// time. **Phase 7**: `%prove-claim` refuses to emit a proof
    /// unless `anchored_tip_height` equals the kernel's current
    /// `tip-height`; this cryptographically binds the claim proof
    /// to a specific chain snapshot. Wallets later enforce
    /// freshness by checking `anchored_tip_height >= their_chain_tip
    /// - max_staleness`. Default `max_staleness = 20` blocks.
    pub anchored_tip_height: u64,
    /// **Level C-A** payment-semantic witness. Hull extracts these
    /// four fields from the on-chain raw-tx; kernel enforces
    /// `tx-id == claim.tx_hash`, `spender-pkh == claim.owner`,
    /// `treasury-amount >= fee-for-name(name)`, and `%prove-claim`
    /// checks the treasury note output's lock root (see
    /// `output_lock_root` on `ClaimWitness`).
    ///
    /// All four fields are flattened onto the poke payload (rather
    /// than nested) to keep the poke-builder simple.
    pub witness: ClaimWitness,
}

/// Level C-A narrow witness for a claim's on-chain payment. Maps
/// to `nns-raw-tx-witness` in Hoon.
///
/// **Trust model**: the hull parses a real `raw-tx:v1:t` noun from
/// Nockchain and packs the four fields below. The kernel enforces
/// consistency between these fields and the claim tuple. A wallet
/// receiving a proof *should* independently fetch the raw-tx from
/// its own Nockchain view and verify that it matches the witness —
/// this is what makes hull extraction a *falsifiable* trust
/// assumption rather than an unbounded one. See
/// `ARCHITECTURE.md` §10.9 "Level C" for the full trust ladder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimWitness {
    /// Must equal the bundle's `tx_hash`.
    pub tx_id: Vec<u8>,
    /// Paying signer's pkh (atom form). Must equal `claim.owner`
    /// at the kernel's atomic-equality representation.
    pub spender_pkh: Vec<u8>,
    /// Total nicks paid to the treasury address across all
    /// outputs. Must be `>= fee-for-name(claim.name)`.
    pub treasury_amount: u64,
    /// v1: base58 lock root of the treasury payment note output
    /// (`note_name_b58` / NockBlocks lockroot). Poke field remains
    pub output_lock_root: String,
}

/// One `nns/v1/claim` candidate extracted from a Nockchain block.
///
/// This mirrors `+$nns-claim-candidate` in `hoon/lib/nns-predicates.hoon`
/// and is the per-transaction payload folded by `%scan-block`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimCandidate {
    pub name: String,
    pub owner: String,
    pub fee: u64,
    pub tx_hash: Vec<u8>,
    pub witness: ClaimWitness,
}

impl ClaimBundle {
    /// Phase 7 convenience: check this bundle's `anchored_tip_height`
    /// against the wallet's current Nockchain tip view under the
    /// supplied freshness policy.
    ///
    /// Typical wallet flow:
    ///
    /// ```no_run
    /// use nns_vesl::{freshness::Freshness, kernel::ClaimBundle};
    /// # fn example(bundle: &ClaimBundle, chain_tip_height: u64) -> Result<(), Box<dyn std::error::Error>> {
    /// let policy = Freshness::default(); // 20 blocks
    /// bundle.check_freshness(chain_tip_height, policy)?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn check_freshness(
        &self,
        chain_tip_height: u64,
        policy: Freshness,
    ) -> Result<(), FreshnessError> {
        policy.check(self.anchored_tip_height, chain_tip_height)
    }

    /// Phase 7 convenience: check this bundle's committed tip digest
    /// matches the wallet's canonical Nockchain view at the same
    /// height. Use after `check_freshness` passes.
    pub fn check_anchor_binding(
        &self,
        wallet_view_digest: &[u8],
    ) -> Result<(), AnchorBindingError> {
        crate::freshness::check_anchor_binding(&self.anchored_tip, wallet_view_digest)
    }
}

/// Build a `[%scan-block parent height page-digest page-tx-ids candidates]`
/// poke slab.
///
/// This is the Path Y non-recursive precursor: the follower supplies one
/// canonical block, the kernel checks parent/height monotonicity, then folds
/// valid candidates into the accumulator.
pub fn build_scan_block_poke(
    parent: &[u8],
    height: u64,
    page_digest: &[u8],
    page_tx_ids: &[Vec<u8>],
    candidates: &[ClaimCandidate],
) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "scan-block");
    let parent_atom = make_atom_in(&mut slab, parent);
    let height_atom = atom_from_u64(&mut slab, height);
    let page_digest_atom = make_atom_in(&mut slab, page_digest);

    let mut tx_ids_list = D(0);
    for id in page_tx_ids.iter().rev() {
        let tx_id = make_atom_in(&mut slab, id);
        tx_ids_list = T(&mut slab, &[tx_id, tx_ids_list]);
    }

    let mut candidates_list = D(0);
    for c in candidates.iter().rev() {
        let name = make_cord_in(&mut slab, &c.name);
        let owner = make_cord_in(&mut slab, &c.owner);
        let fee = atom_from_u64(&mut slab, c.fee);
        let tx_hash = make_atom_in(&mut slab, &c.tx_hash);
        let w_tx_id = make_atom_in(&mut slab, &c.witness.tx_id);
        let w_spender = make_atom_in(&mut slab, &c.witness.spender_pkh);
        let w_amount = atom_from_u64(&mut slab, c.witness.treasury_amount);
        let w_treasury = make_cord_in(&mut slab, &c.witness.output_lock_root);
        let witness = T(&mut slab, &[w_tx_id, w_spender, w_amount, w_treasury]);
        let candidate = T(&mut slab, &[name, owner, fee, tx_hash, witness]);
        candidates_list = T(&mut slab, &[candidate, candidates_list]);
    }

    let poke = T(
        &mut slab,
        &[
            tag,
            parent_atom,
            height_atom,
            page_digest_atom,
            tx_ids_list,
            candidates_list,
        ],
    );
    slab.set_root(poke);
    slab
}

/// Build a `[%validate-claim ...]` poke slab.
///
/// Kernel runs Level A + Level B + G1/C2 predicates and emits
/// `[%validate-claim-ok ~]` on success, or
/// `[%validate-claim-error <tag>]` where `<tag>` is one of
/// `invalid-name`, `fee-below-schedule`, `page-digest-mismatch`,
/// `tx-not-in-page`, `chain-broken`.
pub fn build_validate_claim_poke(bundle: &ClaimBundle) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "validate-claim");

    let name_atom = make_cord_in(&mut slab, &bundle.name);
    let owner_atom = make_cord_in(&mut slab, &bundle.owner);
    let fee_atom = atom_from_u64(&mut slab, bundle.fee);
    let tx_hash_atom = make_atom_in(&mut slab, &bundle.tx_hash);
    let claim_digest_atom = make_atom_in(&mut slab, &bundle.claim_block_digest);

    let mut headers_list = D(0);
    for h in bundle.anchor_headers.iter().rev() {
        let digest = make_atom_in(&mut slab, &h.digest);
        let parent = make_atom_in(&mut slab, &h.parent);
        let height = atom_from_u64(&mut slab, h.height);
        let cell = T(&mut slab, &[digest, height, parent]);
        headers_list = T(&mut slab, &[cell, headers_list]);
    }

    let page_digest_atom = make_atom_in(&mut slab, &bundle.page_digest);

    let mut tx_ids_list = D(0);
    for id in bundle.page_tx_ids.iter().rev() {
        let key = make_atom_in(&mut slab, id);
        tx_ids_list = T(&mut slab, &[key, tx_ids_list]);
    }

    let anchored_tip_atom = make_atom_in(&mut slab, &bundle.anchored_tip);
    let anchored_tip_height_atom = atom_from_u64(&mut slab, bundle.anchored_tip_height);

    // Level C-A witness: four additional atoms tacked on the end.
    let w_tx_id_atom = make_atom_in(&mut slab, &bundle.witness.tx_id);
    let w_spender_atom = make_atom_in(&mut slab, &bundle.witness.spender_pkh);
    let w_amount_atom = atom_from_u64(&mut slab, bundle.witness.treasury_amount);
    let w_treasury_atom = make_cord_in(&mut slab, &bundle.witness.output_lock_root);

    let poke = T(
        &mut slab,
        &[
            tag,
            name_atom,
            owner_atom,
            fee_atom,
            tx_hash_atom,
            claim_digest_atom,
            headers_list,
            page_digest_atom,
            tx_ids_list,
            anchored_tip_atom,
            anchored_tip_height_atom,
            w_tx_id_atom,
            w_spender_atom,
            w_amount_atom,
            w_treasury_atom,
        ],
    );
    slab.set_root(poke);
    slab
}

/// Result of `%validate-claim`: either a pass, or a rejection tag
/// naming which predicate refused the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidateClaimResult {
    Ok,
    Error(String),
}

/// Extract the first `%validate-claim-ok` / `%validate-claim-error`
/// effect from a poke's effect list. Returns `None` if no such effect
/// was emitted (kernel crash or wrong cause).
pub fn validate_claim_result(effect: &NounSlab) -> Option<ValidateClaimResult> {
    let tag = effect_tag(effect)?;
    match tag.as_str() {
        "validate-claim-ok" => Some(ValidateClaimResult::Ok),
        "validate-claim-error" => {
            let noun = unsafe { effect.root() };
            let cell = noun.as_cell().ok()?;
            let atom = cell.tail().as_atom().ok()?;
            let bytes = atom.as_ne_bytes();
            let s = std::str::from_utf8(bytes)
                .ok()?
                .trim_end_matches('\0')
                .to_string();
            Some(ValidateClaimResult::Error(s))
        }
        _ => None,
    }
}

pub fn first_validate_claim_result(effects: &[NounSlab]) -> Option<ValidateClaimResult> {
    effects.iter().find_map(validate_claim_result)
}

/// Build a `[%prove-claim ...]` poke slab.
///
/// Same payload shape as `%validate-claim`; the kernel runs the
/// validator first and only produces a proof on pass. On rejection
/// the kernel emits the usual `%validate-claim-error <tag>` effect.
///
/// Success effect: `[%claim-proof bundle-digest proof]` — see
/// [`ClaimProof`].
pub fn build_prove_claim_poke(bundle: &ClaimBundle) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "prove-claim");

    let name_atom = make_cord_in(&mut slab, &bundle.name);
    let owner_atom = make_cord_in(&mut slab, &bundle.owner);
    let fee_atom = atom_from_u64(&mut slab, bundle.fee);
    let tx_hash_atom = make_atom_in(&mut slab, &bundle.tx_hash);
    let claim_digest_atom = make_atom_in(&mut slab, &bundle.claim_block_digest);

    let mut headers_list = D(0);
    for h in bundle.anchor_headers.iter().rev() {
        let digest = make_atom_in(&mut slab, &h.digest);
        let parent = make_atom_in(&mut slab, &h.parent);
        let height = atom_from_u64(&mut slab, h.height);
        let cell = T(&mut slab, &[digest, height, parent]);
        headers_list = T(&mut slab, &[cell, headers_list]);
    }

    let page_digest_atom = make_atom_in(&mut slab, &bundle.page_digest);

    let mut tx_ids_list = D(0);
    for id in bundle.page_tx_ids.iter().rev() {
        let key = make_atom_in(&mut slab, id);
        tx_ids_list = T(&mut slab, &[key, tx_ids_list]);
    }

    let anchored_tip_atom = make_atom_in(&mut slab, &bundle.anchored_tip);
    let anchored_tip_height_atom = atom_from_u64(&mut slab, bundle.anchored_tip_height);

    // Level C-A witness.
    let w_tx_id_atom = make_atom_in(&mut slab, &bundle.witness.tx_id);
    let w_spender_atom = make_atom_in(&mut slab, &bundle.witness.spender_pkh);
    let w_amount_atom = atom_from_u64(&mut slab, bundle.witness.treasury_amount);
    let w_treasury_atom = make_cord_in(&mut slab, &bundle.witness.output_lock_root);

    let poke = T(
        &mut slab,
        &[
            tag,
            name_atom,
            owner_atom,
            fee_atom,
            tx_hash_atom,
            claim_digest_atom,
            headers_list,
            page_digest_atom,
            tx_ids_list,
            anchored_tip_atom,
            anchored_tip_height_atom,
            w_tx_id_atom,
            w_spender_atom,
            w_amount_atom,
            w_treasury_atom,
        ],
    );
    slab.set_root(poke);
    slab
}

/// Payload of `[%claim-proof bundle-digest=@ proof=*]` emitted on a
/// successful `%prove-claim`.
///
/// `bundle_digest` is the Goldilocks-belt fold of `(jam bundle)` —
/// the commitment the STARK's Fiat-Shamir absorbed. `proof_jam` is
/// the raw vesl-style `proof:sp` noun JAM'd for transport. Wallet
/// flow:
///
///   1. Receive the bundle + this proof from any NNS server.
///   2. Jam the bundle locally; fold to belt-digest via the same
///      procedure. Must equal `bundle_digest`.
///   3. Verify the STARK via `verify:vesl-verifier` with the
///      recomputed bundle-digest as the subject.
///   4. Re-run `validate_claim_bundle` on the bundle locally.
///   5. Check `(root, hull)` against the expected registry snapshot
///      (e.g. from a `/snapshot` peek or prior commitment).
pub struct ClaimProof {
    pub bundle_digest: Vec<u8>,
    pub proof_jam: Vec<u8>,
}

impl std::fmt::Debug for ClaimProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimProof")
            .field("bundle_digest_len", &self.bundle_digest.len())
            .field("proof_jam_len", &self.proof_jam.len())
            .finish()
    }
}

pub fn claim_proof(effect: &NounSlab) -> Option<ClaimProof> {
    if effect_tag(effect)? != "claim-proof" {
        return None;
    }
    let noun = unsafe { *effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let bundle_digest = rest.head().as_atom().ok()?.as_ne_bytes().to_vec();
    let proof_noun = rest.tail();
    let mut stack = new_stack();
    let proof_jam = jam_to_bytes(&mut stack, proof_noun);
    Some(ClaimProof {
        bundle_digest,
        proof_jam,
    })
}

pub fn first_claim_proof(effects: &[NounSlab]) -> Option<ClaimProof> {
    effects.iter().find_map(claim_proof)
}

/// Build a `[%prove-arbitrary subject=* formula=*]` poke slab. Used
/// by Phase 3c step 3 tests and future callers that want to prove a
/// caller-constructed Nock formula under the kernel's current
/// `(root, hull)`.
///
/// `subject_jam` and `formula_jam` are JAM'd noun bytes that the
/// kernel cues before handing to `prove-computation:vp`. Using jam
/// bytes keeps the Rust builder honest about noun construction —
/// the Hoon side does the real work.
pub fn build_prove_arbitrary_poke(subject_jam: &[u8], formula_jam: &[u8]) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "prove-arbitrary");
    // We pass the pre-jammed bytes as a cell `[subject formula]`
    // where each element is itself an atom carrying its jammed form.
    // The kernel will cue them. This is a temporary API surface —
    // once the validator Nock encoding stabilises we'll switch to
    // typed helpers that build the pair from a `ClaimBundle`.
    let subject = make_atom_in(&mut slab, subject_jam);
    let formula = make_atom_in(&mut slab, formula_jam);
    let poke = T(&mut slab, &[tag, subject, formula]);
    slab.set_root(poke);
    slab
}

/// Payload of `[%arbitrary-proof product=* proof=*]`. `product_jam`
/// is the JAM of whatever the formula evaluated to on the subject;
/// `proof_jam` is the STARK proof noun JAM'd for transport.
pub struct ArbitraryProof {
    pub product_jam: Vec<u8>,
    pub proof_jam: Vec<u8>,
}

impl std::fmt::Debug for ArbitraryProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArbitraryProof")
            .field("product_jam_len", &self.product_jam.len())
            .field("proof_jam_len", &self.proof_jam.len())
            .finish()
    }
}

pub fn arbitrary_proof(effect: &NounSlab) -> Option<ArbitraryProof> {
    if effect_tag(effect)? != "arbitrary-proof" {
        return None;
    }
    let noun = unsafe { *effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let product_noun = rest.head();
    let proof_noun = rest.tail();
    let mut stack = new_stack();
    let product_jam = jam_to_bytes(&mut stack, product_noun);
    let proof_jam = jam_to_bytes(&mut stack, proof_noun);
    Some(ArbitraryProof {
        product_jam,
        proof_jam,
    })
}

pub fn first_arbitrary_proof(effects: &[NounSlab]) -> Option<ArbitraryProof> {
    effects.iter().find_map(arbitrary_proof)
}

/// Build a `[%prove-claim-in-stark ...]` poke slab.
///
/// Same payload shape as `%validate-claim` / `%prove-claim`, but the
/// kernel builds a subject-bundled-core trace via
/// `build-validator-trace-inputs:nns-predicates` and runs the
/// validator *inside* the STARK.
///
/// On success emits `[%claim-in-stark-proof product proof]` where
/// `product` is the traced validator's return — a head-tagged
/// `(each ~ validation-error)` noun. `[%& ~]` means validation
/// passed, `[%| err]` names the first failing predicate. The wallet
/// reads the product directly; no validator re-run needed.
pub fn build_prove_claim_in_stark_poke(bundle: &ClaimBundle) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "prove-claim-in-stark");

    let name_atom = make_cord_in(&mut slab, &bundle.name);
    let owner_atom = make_cord_in(&mut slab, &bundle.owner);
    let fee_atom = atom_from_u64(&mut slab, bundle.fee);
    let tx_hash_atom = make_atom_in(&mut slab, &bundle.tx_hash);
    let claim_digest_atom = make_atom_in(&mut slab, &bundle.claim_block_digest);

    let mut headers_list = D(0);
    for h in bundle.anchor_headers.iter().rev() {
        let digest = make_atom_in(&mut slab, &h.digest);
        let parent = make_atom_in(&mut slab, &h.parent);
        let height = atom_from_u64(&mut slab, h.height);
        let cell = T(&mut slab, &[digest, height, parent]);
        headers_list = T(&mut slab, &[cell, headers_list]);
    }

    let page_digest_atom = make_atom_in(&mut slab, &bundle.page_digest);

    let mut tx_ids_list = D(0);
    for id in bundle.page_tx_ids.iter().rev() {
        let key = make_atom_in(&mut slab, id);
        tx_ids_list = T(&mut slab, &[key, tx_ids_list]);
    }

    let anchored_tip_atom = make_atom_in(&mut slab, &bundle.anchored_tip);
    let anchored_tip_height_atom = atom_from_u64(&mut slab, bundle.anchored_tip_height);

    let poke = T(
        &mut slab,
        &[
            tag,
            name_atom,
            owner_atom,
            fee_atom,
            tx_hash_atom,
            claim_digest_atom,
            headers_list,
            page_digest_atom,
            tx_ids_list,
            anchored_tip_atom,
            anchored_tip_height_atom,
        ],
    );
    slab.set_root(poke);
    slab
}

/// The validator's return value as it appears inside the STARK's
/// committed product. Mirrors Hoon's `(each ~ validation-error):np`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InStarkValidation {
    /// `[%& ~]` — every predicate passed. The STARK attests to this.
    Ok,
    /// `[%| <tag>]` — some predicate rejected. The STARK attests to
    /// the specific `<tag>` (`invalid-name`, `fee-below-schedule`,
    /// `page-digest-mismatch`, `tx-not-in-page`, `chain-broken`).
    Rejected(String),
}

/// Payload of `[%claim-in-stark-proof product proof]`.
pub struct ClaimInStarkProof {
    pub validation: InStarkValidation,
    pub proof_jam: Vec<u8>,
}

impl std::fmt::Debug for ClaimInStarkProof {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClaimInStarkProof")
            .field("validation", &self.validation)
            .field("proof_jam_len", &self.proof_jam.len())
            .finish()
    }
}

pub fn claim_in_stark_proof(effect: &NounSlab) -> Option<ClaimInStarkProof> {
    if effect_tag(effect)? != "claim-in-stark-proof" {
        return None;
    }
    let noun = unsafe { *effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let product_noun = rest.head();
    let proof_noun = rest.tail();

    // Decode `(each ~ validation-error)`:
    //   [%& 0]   -> Ok  (loobean 0 = %.y)
    //   [%| <tag-atom>] -> Rejected(tag)
    let product_cell = product_noun.as_cell().ok()?;
    let tag_atom = product_cell.head().as_atom().ok()?;
    let tag_val = tag_atom.as_u64().ok()?;
    let validation = match tag_val {
        0 => InStarkValidation::Ok,
        1 => {
            let err_atom = product_cell.tail().as_atom().ok()?;
            let bytes = err_atom.as_ne_bytes();
            let s = std::str::from_utf8(bytes)
                .ok()?
                .trim_end_matches('\0')
                .to_string();
            InStarkValidation::Rejected(s)
        }
        _ => return None,
    };

    let mut stack = new_stack();
    let proof_jam = jam_to_bytes(&mut stack, proof_noun);

    Some(ClaimInStarkProof {
        validation,
        proof_jam,
    })
}

pub fn first_claim_in_stark_proof(effects: &[NounSlab]) -> Option<ClaimInStarkProof> {
    effects.iter().find_map(claim_in_stark_proof)
}

/// Build a `[%prove-recursive-step prev-proof-jam prev-subject-jam
/// prev-formula-jam]` poke. Y0 recursive-composition spike. See
/// `y0_recursive_composition_spike` in `tests/prover.rs` for the
/// call-site and expected outcome.
///
/// `prev_proof_jam` / `prev_subject_jam` / `prev_formula_jam` are the
/// JAM bytes of a previously-emitted `proof:sp`, its traced subject,
/// and its traced formula (typically captured from an `%arbitrary-proof`
/// effect + the caller-constructed subject/formula the prover ran on).
///
/// The kernel wraps `verify:vesl-stark-verifier(prev-proof, ~, 0,
/// prev-subject, prev-formula)` in a subject-bundled-core trace and
/// runs it through `prove-computation:vp`. Two effects come out:
///
///   1. `[%recursive-step-dry-run-ok ok=?]` — whether the raw nockvm
///      accepted (`ok=%.y`) or rejected (`ok=%.n`) the recursive
///      verification, independently of whether the STARK can trace it.
///   2. Either `[%recursive-step-proof product proof]` (prover
///      succeeded — recursive composition is tractable today) OR
///      `[%prove-failed trace]` (expected outcome — Vesl's prover
///      trapped on Nock 9/10/11 inside `verify:vv`).
pub fn build_prove_recursive_step_poke(
    prev_proof_jam: &[u8],
    prev_subject_jam: &[u8],
    prev_formula_jam: &[u8],
) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, "prove-recursive-step");
    let proof = make_atom_in(&mut slab, prev_proof_jam);
    let subject = make_atom_in(&mut slab, prev_subject_jam);
    let formula = make_atom_in(&mut slab, prev_formula_jam);
    let poke = T(&mut slab, &[tag, proof, subject, formula]);
    slab.set_root(poke);
    slab
}

/// Payload of `[%recursive-step-proof product proof]` emitted by
/// `%prove-recursive-step` on a *successful* recursive-composition
/// prove. Seeing this in a test is the go-signal for Path Y step Y3.
#[derive(Debug)]
pub struct RecursiveStepProof {
    /// JAM of whatever the outer formula evaluated to — should be a
    /// Hoon loobean: `%.y` = 0 (inner proof verified), `%.n` = 1.
    pub product_jam: Vec<u8>,
    /// JAM of the outer STARK proof (the recursive-step proof).
    pub proof_jam: Vec<u8>,
}

pub fn recursive_step_proof(effect: &NounSlab) -> Option<RecursiveStepProof> {
    if effect_tag(effect)? != "recursive-step-proof" {
        return None;
    }
    let noun = unsafe { *effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let product_noun = rest.head();
    let proof_noun = rest.tail();
    let mut stack = new_stack();
    let product_jam = jam_to_bytes(&mut stack, product_noun);
    let proof_jam = jam_to_bytes(&mut stack, proof_noun);
    Some(RecursiveStepProof {
        product_jam,
        proof_jam,
    })
}

pub fn first_recursive_step_proof(effects: &[NounSlab]) -> Option<RecursiveStepProof> {
    effects.iter().find_map(recursive_step_proof)
}

/// `ok` from `[%recursive-step-dry-run-ok ok=?]`. `true` means the raw
/// nockvm successfully computed `verify:vv(prev-proof, ~, 0,
/// prev-subject, prev-formula)` and it returned `%.y`. This is the
/// encoding-level sanity gate — if dry-run is `false` we're asking
/// the STARK to trace a trap, and the spike is invalid before we even
/// get to the Vesl-prover question.
pub fn recursive_step_dry_run_ok(effect: &NounSlab) -> Option<bool> {
    if effect_tag(effect)? != "recursive-step-dry-run-ok" {
        return None;
    }
    let noun = unsafe { *effect.root() };
    let cell = noun.as_cell().ok()?;
    let ok_atom = cell.tail().as_atom().ok()?;
    Some(ok_atom.as_u64().ok()? == 0)
}

pub fn first_recursive_step_dry_run_ok(effects: &[NounSlab]) -> Option<bool> {
    effects.iter().find_map(recursive_step_dry_run_ok)
}

// ---------------------------------------------------------------------------
// Peek builders
// ---------------------------------------------------------------------------

/// Build a `/snapshot ~` peek path slab.
///
/// Kernel response: `[~ ~ claim-id=@ud hull=@ root=@]`.
pub fn build_snapshot_peek() -> NounSlab {
    single_tag_peek("snapshot")
}

/// Build a `/pending-batch ~` peek path slab.
///
/// Kernel response: `[~ ~ (list @t)]` — the sorted list of names
/// with `entry.claim-count > last-settled-claim-id`. An empty list
/// means there is nothing to settle right now.
pub fn build_pending_batch_peek() -> NounSlab {
    single_tag_peek("pending-batch")
}

/// Build a `/last-settled ~` peek path slab.
///
/// Kernel response: `[~ ~ @ud]`.
pub fn build_last_settled_peek() -> NounSlab {
    single_tag_peek("last-settled")
}

/// Build a `/owner/<name>` peek path slab.
///
/// Kernel response: `[~ ~ (unit name-entry)]` where
/// `name-entry = [owner=@t tx-hash=@t claim-count=@ud]`. The inner
/// `(unit ...)` is `~` when the name is not in the registry.
pub fn build_owner_peek(name: &str) -> NounSlab {
    name_peek("owner", name)
}

/// Build a `/accumulator/<name>` peek path slab.
///
/// Kernel response: `[~ ~ (unit nns-accumulator-entry)]` where
/// `nns-accumulator-entry = [owner=@t tx-hash=@ux claim-height=@ud block-digest=@ux]`.
pub fn build_accumulator_peek(name: &str) -> NounSlab {
    name_peek("accumulator", name)
}

/// Build a `/accumulator-proof/<name>` peek path slab.
///
/// Kernel response: `[~ ~ (unit @)]`, the z-map axis for a present key.
/// This is an inclusion locator, not yet a full sibling-hash proof.
pub fn build_accumulator_proof_peek(name: &str) -> NounSlab {
    name_peek("accumulator-proof", name)
}

/// Build an `/accumulator-root ~` peek path slab.
///
/// Kernel response: `[~ ~ @]`, a lossy atom representation of the
/// accumulator's Tip5 root. Wallet inclusion proofs should compare against the
/// full z-map root once the inclusion-proof helper lands.
pub fn build_accumulator_root_peek() -> NounSlab {
    single_tag_peek("accumulator-root")
}

/// Build `/accumulator-jam ~` peek path slab.
///
/// Kernel response: `[~ ~ @]` — atom is `jam(accumulator)` (same encoding as
/// `PathY4LookupBundle.accumulator_snapshot_jam_hex`).
pub fn build_accumulator_jam_peek() -> NounSlab {
    single_tag_peek("accumulator-jam")
}

/// Build a `/scan-state ~` peek path slab.
///
/// Kernel response: `[~ ~ height=@ud digest=@ux root=@ size=@ud]`.
pub fn build_scan_state_peek() -> NounSlab {
    single_tag_peek("scan-state")
}

/// Build a `/proof/<name>` peek path slab.
///
/// Kernel response: `[~ ~ (list [hash=@ side=?])]` where the list
/// is the sibling-chain from the leaf for `name` up to the current
/// Merkle root. Empty list means either:
///   - the tree has a single leaf (proof is trivially empty), or
///   - the name is not in the registry.
/// Disambiguate by peeking `/owner/<name>` first.
pub fn build_proof_peek(name: &str) -> NounSlab {
    name_peek("proof", name)
}

/// Build a `/anchor ~` peek path slab.
///
/// Kernel response: `[~ ~ tip-digest=@ux tip-height=@ud]`. Kernel
/// intentionally does not cache historical headers — per-claim
/// chain linkage is carried in the claim-note bundle and proved by
/// the gate. See `+$anchored-chain` in `hoon/app/app.hoon`.
pub fn build_anchor_peek() -> NounSlab {
    single_tag_peek("anchor")
}

/// Build a `/fee-for-name/<name>` peek path slab.
///
/// Kernel response: `[~ ~ @ud]` — the fee (in nicks) that the kernel
/// would require for a `%claim` of `name`. Delegates to
/// `fee-for-name:nns-predicates` (Phase 3 Level A); this is the
/// single source of truth both Hoon and Rust consult so the fee
/// schedule cannot drift between the two.
pub fn build_fee_for_name_peek(name: &str) -> NounSlab {
    name_peek("fee-for-name", name)
}

/// Decode the `[~ ~ @ud]` result of `/fee-for-name/<name>`.
pub fn decode_fee_for_name(result: &NounSlab) -> Result<u64, String> {
    let inner = peek_unwrap_some(result)?;
    inner
        .as_atom()
        .map_err(|_| "fee-for-name: expected atom".to_string())?
        .as_u64()
        .map_err(|_| "fee-for-name: overflows u64".to_string())
}

fn single_tag_peek(tag_str: &str) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, tag_str);
    let path = T(&mut slab, &[tag, D(0)]);
    slab.set_root(path);
    slab
}

fn name_peek(tag_str: &str, name: &str) -> NounSlab {
    let mut slab = NounSlab::new();
    let tag = make_tag_in(&mut slab, tag_str);
    let name_atom = make_cord_in(&mut slab, name);
    let path = T(&mut slab, &[tag, name_atom, D(0)]);
    slab.set_root(path);
    slab
}

// ---------------------------------------------------------------------------
// Peek result decoders
// ---------------------------------------------------------------------------

/// Current registry commitment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// Number of successful `%claim`s so far. `claim_id = 0` means the
    /// registry has never been written to (and `hull`/`root` are
    /// uninitialized zeros in the kernel).
    pub claim_id: u64,
    /// Raw hull-id atom bytes (LE). Opaque — pass straight to
    /// downstream hooks that need it.
    pub hull: Vec<u8>,
    /// Raw Merkle root atom bytes (LE). Same treatment as `hull`.
    pub root: Vec<u8>,
}

/// Decode the `[~ ~ claim-id hull root]` peek result for `/snapshot`.
pub fn decode_snapshot(result: &NounSlab) -> Result<Snapshot, String> {
    let inner = peek_unwrap_some(result)?;
    let cell = inner
        .as_cell()
        .map_err(|_| "snapshot: expected cell".to_string())?;
    let claim_id = cell
        .head()
        .as_atom()
        .map_err(|_| "snapshot: claim_id not an atom".to_string())?
        .as_u64()
        .map_err(|_| "snapshot: claim_id overflows u64".to_string())?;
    let rest = cell
        .tail()
        .as_cell()
        .map_err(|_| "snapshot: tail not a cell".to_string())?;
    let hull = atom_to_le_bytes(rest.head())?;
    let root = atom_to_le_bytes(rest.tail())?;
    Ok(Snapshot {
        claim_id,
        hull,
        root,
    })
}

/// Decode the `[~ ~ (list @t)]` peek result for `/pending-batch`.
///
/// Returns the names (as Rust strings) in the canonical `aor` order
/// that the kernel used when walking the `names` map.
pub fn decode_pending_batch(result: &NounSlab) -> Result<Vec<String>, String> {
    let inner = peek_unwrap_inner(result)?;
    let mut out = Vec::new();
    let mut cur = match inner {
        None => return Ok(out),
        Some(n) => n,
    };
    loop {
        if cur.as_atom().is_ok() {
            break;
        }
        let cell = cur
            .as_cell()
            .map_err(|_| "pending-batch: malformed list cell".to_string())?;
        out.push(atom_to_cord(cell.head())?);
        cur = cell.tail();
    }
    Ok(out)
}

/// Decode the `[~ ~ @ud]` peek result for `/last-settled`.
pub fn decode_last_settled(result: &NounSlab) -> Result<u64, String> {
    let inner = peek_unwrap_some(result)?;
    inner
        .as_atom()
        .map_err(|_| "last-settled: expected atom".to_string())?
        .as_u64()
        .map_err(|_| "last-settled: overflows u64".to_string())
}

/// A row in the kernel's `names` map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameEntry {
    pub owner: String,
    pub tx_hash: String,
    pub claim_count: u64,
}

/// A row in the Path Y accumulator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccumulatorEntry {
    pub owner: String,
    pub tx_hash: Vec<u8>,
    pub claim_height: u64,
    pub block_digest: Vec<u8>,
}

/// Decode the `[~ ~ (unit nns-accumulator-entry)]` peek result for
/// `/accumulator/<name>`. Returns `Ok(None)` when the name is absent.
pub fn decode_accumulator_entry(result: &NounSlab) -> Result<Option<AccumulatorEntry>, String> {
    let inner = peek_unwrap_some(result)?;
    if inner.as_atom().is_ok() {
        return Ok(None);
    }
    let unit_cell = inner
        .as_cell()
        .map_err(|_| "accumulator: expected (unit entry) cell".to_string())?;
    let entry = unit_cell.tail();
    let entry_cell = entry
        .as_cell()
        .map_err(|_| "accumulator: entry not a cell".to_string())?;
    let owner = atom_to_cord(entry_cell.head())?;
    let rest = entry_cell
        .tail()
        .as_cell()
        .map_err(|_| "accumulator: entry tail not a cell".to_string())?;
    let tx_hash = atom_to_le_bytes(rest.head())?;
    let rest = rest
        .tail()
        .as_cell()
        .map_err(|_| "accumulator: entry tail2 not a cell".to_string())?;
    let claim_height = rest
        .head()
        .as_atom()
        .map_err(|_| "accumulator: claim_height not atom".to_string())?
        .as_u64()
        .map_err(|_| "accumulator: claim_height overflows u64".to_string())?;
    let block_digest = atom_to_le_bytes(rest.tail())?;
    Ok(Some(AccumulatorEntry {
        owner,
        tx_hash,
        claim_height,
        block_digest,
    }))
}

/// Decode `[~ ~ (unit @)]` from `/accumulator-proof/<name>`.
pub fn decode_accumulator_proof_axis(result: &NounSlab) -> Result<Option<Vec<u8>>, String> {
    let inner = peek_unwrap_some(result)?;
    if inner.as_atom().is_ok() {
        return Ok(None);
    }
    let unit_cell = inner
        .as_cell()
        .map_err(|_| "accumulator-proof: expected (unit @) cell".to_string())?;
    Ok(Some(atom_to_le_bytes(unit_cell.tail())?))
}

/// Decode `[~ ~ @]` from `/accumulator-root`.
pub fn decode_accumulator_root(result: &NounSlab) -> Result<Vec<u8>, String> {
    let inner = peek_unwrap_some(result)?;
    atom_to_le_bytes(inner)
}

/// Decode `[~ ~ @]` from `/accumulator-jam` (JAM of the full `nns-accumulator`).
pub fn decode_accumulator_jam(result: &NounSlab) -> Result<Vec<u8>, String> {
    decode_accumulator_root(result)
}

/// The Path Y scan cursor and accumulator summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanState {
    pub last_proved_height: u64,
    pub last_proved_digest: Vec<u8>,
    pub accumulator_root: Vec<u8>,
    pub accumulator_size: u64,
}

/// Decode `[~ ~ height digest root size]` from `/scan-state`.
pub fn decode_scan_state(result: &NounSlab) -> Result<ScanState, String> {
    let inner = peek_unwrap_some(result)?;
    let cell = inner
        .as_cell()
        .map_err(|_| "scan-state: expected cell".to_string())?;
    let last_proved_height = cell
        .head()
        .as_atom()
        .map_err(|_| "scan-state: height not atom".to_string())?
        .as_u64()
        .map_err(|_| "scan-state: height overflows u64".to_string())?;
    let rest = cell
        .tail()
        .as_cell()
        .map_err(|_| "scan-state: tail not a cell".to_string())?;
    let last_proved_digest = atom_to_le_bytes(rest.head())?;
    let rest = rest
        .tail()
        .as_cell()
        .map_err(|_| "scan-state: tail2 not a cell".to_string())?;
    let accumulator_root = atom_to_le_bytes(rest.head())?;
    let accumulator_size = rest
        .tail()
        .as_atom()
        .map_err(|_| "scan-state: size not atom".to_string())?
        .as_u64()
        .map_err(|_| "scan-state: size overflows u64".to_string())?;
    Ok(ScanState {
        last_proved_height,
        last_proved_digest,
        accumulator_root,
        accumulator_size,
    })
}

/// Decode the `[~ ~ (unit name-entry)]` peek result for
/// `/owner/<name>`. Returns `Ok(None)` when the inner unit is `~`
/// (the name is not registered).
pub fn decode_owner(result: &NounSlab) -> Result<Option<NameEntry>, String> {
    let inner = peek_unwrap_some(result)?;
    // Inner is `(unit name-entry)`: atom 0 when missing, `[~ entry]`
    // when present. `entry = [owner=@t tx-hash=@t claim-count=@ud]`.
    if inner.as_atom().is_ok() {
        return Ok(None);
    }
    let unit_cell = inner
        .as_cell()
        .map_err(|_| "owner: expected (unit entry) cell".to_string())?;
    let entry = unit_cell.tail();
    let entry_cell = entry
        .as_cell()
        .map_err(|_| "owner: entry not a cell".to_string())?;
    let owner = atom_to_cord(entry_cell.head())?;
    let rest = entry_cell
        .tail()
        .as_cell()
        .map_err(|_| "owner: entry tail not a cell".to_string())?;
    let tx_hash = atom_to_cord(rest.head())?;
    let claim_count = rest
        .tail()
        .as_atom()
        .map_err(|_| "owner: claim_count not an atom".to_string())?
        .as_u64()
        .map_err(|_| "owner: claim_count overflows u64".to_string())?;
    Ok(Some(NameEntry {
        owner,
        tx_hash,
        claim_count,
    }))
}

/// A single sibling in a Merkle inclusion proof.
///
/// `side = true` means the sibling is on the **left** (matching
/// Hoon's `%.y`): the verifier hashes as `hash-pair(sibling, cur)`.
/// `side = false` means the sibling is on the **right** (Hoon's
/// `%.n`): `hash-pair(cur, sibling)`. See `verify-chunk` in
/// `hoon/lib/vesl-merkle.hoon`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProofNode {
    pub hash: Vec<u8>,
    pub side: bool,
}

/// Decode the `[~ ~ (list [hash=@ side=?])]` peek result for
/// `/proof/<name>`. An empty list is legitimate for a single-leaf
/// registry; callers should cross-check `/owner/<name>` to tell a
/// real empty proof from a missing name.
pub fn decode_proof(result: &NounSlab) -> Result<Vec<ProofNode>, String> {
    let inner = peek_unwrap_some(result)?;
    let mut out = Vec::new();
    let mut cur = inner;
    loop {
        if cur.as_atom().is_ok() {
            // End of list (~).
            break;
        }
        let cell = cur
            .as_cell()
            .map_err(|_| "proof: malformed list cell".to_string())?;
        let node = cell
            .head()
            .as_cell()
            .map_err(|_| "proof: node not a cell".to_string())?;
        let hash = node
            .head()
            .as_atom()
            .map_err(|_| "proof: hash not atom".to_string())?
            .as_ne_bytes()
            .to_vec();
        let side_atom = node
            .tail()
            .as_atom()
            .map_err(|_| "proof: side not atom".to_string())?;
        // Hoon loobean: 0 = %.y (sibling LEFT), 1 = %.n (sibling RIGHT).
        let side_val = side_atom
            .as_u64()
            .map_err(|_| "proof: side overflows u64".to_string())?;
        let side = side_val == 0;
        out.push(ProofNode { hash, side });
        cur = cell.tail();
    }
    Ok(out)
}

/// Current anchored chain view from the kernel.
///
/// `tip_digest` is the raw LE bytes of a 5-felt Tip5 hash (all-zero
/// when uninitialised). The kernel intentionally does not cache
/// historical headers — per-claim chain linkage is supplied by the
/// claim-note's `ClaimChainBundle.header_chain_jam` and proved by
/// the gate. See the `+$anchored-chain` comment in
/// `hoon/app/app.hoon` for the design rationale.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorView {
    pub tip_digest: Vec<u8>,
    pub tip_height: u64,
}

/// Decode the `/anchor` peek result. Kernel returns
/// `[~ ~ tip-digest=@ux tip-height=@ud]`.
pub fn decode_anchor(result: &NounSlab) -> Result<AnchorView, String> {
    let inner = peek_unwrap_some(result)?;
    let cell = inner
        .as_cell()
        .map_err(|_| "anchor: expected cell".to_string())?;
    let tip_digest = atom_to_le_bytes(cell.head())?;
    let tip_height = cell
        .tail()
        .as_atom()
        .map_err(|_| "anchor: tip_height not atom".to_string())?
        .as_u64()
        .map_err(|_| "anchor: tip_height overflows u64".to_string())?;
    Ok(AnchorView {
        tip_digest,
        tip_height,
    })
}

// Strip the outer `(unit (unit *))` wrapping the kernel peek
// produces. Returns the innermost `*` or `None` if the inner unit
// was null (recognized path, no value).
fn peek_unwrap_inner(result: &NounSlab) -> Result<Option<Noun>, String> {
    let noun = unsafe { *result.root() };
    if noun.as_atom().map(|_| true).unwrap_or(false) {
        // Outer `~` — path not recognized.
        return Err("peek: kernel did not recognize path".into());
    }
    let outer = noun
        .as_cell()
        .map_err(|_| "peek: outer not a cell".to_string())?;
    // outer = [~ ...] — outer.head() is the `~` marker (atom 0)
    // and outer.tail() is the inner unit.
    let inner = outer.tail();
    if inner.as_atom().map(|_| true).unwrap_or(false) {
        // `[~ ~]` — recognized, no value.
        return Ok(None);
    }
    let inner_cell = inner
        .as_cell()
        .map_err(|_| "peek: inner not a cell".to_string())?;
    Ok(Some(inner_cell.tail()))
}

// Same as peek_unwrap_inner but errors on recognized-but-empty —
// use for peeks whose result is always present.
fn peek_unwrap_some(result: &NounSlab) -> Result<Noun, String> {
    peek_unwrap_inner(result)?.ok_or_else(|| "peek: expected a value, got empty unit".into())
}

fn atom_to_le_bytes(noun: Noun) -> Result<Vec<u8>, String> {
    let atom = noun.as_atom().map_err(|_| "expected atom".to_string())?;
    Ok(atom.as_ne_bytes().to_vec())
}

fn atom_to_cord(noun: Noun) -> Result<String, String> {
    let atom = noun
        .as_atom()
        .map_err(|_| "expected cord atom".to_string())?;
    Ok(std::str::from_utf8(atom.as_ne_bytes())
        .map_err(|_| "cord not utf-8".to_string())?
        .trim_end_matches('\0')
        .to_string())
}

// ---------------------------------------------------------------------------
// Effect inspection
// ---------------------------------------------------------------------------

/// Read the head-tag string (e.g. `"claimed"`, `"claim-error"`) of
/// a domain effect.
pub fn effect_tag(effect: &NounSlab) -> Option<String> {
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let atom = cell.head().as_atom().ok()?;
    let bytes = atom.as_ne_bytes();
    let s = std::str::from_utf8(bytes).ok()?.trim_end_matches('\0');
    Some(s.to_string())
}

/// Read the error message from a `[%claim-error msg=@t]`,
/// `[%primary-error msg=@t]`, `[%batch-error msg=@t]`,
/// `[%anchor-error msg=@t]`, or
/// `[%vesl-error msg=@t]` effect.
pub fn error_message(effect: &NounSlab) -> Option<String> {
    let tag = effect_tag(effect)?;
    if tag != "claim-error"
        && tag != "primary-error"
        && tag != "batch-error"
        && tag != "anchor-error"
        && tag != "vesl-error"
    {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let msg = cell.tail().as_atom().ok()?;
    let bytes = msg.as_ne_bytes();
    Some(
        std::str::from_utf8(bytes)
            .ok()?
            .trim_end_matches('\0')
            .to_string(),
    )
}

/// `[%anchor-advanced tip-digest=@ux tip-height=@ud count=@ud]` payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorAdvanced {
    pub tip_digest: Vec<u8>,
    pub tip_height: u64,
    pub count: u64,
}

pub fn anchor_advanced(effect: &NounSlab) -> Option<AnchorAdvanced> {
    if effect_tag(effect)? != "anchor-advanced" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let tip_digest = rest.head().as_atom().ok()?.as_ne_bytes().to_vec();
    let rest2 = rest.tail().as_cell().ok()?;
    let tip_height = rest2.head().as_atom().ok()?.as_u64().ok()?;
    let count = rest2.tail().as_atom().ok()?.as_u64().ok()?;
    Some(AnchorAdvanced {
        tip_digest,
        tip_height,
        count,
    })
}

pub fn first_anchor_advanced(effects: &[NounSlab]) -> Option<AnchorAdvanced> {
    effects.iter().find_map(anchor_advanced)
}

/// Successful `%scan-block` effect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanBlockDone {
    pub height: u64,
    pub digest: Vec<u8>,
    pub accumulator_root: Vec<u8>,
}

pub fn scan_block_done(effect: &NounSlab) -> Option<ScanBlockDone> {
    if effect_tag(effect)? != "scan-block-done" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let height = rest.head().as_atom().ok()?.as_u64().ok()?;
    let rest = rest.tail().as_cell().ok()?;
    let digest = atom_to_le_bytes(rest.head()).ok()?;
    let accumulator_root = atom_to_le_bytes(rest.tail()).ok()?;
    Some(ScanBlockDone {
        height,
        digest,
        accumulator_root,
    })
}

pub fn first_scan_block_done(effects: &[NounSlab]) -> Option<ScanBlockDone> {
    effects.iter().find_map(scan_block_done)
}

pub fn scan_block_error(effect: &NounSlab) -> Option<String> {
    if effect_tag(effect)? != "scan-block-error" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    atom_to_cord(cell.tail()).ok()
}

pub fn first_scan_block_error(effects: &[NounSlab]) -> Option<String> {
    effects.iter().find_map(scan_block_error)
}

/// Read `(address, name)` from a `[%primary-set address=@t name=@t]`
/// effect. Returns `None` for any other effect shape.
pub fn primary_set(effect: &NounSlab) -> Option<(String, String)> {
    if effect_tag(effect)? != "primary-set" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let addr_atom = rest.head().as_atom().ok()?;
    let name_atom = rest.tail().as_atom().ok()?;
    let addr = std::str::from_utf8(addr_atom.as_ne_bytes())
        .ok()?
        .trim_end_matches('\0')
        .to_string();
    let name = std::str::from_utf8(name_atom.as_ne_bytes())
        .ok()?
        .trim_end_matches('\0')
        .to_string();
    Some((addr, name))
}

/// Payload of a `[%claim-count-bumped claim-count=@ud hull=@ root=@]` effect
/// emitted by `%claim`. `hull` and `root` are the raw LE atom bytes
/// (opaque — cached in the hull's snapshot view).
#[derive(Debug, Clone)]
pub struct ClaimCountBumped {
    pub claim_count: u64,
    pub hull: Vec<u8>,
    pub root: Vec<u8>,
}

pub fn claim_count_bumped(effect: &NounSlab) -> Option<ClaimCountBumped> {
    if effect_tag(effect)? != "claim-count-bumped" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let claim_count = rest.head().as_atom().ok()?.as_u64().ok()?;
    let rest2 = rest.tail().as_cell().ok()?;
    let hull = rest2.head().as_atom().ok()?.as_ne_bytes().to_vec();
    let root = rest2.tail().as_atom().ok()?.as_ne_bytes().to_vec();
    Some(ClaimCountBumped {
        claim_count,
        hull,
        root,
    })
}

pub fn first_claim_count_bumped(effects: &[NounSlab]) -> Option<ClaimCountBumped> {
    effects.iter().find_map(claim_count_bumped)
}

/// Payload of a graft `[%vesl-settled note=[id hull root [%settled ~]]]`
/// effect. Returns the note-id + hull + root as raw atom bytes.
#[derive(Debug, Clone)]
pub struct VeslSettled {
    pub note_id: Vec<u8>,
    pub hull: Vec<u8>,
    pub root: Vec<u8>,
}

pub fn vesl_settled(effect: &NounSlab) -> Option<VeslSettled> {
    if effect_tag(effect)? != "vesl-settled" {
        return None;
    }
    let noun = unsafe { effect.root() };
    // [%vesl-settled [id hull root [%settled ~]]]
    let cell = noun.as_cell().ok()?;
    let note = cell.tail().as_cell().ok()?;
    let id_atom = note.head().as_atom().ok()?;
    let note_id = id_atom.as_ne_bytes().to_vec();
    let rest = note.tail().as_cell().ok()?;
    let hull = rest.head().as_atom().ok()?.as_ne_bytes().to_vec();
    let rest2 = rest.tail().as_cell().ok()?;
    let root = rest2.head().as_atom().ok()?.as_ne_bytes().to_vec();
    Some(VeslSettled {
        note_id,
        hull,
        root,
    })
}

pub fn first_vesl_settled(effects: &[NounSlab]) -> Option<VeslSettled> {
    effects.iter().find_map(vesl_settled)
}

/// Payload of a `[%batch-settled claim-count=@ud count=@ud note-id=@]`
/// effect emitted by the kernel's `%settle-batch` arm. `claim-count` is
/// the commitment at which the batch was packaged, which the hull
/// stores as its new `last-settled-claim-id`.
#[derive(Debug, Clone)]
pub struct BatchSettled {
    pub claim_count: u64,
    pub count: u64,
    pub note_id: Vec<u8>,
}

pub fn batch_settled(effect: &NounSlab) -> Option<BatchSettled> {
    if effect_tag(effect)? != "batch-settled" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let claim_count = rest.head().as_atom().ok()?.as_u64().ok()?;
    let rest2 = rest.tail().as_cell().ok()?;
    let count = rest2.head().as_atom().ok()?.as_u64().ok()?;
    let note_id = rest2.tail().as_atom().ok()?.as_ne_bytes().to_vec();
    Some(BatchSettled {
        claim_count,
        count,
        note_id,
    })
}

pub fn first_batch_settled(effects: &[NounSlab]) -> Option<BatchSettled> {
    effects.iter().find_map(batch_settled)
}

/// Payload of a `[%batch-proof note-id=@ proof=*]` effect emitted by
/// the kernel's `%prove-batch` arm on a successful STARK generation.
/// `proof_jam` is the JAM'd bytes of the raw proof noun — opaque to
/// the hull, suitable for transport, and CUE'able back into a noun
/// for verification via `verify:sp-verifier`.
#[derive(Debug, Clone)]
pub struct BatchProof {
    pub note_id: Vec<u8>,
    pub proof_jam: Vec<u8>,
}

pub fn batch_proof(effect: &NounSlab) -> Option<BatchProof> {
    if effect_tag(effect)? != "batch-proof" {
        return None;
    }
    let noun = unsafe { *effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let note_id = rest.head().as_atom().ok()?.as_ne_bytes().to_vec();
    let proof_noun = rest.tail();
    let mut stack = new_stack();
    let proof_jam = jam_to_bytes(&mut stack, proof_noun);
    Some(BatchProof { note_id, proof_jam })
}

pub fn first_batch_proof(effects: &[NounSlab]) -> Option<BatchProof> {
    effects.iter().find_map(batch_proof)
}

/// Payload of a `[%prove-failed trace-jam=@]` effect emitted when the
/// kernel's STARK prover crashed. Returns the JAM'd crash trace for
/// diagnostic surfacing; the hull MUST treat this as a failure and
/// NOT apply settlement.
pub fn prove_failed(effect: &NounSlab) -> Option<Vec<u8>> {
    if effect_tag(effect)? != "prove-failed" {
        return None;
    }
    let noun = unsafe { *effect.root() };
    let cell = noun.as_cell().ok()?;
    let trace_atom = cell.tail().as_atom().ok()?;
    Some(trace_atom.as_ne_bytes().to_vec())
}

pub fn first_prove_failed(effects: &[NounSlab]) -> Option<Vec<u8>> {
    effects.iter().find_map(prove_failed)
}

/// `ok` from `[%verify-stark-result ok=?]` (Hoon loobean: `%.y` = true).
pub fn verify_stark_result(effect: &NounSlab) -> Option<bool> {
    if effect_tag(effect)? != "verify-stark-result" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let ok_atom = cell.tail().as_atom().ok()?;
    let v = ok_atom.as_u64().ok()?;
    Some(v == 0)
}

pub fn first_verify_stark_result(effects: &[NounSlab]) -> Option<bool> {
    effects.iter().find_map(verify_stark_result)
}

/// Cord message from `[%verify-stark-error msg=@t]`.
pub fn verify_stark_error(effect: &NounSlab) -> Option<String> {
    if effect_tag(effect)? != "verify-stark-error" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let msg = cell.tail().as_atom().ok()?;
    let bytes = msg.as_ne_bytes();
    Some(
        std::str::from_utf8(bytes)
            .ok()?
            .trim_end_matches('\0')
            .to_string(),
    )
}

pub fn first_verify_stark_error(effects: &[NounSlab]) -> Option<String> {
    effects.iter().find_map(verify_stark_error)
}

/// `ok` from `[%accumulator-snapshot-verify-result ok=?]`.
pub fn accumulator_snapshot_verify_result(effect: &NounSlab) -> Option<bool> {
    if effect_tag(effect)? != "accumulator-snapshot-verify-result" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let ok_atom = cell.tail().as_atom().ok()?;
    let v = ok_atom.as_u64().ok()?;
    Some(v == 0)
}

pub fn first_accumulator_snapshot_verify_result(effects: &[NounSlab]) -> Option<bool> {
    effects
        .iter()
        .find_map(accumulator_snapshot_verify_result)
}

pub fn accumulator_snapshot_verify_error(effect: &NounSlab) -> Option<String> {
    if effect_tag(effect)? != "accumulator-snapshot-verify-error" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let msg = cell.tail().as_atom().ok()?;
    let bytes = msg.as_ne_bytes();
    Some(
        std::str::from_utf8(bytes)
            .ok()?
            .trim_end_matches('\0')
            .to_string(),
    )
}

pub fn first_accumulator_snapshot_verify_error(effects: &[NounSlab]) -> Option<String> {
    effects
        .iter()
        .find_map(accumulator_snapshot_verify_error)
}

/// Returns the first `(address, name)` payload across `effects` from
/// any `%primary-set` effect, if any.
pub fn first_primary_set(effects: &[NounSlab]) -> Option<(String, String)> {
    effects.iter().find_map(primary_set)
}

/// Returns `true` iff `effects` contains an effect tagged `tag`.
pub fn has_effect(effects: &[NounSlab], tag: &str) -> bool {
    effects.iter().filter_map(effect_tag).any(|t| t == tag)
}

/// Comma-separated domain effect tags (for diagnostics when a poke
/// returns an unexpected mix).
pub fn format_effect_tags(effects: &[NounSlab]) -> String {
    let tags: Vec<String> = effects.iter().filter_map(effect_tag).collect();
    if tags.is_empty() {
        "(none)".to_string()
    } else {
        tags.join(", ")
    }
}

/// Returns the first error message across `effects` (from a
/// `%claim-error`, `%primary-error`, `%batch-error`, or
/// `%vesl-error`), if any.
pub fn first_error_message(effects: &[NounSlab]) -> Option<String> {
    effects.iter().find_map(error_message)
}
