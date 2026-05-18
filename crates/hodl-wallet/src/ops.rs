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
use bitcoin::{Address, Amount, OutPoint, Txid};
use hodl_core::consensus::MAX_LOCK_BLOCKS;
use hodl_core::hash::H256;
use hodl_core::l1::{derive_mint_taproot, mint_address};
use hodl_core::proof::{MintProofEnvelope, OutpointProof};
use hodl_core::rpc::HeadResponse;
use hodl_core::smt::LeafKind;
use hodl_core::state::LedgerState;
use hodl_core::tx::{SignedTransfer, TransferBody};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

use crate::api::ApiClient;
use crate::bitcoind::Bitcoind;
use crate::esplora::{self, EsploraClient};
use crate::reclaim;
use crate::verify;
use crate::wallet::{
    parse_outpoint, BitcoindConfig, MintRecord, NetworkName, WalletFile,
};

// ---------- Keygen ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct KeygenInput {
    pub network: NetworkName,
    pub bitcoind: BitcoindConfig,
    pub sequencer_url: String,
    pub node_url: Option<String>,
    pub esplora_url: Option<String>,
    pub force: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct KeygenOutput {
    pub l2_address: XOnlyPublicKey,
    /// The freshly-generated BIP39 mnemonic. **Display once at setup,
    /// invite the user to back it up.** It's also persisted to the
    /// wallet file; this field exists so UIs can surface it without
    /// re-reading the file.
    pub mnemonic: String,
}

pub fn keygen(wallet_path: &Path, input: KeygenInput) -> Result<KeygenOutput> {
    if wallet_path.exists() && !input.force {
        bail!(
            "wallet file {} already exists (set force=true to overwrite)",
            wallet_path.display()
        );
    }
    let secp = Secp256k1::new();
    let mnemonic = Mnemonic::generate(24).context("generate BIP39 mnemonic")?;
    let phrase = mnemonic.to_string();
    let wf = WalletFile {
        network: input.network,
        mnemonic: phrase.clone(),
        bitcoind: input.bitcoind,
        sequencer_url: input.sequencer_url,
        node_url: input.node_url,
        esplora_url: input.esplora_url,
        next_mint_index: 0,
        mints: Vec::new(),
        verified_head: None,
    };
    wf.save(wallet_path)?;
    let l2_address = wf.l2_identity_xonly(&secp)?;
    Ok(KeygenOutput {
        l2_address,
        mnemonic: phrase,
    })
}

// ---------- Address ----------

pub fn address(wallet_path: &Path) -> Result<XOnlyPublicKey> {
    let wf = WalletFile::load(wallet_path)?;
    let secp = Secp256k1::new();
    wf.xonly_pubkey(&secp)
}

// ---------- Mint UTXO ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MintUtxoInput {
    pub lock_blocks: u32,
    pub value_btc: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct MintUtxoOutput {
    pub l1_tip: u32,
    pub lock_blocks: u32,
    pub mint_address: String,
    pub txid: Txid,
    pub vout: u32,
    pub value_sat: u64,
}

pub fn mint_utxo(wallet_path: &Path, input: MintUtxoInput) -> Result<MintUtxoOutput> {
    if input.lock_blocks == 0 || input.lock_blocks > MAX_LOCK_BLOCKS {
        bail!(
            "lock_blocks must be in [1, {}] (BIP112 CSV block-form range)",
            MAX_LOCK_BLOCKS
        );
    }
    let mut wf = WalletFile::load(wallet_path)?;
    let secp = Secp256k1::new();
    // Allocate a fresh BIP32-derived L1 mint key for this mint. Each
    // mint UTXO commits to a different user_pk on chain, so an L1
    // observer cannot group mints by the same user without
    // additional analysis.
    let (mint_kp, bip32_index) = wf.allocate_mint_keypair(&secp)?;
    let mint_xonly = mint_kp.x_only_public_key().0;
    let network = wf.network.into_bitcoin();
    let bd = Bitcoind::connect(&wf.bitcoind)?;
    let l1_tip = bd.block_count()?;
    let (spk, _spend) = derive_mint_taproot(&secp, input.lock_blocks, &mint_xonly);
    let address = mint_address(&secp, input.lock_blocks, &mint_xonly, network);
    let amount = Amount::from_btc(input.value_btc).context("invalid BTC amount")?;
    let (txid, vout) = bd.send_to_address(&address, amount, &spk)?;
    wf.upsert_mint(MintRecord {
        outpoint: format!("{txid}:{vout}"),
        value_sat: amount.to_sat(),
        lock_blocks: input.lock_blocks,
        bip32_index,
        minted: false,
        reclaimed: false,
    });
    wf.save(wallet_path)?;
    Ok(MintUtxoOutput {
        l1_tip,
        lock_blocks: input.lock_blocks,
        mint_address: address.to_string(),
        txid,
        vout,
        value_sat: amount.to_sat(),
    })
}

