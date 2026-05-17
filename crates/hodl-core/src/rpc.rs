//! Shared HTTP DTOs spoken by the wallet, sequencer and node.

use crate::hash::H256;
use crate::proof::MintProofEnvelope;
use crate::smt::InclusionProof;
use crate::state::StateComponents;
use crate::tx::{Amount, L2Address, SignedTransfer};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitMintRequest {
    pub proof: MintProofEnvelope,
    pub l2_destination: L2Address,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitMintResponse {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mint_amount: Option<Amount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nullifier_hex: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitTransferRequest {
    pub transfer: SignedTransfer,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SubmitTransferResponse {
    pub accepted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct HeadResponse {
    pub height: u32,
    pub l2_block_hash: H256,
    pub state_root: H256,
    pub l1_height: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BalanceResponse {
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
