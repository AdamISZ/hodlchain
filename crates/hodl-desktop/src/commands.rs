//! `#[tauri::command]` wrappers. Each one pulls `wallet_path` from
//! `AppState` and forwards to `hodl_wallet::ops::*`. anyhow errors
//! become `String` so the frontend gets them via the standard Tauri
//! invoke-rejection path.

use crate::state::AppState;
use hodl_wallet::ops;
use tauri::State;

fn err_to_string<T>(r: anyhow::Result<T>) -> Result<T, String> {
    r.map_err(|e| format!("{e:#}"))
}

// ---------- Session / wallet-existence ----------

#[tauri::command]
pub fn wallet_path(state: State<AppState>) -> String {
    state.wallet_path.to_string_lossy().into_owned()
}

#[tauri::command]
pub fn wallet_exists(state: State<AppState>) -> bool {
    state.wallet_path.exists()
}

// ---------- Keygen ----------

#[tauri::command]
pub fn keygen(
    state: State<AppState>,
    input: ops::KeygenInput,
) -> Result<ops::KeygenOutput, String> {
    err_to_string(ops::keygen(&state.wallet_path, input))
}

#[tauri::command]
pub fn address(state: State<AppState>) -> Result<String, String> {
    err_to_string(ops::address(&state.wallet_path)).map(|x| hex::encode(x.serialize()))
}

// ---------- Mints (L1 side) ----------

#[tauri::command]
pub fn list_mints(
    state: State<AppState>,
) -> Result<Vec<hodl_wallet::wallet::MintRecord>, String> {
    err_to_string(ops::list_mints(&state.wallet_path))
}

#[tauri::command]
pub fn mint_utxo(
    state: State<AppState>,
    input: ops::MintUtxoInput,
) -> Result<ops::MintUtxoOutput, String> {
    err_to_string(ops::mint_utxo(&state.wallet_path, input))
}

#[tauri::command]
pub async fn check_mint_funding(
    state: State<'_, AppState>,
    input: ops::CheckMintFundingInput,
) -> Result<ops::CheckMintFundingOutput, String> {
    err_to_string(ops::check_mint_funding(&state.wallet_path, input).await)
}

// ---------- Mint message + transfer (L2 side) ----------

#[tauri::command]
pub async fn mint_message(
    state: State<'_, AppState>,
    input: ops::MintMessageInput,
) -> Result<ops::MintMessageOutput, String> {
    err_to_string(ops::mint_message(&state.wallet_path, input).await)
}

#[tauri::command]
pub async fn transfer(
    state: State<'_, AppState>,
    input: ops::TransferInput,
) -> Result<ops::TransferOutput, String> {
    err_to_string(ops::transfer(&state.wallet_path, input).await)
}

// ---------- Balance / verification ----------

#[tauri::command]
pub async fn balance(
    state: State<'_, AppState>,
    input: ops::BalanceInput,
) -> Result<ops::BalanceOutput, String> {
    err_to_string(ops::balance(&state.wallet_path, input).await)
}

#[tauri::command]
pub async fn verify_balance(
    state: State<'_, AppState>,
    input: ops::VerifyBalanceInput,
) -> Result<ops::VerifyBalanceOutput, String> {
    err_to_string(ops::verify_balance(&state.wallet_path, input).await)
}

#[tauri::command]
pub async fn sequencer_head(
    state: State<'_, AppState>,
) -> Result<hodl_core::rpc::HeadResponse, String> {
    err_to_string(ops::sequencer_head(&state.wallet_path).await)
}

#[tauri::command]
pub async fn light_head(
    state: State<'_, AppState>,
) -> Result<ops::LightHeadOutput, String> {
    err_to_string(ops::light_head(&state.wallet_path).await)
}

#[tauri::command]
pub async fn light_balance(
    state: State<'_, AppState>,
    input: ops::LightBalanceInput,
) -> Result<ops::LightBalanceOutput, String> {
    err_to_string(ops::light_balance(&state.wallet_path, input).await)
}

// ---------- Reclaim ----------

#[tauri::command]
pub async fn list_reclaimable_mints(
    state: State<'_, AppState>,
) -> Result<Vec<ops::ReclaimableMint>, String> {
    err_to_string(ops::list_reclaimable_mints(&state.wallet_path).await)
}

#[tauri::command]
pub async fn reclaim_mint(
    state: State<'_, AppState>,
    input: ops::ReclaimMintInput,
) -> Result<ops::ReclaimMintOutput, String> {
    err_to_string(ops::reclaim_mint(&state.wallet_path, input).await)
}
