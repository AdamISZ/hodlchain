//! hodl-wallet CLI: a thin shim over `hodl_wallet::ops`.
//!
//! Every command is a clap arg-parse → typed `ops::*Input` → call →
//! format-the-typed-output. There is intentionally no business logic
//! in this file; if you find yourself reaching past `ops` for the
//! `api` / `bitcoind` / `verify` modules directly, refactor it.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use hodl_core::hash::H256;
use hodl_wallet::ops::{self, LightBalanceMode, MintFundingState, ReclaimStatus};
use hodl_wallet::wallet::{network_from_str, DEFAULT_WALLET_PATH};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "hodl-wallet", version, about = "hodlchain POC wallet")]
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
    /// Derive a fresh CSV-locked Taproot deposit address. Does not
    /// broadcast; the user funds it from their own L1 wallet.
    MintUtxo(MintUtxoArgs),
    /// List recorded mint UTXOs.
    ListMints,
    /// Poll Esplora for a funding UTXO at a recorded mint's deposit
    /// address. Updates the wallet's local MintRecord with the
    /// observed outpoint + value + height.
    MintWatch(MintWatchArgs),
    /// Submit a mint message to the sequencer for a previously
    /// funded mint UTXO.
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
    /// List mint UTXOs with their reclaim status (pending confirmation,
    /// locked for N more blocks, ready to reclaim, or already reclaimed).
    ReclaimList,
    /// Spend a CSV-matured mint UTXO back to an L1 destination address.
    Reclaim(ReclaimArgs),
}

#[derive(clap::Args, Debug)]
struct KeygenArgs {
    #[arg(long)]
    network: String,
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    sequencer_url: String,
    #[arg(long)]
    node_url: Option<String>,
    /// Required: Esplora HTTP base URL. The wallet's only L1 data
    /// source. Point at mempool.space, a self-hosted electrs, or
    /// hodl-node's slim Esplora-compatible subset.
    #[arg(long)]
    esplora_url: String,
    /// Path to a file containing a BIP39 mnemonic to *restore* from
    /// instead of generating a fresh one. The file's contents are
    /// trimmed and parsed as a phrase (any valid BIP39 word count
    /// works). Useful for porting a wallet between machines or
    /// recovering after a config wipe.
    #[arg(long)]
    from_mnemonic_file: Option<PathBuf>,
    /// Overwrite an existing wallet file at the target path.
    #[arg(long)]
    force: bool,
}

#[derive(clap::Args, Debug)]
struct MintUtxoArgs {
    /// Relative locktime T, in blocks (BIP112 CSV). Range: [1, 65535].
    #[arg(long)]
    lock_blocks: u32,
}

#[derive(clap::Args, Debug)]
struct MintWatchArgs {
    /// BIP32 index of the mint to poll for funding. See `list-mints`.
    #[arg(long)]
    bip32_index: u32,
}

#[derive(clap::Args, Debug)]
struct MintMessageArgs {
    /// BIP32 index of the (funded) mint. See `list-mints` /
    /// `mint-watch`.
    #[arg(long)]
    bip32_index: u32,
    /// L2 destination address (bech32m, e.g. `hc1…` / `thc1…` / `hcrt1…`).
    /// Defaults to our own address.
    #[arg(long)]
    to: Option<String>,
}

#[derive(clap::Args, Debug)]
struct TransferArgs {
    /// Destination L2 address (bech32m, e.g. `hc1…` / `thc1…` / `hcrt1…`).
    #[arg(long)]
    to: String,
    /// Amount in L2 atoms.
    #[arg(long)]
    amount: u64,
}

#[derive(clap::Args, Debug)]
struct BalanceArgs {
    /// Address to query (bech32m). Defaults to our own address.
    #[arg(long)]
    addr: Option<String>,
}

#[derive(clap::Args, Debug)]
struct LightBalanceArgs {
    /// Address to query (bech32m). Defaults to our own address.
    #[arg(long)]
    addr: Option<String>,
}

#[derive(clap::Args, Debug)]
struct ReclaimArgs {
    /// BIP32 index of the mint to reclaim.
    #[arg(long)]
    bip32_index: u32,
    /// Destination L1 address.
    #[arg(long)]
    to: String,
    /// Absolute miner fee in satoshis. Default 1000 sat — comfortable
    /// for low-feerate environments, irrelevant on regtest.
    #[arg(long, default_value = "1000")]
    fee_sat: u64,
}

