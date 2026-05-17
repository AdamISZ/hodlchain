//! Shared state held by the HTTP server and the follower loop.

use hodl_core::hash::H256;
use hodl_core::state::LedgerState;
use std::sync::Mutex;

#[derive(Clone, Debug, Default)]
pub struct HeadInfo {
    pub height: u32,
    pub block_hash: H256,
    pub state_root: H256,
    pub l1_height: u32,
}

pub struct Shared {
    pub state: Mutex<LedgerState>,
    pub head: Mutex<HeadInfo>,
}

impl Shared {
    pub fn new(state: LedgerState, head: HeadInfo) -> Self {
        Self {
            state: Mutex::new(state),
            head: Mutex::new(head),
        }
    }
}
