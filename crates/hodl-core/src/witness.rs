//! Per-block state-transition witness.
//!
//! A `BlockWitness` carries every piece of information a light client
//! needs (in addition to the block body) to verify, statelessly, that
//! the block's claimed `state_root` is the correct output of applying
//! the block's transactions to the prior state.
//!
//! Specifically, it contains a pre-state inclusion (or non-inclusion)
//! proof for every account touched by any transaction in the block,
//! taken against the *prior* state's `accounts_root`. The wallet:
//!
//! 1. Verifies every pre-proof against its own persisted prior root.
//! 2. Replays the block's transactions starting from those pre-states
//!    (which gives it post-states for every touched account, with all
//!    signatures and mint witnesses verified as a side effect).
//! 3. Calls `smt::apply_updates` to obtain the new `accounts_root`.
//! 4. Recomputes the full `state_root` from
//!    `(accounts_root, nullifiers_hash, sequencer_fee_address)` and
//!    checks it against `block.header.state_root`.
//!
//! Post-states aren't carried in the witness — they're derivable from
//! the block body, and forcing the wallet to derive them keeps the
//! soundness argument tight (the wallet would have replayed and
//! re-verified the txs anyway).

use crate::hash::H256;
use crate::smt::InclusionProof;
use crate::state::LedgerState;
use crate::tx::{L2Address, L2Tx};
use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct BlockWitness {
    /// L2 height this witness is for. Lets a caller cross-check it
    /// against the block body it pairs with.
    pub height: u32,
    /// The SMT `accounts_root` that pre-proofs verify against. Equals
    /// the previous block's `state_components.accounts_root`. The
    /// wallet cross-checks this against its own persisted prior
    /// `accounts_root` before trusting any of the proofs.
    pub prior_accounts_root: H256,
    /// One entry per address touched by some transaction in the block.
    /// Each entry's `.leaf` is the pre-block state at that address
    /// (`Account` or `Empty`) and `.siblings` is the SMT path at
    /// `prior_accounts_root`. Order is not significant.
    pub pre_proofs: Vec<InclusionProof>,
}

/// Addresses touched by any transaction in `txs`. Used by the
/// producer / follower to know which inclusion proofs to snapshot
/// when building a witness, and by the wallet (independently) as a
/// cross-check that the server didn't under-report.
///
/// `fee_address` (if `Some`) is added to the set when any transfer
/// is present, because transfers credit a fee to that address and
/// therefore touch it. Mint blocks alone don't touch the fee
/// account (mints don't pay fees).
pub fn touched_addresses(txs: &[L2Tx], fee_address: Option<L2Address>) -> Vec<L2Address> {
    let mut set: BTreeSet<L2Address> = BTreeSet::new();
    let mut has_transfer = false;
    for tx in txs {
        match tx {
            L2Tx::Mint(entry) => {
                set.insert(entry.event.l2_destination);
            }
            L2Tx::Transfer(t) => {
                set.insert(t.body.from);
                set.insert(t.body.to);
                has_transfer = true;
            }
        }
    }
    if has_transfer {
        if let Some(addr) = fee_address {
            set.insert(addr);
        }
    }
    set.into_iter().collect()
}

impl BlockWitness {
    /// Build a witness from the *prior* (pre-block) `LedgerState` and
    /// the block's final tx list.
    pub fn build(prior_state: &LedgerState, txs: &[L2Tx], height: u32) -> Self {
        let prior_accounts_root = prior_state.accounts_root();
        let touched = touched_addresses(txs, prior_state.sequencer_fee_address);
        let pre_proofs = touched
            .into_iter()
            .map(|addr| prior_state.account_inclusion_proof(addr))
            .collect();
        Self {
            height,
            prior_accounts_root,
            pre_proofs,
        }
    }
}
