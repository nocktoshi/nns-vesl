//! NNS claim note encoding for **NoteData on v1 outputs** (Path Y scanner).
//! Wallet / CLI support is limited today — see `docs/claim-note-wallet-support.md`
//! and <https://github.com/nockchain/nockchain/pull/85>.
use nock_noun_rs::{cue_from_bytes, jam_to_bytes, make_cord, new_stack, T};
use nockchain_client_rs::{find_opaque_bytes_entry, jam_opaque_bytes_entry, NoteData};
use serde::{Deserialize, Serialize};

/// Key for the JAM'd claim triple `[name=cord owner=cord tx_hash=cord]`.
/// Version is implied by the `v1` path segment (no separate `claim-version` entry).
pub const CLAIM_NOTE_KEY: &str = "nns/v1/claim";

/// Phase 2d — optional chain-bundle keys. A claim-note with any of
/// these is a "post-Phase-2" note carrying on-chain evidence the
/// recursive `nns-gate` circuit will consume in Phase 3.
pub const CLAIM_NOTE_RAW_TX_KEY: &str = "nns/v1/raw-tx";
pub const CLAIM_NOTE_PAGE_KEY: &str = "nns/v1/page";
pub const CLAIM_NOTE_BLOCK_PROOF_KEY: &str = "nns/v1/block-proof";
pub const CLAIM_NOTE_HEADER_CHAIN_KEY: &str = "nns/v1/header-chain";

/// Chain-bundle payload attached to a claim note. Each field is
/// opaque jammed bytes — the decoder lives in Phase 3's gate, not
/// here. Kept out of the core `ClaimNoteV1` struct so local-mode
/// callers don't need to synthesize anything.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimChainBundle {
    /// JAM'd `raw-tx:t` noun of the claim's paying tx.
    #[serde(default)]
    pub raw_tx_jam: Option<Vec<u8>>,
    /// JAM'd `page:t` noun of the block that included the paying tx.
    #[serde(default)]
    pub page_jam: Option<Vec<u8>>,
    /// JAM'd `proof:sp` — the block's PoW STARK.
    #[serde(default)]
    pub block_proof_jam: Option<Vec<u8>>,
    /// JAM'd list of `page-header` from `page`'s block up to the
    /// follower-anchored tip.
    #[serde(default)]
    pub header_chain_jam: Option<Vec<u8>>,
}

