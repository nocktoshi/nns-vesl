//! NockApp poke construction + effect/peek inspection.
//!
//! The kernel is a `data-registry`-shaped NockApp with the Vesl
//! graft wired in. Four poke shapes are in use:
//!
//!   - `%claim` — hot path. The hull sends one per `POST /claim`
//!     and the kernel writes `names` + `tx-hashes`, bumps the
//!     claim-id counter, recomputes the Merkle root over the full
//!     `names` map, and auto-registers a fresh hull in the graft.
//!     Effects: `%claimed`, optional `%primary-set`,
//!     `%claim-id-bumped`, and graft's `%vesl-registered`. Returns
//!     `%claim-error <msg>` on a user-visible failure without
//!     mutating state.
//!
//!   - `%set-primary` — owner-gated reverse-lookup update. Writes
//!     `primaries` only; does NOT bump the claim-id. Effects:
//!     `%primary-set` on success, `%primary-error <msg>` on
//!     rejection.
//!
//!   - `%settle-batch` — batch settlement. Kernel-side: selects
//!     every name with `entry.claim-id > last-settled-claim-id`,
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
//!   `/owner/<name>`, `/primary/<addr>`, `/entries`, `/claim-id`,
//!   `/last-settled`, `/hull`, `/root`, `/snapshot`, `/proof/<name>`,
//!   `/pending-batch`, plus the graft's `/registered/<hull>`,
//!   `/settled/<note-id>`, `/root/<hull>`.

use nock_noun_rs::{atom_from_u64, make_cord_in, make_tag_in, NounSlab};
use nockvm::noun::{Noun, D, T};

// ---------------------------------------------------------------------------
// Poke builders
// ---------------------------------------------------------------------------

/// Build a `[%claim name=@t owner=@t fee=@ud tx-hash=@t]` poke slab.
///
/// Kernel response:
///
///   - `[%claimed name owner tx-hash]` on success; both `names`
///     and `tx-hashes` get updated. Also emits
///     `[%claim-id-bumped claim-id hull root]` and the graft's
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
    let poke = T(&mut slab, &[tag, name_atom, owner_atom, fee_atom, tx_hash_atom]);
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
/// with `entry.claim-id > last-settled-claim-id`. An empty list
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
/// `name-entry = [owner=@t tx-hash=@t claim-id=@ud]`. The inner
/// `(unit ...)` is `~` when the name is not in the registry.
pub fn build_owner_peek(name: &str) -> NounSlab {
    name_peek("owner", name)
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
    Ok(Snapshot { claim_id, hull, root })
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
    pub claim_id: u64,
}

/// Decode the `[~ ~ (unit name-entry)]` peek result for
/// `/owner/<name>`. Returns `Ok(None)` when the inner unit is `~`
/// (the name is not registered).
pub fn decode_owner(result: &NounSlab) -> Result<Option<NameEntry>, String> {
    let inner = peek_unwrap_some(result)?;
    // Inner is `(unit name-entry)`: atom 0 when missing, `[~ entry]`
    // when present. `entry = [owner=@t tx-hash=@t claim-id=@ud]`.
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
    let claim_id = rest
        .tail()
        .as_atom()
        .map_err(|_| "owner: claim_id not an atom".to_string())?
        .as_u64()
        .map_err(|_| "owner: claim_id overflows u64".to_string())?;
    Ok(Some(NameEntry {
        owner,
        tx_hash,
        claim_id,
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
    peek_unwrap_inner(result)?
        .ok_or_else(|| "peek: expected a value, got empty unit".into())
}

fn atom_to_le_bytes(noun: Noun) -> Result<Vec<u8>, String> {
    let atom = noun
        .as_atom()
        .map_err(|_| "expected atom".to_string())?;
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
/// `[%primary-error msg=@t]`, `[%batch-error msg=@t]`, or
/// `[%vesl-error msg=@t]` effect.
pub fn error_message(effect: &NounSlab) -> Option<String> {
    let tag = effect_tag(effect)?;
    if tag != "claim-error"
        && tag != "primary-error"
        && tag != "batch-error"
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

/// Payload of a `[%claim-id-bumped claim-id=@ud hull=@ root=@]` effect
/// emitted by `%claim`. `hull` and `root` are the raw LE atom bytes
/// (opaque — cached in the hull's snapshot view).
#[derive(Debug, Clone)]
pub struct ClaimIdBumped {
    pub claim_id: u64,
    pub hull: Vec<u8>,
    pub root: Vec<u8>,
}

pub fn claim_id_bumped(effect: &NounSlab) -> Option<ClaimIdBumped> {
    if effect_tag(effect)? != "claim-id-bumped" {
        return None;
    }
    let noun = unsafe { effect.root() };
    let cell = noun.as_cell().ok()?;
    let rest = cell.tail().as_cell().ok()?;
    let claim_id = rest.head().as_atom().ok()?.as_u64().ok()?;
    let rest2 = rest.tail().as_cell().ok()?;
    let hull = rest2.head().as_atom().ok()?.as_ne_bytes().to_vec();
    let root = rest2.tail().as_atom().ok()?.as_ne_bytes().to_vec();
    Some(ClaimIdBumped { claim_id, hull, root })
}

pub fn first_claim_id_bumped(effects: &[NounSlab]) -> Option<ClaimIdBumped> {
    effects.iter().find_map(claim_id_bumped)
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
    Some(VeslSettled { note_id, hull, root })
}

pub fn first_vesl_settled(effects: &[NounSlab]) -> Option<VeslSettled> {
    effects.iter().find_map(vesl_settled)
}

/// Payload of a `[%batch-settled claim-id=@ud count=@ud note-id=@]`
/// effect emitted by the kernel's `%settle-batch` arm. `claim-id` is
/// the commitment at which the batch was packaged, which the hull
/// stores as its new `last-settled-claim-id`.
#[derive(Debug, Clone)]
pub struct BatchSettled {
    pub claim_id: u64,
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
    let claim_id = rest.head().as_atom().ok()?.as_u64().ok()?;
    let rest2 = rest.tail().as_cell().ok()?;
    let count = rest2.head().as_atom().ok()?.as_u64().ok()?;
    let note_id = rest2.tail().as_atom().ok()?.as_ne_bytes().to_vec();
    Some(BatchSettled {
        claim_id,
        count,
        note_id,
    })
}

pub fn first_batch_settled(effects: &[NounSlab]) -> Option<BatchSettled> {
    effects.iter().find_map(batch_settled)
}

/// Returns the first `(address, name)` payload across `effects` from
/// any `%primary-set` effect, if any.
pub fn first_primary_set(effects: &[NounSlab]) -> Option<(String, String)> {
    effects.iter().find_map(primary_set)
}

/// Returns `true` iff `effects` contains an effect tagged `tag`.
pub fn has_effect(effects: &[NounSlab], tag: &str) -> bool {
    effects
        .iter()
        .filter_map(effect_tag)
        .any(|t| t == tag)
}

/// Returns the first error message across `effects` (from a
/// `%claim-error`, `%primary-error`, `%batch-error`, or
/// `%vesl-error`), if any.
pub fn first_error_message(effects: &[NounSlab]) -> Option<String> {
    effects.iter().find_map(error_message)
}
