//! `#[tauri::command]` wrappers. Each operation pulls the active
//! wallet path from `AppState::current_wallet` and forwards to
//! `hodl_wallet::ops::*`. The wallet-management commands
//! (list/current/select + keygen) bypass that resolution because
//! they operate on names instead of (or before) a selection.
//!
//! anyhow errors become `String` so the frontend gets them via the
//! standard Tauri invoke-rejection path.

use crate::state::AppState;
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
    #[serde(flatten)]
    pub keygen: ops::KeygenInput,
}

#[tauri::command]
pub fn keygen(
    state: State<AppState>,
    input: GuiKeygenInput,
) -> Result<ops::KeygenOutput, String> {
    let path = err_to_string(wallets::wallet_path_for(&state.wallets_dir, &input.name))?;
    let out = err_to_string(ops::keygen(&path, input.keygen))?;
    // Make the newly-created wallet the active one — saves the user
    // a separate "select" step right after setup.
    *state.current_wallet.lock().unwrap() = Some(input.name);
    Ok(out)
}

// ---------- Address / list mints ----------

#[tauri::command]
pub fn address(state: State<AppState>) -> Result<String, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::address(&path)).map(|x| hex::encode(x.serialize()))
}

#[tauri::command]
pub fn list_mints(
    state: State<AppState>,
) -> Result<Vec<hodl_wallet::wallet::MintRecord>, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::list_mints(&path))
}

// ---------- Mints (L1 side) ----------

#[tauri::command]
pub fn mint_utxo(
    state: State<AppState>,
    input: ops::MintUtxoInput,
) -> Result<ops::MintUtxoOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::mint_utxo(&path, input))
}

#[tauri::command]
pub async fn check_mint_funding(
    state: State<'_, AppState>,
    input: ops::CheckMintFundingInput,
) -> Result<ops::CheckMintFundingOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::check_mint_funding(&path, input).await)
}

// ---------- Mint message + transfer (L2 side) ----------

#[tauri::command]
pub async fn mint_message(
    state: State<'_, AppState>,
    input: ops::MintMessageInput,
) -> Result<ops::MintMessageOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::mint_message(&path, input).await)
}

#[tauri::command]
pub async fn transfer(
    state: State<'_, AppState>,
    input: ops::TransferInput,
) -> Result<ops::TransferOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::transfer(&path, input).await)
}

// ---------- Balance / verification ----------

#[tauri::command]
pub async fn balance(
    state: State<'_, AppState>,
    input: ops::BalanceInput,
) -> Result<ops::BalanceOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::balance(&path, input).await)
}

#[tauri::command]
pub async fn verify_balance(
    state: State<'_, AppState>,
    input: ops::VerifyBalanceInput,
) -> Result<ops::VerifyBalanceOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::verify_balance(&path, input).await)
}

#[tauri::command]
pub async fn sequencer_head(
    state: State<'_, AppState>,
) -> Result<hodl_core::rpc::HeadResponse, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::sequencer_head(&path).await)
}

#[tauri::command]
pub async fn light_head(
    state: State<'_, AppState>,
) -> Result<ops::LightHeadOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::light_head(&path).await)
}

#[tauri::command]
pub async fn light_balance(
    state: State<'_, AppState>,
    input: ops::LightBalanceInput,
) -> Result<ops::LightBalanceOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::light_balance(&path, input).await)
}

// ---------- Reclaim ----------

#[tauri::command]
pub async fn list_reclaimable_mints(
    state: State<'_, AppState>,
) -> Result<Vec<ops::ReclaimableMint>, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::list_reclaimable_mints(&path).await)
}

#[tauri::command]
pub async fn reclaim_mint(
    state: State<'_, AppState>,
    input: ops::ReclaimMintInput,
) -> Result<ops::ReclaimMintOutput, String> {
    let path = err_to_string(state.resolve_current_path())?;
    err_to_string(ops::reclaim_mint(&path, input).await)
}
