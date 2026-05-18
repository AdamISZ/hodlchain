//! Bitcoind wrapper for the node. Read-only: tip polling and block scanning
//! for hodlcoin OP_RETURN attestations.

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::{OutPoint, ScriptBuf, Txid};
use bitcoincore_rpc::{Client, RpcApi};
use hodl_core::op_return::Attestation;
use hodl_core::proof::{L1Output, L1View};
use std::sync::Mutex;

use crate::config::NodeConfig;

pub struct NodeL1 {
    client: Mutex<Client>,
}

#[derive(Clone, Debug)]
pub struct ChainAdvance {
    pub attestation: Attestation,
    /// L1 block height at which the attestation tx was mined.
    pub l1_height: u32,
    /// The attestation tx's own txid (for diagnostics / logs).
    pub txid: Txid,
    /// The chain anchor that was spent to produce this attestation.
    pub spent_anchor: OutPoint,
    /// The new chain anchor — vout=1 of the attestation tx.
    pub new_anchor: OutPoint,
}

impl NodeL1 {
    pub fn connect(cfg: &NodeConfig) -> Result<Self> {
        let auth = cfg.bitcoincore_auth();
        let client = Client::new(&cfg.bitcoind.url, auth)
            .with_context(|| format!("connect bitcoind at {}", cfg.bitcoind.url))?;
        Ok(Self { client: Mutex::new(client) })
    }

    pub fn block_count(&self) -> Result<u32> {
        let c = self.client.lock().unwrap();
        let n = c.get_block_count()?;
        u32::try_from(n).map_err(|_| anyhow!("block count overflows u32: {n}"))
    }

    /// Fetch a transaction by txid, plus the L1 block height at which it
    /// was confirmed (None if unconfirmed). Used by the Esplora `/tx`
    /// endpoint to fill in `status.block_height` so a light wallet can
    /// compute confirmation counts during mint-witness verification.
    pub fn get_tx_with_height(
        &self,
        txid: &Txid,
    ) -> Result<(bitcoin::Transaction, Option<u32>)> {
        let c = self.client.lock().unwrap();
        let info = c.get_raw_transaction_info(txid, None)?;
        let tx = info.transaction().with_context(|| format!("decode tx {txid}"))?;
        let height = match (info.confirmations, c.get_block_count().ok()) {
            (Some(confs), Some(tip)) if confs > 0 => {
                let tip = tip as u32;
                Some(tip.saturating_sub(confs).saturating_add(1))
            }
            _ => None,
        };
        Ok((tx, height))
    }

    /// Walk every tx in L1 block `h` looking for one that spends
    /// `current_anchor`. If found, validate that vout=0 carries a
    /// well-formed hodlcoin OP_RETURN attestation and vout=1 exists
    /// (the new anchor). Returns at most one advance per block — once
    /// the anchor is spent it can't be spent again.
    ///
    /// Failure modes returned as Err: anchor was spent by a tx that
    /// doesn't look like a hodlcoin attestation (chain broken or
    /// impostor) — caller should halt.
    pub fn scan_block_for_chain_advance(
        &self,
        h: u32,
        current_anchor: &OutPoint,
    ) -> Result<Option<ChainAdvance>> {
        let c = self.client.lock().unwrap();
        let hash = c.get_block_hash(h as u64)?;
        let block = c.get_block(&hash)?;
        for tx in &block.txdata {
            let spends_anchor = tx
                .input
                .iter()
                .any(|i| i.previous_output == *current_anchor);
            if !spends_anchor {
                continue;
            }
            let txid = tx.compute_txid();
            if tx.output.len() < 2 {
                bail!(
                    "L1 tx {txid} spends anchor but has {} outputs (expected >=2 with \
                     OP_RETURN at vout=0 and new-anchor change at vout=1)",
                    tx.output.len()
                );
            }
            let att = Attestation::try_from_script(&tx.output[0].script_pubkey)
                .map_err(|e| anyhow!("L1 tx {txid} vout=0 is not an OP_RETURN: {e}"))?
                .ok_or_else(|| {
                    anyhow!(
                        "L1 tx {txid} spends anchor but vout=0 is not a hodlcoin \
                         attestation (wrong magic / length)"
                    )
                })?;
            let new_anchor = OutPoint { txid, vout: 1 };
            return Ok(Some(ChainAdvance {
                attestation: att,
                l1_height: h,
                txid,
                spent_anchor: *current_anchor,
                new_anchor,
            }));
        }
        Ok(None)
    }
}

/// L1View impl: lets the node re-run mint witnesses (which need to look
/// up outpoints on chain) during block validation.
impl L1View for NodeL1 {
    fn get_output(&self, op: &OutPoint) -> Option<L1Output> {
        let c = self.client.lock().ok()?;
        let result = c.get_tx_out(&op.txid, op.vout, Some(false)).ok()??;
        let tip = c.get_block_count().ok()? as u32;
        let confs = result.confirmations;
        let confirmed_height = tip.saturating_sub(confs).saturating_add(1);
        let script_pubkey = ScriptBuf::from_bytes(result.script_pub_key.hex.clone());
        Some(L1Output {
            value_sat: result.value.to_sat(),
            script_pubkey,
            confirmed_height,
            confirmations: confs,
        })
    }

    fn tip_height(&self) -> u32 {
        self.client
            .lock()
            .ok()
            .and_then(|c| c.get_block_count().ok())
            .map(|n| n as u32)
            .unwrap_or(0)
    }
}
