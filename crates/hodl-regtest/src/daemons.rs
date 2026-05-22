//! Find + spawn hodl-sequencer and hodl-node.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::paths::{Layout, BTC_RPC_PORT, NODE_PORT, SEQUENCER_PORT};
use crate::procs;

/// Locate a sibling binary. Lookup order:
///   1. Same directory as the running `hodl-regtest` executable
///      (release install).
///   2. `$PATH`.
///   3. `target/release` or `target/debug` *relative to the running
///      executable* — convenient for `cargo run -p hodl-regtest`.
pub fn find_binary(name: &str) -> Result<PathBuf> {
    let self_path = std::env::current_exe().ok();
    if let Some(p) = &self_path {
        if let Some(dir) = p.parent() {
            let cand = dir.join(name);
            if cand.is_file() {
                return Ok(cand);
            }
            // dev convenience: under `target/{release,debug}`, the
            // sibling binaries land in the same dir, so this case is
            // already covered above. But also handle the cargo-install
            // / system layout where the user's $PATH has it.
        }
    }
    if let Some(p) = which(name) {
        return Ok(p);
    }
    bail!(
        "couldn't locate {name}. Expected it alongside the hodl-regtest \
         binary or on $PATH. If running from a source checkout, do \
         `cargo build` first."
    )
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

pub fn write_seq_config(layout: &Layout, l1_genesis: u32) -> Result<()> {
    let cookie = layout.bitcoin_cookie();
    let cfg = json!({
        "network": "regtest",
        "bitcoind": {
            "url": format!("http://127.0.0.1:{}/wallet/sequencer", BTC_RPC_PORT),
            "auth": { "kind": "cookie", "path": cookie.to_string_lossy() },
        },
        "l1_genesis_height": l1_genesis,
        "listen": format!("127.0.0.1:{}", SEQUENCER_PORT),
        "db_path": layout.seq_db().to_string_lossy(),
        "poll_ms": 500,
    });
    std::fs::write(layout.seq_config(), serde_json::to_string_pretty(&cfg)?)
        .context("write sequencer config")
}

pub fn write_node_config(layout: &Layout, l1_genesis: u32) -> Result<()> {
    let cookie = layout.bitcoin_cookie();
    let cfg = json!({
        "network": "regtest",
        "bitcoind": {
            "url": format!("http://127.0.0.1:{}", BTC_RPC_PORT),
            "auth": { "kind": "cookie", "path": cookie.to_string_lossy() },
        },
        "sequencer_url": format!("http://127.0.0.1:{}", SEQUENCER_PORT),
        "l1_genesis_height": l1_genesis,
        "listen": format!("127.0.0.1:{}", NODE_PORT),
        "db_path": layout.node_db().to_string_lossy(),
        "poll_ms": 500,
    });
    std::fs::write(layout.node_config(), serde_json::to_string_pretty(&cfg)?)
        .context("write node config")
}

pub fn spawn_seq(layout: &Layout, seq_bin: &Path) -> Result<u32> {
    spawn(
        seq_bin,
        &["run", "--config"],
        &layout.seq_config(),
        &layout.seq_log(),
        &layout.seq_pid(),
    )
}

pub fn spawn_node(layout: &Layout, node_bin: &Path) -> Result<u32> {
    spawn(
        node_bin,
        &["run", "--config"],
        &layout.node_config(),
        &layout.node_log(),
        &layout.node_pid(),
    )
}

fn spawn(
    bin: &Path,
    prefix_args: &[&str],
    config_path: &Path,
    log_path: &Path,
    pid_path: &Path,
) -> Result<u32> {
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("open log {}", log_path.display()))?;
    let log_err = log.try_clone()?;
    let child = Command::new(bin)
        .args(prefix_args)
        .arg(config_path)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        .spawn()
        .with_context(|| format!("spawn {}", bin.display()))?;
    let pid = child.id();
    procs::write_pid(pid_path, pid)?;
    // We deliberately leak the Child by forgetting it; the spawned
    // process detaches from this short-lived CLI and the OS will
    // reap it when stopped.
    std::mem::forget(child);
    Ok(pid)
}

/// Block until `http://127.0.0.1:<port>/head` responds 2xx.
pub fn wait_for_head(port: u16, timeout: Duration) -> Result<()> {
    let url = format!("http://127.0.0.1:{port}/head");
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()?;
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Ok(r) = client.get(&url).send() {
            if r.status().is_success() {
                return Ok(());
            }
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    Err(anyhow!("{url} did not become ready within {:?}", timeout))
}