#[derive(clap::Args, Debug)]
struct VerifyBalanceArgs {
    /// Address to query (bech32m). Defaults to our own address.
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
    let wallet = &cli.wallet;
    match cli.cmd {
        Cmd::Keygen(args) => cmd_keygen(wallet, args),
        Cmd::Address => cmd_address(wallet),
        Cmd::MintUtxo(args) => cmd_mint_utxo(wallet, args),
        Cmd::ListMints => cmd_list_mints(wallet),
        Cmd::MintWatch(args) => cmd_mint_watch(wallet, args).await,
        Cmd::MintMessage(args) => cmd_mint_message(wallet, args).await,
        Cmd::Transfer(args) => cmd_transfer(wallet, args).await,
        Cmd::Balance(args) => cmd_balance(wallet, args).await,
        Cmd::VerifyBalance(args) => cmd_verify_balance(wallet, args).await,
        Cmd::Head => cmd_head(wallet).await,
        Cmd::LightHead => cmd_light_head(wallet).await,
        Cmd::LightBalance(args) => cmd_light_balance(wallet, args).await,
        Cmd::ReclaimList => cmd_reclaim_list(wallet).await,
        Cmd::Reclaim(args) => cmd_reclaim(wallet, args).await,
    }
}

fn cmd_keygen(wallet_path: &std::path::Path, args: KeygenArgs) -> Result<()> {
    let network = network_from_str(&args.network)?;
    let mnemonic = match args.from_mnemonic_file {
        Some(path) => Some(
            std::fs::read_to_string(&path)
                .with_context(|| format!("read mnemonic file {}", path.display()))?,
        ),
        None => None,
    };
    let out = ops::keygen(
        wallet_path,
        ops::KeygenInput {
            network,
            sequencer_url: args.sequencer_url,
            node_url: args.node_url,
            esplora_url: args.esplora_url,
            mnemonic,
            force: args.force,
        },
    )?;
    println!("wrote {}", wallet_path.display());
    println!("L2 address: {}", out.l2_address);
    println!();
    if out.was_fresh {
        println!("BIP39 mnemonic (24 words) — back this up:");
    } else {
        println!("restored from supplied BIP39 mnemonic:");
    }
    println!("  {}", out.mnemonic);
    Ok(())
}

fn cmd_address(wallet_path: &std::path::Path) -> Result<()> {
    // Print the bech32m-encoded form so the printed value can be
    // pasted verbatim into a `--to` / `--addr` argument.
    use hodl_core::address;
    let wf = hodl_wallet::wallet::WalletFile::load(wallet_path)?;
    let addr = ops::address(wallet_path)?;
    println!("{}", address::encode(&addr, wf.network));
    Ok(())
}

fn cmd_mint_utxo(wallet_path: &std::path::Path, args: MintUtxoArgs) -> Result<()> {
    let out = ops::mint_utxo(
        wallet_path,
        ops::MintUtxoInput {
            lock_blocks: args.lock_blocks,
        },
    )?;
    println!("mint deposit ready");
    println!("  bip32_index:        {}", out.bip32_index);
    println!("  lock_blocks (CSV):  {}", out.lock_blocks);
    println!("  deposit address:    {}", out.mint_address);
    println!();
    println!("send any BTC amount to this address from your normal wallet,");
    println!("then run `mint-watch --bip32-index {}` to detect funding.", out.bip32_index);
    Ok(())
}

async fn cmd_mint_watch(wallet_path: &std::path::Path, args: MintWatchArgs) -> Result<()> {
    let out = ops::check_mint_funding(
        wallet_path,
        ops::CheckMintFundingInput { bip32_index: args.bip32_index },
    )
    .await?;
    println!("mint #{} ({}):", out.bip32_index, out.mint_address);
    match out.state {
        MintFundingState::Unfunded => {
            println!("  status: unfunded (no UTXO at this address yet)");
        }
        MintFundingState::Pending => {
            println!("  status: pending (UTXO seen but unconfirmed)");
            if let Some(op) = &out.outpoint {
                println!("  outpoint: {op}");
            }
        }
        MintFundingState::Confirmed => {
            println!("  status: CONFIRMED");
            if let Some(op) = &out.outpoint {
                println!("  outpoint:           {op}");
            }
            if let Some(v) = out.value_sat {
                println!("  value:              {v} sat");
            }
            if let Some(h) = out.funded_at_height {
                println!("  funded at height:   {h}");
            }
            println!("  → next: `mint-message --bip32-index {}`", out.bip32_index);
        }
    }
    Ok(())
}

