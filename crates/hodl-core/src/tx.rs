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
use alloc::string::String;
use bitcoin::secp256k1::{schnorr, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub type L2Address = XOnlyPublicKey;
pub type Amount = u64;

/// L2 block body entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
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
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct MintEntry {
    pub event: MintEvent,
    pub witness: MintProofEnvelope,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct MintEvent {
    /// Opaque dedup id. v0: serialized L1 outpoint. v1: ring-signature key
    /// image. Stored as hex for ergonomic JSON.
    pub nullifier_hex: String,
    pub amount: Amount,
    /// L2 destination credited with `amount`. Hex-encoded x-only pubkey.
    #[cfg_attr(
        feature = "std",
        schema(value_type = String, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
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
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct TransferBody {
    /// Sender's L2 address (x-only pubkey, hex-encoded).
    #[cfg_attr(
        feature = "std",
        schema(value_type = String, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
    pub from: L2Address,
    /// Recipient's L2 address.
    #[cfg_attr(
        feature = "std",
        schema(value_type = String, example = "0000000000000000000000000000000000000000000000000000000000000002")
    )]
    pub to: L2Address,
    pub amount: Amount,
    /// Per-sender monotonic nonce. Must equal the current state's
    /// `nonce_of(from)` at apply time.
    pub nonce: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct SignedTransfer {
    pub body: TransferBody,
    /// BIP340 Schnorr signature over the body's sighash. Hex-encoded,
    /// 128 chars (64 bytes).
    #[cfg_attr(feature = "std", schema(value_type = String))]
    pub signature: schnorr::Signature,
}

impl TransferBody {
    /// Canonical sighash for transfers: sha256 over a deterministic byte
    /// layout. Replaces the v0 serde_json-based hash; the new layout is
    /// fixed and audit-friendly, and works in no_std contexts (so the
    /// state-transition zk-program can compute it).
    ///
    /// Layout (length-prefixed where ambiguous):
    /// ```text
    /// "hodl-transfer-v2" || from(32) || to(32) || amount_be(8) || nonce_be(8)
    /// ```
    pub fn sighash(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-transfer-v2");
        h.update(self.from.serialize());            // 32 bytes
        h.update(self.to.serialize());              // 32 bytes
        h.update(self.amount.to_be_bytes());        // 8 bytes
        h.update(self.nonce.to_be_bytes());         // 8 bytes
        H256(h.finalize().into())
    }
}

impl L2Tx {
    /// Hash of an L2 tx for inclusion in `txs_root`. Two-byte
    /// discriminator distinguishes Mint vs Transfer; body is
    /// hashed in a deterministic field-by-field encoding.
    ///
    /// Layout:
    /// ```text
    /// "hodl-tx-v2" || kind(1) || encode_body
    ///   kind = 0x01 for Mint, 0x02 for Transfer
    /// ```
    pub fn hash(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-tx-v2");
        match self {
            L2Tx::Mint(entry) => {
                h.update([0x01u8]);
                let ev = &entry.event;
                // event
                let n = ev.nullifier_hex.as_bytes();
                h.update((n.len() as u32).to_be_bytes());
                h.update(n);
                h.update(ev.amount.to_be_bytes());
                h.update(ev.l2_destination.serialize());
                h.update(ev.l1_create_height.to_be_bytes());
                h.update(ev.lock_blocks.to_be_bytes());
                h.update(ev.l1_value_sat.to_be_bytes());
                // witness — opaque domain-tagged hash so we don't have to
                // enumerate every future MintProofEnvelope variant here.
                let w = witness_canonical_bytes(&entry.witness);
                h.update((w.len() as u32).to_be_bytes());
                h.update(&w);
            }
            L2Tx::Transfer(t) => {
                h.update([0x02u8]);
                h.update(t.body.sighash().0); // domain-separated already
                h.update(t.signature.serialize()); // 64-byte Schnorr sig
            }
        }
        H256(h.finalize().into())
    }
}

/// Canonical bytes for a mint witness. Domain-separated per variant;
/// fields encoded in a stable order. Hashing this committed bytes into
/// `L2Tx::hash` binds the block to the specific witness shape the
/// producer used.
fn witness_canonical_bytes(env: &MintProofEnvelope) -> alloc::vec::Vec<u8> {
    use alloc::vec::Vec;
    let mut out: Vec<u8> = Vec::new();
    match env {
        MintProofEnvelope::V0Outpoint(p) => {
            out.push(0x01);
            // outpoint = txid (32 bytes) || vout
            out.extend_from_slice(AsRef::<[u8]>::as_ref(&p.outpoint.txid));
            out.extend_from_slice(&p.outpoint.vout.to_be_bytes());
            out.extend_from_slice(&p.user_xonly_pubkey.serialize());
            out.extend_from_slice(&p.lock_blocks.to_be_bytes());
            out.extend_from_slice(&p.signature.serialize());
        }
    }
    out
}
