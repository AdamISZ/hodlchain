use anyhow::{anyhow, Context, Result};
use hodl_wallet::wallets;
use std::path::PathBuf;
use std::sync::Mutex;

/// Tauri-managed application state.
///
/// `wallets_dir` is resolved once at startup from the OS config
/// directory (Linux `$XDG_CONFIG_HOME/hodlchain/wallets`, etc.) and
/// stays fixed for the life of the process.
///
/// `current_wallet` is the *name* of the active wallet (without
/// `.json`). It's mutable — the user picks one at startup or via the
/// switch-wallet flow. All non-wallet-management Tauri commands
/// (keygen, light_balance, …) resolve their wallet path through this
/// field and error with a clear message if it's None.
pub struct AppState {
    pub wallets_dir: PathBuf,
    pub current_wallet: Mutex<Option<String>>,
}

impl AppState {
    pub fn init() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow!("could not resolve user config directory"))?;
        let hodl_dir = config_dir.join("hodlchain");
        std::fs::create_dir_all(&hodl_dir).with_context(|| {
            format!("create config dir at {}", hodl_dir.display())
        })?;
        let wallets_dir = hodl_dir.join("wallets");

        // One-shot migration: if the legacy pre-picker
        // `~/.config/hodlchain/wallet.json` exists and the new
        // wallets/ dir is empty, move it to wallets/default.json so
        // it's visible in the picker.
        let legacy = hodl_dir.join("wallet.json");
        wallets::migrate_legacy_wallet(&legacy, &wallets_dir)?;
        wallets::ensure_wallets_dir(&wallets_dir)?;

        Ok(Self {
            wallets_dir,
            current_wallet: Mutex::new(None),
        })
    }

    /// Resolve the active wallet's on-disk path, or return a
    /// frontend-facing error if no wallet is selected.
    pub fn resolve_current_path(&self) -> Result<PathBuf> {
        let guard = self.current_wallet.lock().unwrap();
        let name = guard
            .as_ref()
            .ok_or_else(|| anyhow!("no wallet selected — pick or create one first"))?;
        wallets::wallet_path_for(&self.wallets_dir, name)
    }
}
