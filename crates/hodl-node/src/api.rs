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
use bitcoin::Txid;
use hodl_core::rpc::{BalanceResponse, HeadResponse};
use hodl_core::tx::L2Address;
use serde::Serialize;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use crate::bitcoind::NodeL1;
use crate::shared::Shared;
use crate::store::Store;

#[derive(Clone)]
pub struct AppState {
    pub shared: Arc<Shared>,
    pub store: Arc<Mutex<Store>>,
    pub l1: Arc<NodeL1>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/head", get(get_head))
        .route("/balance/:addr", get(get_balance))
        .route("/block/:height", get(get_block))
        // Esplora-compatible (slim) endpoints so light wallets can walk
        // the L1 attestation chain via standard HTTP without bitcoind.
        .route("/tx/:txid", get(esplora_get_tx))
        .route("/tx/:txid/outspend/:vout", get(esplora_outspend))
        .with_state(state)
        .merge(SwaggerUi::new("/docs").url("/openapi.json", ApiDoc::openapi()))
}

/// OpenAPI spec aggregator for the node's HTTP surface.
/// Served as JSON at `/openapi.json`, rendered as Swagger UI at `/docs`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "hodl-node HTTP API",
        description = "Passive L2 validator + Esplora-compatible L1 lookups.\n\n\
                       Replays L2 blocks from a sequencer's body endpoint, \
                       re-verifies every mint witness against L1, exposes \
                       light-client query endpoints (balance with inclusion \
                       proof) and Esplora-shape `tx` / `outspend` endpoints \
                       so light wallets can walk the attestation chain \
                       without bitcoind.",
        version = "0.1.0",
    ),
    paths(
        get_head, get_balance, get_block,
        esplora_get_tx, esplora_outspend,
    ),
    components(schemas(
        hodl_core::rpc::HeadResponse,
        hodl_core::rpc::BalanceResponse,
        hodl_core::proof::MintProofEnvelope,
        hodl_core::proof::OutpointProof,
        hodl_core::tx::SignedTransfer,
        hodl_core::tx::TransferBody,
        hodl_core::tx::MintEntry,
        hodl_core::tx::MintEvent,
        hodl_core::tx::L2Tx,
        hodl_core::block::L2Block,
        hodl_core::block::L2BlockHeader,
        hodl_core::state::StateComponents,
        hodl_core::state::Account,
        hodl_core::smt::InclusionProof,
        hodl_core::smt::LeafKind,
        hodl_core::hash::H256,
        hodl_core::schemas::OutPointWire,
        EsploraTx,
        EsploraVin,
        EsploraVout,
        EsploraOutspend,
    ))
)]
pub struct ApiDoc;

struct ApiError(anyhow::Error);

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self { ApiError(e) }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (StatusCode::INTERNAL_SERVER_ERROR, self.0.to_string()).into_response()
    }
}

#[utoipa::path(
    get,
    path = "/head",
    responses(
        (status = 200, description = "Current L2 head as observed by this node", body = HeadResponse),
    ),
)]
async fn get_head(State(app): State<AppState>) -> Json<HeadResponse> {
    let head = app.shared.head.lock().unwrap().clone();
    Json(HeadResponse {
        height: head.height,
        l2_block_hash: head.block_hash,
        state_root: head.state_root,
        l1_height: head.l1_height,
    })
}

#[utoipa::path(
    get,
    path = "/balance/{addr}",
    params(
        ("addr" = String, Path, description = "L2 address (BIP340 x-only pubkey, 64-char hex)"),
    ),
    responses(
        (status = 200, description = "Balance + SMT inclusion proof", body = BalanceResponse),
        (status = 500, description = "Invalid address hex"),
    ),
)]
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

#[utoipa::path(
    get,
    path = "/block/{height}",
    params(
        ("height" = u32, Path, description = "L2 block height (0 = genesis)"),
    ),
    responses(
        (status = 200, description = "Full L2 block body (header + txs)", body = hodl_core::block::L2Block),
        (status = 404, description = "No block at that height"),
    ),
)]
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

// ---------- Esplora-compatible (slim) responses --------------------------
//
// These are the subset of Esplora's HTTP API needed by hodlcoin light
// clients to walk the attestation chain. Returned JSON fields match the
// Esplora schema where present; full Esplora responses have more fields
// (block status, fee, sizes, etc.) that we omit. Pointing a hodlcoin
// light client at a real Esplora endpoint (e.g. mempool.space) works
// identically — the wallet just ignores the extra fields.