fn cmd_list_mints(wallet_path: &std::path::Path) -> Result<()> {
    let mints = ops::list_mints(wallet_path)?;
    if mints.is_empty() {
        println!("(no recorded mints)");
        return Ok(());
    }
    for m in &mints {
        let funding = match (&m.outpoint, m.value_sat, m.funded_at_height) {
            (Some(op), Some(v), Some(h)) => format!("funded@{h} {op} v={v}sat"),
            (Some(op), Some(v), None) => format!("pending {op} v={v}sat"),
            _ => "unfunded".to_string(),
        };
        let tags = [
            if m.minted { Some("minted") } else { None },
            if m.reclaimed { Some("reclaimed") } else { None },
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(",");
        println!(
            "#{:>3} T={:>5}b  {}  [{}]  {}",
            m.bip32_index, m.lock_blocks, m.mint_address, tags, funding
        );
    }
    Ok(())
}

async fn cmd_mint_message(wallet_path: &std::path::Path, args: MintMessageArgs) -> Result<()> {
    let out = ops::mint_message(
        wallet_path,
        ops::MintMessageInput {
            bip32_index: args.bip32_index,
            to: args.to,
        },
    )
    .await?;
    if out.accepted {
        println!(
            "accepted: mint_amount={:?} nullifier={:?}",
            out.mint_amount, out.nullifier_hex
        );
        if let Some(sc) = &out.soft_conf {
            println!(
                "  soft-conf: target L2 height {}, accepted_at unix {}",
                sc.target_l2_height, sc.accepted_at_unix
            );
        }
    } else {
        println!("rejected: {}", out.error.unwrap_or_default());
    }
    Ok(())
}

async fn cmd_transfer(wallet_path: &std::path::Path, args: TransferArgs) -> Result<()> {
    let out = ops::transfer(
        wallet_path,
        ops::TransferInput {
            to: args.to,
            amount: args.amount,
        },
    )
    .await?;
    if out.accepted {
        println!("transfer accepted");
        println!("  amount: {} atoms", args.amount);
        println!("  fee:    {} atoms", out.fee);
        println!("  total:  {} atoms (deducted from sender)", out.total);
        if let Some(sc) = &out.soft_conf {
            println!(
                "  soft-conf: target L2 height {}, accepted_at unix {}",
                sc.target_l2_height, sc.accepted_at_unix
            );
        }
    } else {
        println!("rejected: {}", out.error.unwrap_or_default());
    }
    Ok(())
}

async fn cmd_balance(wallet_path: &std::path::Path, args: BalanceArgs) -> Result<()> {
    let out = ops::balance(wallet_path, ops::BalanceInput { addr: args.addr }).await?;
    println!("address: {}", out.address);
    println!("balance: {} atoms", out.balance);
    println!("nonce:   {}", out.nonce);
    Ok(())
}

async fn cmd_verify_balance(wallet_path: &std::path::Path, args: VerifyBalanceArgs) -> Result<()> {
    let against = args
        .against
        .map(|s| H256::from_hex(&s).context("parse --against hex"))
        .transpose()?;
    let out = ops::verify_balance(
        wallet_path,
        ops::VerifyBalanceInput { addr: args.addr, against },
    )
    .await?;
    println!("verified");
    println!("  address:     {}", out.address);
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

async fn cmd_light_balance(wallet_path: &std::path::Path, args: LightBalanceArgs) -> Result<()> {
    let out = ops::light_balance(wallet_path, ops::LightBalanceInput { addr: args.addr }).await?;
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
    println!("  {} {}", label, out.address);
    println!("  balance:          {} atoms", out.balance);
    println!("  nonce:            {}", out.nonce);
    Ok(())
}

async fn cmd_reclaim_list(wallet_path: &std::path::Path) -> Result<()> {
    let mints = ops::list_reclaimable_mints(wallet_path).await?;
    if mints.is_empty() {
        println!("(no recorded mints)");
        return Ok(());
    }
    for m in &mints {
        let minted_tag = if m.minted { "minted" } else { "no-mint" };
        let status = match m.status {
            ReclaimStatus::Pending => "pending funding".to_string(),
            ReclaimStatus::Locked => format!(
                "locked: {} block(s) remaining (funded @ {})",
                m.blocks_remaining.unwrap_or(0),
                m.funded_at_height.unwrap_or(0),
            ),
            ReclaimStatus::Ready => format!(
                "READY (funded @ {})",
                m.funded_at_height.unwrap_or(0)
            ),
            ReclaimStatus::Reclaimed => "reclaimed".to_string(),
        };
        let val = m.value_sat.map(|v| format!("{v}sat")).unwrap_or_else(|| "—".to_string());
        println!(
            "#{:>3} T={:>5}b v={:>10} l2={}  ⇒ {}",
            m.bip32_index, m.lock_blocks, val, minted_tag, status
        );
    }
    Ok(())
}

async fn cmd_reclaim(wallet_path: &std::path::Path, args: ReclaimArgs) -> Result<()> {
    let out = ops::reclaim_mint(
        wallet_path,
        ops::ReclaimMintInput {
            bip32_index: args.bip32_index,
            dest_address: args.to,
            fee_sat: args.fee_sat,
        },
    )
    .await?;
    println!("broadcast reclaim tx");
    println!("  txid:      {}", out.txid);
    println!("  value in:  {} sat", out.value_sat_in);
    println!("  value out: {} sat", out.value_sat_out);
    println!("  fee:       {} sat", out.fee_sat);
    Ok(())
}

