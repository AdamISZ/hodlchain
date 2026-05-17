//! hodl-wallet — CLI wallet for the hodlcoin POC.
//!
//! See `docs/design.md` for the L1 mint UTXO format and protocol invariants.

mod api;
mod bitcoind;
mod wallet;

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::secp256k1::{rand, Message, Secp256k1, XOnlyPublicKey};
use bitcoin::{Amount, OutPoint};
use clap::{Parser, Subcommand};
use hodl_core::l1::{derive_mint_taproot, mint_address};
use hodl_core::proof::OutpointProof;
use hodl_core::proof::MintProofEnvelope;
use hodl_core::tx::{SignedTransfer, TransferBody};
use std::path::PathBuf;

use crate::api::ApiClient;
use crate::bitcoind::Bitcoind;
use crate::wallet::{
    network_from_str, parse_outpoint, BitcoindAuth, BitcoindConfig, MintRecord, WalletFile,
    DEFAULT_WALLET_PATH,
};

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
    /// Query sequencer head.
    Head,
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
    /// Overwrite an existing wallet file at the target path.
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct MintUtxoArgs {
    /// Relative locktime T, in blocks (BIP112 CSV). Range: [1, 65535].
    /// This is the duration baked into L_spend; it does not depend on
    /// the current L1 tip.
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

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Keygen(args) => cmd_keygen(&cli.wallet, args),
        Cmd::Address => cmd_address(&cli.wallet),
        Cmd::MintUtxo(args) => cmd_mint_utxo(&cli.wallet, args),
        Cmd::ListMints => cmd_list_mints(&cli.wallet),
        Cmd::MintMessage(args) => cmd_mint_message(&cli.wallet, args).await,
        Cmd::Transfer(args) => cmd_transfer(&cli.wallet, args).await,
        Cmd::Balance(args) => cmd_balance(&cli.wallet, args).await,
        Cmd::Head => cmd_head(&cli.wallet).await,
    }
}

fn cmd_keygen(path: &std::path::Path, args: KeygenArgs) -> Result<()> {
    if path.exists() && !args.force {
        bail!("{} already exists (use --force to overwrite)", path.display());
    }
    let network = network_from_str(&args.network)?;
    let auth = match (args.bitcoind_cookie, args.bitcoind_user, args.bitcoind_pass) {
        (Some(p), None, None) => BitcoindAuth::Cookie { path: p },
        (None, Some(u), Some(pw)) => BitcoindAuth::UserPass { user: u, password: pw },
        (None, None, None) => bail!("specify either --bitcoind-cookie or --bitcoind-user/--bitcoind-pass"),
        _ => bail!("conflicting bitcoind auth flags"),
    };

    let secp = Secp256k1::new();
    let kp = bitcoin::secp256k1::Keypair::new(&secp, &mut rand::thread_rng());
    let sk = kp.secret_key();
    let (xonly, _) = kp.x_only_public_key();

    let wf = WalletFile {
        network,
        secret_key_hex: hex::encode(sk.secret_bytes()),
        bitcoind: BitcoindConfig { url: args.bitcoind_url, auth },
        sequencer_url: args.sequencer_url,
        node_url: args.node_url,
        mints: Vec::new(),
    };
    wf.save(path)?;
    println!("wrote {}", path.display());
    println!("L2 address: {}", hex::encode(xonly.serialize()));
    Ok(())
}

fn cmd_address(path: &std::path::Path) -> Result<()> {
    let wf = WalletFile::load(path)?;
    let secp = Secp256k1::new();
    let xonly = wf.xonly_pubkey(&secp)?;
    println!("{}", hex::encode(xonly.serialize()));
    Ok(())
}

fn cmd_mint_utxo(path: &std::path::Path, args: MintUtxoArgs) -> Result<()> {
    use hodl_core::consensus::MAX_LOCK_BLOCKS;
    if args.lock_blocks == 0 || args.lock_blocks > MAX_LOCK_BLOCKS {
        bail!(
            "--lock-blocks must be in [1, {}] (BIP112 CSV block-form range)",
            MAX_LOCK_BLOCKS
        );
    }

    let mut wf = WalletFile::load(path)?;
    let secp = Secp256k1::new();
    let xonly = wf.xonly_pubkey(&secp)?;
    let network = wf.network.into_bitcoin();

    let bd = Bitcoind::connect(&wf.bitcoind)?;
    let tip = bd.block_count()?;

    // CSV is relative; the script commits T directly. No need to add
    // tip + lock_blocks — that arithmetic was a CLTV artefact.
    let (spk, _spend) = derive_mint_taproot(&secp, args.lock_blocks, &xonly);
    let address = mint_address(&secp, args.lock_blocks, &xonly, network);

    let amount = Amount::from_btc(args.value_btc).context("invalid BTC amount")?;
    println!("L1 tip: {tip}");
    println!("relative locktime T: {} blocks", args.lock_blocks);
    println!("mint address: {}", address);
    println!("sending {} BTC...", args.value_btc);

    let (txid, vout) = bd.send_to_address(&address, amount, &spk)?;
    println!("broadcast txid: {txid}");
    println!("mint outpoint: {txid}:{vout}");

    wf.upsert_mint(MintRecord {
        outpoint: format!("{txid}:{vout}"),
        value_sat: amount.to_sat(),
        lock_blocks: args.lock_blocks,
        minted: false,
    });
    wf.save(path)?;
    Ok(())
}

