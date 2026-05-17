//! Follower loop.
//!
//! Each tick:
//!   1. Read L1 tip.
//!   2. For each new L1 block, ask: "is there a tx here that spends the
//!      current chain anchor?" — at most one per block by construction
//!      (one UTXO can only be spent once).
//!   3. If yes, extract the OP_RETURN attestation from vout=0, fetch the
//!      matching L2 block body from the sequencer, validate, apply,
//!      advance the chain anchor to vout=1.
//!   4. Advance the L1 cursor.
//!
//! Validation: structural checks against the attestation, then for every
//! mint in the body, re-run the witness against L1 via
//! `hodl_core::proof::verify_mint_entry` and confirm the produced credit
//! matches the declared event. The node's block validity is therefore
//! independent of trusting the sequencer.
//!
//! Authentication: by following the chain from a genesis anchor outpoint
//! embedded in L2 block 0's header, the node ignores any HODL-magic
//! OP_RETURN that isn't on the canonical chain. An impostor would have
//! to spend the same anchor, which L1 itself prevents.

use anyhow::{anyhow, bail, Result};
use bitcoin::secp256k1::Secp256k1;
use hodl_core::block::L2Block;
use hodl_core::op_return::Attestation;
use hodl_core::proof::verify_mint_entry;
use hodl_core::state::LedgerState;
use hodl_core::tx::L2Tx;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bitcoind::{ChainAdvance, NodeL1};
use crate::seq_client::SeqClient;
use crate::shared::{HeadInfo, Shared};
use crate::store::Store;

pub struct Follower {
    pub shared: Arc<Shared>,
    pub store: Arc<Mutex<Store>>,
    pub l1: Arc<NodeL1>,
    pub seq: Arc<SeqClient>,
    pub poll_interval: Duration,
}

