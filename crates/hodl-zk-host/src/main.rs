//! SP1 toolchain smoke test.
//!
//! Loads the `hodl-zk-program` RISC-V ELF, generates a proof that
//! "given private input n, the public output is 2n", verifies it,
//! prints the result. Standalone — talks to nothing else in the
//! workspace.
//!
//! Prerequisites:
//!
//!   1. `curl -L https://sp1up.succinct.xyz | bash` and then `sp1up`
//!      to install the SP1 toolchain (cargo-prove, prover assets).
//!   2. `cd crates/hodl-zk-program && cargo prove build`
//!      — produces `elf/riscv32im-succinct-zkvm-elf`.
//!   3. `cargo run -p hodl-zk-host` from the workspace root.

use anyhow::{bail, Context, Result};
use sp1_sdk::{Prover, ProverClient, ProvingKey, SP1Stdin};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> Result<()> {
    let input: u64 = 21;
    println!("input n  = {input}");
    println!("expected = {} (= 2n)", input * 2);
    println!();

    // Load the zkVM ELF at runtime so `cargo check --workspace`
    // doesn't depend on the program having been built first.
    // SP1 v5 places the build output under target/elf-compilation/.
    let elf_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../hodl-zk-program/target/elf-compilation/riscv64im-succinct-zkvm-elf/release/hodl-zk-program");
    if !elf_path.exists() {
        bail!(
            "program ELF not found at {}\n\
             \n\
             Build it with:\n\
             \tcd crates/hodl-zk-program && cargo prove build\n\
             \n\
             If you don't have the SP1 toolchain yet:\n\
             \tcurl -L https://sp1up.succinct.xyz | bash\n\
             \texec $SHELL\n\
             \tsp1up",
            elf_path.display()
        );
    }
    let program_elf =
        std::fs::read(&elf_path).with_context(|| format!("read {}", elf_path.display()))?;
    println!("program elf: {} bytes ({})", program_elf.len(), elf_path.display());

    // `from_env` picks a prover backend from the SP1_PROVER env var,
    // defaulting to a local CPU prover. (It's async in sp1-sdk v6
    // because it can negotiate with a remote backend.)
    let client = ProverClient::from_env().await;

    // v6: setup takes an Elf enum (Vec<u8>: Into<Elf>) and returns a
    // single ProvingKey value; vk is borrowed off it.
    let pk = client
        .setup(program_elf.into())
        .await
        .context("setup failed")?;
    let vk = pk.verifying_key();
    println!("proving key + verifying key derived");

    let mut stdin = SP1Stdin::new();
    stdin.write(&input);

    println!("generating proof (this may take a while)...");
    // v6: the ProveRequest implements IntoFuture; awaiting it is what
    // runs the proof.
    let mut proof = client
        .prove(&pk, stdin)
        .await
        .context("prove failed")?;
    println!("proof generated");

    let output: u64 = proof.public_values.read();
    println!("public output read from proof = {output}");
    assert_eq!(output, input * 2, "public output mismatch");

    println!("verifying proof...");
    // v6: verify is synchronous and takes an optional StatusCode arg
    // (used by remote provers for finalisation; None is the default).
    client
        .verify(&proof, vk, None)
        .context("verify failed")?;
    println!();
    println!("✓ SP1 toolchain working end-to-end on this machine");

    Ok(())
}
