//! hodl-wallet library.
//!
//! All wallet business logic lives here. UI shells (the CLI in
//! `src/main.rs`; later a Tauri desktop app; potentially others) are
//! intentionally thin — they translate user input into the typed
//! `ops::*` calls below and format the typed outputs back. No UI is
//! ever expected to reimplement a wallet operation; doing so is a
//! refactor smell.
//!
//! Module layout:
//!   - `wallet`    — on-disk wallet file (keys, mints, verified head).
//!   - `api`       — HTTP client for the sequencer and node.
//!   - `bitcoind`  — bitcoind RPC wrapper (for sending the L1 mint UTXO).
//!   - `esplora`   — Esplora HTTP client + L1 attestation-chain walker.
//!   - `verify`    — sparse stateless light-balance verifier.
//!   - `ops`       — typed UI-agnostic operation surface (the public API).

pub mod api;
pub mod bitcoind;
pub mod esplora;
pub mod ops;
pub mod reclaim;
pub mod verify;
pub mod wallet;
