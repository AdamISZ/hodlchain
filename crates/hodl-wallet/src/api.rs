//! Async HTTP clients for the sequencer (mint/transfer submission) and
//! optionally the node (balance lookup).

use anyhow::{anyhow, Context, Result};
use hodl_core::block::L2Block;
use hodl_core::proof::MintProofEnvelope;
use hodl_core::rpc::{
    BalanceResponse, HeadResponse, SubmitMintRequest, SubmitMintResponse,
    SubmitTransferRequest, SubmitTransferResponse,
};
use hodl_core::tx::{L2Address, SignedTransfer};
use hodl_core::witness::BlockWitness;
use reqwest::Client;

pub struct ApiClient {
    http: Client,
    pub sequencer_url: String,
    pub node_url: Option<String>,
}

impl ApiClient {
    pub fn new(sequencer_url: String, node_url: Option<String>) -> Self {
        Self { http: Client::new(), sequencer_url, node_url }
    }

    pub async fn submit_mint(
        &self,
        proof: MintProofEnvelope,
        l2_destination: L2Address,
    ) -> Result<SubmitMintResponse> {
        let url = format!("{}/mint", self.sequencer_url.trim_end_matches('/'));
        let req = SubmitMintRequest { proof, l2_destination };
        let resp = self.http.post(&url).json(&req).send().await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let body: SubmitMintResponse = resp.json().await
            .with_context(|| format!("decode response from {url} (status={status})"))?;
        Ok(body)
    }

    pub async fn submit_transfer(
        &self,
        transfer: SignedTransfer,
    ) -> Result<SubmitTransferResponse> {
        let url = format!("{}/transfer", self.sequencer_url.trim_end_matches('/'));
        let req = SubmitTransferRequest { transfer };
        let resp = self.http.post(&url).json(&req).send().await
            .with_context(|| format!("POST {url}"))?;
        let status = resp.status();
        let body: SubmitTransferResponse = resp.json().await
            .with_context(|| format!("decode response from {url} (status={status})"))?;
        Ok(body)
    }

    /// Fetch an L2 block by height. Prefers the node (more trust-aligned
    /// for light clients); falls back to the sequencer if no node is
    /// configured.
    pub async fn get_block(&self, height: u32) -> Result<L2Block> {
        let base = self.node_url.as_deref().unwrap_or(&self.sequencer_url);
        let url = format!("{}/block/{}", base.trim_end_matches('/'), height);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("{url} returned HTTP {status}"));
        }
        Ok(resp.json::<L2Block>().await
            .with_context(|| format!("decode L2Block from {url}"))?)
    }

    /// Sequencer head — used for both `head` queries and as a fallback for
    /// nonce-bootstrap if no node is configured.
    pub async fn sequencer_head(&self) -> Result<HeadResponse> {
        let url = format!("{}/head", self.sequencer_url.trim_end_matches('/'));
        Ok(self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()?
            .json::<HeadResponse>().await?)
    }

    /// Balance lookup. Tries node first if configured; otherwise hits the
    /// sequencer's `/balance/:addr` endpoint.
    pub async fn balance(&self, addr: &L2Address) -> Result<BalanceResponse> {
        let base = self.node_url.as_deref().unwrap_or(&self.sequencer_url);
        let url = format!(
            "{}/balance/{}",
            base.trim_end_matches('/'),
            hex::encode(addr.serialize())
        );
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("{url} returned HTTP {status}"));
        }
        Ok(resp.json().await?)
    }

    /// Like `balance()` but ALWAYS goes to the sequencer, regardless
    /// of whether a node URL is configured. Used by transfer-building
    /// code that needs a mempool-aware nonce: only the sequencer
    /// reports `effective_nonce = state.nonce + in_flight_count`.
    /// The node has no mempool of its own.
    pub async fn balance_via_sequencer(&self, addr: &L2Address) -> Result<BalanceResponse> {
        let url = format!(
            "{}/balance/{}",
            self.sequencer_url.trim_end_matches('/'),
            hex::encode(addr.serialize())
        );
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("{url} returned HTTP {status}"));
        }
        Ok(resp.json().await?)
    }

    /// Block-witness lookup. Prefers the node.
    pub async fn get_witness(&self, height: u32) -> Result<BlockWitness> {
        let base = self.node_url.as_deref().unwrap_or(&self.sequencer_url);
        let url = format!("{}/witness/{}", base.trim_end_matches('/'), height);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("{url} returned HTTP {status}"));
        }
        Ok(resp.json().await
            .with_context(|| format!("decode BlockWitness from {url}"))?)
    }

    /// Cumulative consumed-nullifier set at the server's head. Used
    /// by the wallet's cold-start bootstrap. Node-only.
    pub async fn get_nullifiers(&self) -> Result<Vec<String>> {
        let base = self.node_url.as_deref().unwrap_or(&self.sequencer_url);
        let url = format!("{}/nullifiers", base.trim_end_matches('/'));
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("{url} returned HTTP {status}"));
        }
        Ok(resp.json().await?)
    }
}
