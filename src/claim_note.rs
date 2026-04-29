//! NNS claim note decoding for **NoteData on v1 outputs** (Path Y scanner).
//! See `docs/claim-note-wallet-support.md` and [nockchain#116](https://github.com/nockchain/nockchain/pull/116).
//!
//! Claims use the canonical **`blob`** note-data key. The on-wire value is **wallet-packed**
//! (JAM of belt list → inner bytes = claim triple JAM); see [`crate::packed_blob`].
//!
//! **No optional “chain bundle” in note-data:** the hull does not trust extra attachments for
//! raw-tx, page, proofs, or headers — it **re-fetches** the paying tx and block context from
//! Nockchain RPC and runs predicates (`chain_follower`, kernel) on that canonical view.
use nock_noun_rs::{cue_from_bytes, jam_to_bytes, make_cord, new_stack, T};
use nockchain_client_rs::{find_opaque_bytes_entry, jam_opaque_bytes_entry, NoteData};
use serde::{Deserialize, Serialize};

/// Programmatic claim payload (`wallet-tx-builder` / gRPC `NoteDataEntry.key`).
pub const CLAIM_NOTE_BLOB_ENTRY_KEY: &str = "blob";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimNoteV1 {
    pub name: String,
    pub owner: String,
    pub tx_hash: String,
}

impl ClaimNoteV1 {
    pub fn new(name: String, owner: String, tx_hash: String) -> Self {
        Self {
            name,
            owner,
            tx_hash,
        }
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

    /// Decode **`blob`** only. Chain evidence is **not** read from note-data; the follower
    /// loads `TransactionDetails` and block metadata from RPC and validates in the kernel.
    pub fn from_note_data(note_data: &NoteData) -> Result<Self, String> {
        let wire = find_opaque_bytes_entry(note_data, CLAIM_NOTE_BLOB_ENTRY_KEY)
            .map_err(|e| format!("missing {CLAIM_NOTE_BLOB_ENTRY_KEY} note-data entry: {e}"))?;
        let tuple_jam = crate::packed_blob::unpack_wallet_blob_jam(wire.as_slice()).map_err(
            |e| format!("{CLAIM_NOTE_BLOB_ENTRY_KEY}: expected wallet packed blob: {e}"),
        )?;

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

    fn sample_note() -> ClaimNoteV1 {
        ClaimNoteV1 {
            name: "foo.nock".to_string(),
            owner: "owner-xyz".to_string(),
            tx_hash: "tx-abc".to_string(),
        }
    }

    fn note_data_fixture(note: &ClaimNoteV1) -> NoteData {
        let wire = crate::packed_blob::pack_wallet_blob_jam(&note.jam_tuple());
        NoteData::new(vec![jam_opaque_bytes_entry(
            CLAIM_NOTE_BLOB_ENTRY_KEY,
            &wire,
        )])
    }

    #[test]
    fn note_data_roundtrip_preserves_fields() {
        let note = sample_note();
        let decoded = ClaimNoteV1::from_note_data(&note_data_fixture(&note)).expect("decode");
        assert_eq!(decoded, note);
    }
}
