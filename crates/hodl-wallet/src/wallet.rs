//! On-disk wallet format.
//!
//! Default path: `./hodl-wallet.json`. Overrideable with `--wallet`.
//! Atomic save: write to `<path>.tmp`, then rename.
//!
//! ## Key model
//!
//! The wallet stores a **BIP39 24-word mnemonic**. All operational
//! keys are derived from it via BIP32 under hodlchain-specific BIP44
//! paths:
//!
//! ```text
//! m / HODL' / coin_type' / account' / 0 / index
//! ```
//!
//! where:
//! - `HODL' = 1213154380'` (= 0x484F444C) — the ASCII bytes
//!   `'H' 'O' 'D' 'L'` interpreted as a big-endian u32, then
//!   hardened. Hodlchain-specific purpose word, deliberately distinct
//!   from BIP86 (which is reserved for plain P2TR receive keys; our
//!   L1 keys go into a custom CSV-locked script and never appear as
//!   P2TR receive addresses).
//! - `coin_type'` — SLIP-44: 0' on mainnet, 1' on testnet/signet/regtest.
//! - `account'` — 0' for L1 mint keys (a stream of one-shot keys, one
//!   per mint UTXO), 1' for the L2 identity (a single stable key
//!   serving as the L2 receive address + transfer signing key).
//! - `index` — for L1 mint keys, equals the per-wallet
//!   `next_mint_index` counter at the time the mint UTXO was created
//!   (stored in `MintRecord.bip32_index`). For the L2 identity, always 0.
//!
//! ## Wallet format note
//!
//! This is a hard break from any earlier `secret_key_hex` format.
//! Old wallet files cannot be loaded; regenerate with `keygen`.

use anyhow::{anyhow, Context, Result};
use bip39::Mnemonic;
use bitcoin::bip32::{DerivationPath, Xpriv};
use bitcoin::secp256k1::{Keypair, Secp256k1, XOnlyPublicKey};
use bitcoin::OutPoint;
use hodl_core::hash::H256;
use hodl_core::smt::LeafKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::str::FromStr;

pub use hodl_core::config::NetworkName;

pub const DEFAULT_WALLET_PATH: &str = "./hodl-wallet.json";

/// Hodlchain BIP44 purpose word: ASCII bytes `'H','O','D','L'` as a
/// big-endian u32 (= 0x484F444C = 1_213_154_380). Hardened in
/// derivation paths via the `'` suffix in the string form.
pub const HODLCHAIN_PURPOSE: u32 = 0x484F444C;

/// L1 confirmation depth at which the wallet considers a record
/// `Finalized`. Mirror of the same-named constant in
/// `hodl_sequencer::producer` — the sequencer uses it to decide when
/// to stop tracking pending L1 attestations; the wallet uses it for
/// the analogous "no more reorg concern" judgement on TxRecords.
///
/// Keep these in sync. There's no automated cross-crate check today
/// because the sequencer's value is a private `const`; mismatching
/// them would mean the wallet shows finalisation earlier or later
/// than the sequencer trusts the same depth, which is cosmetic but
/// confusing. Promotion to a shared `hodl-core` constant would
/// remove the foot-gun and is a sensible follow-up.
pub const REORG_FINALITY_DEPTH: u32 = 2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletFile {
    pub network: NetworkName,
    /// BIP39 mnemonic (24 words by default).
    pub mnemonic: String,
    /// L2 sequencer base URL — submit endpoint for mint messages
    /// and transfers.
    pub sequencer_url: String,
    /// Optional L2 follower (node) base URL — used for block/witness
    /// queries that the light verifier needs.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub node_url: Option<String>,
    /// Esplora HTTP base URL — *required*. Sole L1 data source for
    /// the wallet (the wallet never speaks bitcoind directly). Points
    /// at mempool.space / a self-hosted electrs / hodl-node (which
    /// proxies the Esplora subset over a local bitcoind).
    pub esplora_url: String,
    /// Next derivation index for L1 mint keys. Incremented by every
    /// successful `mint-utxo` call. Per-mint keypair lookup goes via
    /// `MintRecord.bip32_index`, so the counter is just for monotonic
    /// allocation.
    #[serde(default)]
    pub next_mint_index: u32,
    #[serde(default)]
    pub mints: Vec<MintRecord>,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub verified_head: Option<VerifiedHead>,
    /// Append-only transaction history. Stored chronologically (oldest
    /// first); UIs reverse for display. Backed by a separate vec from
    /// `mints` because MintRecord is the operational source-of-truth
    /// for the reclaim flow and TxRecord is purely a presentation/
    /// observability layer — they cross-reference via
    /// `MintRecord.bip32_index` and `TxRecord.bip32_index`.
    ///
    /// `#[serde(default)]` so wallets created before this field load
    /// with an empty history.
    #[serde(default)]
    pub transactions: Vec<TxRecord>,
}

