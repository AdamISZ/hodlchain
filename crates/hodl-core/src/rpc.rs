//! Shared HTTP DTOs spoken by the wallet, sequencer and node.

use crate::hash::H256;
use crate::proof::MintProofEnvelope;
use crate::smt::InclusionProof;
use crate::state::StateComponents;
use crate::tx::{Amount, L2Address, SignedTransfer};
use alloc::string::String;
use bitcoin::secp256k1::{schnorr, Message, Secp256k1, Verification, XOnlyPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Request body for `POST /mint`. The proof is the witness type-tagged
/// envelope (v0 = `OutpointProof`; later: ring proof, ZK proof).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct SubmitMintRequest {
    pub proof: MintProofEnvelope,
    /// L2 address to credit if the mint verifies.
    #[cfg_attr(
        feature = "std",
        schema(value_type = String, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
    pub l2_destination: L2Address,
}

/// Response from `POST /mint`. On accept, includes the amount minted
/// (best-effort: a retarget between submit and inclusion may shift it)
/// and the dedup nullifier.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct SubmitMintResponse {
    pub accepted: bool,
    /// Human-readable rejection reason. Populated only on reject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Submit-time estimate of the L2 amount this mint will credit.
    /// Populated only on accept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mint_amount: Option<Amount>,
    /// Dedup nullifier (hex). Populated only on accept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullifier_hex: Option<String>,
    /// Sequencer-signed soft-confirmation receipt. Present on accept.
    /// Recipients hold this as evidence that the sequencer committed
    /// to including this tx; future work (slashing, equivocation
    /// detection) builds on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft_conf: Option<SoftConf>,
}

/// Request body for `POST /transfer`. Wraps a signed transfer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct SubmitTransferRequest {
    pub transfer: SignedTransfer,
}

/// Response from `POST /transfer`. Submit-time accept doesn't guarantee
/// inclusion: the producer may drop the tx at block-build time if the
/// nonce or balance no longer matches.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct SubmitTransferResponse {
    pub accepted: bool,
    /// Human-readable rejection reason. Populated only on reject.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Sequencer-signed soft-confirmation receipt. Present on accept.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft_conf: Option<SoftConf>,
}

/// Sequencer's signed promise that an accepted tx will land in
/// L2 block `target_l2_height` (= current head + 1 at acceptance
/// time). The signature uses the sequencer's L2 identity key
/// (published in the genesis header as `producer` / matching
/// `sequencer_fee_address`).
///
/// **Trust posture.** Soft-conf is informational: the sequencer can
/// in principle drop the tx at block-build time (insufficient
/// balance after a parallel transfer, etc.) or include it at a
/// later height (mempool overflow). The signed receipt becomes the
/// basis for equivocation detection: if the sequencer ever signs
/// two conflicting receipts (same tx_hash → different heights, or
/// the included height is past the soft-confirmed target without
/// the tx actually landing) anyone holding the receipts can prove
/// misbehaviour. Slashing on top of this is a future item.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct SoftConf {
    /// The L2 tx hash this receipt covers.
    pub tx_hash: H256,
    /// L2 height the sequencer commits to including this tx at.
    pub target_l2_height: u32,
    /// Unix seconds at which the sequencer accepted the tx. The
    /// signature binds this; replay-resistance comes from the
    /// tx_hash, not the timestamp (a tx hash is unique to its
    /// content).
    pub accepted_at_unix: u64,
    /// BIP340 Schnorr signature by the sequencer identity key over
    /// `softconf_sighash(tx_hash, target_l2_height, accepted_at_unix)`.
    /// Serialised as 64-byte hex on the wire.
    #[cfg_attr(
        feature = "std",
        schema(value_type = String, example = "0000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000")
    )]
    pub sequencer_sig: schnorr::Signature,
}

impl SoftConf {
    /// Canonical sighash for the soft-conf payload. Light clients +
    /// the sequencer must agree on this byte layout.
    pub fn sighash(tx_hash: H256, target_l2_height: u32, accepted_at_unix: u64) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-softconf-v1");
        h.update(&tx_hash.0);
        h.update(&target_l2_height.to_be_bytes());
        h.update(&accepted_at_unix.to_be_bytes());
        H256(h.finalize().into())
    }

    /// Verify the embedded Schnorr signature against the sequencer's
    /// published L2 identity pubkey. Returns Ok(()) on valid sig.
    pub fn verify<C: Verification>(
        &self,
        secp: &Secp256k1<C>,
        sequencer_pubkey: &XOnlyPublicKey,
    ) -> Result<(), bitcoin::secp256k1::Error> {
        let digest = Self::sighash(
            self.tx_hash,
            self.target_l2_height,
            self.accepted_at_unix,
        )
        .0;
        let msg = Message::from_digest(digest);
        secp.verify_schnorr(&self.sequencer_sig, &msg, sequencer_pubkey)
    }
}

/// Response from `GET /head`. The L2 tip the responding service knows.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct HeadResponse {
    pub height: u32,
    pub l2_block_hash: H256,
    pub state_root: H256,
    pub l1_height: u32,
}

/// Response from `GET /balance/:addr`. Carries the account values *and*
/// an SMT inclusion proof a light client can verify against an
/// independently-known `state_root` (e.g. one walked off L1 via
/// Esplora).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct BalanceResponse {
    /// L2 address (x-only pubkey, hex-encoded).
    #[cfg_attr(
        feature = "std",
        schema(value_type = String, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
    pub address: L2Address,
    pub balance: Amount,
    pub nonce: u64,
    /// L2 height of the state these values are drawn from.
    pub l2_height: u32,
    /// The state_root computed at that L2 height. Redundant given
    /// `state_components` (it's their hash) but convenient for display
    /// and direct comparison against the L1-derived value.
    pub state_root: H256,
    /// Snapshot of the other inputs to `state_root`. A light client
    /// recomputes `state_components.state_root()` and checks it agrees
    /// with `state_root` (self-consistency) and with the L1-derived
    /// value for the same L2 height (binding to the chain).
    pub state_components: StateComponents,
    /// SMT inclusion (or non-inclusion) proof for `address` against
    /// `state_components.accounts_root`.
    pub proof: InclusionProof,
    /// Total atoms ever minted on this ledger. Equal to the sum of
    /// every account balance (transfers are conservative). Not part
    /// of `state_components` / state_root — exposed for stats panels.
    /// Light clients trust this only at bootstrap; subsequent updates
    /// are accumulated by walking block witnesses.
    #[serde(default)]
    pub total_minted_atoms: Amount,
}
