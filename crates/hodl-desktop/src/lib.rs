//! hodlchain desktop app (Tauri v2 + Svelte 5 + TypeScript).
//!
//! The Rust side is intentionally tiny. All wallet business logic
//! lives in `hodl_wallet::ops`; everything here is glue:
//!
//! - `state::AppState` resolves and holds the wallet-file path
//!   (`$XDG_CONFIG_HOME/hodlchain/wallet.json` on Linux, equivalents
//!   on macOS / Windows via the `dirs` crate). The frontend never
//!   sees a `PathBuf` — that's session context, not a per-call arg.
//! - `commands` wraps each `ops::*` function in a
//!   `#[tauri::command]` that pulls `wallet_path` from `AppState` and
//!   passes the user-provided `*Input` through unchanged.
//!
//! Adding a new wallet operation = adding a new `ops::*` function +
//! a 3-5 line wrapper in `commands`. Nothing more.

mod commands;
mod state;

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("hodl_desktop_lib=info")),
        )
        .init();

    let app_state = state::AppState::init().expect("init AppState");
    tracing::info!(
        wallets_dir = %app_state.wallets_dir.display(),
        "starting hodlchain desktop"
    );

    tauri::Builder::default()
        .manage(app_state)
        .invoke_handler(tauri::generate_handler![
            commands::list_wallets,
            commands::current_wallet,
            commands::is_wallet_encrypted,
            commands::select_wallet,
            commands::deselect_wallet,
            commands::keygen,
            commands::address,
            commands::list_mints,
            commands::list_transactions,
            commands::mint_utxo,
            commands::check_mint_funding,
            commands::mint_message,
            commands::transfer,
            commands::balance,
            commands::verify_balance,
            commands::sequencer_head,
            commands::light_head,
            commands::light_balance,
            commands::list_reclaimable_mints,
            commands::reclaim_mint,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
