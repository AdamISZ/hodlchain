//! Shared HTTP DTOs spoken by the wallet, sequencer and node.

use crate::hash::H256;
use crate::proof::MintProofEnvelope;
use crate::smt::InclusionProof;
use crate::state::StateComponents;
use crate::tx::{Amount, L2Address, SignedTransfer};
use alloc::string::String;
use serde::{Deserialize, Serialize};

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
}