pub fn network_from_str(s: &str) -> Result<NetworkName> {
    NetworkName::from_str_ci(s).ok_or_else(|| anyhow!("unknown network: {s}"))
}

/// One CSV-locked mint UTXO this wallet is tracking.
///
/// State machine (each row is a strict superset of the previous):
///
///   1. **Created**: `mint_address` + `lock_blocks` + `bip32_index`.
///      The wallet has derived the L1 mint key, computed the
///      deposit address, and shown it to the user. No on-chain
///      activity yet.
///   2. **Funded**: above + `outpoint` + `value_sat` +
///      `funded_at_height`. An external wallet sent BTC to
///      `mint_address`; we discovered the UTXO via Esplora.
///   3. **Minted**: above + `minted = true`. The mint message was
///      submitted and the sequencer accepted it.
///   4. **Reclaimed**: above + `reclaimed = true`. The CSV-locked
///      UTXO was spent back to a user destination.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MintRecord {
    /// L1 address (bech32m P2TR) that the user funds. Derived from
    /// `(bip32_index, lock_blocks, network)` and stable for the life
    /// of the record. The user-facing identifier for a mint.
    pub mint_address: String,
    /// Relative locktime baked into L_spend's CSV, in blocks.
    pub lock_blocks: u32,
    /// BIP32 derivation index of the L1 mint key under
    /// `m/HODL'/coin_type'/0'/0/<index>`. Needed for both the mint
    /// message signature and the CSV reclaim signature.
    pub bip32_index: u32,
    /// Funding outpoint "<txid>:<vout>". Populated once a UTXO at
    /// `mint_address` is observed via Esplora.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub outpoint: Option<String>,
    /// Funding value in sats. Populated alongside `outpoint`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub value_sat: Option<u64>,
    /// L1 height the funding tx was confirmed at. Populated alongside
    /// `outpoint`. Used by the reclaim flow to check CSV maturity.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub funded_at_height: Option<u32>,
    /// True once we have submitted a mint message that the sequencer
    /// accepted. Local hint; the sequencer's consumed-nullifier set
    /// is authoritative.
    #[serde(default)]
    pub minted: bool,
    /// True once we have broadcast a reclaim transaction. Local hint;
    /// Esplora is authoritative.
    #[serde(default)]
    pub reclaimed: bool,
}

pub fn parse_outpoint(s: &str) -> Result<OutPoint> {
    let (txid, vout) = s.split_once(':').ok_or_else(|| anyhow!("expected txid:vout"))?;
    let txid: bitcoin::Txid = txid.parse().context("invalid txid")?;
    let vout: u32 = vout.parse().context("invalid vout")?;
    Ok(OutPoint { txid, vout })
}

// ---------- Transaction history ----------
//
// User-facing event log. Captures L1 deposits, L2 mint messages,
// outgoing/incoming L2 transfers, and L1 reclaim broadcasts. See
// the planning notes (this commit's PR / docs) for the full design.

/// The kind of event recorded in `TxRecord`.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TxKind {
    /// Wallet broadcast BTC to a mint deposit address (L1).
    L1Deposit,
    /// Wallet broadcast a reclaim transaction spending a CSV-matured
    /// mint UTXO back to a user destination (L1).
    L1Reclaim,
    /// Wallet submitted a mint message to the sequencer (L2). The
    /// downstream effect — atoms credited to `l2_destination` —
    /// becomes visible once the message lands in an L2 block.
    L2MintApply,
    /// Wallet submitted an outbound transfer (L2).
    L2TransferSent,
    /// An inbound transfer to this wallet's L2 address was observed
    /// while walking blocks. Born at `InBlock` status (we never
    /// see it as `Pending` because we didn't initiate it).
    L2TransferReceived,
}

