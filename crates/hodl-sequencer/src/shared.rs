//! Shared state held by the HTTP server and the producer loop.

use hodl_core::hash::H256;
use hodl_core::state::LedgerState;
use hodl_core::tx::{MintEntry, SignedTransfer};
use std::collections::BTreeSet;
use std::sync::Mutex;

/// Mempool of L2-ready txs. Mints land here only after submit-time
/// L1-verification has produced a `MintCredit`; we keep both the event
/// and the witness because the witness is committed into the L2 block.
#[derive(Default)]
pub struct Mempool {
    pub mints: Vec<MintEntry>,
    pub transfers: Vec<SignedTransfer>,
    /// Nullifiers seen in mempool but not yet committed to state.
    pub pending_nullifiers: BTreeSet<String>,
}

impl Mempool {
    pub fn drain(&mut self) -> (Vec<MintEntry>, Vec<SignedTransfer>) {
        self.pending_nullifiers.clear();
        (std::mem::take(&mut self.mints), std::mem::take(&mut self.transfers))
    }
}

/// Concrete, latest L2 head info. Recomputed after each produced block.
#[derive(Clone, Debug, Default)]
pub struct HeadInfo {
    pub height: u32,
    pub block_hash: H256,
    pub state_root: H256,
    pub l1_height: u32,
    #[allow(dead_code)]
    pub l1_block_hash: H256,
}

pub struct Shared {
    pub state: Mutex<LedgerState>,
    pub mempool: Mutex<Mempool>,
    pub head: Mutex<HeadInfo>,
}

impl Shared {
    pub fn new(state: LedgerState, head: HeadInfo) -> Self {
        Self {
            state: Mutex::new(state),
            mempool: Mutex::new(Mempool::default()),
            head: Mutex::new(head),
        }
    }
}
