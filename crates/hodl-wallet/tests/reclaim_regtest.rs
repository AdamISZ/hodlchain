//! End-to-end test that the CSV-locked reclaim tx the wallet constructs
//! is genuinely spendable on Bitcoin.
//!
//! Spawns a fresh bitcoind on regtest in a tempdir, funds a mint
//! address derived from `hodl_core::l1::mint_address`, then exercises
//! `hodl_wallet::reclaim::build_signed_reclaim_tx` against the real
//! Bitcoin Core mempool + block validation:
//!
//!   Phase 1: broadcast before CSV maturity → must be rejected
//!            (Bitcoin Core's `non-BIP68-final` policy).
//!   Phase 2: mine to maturity, broadcast same tx → must be accepted
//!            and included in the next block; the funding outpoint
//!            must be consumed and the destination address must
//!            receive `value - fee`.
//!
//! This is the only check that can directly answer the audit
//! question "could a malformed L_spend script silently brick funds?"
//! — the unit tests in `hodl_core::l1` pin construction bytes; only
//! a real Bitcoin Core can decide whether those bytes are accepted as
//! a valid spend.
//!
//! Requires `bitcoind` and `bitcoin-cli` on `$PATH`, or set
//! `BITCOIND_BIN` / `BITCOIN_CLI_BIN`. If neither is found the test
//! prints `SKIP` and passes — `cargo test` on a dev machine without
//! Bitcoin Core installed stays green.

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::consensus::encode::serialize_hex;
use bitcoin::secp256k1::{Keypair, Secp256k1};
use bitcoin::{Address, Network, OutPoint, Txid};
use bitcoincore_rpc::{Auth, Client, RpcApi};
use hodl_core::l1::{expected_p2tr_spk, mint_address};
use hodl_wallet::reclaim::build_signed_reclaim_tx;
use serde_json::{json, Value};
use std::env;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::str::FromStr;
use std::time::{Duration, Instant};

// ---------- bitcoind lifecycle (RAII) ----------

struct RegtestBitcoind {
    datadir: tempfile::TempDir,
    rpc_port: u16,
    cli: PathBuf,
    cookie: PathBuf,
}

impl RegtestBitcoind {
    fn start() -> Result<Self> {
        let bitcoind = find_bin("bitcoind", "BITCOIND_BIN")?;
        let cli = find_bin("bitcoin-cli", "BITCOIN_CLI_BIN")?;
        let datadir = tempfile::tempdir().context("create datadir")?;
        let rpc_port = free_port()?;
        let p2p_port = free_port()?;

        let conf = format!(
            "fallbackfee=0.00001\n\
             txindex=1\n\
             [regtest]\n\
             rpcport={rpc_port}\n\
             port={p2p_port}\n",
        );
        std::fs::write(datadir.path().join("bitcoin.conf"), conf)
            .context("write bitcoin.conf")?;

        let status = Command::new(&bitcoind)
            .arg(format!("-datadir={}", datadir.path().display()))
            .arg("-regtest")
            .arg("-daemon")
            .arg("-listen=0")
            .arg("-rpcbind=127.0.0.1")
            .arg("-rpcallowip=127.0.0.1")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .status()
            .context("spawn bitcoind")?;
        if !status.success() {
            bail!("bitcoind exited with status {status} while starting");
        }
        let cookie = datadir.path().join("regtest/.cookie");
        wait_for_rpc(&cli, datadir.path(), Duration::from_secs(30))
            .context("wait for bitcoind RPC")?;
        Ok(Self {
            datadir,
            rpc_port,
            cli,
            cookie,
        })
    }

    fn client(&self, wallet: Option<&str>) -> Result<Client> {
        let url = match wallet {
            Some(w) => format!("http://127.0.0.1:{}/wallet/{w}", self.rpc_port),
            None => format!("http://127.0.0.1:{}", self.rpc_port),
        };
        Client::new(&url, Auth::CookieFile(self.cookie.clone()))
            .context("connect bitcoincore-rpc")
    }
}

