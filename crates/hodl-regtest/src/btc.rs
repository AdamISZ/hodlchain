//! Bitcoin Core discovery, lifecycle, and a minimal JSON-RPC client.
//!
//! Everything here is *blocking*; the tool is short-lived, no async
//! needed.

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::paths::{Layout, BTC_P2P_PORT, BTC_RPC_PORT};

/// Resolve `bitcoind` + `bitcoin-cli`. Honours `BITCOIND_BIN` and
/// `BITCOIN_CLI_BIN` overrides and `BITCOIND_PREFIX` (a directory
/// containing both binaries).
pub fn discover() -> Result<(PathBuf, PathBuf)> {
    let prefix = std::env::var("BITCOIND_PREFIX").ok();
    let bitcoind = find_one("bitcoind", "BITCOIND_BIN", prefix.as_deref())?;
    let bitcoin_cli = find_one("bitcoin-cli", "BITCOIN_CLI_BIN", prefix.as_deref())?;
    Ok((bitcoind, bitcoin_cli))
}

fn find_one(name: &str, env: &str, prefix: Option<&str>) -> Result<PathBuf> {
    if let Ok(p) = std::env::var(env) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        bail!("{} points to {} which is not a file", env, p.display());
    }
    if let Some(found) = which(name) {
        return Ok(found);
    }
    if let Some(prefix) = prefix {
        let p = PathBuf::from(prefix).join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    bail!(
        "{name} not found on $PATH. Install Bitcoin Core v22+:\n\
           Ubuntu/Debian: see https://bitcoincore.org/en/download/\n\
           macOS:         brew install bitcoin\n\
         Or set BITCOIND_BIN / BITCOIN_CLI_BIN / BITCOIND_PREFIX."
    );
}

fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let cand = dir.join(name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

pub fn write_bitcoin_conf(layout: &Layout) -> Result<()> {
    // Minimal conf — RPC on a non-default port so we don't collide
    // with another regtest bitcoind a developer may already run.
    // txindex=1 is required so the node's Esplora-compatible
    // `/tx/:txid` endpoint can resolve any txid without a wallet
    // hint (light wallets rely on this).
    let conf = format!(
        "fallbackfee=0.00001\n\
         txindex=1\n\
         [regtest]\n\
         rpcport={rpc}\n\
         port={p2p}\n",
        rpc = BTC_RPC_PORT,
        p2p = BTC_P2P_PORT,
    );
    std::fs::write(layout.bitcoin_conf(), conf)
        .context("write bitcoin.conf")
}

pub fn start_bitcoind(layout: &Layout, bitcoind: &Path) -> Result<()> {
    let datadir = layout.bitcoin_dir();
    // `-daemon` makes bitcoind double-fork and write its own pid
    // file (path defined in bitcoin.conf, defaults to
    // <datadir>/regtest/bitcoind.pid for regtest). We read that
    // back below.
    let status = Command::new(bitcoind)
        .arg(format!("-datadir={}", datadir.display()))
        .arg("-regtest")
        .arg("-daemon")
        .arg("-listen=0") // no p2p listen needed on regtest
        .arg("-rpcbind=127.0.0.1")
        .arg("-rpcallowip=127.0.0.1")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .context("spawn bitcoind")?;
    if !status.success() {
        bail!("bitcoind exited with status {status}");
    }
    Ok(())
}

/// Block (up to `timeout`) until bitcoind RPC responds. Returns the
/// PID it reports (read from `<datadir>/regtest/bitcoind.pid`).
pub fn wait_for_rpc(layout: &Layout, bitcoin_cli: &Path, timeout: Duration) -> Result<u32> {
    let cookie = layout.bitcoin_cookie();
    let datadir = layout.bitcoin_dir();
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if cookie.exists() {
            let out = Command::new(bitcoin_cli)
                .arg(format!("-datadir={}", datadir.display()))
                .arg("-regtest")
                .arg("getblockcount")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if let Ok(s) = out {
                if s.success() {
                    // RPC ready — read the pid file bitcoind writes.
                    let pid_path = layout.bitcoin_pid();
                    if let Ok(s) = std::fs::read_to_string(&pid_path) {
                        if let Ok(pid) = s.trim().parse::<u32>() {
                            return Ok(pid);
                        }
                    }
                    return Err(anyhow!(
                        "bitcoind RPC ready but pid file {} missing/unreadable",
                        pid_path.display()
                    ));
                }
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!(
        "bitcoind did not become RPC-ready within {:?}; check {}",
        timeout,
        layout.bitcoin_dir().join("regtest").join("debug.log").display()
    )
}

// ---------- JSON-RPC client ----------

#[derive(Clone)]
pub struct Rpc {
    client: reqwest::blocking::Client,
    url: String,
    user: String,
    pass: String,
}

#[derive(Deserialize)]
struct RpcReply {
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Deserialize, Debug)]
struct RpcError {
    code: i32,
    message: String,
}

impl Rpc {
    pub fn from_cookie(cookie_path: &Path, wallet: Option<&str>) -> Result<Self> {
        let cookie = std::fs::read_to_string(cookie_path)
            .with_context(|| format!("read cookie {}", cookie_path.display()))?;
        let (user, pass) = cookie
            .trim()
            .split_once(':')
            .ok_or_else(|| anyhow!("malformed cookie: {cookie:?}"))?;
        let url = match wallet {
            Some(w) => format!("http://127.0.0.1:{}/wallet/{}", BTC_RPC_PORT, w),
            None => format!("http://127.0.0.1:{}", BTC_RPC_PORT),
        };
        Ok(Self {
            client: reqwest::blocking::Client::new(),
            url,
            user: user.to_string(),
            pass: pass.to_string(),
        })
    }

    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        let body = json!({
            "jsonrpc": "1.0",
            "id": "hodl-regtest",
            "method": method,
            "params": params,
        });
        let resp = self
            .client
            .post(&self.url)
            .basic_auth(&self.user, Some(&self.pass))
            .json(&body)
            .send()
            .with_context(|| format!("POST {} ({method})", self.url))?;
        let status = resp.status();
        let text = resp.text().unwrap_or_default();
        if !status.is_success() {
            bail!("bitcoind RPC {method} {status}: {text}");
        }
        let reply: RpcReply = serde_json::from_str(&text)
            .with_context(|| format!("decode RPC reply for {method}: {text}"))?;
        if let Some(e) = reply.error {
            bail!("bitcoind RPC {method} error {}: {}", e.code, e.message);
        }
        reply.result.ok_or_else(|| anyhow!("RPC reply missing result"))
    }
}
