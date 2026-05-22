//! hodl-regtest — orchestrator for a local regtest backend.
//!
//! Wraps bitcoind (regtest) + hodl-sequencer + hodl-node into a single
//! click-to-run tool for testers who don't want to clone the repo
//! and learn the shell script. The chain is persistent across
//! reboots; use `reset` to wipe and start over.

mod btc;
mod daemons;
mod paths;
mod procs;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use paths::{Layout, BTC_RPC_PORT, NODE_PORT, SEQUENCER_PORT};
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Parser)]
#[command(
    name = "hodl-regtest",
    version,
    about = "Click-to-run local regtest backend for the hodlchain POC",
    long_about = "Starts bitcoind (regtest) plus hodl-sequencer and hodl-node. \
                  Datadir is persistent — stop and start at will; use `reset` \
                  to wipe."
)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Bring the backend up. First start initialises a fresh
    /// regtest chain and mines 102 blocks to the local user wallet
    /// so there are spendable funds; subsequent starts resume from
    /// the persisted datadir.
    Start,
    /// Stop bitcoind + sequencer + node. State is preserved.
    Stop,
    /// Report PIDs, ports, L2 head, L1 height.
    Status,
    /// Mine N L1 blocks to the persistent user wallet (default 1).
    Mine {
        #[arg(default_value_t = 1)]
        n: u32,
    },
    /// Send BTC from the persistent user wallet to a destination
    /// address. Useful for topping up the hodl-wallet's deposit
    /// addresses. Amount is required — no silent default for
    /// money-moving commands.
    Fund {
        addr: String,
        btc: f64,
    },
    /// Stop everything and delete the datadir. Confirms unless
    /// `--yes` is passed.
    Reset {
        #[arg(long)]
        yes: bool,
    },
    /// Print the sequencer + node log file paths and the last 40
    /// lines of each.
    Logs,
}

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let layout = Layout::resolve()?;
    match cli.command {
        Cmd::Start => cmd_start(&layout),
        Cmd::Stop => cmd_stop(&layout),
        Cmd::Status => cmd_status(&layout),
        Cmd::Mine { n } => cmd_mine(&layout, n),
        Cmd::Fund { addr, btc } => cmd_fund(&layout, &addr, btc),
        Cmd::Reset { yes } => cmd_reset(&layout, yes),
        Cmd::Logs => cmd_logs(&layout),
    }
}

// ---------- start ----------