impl Follower {
    pub async fn run(self) {
        loop {
            if let Err(e) = self.tick().await {
                tracing::error!(error = ?e, "follower tick failed");
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn tick(&self) -> Result<()> {
        let l1 = self.l1.clone();
        let tip: u32 = tokio::task::spawn_blocking(move || l1.block_count()).await??;

        let cursor: u32 = {
            let store = self.store.lock().unwrap();
            store.l1_cursor()?.unwrap_or(0)
        };

        if tip <= cursor {
            return Ok(());
        }

        for h in (cursor + 1)..=tip {
            // Multiple chain advances can sit in a single L1 block
            // (sequencer broadcasts as soon as each L2 block is built;
            // if several mempool txs land in the same L1 block, the chain
            // advances several times). Loop until this block yields no
            // more advances.
            loop {
                let current_anchor = {
                    let s = self.store.lock().unwrap();
                    s.get_anchor()?
                        .ok_or_else(|| anyhow!("anchor not initialised in store"))?
                };
                let l1 = self.l1.clone();
                let advance = tokio::task::spawn_blocking(move || {
                    l1.scan_block_for_chain_advance(h, &current_anchor)
                })
                .await??;
                match advance {
                    Some(adv) => self.process_advance(adv).await?,
                    None => break,
                }
            }

            let store = self.store.lock().unwrap();
            store.set_l1_cursor(h)?;
        }
        Ok(())
    }

    async fn process_advance(&self, adv: ChainAdvance) -> Result<()> {
        let att = adv.attestation;
        let head_height = self.shared.head.lock().unwrap().height;

        if att.height <= head_height {
            tracing::debug!(
                l2_height = att.height,
                head = head_height,
                "skipping already-known attestation"
            );
            return Ok(());
        }

        let block: L2Block = self.seq.get_block(att.height).await?;
        validate_block(&block, &att)?;

        // Snapshot the r that the producer would have used at the start
        // of this block — it's the r in our current state, before we
        // apply this block. The producer used the same value.
        let r_for_block = self.shared.state.lock().unwrap().current_r;

        // Re-verify every mint witness against L1 before touching state.
        // Each verify is a blocking gettxout call, so off the runtime thread.
        for (i, tx) in block.txs.iter().enumerate() {
            if let L2Tx::Mint(entry) = tx {
                let entry = entry.clone();
                let l1 = self.l1.clone();
                let r = r_for_block;
                let height = block.header.height;
                tokio::task::spawn_blocking(move || {
                    let secp = Secp256k1::verification_only();
                    verify_mint_entry(&entry, &secp, l1.as_ref(), r)
                })
                .await
                .map_err(|e| anyhow!("join: {e}"))?
                .map_err(|e| anyhow!("mint entry #{i} in block {height} invalid: {e}"))?;
            }
        }

        // Replay txs against current state. Apply, then close the block
        // — `end_of_block` runs the retarget at window boundaries.
        let secp = Secp256k1::new();
        let mut next_state: LedgerState = self.shared.state.lock().unwrap().clone();
        for tx in &block.txs {
            next_state
                .apply(&secp, tx)
                .map_err(|e| anyhow!("tx in block {} invalid: {e}", block.header.height))?;
        }
        next_state.end_of_block(block.header.height);

        // Continuity + state-root sanity.
        let computed_state_root = next_state.state_root();
        if computed_state_root != block.header.state_root {
            bail!(
                "state root mismatch at L2 height {}: computed {} != header {}",
                block.header.height,
                computed_state_root,
                block.header.state_root
            );
        }
        let prev_hash = self.shared.head.lock().unwrap().block_hash;
        if block.header.prev_hash != prev_hash {
            bail!(
                "prev_hash mismatch at L2 height {}: block.prev_hash={} != head={}",
                block.header.height,
                block.header.prev_hash,
                prev_hash
            );
        }

        // Commit. The chain anchor advances to the change output of the
        // attestation tx we just followed.
        let block_hash = block.hash();
        let new_head = HeadInfo {
            height: block.header.height,
            block_hash,
            state_root: block.header.state_root,
            l1_height: adv.l1_height,
        };
        {
            let mut state = self.shared.state.lock().unwrap();
            *state = next_state;
        }
        {
            let mut store = self.store.lock().unwrap();
            store.write_block_and_state(&block, &*self.shared.state.lock().unwrap())?;
            store.set_anchor(&adv.new_anchor)?;
            // Record the chain link for the Esplora-compatible /outspend
            // endpoint that light clients walk.
            store.record_anchor_spend(&adv.spent_anchor, &adv.txid, adv.l1_height)?;
        }
        {
            let mut head = self.shared.head.lock().unwrap();
            *head = new_head;
        }

        tracing::info!(
            l2_height = block.header.height,
            l1_height = adv.l1_height,
            txs = block.txs.len(),
            att_txid = %adv.txid,
            "applied L2 block; advanced chain anchor"
        );
        Ok(())
    }
}

/// Cheap structural checks: the attestation matches the body in height,
/// hash and state root. Tx-replay and continuity checks happen in the
/// caller because they require the live state.
fn validate_block(block: &L2Block, att: &Attestation) -> Result<()> {
    if block.header.height != att.height {
        bail!(
            "block height {} disagrees with attestation height {}",
            block.header.height,
            att.height
        );
    }
    let computed_hash = block.hash();
    if computed_hash != att.l2_block_hash {
        bail!(
            "block hash {} disagrees with attestation hash {}",
            computed_hash,
            att.l2_block_hash
        );
    }
    if block.header.state_root != att.state_root {
        bail!(
            "block state root {} disagrees with attestation state root {}",
            block.header.state_root,
            att.state_root
        );
    }
    let computed_txs_root = L2Block::compute_txs_root(&block.txs);
    if computed_txs_root != block.header.txs_root {
        bail!(
            "txs root mismatch at L2 height {}: computed {} != header {}",
            block.header.height,
            computed_txs_root,
            block.header.txs_root
        );
    }
    Ok(())
}