/// Lifecycle state of a `TxRecord`.
///
/// The pre-block state intentionally splits L1 vs L2 because the two
/// have materially different reliability profiles:
///
///   - `Soft` (L2 only): the sequencer has accepted the message and
///     applied it to its in-memory state. The soft balance reflects
///     it immediately. Barring sequencer crash or an L1 reorg of the
///     underlying deposit (for mints), it will land in the next L2
///     block (~30s under current cadence).
///   - `L1Mempool` (L1 only): broadcast to Bitcoin's p2p network.
///     No commitment from anyone — may be evicted by fee policy,
///     RBF-replaced, or never mined.
///
/// Records transition forward as new evidence arrives and terminally
/// land in `Finalized` or `Failed`. Reorgs can move `InBlock` back
/// to `Soft` / `L1Mempool` during the `REORG_FINALITY_DEPTH` window;
/// nothing ever leaves `Finalized`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TxStatus {
    /// L2 only — sequencer has accepted, soft-credited in its state.
    /// Used by `L2MintApply` and `L2TransferSent`.
    Soft {
        /// Unix-seconds when the sequencer accepted the message.
        since_ts: u64,
    },
    /// L1 only — broadcast to Bitcoin mempool, not yet confirmed.
    /// Used by `L1Deposit` and `L1Reclaim`.
    L1Mempool {
        /// Unix-seconds when the broadcast was made.
        since_ts: u64,
    },
    /// Observed in a block. For L2 records this means an L2 block
    /// (not necessarily L1-anchored yet); for L1 records this
    /// means 1+ L1 confirmations.
    InBlock {
        /// L2 block height the tx landed in. `0` for pure-L1 records
        /// (deposit / reclaim confirmations).
        l2_height: u32,
        /// L1 height at which the inclusion (or for L1 records, the
        /// confirmation) was observed.
        l1_height: u32,
        /// Unix-seconds when the transition was observed locally.
        included_ts: u64,
    },
    /// For L2 records: containing L2 block is L1-anchored past
    /// `REORG_FINALITY_DEPTH`. For L1 records: confirmation depth
    /// past the same threshold.
    Finalized {
        l2_height: u32,
        l1_height: u32,
    },
    /// Sequencer rejected, broadcast failed, witness re-verify
    /// failed during walk-forward, etc.
    Failed {
        reason: String,
        ts: u64,
    },
}

/// One event in the wallet's transaction history.
///
/// This struct is presentation-shaped, not consensus-shaped — multiple
/// `TxRecord`s may describe a single logical operation (e.g. an
/// `L1Deposit` plus an `L2MintApply` for one mint cycle), linked via
/// `bip32_index`. The full state of a CSV-locked mint UTXO still
/// lives in `MintRecord`; TxRecord is for observability only.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxRecord {
    /// Stable local identifier. 16 hex chars from a random u64;
    /// uniqueness within this wallet file is the only requirement.
    pub id: String,
    pub kind: TxKind,
    /// Unix-seconds when this record was first created (submission
    /// time for outbound, discovery time for inbound).
    pub created_ts: u64,
    /// Amount in the natural unit: sat for L1 records, atoms for L2.
    pub amount: u64,
    /// Per-transfer protocol fee in atoms. Populated for
    /// `L2TransferSent` only.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fee_atoms: Option<u64>,
    /// L1 miner fee in sat. Populated for `L1Reclaim` only.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub fee_sat: Option<u64>,
    /// The "other side" of the tx:
    ///   - L1Deposit: the bech32m mint address (always our own).
    ///   - L1Reclaim: the destination bech32 address.
    ///   - L2MintApply: our own L2 address (hex xonly pubkey).
    ///   - L2TransferSent: the recipient (hex xonly).
    ///   - L2TransferReceived: the sender (hex xonly).
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub counterparty: Option<String>,
    pub status: TxStatus,
    /// L1 txid (hex) for `L1Deposit` and `L1Reclaim`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub l1_txid: Option<String>,
    /// Hex-encoded `body.sighash()` for `L2TransferSent` (so the
    /// walk-forward path can match the in-block transfer against
    /// this record), or the nullifier_hex for `L2MintApply`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub l2_sighash: Option<String>,
    /// BIP32 index linking back to a `MintRecord` for L1Deposit /
    /// L1Reclaim / L2MintApply records.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub bip32_index: Option<u32>,
    /// User-supplied free-form note. Reserved for a future UX
    /// feature; always `None` today.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub note: Option<String>,
}