// ---------- List Mints ----------

pub fn list_mints(wallet_path: &Path) -> Result<Vec<MintRecord>> {
    let wf = WalletFile::load(wallet_path)?;
    Ok(wf.mints)
}

// ---------- Mint Message ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct MintMessageInput {
    pub outpoint: String,
    pub to: Option<XOnlyPublicKey>,
}

#[derive(Clone, Debug, Serialize)]
pub struct MintMessageOutput {
    pub accepted: bool,
    pub mint_amount: Option<u64>,
    pub nullifier_hex: Option<String>,
    pub error: Option<String>,
}

pub async fn mint_message(wallet_path: &Path, input: MintMessageInput) -> Result<MintMessageOutput> {
    let mut wf = WalletFile::load(wallet_path)?;
    let secp = Secp256k1::new();
    let l2_identity = wf.l2_identity_xonly(&secp)?;
    let record = wf
        .find_mint(&input.outpoint)
        .ok_or_else(|| anyhow!("no recorded mint for {}", input.outpoint))?
        .clone();
    let outpoint: OutPoint = parse_outpoint(&record.outpoint)?;
    let l2_destination = input.to.unwrap_or(l2_identity);

    // Sign the mint message with the L1 mint key that the mint UTXO
    // commits to (via `user_pk` in L_spend).
    let mint_kp = wf.mint_keypair(&secp, record.bip32_index)?;
    let mint_xonly = mint_kp.x_only_public_key().0;
    let sighash = OutpointProof::sighash(&outpoint, &l2_destination);
    let msg = Message::from_digest(sighash);
    let signature = secp.sign_schnorr(&msg, &mint_kp);
    let proof = OutpointProof {
        outpoint,
        user_xonly_pubkey: mint_xonly,
        lock_blocks: record.lock_blocks,
        signature,
    };

    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let resp = api
        .submit_mint(MintProofEnvelope::V0Outpoint(proof), l2_destination)
        .await?;
    if resp.accepted {
        if let Some(r) = wf.find_mint_mut(&input.outpoint) {
            r.minted = true;
        }
        wf.save(wallet_path)?;
    }
    Ok(MintMessageOutput {
        accepted: resp.accepted,
        mint_amount: resp.mint_amount,
        nullifier_hex: resp.nullifier_hex,
        error: resp.error,
    })
}

// ---------- Transfer ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TransferInput {
    pub to: XOnlyPublicKey,
    pub amount: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct TransferOutput {
    pub accepted: bool,
    pub error: Option<String>,
}

pub async fn transfer(wallet_path: &Path, input: TransferInput) -> Result<TransferOutput> {
    let wf = WalletFile::load(wallet_path)?;
    let secp = Secp256k1::new();
    let kp = wf.l2_identity_keypair(&secp)?;
    let from = wf.l2_identity_xonly(&secp)?;
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let bal = api.balance(&from).await?;
    let body = TransferBody {
        from,
        to: input.to,
        amount: input.amount,
        nonce: bal.nonce,
    };
    let msg = Message::from_digest(body.sighash().0);
    let signature = secp.sign_schnorr(&msg, &kp);
    let signed = SignedTransfer { body, signature };
    let resp = api.submit_transfer(signed).await?;
    Ok(TransferOutput {
        accepted: resp.accepted,
        error: resp.error,
    })
}

// ---------- Balance ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct BalanceInput {
    pub addr: Option<XOnlyPublicKey>,
}

#[derive(Clone, Debug, Serialize)]
pub struct BalanceOutput {
    pub address: XOnlyPublicKey,
    pub balance: u64,
    pub nonce: u64,
}

