//! `#[tauri::command]` wrappers. Each operation pulls the active
//! wallet path from `AppState::current_wallet`, loads the wallet, runs
//! the requested `hodl_wallet::ops::*` against the parsed `WalletFile`,
//! and (for mutating ops) saves it back. The ops layer no longer
//! touches disk — all load/save plumbing lives here. The
//! wallet-management commands (list/current/select + keygen) bypass
//! the current-wallet resolution because they operate on names
//! instead of (or before) a selection.
//!
//! anyhow errors become `String` so the frontend gets them via the
//! standard Tauri invoke-rejection path.

use crate::state::AppState;
use hodl_core::address;
use hodl_wallet::{ops, wallets};
use serde::Deserialize;
use tauri::State;

fn err_to_string<T>(r: anyhow::Result<T>) -> Result<T, String> {
    r.map_err(|e| format!("{e:#}"))
}

// ---------- Wallet management ----------

#[tauri::command]
pub fn list_wallets(state: State<AppState>) -> Result<Vec<String>, String> {
    err_to_string(wallets::list_wallet_files(&state.wallets_dir))
}

#[tauri::command]
pub fn current_wallet(state: State<AppState>) -> Option<String> {
    state.current_wallet.lock().unwrap().clone()
}

#[tauri::command]
pub fn select_wallet(state: State<AppState>, name: String) -> Result<(), String> {
    let path = err_to_string(wallets::wallet_path_for(&state.wallets_dir, &name))?;
    if !path.exists() {
        return Err(format!("no wallet named {name:?} at {}", path.display()));
    }
    *state.current_wallet.lock().unwrap() = Some(name);
    Ok(())
}

/// Forget the current selection. Used when the user picks "switch
/// wallet" in the dashboard to return to the picker.
#[tauri::command]
pub fn deselect_wallet(state: State<AppState>) {
    *state.current_wallet.lock().unwrap() = None;
}

// ---------- Keygen (creates a new named wallet) ----------

#[derive(Debug, Deserialize)]
pub struct GuiKeygenInput {
    /// Name to give the new wallet (`<name>.json` in the wallets dir).
    pub name: String,
    /// Overwrite an existing file with the same name. Mirrors the
    /// CLI's `--force` flag; without it we refuse to clobber.
    #[serde(default)]
    pub force: bool,
    #[serde(flatten)]
    pub keygen: ops::KeygenInput,
}

#[tauri::command]
pub fn keygen(
    state: State<AppState>,
    input: GuiKeygenInput,
) -> Result<ops::KeygenOutput, String> {
    let path = err_to_string(wallets::wallet_path_for(&state.wallets_dir, &input.name))?;
    if path.exists() && !input.force {
        return Err(format!(
            "wallet file {} already exists",
            path.display()
        ));
    }
    let (wf, out) = err_to_string(ops::keygen(input.keygen))?;
    err_to_string(wf.save(&path))?;
    // Make the newly-created wallet the active one — saves the user
    // a separate "select" step right after setup.
    *state.current_wallet.lock().unwrap() = Some(input.name);
    Ok(out)
}

// ---------- Address / list mints ----------

#[tauri::command]
pub fn address(state: State<AppState>) -> Result<String, String> {
    let wf = err_to_string(state.load_current())?;
    let pk = err_to_string(ops::address(&wf))?;
    Ok(address::encode(&pk, wf.network))
}

#[tauri::command]
pub fn list_mints(
    state: State<AppState>,
) -> Result<Vec<hodl_wallet::wallet::MintRecord>, String> {
    let wf = err_to_string(state.load_current())?;
    Ok(ops::list_mints(&wf))
}

#[tauri::command]
pub fn list_transactions(
    state: State<AppState>,
) -> Result<Vec<hodl_wallet::wallet::TxRecord>, String> {
    let wf = err_to_string(state.load_current())?;
    Ok(ops::list_transactions(&wf))
}

// ---------- Mints (L1 side) ----------

