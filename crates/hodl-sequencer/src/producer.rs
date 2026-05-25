//! Block production loop.
//!
//! Two independent cadences run concurrently:
//!
//! 1. **L2 block production** — `block_interval` (default 30s).
//!    On each tick, drain the mempool, build a new L2 block at
//!    height = prev + 1 with the current L1 tip recorded as the
//!    "L1 view at production time". No L1 attestation here.
//!
//! 2. **L1 attestation** — `poll_interval` (default 1s).
//!    Poll bitcoind for the L1 tip. When it advances past the
//!    previously-attested L1 height, post **one** OP_RETURN
//!    attestation covering the *current* L2 head — a batch covering
//!    every L2 block produced since the previous attestation. The
//!    attestation tx spends the previous anchor and creates a new
//!    one; on success, `last_attested_l1_height` and `anchor`
//!    advance.
//!
//! If an attestation post fails, no state advances — the next L1
//! tick will retry, and because the attestation always references
//! the *current* L2 head (not a specific block), the retry simply
//! batches more L2 blocks. There's no per-L2-block "unattested"
//! tracking anymore.

use anyhow::Result;
use bitcoin::secp256k1::Secp256k1;
use bitcoin::{OutPoint, Txid};
use hodl_core::block::{L2Block, L2BlockHeader};
use hodl_core::hash::H256;
use hodl_core::op_return::Attestation;
use hodl_core::proof::MintProof;
use hodl_core::state::LedgerState;
use hodl_core::tx::{L2Tx, MintEntry};
use hodl_core::witness::BlockWitness;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::bitcoind::{AttestationStatus, SequencerL1};
use crate::shared::{HeadInfo, Shared};
use crate::store::{PendingAttestation, Store};

/// L1 confirmation depth at which we consider an attestation tx
/// final and stop tracking it for reorg recovery. Bitcoin reorgs of
/// depth ≥ 2 are rare in practice (single-digit per year
/// historically); 2 is the conservative default for the POC.
const REORG_FINALITY_DEPTH: u32 = 2;

pub struct Producer {
    pub shared: Arc<Shared>,
    pub store: Arc<Mutex<Store>>,
    pub l1: Arc<SequencerL1>,
    /// How often to check bitcoind for new L1 blocks (and post an
    /// attestation if needed).
    pub poll_interval: Duration,
    /// How often to produce a new L2 block, regardless of L1.
    pub block_interval: Duration,
}

