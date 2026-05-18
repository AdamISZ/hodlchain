use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;

/// Tauri-managed application state.
pub struct AppState {
    /// Resolved on-disk wallet path. Linux:
    /// `$XDG_CONFIG_HOME/hodlcoin/wallet.json` (or
    /// `~/.config/hodlcoin/wallet.json` if `XDG_CONFIG_HOME` is unset).
    /// macOS: `~/Library/Application Support/hodlcoin/wallet.json`.
    /// Windows: `%APPDATA%/hodlcoin/wallet.json`.
    ///
    /// Override-via-env is intentionally not supported here: a
    /// production desktop app's wallet location is part of its
    /// stable contract with the user. CLI users have `--wallet`.
    pub wallet_path: PathBuf,
}

impl AppState {
    pub fn init() -> Result<Self> {
        let config_dir = dirs::config_dir()
            .ok_or_else(|| anyhow!("could not resolve user config directory"))?;
        let wallet_dir = config_dir.join("hodlcoin");
        std::fs::create_dir_all(&wallet_dir).with_context(|| {
            format!("create wallet config dir at {}", wallet_dir.display())
        })?;
        Ok(Self {
            wallet_path: wallet_dir.join("wallet.json"),
        })
    }
}
