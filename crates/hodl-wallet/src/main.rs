//! hodl-wallet CLI: a thin shim over `hodl_wallet::ops`.
//!
//! Every command is a clap arg-parse → typed `ops::*Input` → call →
//! format-the-typed-output. There is intentionally no business logic
//! in this file; if you find yourself reaching past `ops` for the
//! `api` / `bitcoind` / `verify` modules directly, refactor it.

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::secp256k1::XOnlyPublicKey;
use clap::{Parser, Subcommand};
use hodl_core::hash::H256;
use hodl_wallet::ops::{self, LightBalanceMode};
use hodl_wallet::wallet::{
    network_from_str, BitcoindAuth, BitcoindConfig, DEFAULT_WALLET_PATH,
};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hodl-wallet", version, about = "hodlcoin POC wallet")]
struct Cli {
    /// Path to the wallet JSON file.
    #[arg(long, global = true, default_value = DEFAULT_WALLET_PATH)]
    wallet: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Create a new wallet file with a freshly generated keypair.
    Keygen(KeygenArgs),
    /// Print our L2 address (x-only pubkey hex).
    Address,
    /// Create a CSV-locked Taproot mint UTXO on L1.
    MintUtxo(MintUtxoArgs),
    /// List recorded mint UTXOs.
    ListMints,
    /// Submit a mint message to the sequencer for a previously created UTXO.
    MintMessage(MintMessageArgs),
    /// Send an L2 transfer.
    Transfer(TransferArgs),
    /// Query an L2 balance.
    Balance(BalanceArgs),
    /// Query an L2 balance AND cryptographically verify the
    /// inclusion proof against the response's state_root.
    /// With `--against`, also check that state_root matches an
    /// externally-supplied value (e.g. one obtained from L1 via
    /// the attestation chain).
    VerifyBalance(VerifyBalanceArgs),
    /// Query sequencer head.
    Head,
    /// Light-client mode: walk the L1 attestation chain via the
    /// configured Esplora endpoint and report the current L2 head's
    /// state_root. No bitcoin node required.
    LightHead,
    /// Light-client mode: replay every L2 block from genesis, verifying
    /// each one (mint witnesses against L1 via Esplora, transfer
    /// signatures, state-root continuity). Then report the balance from
    /// the locally-rebuilt LedgerState. End-to-end trustless wrt the
    /// chosen Esplora + sequencer block-body endpoint.
    LightBalance(LightBalanceArgs),
}

#[derive(clap::Args, Debug)]
struct KeygenArgs {
    #[arg(long)]
    network: String,
    #[arg(long, default_value = "http://127.0.0.1:18443")]
    bitcoind_url: String,
    #[arg(long, conflicts_with_all = ["bitcoind_user", "bitcoind_pass"])]
    bitcoind_cookie: Option<PathBuf>,
    #[arg(long, requires = "bitcoind_pass")]
    bitcoind_user: Option<String>,
    #[arg(long, requires = "bitcoind_user")]
    bitcoind_pass: Option<String>,
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    sequencer_url: String,
    #[arg(long)]
    node_url: Option<String>,
    /// Esplora HTTP base URL for light-client mode (light-head /
    /// light-balance). Demo points it at hodl-node which exposes a
    /// slim Esplora-compatible subset.
    #[arg(long)]
    esplora_url: Option<String>,
    /// Overwrite an existing wallet file at the target path.
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct MintUtxoArgs {
    /// Relative locktime T, in blocks (BIP112 CSV). Range: [1, 65535].
    #[arg(long)]
    lock_blocks: u32,
    /// Value to lock, in BTC.
    #[arg(long)]
    value_btc: f64,
}

#[derive(clap::Args, Debug)]
struct MintMessageArgs {
    /// "<txid>:<vout>" of a previously created mint UTXO.
    #[arg(long)]
    outpoint: String,
    /// L2 destination x-only pubkey hex. Defaults to our own address.
    #[arg(long)]
    to: Option<String>,
}

#[derive(clap::Args, Debug)]
struct TransferArgs {
    /// Destination L2 x-only pubkey hex.
    #[arg(long)]
    to: String,
    /// Amount in L2 atoms.
    #[arg(long)]
    amount: u64,
}

#[derive(clap::Args, Debug)]
struct BalanceArgs {
    /// Address to query (x-only pubkey hex). Defaults to our own address.
    #[arg(long)]
    addr: Option<String>,
}

#[derive(clap::Args, Debug)]
struct LightBalanceArgs {
    /// Address to query (x-only pubkey hex). Defaults to our own address.
    #[arg(long)]
    addr: Option<String>,
}

#[derive(clap::Args, Debug)]
struct VerifyBalanceArgs {
    /// Address to query (x-only pubkey hex). Defaults to our own address.
    #[arg(long)]
    addr: Option<String>,
    /// Optional 32-byte hex state_root to compare against.
    #[arg(long)]
    against: Option<String>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Keygen(args) => cmd_keygen(cli.wallet, args),
        Cmd::Address => cmd_address(&cli.wallet),
        Cmd::MintUtxo(args) => cmd_mint_utxo(cli.wallet, args),
        Cmd::ListMints => cmd_list_mints(&cli.wallet),
        Cmd::MintMessage(args) => cmd_mint_message(cli.wallet, args).await,
        Cmd::Transfer(args) => cmd_transfer(cli.wallet, args).await,
        Cmd::Balance(args) => cmd_balance(cli.wallet, args).await,
        Cmd::VerifyBalance(args) => cmd_verify_balance(cli.wallet, args).await,
        Cmd::Head => cmd_head(&cli.wallet).await,
        Cmd::LightHead => cmd_light_head(&cli.wallet).await,
        Cmd::LightBalance(args) => cmd_light_balance(cli.wallet, args).await,
    }
}

