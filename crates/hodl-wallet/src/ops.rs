//! Typed UI-agnostic operations. The single public surface that every
//! UI shell calls into.
//!
//! Inputs are plain data structs (Serialize+Deserialize where useful)
//! that can be built equally well by clap (CLI), Tauri commands (web
//! frontend), or programmatic callers. Outputs are Serialize structs;
//! UIs format them to text / JSON / whatever they need.
//!
//! Rules of the road:
//!   - No `println!` here. Ever.
//!   - Errors via `anyhow::Error`. Messages describe the failure
//!     condition; they do not target a specific UI.
//!   - Side effects (wallet-file load/save, sequencer HTTP calls,
//!     bitcoind RPC) live here; UIs never reach past this surface
//!     directly into `api` / `bitcoind` / `verify` to reimplement an
//!     operation.

use anyhow::{anyhow, bail, Context, Result};
use bip39::Mnemonic;
use bitcoin::secp256k1::{Message, Secp256k1, XOnlyPublicKey};
use bitcoin::{Address, OutPoint, Txid};
use hodl_core::address;
use hodl_core::consensus::MAX_LOCK_BLOCKS;
use hodl_core::hash::H256;
use hodl_core::l1::mint_address;
use hodl_core::proof::{MintProofEnvelope, OutpointProof};
use hodl_core::rpc::HeadResponse;
use hodl_core::smt::LeafKind;
use hodl_core::state::LedgerState;
use hodl_core::tx::{SignedTransfer, TransferBody};
use serde::{Deserialize, Serialize};
use std::str::FromStr;

use crate::api::ApiClient;
use crate::esplora::{self, EsploraClient};
use crate::reclaim;
use crate::verify::{self, ObservedTxKind, TxEvent};
use crate::wallet::{
    new_tx_id, now_ts, parse_outpoint, MintRecord, NetworkName, TxKind, TxRecord, TxStatus,
    WalletFile, REORG_FINALITY_DEPTH,
};

/// Apply a batch of `walk_forward` events to the wallet's tx history.
/// Matches existing `Soft` records by sighash / nullifier and flips
/// them to `InBlock`; emits fresh records for events with no match.
///
/// Idempotent: re-running on the same `events` won't duplicate
/// records (each event's unique key — sighash for transfers,
/// nullifier for mints — is checked against existing records before
/// inserting a new one).
fn project_events(wf: &mut WalletFile, events: &[TxEvent]) {
    for ev in events {
        let included_ts = if ev.block_ts != 0 { ev.block_ts } else { now_ts() };
        match &ev.kind {
            ObservedTxKind::TransferIn { from, amount, body_sighash_hex } => {
                // Dedupe by sighash. (Two TransferIns with the same
                // sighash would mean the same transfer was observed
                // twice, which can happen on a cold-start re-walk.)
                if wf.transactions.iter().any(|t| {
                    matches!(t.kind, TxKind::L2TransferReceived)
                        && t.l2_sighash.as_deref() == Some(body_sighash_hex.as_str())
                }) {
                    continue;
                }
                wf.append_tx(TxRecord {
                    id: new_tx_id(),
                    kind: TxKind::L2TransferReceived,
                    created_ts: included_ts,
                    amount: *amount,
                    fee_atoms: None,
                    fee_sat: None,
                    counterparty: Some(hex::encode(from.serialize())),
                    status: TxStatus::InBlock {
                        l2_height: ev.l2_height,
                        l1_height: ev.l1_height,
                        included_ts,
                    },
                    l1_txid: None,
                    l2_sighash: Some(body_sighash_hex.clone()),
                    bip32_index: None,
                    note: None,
                });
            }
            ObservedTxKind::TransferOut { to, amount, body_sighash_hex } => {
                // Match a previously-submitted Soft record by sighash.
                if let Some(rec) = wf.transactions.iter_mut().find(|t| {
                    matches!(t.kind, TxKind::L2TransferSent)
                        && t.l2_sighash.as_deref() == Some(body_sighash_hex.as_str())
                }) {
                    // Flip Soft (or Failed) → InBlock. Don't touch
                    // an already-Finalized record. Failed → InBlock
                    // is a legitimate transition if the sequencer's
                    // initial reject was racy and the tx eventually
                    // landed; rare but the algorithmic right thing.
                    if !matches!(rec.status, TxStatus::Finalized { .. }) {
                        rec.status = TxStatus::InBlock {
                            l2_height: ev.l2_height,
                            l1_height: ev.l1_height,
                            included_ts,
                        };
                    }
                } else {
                    // Cold-start backfill: we're seeing our own past
                    // send for the first time. Create the record
                    // retroactively in InBlock status.
                    wf.append_tx(TxRecord {
                        id: new_tx_id(),
                        kind: TxKind::L2TransferSent,
                        created_ts: included_ts,
                        amount: *amount,
                        fee_atoms: None, // unknown without re-computing
                        fee_sat: None,
                        counterparty: Some(hex::encode(to.serialize())),
                        status: TxStatus::InBlock {
                            l2_height: ev.l2_height,
                            l1_height: ev.l1_height,
                            included_ts,
                        },
                        l1_txid: None,
                        l2_sighash: Some(body_sighash_hex.clone()),
                        bip32_index: None,
                        note: None,
                    });
                }
            }
            ObservedTxKind::MintIn { amount, nullifier_hex } => {
                // Match a previously-submitted Soft L2MintApply by
                // nullifier_hex (which the mint_message ops captures
                // into l2_sighash). Flip to InBlock if found, else
                // create a new record (covers third-party mints to
                // our address and cold-start backfills).
                if let Some(rec) = wf.transactions.iter_mut().find(|t| {
                    matches!(t.kind, TxKind::L2MintApply)
                        && t.l2_sighash.as_deref() == Some(nullifier_hex.as_str())
                }) {
                    if !matches!(rec.status, TxStatus::Finalized { .. }) {
                        rec.status = TxStatus::InBlock {
                            l2_height: ev.l2_height,
                            l1_height: ev.l1_height,
                            included_ts,
                        };
                    }
                } else {
                    wf.append_tx(TxRecord {
                        id: new_tx_id(),
                        kind: TxKind::L2MintApply,
                        created_ts: included_ts,
                        amount: *amount,
                        fee_atoms: None,
                        fee_sat: None,
                        counterparty: None,
                        status: TxStatus::InBlock {
                            l2_height: ev.l2_height,
                            l1_height: ev.l1_height,
                            included_ts,
                        },
                        l1_txid: None,
                        l2_sighash: Some(nullifier_hex.clone()),
                        bip32_index: None,
                        note: None,
                    });
                }
            }
        }
    }
}

