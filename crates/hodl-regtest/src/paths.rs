//! On-disk layout for the regtest backend.
//!
//! Everything lives under one root that survives reboots, so a
//! tester can `hodl-regtest start`, leave it overnight, and pick up
//! the same chain state next morning. Use `reset` to wipe.

use anyhow::{Context, Result};

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
use anyhow::anyhow;
use std::path::PathBuf;

/// Bitcoin RPC port. Chosen to not collide with the Bitcoin Core
/// default regtest RPC port 18443 (which a user may already have
/// running for another project).
pub const BTC_RPC_PORT: u16 = 28443;
pub const BTC_P2P_PORT: u16 = 28444;
pub const SEQUENCER_PORT: u16 = 28080;
pub const NODE_PORT: u16 = 28081;

#[derive(Clone, Debug)]
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    /// Resolve the on-disk root. Honours `HODL_REGTEST_DATA` for tests.
    pub fn resolve() -> Result<Self> {
        if let Ok(p) = std::env::var("HODL_REGTEST_DATA") {
            return Ok(Self { root: PathBuf::from(p) });
        }
        let root = default_root()?;
        Ok(Self { root })
    }

    pub fn ensure_exists(&self) -> Result<()> {
        for p in [&self.bitcoin_dir(), &self.seq_dir(), &self.node_dir()] {
            std::fs::create_dir_all(p)
                .with_context(|| format!("create {}", p.display()))?;
        }
        Ok(())
    }

    pub fn exists(&self) -> bool {
        self.root.exists()
    }

    pub fn bitcoin_dir(&self) -> PathBuf { self.root.join("bitcoin") }
    pub fn seq_dir(&self) -> PathBuf { self.root.join("seq") }
    pub fn node_dir(&self) -> PathBuf { self.root.join("node") }

    pub fn bitcoin_conf(&self) -> PathBuf { self.bitcoin_dir().join("bitcoin.conf") }
    pub fn bitcoin_cookie(&self) -> PathBuf {
        self.bitcoin_dir().join("regtest").join(".cookie")
    }
    pub fn bitcoin_pid(&self) -> PathBuf {
        self.bitcoin_dir().join("regtest").join("bitcoind.pid")
    }

    pub fn seq_config(&self) -> PathBuf { self.seq_dir().join("config.json") }
    pub fn seq_log(&self) -> PathBuf { self.seq_dir().join("log") }
    pub fn seq_pid(&self) -> PathBuf { self.seq_dir().join("pid") }
    pub fn seq_db(&self) -> PathBuf { self.seq_dir().join("hodl-sequencer.db") }

    pub fn node_config(&self) -> PathBuf { self.node_dir().join("config.json") }
    pub fn node_log(&self) -> PathBuf { self.node_dir().join("log") }
    pub fn node_pid(&self) -> PathBuf { self.node_dir().join("pid") }
    pub fn node_db(&self) -> PathBuf { self.node_dir().join("hodl-node.db") }

    /// L1 genesis height — the L1 block the L2 anchors at on first
    /// boot. Cached in a file so `status` can show it without going
    /// to bitcoind.
    pub fn l1_genesis_file(&self) -> PathBuf { self.root.join("l1_genesis") }

    /// The bitcoind wallet address we mine all regtest blocks to.
    /// Cached so `mine` doesn't need to look up a fresh address.
    pub fn mining_address_file(&self) -> PathBuf { self.root.join("mining_address") }
}

fn default_root() -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(xdg) = std::env::var("XDG_DATA_HOME") {
            if !xdg.is_empty() {
                return Ok(PathBuf::from(xdg).join("hodlchain").join("regtest"));
            }
        }
        let home = std::env::var("HOME").context("HOME unset")?;
        Ok(PathBuf::from(home)
            .join(".local").join("share").join("hodlchain").join("regtest"))
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").context("HOME unset")?;
        Ok(PathBuf::from(home)
            .join("Library").join("Application Support")
            .join("hodlchain").join("regtest"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(anyhow!("unsupported OS — only Linux and macOS are supported today"))
    }
}
