//! On-disk wallet format.
//!
//! Default path: `./hodl-wallet.json`. Overrideable with `--wallet`.
//! Atomic save: write to `<path>.tmp`, then rename.

use anyhow::{anyhow, Context, Result};
use bitcoin::secp256k1::{Keypair, Secp256k1, SecretKey, XOnlyPublicKey};
use bitcoin::OutPoint;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub use hodl_core::config::{BitcoindAuth, BitcoindConfig, NetworkName};

pub const DEFAULT_WALLET_PATH: &str = "./hodl-wallet.json";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WalletFile {
    pub network: NetworkName,
    pub secret_key_hex: String,
    pub bitcoind: BitcoindConfig,
    pub sequencer_url: String,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub node_url: Option<String>,
    #[serde(default)]
    pub mints: Vec<MintRecord>,
}

pub fn network_from_str(s: &str) -> Result<NetworkName> {
    NetworkName::from_str_ci(s).ok_or_else(|| anyhow!("unknown network: {s}"))
}

/// One CSV-locked mint UTXO we created via this wallet. Persisted so a
/// later `mint-message` can find the proof inputs without re-querying the
/// chain for everything.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MintRecord {
    /// "<txid>:<vout>"
    pub outpoint: String,
    pub value_sat: u64,
    /// Relative locktime baked into L_spend's CSV, in blocks.
    pub lock_blocks: u32,
    /// True once we have submitted a mint message that the sequencer
    /// accepted. Local hint; the sequencer's consumed-nullifier set is
    /// authoritative.
    #[serde(default)]
    pub minted: bool,
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

    pub fn secret_key(&self) -> Result<SecretKey> {
        let bytes = hex::decode(&self.secret_key_hex).context("decode secret_key_hex")?;
        Ok(SecretKey::from_slice(&bytes).context("parse secret_key")?)
    }

    pub fn keypair<C: bitcoin::secp256k1::Signing>(
        &self,
        secp: &Secp256k1<C>,
    ) -> Result<Keypair> {
        Ok(Keypair::from_secret_key(secp, &self.secret_key()?))
    }

    pub fn xonly_pubkey<C: bitcoin::secp256k1::Signing>(
        &self,
        secp: &Secp256k1<C>,
    ) -> Result<XOnlyPublicKey> {
        Ok(self.keypair(secp)?.x_only_public_key().0)
    }

    pub fn upsert_mint(&mut self, record: MintRecord) {
        if let Some(existing) = self.mints.iter_mut().find(|m| m.outpoint == record.outpoint) {
            *existing = record;
        } else {
            self.mints.push(record);
        }
    }

    pub fn find_mint(&self, outpoint: &str) -> Option<&MintRecord> {
        self.mints.iter().find(|m| m.outpoint == outpoint)
    }

    pub fn find_mint_mut(&mut self, outpoint: &str) -> Option<&mut MintRecord> {
        self.mints.iter_mut().find(|m| m.outpoint == outpoint)
    }
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
        ).unwrap();
        assert_eq!(op.vout, 7);
    }
}
