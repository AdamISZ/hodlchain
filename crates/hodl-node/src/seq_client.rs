//! HTTP client for the sequencer's block-body endpoint.

use anyhow::{anyhow, Context, Result};
use hodl_core::block::L2Block;
use hodl_core::rpc::HeadResponse;
use reqwest::Client;

pub struct SeqClient {
    http: Client,
    base: String,
}

impl SeqClient {
    pub fn new(base: String) -> Self {
        Self { http: Client::new(), base }
    }

    pub async fn get_block(&self, height: u32) -> Result<L2Block> {
        let url = format!("{}/block/{}", self.base.trim_end_matches('/'), height);
        let resp = self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(anyhow!("{url} returned HTTP {status}"));
        }
        Ok(resp.json::<L2Block>().await
            .with_context(|| format!("decode L2Block from {url}"))?)
    }

    #[allow(dead_code)]
    pub async fn get_head(&self) -> Result<HeadResponse> {
        let url = format!("{}/head", self.base.trim_end_matches('/'));
        Ok(self.http.get(&url).send().await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()?
            .json::<HeadResponse>().await?)
    }
}
