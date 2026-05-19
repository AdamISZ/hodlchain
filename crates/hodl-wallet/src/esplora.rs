//! Esplora-compatible HTTP client for the L1 side of the wallet.
//!
//! The wallet talks to L1 *exclusively* through this surface — no
//! bitcoind RPC, no wallet-scoped calls. In production this points at
//! a real Esplora deployment (mempool.space, BlockStream's electrs)
//! or the user's own electrs / mempool-space-self-host instance.
//! In the demo it points at `hodl-node`, which exposes a slim
//! Esplora-compatible subset on top of bitcoind.
//!
//! Endpoints we consume:
//!
//!   GET  /tx/{txid}                    — tx info (vin + vout + status)
//!   GET  /tx/{txid}/outspend/{vout}    — "is this outpoint spent? by whom?"
//!   GET  /address/{addr}/utxo          — current UTXOs at a P2TR address
//!   GET  /blocks/tip/height            — current L1 tip height
//!   POST /tx                           — broadcast a raw signed tx

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::{Address, OutPoint, ScriptBuf, Transaction, Txid};
use hodl_core::op_return::Attestation;
use reqwest::Client;
use serde::Deserialize;
use std::str::FromStr;

pub struct EsploraClient {
    http: Client,
    base: String,
}

#[derive(Debug, Deserialize)]
pub struct EsploraTx {
    #[allow(dead_code)]
    pub txid: String,
    pub vin: Vec<EsploraVin>,
    pub vout: Vec<EsploraVout>,
    #[serde(default)]
    pub status: TxStatus,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct TxStatus {
    /// L1 height at which the tx was mined; `None` for unconfirmed.
    /// We don't read `status.confirmed` separately — `block_height
    /// is_some()` is equivalent.
    #[serde(default)]
    pub block_height: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct EsploraVin {
    #[allow(dead_code)]
    pub txid: String,
    #[allow(dead_code)]
    pub vout: u32,
}

#[derive(Debug, Deserialize)]
pub struct EsploraVout {
    /// scriptPubKey, hex-encoded.
    pub scriptpubkey: String,
    /// Output value in satoshis.
    #[serde(default)]
    pub value: u64,
}

#[derive(Debug, Deserialize)]
pub struct Outspend {
    pub spent: bool,
    /// When `spent`, the spending tx's txid.
    #[serde(default)]
    pub txid: Option<String>,
    /// L1 block height of the spending tx.
    #[serde(default)]
    pub block_height: Option<u32>,
}

impl EsploraClient {
    pub fn new(base: String) -> Self {
        Self { http: Client::new(), base }
    }

    pub async fn get_tx(&self, txid: &Txid) -> Result<EsploraTx> {
        let url = format!("{}/tx/{}", self.base.trim_end_matches('/'), txid);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            bail!("{url} returned HTTP {}", resp.status());
        }
        Ok(resp.json::<EsploraTx>().await
            .with_context(|| format!("decode EsploraTx from {url}"))?)
    }

    pub async fn get_outspend(&self, txid: &Txid, vout: u32) -> Result<Outspend> {
        let url = format!(
            "{}/tx/{}/outspend/{}",
            self.base.trim_end_matches('/'),
            txid,
            vout
        );
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            bail!("{url} returned HTTP {}", resp.status());
        }
        Ok(resp.json::<Outspend>().await
            .with_context(|| format!("decode Outspend from {url}"))?)
    }