fn cmd_start(layout: &Layout) -> Result<()> {
    let (bitcoind, bitcoin_cli) = btc::discover()?;
    println!("[ok]  bitcoind:    {}", bitcoind.display());
    println!("[ok]  bitcoin-cli: {}", bitcoin_cli.display());

    let seq_bin = daemons::find_binary("hodl-sequencer")?;
    let node_bin = daemons::find_binary("hodl-node")?;
    println!("[ok]  hodl-sequencer: {}", seq_bin.display());
    println!("[ok]  hodl-node:      {}", node_bin.display());

    let fresh = !layout.exists();
    layout.ensure_exists()?;
    println!("[ok]  datadir: {}", layout.root.display());

    // Guard against double-start of any of the three.
    if pid_running(&layout.bitcoin_pid())?.is_some() {
        bail!("bitcoind already running — `hodl-regtest stop` first");
    }
    if pid_running(&layout.seq_pid())?.is_some() {
        bail!("hodl-sequencer already running — `hodl-regtest stop` first");
    }
    if pid_running(&layout.node_pid())?.is_some() {
        bail!("hodl-node already running — `hodl-regtest stop` first");
    }

    // Preflight: catch a leftover daemon (from a previous run or
    // another tool) bound to one of our ports. Otherwise bitcoind
    // silently fails to bind RPC and we hit a confusing "didn't
    // become RPC-ready" 30s later.
    for (port, name) in [
        (BTC_RPC_PORT, "bitcoind RPC"),
        (SEQUENCER_PORT, "hodl-sequencer HTTP"),
        (NODE_PORT, "hodl-node HTTP"),
    ] {
        check_port_free(port, name)?;
    }

    if fresh {
        btc::write_bitcoin_conf(layout)?;
    }

    println!("[..]  starting bitcoind (regtest)…");
    btc::start_bitcoind(layout, &bitcoind)?;
    let btc_pid = btc::wait_for_rpc(layout, &bitcoin_cli, Duration::from_secs(30))?;
    println!("[ok]  bitcoind RPC ready (pid {btc_pid}) on 127.0.0.1:{BTC_RPC_PORT}");

    // RPC client (wallet-less; we attach wallet names when needed).
    let rpc = btc::Rpc::from_cookie(&layout.bitcoin_cookie(), None)?;

    if fresh {
        first_time_bitcoin_setup(layout, &rpc)?;
    } else {
        // Re-load wallets — bitcoind unloads them on shutdown.
        load_wallet_if_exists(&rpc, "user")?;
        load_wallet_if_exists(&rpc, "sequencer")?;
    }

    let l1_genesis = read_l1_genesis(layout)?;
    println!("[ok]  L2 anchored at L1 height {l1_genesis}");

    if fresh {
        daemons::write_seq_config(layout, l1_genesis)?;
        daemons::write_node_config(layout, l1_genesis)?;
    }

    println!("[..]  starting hodl-sequencer…");
    let seq_pid = daemons::spawn_seq(layout, &seq_bin)?;
    daemons::wait_for_head(SEQUENCER_PORT, Duration::from_secs(30))
        .with_context(|| format!("sequencer log: {}", layout.seq_log().display()))?;
    println!("[ok]  hodl-sequencer up (pid {seq_pid}) on 127.0.0.1:{SEQUENCER_PORT}");

    println!("[..]  starting hodl-node…");
    let node_pid = daemons::spawn_node(layout, &node_bin)?;
    daemons::wait_for_head(NODE_PORT, Duration::from_secs(30))
        .with_context(|| format!("node log: {}", layout.node_log().display()))?;
    println!("[ok]  hodl-node up (pid {node_pid}) on 127.0.0.1:{NODE_PORT}");

    println!();
    println!("backend ready. Point the desktop wallet at:");
    println!("  sequencer URL: http://127.0.0.1:{SEQUENCER_PORT}");
    println!("  node URL:      http://127.0.0.1:{NODE_PORT}");
    println!("  esplora URL:   http://127.0.0.1:{NODE_PORT}");
    println!();
    println!("useful commands:");
    println!("  hodl-regtest mine 1        # advance L1 by one block");
    println!("  hodl-regtest fund <addr>   # send 0.1 BTC to a deposit address");
    println!("  hodl-regtest status        # health check");
    println!("  hodl-regtest stop          # graceful shutdown");
    Ok(())
}

fn first_time_bitcoin_setup(layout: &Layout, rpc: &btc::Rpc) -> Result<()> {
    println!("[..]  creating bitcoind wallets (user, sequencer)…");
    rpc.call("createwallet", json!(["user"]))?;
    rpc.call("createwallet", json!(["sequencer"]))?;

    let user = btc::Rpc::from_cookie(&layout.bitcoin_cookie(), Some("user"))?;
    let seq = btc::Rpc::from_cookie(&layout.bitcoin_cookie(), Some("sequencer"))?;

    let mining_addr = user.call("getnewaddress", json!(["mining", "bech32m"]))?;
    let mining_addr = mining_addr.as_str()
        .ok_or_else(|| anyhow!("getnewaddress: expected string, got {mining_addr}"))?;
    std::fs::write(layout.mining_address_file(), mining_addr)
        .context("persist mining address")?;
    println!("[..]  mining 101 blocks to user wallet (regtest maturity)…");
    rpc.call("generatetoaddress", json!([101, mining_addr]))?;

    // Fund the sequencer wallet (it needs sats for OP_RETURN fees).
    let seq_addr = seq.call("getnewaddress", json!(["fees", "bech32m"]))?;
    let seq_addr = seq_addr.as_str().ok_or_else(|| anyhow!("seq addr type"))?;
    user.call("sendtoaddress", json!([seq_addr, 1.0]))?;
    rpc.call("generatetoaddress", json!([1, mining_addr]))?;
    println!("[ok]  funded sequencer with 1 BTC for attestation fees");

    let height: u64 = rpc
        .call("getblockcount", json!([]))?
        .as_u64()
        .ok_or_else(|| anyhow!("getblockcount: expected u64"))?;
    std::fs::write(layout.l1_genesis_file(), height.to_string())
        .context("persist L1 genesis height")?;
    Ok(())
}

fn load_wallet_if_exists(rpc: &btc::Rpc, name: &str) -> Result<()> {
    // loadwallet is idempotent enough — if it's already loaded
    // bitcoind returns a "wallet already loaded" error which we
    // ignore. List then load only if absent, to keep stderr clean.
    let listed = rpc.call("listwallets", json!([]))?;
    if let Some(arr) = listed.as_array() {
        if arr.iter().any(|v| v.as_str() == Some(name)) {
            return Ok(());
        }
    }
    rpc.call("loadwallet", json!([name]))?;
    Ok(())
}

