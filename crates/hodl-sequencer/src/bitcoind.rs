//! Bitcoind wrapper for the sequencer. Implements `hodl_core::proof::L1View`
//! and exposes `post_attestation`.

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::{OutPoint, ScriptBuf, Txid};
use bitcoincore_rpc::{Client, RpcApi};
use hodl_core::op_return::Attestation;
use hodl_core::proof::{L1Output, L1View};
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Mutex;

use crate::config::SequencerConfig;

pub struct SequencerL1 {
    client: Mutex<Client>,
}

impl SequencerL1 {
    pub fn connect(cfg: &SequencerConfig) -> Result<Self> {
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

    pub fn block_hash_hex(&self, height: u32) -> Result<String> {
        let c = self.client.lock().unwrap();
        let h = c.get_block_hash(height as u64)?;
        Ok(h.to_string())
    }

    /// Pick the wallet's largest unspent output to use as anchor_0 — the
    /// root of the attestation chain. Embedded into the L2 genesis header.
    pub fn pick_initial_anchor(&self) -> Result<OutPoint> {
        let c = self.client.lock().unwrap();
        // listunspent(minconf, maxconf, addresses, include_unsafe, query_options)
        let utxos: Vec<Value> = c.call(
            "listunspent",
            &[json!(1), json!(9_999_999), json!([]), json!(false)],
        )?;
        if utxos.is_empty() {
            bail!(
                "sequencer wallet has no confirmed UTXOs; fund the wallet \
                 before starting the sequencer"
            );
        }
        let best = utxos
            .iter()
            .max_by_key(|u| {
                u["amount"]
                    .as_f64()
                    .map(|a| (a * 100_000_000.0) as u64)
                    .unwrap_or(0)
            })
            .expect("non-empty");
        let txid = best["txid"]
            .as_str()
            .ok_or_else(|| anyhow!("listunspent entry missing txid: {best}"))?;
        let vout = best["vout"]
            .as_u64()
            .ok_or_else(|| anyhow!("listunspent entry missing vout: {best}"))?
            as u32;
        Ok(OutPoint { txid: Txid::from_str(txid)?, vout })
    }

    /// Broadcast an OP_RETURN attestation tx as the next link in the
    /// chain. The tx has exactly one input (the current anchor), exactly
    /// two outputs: vout=0 is the OP_RETURN attestation, vout=1 is the
    /// change back to the wallet — the new anchor.
    ///
    /// Uses bitcoind's `send` with `options.inputs` and `change_position=1`
    /// so the change always lands at vout=1 (which nodes assume when
    /// following the chain forward).
    pub fn post_attestation_chained(
        &self,
        att: &Attestation,
        anchor_in: OutPoint,
    ) -> Result<(Txid, OutPoint)> {
        let payload_hex = hex::encode(att.encode());
        let outputs = json!([{ "data": payload_hex }]);
        let options = json!({
            "inputs": [{ "txid": anchor_in.txid.to_string(), "vout": anchor_in.vout }],
            "add_inputs": false,
            "change_position": 1,
        });
        let c = self.client.lock().unwrap();
        // send(outputs, conf_target, estimate_mode, fee_rate, options) — positional.
        let result: Value = c.call(
            "send",
            &[outputs, json!(null), json!("unset"), json!(null), options],
        )?;
        if !result.get("complete").and_then(|v| v.as_bool()).unwrap_or(false) {
            bail!("send not complete: {result}");
        }
        let txid_str = result
            .get("txid")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("send response has no txid: {result}"))?;
        let txid: Txid = Txid::from_str(txid_str).context("parse send txid")?;
        Ok((txid, OutPoint { txid, vout: 1 }))
    }
}

impl L1View for SequencerL1 {
    fn get_output(&self, op: &OutPoint) -> Option<L1Output> {
        let c = self.client.lock().ok()?;
        let result = c.get_tx_out(&op.txid, op.vout, Some(false)).ok()??;
        let tip = c.get_block_count().ok()? as u32;
        // gettxout's `confirmations` is >= 1 once mined (since we passed
        // include_mempool=false).
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