/// Late-stage status refresh: poll esplora for unconfirmed L1
/// records, then walk every non-Finalized record and flip to
/// `Finalized` if its L1 height is now `REORG_FINALITY_DEPTH` or
/// more deep in the chain.
///
/// Reorg recovery (the inverse: demoting `InBlock` back to
/// `L1Mempool` / `Soft` when an L1 reorg invalidates the containing
/// block) is **not** handled here; that requires per-record proof
/// that the previously-observed inclusion has been undone, which
/// the current Esplora-only data path doesn't always provide
/// cheaply. Leaving it as a deliberate gap until the use case
/// justifies the complexity.
///
/// Returns `Ok(true)` if any record changed status (so the caller
/// knows to persist), `Ok(false)` if nothing moved.
async fn finalize_and_refresh(
    wf: &mut WalletFile,
    esplora: &EsploraClient,
    l1_tip: u32,
) -> Result<bool> {
    let mut changed = false;

    // ---- Pass 1: promote L1Mempool records to InBlock by polling
    //              esplora for their txid.
    //
    // We collect indices first to avoid double-borrowing `wf` (mutable
    // iteration while awaiting). Cheap; the list is bounded by the
    // number of in-flight L1 txs, which is small in practice.
    let mut promotions: Vec<(usize, u32)> = Vec::new();
    for (i, t) in wf.transactions.iter().enumerate() {
        if matches!(t.status, TxStatus::L1Mempool { .. }) {
            if let Some(txid_hex) = &t.l1_txid {
                let txid: Txid = match txid_hex.parse() {
                    Ok(x) => x,
                    Err(_) => continue,
                };
                match esplora.get_tx(&txid).await {
                    Ok(info) => {
                        if let Some(h) = info.status.block_height {
                            promotions.push((i, h));
                        }
                    }
                    Err(_) => {
                        // 404 / network error: leave as L1Mempool.
                        // We could attempt to differentiate "tx not
                        // found" from transport error here but the
                        // benign action is the same — try again on
                        // the next refresh.
                    }
                }
            }
        }
    }
    for (i, h) in promotions {
        wf.transactions[i].status = TxStatus::InBlock {
            l2_height: 0, // pure-L1 record
            l1_height: h,
            included_ts: now_ts(),
        };
        changed = true;
    }

    // ---- Pass 2: flip InBlock records to Finalized when deep enough.
    //
    // `tip - l1_height >= REORG_FINALITY_DEPTH` is the gate. We use
    // saturating_sub so a record whose l1_height somehow exceeds the
    // tip (shouldn't happen, but cheap defence) falls through as
    // depth=0 → stays InBlock.
    for t in wf.transactions.iter_mut() {
        if let TxStatus::InBlock { l2_height, l1_height, .. } = t.status {
            let depth = l1_tip.saturating_sub(l1_height);
            if depth >= REORG_FINALITY_DEPTH {
                t.status = TxStatus::Finalized {
                    l2_height,
                    l1_height,
                };
                changed = true;
            }
        }
    }

    Ok(changed)
}

// ---------- Keygen ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KeygenInput {
    pub network: NetworkName,
    pub sequencer_url: String,
    pub node_url: Option<String>,
    /// Required: Esplora HTTP base URL. The wallet's only L1 data
    /// source.
    pub esplora_url: String,
    /// Optional BIP39 mnemonic phrase. When `None` we generate a
    /// fresh 24-word mnemonic. When `Some`, we validate the supplied
    /// phrase via `bip39::Mnemonic::from_str` (full checksum check)
    /// and use it to derive the wallet's keys — i.e. **restore** a
    /// wallet from a previously-backed-up phrase.
    #[serde(default)]
    pub mnemonic: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct KeygenOutput {
    /// Bech32m-encoded L2 address for the wallet's network. HRP is
    /// `hc` on mainnet, `thc` on testnet/signet, `hcrt` on regtest.
    pub l2_address: String,
    /// The wallet's BIP39 mnemonic. Echoed back so UIs can display
    /// it once for backup (fresh wallets) or simply confirm the
    /// caller-supplied phrase was accepted (restored wallets).
    pub mnemonic: String,
    /// `true` if the wallet was created fresh; `false` if restored
    /// from a caller-supplied mnemonic.
    pub was_fresh: bool,
}

/// Build a brand-new wallet from the supplied input. The caller is
/// responsible for picking a target path, checking it doesn't already
/// exist (or that overwrite is intended), and persisting the returned
/// `WalletFile`. We split it this way so the path-aware concerns —
/// existence checks, encryption choice — live entirely at the call
/// site instead of leaking into the ops layer.
pub fn keygen(input: KeygenInput) -> Result<(WalletFile, KeygenOutput)> {
    let secp = Secp256k1::new();
    let (phrase, was_fresh) = match input.mnemonic {
        Some(supplied) => {
            // Full BIP39 validation (wordlist + checksum) happens here.
            let mnemonic =
                Mnemonic::from_str(supplied.trim()).context("parse supplied BIP39 mnemonic")?;
            (mnemonic.to_string(), false)
        }
        None => {
            let mnemonic = Mnemonic::generate(24).context("generate BIP39 mnemonic")?;
            (mnemonic.to_string(), true)
        }
    };
    let wf = WalletFile {
        network: input.network,
        mnemonic: phrase.clone(),
        sequencer_url: input.sequencer_url,
        node_url: input.node_url,
        esplora_url: input.esplora_url,
        next_mint_index: 0,
        mints: Vec::new(),
        verified_head: None,
        transactions: Vec::new(),
    };
    let l2_address = wf.l2_identity_xonly(&secp)?;
    let out = KeygenOutput {
        l2_address: address::encode(&l2_address, wf.network),
        mnemonic: phrase,
        was_fresh,
    };
    Ok((wf, out))
}

