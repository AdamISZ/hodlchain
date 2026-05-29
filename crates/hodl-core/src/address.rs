//! Bech32m-encoded L2 address format.
//!
//! Encodes an x-only pubkey (32 bytes) with a network-specific HRP:
//!
//! ```text
//!   hc1...    mainnet
//!   thc1...   testnet / signet
//!   hcrt1...  regtest
//! ```
//!
//! Patterned after Bitcoin's HRP convention (`bc`/`tb`/`bcrt`). Testnet
//! and signet share an HRP for the same reason Bitcoin does: keeping
//! them mutually compatible at the addressing layer mirrors user
//! expectation that they are interchangeable for testing.
//!
//! This is purely a presentation/parsing format. The on-chain identity
//! is still the 32-byte x-only pubkey; consensus, the SMT, and the
//! HTTP wire (`/balance/<hex>`) all stay raw-bytes / hex. Only the
//! user-facing surfaces — the CLI, the desktop GUI, error messages
//! exposed to the user — use bech32m.

use crate::config::NetworkName;
use alloc::string::{String, ToString};
use bech32::{Bech32m, Hrp};
use bitcoin::secp256k1::XOnlyPublicKey;

const HRP_MAINNET: &str = "hc";
/// Testnet and signet share an HRP, mirroring how Bitcoin reuses `tb1`
/// for both. A wallet on testnet will accept signet-encoded addresses
/// and vice versa.
const HRP_TEST: &str = "thc";
const HRP_REGTEST: &str = "hcrt";

/// Coarser-grained "addressing class" — what survives an
/// encode/decode round-trip when testnet and signet collapse into a
/// single HRP. Used to gate decode-time HRP↔network agreement
/// without falsely rejecting (e.g.) a signet address on a testnet
/// wallet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressClass {
    Mainnet,
    TestnetOrSignet,
    Regtest,
}

impl AddressClass {
    pub fn of(net: NetworkName) -> Self {
        match net {
            NetworkName::Bitcoin => AddressClass::Mainnet,
            NetworkName::Testnet | NetworkName::Signet => AddressClass::TestnetOrSignet,
            NetworkName::Regtest => AddressClass::Regtest,
        }
    }

