//! Packed `%blob` / `%memo` note-data values matching [nockchain `wallet-tx-builder`](https://github.com/nockchain/nockchain/blob/master/crates/wallet-tx-builder/src/note_data.rs)
//! (PR #116). On-chain `NoteDataEntry.blob` holds **JAM bytes of a belt noun**
//! `[byte-length=@ belt …]` (little-endian `u32` limbs), **not** the raw inner payload.
//!
//! Decode path mirrors [`MemoDataPayload::from_blob`] there: `cue` → `Vec<Belt>` → length-prefixed bytes.

use nockapp::utils::NOCK_STACK_SIZE;
use nockchain_math::belt::Belt;
use nockvm::ext::NounExt;
use nockvm::mem::NockStack;
use nockvm::noun::Noun;
use noun_serde::NounDecode;

/// Matches `wallet-tx-builder`: arbitrary memo/blob payload size cap.
const MAX_BLOB_PAYLOAD_BYTES: usize = 256 * 1024;

fn decode_len_prefixed_blob(belts: &[Belt]) -> Option<Vec<u8>> {
    if belts.is_empty() {
        return None;
    }
    let byte_len = usize::try_from(belts[0].0).ok()?;
    if byte_len > MAX_BLOB_PAYLOAD_BYTES {
        return None;
    }
    let expected_belts = 1 + (byte_len + 3) / 4;
    if belts.len() != expected_belts {
        return None;
    }
    let mut body = Belt::to_le_bytes(&belts[1..]).ok()?;
    if body.len() < byte_len {
        return None;
    }
    body.truncate(byte_len);
    Some(body)
}

/// Decode wallet-style note-data value: JAM → belt list → inner payload bytes.
pub(crate) fn unpack_wallet_blob_jam(jam: &[u8]) -> Result<Vec<u8>, String> {
    let mut stack = NockStack::new(NOCK_STACK_SIZE, 0);
    let noun = Noun::cue_bytes_slice(&mut stack, jam)
        .map_err(|e| format!("cue blob jam: {e}"))?;
    let belts = Vec::<Belt>::from_noun(&noun)
        .map_err(|e| format!("blob belt list noun: {e}"))?;
    decode_len_prefixed_blob(&belts)
        .ok_or_else(|| "invalid length-prefixed packed blob".to_string())
}

/// Encode inner payload the same way the wallet does ([`crate::claim_note`] tests only).
#[cfg(test)]
pub(crate) fn pack_wallet_blob_jam(payload: &[u8]) -> Vec<u8> {
    use nockapp::noun::slab::{NockJammer, NounSlab};
    use noun_serde::NounEncode;

    fn encode_blob_belts(bytes: &[u8]) -> Vec<Belt> {
        let mut belts = Vec::with_capacity(1 + (bytes.len() + 3) / 4);
        belts.push(Belt(bytes.len() as u64));
        belts.extend(Belt::from_le_bytes(bytes));
        belts
    }

    let belts = encode_blob_belts(payload);
    let mut slab: NounSlab<NockJammer> = NounSlab::new();
    let noun = belts.to_noun(&mut slab);
    slab.set_root(noun);
    slab.jam().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_roundtrips() {
        let inner = b"hello packed blob payload".to_vec();
        let jam = pack_wallet_blob_jam(&inner);
        assert_eq!(unpack_wallet_blob_jam(&jam).expect("unpack"), inner);
    }
}
