//! SQLite persistence for the node.

use anyhow::{anyhow, Context, Result};
use bitcoin::{OutPoint, Txid};
use hodl_core::block::L2Block;
use hodl_core::state::LedgerState;
use hodl_core::witness::BlockWitness;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use std::str::FromStr;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS blocks (
    height INTEGER PRIMARY KEY,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS state_snapshots (
    l2_height INTEGER PRIMARY KEY,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS block_witnesses (
    height INTEGER PRIMARY KEY,
    json TEXT NOT NULL
);
-- Maps each anchor outpoint to the L1 tx that spent it (i.e., the
-- next attestation tx in the hodlchain chain). Populated by the
-- follower for every ChainAdvance it processes. Powers the Esplora-
-- compatible /tx/:txid/outspend/:vout endpoint used by light clients.
CREATE TABLE IF NOT EXISTS anchor_spends (
    spent_txid TEXT NOT NULL,
    spent_vout INTEGER NOT NULL,
    spender_txid TEXT NOT NULL,
    spender_l1_height INTEGER NOT NULL,
    PRIMARY KEY (spent_txid, spent_vout)
);
"#;

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open sqlite at {}", path.display()))?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn kv_get(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM kv WHERE key = ?1", params![key], |r| r.get(0))
            .optional()?)
    }

    pub fn kv_put(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO kv(key, value) VALUES(?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get_block(&self, height: u32) -> Result<Option<L2Block>> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM blocks WHERE height = ?1",
                params![height],
                |r| r.get(0),
            )
            .optional()?;
        match row {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    pub fn write_block_and_state(
        &mut self,
        block: &L2Block,
        state: &LedgerState,
        witness: &BlockWitness,
    ) -> Result<()> {
        let block_json = serde_json::to_string(block)?;
        let state_json = serde_json::to_string(state)?;
        let witness_json = serde_json::to_string(witness)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO blocks(height, json) VALUES(?1, ?2)",
            params![block.header.height, block_json],
        )?;
        tx.execute(
            "INSERT INTO state_snapshots(l2_height, json) VALUES(?1, ?2)",
            params![block.header.height, state_json],
        )?;
        tx.execute(
            "INSERT INTO block_witnesses(height, json) VALUES(?1, ?2)",
            params![block.header.height, witness_json],
        )?;
        tx.execute(
            "INSERT INTO kv(key, value) VALUES('l2_head_height', ?1) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![block.header.height.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_witness(&self, height: u32) -> Result<Option<BlockWitness>> {
        let row: Option<String> = self
            .conn
            .query_row(
                "SELECT json FROM block_witnesses WHERE height = ?1",
                params![height],
                |r| r.get(0),
            )
            .optional()?;
        match row {
            Some(s) => Ok(Some(serde_json::from_str(&s)?)),
            None => Ok(None),
        }
    }

    pub fn load_latest_state(&self) -> Result<Option<(u32, LedgerState)>> {
        let row: Option<(u32, String)> = self
            .conn
            .query_row(
                "SELECT l2_height, json FROM state_snapshots \
                 ORDER BY l2_height DESC LIMIT 1",
                [],
                |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;
        match row {
            Some((h, s)) => Ok(Some((h, serde_json::from_str(&s)?))),
            None => Ok(None),
        }
    }

    pub fn set_l1_cursor(&self, l1_height: u32) -> Result<()> {
        self.kv_put("l1_cursor", &l1_height.to_string())
    }

    pub fn l1_cursor(&self) -> Result<Option<u32>> {
        Ok(self.kv_get("l1_cursor")?.and_then(|s| s.parse::<u32>().ok()))
    }

    pub fn get_anchor(&self) -> Result<Option<OutPoint>> {
        match self.kv_get("anchor")? {
            None => Ok(None),
            Some(s) => {
                let (txid, vout) = s.split_once(':')
                    .ok_or_else(|| anyhow!("malformed anchor in store: {s}"))?;
                Ok(Some(OutPoint {
                    txid: Txid::from_str(txid)?,
                    vout: vout.parse()?,
                }))
            }
        }
    }

    pub fn set_anchor(&self, op: &OutPoint) -> Result<()> {
        self.kv_put("anchor", &format!("{}:{}", op.txid, op.vout))
    }

    /// Record that `spender_txid` spent `spent` at L1 height `l1_height`.
    /// Used by the follower as it walks the chain, queried by the Esplora-
    /// compatible /outspend endpoint.
    pub fn record_anchor_spend(
        &self,
        spent: &OutPoint,
        spender_txid: &Txid,
        spender_l1_height: u32,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO anchor_spends(spent_txid, spent_vout, spender_txid, spender_l1_height)
             VALUES(?1, ?2, ?3, ?4)
             ON CONFLICT(spent_txid, spent_vout) DO UPDATE SET
                spender_txid = excluded.spender_txid,
                spender_l1_height = excluded.spender_l1_height",
            params![
                spent.txid.to_string(),
                spent.vout,
                spender_txid.to_string(),
                spender_l1_height
            ],
        )?;
        Ok(())
    }

    /// Look up the spender of an outpoint, if known. Returns
    /// (spender_txid, l1_height) or None.
    pub fn get_anchor_spender(
        &self,
        spent_txid: &str,
        spent_vout: u32,
    ) -> Result<Option<(Txid, u32)>> {
        let row: Option<(String, u32)> = self
            .conn
            .query_row(
                "SELECT spender_txid, spender_l1_height
                 FROM anchor_spends WHERE spent_txid = ?1 AND spent_vout = ?2",
                params![spent_txid, spent_vout],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, u32>(1)?)),
            )
            .optional()?;
        match row {
            Some((txid, h)) => Ok(Some((Txid::from_str(&txid)?, h))),
            None => Ok(None),
        }
    }
}
