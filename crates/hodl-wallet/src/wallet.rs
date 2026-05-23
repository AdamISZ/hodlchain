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
    pub current_r: f64,
    pub current_window_atoms: u64,
    pub current_window_start_l1_height: Option<u32>,
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
}
