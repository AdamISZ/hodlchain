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
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

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
        .route("/witness/:height", get(get_witness))
        .with_state(state)
        .merge(SwaggerUi::new("/docs").url("/openapi.json", ApiDoc::openapi()))
}

/// OpenAPI spec aggregator for the sequencer's HTTP surface.
/// Served as JSON at `/openapi.json`, rendered as Swagger UI at `/docs`.
#[derive(OpenApi)]
#[openapi(
    info(
        title = "hodl-sequencer HTTP API",
        description = "Single-sequencer L2 producer for the hodlcoin POC.\n\n\
                       Accepts mint proofs and signed transfers, builds L2 \
                       blocks (one per L1 block), commits attestations on L1 \
                       as a chain of OP_RETURN transactions.",
        version = "0.1.0",
    ),
    paths(submit_mint, submit_transfer, get_head, get_balance, get_block, get_witness),
    components(schemas(
        // hodl-core::rpc
        hodl_core::rpc::SubmitMintRequest,
        hodl_core::rpc::SubmitMintResponse,
        hodl_core::rpc::SubmitTransferRequest,
        hodl_core::rpc::SubmitTransferResponse,
        hodl_core::rpc::HeadResponse,
        hodl_core::rpc::BalanceResponse,
        // hodl-core::proof
        hodl_core::proof::MintProofEnvelope,
        hodl_core::proof::OutpointProof,
        // hodl-core::tx
        hodl_core::tx::SignedTransfer,
        hodl_core::tx::TransferBody,
        hodl_core::tx::MintEntry,
        hodl_core::tx::MintEvent,
        hodl_core::tx::L2Tx,
        // hodl-core::block (response of /block/:height)
        hodl_core::block::L2Block,
        hodl_core::block::L2BlockHeader,
        // hodl-core::state
        hodl_core::state::StateComponents,
        hodl_core::state::Account,
        // hodl-core::smt
        hodl_core::smt::InclusionProof,
        hodl_core::smt::LeafKind,
        // hodl-core::witness
        hodl_core::witness::BlockWitness,
        // hodl-core::hash
        hodl_core::hash::H256,
        // doc-only stubs for external types
        hodl_core::schemas::OutPointWire,
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
    post,
    path = "/mint",
    request_body = SubmitMintRequest,
    responses(
        (status = 200, description = "Mint accepted or rejected; check `accepted` field", body = SubmitMintResponse),
        (status = 500, description = "Internal error verifying the proof against L1"),
    ),
)]
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

#[utoipa::path(
    post,
    path = "/transfer",
    request_body = SubmitTransferRequest,
    responses(
        (status = 200, description = "Transfer accepted or rejected; check `accepted` field", body = SubmitTransferResponse),
    ),
)]
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

#[utoipa::path(
    get,
    path = "/head",
    responses(
        (status = 200, description = "Current L2 head as known to this sequencer", body = HeadResponse),
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
    let block = store.get_block(height)?;
    match block {
        Some(b) => Ok(Json(b).into_response()),
        None => Ok((StatusCode::NOT_FOUND, "no such block").into_response()),
    }
}

#[utoipa::path(
    get,
    path = "/witness/{height}",
    params(
        ("height" = u32, Path, description = "L2 block height (>= 1; genesis has no witness)"),
    ),
    responses(
        (status = 200, description = "Pre-state inclusion proofs for every account touched by the block at that height", body = hodl_core::witness::BlockWitness),
        (status = 404, description = "No witness stored at that height"),
    ),
)]
async fn get_witness(
    State(app): State<AppState>,
    Path(height): Path<u32>,
) -> Result<Response, ApiError> {
    let store = app.store.lock().unwrap();
    match store.get_witness(height)? {
        Some(w) => Ok(Json(w).into_response()),
        None => Ok((StatusCode::NOT_FOUND, "no witness at that height").into_response()),
    }
}

fn parse_xonly(s: &str) -> anyhow::Result<XOnlyPublicKey> {
    let bytes = hex::decode(s)?;
    Ok(XOnlyPublicKey::from_slice(&bytes)?)
}