/// Slim Esplora-shape transaction. Real Esplora returns more fields
/// (block status, fee, sizes); we omit them — the wallet doesn't read
/// them, and a wallet pointed at a real Esplora ignores the extras.
#[derive(Serialize, ToSchema)]
pub struct EsploraTx {
    /// Transaction id (32-byte hex).
    pub txid: String,
    pub vin: Vec<EsploraVin>,
    pub vout: Vec<EsploraVout>,
}

#[derive(Serialize, ToSchema)]
pub struct EsploraVin {
    /// The spent outpoint's tx id.
    pub txid: String,
    /// The spent outpoint's vout.
    pub vout: u32,
}

#[derive(Serialize, ToSchema)]
pub struct EsploraVout {
    /// scriptPubKey, hex-encoded.
    pub scriptpubkey: String,
    /// Output value in satoshis.
    pub value: u64,
}

/// Esplora-shape outspend response.
#[derive(Serialize, ToSchema)]
pub struct EsploraOutspend {
    pub spent: bool,
    /// When `spent`, the spending tx's txid (32-byte hex).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub txid: Option<String>,
    /// L1 block height at which the spending tx was mined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub block_height: Option<u32>,
}

#[utoipa::path(
    get,
    path = "/tx/{txid}",
    params(
        ("txid" = String, Path, description = "Transaction id (32-byte hex)"),
    ),
    responses(
        (status = 200, description = "Esplora-shape (slim) transaction info", body = EsploraTx),
        (status = 404, description = "tx not found in bitcoind (txindex required)"),
    ),
    tag = "Esplora",
)]
async fn esplora_get_tx(
    State(app): State<AppState>,
    Path(txid_hex): Path<String>,
) -> Result<Response, ApiError> {
    let txid = Txid::from_str(&txid_hex)
        .map_err(|e| ApiError(anyhow::anyhow!("bad txid {txid_hex}: {e}")))?;
    let l1 = app.l1.clone();
    let tx = tokio::task::spawn_blocking(move || l1.get_tx(&txid))
        .await
        .map_err(|e| ApiError(anyhow::anyhow!("join: {e}")))?;
    let tx = match tx {
        Ok(t) => t,
        Err(_) => return Ok((StatusCode::NOT_FOUND, "tx not found").into_response()),
    };
    let body = EsploraTx {
        txid: tx.compute_txid().to_string(),
        vin: tx
            .input
            .iter()
            .map(|i| EsploraVin {
                txid: i.previous_output.txid.to_string(),
                vout: i.previous_output.vout,
            })
            .collect(),
        vout: tx
            .output
            .iter()
            .map(|o| EsploraVout {
                scriptpubkey: hex::encode(o.script_pubkey.as_bytes()),
                value: o.value.to_sat(),
            })
            .collect(),
    };
    Ok(Json(body).into_response())
}

#[utoipa::path(
    get,
    path = "/tx/{txid}/outspend/{vout}",
    params(
        ("txid" = String, Path, description = "Transaction id of the spent output's parent (32-byte hex)"),
        ("vout" = u32, Path, description = "Output index within the parent tx"),
    ),
    responses(
        (status = 200, description = "Whether the outpoint is spent and, if so, by which tx", body = EsploraOutspend),
    ),
    tag = "Esplora",
)]
async fn esplora_outspend(
    State(app): State<AppState>,
    Path((txid_hex, vout)): Path<(String, u32)>,
) -> Result<Json<EsploraOutspend>, ApiError> {
    // Validate the txid format but we look it up as a string.
    let _ = Txid::from_str(&txid_hex)
        .map_err(|e| ApiError(anyhow::anyhow!("bad txid {txid_hex}: {e}")))?;
    let store = app.store.lock().unwrap();
    Ok(Json(match store.get_anchor_spender(&txid_hex, vout)? {
        Some((spender_txid, height)) => EsploraOutspend {
            spent: true,
            txid: Some(spender_txid.to_string()),
            block_height: Some(height),
        },
        None => EsploraOutspend {
            spent: false,
            txid: None,
            block_height: None,
        },
    }))
}