fn read_l1_genesis(layout: &Layout) -> Result<u32> {
    let s = std::fs::read_to_string(layout.l1_genesis_file())
        .context("read l1_genesis (datadir incomplete? try `reset`)")?;
    s.trim().parse::<u32>().map_err(|e| anyhow!("parse l1_genesis: {e}"))
}

fn read_mining_address(layout: &Layout) -> Result<String> {
    let s = std::fs::read_to_string(layout.mining_address_file())
        .context("read mining_address (datadir incomplete? try `reset`)")?;
    Ok(s.trim().to_string())
}

// ---------- stop ----------

fn cmd_stop(layout: &Layout) -> Result<()> {
    let grace = Duration::from_secs(5);
    // Stop daemons before bitcoind so the sequencer's last
    // attestation-post attempt doesn't error noisily in the log.
    if let Some(pid) = pid_running(&layout.seq_pid())? {
        println!("[..]  stopping hodl-sequencer (pid {pid})…");
        procs::graceful_kill(pid, grace)?;
        let _ = std::fs::remove_file(layout.seq_pid());
        println!("[ok]  hodl-sequencer stopped");
    }
    if let Some(pid) = pid_running(&layout.node_pid())? {
        println!("[..]  stopping hodl-node (pid {pid})…");
        procs::graceful_kill(pid, grace)?;
        let _ = std::fs::remove_file(layout.node_pid());
        println!("[ok]  hodl-node stopped");
    }
    if let Some(pid) = pid_running(&layout.bitcoin_pid())? {
        // Prefer `bitcoin-cli stop` for a clean shutdown, fall back
        // to SIGTERM. Cookie may still be valid here since bitcoind
        // hasn't exited yet.
        let (_, cli) = btc::discover()?;
        println!("[..]  stopping bitcoind (pid {pid})…");
        let _ = std::process::Command::new(&cli)
            .arg(format!("-datadir={}", layout.bitcoin_dir().display()))
            .arg("-regtest")
            .arg("stop")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        // Poll the pid until it's gone, escalating after grace.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        while std::time::Instant::now() < deadline && procs::pid_alive(pid) {
            std::thread::sleep(Duration::from_millis(200));
        }
        if procs::pid_alive(pid) {
            procs::graceful_kill(pid, grace)?;
        }
        println!("[ok]  bitcoind stopped");
    } else {
        println!("[ok]  nothing to stop");
    }
    Ok(())
}

// ---------- status ----------

fn cmd_status(layout: &Layout) -> Result<()> {
    if !layout.exists() {
        println!("not initialised. Run `hodl-regtest start`.");
        return Ok(());
    }
    let btc_pid = pid_running(&layout.bitcoin_pid())?;
    let seq_pid = pid_running(&layout.seq_pid())?;
    let node_pid = pid_running(&layout.node_pid())?;
    let pid_line = |label, port, pid: Option<u32>| match pid {
        Some(p) => format!("  {label:14} pid {p}, 127.0.0.1:{port}"),
        None => format!("  {label:14} stopped"),
    };
    println!("datadir: {}", layout.root.display());
    println!("{}", pid_line("bitcoind", BTC_RPC_PORT, btc_pid));
    println!("{}", pid_line("hodl-sequencer", SEQUENCER_PORT, seq_pid));
    println!("{}", pid_line("hodl-node", NODE_PORT, node_pid));

    if btc_pid.is_some() {
        if let Ok(rpc) = btc::Rpc::from_cookie(&layout.bitcoin_cookie(), None) {
            if let Ok(h) = rpc.call("getblockcount", json!([])) {
                println!("\n  L1 height:    {h}");
            }
        }
    }
    if seq_pid.is_some() {
        if let Some(v) = http_get(&format!("http://127.0.0.1:{SEQUENCER_PORT}/head")) {
            if let Some(h) = v.get("height") {
                let l1 = v.get("l1_height").map(|x| x.to_string()).unwrap_or_default();
                println!("  L2 (seq):     height {h}, anchored at L1 {l1}");
            }
        }
    }
    if node_pid.is_some() {
        if let Some(v) = http_get(&format!("http://127.0.0.1:{NODE_PORT}/head")) {
            if let Some(h) = v.get("height") {
                let l1 = v.get("l1_height").map(|x| x.to_string()).unwrap_or_default();
                println!("  L2 (node):    height {h}, anchored at L1 {l1}");
            }
        }
    }
    Ok(())
}

