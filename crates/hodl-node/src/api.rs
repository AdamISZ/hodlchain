//! Node HTTP API.
//!
//!   GET /head
//!   GET /balance/:addr_hex
//!   GET /block/:height

use anyhow::Result;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use bitcoin::secp256k1::XOnlyPublicKey;
use hodl_core::rpc::{BalanceResponse, HeadResponse};
use hodl_core::tx::L2Address;
use std::sync::{Arc, Mutex};

use crate::shared::Shared;
use crate::store::Store;

#[derive(Clone)]
pub struct AppState {
    pub shared: Arc<Shared>,
    pub store: Arc<Mutex<Store>>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
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
    let balance = state.balance_of(&addr);
    let nonce = state.nonce_of(&addr);
    Ok(Json(BalanceResponse { address: addr, balance, nonce }))
}

async fn get_block(
    State(app): State<AppState>,
    Path(height): Path<u32>,
) -> Result<Response, ApiError> {
    let store = app.store.lock().unwrap();
    match store.get_block(height)? {
        Some(b) => Ok(Json(b).into_response()),
        None => Ok((StatusCode::NOT_FOUND, "no such block").into_response()),
    }
}

fn parse_xonly(s: &str) -> anyhow::Result<XOnlyPublicKey> {
    let bytes = hex::decode(s)?;
    Ok(XOnlyPublicKey::from_slice(&bytes)?)
}