fn cmd_list_mints(path: &std::path::Path) -> Result<()> {
    let wf = WalletFile::load(path)?;
    if wf.mints.is_empty() {
        println!("(no recorded mints)");
        return Ok(());
    }
    for m in &wf.mints {
        let used = if m.minted { "minted" } else { "available" };
        println!("{} v={}sat T={}blocks {}", m.outpoint, m.value_sat, m.lock_blocks, used);
    }
    Ok(())
}

async fn cmd_mint_message(path: &std::path::Path, args: MintMessageArgs) -> Result<()> {
    let mut wf = WalletFile::load(path)?;
    let secp = Secp256k1::new();
    let kp = wf.keypair(&secp)?;
    let xonly = wf.xonly_pubkey(&secp)?;

    let record = wf
        .find_mint(&args.outpoint)
        .ok_or_else(|| anyhow!("no recorded mint for {}", args.outpoint))?
        .clone();
    let outpoint: OutPoint = parse_outpoint(&record.outpoint)?;

    let l2_destination = match args.to {
        Some(hex_s) => parse_xonly(&hex_s)?,
        None => xonly,
    };

    // Signature over sha256("hodl-mint-v0" || outpoint || l2_destination).
    let sighash = OutpointProof::sighash(&outpoint, &l2_destination);
    let msg = Message::from_digest(sighash);
    let signature = secp.sign_schnorr(&msg, &kp);

    let proof = OutpointProof {
        outpoint,
        user_xonly_pubkey: xonly,
        lock_blocks: record.lock_blocks,
        signature,
    };

    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let resp = api.submit_mint(MintProofEnvelope::V0Outpoint(proof), l2_destination).await?;
    if resp.accepted {
        println!("accepted: mint_amount={:?} nullifier={:?}", resp.mint_amount, resp.nullifier_hex);
        if let Some(r) = wf.find_mint_mut(&args.outpoint) {
            r.minted = true;
        }
        wf.save(path)?;
    } else {
        println!("rejected: {}", resp.error.unwrap_or_default());
    }
    Ok(())
}

async fn cmd_transfer(path: &std::path::Path, args: TransferArgs) -> Result<()> {
    let wf = WalletFile::load(path)?;
    let secp = Secp256k1::new();
    let kp = wf.keypair(&secp)?;
    let from = wf.xonly_pubkey(&secp)?;
    let to = parse_xonly(&args.to)?;

    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let bal = api.balance(&from).await?;

    let body = TransferBody { from, to, amount: args.amount, nonce: bal.nonce };
    let msg = Message::from_digest(body.sighash().0);
    let signature = secp.sign_schnorr(&msg, &kp);
    let signed = SignedTransfer { body, signature };

    let resp = api.submit_transfer(signed).await?;
    if resp.accepted {
        println!("transfer accepted");
    } else {
        println!("rejected: {}", resp.error.unwrap_or_default());
    }
    Ok(())
}

async fn cmd_balance(path: &std::path::Path, args: BalanceArgs) -> Result<()> {
    let wf = WalletFile::load(path)?;
    let secp = Secp256k1::new();
    let target = match args.addr {
        Some(s) => parse_xonly(&s)?,
        None => wf.xonly_pubkey(&secp)?,
    };
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let bal = api.balance(&target).await?;
    println!("address: {}", hex::encode(target.serialize()));
    println!("balance: {} atoms", bal.balance);
    println!("nonce:   {}", bal.nonce);
    Ok(())
}

async fn cmd_head(path: &std::path::Path) -> Result<()> {
    let wf = WalletFile::load(path)?;
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let head = api.sequencer_head().await?;
    println!("{}", serde_json::to_string_pretty(&head)?);
    Ok(())
}

fn parse_xonly(s: &str) -> Result<XOnlyPublicKey> {
    let bytes = hex::decode(s).context("decode pubkey hex")?;
    Ok(XOnlyPublicKey::from_slice(&bytes).context("parse x-only pubkey")?)
}
