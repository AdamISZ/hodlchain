//! Sequencer attestation OP_RETURN payload codec.
//!
//! Layout (73 bytes):
//!
//! ```text
//! magic(4)  | version(1) | height(4 BE) | l2_block_hash(32) | state_root(32)
//! ```

use crate::consensus::{ATTESTATION_LEN, ATTESTATION_VERSION, MAGIC};
use crate::hash::H256;
use bitcoin::opcodes::all::OP_RETURN;
use bitcoin::script::Builder;
use bitcoin::ScriptBuf;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attestation {
    pub version: u8,
    pub height: u32,
    pub l2_block_hash: H256,
    pub state_root: H256,
}

#[derive(Debug, Error)]
pub enum AttestationError {
    #[error("attestation payload has wrong length: got {0}, expected {ATTESTATION_LEN}")]
    BadLength(usize),
    #[error("attestation magic mismatch")]
    BadMagic,
    #[error("unsupported attestation version: {0}")]
    BadVersion(u8),
    #[error("not an OP_RETURN output")]
    NotOpReturn,
}

impl Attestation {
    pub fn new(height: u32, l2_block_hash: H256, state_root: H256) -> Self {
        Self { version: ATTESTATION_VERSION, height, l2_block_hash, state_root }
    }

    /// Encode as the 73-byte payload (excluding the OP_RETURN wrapper).
    pub fn encode(&self) -> [u8; ATTESTATION_LEN] {
        let mut out = [0u8; ATTESTATION_LEN];
        out[0..4].copy_from_slice(&MAGIC);
        out[4] = self.version;
        out[5..9].copy_from_slice(&self.height.to_be_bytes());
        out[9..41].copy_from_slice(&self.l2_block_hash.0);
        out[41..73].copy_from_slice(&self.state_root.0);
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, AttestationError> {
        if bytes.len() != ATTESTATION_LEN {
            return Err(AttestationError::BadLength(bytes.len()));
        }
        if bytes[0..4] != MAGIC {
            return Err(AttestationError::BadMagic);
        }
        let version = bytes[4];
        if version != ATTESTATION_VERSION {
            return Err(AttestationError::BadVersion(version));
        }
        let mut height_bytes = [0u8; 4];
        height_bytes.copy_from_slice(&bytes[5..9]);
        let height = u32::from_be_bytes(height_bytes);
        let mut bh = [0u8; 32];
        bh.copy_from_slice(&bytes[9..41]);
        let mut sr = [0u8; 32];
        sr.copy_from_slice(&bytes[41..73]);
        Ok(Self { version, height, l2_block_hash: H256(bh), state_root: H256(sr) })
    }

    /// Build the `OP_RETURN <push 73 bytes>` scriptPubKey for this attestation.
    pub fn to_script(&self) -> ScriptBuf {
        let payload = self.encode();
        Builder::new()
            .push_opcode(OP_RETURN)
            .push_slice(&payload)
            .into_script()
    }

    /// Attempt to parse a scriptPubKey as a hodlcoin attestation OP_RETURN.
    /// Returns Ok(Some(_)) on a match, Ok(None) if it's an OP_RETURN whose
    /// payload isn't ours (wrong magic / length), or Err if the script isn't
    /// even OP_RETURN-shaped.
    pub fn try_from_script(spk: &ScriptBuf) -> Result<Option<Self>, AttestationError> {
        let bytes = spk.as_bytes();
        if bytes.is_empty() || bytes[0] != OP_RETURN.to_u8() {
            return Err(AttestationError::NotOpReturn);
        }
        // Find the pushed payload by walking instructions.
        let mut instructions = spk.instructions();
        let _ = instructions.next(); // OP_RETURN
        let pushed = match instructions.next() {
            Some(Ok(bitcoin::script::Instruction::PushBytes(p))) => p.as_bytes().to_vec(),
            _ => return Ok(None),
        };
        match Self::decode(&pushed) {
            Ok(a) => Ok(Some(a)),
            Err(AttestationError::BadMagic) | Err(AttestationError::BadLength(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_payload() {
        let a = Attestation::new(42, H256([1u8; 32]), H256([2u8; 32]));
        let bytes = a.encode();
        assert_eq!(bytes.len(), ATTESTATION_LEN);
        let b = Attestation::decode(&bytes).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn roundtrip_script() {
        let a = Attestation::new(7, H256([3u8; 32]), H256([4u8; 32]));
        let spk = a.to_script();
        let b = Attestation::try_from_script(&spk).unwrap().unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn rejects_foreign_magic() {
        let mut bytes = Attestation::new(1, H256::ZERO, H256::ZERO).encode().to_vec();
        bytes[0] = b'X';
        assert!(matches!(Attestation::decode(&bytes), Err(AttestationError::BadMagic)));
    }
}