#[tauri::command]
pub fn mint_utxo(
    state: State<AppState>,
    input: ops::MintUtxoInput,
) -> Result<ops::MintUtxoOutput, String> {
    let mut wf = err_to_string(state.load_current())?;
    let out = err_to_string(ops::mint_utxo(&mut wf, input))?;
    err_to_string(state.save_current(&wf))?;
    Ok(out)
}

#[tauri::command]
pub async fn check_mint_funding(
    state: State<'_, AppState>,
    input: ops::CheckMintFundingInput,
) -> Result<ops::CheckMintFundingOutput, String> {
    let mut wf = err_to_string(state.load_current())?;
    let out = err_to_string(ops::check_mint_funding(&mut wf, input).await)?;
    err_to_string(state.save_current(&wf))?;
    Ok(out)
}

// ---------- Mint message + transfer (L2 side) ----------

#[tauri::command]
pub async fn mint_message(
    state: State<'_, AppState>,
    input: ops::MintMessageInput,
) -> Result<ops::MintMessageOutput, String> {
    let mut wf = err_to_string(state.load_current())?;
    let out = err_to_string(ops::mint_message(&mut wf, input).await)?;
    err_to_string(state.save_current(&wf))?;
    Ok(out)
}

#[tauri::command]
pub async fn transfer(
    state: State<'_, AppState>,
    input: ops::TransferInput,
) -> Result<ops::TransferOutput, String> {
    let mut wf = err_to_string(state.load_current())?;
    let out = err_to_string(ops::transfer(&mut wf, input).await)?;
    err_to_string(state.save_current(&wf))?;
    Ok(out)
}

// ---------- Balance / verification ----------

#[tauri::command]
pub async fn balance(
    state: State<'_, AppState>,
    input: ops::BalanceInput,
) -> Result<ops::BalanceOutput, String> {
    let wf = err_to_string(state.load_current())?;
    err_to_string(ops::balance(&wf, input).await)
}

#[tauri::command]
pub async fn verify_balance(
    state: State<'_, AppState>,
    input: ops::VerifyBalanceInput,
) -> Result<ops::VerifyBalanceOutput, String> {
    let wf = err_to_string(state.load_current())?;
    err_to_string(ops::verify_balance(&wf, input).await)
}

#[tauri::command]
pub async fn sequencer_head(
    state: State<'_, AppState>,
) -> Result<hodl_core::rpc::HeadResponse, String> {
    let wf = err_to_string(state.load_current())?;
    err_to_string(ops::sequencer_head(&wf).await)
}

#[tauri::command]
pub async fn light_head(
    state: State<'_, AppState>,
) -> Result<ops::LightHeadOutput, String> {
    let wf = err_to_string(state.load_current())?;
    err_to_string(ops::light_head(&wf).await)
}

#[tauri::command]
pub async fn light_balance(
    state: State<'_, AppState>,
    input: ops::LightBalanceInput,
) -> Result<ops::LightBalanceOutput, String> {
    let mut wf = err_to_string(state.load_current())?;
    let out = err_to_string(ops::light_balance(&mut wf, input).await)?;
    err_to_string(state.save_current(&wf))?;
    Ok(out)
}

// ---------- Reclaim ----------

#[tauri::command]
pub async fn list_reclaimable_mints(
    state: State<'_, AppState>,
) -> Result<Vec<ops::ReclaimableMint>, String> {
    let wf = err_to_string(state.load_current())?;
    err_to_string(ops::list_reclaimable_mints(&wf).await)
}

#[tauri::command]
pub async fn reclaim_mint(
    state: State<'_, AppState>,
    input: ops::ReclaimMintInput,
) -> Result<ops::ReclaimMintOutput, String> {
    let mut wf = err_to_string(state.load_current())?;
    let out = err_to_string(ops::reclaim_mint(&mut wf, input).await)?;
    err_to_string(state.save_current(&wf))?;
    Ok(out)
}
