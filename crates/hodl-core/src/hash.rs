//! 32-byte hash newtype with hex display and sha256 helpers.

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct H256(pub [u8; 32]);

impl H256 {
    pub const ZERO: H256 = H256([0u8; 32]);

    pub fn sha256(bytes: &[u8]) -> Self {
        let mut h = Sha256::new();
        h.update(bytes);
        H256(h.finalize().into())
    }

    pub fn sha256d(bytes: &[u8]) -> Self {
        Self::sha256(&Self::sha256(bytes).0)
    }

    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    pub fn from_hex(s: &str) -> Result<Self, hex::FromHexError> {
        let v = hex::decode(s)?;
        if v.len() != 32 {
            return Err(hex::FromHexError::InvalidStringLength);
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&v);
        Ok(H256(out))
    }
}

impl fmt::Debug for H256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "H256({})", self.to_hex())
    }
}

impl fmt::Display for H256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

impl Serialize for H256 {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for H256 {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        H256::from_hex(&s).map_err(serde::de::Error::custom)
    }
}