// ---------- Address ----------

pub fn address(wf: &WalletFile) -> Result<XOnlyPublicKey> {
    let secp = Secp256k1::new();
    wf.xonly_pubkey(&secp)
}

// ---------- Mint UTXO: derive deposit address ----------
//
// The wallet does *not* construct or broadcast a funding tx. We just
// derive a fresh L1 mint key, compute the CSV-locked taproot address,
// record it, and return it. The user is expected to send BTC to that
// address from whatever external wallet they actually use (Sparrow,
// Electrum, hardware-wallet flow, exchange withdrawal, …). Our app
// then watches the address via `check_mint_funding`.

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MintUtxoInput {
    pub lock_blocks: u32,
}

#[derive(Clone, Debug, Serialize)]
pub struct MintUtxoOutput {
    pub bip32_index: u32,
    pub lock_blocks: u32,
    pub mint_address: String,
}

pub fn mint_utxo(wf: &mut WalletFile, input: MintUtxoInput) -> Result<MintUtxoOutput> {
    if input.lock_blocks == 0 || input.lock_blocks > MAX_LOCK_BLOCKS {
        bail!(
            "lock_blocks must be in [1, {}] (BIP112 CSV block-form range)",
            MAX_LOCK_BLOCKS
        );
    }
    let secp = Secp256k1::new();
    let network = wf.network.into_bitcoin();
    // Allocate a fresh BIP32-derived L1 mint key for this mint. Each
    // mint UTXO commits to a different user_pk on chain, so an L1
    // observer cannot trivially group mints by the same user.
    // Validate lock_blocks BEFORE allocating a BIP32 key index. A
    // bad input here would otherwise burn an index (the wallet's
    // next_mint_index counter only goes up) without committing
    // anything irreversible — but failing early is cleaner and
    // produces a better user-facing error than letting the failure
    // surface inside mint_address.
    hodl_core::l1::validate_lock_blocks(input.lock_blocks).with_context(|| {
        format!(
            "rejecting mint-utxo request: lock_blocks={} is out of range",
            input.lock_blocks,
        )
    })?;

    let (mint_kp, bip32_index) = wf.allocate_mint_keypair(&secp)?;
    let mint_xonly = mint_kp.x_only_public_key().0;
    // lock_blocks already validated above, so the construction
    // can't return an error here.
    let address = mint_address(&secp, input.lock_blocks, &mint_xonly, network)
        .expect("lock_blocks validated above");

    wf.append_mint(MintRecord {
        mint_address: address.to_string(),
        lock_blocks: input.lock_blocks,
        bip32_index,
        outpoint: None,
        value_sat: None,
        funded_at_height: None,
        minted: false,
        reclaimed: false,
    });
    Ok(MintUtxoOutput {
        bip32_index,
        lock_blocks: input.lock_blocks,
        mint_address: address.to_string(),
    })
}

