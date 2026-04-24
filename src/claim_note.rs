use nock_noun_rs::{cue_from_bytes, jam_to_bytes, make_cord, new_stack, T};
use nockchain_client_rs::{
    find_opaque_bytes_entry, jam_opaque_bytes_entry, jam_u64_entry, NoteData,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CLAIM_NOTE_KEY: &str = "nns/v1/claim";
pub const CLAIM_NOTE_VERSION_KEY: &str = "nns/v1/claim-version";
pub const CLAIM_NOTE_ID_KEY: &str = "nns/v1/claim-id";

/// Phase 2d — optional chain-bundle keys. A claim-note with any of
/// these is a "post-Phase-2" note carrying the on-chain evidence the
/// recursive `nns-gate` circuit will consume in Phase 3. Claim-notes
/// without them are treated as legacy (accepted in local mode,
/// rejected by a strict Phase 3 circuit).
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
    pub version: u64,
    pub claim_id: String,
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
            version: 1,
            claim_id: Uuid::new_v4().to_string(),
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

    /// Canonical jam payload for the claim tuple.
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
    /// Legacy `nns/v1/claim-*` entries are always emitted. Phase 2d
    /// chain-bundle keys are emitted only when present — absent keys
    /// are indistinguishable from a legacy note on the wire so
    /// downstream readers can stay backward-compatible.
    pub fn to_note_data(&self) -> NoteData {
        let mut entries = vec![
            jam_u64_entry(CLAIM_NOTE_VERSION_KEY, self.version),
            jam_opaque_bytes_entry(CLAIM_NOTE_ID_KEY, self.claim_id.as_bytes()),
            jam_opaque_bytes_entry(CLAIM_NOTE_KEY, &self.jam_tuple()),
        ];
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
            entries.push(jam_opaque_bytes_entry(
                CLAIM_NOTE_HEADER_CHAIN_KEY,
                bytes,
            ));
        }
        NoteData::new(entries)
    }

    /// Decode a chain note-data payload back into a claim note.
    ///
    /// Missing chain-bundle keys surface as `None` on the respective
    /// `ClaimChainBundle` fields — callers that require them (e.g.
    /// strict Phase 3 follower) must check `chain_bundle.is_complete`.
    pub fn from_note_data(note_data: &NoteData) -> Result<Self, String> {
        let version = nockchain_client_rs::find_u64_entry(note_data, CLAIM_NOTE_VERSION_KEY)
            .map_err(|e| format!("missing claim version: {e}"))?;
        if version != 1 {
            return Err(format!("unsupported claim version: {version}"));
        }
        let claim_id_bytes = find_opaque_bytes_entry(note_data, CLAIM_NOTE_ID_KEY)
            .map_err(|e| format!("missing claim id: {e}"))?;
        let claim_id = String::from_utf8(claim_id_bytes)
            .map_err(|e| format!("claim id is not utf8: {e}"))?;
        let tuple_jam = find_opaque_bytes_entry(note_data, CLAIM_NOTE_KEY)
            .map_err(|e| format!("missing claim tuple: {e}"))?;
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
            block_proof_jam: find_opaque_bytes_entry(note_data, CLAIM_NOTE_BLOCK_PROOF_KEY)
                .ok(),
            header_chain_jam: find_opaque_bytes_entry(note_data, CLAIM_NOTE_HEADER_CHAIN_KEY)
                .ok(),
        };

        Ok(Self {
            version,
            claim_id,
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
            version: 1,
            claim_id: "c-123".to_string(),
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
        assert_eq!(decoded.version, note.version);
        assert_eq!(decoded.claim_id, note.claim_id);
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
