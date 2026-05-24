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
use crate::tx::{L2Address, L2Tx};
use alloc::vec::Vec;
use bitcoin::OutPoint;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct L2BlockHeader {
    pub height: u32,
    pub prev_hash: H256,
    /// L1 block under which this L2 block is anchored. With sub-L1
    /// block cadence, this is "the L1 tip observed at production
    /// time" — many L2 blocks can share the same `l1_height` while
    /// L1 is between blocks.
    pub l1_block_hash: H256,
    pub l1_height: u32,
    pub txs_root: H256,
    pub state_root: H256,
    /// Unix seconds.
    pub timestamp: u64,
    /// L1 outpoint that roots the sequencer's attestation chain.
    /// Some only in the genesis header (height 0); None otherwise.
    /// Nodes pick this up at cold-start and walk the chain forward
    /// from it: each subsequent L1 attestation is the unique tx
    /// that spends the previous anchor and outputs a new anchor at
    /// vout=1 (with the attestation OP_RETURN at vout=0).
    #[cfg_attr(feature = "std", schema(value_type = Option<crate::schemas::OutPointWire>))]
    pub anchor_outpoint: Option<OutPoint>,
    /// L2 identity of whoever produced this block. The field is in
    /// every header so a future multi-sequencer / threshold-signing
    /// design — where each block names the responsible party —
    /// doesn't require a hard fork. Under threshold signing it
    /// would hold a single aggregated L2 address.
    #[cfg_attr(
        feature = "std",
        schema(value_type = Option<String>, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
    pub producer: Option<L2Address>,
    /// Chain-wide fee destination, set at genesis and immutable
    /// thereafter. Some only in the genesis header (height 0);
    /// None on every subsequent block. Followers / light clients
    /// use this to seed their `LedgerState.sequencer_fee_address`
    /// before computing genesis state_root. Distinct from
    /// `producer` so a future multi-sequencer chain — where each
    /// block names a different responsible party — can still
    /// commit to a single fee destination from chain init.
    #[cfg_attr(
        feature = "std",
        schema(value_type = Option<String>, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
    pub sequencer_fee_address: Option<L2Address>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct L2Block {
    pub header: L2BlockHeader,
    pub txs: Vec<L2Tx>,
}

impl L2BlockHeader {
    /// Canonical block-hash: domain-separated sha256 over a fixed
    /// field-by-field byte layout. v3 — adds the `producer` field
    /// tail; the v2 prefix stays identical so the change is
    /// localised.
    ///
    /// Layout (all multi-byte values big-endian):
    /// ```text
    /// "hodl-block-v3"
    ///   || height(4)            || prev_hash(32)        || l1_block_hash(32)
    ///   || l1_height(4)         || txs_root(32)         || state_root(32)
    ///   || timestamp(8)         || has_anchor(1)
    ///   || if has_anchor: anchor_outpoint.txid(32) || anchor_outpoint.vout(4)
    ///   || has_producer(1)
    ///   || if has_producer: producer.serialize(32)
    ///   || has_fee_address(1)
    ///   || if has_fee_address: sequencer_fee_address.serialize(32)
    /// ```
    pub fn block_hash(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-block-v3");
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
        match &self.producer {
            Some(p) => {
                h.update([1u8]);
                h.update(p.serialize());
            }
            None => {
                h.update([0u8]);
            }
        }
        match &self.sequencer_fee_address {
            Some(a) => {
                h.update([1u8]);
                h.update(a.serialize());
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
/// initial `r`, retarget-window counters, and `sequencer_fee_address`).
/// `anchor_outpoint` is the chain root for L1 attestation transactions;
/// subsequent L2 blocks have `anchor_outpoint = None`. `producer` is
/// the sequencer's L2 identity address — `None` until the sequencer
/// identity key is wired through genesis in Phase 3.
///
/// `sequencer_fee_address` is also threaded in here so the genesis
/// state_root commits to the chain's fee destination from block 0.
pub fn genesis(
    l1_block_hash: H256,
    l1_height: u32,
    timestamp: u64,
    anchor_outpoint: OutPoint,
    producer: Option<L2Address>,
    sequencer_fee_address: Option<L2Address>,
) -> L2Block {
    let txs: Vec<L2Tx> = Vec::new();
    let mut genesis_state = LedgerState::new();
    genesis_state.sequencer_fee_address = sequencer_fee_address;
    let header = L2BlockHeader {
        height: 0,
        prev_hash: H256::ZERO,
        l1_block_hash,
        l1_height,
        txs_root: L2Block::compute_txs_root(&txs),
        state_root: genesis_state.state_root(),
        timestamp,
        anchor_outpoint: Some(anchor_outpoint),
        producer,
        sequencer_fee_address,
    };
    L2Block { header, txs }
}
