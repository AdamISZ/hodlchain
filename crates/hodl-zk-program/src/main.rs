//! SP1 toolchain smoke-test program.
//!
//! Reads a u64 `n` from the zkVM's stdin, commits `2*n` as a public
//! output. The point is not to demonstrate anything about hodlcoin —
//! it's to confirm that on *this* machine:
//!
//!   * `cargo prove build` produces a RISC-V ELF here;
//!   * `sp1-sdk` (in `hodl-zk-host`) can load that ELF, generate a
//!     proof, and verify it;
//!
//! …before we commit to the no_std refactor of hodl-core and the real
//! state-transition program.

#![no_main]
sp1_zkvm::entrypoint!(main);

pub fn main() {
    let n: u64 = sp1_zkvm::io::read();
    let doubled = n.saturating_mul(2);
    sp1_zkvm::io::commit(&doubled);
}
