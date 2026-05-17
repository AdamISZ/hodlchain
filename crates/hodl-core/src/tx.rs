//! L2 transaction kinds.
//!
//! Each `L2Tx::Mint` carries BOTH the event (what the mint produced —
//! amount, nullifier, destination) AND the witness (the proof that the
//! mint is authorised). Nodes re-run the witness when applying a block,
//! so block validity does not depend on trusting the sequencer.
//!
//! The witness's variant changes across proof families (transparent
//! outpoint proof → ring proof → ZK proof) but the `MintEntry` shape and
//! `MintEvent` shape are stable, so block-format scaffolding is reused
//! across upgrades.
//!
//! L2 transfer signatures cover (from, to, amount, nonce) with no L1
//! coupling.

use crate::hash::H256;
use crate::proof::MintProofEnvelope;
use bitcoin::secp256k1::{schnorr, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type L2Address = XOnlyPublicKey;
pub type Amount = u64;

/// L2 block body entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum L2Tx {
    /// Mint, with both the resulting event AND the proof that authorised it.
    Mint(MintEntry),
    Transfer(SignedTransfer),
}

/// One mint as it appears inside an L2 block: the declared outcome plus
/// the proof. A validator re-runs `witness.verify(...)` and checks that
/// the resulting `MintCredit` matches `event`; if either fails, the
/// containing block is rejected.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MintEntry {
    pub event: MintEvent,
    pub witness: MintProofEnvelope,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MintEvent {
    /// Opaque dedup id. v0: serialized L1 outpoint. v1: ring-signature key
    /// image. Stored as hex for ergonomic JSON.
    pub nullifier_hex: String,
    pub amount: Amount,
    pub l2_destination: L2Address,
    /// L1 height at which the funding UTXO was confirmed (T_create).
    pub l1_create_height: u32,
    /// Relative locktime baked into L_spend's CSV (T, in blocks). This is
    /// the duration argument that mint_fn consumed; the L1 unlock height
    /// is `l1_create_height + lock_blocks` and computable downstream.
    pub lock_blocks: u32,
    /// V in satoshis. Recorded for replay verification.
    pub l1_value_sat: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransferBody {
    pub from: L2Address,
    pub to: L2Address,
    pub amount: Amount,
    pub nonce: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedTransfer {
    pub body: TransferBody,
    pub signature: schnorr::Signature,
}

impl TransferBody {
    /// Canonical sighash for transfers: sha256("hodl-transfer-v0" || json(body)).
    pub fn sighash(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-transfer-v0");
        let body_json = serde_json::to_vec(self).expect("transfer body serializes");
        h.update(&body_json);
        H256(h.finalize().into())
    }
}

impl L2Tx {
    pub fn hash(&self) -> H256 {
        let bytes = serde_json::to_vec(self).expect("L2Tx serializes");
        H256::sha256(&bytes)
    }
}
