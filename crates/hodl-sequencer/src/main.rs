//! hodl-sequencer daemon entry point.

mod api;
mod bitcoind;
mod config;
mod producer;
mod shared;
mod store;

use anyhow::{Context, Result};
use clap::Parser;
use hodl_core::block::genesis;
use hodl_core::hash::H256;
use hodl_core::state::LedgerState;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::api::AppState;
use crate::bitcoind::SequencerL1;
use crate::config::{write_example, SequencerConfig};
use crate::producer::Producer;
use crate::shared::{HeadInfo, Shared};
use crate::store::Store;

#[derive(Parser, Debug)]
#[command(name = "hodl-sequencer", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(clap::Subcommand, Debug)]
enum Cmd {
    /// Run the sequencer.
    Run {
        #[arg(long, default_value = "./hodl-sequencer.json")]
        config: PathBuf,
    },
    /// Write an example config file.
    InitConfig {
        #[arg(long, default_value = "./hodl-sequencer.json")]
        path: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "hodl_sequencer=info,info".into()),
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
    let cfg = SequencerConfig::load(config_path)
        .with_context(|| format!("load config {}", config_path.display()))?;

    tracing::info!(?cfg.listen, db = %cfg.db_path.display(), "starting sequencer");

    let l1 = Arc::new(SequencerL1::connect(&cfg)?);
    let store = Arc::new(Mutex::new(Store::open(&cfg.db_path)?));

    let (state, head) = bootstrap(&cfg, &l1, &store).await?;
    let shared = Arc::new(Shared::new(state, head));

    let producer = Producer {
        shared: shared.clone(),
        store: store.clone(),
        l1: l1.clone(),
        poll_interval: cfg.poll_interval(),
    };
    tokio::spawn(producer.run());

    let app_state = AppState { shared: shared.clone(), store: store.clone(), l1: l1.clone() };
    let app = api::router(app_state);

    let listener = tokio::net::TcpListener::bind(cfg.listen).await
        .with_context(|| format!("bind {}", cfg.listen))?;
    tracing::info!(addr = %cfg.listen, "http listening");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn bootstrap(
    cfg: &SequencerConfig,
    l1: &Arc<SequencerL1>,
    store: &Arc<Mutex<Store>>,
) -> Result<(LedgerState, HeadInfo)> {
    let snapshot = { store.lock().unwrap().load_latest_state()? };
    if let Some((height, state)) = snapshot {
        let store_guard = store.lock().unwrap();
        let l1_cursor = store_guard.l1_cursor()?.unwrap_or(0);
        let block = store_guard
            .get_block(height)?
            .ok_or_else(|| anyhow::anyhow!("inconsistent: state snapshot at h={height} without block"))?;
        let head = HeadInfo {
            height: block.header.height,
            block_hash: block.hash(),
            state_root: block.header.state_root,
            l1_height: block.header.l1_height,
            l1_block_hash: block.header.l1_block_hash,
        };
        drop(store_guard);
        tracing::info!(l2_height = head.height, l1_cursor, "resumed from snapshot");
        Ok((state, head))
    } else {
        // Bootstrap: wait until L1 reaches `l1_genesis_height`, then anchor.
        let target = cfg.l1_genesis_height;
        let mut tip = {
            let l1c = l1.clone();
            tokio::task::spawn_blocking(move || l1c.block_count()).await??
        };
        while tip < target {
            tracing::info!(tip, target, "waiting for L1 to reach genesis height");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let l1c = l1.clone();
            tip = tokio::task::spawn_blocking(move || l1c.block_count()).await??;
        }
        let l1c = l1.clone();
        let block_hash_hex = tokio::task::spawn_blocking(move || l1c.block_hash_hex(target)).await??;
        let l1_block_hash = parse_l1_block_hash(&block_hash_hex)?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?.as_secs();

        // Root the attestation chain: pick the wallet's largest UTXO.
        let l1c = l1.clone();
        let anchor_0 = tokio::task::spawn_blocking(move || l1c.pick_initial_anchor()).await??;
        tracing::info!(anchor = %format_args!("{}:{}", anchor_0.txid, anchor_0.vout), "selected anchor_0");

        let block = genesis(l1_block_hash, target, now, anchor_0);
        let state = LedgerState::new();
        // Genesis carries no txs; its witness is trivially empty.
        let witness = hodl_core::witness::BlockWitness::build(&state, &block.txs, 0);
        {
            let mut store_guard = store.lock().unwrap();
            store_guard.write_block_and_state(&block, &state, &witness)?;
            store_guard.set_l1_cursor(target)?;
            store_guard.set_anchor(&anchor_0)?;
        }
        let head = HeadInfo {
            height: 0,
            block_hash: block.hash(),
            state_root: block.header.state_root,
            l1_height: target,
            l1_block_hash,
        };
        tracing::info!(l1_height = target, "initialised L2 genesis");
        Ok((state, head))
    }
}

fn parse_l1_block_hash(s: &str) -> Result<H256> {
    let bytes = hex::decode(s)?;
    if bytes.len() != 32 {
        anyhow::bail!("bad l1 block hash length: {}", bytes.len());
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(H256(out))
}
