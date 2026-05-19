//! Bitcoind wrapper for the node. Read-only: tip polling and block scanning
//! for hodlchain OP_RETURN attestations.

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

/// One UTXO at an address, as exposed by the node's
/// Esplora-compatible /address/{addr}/utxo endpoint.
#[derive(Clone, Debug)]
pub struct AddressUtxo {
    pub txid: String,
    pub vout: u32,
    pub value_sat: u64,
    /// L1 confirmation height. Always Some for results from
    /// scantxoutset (which only scans confirmed state).
    pub block_height: Option<u32>,
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

    /// Scan the chain's UTXO set for unspent outputs paying to `addr`.
    /// Backed by bitcoind's `scantxoutset` — wallet-free, slow on
    /// mainnet, fine for regtest. Returns confirmed UTXOs only; the
    /// mempool isn't included (matches what an electrs deployment
    /// would return for a fresh wallet).
    pub fn scan_address_utxos(&self, addr: &str) -> Result<Vec<AddressUtxo>> {
        use serde_json::json;
        let c = self.client.lock().unwrap();
        let result: serde_json::Value = c
            .call(
                "scantxoutset",
                &[json!("start"), json!([format!("addr({})", addr)])],
            )
            .context("scantxoutset RPC")?;
        if !result.get("success").and_then(|v| v.as_bool()).unwrap_or(false) {
            bail!("scantxoutset returned success=false: {result}");
        }
        let unspents = result
            .get("unspents")
            .and_then(|v| v.as_array())
            .ok_or_else(|| anyhow!("scantxoutset response missing 'unspents'"))?;
        let mut out = Vec::with_capacity(unspents.len());
        for u in unspents {
            let txid_s = u
                .get("txid")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow!("unspent missing txid"))?;
            let vout = u
                .get("vout")
                .and_then(|v| v.as_u64())
                .ok_or_else(|| anyhow!("unspent missing vout"))? as u32;
            // `amount` is in BTC; convert to sats.
            let amount_btc = u
                .get("amount")
                .and_then(|v| v.as_f64())
                .ok_or_else(|| anyhow!("unspent missing amount"))?;
            let value_sat = (amount_btc * 100_000_000.0).round() as u64;
            let height = u.get("height").and_then(|v| v.as_u64()).map(|h| h as u32);
            out.push(AddressUtxo {
                txid: txid_s.to_string(),
                vout,
                value_sat,
                block_height: height,
            });
        }
        Ok(out)
    }

    /// Broadcast a signed transaction. Wallet-free.
    pub fn send_raw_transaction(&self, raw: &[u8]) -> Result<Txid> {
        let c = self.client.lock().unwrap();
        Ok(c.send_raw_transaction(raw)?)
    }

    /// Walk every tx in L1 block `h` looking for one that spends
    /// `current_anchor`. If found, validate that vout=0 carries a
    /// well-formed hodlchain OP_RETURN attestation and vout=1 exists
    /// (the new anchor). Returns at most one advance per block — once
    /// the anchor is spent it can't be spent again.
    ///
    /// Failure modes returned as Err: anchor was spent by a tx that
    /// doesn't look like a hodlchain attestation (chain broken or
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
                        "L1 tx {txid} spends anchor but vout=0 is not a hodlchain \
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