impl Drop for RegtestBitcoind {
    fn drop(&mut self) {
        // Prefer a clean shutdown so TempDir::drop can recursively
        // remove the datadir without tripping on bitcoind's still-open
        // LMDB / blk files.
        let _ = Command::new(&self.cli)
            .arg(format!("-datadir={}", self.datadir.path().display()))
            .arg("-regtest")
            .arg("stop")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn find_bin(name: &str, env_var: &str) -> Result<PathBuf> {
    if let Ok(p) = env::var(env_var) {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Ok(p);
        }
        bail!("{env_var}={} is not a file", p.display());
    }
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            let cand = dir.join(name);
            if cand.is_file() {
                return Ok(cand);
            }
        }
    }
    bail!("{name} not found on $PATH and {env_var} unset")
}

/// Bind an ephemeral TCP port, drop the listener, return the number.
/// Small TOCTOU race vs. bitcoind binding it, but in practice we don't
/// see collisions because no other process is grabbing high ports
/// in the test's tempdir lifetime.
fn free_port() -> Result<u16> {
    let l = TcpListener::bind("127.0.0.1:0").context("bind ephemeral port")?;
    Ok(l.local_addr()?.port())
}

fn wait_for_rpc(cli: &Path, datadir: &Path, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let probe = Command::new(cli)
            .arg(format!("-datadir={}", datadir.display()))
            .arg("-regtest")
            .arg("getblockcount")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if probe.map(|s| s.success()).unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    bail!("bitcoind did not become RPC-ready within {timeout:?}")
}

// ---------- the test ----------

#[test]
fn reclaim_csv_lock_is_enforced_and_then_spendable() {
    // Skip cleanly if Bitcoin Core isn't installed locally. CI is
    // expected to provision it; a developer running `cargo test` on
    // a machine without it should not see a red bar.
    if find_bin("bitcoind", "BITCOIND_BIN").is_err()
        || find_bin("bitcoin-cli", "BITCOIN_CLI_BIN").is_err()
    {
        eprintln!(
            "SKIP reclaim_csv_lock_is_enforced_and_then_spendable: \
             bitcoind/bitcoin-cli not found (set BITCOIND_BIN, \
             BITCOIN_CLI_BIN, or put them on $PATH)"
        );
        return;
    }
    run().expect("reclaim regtest")
}