    /// Current L1 tip height. Used by the light client to compute
    /// confirmation counts for mint witness verification.
    pub async fn tip_height(&self) -> Result<u32> {
        let url = format!("{}/blocks/tip/height", self.base.trim_end_matches('/'));
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            bail!("{url} returned HTTP {}", resp.status());
        }
        let body = resp.text().await
            .with_context(|| format!("read {url} body"))?;
        body.trim().parse::<u32>()
            .with_context(|| format!("parse tip height from {url}: {body:?}"))
    }

    /// UTXOs currently held at `addr`. Used by the wallet to discover
    /// the funding tx for a mint UTXO that the user paid into from
    /// their external wallet.
    pub async fn address_utxos(&self, addr: &Address) -> Result<Vec<AddressUtxo>> {
        let url = format!(
            "{}/address/{}/utxo",
            self.base.trim_end_matches('/'),
            addr
        );
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        if !resp.status().is_success() {
            bail!("{url} returned HTTP {}", resp.status());
        }
        Ok(resp.json::<Vec<AddressUtxo>>().await
            .with_context(|| format!("decode AddressUtxo list from {url}"))?)
    }

    /// Broadcast a signed transaction. Esplora's POST /tx takes a
    /// hex-encoded raw tx as the request body and returns the txid
    /// as plain text on success.
    pub async fn broadcast(&self, tx: &Transaction) -> Result<Txid> {
        let url = format!("{}/tx", self.base.trim_end_matches('/'));
        let body = serialize_hex(tx);
        let resp = self.http.post(&url).body(body).send().await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let text = resp.text().await
            .with_context(|| format!("read {url} body"))?;
        if !status.is_success() {
            bail!("{url} returned HTTP {status}: {}", text.trim());
        }
        Txid::from_str(text.trim())
            .with_context(|| format!("parse txid from {url}: {text:?}"))
    }
}

/// One UTXO returned by Esplora's `/address/{addr}/utxo` endpoint.
#[derive(Clone, Debug, Deserialize)]
pub struct AddressUtxo {
    pub txid: Txid,
    pub vout: u32,
    pub value: u64,
    #[serde(default)]
    pub status: TxStatus,
}

/// One step of the attestation chain.
#[allow(dead_code)]
pub struct ChainStep {
    pub attestation: Attestation,
    /// L1 height at which the attestation tx was mined.
    pub l1_height: u32,
    /// Outpoint that becomes the next link's input.
    pub new_anchor: OutPoint,
}

/// Walk the attestation chain forward from `anchor_0` until we hit an
/// unspent anchor (the chain tip). Returns one `ChainStep` per L2 block
/// past genesis. Empty vector if no attestations have been posted yet.
///
/// Each step makes two HTTP calls (outspend + tx), so chain depth N
/// costs 2N requests. For a phone with intermittent connectivity that
/// catches up after being offline: still cheap, scaling linearly in
/// blocks-missed.
pub async fn walk_attestation_chain(
    esplora: &EsploraClient,
    anchor_0: OutPoint,
) -> Result<Vec<ChainStep>> {
    let mut steps = Vec::new();
    let mut current = anchor_0;
    loop {
        let outspend = esplora.get_outspend(&current.txid, current.vout).await?;
        if !outspend.spent {
            return Ok(steps);
        }
        let spender_txid_str = outspend
            .txid
            .as_ref()
            .ok_or_else(|| anyhow!("outspend says spent but has no txid: {:?}", outspend))?;
        let spender_txid = Txid::from_str(spender_txid_str)
            .with_context(|| format!("parse spender txid {spender_txid_str}"))?;
        let l1_height = outspend
            .block_height
            .ok_or_else(|| anyhow!("outspend says spent but has no block_height"))?;

        let tx = esplora.get_tx(&spender_txid).await?;
        if tx.vout.len() < 2 {
            bail!(
                "spending tx {spender_txid} has only {} outputs; expected ≥2 \
                 (OP_RETURN @ vout=0, anchor @ vout=1)",
                tx.vout.len()
            );
        }
        let spk_bytes = hex::decode(&tx.vout[0].scriptpubkey)
            .with_context(|| format!("decode scriptpubkey hex for {spender_txid}"))?;
        let spk = ScriptBuf::from_bytes(spk_bytes);
        let attestation = Attestation::try_from_script(&spk)
            .with_context(|| format!("parse OP_RETURN of {spender_txid}"))?
            .ok_or_else(|| {
                anyhow!(
                    "vout=0 of {spender_txid} is not a hodlcoin attestation \
                     (wrong magic / length) — chain corrupt at L1 height {l1_height}"
                )
            })?;

        let new_anchor = OutPoint { txid: spender_txid, vout: 1 };
        steps.push(ChainStep { attestation, l1_height, new_anchor });
        current = new_anchor;
    }
}
