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

}

fn find_vout(tx: &Transaction, expected_spk: &ScriptBuf) -> Option<u32> {
    tx.output
        .iter()
        .enumerate()
        .find(|(_, o)| &o.script_pubkey == expected_spk)
        .map(|(i, _)| i as u32)
}
