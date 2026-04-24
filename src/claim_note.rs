use nock_noun_rs::{cue_from_bytes, jam_to_bytes, make_cord, new_stack, T};
use nockchain_client_rs::{
    find_opaque_bytes_entry, jam_opaque_bytes_entry, jam_u64_entry, NoteData,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CLAIM_NOTE_KEY: &str = "nns/v1/claim";
pub const CLAIM_NOTE_VERSION_KEY: &str = "nns/v1/claim-version";
pub const CLAIM_NOTE_ID_KEY: &str = "nns/v1/claim-id";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimNoteV1 {
    pub version: u64,
    pub claim_id: String,
    pub name: String,
    pub owner: String,
    pub tx_hash: String,
}

impl ClaimNoteV1 {
    pub fn new(name: String, owner: String, tx_hash: String) -> Self {
        Self {
            version: 1,
            claim_id: Uuid::new_v4().to_string(),
            name,
            owner,
            tx_hash,
        }
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
    pub fn to_note_data(&self) -> NoteData {
        let entries = vec![
            jam_u64_entry(CLAIM_NOTE_VERSION_KEY, self.version),
            jam_opaque_bytes_entry(CLAIM_NOTE_ID_KEY, self.claim_id.as_bytes()),
            jam_opaque_bytes_entry(CLAIM_NOTE_KEY, &self.jam_tuple()),
        ];
        NoteData::new(entries)
    }

    /// Decode a chain note-data payload back into a claim note.
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
        Ok(Self {
            version,
            claim_id,
            name: atom_to_cord(name)?,
            owner: atom_to_cord(owner)?,
            tx_hash: atom_to_cord(tx_hash)?,
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

    #[test]
    fn note_data_roundtrip_preserves_fields() {
        let note = ClaimNoteV1 {
            version: 1,
            claim_id: "c-123".to_string(),
            name: "foo.nock".to_string(),
            owner: "owner-xyz".to_string(),
            tx_hash: "tx-abc".to_string(),
        };
        let decoded = ClaimNoteV1::from_note_data(&note.to_note_data()).expect("decode");
        assert_eq!(decoded.version, note.version);
        assert_eq!(decoded.claim_id, note.claim_id);
        assert_eq!(decoded.name, note.name);
        assert_eq!(decoded.owner, note.owner);
        assert_eq!(decoded.tx_hash, note.tx_hash);
    }
}