    fn hrp(self) -> &'static str {
        match self {
            AddressClass::Mainnet => HRP_MAINNET,
            AddressClass::TestnetOrSignet => HRP_TEST,
            AddressClass::Regtest => HRP_REGTEST,
        }
    }

    fn from_hrp(hrp: &str) -> Option<Self> {
        match hrp {
            HRP_MAINNET => Some(AddressClass::Mainnet),
            HRP_TEST => Some(AddressClass::TestnetOrSignet),
            HRP_REGTEST => Some(AddressClass::Regtest),
            _ => None,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AddressError {
    #[error("bech32 decode failed: {0}")]
    Bech32(#[from] bech32::DecodeError),
    #[error("wrong HRP for this network: expected {expected:?}, got {found:?}")]
    WrongHrp {
        expected: &'static str,
        found: String,
    },
    #[error("unknown HRP {0:?}; expected one of hc, thc, hcrt")]
    UnknownHrp(String),
    #[error("payload must be 32 bytes, got {0}")]
    WrongLength(usize),
    #[error("decoded payload is not a valid x-only pubkey")]
    InvalidPubkey,
}

/// Encode an x-only pubkey as a bech32m L2 address for `network`.
pub fn encode(addr: &XOnlyPublicKey, network: NetworkName) -> String {
    let class = AddressClass::of(network);
    // HRPs are static ASCII; parse failure is a code bug, not a runtime
    // condition. Likewise bech32m encoding never fails for a valid
    // (HRP, payload) pair — the only failure modes are oversized HRP
    // or oversized payload, neither of which is reachable here.
    let hrp = Hrp::parse(class.hrp()).expect("static HRP must parse");
    bech32::encode::<Bech32m>(hrp, &addr.serialize()).expect("bech32m encode")
}

/// Decode an L2 address, returning the pubkey and the address-class
/// embedded in the HRP. Use [`decode_for`] when you want a single-call
/// network check.
pub fn decode(s: &str) -> Result<(XOnlyPublicKey, AddressClass), AddressError> {
    let (hrp, data) = bech32::decode(s)?;
    let class = AddressClass::from_hrp(hrp.as_str())
        .ok_or_else(|| AddressError::UnknownHrp(hrp.to_string()))?;
    if data.len() != 32 {
        return Err(AddressError::WrongLength(data.len()));
    }
    let pk = XOnlyPublicKey::from_slice(&data).map_err(|_| AddressError::InvalidPubkey)?;
    Ok((pk, class))
}

/// Decode an L2 address and verify it's for the expected network.
///
/// Convenience for the common path (wallet decoding a user-pasted
/// destination): rejects with `WrongHrp` if the address was encoded
/// for a different network class than the wallet's. Testnet and signet
/// are interchangeable (same HRP).
pub fn decode_for(s: &str, network: NetworkName) -> Result<XOnlyPublicKey, AddressError> {
    let (pk, class) = decode(s)?;
    let expected = AddressClass::of(network);
    if class != expected {
        return Err(AddressError::WrongHrp {
            expected: expected.hrp(),
            found: class.hrp().to_string(),
        });
    }
    Ok(pk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::secp256k1::{Keypair, Secp256k1};

    fn sample_pubkey() -> XOnlyPublicKey {
        let secp = Secp256k1::new();
        // Deterministic test key — not used anywhere except these tests.
        let sk_bytes = [0x42u8; 32];
        let kp = Keypair::from_seckey_slice(&secp, &sk_bytes).unwrap();
        kp.x_only_public_key().0
    }

    #[test]
    fn mainnet_round_trip() {
        let pk = sample_pubkey();
        let s = encode(&pk, NetworkName::Bitcoin);
        assert!(s.starts_with("hc1"), "got {s}");
        let back = decode_for(&s, NetworkName::Bitcoin).unwrap();
        assert_eq!(back, pk);
    }

    #[test]
    fn testnet_signet_share_hrp() {
        let pk = sample_pubkey();
        let s_test = encode(&pk, NetworkName::Testnet);
        let s_signet = encode(&pk, NetworkName::Signet);
        assert!(s_test.starts_with("thc1"));
        assert_eq!(s_test, s_signet);
        // Signet-encoded address is acceptable to a testnet wallet,
        // and vice versa.
        assert_eq!(decode_for(&s_signet, NetworkName::Testnet).unwrap(), pk);
        assert_eq!(decode_for(&s_test, NetworkName::Signet).unwrap(), pk);
    }

    #[test]
    fn regtest_round_trip() {
        let pk = sample_pubkey();
        let s = encode(&pk, NetworkName::Regtest);
        assert!(s.starts_with("hcrt1"), "got {s}");
        assert_eq!(decode_for(&s, NetworkName::Regtest).unwrap(), pk);
    }

    #[test]
    fn cross_network_rejected() {
        let pk = sample_pubkey();
        let s = encode(&pk, NetworkName::Bitcoin);
        let err = decode_for(&s, NetworkName::Regtest).unwrap_err();
        match err {
            AddressError::WrongHrp { expected, .. } => assert_eq!(expected, "hcrt"),
            other => panic!("expected WrongHrp, got {other:?}"),
        }
    }

    #[test]
    fn typo_caught_by_checksum() {
        let pk = sample_pubkey();
        let mut s = encode(&pk, NetworkName::Bitcoin);
        // Flip a single character in the payload section. Bech32m's
        // checksum should catch this with overwhelming probability.
        let last = s.pop().unwrap();
        let tweaked = if last == 'q' { 'p' } else { 'q' };
        s.push(tweaked);
        assert!(decode_for(&s, NetworkName::Bitcoin).is_err());
    }

    #[test]
    fn unknown_hrp() {
        // Bech32m string with a payload that decodes cleanly but
        // under an HRP we don't recognise.
        let pk = sample_pubkey();
        let hrp = Hrp::parse("bogus").unwrap();
        let s = bech32::encode::<Bech32m>(hrp, &pk.serialize()).unwrap();
        let err = decode(&s).unwrap_err();
        matches!(err, AddressError::UnknownHrp(_));
    }
}
