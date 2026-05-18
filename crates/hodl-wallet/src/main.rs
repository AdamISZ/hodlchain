//! hodl-wallet — CLI wallet for the hodlcoin POC.
//!
//! See `docs/design.md` for the L1 mint UTXO format and protocol invariants.

mod api;
mod bitcoind;
mod esplora;
mod verify;
mod wallet;

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::secp256k1::{rand, Message, Secp256k1, XOnlyPublicKey};
use bitcoin::{Amount, OutPoint};
use clap::{Parser, Subcommand};
use hodl_core::hash::H256;
use hodl_core::l1::{derive_mint_taproot, mint_address};
use hodl_core::proof::OutpointProof;
use hodl_core::proof::MintProofEnvelope;
use hodl_core::smt::LeafKind;
use hodl_core::state::LedgerState;
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
    /// Optional 32-byte hex state_root to compare against. If supplied,
    /// the verification also checks that the response's state_root
    /// equals this value — the binding to L1. In Phase 3, the
    /// light-client mode reads this off the L1 attestation chain.
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
        Cmd::Keygen(args) => cmd_keygen(&cli.wallet, args),
        Cmd::Address => cmd_address(&cli.wallet),
        Cmd::MintUtxo(args) => cmd_mint_utxo(&cli.wallet, args),
        Cmd::ListMints => cmd_list_mints(&cli.wallet),
        Cmd::MintMessage(args) => cmd_mint_message(&cli.wallet, args).await,
        Cmd::Transfer(args) => cmd_transfer(&cli.wallet, args).await,
        Cmd::Balance(args) => cmd_balance(&cli.wallet, args).await,
        Cmd::VerifyBalance(args) => cmd_verify_balance(&cli.wallet, args).await,
        Cmd::Head => cmd_head(&cli.wallet).await,
        Cmd::LightHead => cmd_light_head(&cli.wallet).await,
        Cmd::LightBalance(args) => cmd_light_balance(&cli.wallet, args).await,
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
        esplora_url: args.esplora_url,
        mints: Vec::new(),
        verified_head: None,
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

async fn cmd_verify_balance(
    path: &std::path::Path,
    args: VerifyBalanceArgs,
) -> Result<()> {
    let wf = WalletFile::load(path)?;
    let secp = Secp256k1::new();
    let target = match args.addr {
        Some(s) => parse_xonly(&s)?,
        None => wf.xonly_pubkey(&secp)?,
    };
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let bal = api.balance(&target).await?;

    // 1. Self-consistency: the response's state_components must hash to
    //    the state_root the response also reports. A sloppy server
    //    sending mismatched values is caught here.
    let derived = bal.state_components.state_root();
    if derived != bal.state_root {
        bail!(
            "response self-inconsistent: components.state_root()={} != reported state_root={}",
            derived,
            bal.state_root
        );
    }

    // 2. Optional binding to an externally-supplied state_root (the L1
    //    chain walk). When this matches, the proof's relationship to
    //    L1 is established and the server is fully checked.
    if let Some(s) = args.against.as_ref() {
        let expected =
            H256::from_hex(s).context("parse --against hex")?;
        if expected != bal.state_root {
            bail!(
                "state_root mismatch: response says {}, --against says {}",
                bal.state_root,
                expected
            );
        }
    }

    // 3. SMT proof verifies against the accounts_root.
    if bal.proof.address != bal.address {
        bail!(
            "proof address {} disagrees with response address {}",
            hex::encode(bal.proof.address.serialize()),
            hex::encode(bal.address.serialize())
        );
    }
    if !bal.proof.verify(bal.state_components.accounts_root) {
        bail!("inclusion proof does not verify against accounts_root");
    }

    // 4. Proof leaf payload must equal the reported balance/nonce.
    match &bal.proof.leaf {
        LeafKind::Account { balance, nonce } => {
            if *balance != bal.balance {
                bail!(
                    "proof leaf balance {} disagrees with reported balance {}",
                    balance, bal.balance
                );
            }
            if *nonce != bal.nonce {
                bail!(
                    "proof leaf nonce {} disagrees with reported nonce {}",
                    nonce, bal.nonce
                );
            }
        }
        LeafKind::Empty => {
            if bal.balance != 0 || bal.nonce != 0 {
                bail!(
                    "proof claims no-such-account but reported balance/nonce are non-zero ({}, {})",
                    bal.balance, bal.nonce
                );
            }
        }
    }

    println!("verified");
    println!("  address:     {}", hex::encode(target.serialize()));
    println!("  balance:     {} atoms", bal.balance);
    println!("  nonce:       {}", bal.nonce);
    println!("  l2_height:   {}", bal.l2_height);
    println!("  state_root:  {}", bal.state_root);
    if args.against.is_some() {
        println!("  ⇒ state_root matches --against value (bound to L1)");
    } else {
        println!("  ⇒ state_root not checked against an external source");
        println!("    (pass --against <hex> to bind to L1 in light-client mode)");
    }
    Ok(())
}

async fn cmd_head(path: &std::path::Path) -> Result<()> {
    let wf = WalletFile::load(path)?;
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let head = api.sequencer_head().await?;
    println!("{}", serde_json::to_string_pretty(&head)?);
    Ok(())
}