pub async fn balance(wallet_path: &Path, input: BalanceInput) -> Result<BalanceOutput> {
    let wf = WalletFile::load(wallet_path)?;
    let secp = Secp256k1::new();
    let target = match input.addr {
        Some(a) => a,
        None => wf.xonly_pubkey(&secp)?,
    };
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let bal = api.balance(&target).await?;
    Ok(BalanceOutput {
        address: target,
        balance: bal.balance,
        nonce: bal.nonce,
    })
}

// ---------- Verify Balance ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct VerifyBalanceInput {
    pub addr: Option<XOnlyPublicKey>,
    /// Optional externally-supplied state_root to compare against
    /// (e.g. one walked off L1). When supplied, the verification also
    /// checks `state_root == against`.
    pub against: Option<H256>,
}

#[derive(Clone, Debug, Serialize)]
pub struct VerifyBalanceOutput {
    pub address: XOnlyPublicKey,
    pub balance: u64,
    pub nonce: u64,
    pub l2_height: u32,
    pub state_root: H256,
    pub bound_to_l1: bool,
}

pub async fn verify_balance(wallet_path: &Path, input: VerifyBalanceInput) -> Result<VerifyBalanceOutput> {
    let wf = WalletFile::load(wallet_path)?;
    let secp = Secp256k1::new();
    let target = match input.addr {
        Some(a) => a,
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
            hex::encode(bal.proof.address.serialize()),
            hex::encode(bal.address.serialize())
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
        address: target,
        balance: bal.balance,
        nonce: bal.nonce,
        l2_height: bal.l2_height,
        state_root: bal.state_root,
        bound_to_l1: input.against.is_some(),
    })
}

// ---------- Head ----------

pub async fn sequencer_head(wallet_path: &Path) -> Result<HeadResponse> {
    let wf = WalletFile::load(wallet_path)?;
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

pub async fn light_head(wallet_path: &Path) -> Result<LightHeadOutput> {
    let wf = WalletFile::load(wallet_path)?;
    let esplora_url = wf
        .esplora_url
        .as_ref()
        .ok_or_else(|| anyhow!("wallet has no esplora_url configured"))?
        .clone();
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());
    let esplora = EsploraClient::new(esplora_url);

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
    pub addr: Option<XOnlyPublicKey>,
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
    pub address: XOnlyPublicKey,
    pub balance: u64,
    pub nonce: u64,
    pub is_own_address: bool,
}

pub async fn light_balance(wallet_path: &Path, input: LightBalanceInput) -> Result<LightBalanceOutput> {
    let mut wf = WalletFile::load(wallet_path)?;
    let secp = Secp256k1::new();
    let own_addr = wf.xonly_pubkey(&secp)?;
    let target = input.addr.unwrap_or(own_addr);

    let esplora_url = wf
        .esplora_url
        .as_ref()
        .ok_or_else(|| anyhow!("wallet has no esplora_url configured"))?
        .clone();
    let esplora = EsploraClient::new(esplora_url);
    let api = ApiClient::new(wf.sequencer_url.clone(), wf.node_url.clone());

    if let Some(h) = &wf.verified_head {
        if h.own_address != own_addr {
            bail!(
                "persisted verified_head tracks a different address ({}); \
                 wallet key may have changed",
                hex::encode(h.own_address.serialize())
            );
        }
    }

    let (head, mode, blocks_verified) = match wf.verified_head.take() {
        None => {
            let h = verify::bootstrap(&api, &esplora, own_addr).await?;
            let (h, n) = verify::walk_forward(h, &api, &esplora).await?;
            (h, LightBalanceMode::ColdStart, n)
        }
        Some(h) => {
            let (h, n) = verify::walk_forward(h, &api, &esplora).await?;
            (h, LightBalanceMode::WarmStart, n)
        }
    };

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
        address: target,
        balance,
        nonce,
        is_own_address: target == own_addr,
    };

    // Persist the (possibly-advanced) verified head before returning.
    wf.verified_head = Some(head);
    wf.save(wallet_path)?;
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
    pub outpoint: String,
    pub value_sat: u64,
    pub lock_blocks: u32,
    pub bip32_index: u32,
    pub minted: bool,
    pub status: ReclaimStatus,
    /// Set when status is Locked or Ready. None when Pending or
    /// Reclaimed.
    pub funded_at_height: Option<u32>,
    /// Blocks remaining until CSV maturity. Zero when Ready, None when
    /// Pending or Reclaimed.
    pub blocks_remaining: Option<u32>,
}

