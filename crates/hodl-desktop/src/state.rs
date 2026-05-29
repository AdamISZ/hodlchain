use anyhow::{anyhow, bail, Context, Result};
use hodl_wallet::encryption::UnlockContext;
use hodl_wallet::wallet::WalletFile;
use hodl_wallet::wallets;
use std::collections::HashMap;
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
///
/// `unlocked` caches the per-wallet KDF-derived key after the user
/// has typed their passphrase once. While an entry sits here, the
/// corresponding encrypted wallet can be loaded + saved with no
/// further prompt. The entry is cleared when the user deselects the
/// wallet ("switch wallet"). See plan H3.
pub struct AppState {
    pub wallets_dir: PathBuf,
    pub current_wallet: Mutex<Option<String>>,
    pub unlocked: Mutex<HashMap<String, UnlockContext>>,
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
            unlocked: Mutex::new(HashMap::new()),
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

    /// Load the currently-selected wallet from disk. For an encrypted
    /// wallet, the unlock context cached by `select_wallet` is used to
    /// decrypt without re-prompting; a missing cache entry produces a
    /// clear "locked" error.
    pub fn load_current(&self) -> Result<WalletFile> {
        let path = self.resolve_current_path()?;
        if WalletFile::is_encrypted_at(&path)? {
            // We hold this lock for the duration of the load; ops are
            // millisecond-scale and there's no contention since IPC
            // commands run serially.
            let unlocked = self.unlocked.lock().unwrap();
            let guard = self.current_wallet.lock().unwrap();
            let name = guard
                .as_ref()
                .expect("path resolved => current_wallet is Some");
            let ctx = unlocked.get(name).ok_or_else(|| {
                anyhow!(
                    "wallet is encrypted and locked — return to the picker and \
                     re-enter the passphrase"
                )
            })?;
            WalletFile::load_with_context(&path, ctx)
        } else {
            WalletFile::load(&path)
        }
    }

    /// Persist a wallet back to the currently-selected slot. The
    /// `WalletFile`'s own `encryption_ctx` decides v1 vs v2 — for an
    /// encrypted wallet that's the same context the AppState cached,
    /// so the on-disk format matches what was loaded.
    pub fn save_current(&self, wf: &WalletFile) -> Result<()> {
        let path = self.resolve_current_path()?;
        wf.save(&path)
    }

    /// Select a wallet by name, decrypting it if necessary. `passphrase`
    /// must be `Some` for encrypted wallets and is ignored for plain
    /// ones. On success the wallet becomes the active one and (for
    /// encrypted wallets) its unlock context is cached.
    pub fn select(&self, name: String, passphrase: Option<&str>) -> Result<()> {
        let path = wallets::wallet_path_for(&self.wallets_dir, &name)?;
        if !path.exists() {
            bail!("no wallet named {name:?} at {}", path.display());
        }
        if WalletFile::is_encrypted_at(&path)? {
            let pp = passphrase.ok_or_else(|| {
                anyhow!("wallet is encrypted; passphrase required")
            })?;
            let mut wf = WalletFile::load_unlocked(&path, Some(pp))?;
            let ctx = wf
                .take_encryption_ctx()
                .expect("encrypted wallet load => Some(ctx)");
            self.unlocked.lock().unwrap().insert(name.clone(), ctx);
        }
        *self.current_wallet.lock().unwrap() = Some(name);
        Ok(())
    }

    /// Clear the active selection and (if the wallet was encrypted)
    /// evict its cached unlock context. The user has to re-enter the
    /// passphrase to select the same wallet again.
    pub fn deselect(&self) {
        let mut current = self.current_wallet.lock().unwrap();
        if let Some(n) = current.as_ref() {
            self.unlocked.lock().unwrap().remove(n);
        }
        *current = None;
    }
}
