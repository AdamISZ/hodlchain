//! hodl-node daemon entry point.

mod api;
mod bitcoind;
mod config;
mod follower;
mod seq_client;
mod shared;
mod store;

use anyhow::{anyhow, bail, Context, Result};
use clap::Parser;
use hodl_core::state::LedgerState;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::api::AppState;
use crate::bitcoind::NodeL1;
use crate::config::{write_example, NodeConfig};
use crate::follower::Follower;
use crate::seq_client::SeqClient;
use crate::shared::{HeadInfo, Shared};
use crate::store::Store;

#[derive(Parser, Debug)]
#[command(name = "hodl-node", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// Run the follower + HTTP server.
    Run {
        #[arg(long, default_value = "./hodl-node.json")]
        config: PathBuf,
    },
    /// Write an example config file.
    InitConfig {
        #[arg(long, default_value = "./hodl-node.json")]
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hodl_node=info,info".into()),
        )
        .init();

    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Run { config } => run(&config).await,
        Cmd::InitConfig { path } => {
            write_example(&path)?;
            println!("wrote {}", path.display());
            Ok(())
        }
    }
}

async fn run(config_path: &std::path::Path) -> Result<()> {
    let cfg = NodeConfig::load(config_path)
        .with_context(|| format!("load config {}", config_path.display()))?;

    tracing::info!(
        ?cfg.listen,
        sequencer = %cfg.sequencer_url,
        db = %cfg.db_path.display(),
        "starting node"
    );

    let l1 = Arc::new(NodeL1::connect(&cfg)?);
    let store = Arc::new(Mutex::new(Store::open(&cfg.db_path)?));
    let seq = Arc::new(SeqClient::new(cfg.sequencer_url.clone()));

    let (state, head) = bootstrap(&cfg, &store, &seq).await?;
    let shared = Arc::new(Shared::new(state, head));

    let follower = Follower {
        shared: shared.clone(),
        store: store.clone(),
        l1: l1.clone(),
        seq: seq.clone(),
        poll_interval: cfg.poll_interval(),
    };
    tokio::spawn(follower.run());

    let app_state = AppState { shared: shared.clone(), store: store.clone(), l1: l1.clone() };
    let app = api::router(app_state);

    let listener = tokio::net::TcpListener::bind(cfg.listen).await
        .with_context(|| format!("bind {}", cfg.listen))?;
    tracing::info!(addr = %cfg.listen, "http listening");
    axum::serve(listener, app).await?;
    Ok(())
}

/// On first run, fetch L2 genesis from the sequencer and accept it after
/// checking its declared `l1_height` agrees with our configured genesis.
/// On subsequent runs, resume from the latest persisted state snapshot.
async fn bootstrap(
    cfg: &NodeConfig,
    store: &Arc<Mutex<Store>>,
    seq: &Arc<SeqClient>,
) -> Result<(LedgerState, HeadInfo)> {
    let snapshot = { store.lock().unwrap().load_latest_state()? };
    if let Some((height, state)) = snapshot {
        let store_guard = store.lock().unwrap();
        let block = store_guard
            .get_block(height)?
            .ok_or_else(|| anyhow!("inconsistent: state snapshot at h={height} without block"))?;
        let head = HeadInfo {
            height: block.header.height,
            block_hash: block.hash(),
            state_root: block.header.state_root,
            l1_height: block.header.l1_height,
        };
        let l1_cursor = store_guard.l1_cursor()?.unwrap_or(0);
        tracing::info!(l2_height = head.height, l1_cursor, "resumed from snapshot");
        return Ok((state, head));
    }

    // Cold start: pull L2 genesis (height 0) from the sequencer.
    tracing::info!("cold start; fetching L2 genesis from sequencer");
    let genesis = seq.get_block(0).await
        .context("fetch L2 genesis from sequencer; is it running?")?;
    if genesis.header.height != 0 {
        bail!("sequencer returned non-genesis block as height 0");
    }
    if genesis.header.l1_height != cfg.l1_genesis_height {
        bail!(
            "L2 genesis L1 anchor mismatch: sequencer says L1={}, our config says L1={}",
            genesis.header.l1_height,
            cfg.l1_genesis_height
        );
    }
    // Seed the chain-wide fee destination from the genesis header
    // (set by the sequencer at chain init; immutable thereafter).
    // Required before computing state_root since the v3 state_root
    // commits to the fee address.
    let mut state = LedgerState::new();
    state.sequencer_fee_address = genesis.header.sequencer_fee_address;
    if state.state_root() != genesis.header.state_root {
        bail!(
            "L2 genesis state root mismatch: computed {} != genesis header {} \
             (sequencer_fee_address in header: {:?})",
            state.state_root(),
            genesis.header.state_root,
            genesis.header.sequencer_fee_address,
        );
    }

    let anchor_outpoint = genesis
        .header
        .anchor_outpoint
        .ok_or_else(|| anyhow!("L2 genesis header has no anchor_outpoint"))?;
    tracing::info!(
        anchor = %format_args!("{}:{}", anchor_outpoint.txid, anchor_outpoint.vout),
        "adopted anchor_0 from L2 genesis"
    );

    {
        let mut s = store.lock().unwrap();
        // Genesis carries no txs and no state transition; its witness
        // is trivially empty.
        let witness = hodl_core::witness::BlockWitness::build(&state, &genesis.txs, 0);
        s.write_block_and_state(&genesis, &state, &witness)?;
        // Start scanning from the genesis L1 height; the first real
        // attestation tx can only land at L1 height >= genesis + 1.
        s.set_l1_cursor(cfg.l1_genesis_height)?;
        s.set_anchor(&anchor_outpoint)?;
    }

    let head = HeadInfo {
        height: 0,
        block_hash: genesis.hash(),
        state_root: genesis.header.state_root,
        l1_height: genesis.header.l1_height,
    };
    tracing::info!(l1_height = head.l1_height, "initialised L2 genesis from sequencer");
    Ok((state, head))
}
