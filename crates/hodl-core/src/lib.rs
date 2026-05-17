//! hodl-core: shared types and consensus rules for the hodlcoin POC.
//!
//! See `docs/design.md` and `docs/issuance.tex`.
//!
//! Compiles with `default-features = false` (no `std`) for use inside
//! SP1 zk-programs: the state machine, SMT, hashing, types, and proof
//! verification are all `no_std`. The `std` feature additionally pulls
//! in daemon-side helpers (`config`, `schemas`, the utoipa OpenAPI
//! derives) that aren't needed inside the prover.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod block;
#[cfg(feature = "std")]
pub mod config;
pub mod consensus;
pub mod hash;
pub mod l1;
pub mod op_return;
pub mod proof;
pub mod rpc;
#[cfg(feature = "std")]
pub mod schemas;
pub mod smt;
pub mod state;
pub mod tx;

pub use hash::H256;