impl Producer {
    pub async fn run(self) {
        // Two interleaved tickers via `tokio::select!`. Sharing the
        // single producer struct keeps state coherent — there's no
        // concurrent mutation of mempool / shared state across the
        // two paths since each path completes before yielding.
        let mut block_tick = tokio::time::interval(self.block_interval);
        // `MissedTickBehavior::Skip` so if produce_block ever takes
        // longer than the interval, we don't accumulate a backlog.
        block_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut l1_tick = tokio::time::interval(self.poll_interval);
        l1_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = block_tick.tick() => {
                    if let Err(e) = self.produce_block().await {
                        tracing::error!(error = ?e, "block production failed");
                    }
                }
                _ = l1_tick.tick() => {
                    // Two L1-side tasks per tick: reorg-monitor
                    // any pending attestations, then post a fresh
                    // one if L1 has advanced. The monitor goes
                    // first so a reorg recovery can revert the
                    // anchor before the new post tries to chain
                    // from it.
                    if let Err(e) = self.monitor_pending().await {
                        tracing::error!(error = ?e, "pending attestation monitor failed");
                    }
                    if let Err(e) = self.check_and_attest().await {
                        tracing::error!(error = ?e, "attestation check failed");
                    }
                }
            }
        }
    }

    /// Build one L2 block from the current mempool, recording the
    /// current L1 tip as the block's `l1_height` / `l1_block_hash`.
    /// Does not post any L1 attestation.
    async fn produce_block(&self) -> Result<()> {
        let l1 = self.l1.clone();
        let l1_height: u32 = tokio::task::spawn_blocking(move || l1.block_count()).await??;
        let l1c = self.l1.clone();
        let l1_block_hash_hex: String = tokio::task::spawn_blocking(move || {
            l1c.block_hash_hex(l1_height)
        })
        .await??;
        let l1_block_hash = parse_l1_block_hash(&l1_block_hash_hex)?;

        // Snapshot mempool.
        let (mints, transfers) = {
            let mut m = self.shared.mempool.lock().unwrap();
            m.drain()
        };

        let mut txs: Vec<L2Tx> = Vec::new();

        let secp = Secp256k1::new();
        let prior_state: LedgerState = self.shared.state.lock().unwrap().clone();
        let mut state_clone: LedgerState = prior_state.clone();

        for entry in mints {
            let credit_result = entry.witness.verify(
                &secp,
                self.l1.as_ref(),
                entry.event.l2_destination,
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
        // v2 design: no retargeting — `r` is a fixed consensus
        // constant (`hodl_core::consensus::R`). No end-of-block hook.

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
            anchor_outpoint: None,
            producer: Some(self.shared.identity_pubkey),
            sequencer_fee_address: None, // genesis-only field
        };
        let block = L2Block { header, txs };
        let block_hash = block.hash();

        let witness = BlockWitness::build(&prior_state, &block.txs, new_height);

        {
            let mut state = self.shared.state.lock().unwrap();
            *state = state_clone;
        }
        {
            let mut store = self.store.lock().unwrap();
            store.write_block_and_state(
                &block,
                &*self.shared.state.lock().unwrap(),
                &witness,
            )?;
            // `l1_cursor` is kept as "the highest L1 height we have
            // observed at production time". It's informational; the
            // attestation path uses `last_attested_l1_height`
            // separately.
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

        Ok(())
    }

    /// Check the L1 tip; if it has advanced past
    /// `last_attested_l1_height`, drain any pending mempool into a
    /// fresh L2 block then post one attestation covering the new
    /// L2 head. Idempotent in the no-advance case.
    ///
    /// The drain-before-attest ordering matters: without it, a tx
    /// submitted just before L1 advanced could land in mempool, get
    /// attested-too-early (when L2 head still didn't contain it),
    /// and then be batched into the *next* attestation a full L1
    /// block later. By producing an L2 block first, the attestation
    /// always commits to the freshest possible state.
    async fn check_and_attest(&self) -> Result<()> {
        let l1c = self.l1.clone();
        let tip: u32 = tokio::task::spawn_blocking(move || l1c.block_count()).await??;
        let last_attested: u32 = {
            let s = self.store.lock().unwrap();
            // Genesis attests *as* the genesis state — there's no
            // explicit attestation post for it. Default
            // `last_attested_l1_height` to `l1_genesis_height` (the
            // store's `l1_cursor` was set to that at bootstrap).
            s.last_attested_l1_height()?
                .unwrap_or_else(|| s.l1_cursor().ok().flatten().unwrap_or(0))
        };
        if tip <= last_attested {
            return Ok(());
        }

        // Drain mempool into a fresh L2 block so the attestation
        // we're about to post covers it. The 30s block_tick keeps
        // producing intermediate blocks between L1 events; this
        // extra production right before attestation is the cheap
        // fix for the L1-vs-block-tick race.
        self.produce_block().await?;

        // Snapshot current L2 head — this is what we attest to.
        let head = self.shared.head.lock().unwrap().clone();
        let anchor = {
            let s = self.store.lock().unwrap();
            s.get_anchor()?
                .ok_or_else(|| anyhow::anyhow!("anchor not initialised in store"))?
        };

        let att = Attestation::new(head.height, head.block_hash, head.state_root);
        let spent_anchor = anchor;
        let l1 = self.l1.clone();
        match tokio::task::spawn_blocking(move || l1.post_attestation_chained(&att, anchor)).await? {
            Ok((txid, new_anchor)) => {
                let s = self.store.lock().unwrap();
                s.set_anchor(&new_anchor)?;
                s.set_last_attested_l1_height(tip)?;
                // Track this attestation until it reaches
                // REORG_FINALITY_DEPTH L1 confirmations. The
                // monitor_pending tick checks on each L1 poll.
                let mut pending = s.pending_attestations()?;
                pending.push(PendingAttestation {
                    txid: txid.to_string(),
                    spent_anchor: format!("{}:{}", spent_anchor.txid, spent_anchor.vout),
                    new_anchor: format!("{}:{}", new_anchor.txid, new_anchor.vout),
                    l2_head_height: head.height,
                    posted_at_l1_height: tip,
                });
                s.set_pending_attestations(&pending)?;
                tracing::info!(
                    l1_tip = tip,
                    l2_head = head.height,
                    %txid,
                    new_anchor = %format_args!("{}:{}", new_anchor.txid, new_anchor.vout),
                    "posted attestation"
                );
            }
            Err(e) => {
                tracing::warn!(l1_tip = tip, error = ?e, "attestation post failed; will retry next L1 tick");
            }
        }
        Ok(())
    }

    /// Walk every still-pending attestation; promote those that have
    /// reached `REORG_FINALITY_DEPTH`, retry those still in mempool,
    /// and recover from L1 reorgs that have evicted the tx entirely.
    async fn monitor_pending(&self) -> Result<()> {
        let pending = {
            let s = self.store.lock().unwrap();
            s.pending_attestations()?
        };
        if pending.is_empty() {
            return Ok(());
        }

        let mut surviving = Vec::with_capacity(pending.len());
        let mut anchor_to_revert: Option<OutPoint> = None;

        for p in pending {
            let txid = Txid::from_str(&p.txid)
                .map_err(|e| anyhow::anyhow!("malformed pending txid {}: {e}", p.txid))?;
            let l1 = self.l1.clone();
            let status = tokio::task::spawn_blocking(move || l1.attestation_status(&txid)).await??;

            match status {
                AttestationStatus::Confirmed { confirmations, block_height } => {
                    if confirmations >= REORG_FINALITY_DEPTH {
                        tracing::info!(
                            txid = %p.txid,
                            l2_head = p.l2_head_height,
                            confirmations,
                            block_height = ?block_height,
                            "attestation finalised (≥ REORG_FINALITY_DEPTH); dropping from pending"
                        );
                        // Drop from list — past reorg risk.
                    } else {
                        tracing::trace!(
                            txid = %p.txid,
                            confirmations,
                            block_height = ?block_height,
                            "attestation confirming"
                        );
                        surviving.push(p);
                    }
                }
                AttestationStatus::Mempool => {
                    // Still in flight; wait. (No retry — bitcoind
                    // handles rebroadcasts.)
                    tracing::trace!(txid = %p.txid, "attestation still in mempool");
                    surviving.push(p);
                }
                AttestationStatus::Reorged => {
                    // Bitcoind still has it; it'll be re-mined. Hold
                    // off treating this as a recovery case unless it
                    // stays Reorged across several ticks. For now
                    // just keep tracking.
                    tracing::warn!(
                        txid = %p.txid,
                        l2_head = p.l2_head_height,
                        "attestation tx reorged but still in mempool — bitcoind will re-mine"
                    );
                    surviving.push(p);
                }
                AttestationStatus::Missing => {
                    // Tx is gone from bitcoind's view entirely. The
                    // chain anchor at `new_anchor` doesn't exist on
                    // L1 anymore. Revert to `spent_anchor` so the
                    // next `check_and_attest` can chain from there.
                    // We also rewind `last_attested_l1_height` so
                    // the next L1 tick fires a fresh post.
                    let spent = parse_outpoint(&p.spent_anchor)?;
                    tracing::warn!(
                        txid = %p.txid,
                        spent_anchor = %p.spent_anchor,
                        new_anchor = %p.new_anchor,
                        l2_head = p.l2_head_height,
                        "attestation tx missing from L1 (reorged + evicted); \
                         reverting anchor to spent and re-posting on next tick"
                    );
                    anchor_to_revert = Some(spent);
                    // Don't carry forward in `surviving`.
                }
            }
        }

        {
            let s = self.store.lock().unwrap();
            s.set_pending_attestations(&surviving)?;
            if let Some(revert_to) = anchor_to_revert {
                s.set_anchor(&revert_to)?;
                // Rewind so the next L1 advance triggers a fresh
                // attestation covering the current L2 head, chained
                // from `revert_to`. We rewind by 1 to be sure tip >
                // last_attested at the next check.
                let last = s.last_attested_l1_height()?.unwrap_or(0);
                if last > 0 {
                    s.set_last_attested_l1_height(last.saturating_sub(1))?;
                }
            }
        }

        Ok(())
    }
}

fn parse_outpoint(s: &str) -> Result<OutPoint> {
    let (txid, vout) = s
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("malformed outpoint: {s}"))?;
    Ok(OutPoint {
        txid: Txid::from_str(txid)?,
        vout: vout.parse()?,
    })
}

fn parse_l1_block_hash(s: &str) -> Result<H256> {
    let bytes = hex::decode(s)?;
    if bytes.len() != 32 {
        anyhow::bail!("bad l1 block hash length: {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(H256(out))
}