// ---------- Check mint funding ----------
//
// Polls Esplora's `/address/{mint_address}/utxo` for unspent outputs
// at a recorded mint's deposit address. The first UTXO found is taken
// to be the funding tx — addresses are one-shot in our scheme so
// multiple deposits to the same address are user error and we
// intentionally lock onto the first one observed.

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CheckMintFundingInput {
    pub bip32_index: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MintFundingState {
    /// No UTXO observed at the mint address yet.
    Unfunded,
    /// A UTXO is visible but unconfirmed (mempool only). Most
    /// Esplora deployments don't return mempool UTXOs at the
    /// `/address/{addr}/utxo` endpoint, so this state may be
    /// effectively unreachable depending on the backend.
    Pending,
    /// UTXO confirmed; outpoint + value + funded_at_height are now
    /// persisted on the MintRecord.
    Confirmed,
}

#[derive(Clone, Debug, Serialize)]
pub struct CheckMintFundingOutput {
    pub bip32_index: u32,
    pub mint_address: String,
    pub state: MintFundingState,
    /// "<txid>:<vout>" once funded.
    pub outpoint: Option<String>,
    pub value_sat: Option<u64>,
    pub funded_at_height: Option<u32>,
}

pub async fn check_mint_funding(
    wf: &mut WalletFile,
    input: CheckMintFundingInput,
) -> Result<CheckMintFundingOutput> {
    let record = wf
        .find_mint_by_index(input.bip32_index)
        .ok_or_else(|| anyhow!("no mint record with bip32_index {}", input.bip32_index))?
        .clone();

    // If we already have a confirmed outpoint, short-circuit.
    if record.outpoint.is_some() && record.funded_at_height.is_some() {
        return Ok(CheckMintFundingOutput {
            bip32_index: record.bip32_index,
            mint_address: record.mint_address,
            state: MintFundingState::Confirmed,
            outpoint: record.outpoint,
            value_sat: record.value_sat,
            funded_at_height: record.funded_at_height,
        });
    }

    let esplora = EsploraClient::new(wf.esplora_url.clone());
    let network = wf.network.into_bitcoin();
    let address = Address::from_str(&record.mint_address)
        .context("parse mint_address")?
        .require_network(network)
        .with_context(|| format!("mint_address not on network {network:?}"))?;
    let utxos = esplora.address_utxos(&address).await?;

    if utxos.is_empty() {
        return Ok(CheckMintFundingOutput {
            bip32_index: record.bip32_index,
            mint_address: record.mint_address,
            state: MintFundingState::Unfunded,
            outpoint: None,
            value_sat: None,
            funded_at_height: None,
        });
    }

    // Take the first UTXO. Addresses are one-shot.
    let u = &utxos[0];
    let state = if u.status.block_height.is_some() {
        MintFundingState::Confirmed
    } else {
        MintFundingState::Pending
    };

    // Update the record only when confirmed — the mint_message flow
    // and the reclaim flow both need a confirmed funded_at_height.
    let (out_outpoint, out_value, out_height) = if state == MintFundingState::Confirmed {
        let outpoint_s = format!("{}:{}", u.txid, u.vout);
        let height = u.status.block_height;
        let r = wf
            .find_mint_by_index_mut(input.bip32_index)
            .expect("record exists; we just read it");
        r.outpoint = Some(outpoint_s.clone());
        r.value_sat = Some(u.value);
        r.funded_at_height = height;

        // History: record the L1 deposit at first confirmed observation.
        // Subsequent polls find the existing record and leave it alone
        // (status flip to Finalized is handled by the walk-forward
        // pass once L1 confs grow past REORG_FINALITY_DEPTH; not
        // captured here in step 2).
        if wf.find_l1_deposit_tx_mut(input.bip32_index).is_none() {
            wf.append_tx(TxRecord {
                id: new_tx_id(),
                kind: TxKind::L1Deposit,
                created_ts: now_ts(),
                amount: u.value,
                fee_atoms: None,
                fee_sat: None,
                counterparty: Some(record.mint_address.clone()),
                status: TxStatus::InBlock {
                    l2_height: 0,
                    l1_height: height.unwrap_or(0),
                    included_ts: now_ts(),
                },
                l1_txid: Some(u.txid.to_string()),
                l2_sighash: None,
                bip32_index: Some(input.bip32_index),
                note: None,
            });
        }

        (Some(outpoint_s), Some(u.value), height)
    } else {
        (Some(format!("{}:{}", u.txid, u.vout)), Some(u.value), None)
    };

    Ok(CheckMintFundingOutput {
        bip32_index: record.bip32_index,
        mint_address: record.mint_address,
        state,
        outpoint: out_outpoint,
        value_sat: out_value,
        funded_at_height: out_height,
    })
}

// ---------- List Mints ----------

pub fn list_mints(wf: &WalletFile) -> Vec<MintRecord> {
    wf.mints.clone()
}

// ---------- List transaction history ----------

/// Return every persisted `TxRecord` in this wallet, **newest-first**.
/// The wallet stores them in append order (oldest first); we reverse
/// here so UIs can render the list directly without re-sorting.
pub fn list_transactions(wf: &WalletFile) -> Vec<TxRecord> {
    let mut out = wf.transactions.clone();
    out.reverse();
    out
}

// ---------- Mint Message ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MintMessageInput {
    /// Identifies the mint by its BIP32 index. Must reference a
    /// MintRecord whose funding has been observed (use
    /// `check_mint_funding` first).
    pub bip32_index: u32,
    /// Optional L2 destination address (bech32m). Defaults to the
    /// wallet's own L2 identity. Must encode for the same network
    /// class (mainnet / testnet+signet / regtest) as the wallet.
    pub to: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MintMessageOutput {
    pub accepted: bool,
    pub mint_amount: Option<u64>,
    pub nullifier_hex: Option<String>,
    pub error: Option<String>,
    /// Sequencer-signed soft-confirmation receipt promising inclusion
    /// at `target_l2_height`. Present on accept; UIs can show the
    /// distinction between "sequencer acknowledged" and "L1-confirmed".
    pub soft_conf: Option<hodl_core::rpc::SoftConf>,
}

pub async fn mint_message(wf: &mut WalletFile, input: MintMessageInput) -> Result<MintMessageOutput> {
    let secp = Secp256k1::new();
    let l2_identity = wf.l2_identity_xonly(&secp)?;
    let record = wf
        .find_mint_by_index(input.bip32_index)
        .ok_or_else(|| anyhow!("no mint record with bip32_index {}", input.bip32_index))?
        .clone();
    let outpoint_s = record.outpoint.as_ref().ok_or_else(|| {
        anyhow!(
            "mint {} has no observed funding UTXO yet — run check-mint-funding \
             after sending BTC to {}",
            input.bip32_index,
            record.mint_address
        )
    })?;
    let outpoint: OutPoint = parse_outpoint(outpoint_s)?;
    let l2_destination = match input.to {
        Some(s) => address::decode_for(&s, wf.network).with_context(|| {
            format!("decode --to address {s:?} as bech32m L2 address")
        })?,
        None => l2_identity,
    };

    // Sign the mint message with the L1 mint key that the mint UTXO
    // commits to (via `user_pk` in L_spend). The signed message
    // includes the current L1 tip height as `claimed_block_height`
    // (paper §3, `m = (outpoint, h, L2-destination)`), so the
    // verifier can enforce the active-lock-period bound.
    let mint_kp = wf.mint_keypair(&secp, record.bip32_index)?;
    let mint_xonly = mint_kp.x_only_public_key().0;
    let esplora = EsploraClient::new(wf.esplora_url.clone());
    let claimed_block_height = esplora
        .tip_height()
        .await
        .context("query L1 tip for mint message claimed_block_height")?;
    let sighash = OutpointProof::sighash(&outpoint, claimed_block_height, &l2_destination);
    let msg = Message::from_digest(sighash);
    let signature = secp.sign_schnorr(&msg, &mint_kp);
    let proof = OutpointProof {
        outpoint,
        user_xonly_pubkey: mint_xonly,
        lock_blocks: record.lock_blocks,
        claimed_block_height,
        signature,
    };

    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let resp = api
        .submit_mint(MintProofEnvelope::V0Outpoint(proof), l2_destination)
        .await?;
    if resp.accepted {
        if let Some(r) = wf.find_mint_by_index_mut(input.bip32_index) {
            r.minted = true;
        }
        wf.append_tx(TxRecord {
            id: new_tx_id(),
            kind: TxKind::L2MintApply,
            created_ts: now_ts(),
            amount: resp.mint_amount.unwrap_or(0),
            fee_atoms: None,
            fee_sat: None,
            counterparty: Some(address::encode(&l2_destination, wf.network)),
            status: TxStatus::Soft { since_ts: now_ts() },
            l1_txid: None,
            l2_sighash: resp.nullifier_hex.clone(),
            bip32_index: Some(input.bip32_index),
            note: None,
        });
    } else {
        wf.append_tx(TxRecord {
            id: new_tx_id(),
            kind: TxKind::L2MintApply,
            created_ts: now_ts(),
            amount: 0,
            fee_atoms: None,
            fee_sat: None,
            counterparty: Some(address::encode(&l2_destination, wf.network)),
            status: TxStatus::Failed {
                reason: resp
                    .error
                    .clone()
                    .unwrap_or_else(|| "sequencer rejected mint (no error string)".into()),
                ts: now_ts(),
            },
            l1_txid: None,
            l2_sighash: None,
            bip32_index: Some(input.bip32_index),
            note: None,
        });
    }
    Ok(MintMessageOutput {
        accepted: resp.accepted,
        mint_amount: resp.mint_amount,
        nullifier_hex: resp.nullifier_hex,
        error: resp.error,
        soft_conf: resp.soft_conf,
    })
}

// ---------- Transfer ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransferInput {
    /// Bech32m-encoded destination address. Must encode for the same
    /// network class as the wallet — a hex string or wrong-HRP
    /// address is rejected with a typed error.
    pub to: String,
    pub amount: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransferOutput {
    pub accepted: bool,
    pub error: Option<String>,
    /// Protocol fee the chain will deduct from the sender, in atoms.
    /// Computed as `max(MIN_FEE, amount * FEE_BPS / 10_000)`. Surfaced
    /// here so UIs can display "amount + fee = total" without
    /// re-deriving the formula client-side.
    pub fee: u64,
    /// Convenience: `amount + fee`. The sender's balance decreases by
    /// this much; the recipient receives `amount`.
    pub total: u64,
    /// Sequencer-signed soft-confirmation receipt. Present on accept.
    pub soft_conf: Option<hodl_core::rpc::SoftConf>,
}

/// Mirror of the on-chain formula. Kept in sync with
/// `hodl_core::state::apply_transfer`.
pub fn compute_transfer_fee(amount: u64) -> u64 {
    use hodl_core::consensus::{FEE_BPS, MIN_FEE};
    std::cmp::max(MIN_FEE, amount.saturating_mul(FEE_BPS) / 10_000)
}

pub async fn transfer(wf: &mut WalletFile, input: TransferInput) -> Result<TransferOutput> {
    let secp = Secp256k1::new();
    let kp = wf.l2_identity_keypair(&secp)?;
    let from = wf.l2_identity_xonly(&secp)?;
    let to = address::decode_for(&input.to, wf.network).with_context(|| {
        format!("decode transfer destination {:?} as bech32m L2 address", input.to)
    })?;
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    // Use `effective_nonce` from the *sequencer* (not the node):
    // it accounts for any of our own transfers still sitting in
    // mempool, so back-to-back submits don't conflict on a stale
    // on-chain nonce.
    let bal = api.balance_via_sequencer(&from).await?;
    let body = TransferBody {
        from,
        to,
        amount: input.amount,
        nonce: bal.effective_nonce,
    };
    let sighash_bytes = body.sighash().0;
    let msg = Message::from_digest(sighash_bytes);
    let signature = secp.sign_schnorr(&msg, &kp);
    let signed = SignedTransfer { body, signature };
    let resp = api.submit_transfer(signed).await?;
    let fee = compute_transfer_fee(input.amount);

    let counterparty = address::encode(&to, wf.network);
    let sighash_hex = hex::encode(sighash_bytes);
    if resp.accepted {
        wf.append_tx(TxRecord {
            id: new_tx_id(),
            kind: TxKind::L2TransferSent,
            created_ts: now_ts(),
            amount: input.amount,
            fee_atoms: Some(fee),
            fee_sat: None,
            counterparty: Some(counterparty),
            status: TxStatus::Soft { since_ts: now_ts() },
            l1_txid: None,
            l2_sighash: Some(sighash_hex),
            bip32_index: None,
            note: None,
        });
    } else {
        wf.append_tx(TxRecord {
            id: new_tx_id(),
            kind: TxKind::L2TransferSent,
            created_ts: now_ts(),
            amount: input.amount,
            fee_atoms: Some(fee),
            fee_sat: None,
            counterparty: Some(counterparty),
            status: TxStatus::Failed {
                reason: resp
                    .error
                    .clone()
                    .unwrap_or_else(|| "sequencer rejected transfer (no error string)".into()),
                ts: now_ts(),
            },
            l1_txid: None,
            l2_sighash: Some(sighash_hex),
            bip32_index: None,
            note: None,
        });
    }

    Ok(TransferOutput {
        accepted: resp.accepted,
        error: resp.error,
        fee,
        total: input.amount.saturating_add(fee),
        soft_conf: resp.soft_conf,
    })
}

// ---------- Balance ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BalanceInput {
    /// Optional bech32m address to query. Defaults to the wallet's own.
    pub addr: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BalanceOutput {
    /// Bech32m-encoded for the wallet's network.
    pub address: String,
    pub balance: u64,
    pub nonce: u64,
}

/// "Soft" balance — reads directly from the **sequencer**, which
/// reflects the current state including L2 blocks produced but not
/// yet L1-attested. This is what the UI's headline balance figure
/// should use: it updates within one L2 block interval (~30s by
/// default) of a transfer landing in a sequencer-built block.
///
/// For the trustless / L1-anchored view, see `verify_balance` or
/// `light_balance`. Reading from the node here (the previous
/// default) caused the headline balance to lag by 1–2 L1 blocks —
/// effectively the same value as the verified balance, which made
/// the "SOFT" / "L1-confirmed" pill distinction in the GUI a lie.
pub async fn balance(wf: &WalletFile, input: BalanceInput) -> Result<BalanceOutput> {
    let secp = Secp256k1::new();
    let target = match input.addr {
        Some(s) => address::decode_for(&s, wf.network)
            .with_context(|| format!("decode --addr {s:?} as bech32m L2 address"))?,
        None => wf.xonly_pubkey(&secp)?,
    };
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let bal = api.balance_via_sequencer(&target).await?;
    Ok(BalanceOutput {
        address: address::encode(&target, wf.network),
        balance: bal.balance,
        nonce: bal.nonce,
    })
}

// ---------- Verify Balance ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerifyBalanceInput {
    /// Optional bech32m address to query. Defaults to the wallet's own.
    pub addr: Option<String>,
    /// Optional externally-supplied state_root to compare against
    /// (e.g. one walked off L1). When supplied, the verification also
    /// checks `state_root == against`.
    pub against: Option<H256>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifyBalanceOutput {
    /// Bech32m-encoded for the wallet's network.
    pub address: String,
    pub balance: u64,
    pub nonce: u64,
    pub l2_height: u32,
    pub state_root: H256,
    pub bound_to_l1: bool,
}

pub async fn verify_balance(wf: &WalletFile, input: VerifyBalanceInput) -> Result<VerifyBalanceOutput> {
    let secp = Secp256k1::new();
    let target = match input.addr {
        Some(s) => address::decode_for(&s, wf.network)
            .with_context(|| format!("decode --addr {s:?} as bech32m L2 address"))?,
        None => wf.xonly_pubkey(&secp)?,
    };
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let bal = api.balance(&target).await?;

    // 1. Self-consistency: components hash to state_root.
    let derived = bal.state_components.state_root();
    if derived != bal.state_root {
        bail!(
            "response self-inconsistent: components.state_root()={} != reported state_root={}",
            derived,
            bal.state_root
        );
    }

    // 2. Optional L1 binding.
    if let Some(expected) = input.against {
        if expected != bal.state_root {
            bail!(
                "state_root mismatch: response says {}, against says {}",
                bal.state_root,
                expected
            );
        }
    }

    // 3. SMT proof.
    if bal.proof.address != bal.address {
        bail!(
            "proof address {} disagrees with response address {}",
            address::encode(&bal.proof.address, wf.network),
            address::encode(&bal.address, wf.network)
        );
    }
    if !bal.proof.verify(bal.state_components.accounts_root) {
        bail!("inclusion proof does not verify against accounts_root");
    }

    // 4. Leaf payload matches reported values.
    match &bal.proof.leaf {
        LeafKind::Account { balance, nonce } => {
            if *balance != bal.balance {
                bail!(
                    "proof leaf balance {} disagrees with reported balance {}",
                    balance,
                    bal.balance
                );
            }
            if *nonce != bal.nonce {
                bail!(
                    "proof leaf nonce {} disagrees with reported nonce {}",
                    nonce,
                    bal.nonce
                );
            }
        }
        LeafKind::Empty => {
            if bal.balance != 0 || bal.nonce != 0 {
                bail!(
                    "proof claims no-such-account but reported balance/nonce are non-zero ({}, {})",
                    bal.balance,
                    bal.nonce
                );
            }
        }
    }

    Ok(VerifyBalanceOutput {
        address: address::encode(&target, wf.network),
        balance: bal.balance,
        nonce: bal.nonce,
        l2_height: bal.l2_height,
        state_root: bal.state_root,
        bound_to_l1: input.against.is_some(),
    })
}

// ---------- Head ----------

pub async fn sequencer_head(wf: &WalletFile) -> Result<HeadResponse> {
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    api.sequencer_head().await
}

// ---------- Light Head ----------

#[derive(Clone, Debug, Serialize)]
pub struct LightHeadOutput {
    pub l2_height: u32,
    pub state_root: H256,
    pub attestations_walked: usize,
}

pub async fn light_head(wf: &WalletFile) -> Result<LightHeadOutput> {
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let esplora = EsploraClient::new(wf.esplora_url.clone());

    let genesis = api
        .get_block(0)
        .await
        .context("fetch L2 genesis (height 0) for anchor_0 bootstrap")?;
    let anchor_0 = genesis
        .header
        .anchor_outpoint
        .ok_or_else(|| anyhow!("L2 genesis header has no anchor_outpoint"))?;

    let chain = esplora::walk_attestation_chain(&esplora, anchor_0).await?;
    let (state_root, l2_height) = if chain.is_empty() {
        (LedgerState::new().state_root(), 0)
    } else {
        let last = chain.last().unwrap();
        (last.attestation.state_root, last.attestation.height)
    };
    Ok(LightHeadOutput {
        l2_height,
        state_root,
        attestations_walked: chain.len(),
    })
}

// ---------- Light Balance (sparse, with persisted head) ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct LightBalanceInput {
    /// Optional bech32m address to query. Defaults to the wallet's own.
    pub addr: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LightBalanceMode {
    /// First time on this wallet — bootstrap via the node's inclusion
    /// proof + nullifier-set snapshot, then incrementally verify any
    /// blocks past the bootstrap snapshot.
    ColdStart,
    /// Wallet already had a verified head; only new blocks since then
    /// were verified.
    WarmStart,
}

#[derive(Clone, Debug, Serialize)]
pub struct LightBalanceOutput {
    pub mode: LightBalanceMode,
    pub blocks_verified: usize,
    pub l2_height: u32,
    pub state_root: H256,
    pub accounts_root: H256,
    pub block_hash: H256,
    pub l1_height: u32,
    /// Bech32m-encoded for the wallet's network.
    pub address: String,
    pub balance: u64,
    pub nonce: u64,
    pub is_own_address: bool,
    /// Total atoms ever minted on this chain. See VerifiedHead for
    /// trust caveat (sequencer-trusted on cold-start, verified after).
    pub total_minted_atoms: u64,
}

pub async fn light_balance(wf: &mut WalletFile, input: LightBalanceInput) -> Result<LightBalanceOutput> {
    let secp = Secp256k1::new();
    let own_addr = wf.xonly_pubkey(&secp)?;
    let target = match input.addr {
        Some(s) => address::decode_for(&s, wf.network)
            .with_context(|| format!("decode --addr {s:?} as bech32m L2 address"))?,
        None => own_addr,
    };

    let esplora = EsploraClient::new(wf.esplora_url.clone());
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());

    if let Some(h) = &wf.verified_head {
        if h.own_address != own_addr {
            bail!(
                "persisted verified_head tracks a different address ({}); \
                 wallet key may have changed",
                address::encode(&h.own_address, wf.network)
            );
        }
    }

    let (mut head, mode, blocks_verified, events) = match wf.verified_head.take() {
        None => {
            let h = verify::bootstrap(&api, &esplora, own_addr).await?;
            let (h, n, evs) = verify::walk_forward(h, &api, &esplora).await?;
            (h, LightBalanceMode::ColdStart, n, evs)
        }
        Some(h) => {
            let (h, n, evs) = verify::walk_forward(h, &api, &esplora).await?;
            (h, LightBalanceMode::WarmStart, n, evs)
        }
    };

    // Project walk-forward events into TxRecord changes. Existing
    // Soft records get flipped to InBlock when matched by
    // sighash/nullifier; unmatched events become new records (this
    // covers inbound transfers, third-party mints to our address,
    // and cold-start backfills of our own past sends).
    if !events.is_empty() {
        project_events(wf, &events);
    }

    // Late-stage status refresh: promote L1Mempool → InBlock for
    // any L1 records that have since confirmed, and flip InBlock →
    // Finalized for any record whose L1 height is now buried past
    // REORG_FINALITY_DEPTH. Cheap one-shot pass; the L1 tip we use
    // is the actual chain tip from Esplora (not head.l1_height,
    // which trails by up to one attestation cadence).
    let l1_tip = esplora.tip_height().await.unwrap_or(head.l1_height);
    finalize_and_refresh(wf, &esplora, l1_tip)
        .await
        .context("late-stage status refresh")?;

    // One-time top-up for wallets that persisted a `VerifiedHead`
    // before `total_minted_atoms` existed: the field defaults to 0
    // under `#[serde(default)]`, and warm-start walks only add the
    // per-block mint amounts of *new* L2 blocks (history is not
    // replayed). Detect the heuristic "field reads zero but chain is
    // non-empty" and seed from /balance. Trust model on this path
    // matches cold-start bootstrap; no-op if the chain genuinely has
    // zero supply.
    if head.total_minted_atoms == 0 && head.l2_height > 0 {
        let bal = api.balance(&own_addr).await?;
        head.total_minted_atoms = bal.total_minted_atoms;
    }

    let (balance, nonce) = if target == own_addr {
        verify::balance_from(&head, &own_addr)
    } else {
        // Other-address path: trust the node's inclusion proof iff it
        // verifies against our locally-verified accounts_root.
        let bal = api.balance(&target).await?;
        if bal.state_root != head.state_root {
            bail!(
                "node-reported state_root {} disagrees with locally-verified head {}",
                bal.state_root,
                head.state_root
            );
        }
        if !bal.proof.verify(head.accounts_root) {
            bail!("inclusion proof does not verify against verified accounts_root");
        }
        let (b, n) = match bal.proof.leaf {
            LeafKind::Account { balance, nonce } => (balance, nonce),
            LeafKind::Empty => (0, 0),
        };
        if b != bal.balance || n != bal.nonce {
            bail!("inclusion-proof leaf disagrees with reported balance/nonce");
        }
        (b, n)
    };

    let output = LightBalanceOutput {
        mode,
        blocks_verified,
        l2_height: head.l2_height,
        state_root: head.state_root,
        accounts_root: head.accounts_root,
        block_hash: head.block_hash,
        l1_height: head.l1_height,
        address: address::encode(&target, wf.network),
        balance,
        nonce,
        is_own_address: target == own_addr,
        total_minted_atoms: head.total_minted_atoms,
    };

    // Persist the (possibly-advanced) verified head before returning.
    // Caller saves the WalletFile.
    wf.verified_head = Some(head);
    Ok(output)
}

