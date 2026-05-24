//! SQLite persistence for the sequencer.
//!
//! Schema:
//!   kv(key TEXT PRIMARY KEY, value TEXT NOT NULL)
//!   blocks(height INT PRIMARY KEY, json TEXT NOT NULL, attested_txid TEXT)
//!   state_snapshots(l2_height INT PRIMARY KEY, json TEXT NOT NULL)

use anyhow::{Context, Result};
use bitcoin::{OutPoint, Txid};
use hodl_core::block::L2Block;
use hodl_core::state::LedgerState;
use hodl_core::witness::BlockWitness;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;

/// Reorg-tracking record for one attestation tx the sequencer has
/// posted to L1. The producer keeps a list of these in the store
/// until each tx has reached the L1 confirmation depth required for
/// finality (`REORG_FINALITY_DEPTH`). Pre-finalisation we poll
/// bitcoind on each L1 tick; on detection that the tx has been
/// reorged out and lost, the sequencer reverts the chain anchor to
/// `spent_anchor` so the next post can chain from there.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PendingAttestation {
    /// The attestation tx's own txid.
    pub txid: String,
    /// The anchor outpoint this tx spent (`txid:vout` string form).
    pub spent_anchor: String,
    /// The new anchor outpoint this tx created (`txid:vout` form).
    pub new_anchor: String,
    /// L2 head height this attestation committed to.
    pub l2_head_height: u32,
    /// L1 height at which the sequencer posted this attestation (NOT
    /// necessarily the height it landed in — that's what we poll
    /// bitcoind to find out).
    pub posted_at_l1_height: u32,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS blocks (
    height INTEGER PRIMARY KEY,
    json TEXT NOT NULL,
    attested_txid TEXT
);
CREATE TABLE IF NOT EXISTS state_snapshots (
    l2_height INTEGER PRIMARY KEY,
    json TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS block_witnesses (
    height INTEGER PRIMARY KEY,
    json TEXT NOT NULL
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

    #[allow(dead_code)]
    pub fn get_attested_txid(&self, height: u32) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT attested_txid FROM blocks WHERE height = ?1",
                params![height],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
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

    #[allow(dead_code)] // Pre-Phase-2 per-L2-block attestation
                        // tracking; kept as a debugging aid.
    pub fn set_attested_txid(&self, height: u32, txid: &Txid) -> Result<()> {
        self.conn.execute(
            "UPDATE blocks SET attested_txid = ?1 WHERE height = ?2",
            params![txid.to_string(), height],
        )?;
        Ok(())
    }

    /// Lowest L2 block height that should-have-but-doesn't-have an L1
    /// attestation. Genesis (height 0) is never attested by design (it
    /// is the chain root, not a chain link), so this skips height 0.
    #[allow(dead_code)] // Pre-Phase-2: no longer used since
                        // attestation is now per-L1-block, not per-L2-block.
    pub fn latest_unattested_height(&self) -> Result<Option<u32>> {
        Ok(self
            .conn
            .query_row(
                "SELECT MIN(height) FROM blocks \
                 WHERE attested_txid IS NULL AND height > 0",
                [],
                |r| r.get::<_, Option<u32>>(0),
            )
            .optional()?
            .flatten())
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

    #[allow(dead_code)]
    pub fn head_height(&self) -> Result<Option<u32>> {
        Ok(self.kv_get("l2_head_height")?
            .and_then(|s| s.parse::<u32>().ok()))
    }

    pub fn set_l1_cursor(&self, l1_height: u32) -> Result<()> {
        self.kv_put("l1_cursor", &l1_height.to_string())
    }

    pub fn l1_cursor(&self) -> Result<Option<u32>> {
        Ok(self.kv_get("l1_cursor")?.and_then(|s| s.parse::<u32>().ok()))
    }

    /// The L1 outpoint that the *next* attestation tx must spend.
    /// Initialised at genesis from the wallet's largest UTXO.
    pub fn get_anchor(&self) -> Result<Option<OutPoint>> {
        match self.kv_get("anchor")? {
            None => Ok(None),
            Some(s) => {
                let (txid, vout) = s.split_once(':')
                    .ok_or_else(|| anyhow::anyhow!("malformed anchor in store: {s}"))?;
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

    /// The highest L1 height for which we've successfully posted an
    /// L1 attestation. Used to drive "post one attestation per new
    /// L1 block" semantics — when L1 advances past this value, the
    /// producer posts a new attestation covering the current L2 head.
    pub fn last_attested_l1_height(&self) -> Result<Option<u32>> {
        Ok(self.kv_get("last_attested_l1_height")?
            .and_then(|s| s.parse::<u32>().ok()))
    }

    pub fn set_last_attested_l1_height(&self, l1_height: u32) -> Result<()> {
        self.kv_put("last_attested_l1_height", &l1_height.to_string())
    }

    /// Sequencer L2 identity secret key (hex-encoded 32 bytes).
    /// Generated on first chain init; persisted across restarts.
    /// Used to sign soft-confirmation receipts and to identify the
    /// block producer in each L2 block header.
    pub fn sequencer_seckey_hex(&self) -> Result<Option<String>> {
        self.kv_get("sequencer_seckey")
    }

    pub fn set_sequencer_seckey_hex(&self, hex: &str) -> Result<()> {
        self.kv_put("sequencer_seckey", hex)
    }

    /// Read the list of pending (unfinalised) attestations. Empty
    /// list if none. JSON-encoded in a single kv entry — simpler
    /// than a new table for a list that's bounded by
    /// `REORG_FINALITY_DEPTH` × attestation cadence (a handful of
    /// entries at any time).
    pub fn pending_attestations(&self) -> Result<Vec<PendingAttestation>> {
        match self.kv_get("pending_attestations")? {
            None => Ok(Vec::new()),
            Some(s) => Ok(serde_json::from_str(&s)?),
        }
    }

    pub fn set_pending_attestations(&self, list: &[PendingAttestation]) -> Result<()> {
        self.kv_put("pending_attestations", &serde_json::to_string(list)?)
    }
}
