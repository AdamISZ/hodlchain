//! Node config file. Same shape as the sequencer's, minus the production-side
//! knobs.

use anyhow::{anyhow, Context, Result};
use hodl_core::config::{BitcoindAuth, BitcoindConfig, NetworkName};
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeConfig {
    pub network: NetworkName,
    pub bitcoind: BitcoindConfig,
    /// Where the sequencer's HTTP API lives. Used both for block-body
    /// retrieval and (incidentally) for `/head` cross-checks.
    pub sequencer_url: String,
    /// Must match the sequencer's configured value. Used to bootstrap
    /// the L1 cursor on first run.
    pub l1_genesis_height: u32,
    /// HTTP listen address.
    pub listen: SocketAddr,
    /// SQLite database file.
    pub db_path: PathBuf,
    /// Poll interval for L1 tip changes, in milliseconds. Default 1000.
    #[serde(default)]
    pub poll_ms: Option<u64>,
}

impl NodeConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let s = fs::read_to_string(path)
            .with_context(|| format!("read config {}", path.display()))?;
        Ok(serde_json::from_str(&s)?)
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

pub fn write_example(path: &Path) -> Result<()> {
    if path.exists() {
        return Err(anyhow!("{} already exists", path.display()));
    }
    let example = NodeConfig {
        network: NetworkName::Regtest,
        bitcoind: BitcoindConfig {
            url: "http://127.0.0.1:18443".into(),
            auth: BitcoindAuth::Cookie {
                path: PathBuf::from("/home/USER/.bitcoin/regtest/.cookie"),
            },
        },
        sequencer_url: "http://127.0.0.1:8080".into(),
        l1_genesis_height: 0,
        listen: "127.0.0.1:8081".parse().unwrap(),
        db_path: PathBuf::from("./hodl-node.db"),
        poll_ms: None,
    };
    fs::write(path, serde_json::to_vec_pretty(&example)?)?;
    Ok(())
}