impl ClaimChainBundle {
    /// True when the bundle carries every field the Phase 3 circuit
    /// needs. A local-mode claim (or a legacy chain claim) will be
    /// `false`; the follower is expected to reject those in
    /// non-degraded non-local mode.
    pub fn is_complete(&self) -> bool {
        self.raw_tx_jam.is_some()
            && self.page_jam.is_some()
            && self.block_proof_jam.is_some()
            && self.header_chain_jam.is_some()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimNoteV1 {
    pub name: String,
    pub owner: String,
    pub tx_hash: String,
    /// Phase 2d: optional on-chain evidence for the Phase 3 circuit.
    /// Empty / all-`None` for legacy & local-mode notes.
    #[serde(default)]
    pub chain_bundle: ClaimChainBundle,
}

impl ClaimNoteV1 {
    pub fn new(name: String, owner: String, tx_hash: String) -> Self {
        Self {
            name,
            owner,
            tx_hash,
            chain_bundle: ClaimChainBundle::default(),
        }
    }

    /// Attach a chain bundle (moving the struct fluent-style). Used
    /// by the Phase 4 `/claim` handler to enrich a note before chain
    /// submission.
    pub fn with_chain_bundle(mut self, bundle: ClaimChainBundle) -> Self {
        self.chain_bundle = bundle;
        self
    }

    /// Canonical jam payload for the claim triple `[name owner tx_hash]`.
    pub fn jam_tuple(&self) -> Vec<u8> {
        let mut stack = new_stack();
        let n = make_cord(&mut stack, &self.name);
        let o = make_cord(&mut stack, &self.owner);
        let tx = make_cord(&mut stack, &self.tx_hash);
        let noun = T(&mut stack, &[n, o, tx]);
        jam_to_bytes(&mut stack, noun)
    }

    /// Encode this claim note as NoteData entries for a NoteV1 output.
    ///
    /// Emits only **`nns/v1/claim`** (JAM triple) plus any optional Phase 2d
    /// chain-bundle keys when set. Version is implied by the key path; block
    /// height / tx id on chain disambiguate claims for wallets and proofs.
    pub fn to_note_data(&self) -> NoteData {
        let mut entries = vec![jam_opaque_bytes_entry(CLAIM_NOTE_KEY, &self.jam_tuple())];
        if let Some(bytes) = self.chain_bundle.raw_tx_jam.as_ref() {
            entries.push(jam_opaque_bytes_entry(CLAIM_NOTE_RAW_TX_KEY, bytes));
        }
        if let Some(bytes) = self.chain_bundle.page_jam.as_ref() {
            entries.push(jam_opaque_bytes_entry(CLAIM_NOTE_PAGE_KEY, bytes));
        }
        if let Some(bytes) = self.chain_bundle.block_proof_jam.as_ref() {
            entries.push(jam_opaque_bytes_entry(CLAIM_NOTE_BLOCK_PROOF_KEY, bytes));
        }
        if let Some(bytes) = self.chain_bundle.header_chain_jam.as_ref() {
            entries.push(jam_opaque_bytes_entry(CLAIM_NOTE_HEADER_CHAIN_KEY, bytes));
        }
        NoteData::new(entries)
    }

    /// Decode a chain note-data payload back into a claim note.
    ///
    /// Missing chain-bundle keys surface as `None` on the respective
    /// `ClaimChainBundle` fields — callers that require them (e.g.
    /// strict Phase 3 follower) must check `chain_bundle.is_complete`.
    pub fn from_note_data(note_data: &NoteData) -> Result<Self, String> {
        let tuple_jam = find_opaque_bytes_entry(note_data, CLAIM_NOTE_KEY)
            .map_err(|e| format!("missing nns/v1/claim: {e}"))?;
        let mut stack = new_stack();
        let tuple = cue_from_bytes(&mut stack, &tuple_jam)
            .ok_or_else(|| "failed to decode claim tuple".to_string())?;
        let c1 = tuple
            .as_cell()
            .map_err(|_| "claim tuple malformed (slot 1)".to_string())?;
        let name = c1.head();
        let c2 = c1
            .tail()
            .as_cell()
            .map_err(|_| "claim tuple malformed (slot 2)".to_string())?;
        let owner = c2.head();
        let tx_hash = c2.tail();

        let chain_bundle = ClaimChainBundle {
            raw_tx_jam: find_opaque_bytes_entry(note_data, CLAIM_NOTE_RAW_TX_KEY).ok(),
            page_jam: find_opaque_bytes_entry(note_data, CLAIM_NOTE_PAGE_KEY).ok(),
            block_proof_jam: find_opaque_bytes_entry(note_data, CLAIM_NOTE_BLOCK_PROOF_KEY).ok(),
            header_chain_jam: find_opaque_bytes_entry(note_data, CLAIM_NOTE_HEADER_CHAIN_KEY).ok(),
        };

        Ok(Self {
            name: atom_to_cord(name)?,
            owner: atom_to_cord(owner)?,
            tx_hash: atom_to_cord(tx_hash)?,
            chain_bundle,
        })
    }
}

fn atom_to_cord(noun: nockvm::noun::Noun) -> Result<String, String> {
    let atom = noun
        .as_atom()
        .map_err(|_| "claim tuple field is not an atom".to_string())?;
    std::str::from_utf8(atom.as_ne_bytes())
        .map(|s| s.trim_end_matches('\0').to_string())
        .map_err(|e| format!("claim tuple field is not utf8: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_note() -> ClaimNoteV1 {
        ClaimNoteV1 {
            name: "foo.nock".to_string(),
            owner: "owner-xyz".to_string(),
            tx_hash: "tx-abc".to_string(),
            chain_bundle: ClaimChainBundle::default(),
        }
    }

    #[test]
    fn note_data_roundtrip_preserves_fields() {
        let note = sample_note();
        let decoded = ClaimNoteV1::from_note_data(&note.to_note_data()).expect("decode");
        assert_eq!(decoded.name, note.name);
        assert_eq!(decoded.owner, note.owner);
        assert_eq!(decoded.tx_hash, note.tx_hash);
        assert_eq!(decoded.chain_bundle, ClaimChainBundle::default());
        assert!(!decoded.chain_bundle.is_complete());
    }

    #[test]
    fn chain_bundle_roundtrips_through_note_data() {
        let bundle = ClaimChainBundle {
            raw_tx_jam: Some(b"raw-tx-bytes".to_vec()),
            page_jam: Some(b"page-bytes".to_vec()),
            block_proof_jam: Some(vec![0u8, 1, 2, 3, 255]),
            header_chain_jam: Some(b"header-chain-bytes".to_vec()),
        };
        assert!(bundle.is_complete());
        let note = sample_note().with_chain_bundle(bundle.clone());
        let decoded = ClaimNoteV1::from_note_data(&note.to_note_data()).expect("decode");
        assert_eq!(decoded.chain_bundle, bundle);
        assert!(decoded.chain_bundle.is_complete());
    }

    #[test]
    fn partial_chain_bundle_is_not_complete() {
        let partial = ClaimChainBundle {
            raw_tx_jam: Some(b"a".to_vec()),
            page_jam: None,
            block_proof_jam: Some(b"b".to_vec()),
            header_chain_jam: None,
        };
        let note = sample_note().with_chain_bundle(partial.clone());
        let decoded = ClaimNoteV1::from_note_data(&note.to_note_data()).expect("decode");
        assert_eq!(decoded.chain_bundle, partial);
        assert!(!decoded.chain_bundle.is_complete());
    }
}
