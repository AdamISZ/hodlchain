//! Thin wrapper over `bitcoincore-rpc`.

use anyhow::{anyhow, Context, Result};
use bitcoin::{Address, Amount, ScriptBuf, Transaction, Txid};
use bitcoincore_rpc::{Auth, Client, RpcApi};

use crate::wallet::{BitcoindAuth, BitcoindConfig};

pub struct Bitcoind {
    client: Client,
}

impl Bitcoind {
    pub fn connect(cfg: &BitcoindConfig) -> Result<Self> {
        let auth = match &cfg.auth {
            BitcoindAuth::Cookie { path } => Auth::CookieFile(path.clone()),
            BitcoindAuth::UserPass { user, password } => {
                Auth::UserPass(user.clone(), password.clone())
            }
        };
        let client = Client::new(&cfg.url, auth)
            .with_context(|| format!("connect bitcoind at {}", cfg.url))?;
        Ok(Self { client })
    }

    pub fn block_count(&self) -> Result<u32> {
        let n = self.client.get_block_count()?;
        u32::try_from(n).map_err(|_| anyhow!("block count overflows u32: {n}"))
    }

    /// Send to `address`, then look up the broadcast tx and find the vout
    /// whose scriptPubKey matches `expected_spk`. Returns (txid, vout).
    pub fn send_to_address(
        &self,
        address: &Address,
        amount: Amount,
        expected_spk: &ScriptBuf,
    ) -> Result<(Txid, u32)> {
        let txid = self.client.send_to_address(
            address, amount, None, None, None, None, None, None,
        )?;
        let raw = self.client.get_raw_transaction(&txid, None)?;
        let vout = find_vout(&raw, expected_spk).ok_or_else(|| {
            anyhow!(
                "broadcast tx {txid} has no output matching expected SPK \
                 (this should not happen if the address was derived correctly)"
            )
        })?;
        Ok((txid, vout))
    }

    /// Broadcast a signed transaction. Returns its txid.
    pub fn send_raw_transaction(&self, tx: &Transaction) -> Result<Txid> {
        Ok(self.client.send_raw_transaction(tx)?)
    }

    /// Get the L1 block height at which `txid` was mined, plus the
    /// current tip. Returns (confirmed_at_height, tip_height). Returns
    /// `(None, tip)` if the tx is in the mempool but unconfirmed, and
    /// errors if the tx is unknown.
    pub fn tx_confirmation(&self, txid: &Txid) -> Result<(Option<u32>, u32)> {
        let tip = self.block_count()?;
        let info = self
            .client
            .get_raw_transaction_info(txid, None)
            .with_context(|| format!("get_raw_transaction_info({txid})"))?;
        let confirmed_at = match info.confirmations {
            Some(c) if c > 0 => Some(tip.saturating_sub(c).saturating_add(1)),
            _ => None,
        };
        Ok((confirmed_at, tip))
    }
}

fn find_vout(tx: &Transaction, expected_spk: &ScriptBuf) -> Option<u32> {
    tx.output
        .iter()
        .enumerate()
        .find(|(_, o)| &o.script_pubkey == expected_spk)
        .map(|(i, _)| i as u32)
}
