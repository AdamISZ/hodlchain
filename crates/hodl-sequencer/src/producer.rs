//! Block production loop. One L2 block per L1 block.

use anyhow::Result;
use bitcoin::secp256k1::Secp256k1;
use hodl_core::block::{L2Block, L2BlockHeader};
use hodl_core::hash::H256;
use hodl_core::op_return::Attestation;
use hodl_core::proof::MintProof;
use hodl_core::state::LedgerState;
use hodl_core::tx::{L2Tx, MintEntry};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bitcoind::SequencerL1;
use crate::shared::{HeadInfo, Shared};
use crate::store::Store;

pub struct Producer {
    pub shared: Arc<Shared>,
    pub store: Arc<Mutex<Store>>,
    pub l1: Arc<SequencerL1>,
    pub poll_interval: Duration,
}

impl Producer {
    pub async fn run(self) {
        loop {
            if let Err(e) = self.tick().await {
                tracing::error!(error = ?e, "producer tick failed");
            }
            tokio::time::sleep(self.poll_interval).await;
        }
    }

    async fn tick(&self) -> Result<()> {
        let l1 = self.l1.clone();
        let tip: u32 = tokio::task::spawn_blocking(move || l1.block_count())
            .await??;

        let cursor: u32 = {
            let store = self.store.lock().unwrap();
            store.l1_cursor()?.unwrap_or(0)
        };

        if tip <= cursor {
            // No new L1 blocks. Still: retry posting any un-attested L2 blocks.
            self.retry_attestations().await?;
            return Ok(());
        }

        for next_l1_height in (cursor + 1)..=tip {
            self.produce_one(next_l1_height).await?;
        }
        Ok(())
    }

