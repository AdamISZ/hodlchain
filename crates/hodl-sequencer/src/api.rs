//! Sequencer HTTP API (axum).
//!
//!   POST /mint
//!   POST /transfer
//!   GET  /head
//!   GET  /block/:height
//!   GET  /balance/:addr_hex

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use bitcoin::secp256k1::{Message, Secp256k1, XOnlyPublicKey};
use hodl_core::proof::MintProof;
use hodl_core::rpc::{
    BalanceResponse, HeadResponse, SubmitMintRequest, SubmitMintResponse,
    SubmitTransferRequest, SubmitTransferResponse,
};
use hodl_core::tx::{L2Address, MintEntry};
use std::sync::{Arc, Mutex};

use crate::bitcoind::SequencerL1;
use crate::shared::Shared;
use crate::store::Store;

#[derive(Clone)]
pub struct AppState {
    pub shared: Arc<Shared>,
    pub store: Arc<Mutex<Store>>,
    pub l1: Arc<SequencerL1>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/mint", post(submit_mint))
        .route("/transfer", post(submit_transfer))
        .route("/head", get(get_head))
        .route("/balance/:addr", get(get_balance))
        .route("/block/:height", get(get_block))
        .with_state(state)
}

struct ApiError(anyhow::Error);

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self { ApiError(e) }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

async fn submit_mint(
    State(app): State<AppState>,
    Json(req): Json<SubmitMintRequest>,
) -> Result<Json<SubmitMintResponse>, ApiError> {
    let l1 = app.l1.clone();
    let r = app.shared.state.lock().unwrap().current_r;
    let dest = req.l2_destination;
    let witness = req.proof;
    let witness_for_verify = witness.clone();

    let credit_result = tokio::task::spawn_blocking(move || {
        let secp = Secp256k1::verification_only();
        witness_for_verify.verify(&secp, l1.as_ref(), dest, r)
    })
    .await
    .map_err(|e| anyhow::anyhow!("join: {e}"))?;

    let credit = match credit_result {
        Ok(c) => c,
        Err(e) => {
            return Ok(Json(SubmitMintResponse {
                accepted: false,
                error: Some(e.to_string()),
                mint_amount: None,
                nullifier_hex: None,
            }));
        }
    };

    let nullifier_hex = credit.event.nullifier_hex.clone();
    let amount = credit.event.amount;

    // Check the committed state + pending mempool for nullifier duplicates.
    {
        let state = app.shared.state.lock().unwrap();
        if state.consumed_nullifiers.contains(&nullifier_hex) {
            return Ok(Json(SubmitMintResponse {
                accepted: false,
                error: Some("nullifier already consumed".into()),
                mint_amount: None,
                nullifier_hex: None,
            }));
        }
    }
    {
        let mut mempool = app.shared.mempool.lock().unwrap();
        if mempool.pending_nullifiers.contains(&nullifier_hex) {
            return Ok(Json(SubmitMintResponse {
                accepted: false,
                error: Some("nullifier already in mempool".into()),
                mint_amount: None,
                nullifier_hex: None,
            }));
        }
        mempool.pending_nullifiers.insert(nullifier_hex.clone());
        mempool.mints.push(MintEntry {
            event: credit.event,
            witness,
        });
    }

    Ok(Json(SubmitMintResponse {
        accepted: true,
        error: None,
        mint_amount: Some(amount),
        nullifier_hex: Some(nullifier_hex),
    }))
}

async fn submit_transfer(
    State(app): State<AppState>,
    Json(req): Json<SubmitTransferRequest>,
) -> Result<Json<SubmitTransferResponse>, ApiError> {
    // Cheap pre-check: signature + nonce against committed state. The producer
    // is the authoritative validator at block-build time.
    let secp = Secp256k1::verification_only();
    let sighash = req.transfer.body.sighash().0;
    let msg = Message::from_digest(sighash);
    if secp
        .verify_schnorr(&req.transfer.signature, &msg, &req.transfer.body.from)
        .is_err()
    {
        return Ok(Json(SubmitTransferResponse {
            accepted: false,
            error: Some("bad signature".into()),
        }));
    }
    {
        let state = app.shared.state.lock().unwrap();
        let expected = state.nonce_of(&req.transfer.body.from);
        if req.transfer.body.nonce != expected {
            return Ok(Json(SubmitTransferResponse {
                accepted: false,
                error: Some(format!(
                    "nonce mismatch: expected {expected}, got {}",
                    req.transfer.body.nonce
                )),
            }));
        }
    }
    {
        let mut mempool = app.shared.mempool.lock().unwrap();
        mempool.transfers.push(req.transfer);
    }
    Ok(Json(SubmitTransferResponse { accepted: true, error: None }))
}

async fn get_head(State(app): State<AppState>) -> Json<HeadResponse> {
    let head = app.shared.head.lock().unwrap().clone();
    Json(HeadResponse {
        height: head.height,
        l2_block_hash: head.block_hash,
        state_root: head.state_root,
        l1_height: head.l1_height,
    })
}

async fn get_balance(
    State(app): State<AppState>,
    Path(addr_hex): Path<String>,
) -> Result<Json<BalanceResponse>, ApiError> {
    let addr: L2Address = parse_xonly(&addr_hex).map_err(ApiError)?;
    let state = app.shared.state.lock().unwrap();
    let head_height = app.shared.head.lock().unwrap().height;
    let balance = state.balance_of(&addr);
    let nonce = state.nonce_of(&addr);
    let proof = state.account_inclusion_proof(addr);
    let components = state.components();
    let state_root = components.state_root();
    Ok(Json(BalanceResponse {
        address: addr,
        balance,
        nonce,
        l2_height: head_height,
        state_root,
        state_components: components,
        proof,
    }))
}

async fn get_block(
    State(app): State<AppState>,
    Path(height): Path<u32>,
) -> Result<Response, ApiError> {
    let store = app.store.lock().unwrap();
    let block = store.get_block(height)?;
    match block {
        Some(b) => Ok(Json(b).into_response()),
        None => Ok((StatusCode::NOT_FOUND, "no such block").into_response()),
    }
}

fn parse_xonly(s: &str) -> anyhow::Result<XOnlyPublicKey> {
    let bytes = hex::decode(s)?;
    Ok(XOnlyPublicKey::from_slice(&bytes)?)
}