fn http_get(url: &str) -> Option<Value> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    client.get(url).send().ok()?.json::<Value>().ok()
}

// ---------- mine ----------

fn cmd_mine(layout: &Layout, n: u32) -> Result<()> {
    require_running(layout, "bitcoind", &layout.bitcoin_pid())?;
    let rpc = btc::Rpc::from_cookie(&layout.bitcoin_cookie(), None)?;
    let addr = read_mining_address(layout)?;
    println!("[..]  mining {n} block{}…", if n == 1 { "" } else { "s" });
    let res = rpc.call("generatetoaddress", json!([n, addr]))?;
    let blocks = res.as_array().map(|a| a.len()).unwrap_or(0);
    let height: u64 = rpc
        .call("getblockcount", json!([]))?
        .as_u64()
        .ok_or_else(|| anyhow!("getblockcount: expected u64"))?;
    println!("[ok]  mined {blocks} block(s); L1 height now {height}");
    Ok(())
}

// ---------- fund ----------

fn cmd_fund(layout: &Layout, addr: &str, btc_amount: f64) -> Result<()> {
    require_running(layout, "bitcoind", &layout.bitcoin_pid())?;
    if btc_amount <= 0.0 {
        bail!("amount must be positive");
    }
    let user = btc::Rpc::from_cookie(&layout.bitcoin_cookie(), Some("user"))?;
    println!("[..]  sending {btc_amount} BTC to {addr}…");
    let txid = user.call("sendtoaddress", json!([addr, btc_amount]))?;
    let txid = txid.as_str().ok_or_else(|| anyhow!("sendtoaddress: expected txid string"))?;
    println!("[ok]  broadcast txid: {txid}");
    println!("       run `hodl-regtest mine 1` to confirm.");
    Ok(())
}

// ---------- reset ----------

fn cmd_reset(layout: &Layout, yes: bool) -> Result<()> {
    if !yes {
        print!(
            "this will wipe {}. proceed? [y/N] ",
            layout.root.display()
        );
        use std::io::Write;
        std::io::stdout().flush().ok();
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).ok();
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("aborted.");
            return Ok(());
        }
    }
    // Be defensive: even if `stop` errors, we still try to remove.
    let _ = cmd_stop(layout);
    if layout.root.exists() {
        std::fs::remove_dir_all(&layout.root)
            .with_context(|| format!("rm -rf {}", layout.root.display()))?;
        println!("[ok]  wiped {}", layout.root.display());
    } else {
        println!("[ok]  nothing to wipe");
    }
    Ok(())
}

// ---------- logs ----------

fn cmd_logs(layout: &Layout) -> Result<()> {
    for (label, path) in [
        ("hodl-sequencer", layout.seq_log()),
        ("hodl-node", layout.node_log()),
    ] {
        println!("--- {label} ({}) ---", path.display());
        if path.exists() {
            // Last ~40 lines, cheap and good enough.
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            for line in text.lines().rev().take(40).collect::<Vec<_>>().into_iter().rev() {
                println!("{line}");
            }
        } else {
            println!("(no log yet)");
        }
        println!();
    }
    Ok(())
}

// ---------- shared helpers ----------

fn pid_running(pid_path: &std::path::Path) -> Result<Option<u32>> {
    Ok(match procs::read_pid(pid_path)? {
        Some(p) if procs::pid_alive(p) => Some(p),
        // Stale pid file — clean it up so subsequent commands aren't
        // confused.
        Some(_) => {
            let _ = std::fs::remove_file(pid_path);
            None
        }
        None => None,
    })
}

/// Reject if `port` is already bound on 127.0.0.1. Uses a TCP
/// connect probe — if it succeeds, something else is listening.
fn check_port_free(port: u16, name: &str) -> Result<()> {
    use std::net::TcpStream;
    use std::time::Duration;
    let addr = format!("127.0.0.1:{port}");
    match TcpStream::connect_timeout(
        &addr.parse().expect("static addr"),
        Duration::from_millis(200),
    ) {
        Ok(_) => bail!(
            "port {port} ({name}) is already in use. \
             Stop the process bound to it, then retry. \
             (Hint: `ss -tlnp | grep {port}`)"
        ),
        Err(_) => Ok(()),
    }
}

fn require_running(layout: &Layout, name: &str, pid_path: &std::path::Path) -> Result<()> {
    if pid_running(pid_path)?.is_none() {
        bail!(
            "{name} is not running. Run `hodl-regtest start` first \
             (datadir: {}).",
            layout.root.display()
        );
    }
    Ok(())
}