// ---------- Reclaim: list reclaimable mints ----------

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReclaimStatus {
    /// Funding tx not yet confirmed on L1.
    Pending,
    /// CSV not yet matured. `blocks_remaining` is how many more L1
    /// blocks until it can be reclaimed.
    Locked,
    /// CSV matured; can be reclaimed now.
    Ready,
    /// Wallet already broadcast a reclaim tx for this mint.
    Reclaimed,
}

#[derive(Clone, Debug, Serialize)]
pub struct ReclaimableMint {
    pub bip32_index: u32,
    pub mint_address: String,
    pub lock_blocks: u32,
    /// Funding outpoint "<txid>:<vout>". `None` while unfunded.
    pub outpoint: Option<String>,
    pub value_sat: Option<u64>,
    pub funded_at_height: Option<u32>,
    pub minted: bool,
    pub status: ReclaimStatus,
    /// Blocks remaining until CSV maturity. Zero when Ready, None
    /// when Pending or Reclaimed.
    pub blocks_remaining: Option<u32>,
}

pub async fn list_reclaimable_mints(wf: &WalletFile) -> Result<Vec<ReclaimableMint>> {
    let esplora = EsploraClient::new(wf.esplora_url.clone());

    // Single L1-tip lookup, used for every CSV check below. Avoids a
    // round-trip per mint.
    let tip = if wf.mints.iter().any(|m| !m.reclaimed && m.funded_at_height.is_some()) {
        Some(esplora.tip_height().await?)
    } else {
        None
    };

    let mut out = Vec::with_capacity(wf.mints.len());
    for m in &wf.mints {
        if m.reclaimed {
            out.push(ReclaimableMint {
                bip32_index: m.bip32_index,
                mint_address: m.mint_address.clone(),
                lock_blocks: m.lock_blocks,
                outpoint: m.outpoint.clone(),
                value_sat: m.value_sat,
                funded_at_height: m.funded_at_height,
                minted: m.minted,
                status: ReclaimStatus::Reclaimed,
                blocks_remaining: None,
            });
            continue;
        }
        let (status, blocks_remaining) = match m.funded_at_height {
            None => (ReclaimStatus::Pending, None),
            Some(h) => {
                // CSV-mature condition: spend tx mineable in block at
                // height >= funded_at + lock_blocks. So the spend can
                // be mined "right now" when tip + 1 >= that bound.
                let unlock_height = h.saturating_add(m.lock_blocks);
                let tip = tip.expect("tip queried when any funded record exists");
                if tip.saturating_add(1) >= unlock_height {
                    (ReclaimStatus::Ready, Some(0))
                } else {
                    let remaining = unlock_height.saturating_sub(tip.saturating_add(1));
                    (ReclaimStatus::Locked, Some(remaining))
                }
            }
        };
        out.push(ReclaimableMint {
            bip32_index: m.bip32_index,
            mint_address: m.mint_address.clone(),
            lock_blocks: m.lock_blocks,
            outpoint: m.outpoint.clone(),
            value_sat: m.value_sat,
            funded_at_height: m.funded_at_height,
            minted: m.minted,
            status,
            blocks_remaining,
        });
    }
    Ok(out)
}

