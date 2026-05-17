//! L2 block header and body.
//!
//! Cadence: one L2 block per L1 block.
//!
//! `block_hash = sha256("hodl-block-v0" || canonical(header))`
//!
//! The header commits to `txs_root` (over the block body) and `state_root`
//! (post-state). The OP_RETURN attestation duplicates `state_root` and the
//! `block_hash`, so light clients can verify state membership without
//! downloading the body, and can verify block-chain continuity without
//! downloading every L1 block in between.

use crate::hash::H256;
use crate::state::LedgerState;
use crate::tx::L2Tx;
use alloc::vec::Vec;
use bitcoin::OutPoint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct L2BlockHeader {
    pub height: u32,
    pub prev_hash: H256,
    /// L1 block under which this L2 block is anchored.
    pub l1_block_hash: H256,
    pub l1_height: u32,
    pub txs_root: H256,
    pub state_root: H256,
    /// Unix seconds.
    pub timestamp: u64,
    /// L1 outpoint that roots the sequencer's attestation chain.
    /// Some only in the genesis header (height 0); None otherwise.
    /// Nodes pick this up at cold-start and walk the chain forward
    /// from it: each subsequent L2 block's L1 attestation is the
    /// unique tx that spends the previous anchor and outputs a new
    /// anchor at vout=1 (with the attestation OP_RETURN at vout=0).
    #[cfg_attr(feature = "std", schema(value_type = Option<crate::schemas::OutPointWire>))]
    pub anchor_outpoint: Option<OutPoint>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct L2Block {
    pub header: L2BlockHeader,
    pub txs: Vec<L2Tx>,
}

impl L2BlockHeader {
    /// Canonical block-hash: domain-separated sha256 over a fixed
    /// field-by-field byte layout. Replaces the v0 serde_json-based
    /// hash so the encoding is explicit (auditable) and works in
    /// no_std contexts (the state-transition zk-program builds the
    /// same hash).
    ///
    /// Layout (all multi-byte values big-endian):
    /// ```text
    /// "hodl-block-v2"
    ///   || height(4)            || prev_hash(32)        || l1_block_hash(32)
    ///   || l1_height(4)         || txs_root(32)         || state_root(32)
    ///   || timestamp(8)         || has_anchor(1)
    ///   || if has_anchor: anchor_outpoint.txid(32) || anchor_outpoint.vout(4)
    /// ```
    pub fn block_hash(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-block-v2");
        h.update(self.height.to_be_bytes());
        h.update(self.prev_hash.0);
        h.update(self.l1_block_hash.0);
        h.update(self.l1_height.to_be_bytes());
        h.update(self.txs_root.0);
        h.update(self.state_root.0);
        h.update(self.timestamp.to_be_bytes());
        match &self.anchor_outpoint {
            Some(op) => {
                h.update([1u8]);
                h.update(AsRef::<[u8]>::as_ref(&op.txid));
                h.update(op.vout.to_be_bytes());
            }
            None => {
                h.update([0u8]);
            }
        }
        H256(h.finalize().into())
    }
}

impl L2Block {
    /// Compute txs_root as sha256 over concatenated tx hashes in order.
    /// (A real Merkle tree can replace this later without changing block
    /// format, since `txs_root` is opaque to callers.)
    pub fn compute_txs_root(txs: &[L2Tx]) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-txs-v2");
        for tx in txs {
            h.update(&tx.hash().0);
        }
        H256(h.finalize().into())
    }

    pub fn hash(&self) -> H256 { self.header.block_hash() }
}

/// Genesis block: height 0, all-zero parents, empty body, sentinel L1 anchor.
///
/// `state_root` comes from an empty `LedgerState` (which commits to the
/// initial `r` and retarget-window counters in addition to accounts and
/// nullifiers). `anchor_outpoint` is the chain root for L1 attestation
/// transactions; subsequent L2 blocks have `anchor_outpoint = None`.
/// Producer and follower must compute genesis the same way.
pub fn genesis(
    l1_block_hash: H256,
    l1_height: u32,
    timestamp: u64,
    anchor_outpoint: OutPoint,
) -> L2Block {
    let txs: Vec<L2Tx> = Vec::new();
    let header = L2BlockHeader {
        height: 0,
        prev_hash: H256::ZERO,
        l1_block_hash,
        l1_height,
        txs_root: L2Block::compute_txs_root(&txs),
        state_root: LedgerState::new().state_root(),
        timestamp,
        anchor_outpoint: Some(anchor_outpoint),
    };
    L2Block { header, txs }
}