/// Pull L2 genesis, extract anchor_0, walk the L1 attestation chain via
/// the configured Esplora endpoint. Returns the latest state_root
/// (genesis's empty-state root if no attestations exist yet).
async fn derive_state_root_from_l1(
    wf: &WalletFile,
) -> Result<(H256, u32, usize)> {
    let esplora_url = wf
        .esplora_url
        .as_ref()
        .ok_or_else(|| anyhow!("wallet has no esplora_url configured; pass --esplora-url to keygen"))?;

    // Bootstrap: fetch L2 genesis from the L2 RPC for anchor_0.
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let genesis = api
        .get_block(0)
        .await
        .context("fetch L2 genesis (height 0) for anchor_0 bootstrap")?;
    let anchor_0 = genesis
        .header
        .anchor_outpoint
        .ok_or_else(|| anyhow!("L2 genesis header has no anchor_outpoint"))?;

    // Walk forward via Esplora.
    let esplora = esplora::EsploraClient::new(esplora_url.clone());
    let chain = esplora::walk_attestation_chain(&esplora, anchor_0).await?;
    if chain.is_empty() {
        // No attestations yet — the head is genesis. State_root is the
        // empty-LedgerState root.
        return Ok((LedgerState::new().state_root(), 0, 0));
    }
    let last = chain.last().unwrap();
    Ok((last.attestation.state_root, last.attestation.height, chain.len()))
}

async fn cmd_light_head(path: &std::path::Path) -> Result<()> {
    let wf = WalletFile::load(path)?;
    let (state_root, l2_height, walked) = derive_state_root_from_l1(&wf).await?;
    println!("L2 head (derived from L1 attestation chain via Esplora):");
    println!("  l2_height:  {}", l2_height);
    println!("  state_root: {}", state_root);
    println!("  walked {} attestation(s) from anchor_0", walked);
    Ok(())
}

async fn cmd_light_balance(
    path: &std::path::Path,
    args: LightBalanceArgs,
) -> Result<()> {
    let mut wf = WalletFile::load(path)?;
    let secp = Secp256k1::new();
    let own_addr = wf.xonly_pubkey(&secp)?;
    let target = match &args.addr {
        Some(s) => parse_xonly(s)?,
        None => own_addr,
    };

    let esplora_url = wf
        .esplora_url
        .as_ref()
        .ok_or_else(|| {
            anyhow!("wallet has no esplora_url configured; pass --esplora-url to keygen")
        })?
        .clone();
    let esplora = esplora::EsploraClient::new(esplora_url);
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());

    // Sanity-check any persisted head matches our current key.
    if let Some(h) = &wf.verified_head {
        if h.own_address != own_addr {
            bail!(
                "persisted verified_head tracks a different address ({}); \
                 wallet key may have changed. Delete verified_head from the \
                 wallet file to reset.",
                hex::encode(h.own_address.serialize())
            );
        }
    }

    let (head, mode, blocks_verified) = match wf.verified_head.take() {
        None => {
            println!("cold-start: bootstrapping verified head from node + L1 chain...");
            let h = verify::bootstrap(&api, &esplora, own_addr).await?;
            println!("  bootstrapped at l2_height={} state_root={}", h.l2_height, h.state_root);
            // Then a walk-forward — any blocks the node had after the
            // bootstrap snapshot get verified statelessly.
            let (h, n) = verify::walk_forward(h, &api, &esplora).await?;
            (h, "cold-start", n)
        }
        Some(h) => {
            let (h, n) = verify::walk_forward(h, &api, &esplora).await?;
            (h, "warm-start", n)
        }
    };

    println!("verified ({mode}, {blocks_verified} new block(s))");
    println!("  l2_height:        {}", head.l2_height);
    println!("  state_root:       {}", head.state_root);
    println!("  accounts_root:    {}", head.accounts_root);
    println!("  block_hash:       {}", head.block_hash);
    println!("  l1_height:        {}", head.l1_height);

    if target == own_addr {
        let (balance, nonce) = verify::balance_from(&head, &own_addr);
        println!("  address (own):    {}", hex::encode(own_addr.serialize()));
        println!("  balance:          {} atoms", balance);
        println!("  nonce:            {}", nonce);
    } else {
        // Query the node for the other address; verify the inclusion
        // proof against our locally-verified accounts_root.
        let bal = api.balance(&target).await?;
        if bal.state_root != head.state_root {
            bail!(
                "node-reported state_root {} disagrees with verified head {}; \
                 retry to let the node catch up",
                bal.state_root,
                head.state_root
            );
        }
        if !bal.proof.verify(head.accounts_root) {
            bail!("inclusion proof for {} does not verify against verified accounts_root",
                  hex::encode(target.serialize()));
        }
        let (balance, nonce) = match bal.proof.leaf {
            LeafKind::Account { balance, nonce } => (balance, nonce),
            LeafKind::Empty => (0, 0),
        };
        if balance != bal.balance || nonce != bal.nonce {
            bail!("inclusion-proof leaf disagrees with reported balance/nonce");
        }
        println!("  address:          {}", hex::encode(target.serialize()));
        println!("  balance:          {} atoms", balance);
        println!("  nonce:            {}", nonce);
    }

    // Persist the verified head back to the wallet file.
    wf.verified_head = Some(head);
    wf.save(path)?;
    Ok(())
}

fn parse_xonly(s: &str) -> Result<XOnlyPublicKey> {
    let bytes = hex::decode(s).context("decode pubkey hex")?;
    Ok(XOnlyPublicKey::from_slice(&bytes).context("parse x-only pubkey")?)
}