// ---------- Reclaim: build, sign, broadcast ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReclaimMintInput {
    pub bip32_index: u32,
    /// Destination L1 address. Network must match the wallet's network.
    pub dest_address: String,
    /// Absolute fee in satoshis. Reclaim tx is small and predictable
    /// (~150 vB for a script-path Taproot spend with our 2-leaf tree);
    /// 1000 sat is a comfortable default for low-feerate environments
    /// and irrelevant on regtest.
    #[serde(default = "default_fee_sat")]
    pub fee_sat: u64,
}

fn default_fee_sat() -> u64 {
    1000
}

#[derive(Clone, Debug, Serialize)]
pub struct ReclaimMintOutput {
    pub txid: Txid,
    pub value_sat_in: u64,
    pub value_sat_out: u64,
    pub fee_sat: u64,
}

pub async fn reclaim_mint(wf: &mut WalletFile, input: ReclaimMintInput) -> Result<ReclaimMintOutput> {
    let secp = Secp256k1::new();
    let network = wf.network.into_bitcoin();

    let record_position = wf
        .mints
        .iter()
        .position(|m| m.bip32_index == input.bip32_index)
        .ok_or_else(|| anyhow!("no mint record with bip32_index {}", input.bip32_index))?;
    let record = wf.mints[record_position].clone();
    if record.reclaimed {
        bail!("mint {} already reclaimed", input.bip32_index);
    }
    let outpoint_s = record.outpoint.as_ref().ok_or_else(|| {
        anyhow!(
            "mint {} has no observed funding UTXO yet (run check-mint-funding)",
            input.bip32_index
        )
    })?;
    let outpoint: OutPoint = parse_outpoint(outpoint_s)?;
    let value_sat = record
        .value_sat
        .ok_or_else(|| anyhow!("mint {} has no recorded value", input.bip32_index))?;
    let funded_at = record.funded_at_height.ok_or_else(|| {
        anyhow!(
            "mint {} not yet confirmed (run check-mint-funding to refresh)",
            input.bip32_index
        )
    })?;

    let dest = Address::from_str(&input.dest_address)
        .with_context(|| format!("parse destination address {:?}", input.dest_address))?
        .require_network(network)
        .with_context(|| format!("destination address is not on network {network:?}"))?;

    let esplora = EsploraClient::new(wf.esplora_url.clone());
    let tip = esplora.tip_height().await?;
    let unlock_height = funded_at.saturating_add(record.lock_blocks);
    if tip.saturating_add(1) < unlock_height {
        let remaining = unlock_height.saturating_sub(tip.saturating_add(1));
        bail!(
            "mint not yet reclaimable: needs {} more L1 block(s) (unlock at {}, tip {})",
            remaining,
            unlock_height,
            tip
        );
    }

    let mint_kp = wf.mint_keypair(&secp, record.bip32_index)?;
    let tx = reclaim::build_signed_reclaim_tx(
        &secp,
        &mint_kp,
        outpoint,
        value_sat,
        record.lock_blocks,
        &dest,
        input.fee_sat,
    )?;

    let txid = esplora.broadcast(&tx).await.context("broadcast reclaim tx")?;

    wf.mints[record_position].reclaimed = true;
    wf.append_tx(TxRecord {
        id: new_tx_id(),
        kind: TxKind::L1Reclaim,
        created_ts: now_ts(),
        amount: value_sat.saturating_sub(input.fee_sat),
        fee_atoms: None,
        fee_sat: Some(input.fee_sat),
        counterparty: Some(dest.to_string()),
        // Reclaim is just-broadcast at this point; we don't poll for
        // confirmations here. Step-6 walk-forward will flip this to
        // InBlock / Finalized once esplora reports confs.
        status: TxStatus::L1Mempool { since_ts: now_ts() },
        l1_txid: Some(txid.to_string()),
        l2_sighash: None,
        bip32_index: Some(input.bip32_index),
        note: None,
    });

    Ok(ReclaimMintOutput {
        txid,
        value_sat_in: value_sat,
        value_sat_out: value_sat.saturating_sub(input.fee_sat),
        fee_sat: input.fee_sat,
    })
}