fn cmd_keygen(wallet_path: PathBuf, args: KeygenArgs) -> Result<()> {
    let network = network_from_str(&args.network)?;
    let auth = match (args.bitcoind_cookie, args.bitcoind_user, args.bitcoind_pass) {
        (Some(p), None, None) => BitcoindAuth::Cookie { path: p },
        (None, Some(u), Some(pw)) => BitcoindAuth::UserPass { user: u, password: pw },
        (None, None, None) => {
            bail!("specify either --bitcoind-cookie or --bitcoind-user/--bitcoind-pass")
        }
        _ => bail!("conflicting bitcoind auth flags"),
    };
    let out = ops::keygen(ops::KeygenInput {
        wallet_path,
        network,
        bitcoind: BitcoindConfig {
            url: args.bitcoind_url,
            auth,
        },
        sequencer_url: args.sequencer_url,
        node_url: args.node_url,
        esplora_url: args.esplora_url,
        force: args.force,
    })?;
    println!("wrote {}", out.wallet_path.display());
    println!("L2 address: {}", hex::encode(out.l2_address.serialize()));
    Ok(())
}

fn cmd_address(wallet_path: &std::path::Path) -> Result<()> {
    let addr = ops::address(wallet_path)?;
    println!("{}", hex::encode(addr.serialize()));
    Ok(())
}

fn cmd_mint_utxo(wallet_path: PathBuf, args: MintUtxoArgs) -> Result<()> {
    let out = ops::mint_utxo(ops::MintUtxoInput {
        wallet_path,
        lock_blocks: args.lock_blocks,
        value_btc: args.value_btc,
    })?;
    println!("L1 tip: {}", out.l1_tip);
    println!("relative locktime T: {} blocks", out.lock_blocks);
    println!("mint address: {}", out.mint_address);
    println!("sending {} sat ({} BTC)...",
        out.value_sat,
        out.value_sat as f64 / 100_000_000.0,
    );
    println!("broadcast txid: {}", out.txid);
    println!("mint outpoint: {}:{}", out.txid, out.vout);
    Ok(())
}

fn cmd_list_mints(wallet_path: &std::path::Path) -> Result<()> {
    let mints = ops::list_mints(wallet_path)?;
    if mints.is_empty() {
        println!("(no recorded mints)");
        return Ok(());
    }
    for m in &mints {
        let used = if m.minted { "minted" } else { "available" };
        println!(
            "{} v={}sat T={}blocks {}",
            m.outpoint, m.value_sat, m.lock_blocks, used
        );
    }
    Ok(())
}