fn run() -> Result<()> {
    let btcd = RegtestBitcoind::start()?;

    // ---- Set up wallet and mine maturity ----
    let root = btcd.client(None)?;
    let _: Value = root.call("createwallet", &[json!("test")])?;
    let w: Client = btcd.client(Some("test"))?;
    let mining_addr_s: String = w.call("getnewaddress", &[json!("mining"), json!("bech32m")])?;
    let _: Value = w.call("generatetoaddress", &[json!(101), json!(mining_addr_s)])?;

    // ---- Derive mint address from a fresh "user" keypair ----
    let secp = Secp256k1::new();
    let user_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
    let (user_xonly, _) = user_kp.x_only_public_key();
    let lock_blocks: u32 = 20;
    let mint_addr: Address = mint_address(&secp, lock_blocks, &user_xonly, Network::Regtest)
        .map_err(|e| anyhow!("build mint address: {e:?}"))?;

    // ---- Fund mint UTXO ----
    let mint_value_sat: u64 = 10_000_000; // 0.1 BTC
    let funding_txid_s: String = w.call(
        "sendtoaddress",
        &[json!(mint_addr.to_string()), json!(0.1)],
    )?;
    // Confirm the funding tx. Mine to the existing mining address so
    // the wallet's coinbase outputs stay consolidated.
    let _: Value = w.call("generatetoaddress", &[json!(1), json!(mining_addr_s)])?;

    // ---- Resolve the funding outpoint by SPK match ----
    // We expect exactly one of the funding tx's outputs to carry the
    // mint SPK (`bitcoind`'s wallet additionally creates a change
    // output for itself). Match by the expected P2TR scriptPubKey.
    let mint_spk = expected_p2tr_spk(&secp, lock_blocks, &user_xonly)
        .map_err(|e| anyhow!("derive mint SPK: {e:?}"))?;
    let mint_spk_hex = hex::encode(mint_spk.as_bytes());
    let funding_tx: Value =
        w.call("getrawtransaction", &[json!(funding_txid_s), json!(2)])?;
    let vouts = funding_tx["vout"]
        .as_array()
        .ok_or_else(|| anyhow!("funding tx vout missing"))?;
    let (vout_index, vout_entry) = vouts
        .iter()
        .enumerate()
        .find(|(_, v)| v["scriptPubKey"]["hex"].as_str() == Some(mint_spk_hex.as_str()))
        .ok_or_else(|| anyhow!("mint SPK not found among funding tx vouts"))?;
    let mint_outpoint = OutPoint {
        txid: Txid::from_str(&funding_txid_s)?,
        vout: vout_index as u32,
    };
    let observed_sat =
        (vout_entry["value"].as_f64().unwrap_or(0.0) * 100_000_000.0).round() as u64;
    assert_eq!(observed_sat, mint_value_sat, "funding amount mismatch");

    let funded_at: u64 = w.call("getblockcount", &[])?;

    // ---- Build & sign the reclaim tx (same tx used for both phases) ----
    let dest_addr_s: String = w.call("getnewaddress", &[json!("dest"), json!("bech32m")])?;
    let dest_addr = Address::from_str(&dest_addr_s)?.require_network(Network::Regtest)?;
    let fee_sat: u64 = 1_000;
    let reclaim_tx = build_signed_reclaim_tx(
        &secp,
        &user_kp,
        mint_outpoint,
        mint_value_sat,
        lock_blocks,
        &dest_addr,
        fee_sat,
    )?;
    let reclaim_hex = serialize_hex(&reclaim_tx);

    // ---- Phase 1: pre-maturity broadcast must be rejected ----
    let early: std::result::Result<Value, _> =
        w.call("sendrawtransaction", &[json!(reclaim_hex.clone())]);
    let err = early.err().ok_or_else(|| {
        anyhow!("expected pre-maturity broadcast to fail with non-BIP68-final, but it succeeded")
    })?;
    let err_s = err.to_string();
    assert!(
        err_s.contains("non-BIP68-final")
            || err_s.contains("non-final")
            || err_s.contains("BIP68"),
        "expected non-BIP68-final rejection, got: {err_s}"
    );

    // ---- Phase 2: mine to CSV maturity, then broadcast ----
    // CSV-final at block h = funded_at + lock_blocks. To mine the
    // reclaim into block h, tip must be h - 1 = funded_at + lock_blocks - 1
    // before broadcast. We already mined 1 conf block, so the tip is
    // funded_at; mine `lock_blocks - 1` more.
    let need_blocks = (lock_blocks - 1) as u64;
    let _: Value =
        w.call("generatetoaddress", &[json!(need_blocks), json!(mining_addr_s)])?;
    let tip: u64 = w.call("getblockcount", &[])?;
    assert_eq!(
        tip,
        funded_at + (lock_blocks as u64) - 1,
        "tip arithmetic"
    );

    let broadcast_txid_s: String =
        w.call("sendrawtransaction", &[json!(reclaim_hex)])?;
    let broadcast_txid = Txid::from_str(&broadcast_txid_s)?;
    assert_eq!(
        broadcast_txid,
        reclaim_tx.compute_txid(),
        "txid returned by bitcoind differs from locally computed",
    );

    // Mine 1 more block, expect the reclaim to be in it.
    let block_hashes: Vec<String> =
        w.call("generatetoaddress", &[json!(1), json!(mining_addr_s)])?;
    let block_hash = block_hashes.first().ok_or_else(|| anyhow!("no block hash"))?;
    let block: Value = w.call("getblock", &[json!(block_hash), json!(1)])?;
    let block_txids = block["tx"]
        .as_array()
        .ok_or_else(|| anyhow!("block tx list missing"))?;
    assert!(
        block_txids
            .iter()
            .any(|t| t.as_str() == Some(broadcast_txid_s.as_str())),
        "reclaim tx not included in the next mined block"
    );

    // ---- Verify state: funding spent, dest credited ----
    let utxo_after: Value = w.call(
        "gettxout",
        &[
            json!(funding_txid_s),
            json!(vout_index as u32),
            json!(true), // include mempool
        ],
    )?;
    assert!(
        utxo_after.is_null(),
        "funding outpoint still in UTXO set after reclaim: {utxo_after}"
    );

    let received_btc: f64 =
        w.call("getreceivedbyaddress", &[json!(dest_addr_s), json!(1u32)])?;
    let received_sat = (received_btc * 100_000_000.0).round() as u64;
    let expected_sat = mint_value_sat - fee_sat;
    assert_eq!(
        received_sat, expected_sat,
        "destination received {received_sat} sat, expected {expected_sat}"
    );

    Ok(())
}