pub fn list_reclaimable_mints(wallet_path: &Path) -> Result<Vec<ReclaimableMint>> {
    let wf = WalletFile::load(wallet_path)?;
    let bd = Bitcoind::connect(&wf.bitcoind)?;
    let tip = bd.block_count()?;

    let mut out = Vec::with_capacity(wf.mints.len());
    for m in &wf.mints {
        let outpoint: OutPoint = parse_outpoint(&m.outpoint)?;
        if m.reclaimed {
            out.push(ReclaimableMint {
                outpoint: m.outpoint.clone(),
                value_sat: m.value_sat,
                lock_blocks: m.lock_blocks,
                bip32_index: m.bip32_index,
                minted: m.minted,
                status: ReclaimStatus::Reclaimed,
                funded_at_height: None,
                blocks_remaining: None,
            });
            continue;
        }
        let (confirmed_at, _) = bd.tx_confirmation(&outpoint.txid)?;
        let (status, funded_at_height, blocks_remaining) = match confirmed_at {
            None => (ReclaimStatus::Pending, None, None),
            Some(h) => {
                // CSV-mature condition: spend tx mineable in block at
                // height >= funded_at + lock_blocks. So the spend can
                // be mined "right now" when tip + 1 >= that bound.
                let unlock_height = h.saturating_add(m.lock_blocks);
                if tip.saturating_add(1) >= unlock_height {
                    (ReclaimStatus::Ready, Some(h), Some(0))
                } else {
                    let remaining = unlock_height.saturating_sub(tip.saturating_add(1));
                    (ReclaimStatus::Locked, Some(h), Some(remaining))
                }
            }
        };
        out.push(ReclaimableMint {
            outpoint: m.outpoint.clone(),
            value_sat: m.value_sat,
            lock_blocks: m.lock_blocks,
            bip32_index: m.bip32_index,
            minted: m.minted,
            status,
            funded_at_height,
            blocks_remaining,
        });
    }
    Ok(out)
}

// ---------- Reclaim: build, sign, broadcast ----------

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ReclaimMintInput {
    /// "<txid>:<vout>" of the mint UTXO to reclaim.
    pub outpoint: String,
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

pub fn reclaim_mint(wallet_path: &Path, input: ReclaimMintInput) -> Result<ReclaimMintOutput> {
    let mut wf = WalletFile::load(wallet_path)?;
    let secp = Secp256k1::new();
    let network = wf.network.into_bitcoin();

    // Find the mint record.
    let record_index = wf
        .mints
        .iter()
        .position(|m| m.outpoint == input.outpoint)
        .ok_or_else(|| anyhow!("no mint record for {}", input.outpoint))?;
    let record = wf.mints[record_index].clone();
    if record.reclaimed {
        bail!("mint {} already reclaimed", input.outpoint);
    }
    let outpoint: OutPoint = parse_outpoint(&record.outpoint)?;

    // Parse destination address with strict network binding.
    let dest = Address::from_str(&input.dest_address)
        .with_context(|| format!("parse destination address {:?}", input.dest_address))?
        .require_network(network)
        .with_context(|| format!("destination address is not on network {network:?}"))?;

    // CSV-maturity check. Same logic as list_reclaimable_mints.
    let bd = Bitcoind::connect(&wf.bitcoind)?;
    let tip = bd.block_count()?;
    let (confirmed_at, _) = bd.tx_confirmation(&outpoint.txid)?;
    let funded_at = confirmed_at
        .ok_or_else(|| anyhow!("mint funding tx {} unconfirmed", outpoint.txid))?;
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

    // Derive the mint key (BIP32) and build the signed reclaim tx.
    let mint_kp = wf.mint_keypair(&secp, record.bip32_index)?;
    let tx = reclaim::build_signed_reclaim_tx(
        &secp,
        &mint_kp,
        outpoint,
        record.value_sat,
        record.lock_blocks,
        &dest,
        input.fee_sat,
    )?;

    let txid = bd.send_raw_transaction(&tx).context("broadcast reclaim tx")?;

    // Mark the mint as reclaimed and persist.
    wf.mints[record_index].reclaimed = true;
    wf.save(wallet_path)?;

    Ok(ReclaimMintOutput {
        txid,
        value_sat_in: record.value_sat,
        value_sat_out: record.value_sat.saturating_sub(input.fee_sat),
        fee_sat: input.fee_sat,
    })
}