async fn cmd_mint_message(wallet_path: PathBuf, args: MintMessageArgs) -> Result<()> {
    let to = args.to.map(|s| parse_xonly(&s)).transpose()?;
    let out = ops::mint_message(ops::MintMessageInput {
        wallet_path,
        outpoint: args.outpoint,
        to,
    })
    .await?;
    if out.accepted {
        println!(
            "accepted: mint_amount={:?} nullifier={:?}",
            out.mint_amount, out.nullifier_hex
        );
    } else {
        println!("rejected: {}", out.error.unwrap_or_default());
    }
    Ok(())
}

async fn cmd_transfer(wallet_path: PathBuf, args: TransferArgs) -> Result<()> {
    let to = parse_xonly(&args.to)?;
    let out = ops::transfer(ops::TransferInput {
        wallet_path,
        to,
        amount: args.amount,
    })
    .await?;
    if out.accepted {
        println!("transfer accepted");
    } else {
        println!("rejected: {}", out.error.unwrap_or_default());
    }
    Ok(())
}

async fn cmd_balance(wallet_path: PathBuf, args: BalanceArgs) -> Result<()> {
    let addr = args.addr.map(|s| parse_xonly(&s)).transpose()?;
    let out = ops::balance(ops::BalanceInput { wallet_path, addr }).await?;
    println!("address: {}", hex::encode(out.address.serialize()));
    println!("balance: {} atoms", out.balance);
    println!("nonce:   {}", out.nonce);
    Ok(())
}

async fn cmd_verify_balance(wallet_path: PathBuf, args: VerifyBalanceArgs) -> Result<()> {
    let addr = args.addr.map(|s| parse_xonly(&s)).transpose()?;
    let against = args
        .against
        .map(|s| H256::from_hex(&s).context("parse --against hex"))
        .transpose()?;
    let out = ops::verify_balance(ops::VerifyBalanceInput {
        wallet_path,
        addr,
        against,
    })
    .await?;
    println!("verified");
    println!("  address:     {}", hex::encode(out.address.serialize()));
    println!("  balance:     {} atoms", out.balance);
    println!("  nonce:       {}", out.nonce);
    println!("  l2_height:   {}", out.l2_height);
    println!("  state_root:  {}", out.state_root);
    if out.bound_to_l1 {
        println!("  ⇒ state_root matches --against value (bound to L1)");
    } else {
        println!("  ⇒ state_root not checked against an external source");
        println!("    (pass --against <hex> to bind to L1 in light-client mode)");
    }
    Ok(())
}

async fn cmd_head(wallet_path: &std::path::Path) -> Result<()> {
    let head = ops::sequencer_head(wallet_path).await?;
    println!("{}", serde_json::to_string_pretty(&head)?);
    Ok(())
}

async fn cmd_light_head(wallet_path: &std::path::Path) -> Result<()> {
    let out = ops::light_head(wallet_path).await?;
    println!("L2 head (derived from L1 attestation chain via Esplora):");
    println!("  l2_height:  {}", out.l2_height);
    println!("  state_root: {}", out.state_root);
    println!(
        "  walked {} attestation(s) from anchor_0",
        out.attestations_walked
    );
    Ok(())
}

async fn cmd_light_balance(wallet_path: PathBuf, args: LightBalanceArgs) -> Result<()> {
    let addr = args.addr.map(|s| parse_xonly(&s)).transpose()?;
    let out = ops::light_balance(ops::LightBalanceInput { wallet_path, addr }).await?;
    let mode_label = match out.mode {
        LightBalanceMode::ColdStart => "cold-start",
        LightBalanceMode::WarmStart => "warm-start",
    };
    println!(
        "verified ({mode_label}, {} new block(s))",
        out.blocks_verified
    );
    println!("  l2_height:        {}", out.l2_height);
    println!("  state_root:       {}", out.state_root);
    println!("  accounts_root:    {}", out.accounts_root);
    println!("  block_hash:       {}", out.block_hash);
    println!("  l1_height:        {}", out.l1_height);
    let label = if out.is_own_address {
        "address (own):   "
    } else {
        "address:         "
    };
    println!("  {} {}", label, hex::encode(out.address.serialize()));
    println!("  balance:          {} atoms", out.balance);
    println!("  nonce:            {}", out.nonce);
    Ok(())
}

fn parse_xonly(s: &str) -> Result<XOnlyPublicKey> {
    let bytes = hex::decode(s).context("decode pubkey hex")?;
    XOnlyPublicKey::from_slice(&bytes)
        .map_err(|_| anyhow!("invalid x-only pubkey hex"))
}
