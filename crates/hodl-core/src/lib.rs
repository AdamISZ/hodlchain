//! hodl-core: shared types and consensus rules for the hodlchain POC.
//!
//! See `docs/design.md` and the design paper at
//! <https://github.com/AdamISZ/hodlchain-paper>.
//!
//! Compiles with `default-features = false` (no `std`): the state
//! machine, SMT, hashing, types, and proof verification are all
//! `no_std`-compatible. The `std` feature additionally pulls in
//! daemon-side helpers (`config`, `schemas`, the utoipa OpenAPI
//! derives). The split was originally added to support an SP1
//! zkVM build target (since removed — see
//! `docs/zk-design-discussion.md`); the no_std capability has been
//! kept since it imposes no runtime cost on daemons.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

#[cfg(feature = "std")]
pub mod address;
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
pub mod witness;

pub use hash::H256;