    async fn produce_one(&self, l1_height: u32) -> Result<()> {
        let l1 = self.l1.clone();
        let l1_block_hash_hex: String =
            tokio::task::spawn_blocking(move || l1.block_hash_hex(l1_height)).await??;
        let l1_block_hash = parse_l1_block_hash(&l1_block_hash_hex)?;

        // Snapshot mempool.
        let (mints, transfers) = {
            let mut m = self.shared.mempool.lock().unwrap();
            m.drain()
        };

        let mut txs: Vec<L2Tx> = Vec::new();

        let secp = Secp256k1::new();
        // Apply candidate txs to a clone of current state; drop invalid ones.
        let mut state_clone: LedgerState = self.shared.state.lock().unwrap().clone();
        // Snapshot the active r at block start — every mint in this block
        // earns amounts computed at this r (so the node, replaying with
        // the same state, produces matching credits).
        let r_for_block = state_clone.current_r;

        for entry in mints {
            // Re-verify the witness at the *current* r and rebuild the
            // event from the freshly-derived credit. If r has retargeted
            // since the user submitted, the amount they get will reflect
            // the new r — not the submit-time estimate.
            let credit_result = entry.witness.verify(
                &secp,
                self.l1.as_ref(),
                entry.event.l2_destination,
                r_for_block,
            );
            let credit = match credit_result {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "dropping mint at block-build (witness reverify)");
                    continue;
                }
            };
            let fresh_entry = MintEntry { event: credit.event, witness: entry.witness };
            let tx = L2Tx::Mint(fresh_entry);
            match state_clone.apply(&secp, &tx) {
                Ok(()) => txs.push(tx),
                Err(e) => tracing::warn!(error = %e, "dropping mint at block-build (apply)"),
            }
        }
        for st in transfers {
            let tx = L2Tx::Transfer(st);
            match state_clone.apply(&secp, &tx) {
                Ok(()) => txs.push(tx),
                Err(e) => tracing::warn!(error = %e, "dropping transfer at block-build"),
            }
        }

        let (prev_hash, prev_height) = {
            let head = self.shared.head.lock().unwrap();
            (head.block_hash, head.height)
        };

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let new_height = prev_height + 1;
        // Retarget happens AFTER txs apply, at the block-end boundary.
        // This shifts state_clone.current_r if new_height is a window
        // boundary; the state root reflects the post-retarget r.
        state_clone.end_of_block(new_height);

        let txs_root = L2Block::compute_txs_root(&txs);
        let state_root = state_clone.state_root();
        let header = L2BlockHeader {
            height: new_height,
            prev_hash,
            l1_block_hash,
            l1_height,
            txs_root,
            state_root,
            timestamp: now,
            anchor_outpoint: None, // non-genesis: chain advances by spending, not by re-anchoring
        };
        let block = L2Block { header, txs };
        let block_hash = block.hash();

        // Commit: replace state, persist, advance head + cursor.
        {
            let mut state = self.shared.state.lock().unwrap();
            *state = state_clone;
        }
        {
            let mut store = self.store.lock().unwrap();
            store.write_block_and_state(&block, &*self.shared.state.lock().unwrap())?;
            store.set_l1_cursor(l1_height)?;
        }
        {
            let mut head = self.shared.head.lock().unwrap();
            *head = HeadInfo {
                height: block.header.height,
                block_hash,
                state_root: block.header.state_root,
                l1_height,
                l1_block_hash,
            };
        }

        tracing::info!(
            l1_height,
            l2_height = block.header.height,
            txs = block.txs.len(),
            "produced L2 block"
        );

        // Best-effort attest. Failure leaves the block unattested; the
        // next tick's retry_attestations picks it up before producing N+1.
        let att = Attestation::new(block.header.height, block_hash, block.header.state_root);
        let height = block.header.height;
        let anchor = {
            let s = self.store.lock().unwrap();
            s.get_anchor()?
                .ok_or_else(|| anyhow::anyhow!("anchor not initialised in store"))?
        };
        let l1 = self.l1.clone();
        match tokio::task::spawn_blocking(move || l1.post_attestation_chained(&att, anchor))
            .await?
        {
            Ok((txid, new_anchor)) => {
                let s = self.store.lock().unwrap();
                s.set_attested_txid(height, &txid)?;
                s.set_anchor(&new_anchor)?;
                tracing::info!(
                    l2_height = height,
                    %txid,
                    new_anchor = %format_args!("{}:{}", new_anchor.txid, new_anchor.vout),
                    "posted attestation"
                );
            }
            Err(e) => {
                tracing::warn!(l2_height = height, error = ?e, "attestation post failed");
            }
        }
        Ok(())
    }

    async fn retry_attestations(&self) -> Result<()> {
        // Walk un-attested blocks in ascending height. Stop at the first
        // failure since the chain anchor only advances on success.
        loop {
            let pending = {
                let s = self.store.lock().unwrap();
                s.latest_unattested_height()?
            };
            let Some(height) = pending else { return Ok(()) };
            let block = {
                let s = self.store.lock().unwrap();
                match s.get_block(height)? {
                    Some(b) => b,
                    None => return Ok(()),
                }
            };
            let anchor = {
                let s = self.store.lock().unwrap();
                s.get_anchor()?
                    .ok_or_else(|| anyhow::anyhow!("anchor not initialised in store"))?
            };
            let att = Attestation::new(height, block.hash(), block.header.state_root);
            let l1 = self.l1.clone();
            match tokio::task::spawn_blocking(move || l1.post_attestation_chained(&att, anchor))
                .await?
            {
                Ok((txid, new_anchor)) => {
                    let s = self.store.lock().unwrap();
                    s.set_attested_txid(height, &txid)?;
                    s.set_anchor(&new_anchor)?;
                    tracing::info!(
                        l2_height = height,
                        %txid,
                        "posted attestation (retry)"
                    );
                    // Continue draining the unattested queue.
                }
                Err(e) => {
                    tracing::debug!(l2_height = height, error = ?e, "attestation retry failed");
                    return Ok(());
                }
            }
        }
    }
}

fn parse_l1_block_hash(s: &str) -> Result<H256> {
    // bitcoin block hashes are big-endian-displayed sha256d; we just keep
    // them as opaque H256 bytes (using the byte order returned by RPC).
    let bytes = hex::decode(s)?;
    if bytes.len() != 32 {
        anyhow::bail!("bad l1 block hash length: {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(H256(out))
}