/// Generate a 16-hex-char random id for a `TxRecord`. Uses
/// `rand::thread_rng()` via the bitcoin crate's re-export.
pub fn new_tx_id() -> String {
    use bitcoin::secp256k1::rand::RngCore;
    let mut buf = [0u8; 8];
    bitcoin::secp256k1::rand::thread_rng().fill_bytes(&mut buf);
    format!("{:016x}", u64::from_be_bytes(buf))
}

/// Convenience: current unix-seconds, saturating to 0 if the clock
/// is set before 1970 (unlikely outside of integration tests with
/// frozen clocks).
pub fn now_ts() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl WalletFile {
    pub fn load(path: &Path) -> Result<Self> {
        let s = fs::read_to_string(path)
            .with_context(|| format!("read wallet file {}", path.display()))?;
        Ok(serde_json::from_str(&s)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let tmp = path.with_extension("json.tmp");
        let data = serde_json::to_vec_pretty(self)?;
        fs::write(&tmp, &data)
            .with_context(|| format!("write wallet tmp file {}", tmp.display()))?;
        fs::rename(&tmp, path)
            .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
        Ok(())
    }

    /// Parsed BIP39 mnemonic.
    fn mnemonic_parsed(&self) -> Result<Mnemonic> {
        Mnemonic::from_str(self.mnemonic.trim()).context("parse wallet mnemonic")
    }

    /// BIP39 seed bytes (PBKDF2 over the mnemonic + optional passphrase).
    /// We do not currently support passphrases; the empty string is
    /// the standard "no passphrase" input.
    pub fn seed(&self) -> Result<[u8; 64]> {
        Ok(self.mnemonic_parsed()?.to_seed(""))
    }

    /// BIP32 master extended private key derived from the seed.
    pub fn master_xpriv(&self) -> Result<Xpriv> {
        let seed = self.seed()?;
        Xpriv::new_master(self.network.into_bitcoin(), &seed).context("derive master xpriv")
    }

    /// Derivation path for an L1 mint key at the given index.
    /// `m / HODL' / coin_type' / 0' / 0 / index`
    pub fn mint_key_path(&self, index: u32) -> Result<DerivationPath> {
        let coin = self.network.slip44_coin_type();
        DerivationPath::from_str(&format!(
            "m/{}'/{}'/0'/0/{}",
            HODLCHAIN_PURPOSE, coin, index
        ))
        .context("build mint-key derivation path")
    }

    /// Derivation path for the L2 identity key.
    /// `m / HODL' / coin_type' / 1' / 0 / 0`
    pub fn l2_identity_path(&self) -> Result<DerivationPath> {
        let coin = self.network.slip44_coin_type();
        DerivationPath::from_str(&format!("m/{}'/{}'/1'/0/0", HODLCHAIN_PURPOSE, coin))
            .context("build L2-identity derivation path")
    }

    /// L1 mint keypair at a specific index. Caller is responsible for
    /// supplying the right index (from `MintRecord.bip32_index`).
    pub fn mint_keypair<C: bitcoin::secp256k1::Signing>(
        &self,
        secp: &Secp256k1<C>,
        index: u32,
    ) -> Result<Keypair> {
        let master = self.master_xpriv()?;
        let path = self.mint_key_path(index)?;
        let derived = master.derive_priv(secp, &path).context("derive mint key")?;
        Ok(Keypair::from_secret_key(secp, &derived.private_key))
    }

    /// Stable L2 identity keypair. Same path on every call.
    pub fn l2_identity_keypair<C: bitcoin::secp256k1::Signing>(
        &self,
        secp: &Secp256k1<C>,
    ) -> Result<Keypair> {
        let master = self.master_xpriv()?;
        let path = self.l2_identity_path()?;
        let derived = master.derive_priv(secp, &path).context("derive L2 identity key")?;
        Ok(Keypair::from_secret_key(secp, &derived.private_key))
    }

    /// Allocate the next L1 mint index and return the corresponding
    /// keypair. Does *not* save the wallet — the caller is expected to
    /// persist alongside whatever side-effects (mint record creation
    /// etc.) the operation produces.
    pub fn allocate_mint_keypair<C: bitcoin::secp256k1::Signing>(
        &mut self,
        secp: &Secp256k1<C>,
    ) -> Result<(Keypair, u32)> {
        let index = self.next_mint_index;
        let kp = self.mint_keypair(secp, index)?;
        self.next_mint_index = self
            .next_mint_index
            .checked_add(1)
            .ok_or_else(|| anyhow!("next_mint_index overflow"))?;
        Ok((kp, index))
    }

    /// Convenience: the L2 identity x-only pubkey. This is the L2
    /// address shown to other users, the destination of mint
    /// messages, and the signer of transfers.
    pub fn xonly_pubkey<C: bitcoin::secp256k1::Signing>(
        &self,
        secp: &Secp256k1<C>,
    ) -> Result<XOnlyPublicKey> {
        Ok(self.l2_identity_keypair(secp)?.x_only_public_key().0)
    }

    /// Alias for `xonly_pubkey` — clearer at call sites that handle
    /// the L2 identity specifically.
    pub fn l2_identity_xonly<C: bitcoin::secp256k1::Signing>(
        &self,
        secp: &Secp256k1<C>,
    ) -> Result<XOnlyPublicKey> {
        self.xonly_pubkey(secp)
    }

    /// Append a freshly-created mint record. Each mint gets a unique
    /// `bip32_index`, so duplicates here would be a bug. Caller is
    /// expected to allocate the index via `allocate_mint_keypair`
    /// before calling.
    pub fn append_mint(&mut self, record: MintRecord) {
        debug_assert!(
            !self.mints.iter().any(|m| m.bip32_index == record.bip32_index),
            "mint at bip32_index {} already exists",
            record.bip32_index
        );
        self.mints.push(record);
    }

    /// Append a transaction-history record. The caller is responsible
    /// for `wf.save()` afterwards; we don't auto-save because most
    /// ops batch this with other state mutations (e.g. flipping
    /// `MintRecord.minted = true`) and the save should be one shot.
    pub fn append_tx(&mut self, record: TxRecord) {
        self.transactions.push(record);
    }

    /// Find a `TxRecord` by its local id (the 16-hex string returned
    /// by `new_tx_id`). Returned for the walk-forward path to flip
    /// status on already-tracked records.
    pub fn find_tx_by_id_mut(&mut self, id: &str) -> Option<&mut TxRecord> {
        self.transactions.iter_mut().find(|t| t.id == id)
    }

    /// Find the first `L1Deposit` record for the given `bip32_index`.
    /// Used by `check_mint_funding` to avoid creating a duplicate
    /// record on repeat polls once we already know about the deposit.
    pub fn find_l1_deposit_tx_mut(&mut self, bip32_index: u32) -> Option<&mut TxRecord> {
        self.transactions
            .iter_mut()
            .find(|t| matches!(t.kind, TxKind::L1Deposit) && t.bip32_index == Some(bip32_index))
    }

    /// Look up a mint by its BIP32 derivation index — the canonical
    /// stable identifier (the deposit address is also unique but the
    /// index is shorter and used at every CLI / UI boundary).
    pub fn find_mint_by_index(&self, index: u32) -> Option<&MintRecord> {
        self.mints.iter().find(|m| m.bip32_index == index)
    }

    pub fn find_mint_by_index_mut(&mut self, index: u32) -> Option<&mut MintRecord> {
        self.mints.iter_mut().find(|m| m.bip32_index == index)
    }

    /// Look up a mint by its deposit address. Useful at funding-watch
    /// time when the index isn't already in hand.
    pub fn find_mint_by_address(&self, addr: &str) -> Option<&MintRecord> {
        self.mints.iter().find(|m| m.mint_address == addr)
    }
}

/// Persistent state of the wallet's incremental light-balance
/// verification. Captured after each successful light-balance run.
///
/// The wallet only carries *its own* leaf and SMT path — full state
/// stays at the sequencer/node. Per new block, the wallet uses the
/// block witness to recompute the post-block accounts_root sparsely.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VerifiedHead {
    pub state_root: H256,
    pub accounts_root: H256,
    pub l2_height: u32,
    pub block_hash: H256,
    pub l1_height: u32,
    pub anchor_outpoint: OutPoint,
    pub own_address: XOnlyPublicKey,
    pub own_leaf: LeafKind,
    pub own_path: Vec<H256>,
    pub consumed_nullifiers: BTreeSet<String>,
    /// Running tally of all atoms ever minted on the chain. Seeded
    /// from `BalanceResponse.total_minted_atoms` at cold-start
    /// bootstrap (sequencer-trusted), then accumulated locally
    /// from block witnesses during walk-forward. Not part of any
    /// state-root commitment — purely for stats display.
    #[serde(default)]
    pub total_minted_atoms: u64,
    /// L2 address that receives per-transfer fees on this chain.
    /// Immutable from genesis, so we just carry it forward across
    /// sparse-walk steps and feed it back into `StateComponents`
    /// when recomputing the post-block state_root. `None` means
    /// fees are burned.
    #[serde(default)]
    pub sequencer_fee_address: Option<XOnlyPublicKey>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_name_roundtrip() {
        for n in ["bitcoin", "regtest", "signet", "testnet"] {
            let parsed = NetworkName::from_str_ci(n).unwrap();
            let s = serde_json::to_string(&parsed).unwrap();
            assert!(s.contains(n));
        }
    }

    #[test]
    fn outpoint_parses() {
        let op = parse_outpoint(
            "0000000000000000000000000000000000000000000000000000000000000000:7",
        )
        .unwrap();
        assert_eq!(op.vout, 7);
    }

    #[test]
    fn hodl_purpose_constant() {
        // ASCII 'H','O','D','L' as big-endian u32 = 0x484F444C = 1_213_154_380.
        assert_eq!(HODLCHAIN_PURPOSE, u32::from_be_bytes(*b"HODL"));
        assert_eq!(HODLCHAIN_PURPOSE, 0x484F444C);
        assert_eq!(HODLCHAIN_PURPOSE, 1_213_154_380);
    }

    fn make_test_wallet(network: NetworkName) -> WalletFile {
        let mn = Mnemonic::generate(24).unwrap().to_string();
        WalletFile {
            network,
            mnemonic: mn,
            sequencer_url: "http://localhost:0".into(),
            node_url: None,
            esplora_url: "http://localhost:0".into(),
            next_mint_index: 0,
            mints: vec![],
            verified_head: None,
            transactions: vec![],
        }
    }

    #[test]
    fn l2_identity_key_is_stable() {
        let wf = make_test_wallet(NetworkName::Regtest);
        let secp = Secp256k1::new();
        let a = wf.l2_identity_xonly(&secp).unwrap();
        let b = wf.l2_identity_xonly(&secp).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn mint_keys_differ_per_index() {
        let wf = make_test_wallet(NetworkName::Regtest);
        let secp = Secp256k1::new();
        let k0 = wf.mint_keypair(&secp, 0).unwrap().x_only_public_key().0;
        let k1 = wf.mint_keypair(&secp, 1).unwrap().x_only_public_key().0;
        let k2 = wf.mint_keypair(&secp, 2).unwrap().x_only_public_key().0;
        assert_ne!(k0, k1);
        assert_ne!(k1, k2);
        assert_ne!(k0, k2);
    }

    #[test]
    fn mint_keys_differ_from_l2_identity() {
        let wf = make_test_wallet(NetworkName::Regtest);
        let secp = Secp256k1::new();
        let l2 = wf.l2_identity_xonly(&secp).unwrap();
        let m0 = wf.mint_keypair(&secp, 0).unwrap().x_only_public_key().0;
        assert_ne!(l2, m0);
    }

    #[test]
    fn allocate_mint_increments_counter() {
        let mut wf = make_test_wallet(NetworkName::Regtest);
        let secp = Secp256k1::new();
        assert_eq!(wf.next_mint_index, 0);
        let (_, i) = wf.allocate_mint_keypair(&secp).unwrap();
        assert_eq!(i, 0);
        assert_eq!(wf.next_mint_index, 1);
        let (_, j) = wf.allocate_mint_keypair(&secp).unwrap();
        assert_eq!(j, 1);
        assert_eq!(wf.next_mint_index, 2);
    }

    #[test]
    fn allocate_then_recall_match() {
        let mut wf = make_test_wallet(NetworkName::Regtest);
        let secp = Secp256k1::new();
        let (kp, idx) = wf.allocate_mint_keypair(&secp).unwrap();
        let again = wf.mint_keypair(&secp, idx).unwrap();
        assert_eq!(kp.secret_key(), again.secret_key());
    }

    // ---------- TxRecord / TxStatus / TxKind ----------

    #[test]
    fn tx_id_is_16_hex_chars_and_unique() {
        let a = new_tx_id();
        let b = new_tx_id();
        assert_eq!(a.len(), 16);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // Birthday paradox at 64 bits: two consecutive calls colliding
        // is ~2^-64. Probabilistic but fine.
        assert_ne!(a, b);
    }

    #[test]
    fn tx_record_roundtrips_through_serde_for_every_status_variant() {
        let cases = [
            TxStatus::Soft { since_ts: 1_700_000_000 },
            TxStatus::L1Mempool { since_ts: 1_700_000_000 },
            TxStatus::InBlock {
                l2_height: 42,
                l1_height: 1234,
                included_ts: 1_700_000_500,
            },
            TxStatus::Finalized { l2_height: 42, l1_height: 1234 },
            TxStatus::Failed {
                reason: "non-BIP68-final".into(),
                ts: 1_700_000_600,
            },
        ];
        for status in cases {
            let rec = TxRecord {
                id: new_tx_id(),
                kind: TxKind::L2TransferSent,
                created_ts: 1_700_000_000,
                amount: 12_345,
                fee_atoms: Some(100),
                fee_sat: None,
                counterparty: Some("ab".repeat(32)),
                status,
                l1_txid: None,
                l2_sighash: Some("cd".repeat(32)),
                bip32_index: None,
                note: None,
            };
            let json = serde_json::to_string(&rec).unwrap();
            let back: TxRecord = serde_json::from_str(&json).unwrap();
            // Spot-check rather than full structural equality (would
            // need PartialEq on the enum; not worth pulling in for
            // a serde sanity check).
            assert_eq!(back.id, rec.id);
            assert_eq!(back.kind, rec.kind);
            assert_eq!(back.amount, rec.amount);
        }
    }

    #[test]
    fn tx_kind_serialises_to_snake_case() {
        let json = serde_json::to_string(&TxKind::L2TransferReceived).unwrap();
        assert_eq!(json, "\"l2_transfer_received\"");
    }

    #[test]
    fn tx_status_serialises_with_internal_tag() {
        let s = TxStatus::Soft { since_ts: 7 };
        let json = serde_json::to_string(&s).unwrap();
        // serde tag = "kind" makes the discriminator explicit, which
        // is what the UI keys on for status pills.
        assert!(json.contains("\"kind\":\"soft\""));
        assert!(json.contains("\"since_ts\":7"));

        let s = TxStatus::L1Mempool { since_ts: 9 };
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"kind\":\"l1_mempool\""));
    }

    #[test]
    fn wallet_file_loads_with_empty_transactions_when_field_missing() {
        // Old wallets serialised before `transactions` existed must
        // still load. The default attribute is what makes this work.
        let old_json = r#"{
            "network": "regtest",
            "mnemonic": "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about",
            "sequencer_url": "http://127.0.0.1:8081",
            "esplora_url": "http://127.0.0.1:8080",
            "next_mint_index": 0,
            "mints": []
        }"#;
        let wf: WalletFile = serde_json::from_str(old_json).unwrap();
        assert!(wf.transactions.is_empty());
    }
}
