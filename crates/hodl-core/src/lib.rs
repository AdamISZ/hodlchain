//! hodl-core: shared types and consensus rules for the hodlcoin POC.
//!
//! See `docs/design.md` and `docs/issuance.tex`.

pub mod block;
pub mod config;
pub mod consensus;
pub mod hash;
pub mod l1;
pub mod op_return;
pub mod proof;
pub mod rpc;
pub mod smt;
pub mod state;
pub mod tx;

pub use hash::H256;
