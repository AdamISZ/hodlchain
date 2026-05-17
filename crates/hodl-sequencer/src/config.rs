//! Sequencer config file.

use anyhow::{anyhow, Context, Result};
use bitcoin::Network;
use hodl_core::config::{BitcoindAuth, BitcoindConfig, NetworkName};
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SequencerConfig {
    pub network: NetworkName,
    pub bitcoind: BitcoindConfig,
    /// L1 height at which to anchor the L2 genesis block. The sequencer will
    /// start producing L2 blocks from L1 height >= this value.
    pub l1_genesis_height: u32,
    /// HTTP listen address.
    pub listen: SocketAddr,
    /// SQLite database file.
    pub db_path: PathBuf,
    /// Poll interval for L1 tip changes, in milliseconds. Default 1000.
    #[serde(default)]
    pub poll_ms: Option<u64>,
}

impl SequencerConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let s = fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        Ok(serde_json::from_str(&s)?)
    }

    #[allow(dead_code)]
    pub fn network(&self) -> Network {
        self.network.into_bitcoin()
    }

    pub fn poll_interval(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.poll_ms.unwrap_or(1000))
    }

    pub fn bitcoincore_auth(&self) -> bitcoincore_rpc::Auth {
        match &self.bitcoind.auth {
            BitcoindAuth::Cookie { path } => bitcoincore_rpc::Auth::CookieFile(path.clone()),
            BitcoindAuth::UserPass { user, password } => {
                bitcoincore_rpc::Auth::UserPass(user.clone(), password.clone())
            }
        }
    }
}

/// Convenience: write an example config to disk.
pub fn write_example(path: &Path) -> Result<()> {
    if path.exists() {
        return Err(anyhow!("{} already exists", path.display()));
    }
    let example = SequencerConfig {
        network: NetworkName::Regtest,
        bitcoind: BitcoindConfig {
            url: "http://127.0.0.1:18443/wallet/sequencer".into(),
            auth: BitcoindAuth::Cookie {
                path: PathBuf::from("/home/USER/.bitcoin/regtest/.cookie"),
            },
        },
        l1_genesis_height: 0,
        listen: "127.0.0.1:8080".parse().unwrap(),
        db_path: PathBuf::from("./hodl-sequencer.db"),
        poll_ms: None,
    };
    fs::write(path, serde_json::to_vec_pretty(&example)?)?;
    Ok(())
}
