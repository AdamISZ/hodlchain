# Building hodlchain from source

End-to-end build instructions for the headless daemons
(`hodl-sequencer`, `hodl-node`) and the `hodl-wallet` CLI on Linux
and macOS. The desktop GUI is covered separately in
[`run-the-gui.md`](./run-the-gui.md) — most users should download a
release binary instead of building the GUI from source.

If something here is unclear or broken, please file an issue.

---

## Linux (Ubuntu 22.04+, Debian 12+, or equivalent)

### 1. Install prerequisites

```bash
sudo apt update
sudo apt install -y build-essential pkg-config libssl-dev curl git
```

Install a recent Rust toolchain (1.75+ is fine; we test on stable):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

Install Bitcoin Core (any v22+). On Ubuntu the easiest path is the
[official binary release](https://bitcoincore.org/en/download/):

```bash
# Adjust the version + arch for your machine.
cd /tmp
wget https://bitcoincore.org/bin/bitcoin-core-27.0/bitcoin-27.0-x86_64-linux-gnu.tar.gz
tar xzf bitcoin-27.0-x86_64-linux-gnu.tar.gz
sudo install -m 755 bitcoin-27.0/bin/bitcoind /usr/local/bin/
sudo install -m 755 bitcoin-27.0/bin/bitcoin-cli /usr/local/bin/
```

Verify:

```bash
bitcoind --version
cargo --version
```

### 2. Clone and build

```bash
git clone https://github.com/AdamISZ/hodlchain
cd hodlchain
cargo build --release
```

The first build pulls a sizeable dep tree (axum, tokio, bitcoin,
secp256k1) — expect 3–5 minutes on a typical laptop. Subsequent
builds are seconds.

Run the unit tests to sanity-check your toolchain:

```bash
cargo test --workspace
```

### 3. Run the regtest demo

The `scripts/regtest-demo.sh` script is the canonical end-to-end run.
It spins up a fresh `bitcoind` in a temp directory, starts the
sequencer and node, and drives two wallets (Alice and Bob) through a
full deposit → mint → transfer → light-verify cycle:

```bash
./scripts/regtest-demo.sh
```

The full cycle takes ~15 seconds. To leave the daemons running so you
can poke at the HTTP APIs, browse the Swagger UI, or connect the
desktop wallet to them:

```bash
./scripts/regtest-demo.sh --keep-running
```

While it's paused at "press enter to tear down":

- Sequencer Swagger UI: <http://127.0.0.1:28080/docs/>
- Node Swagger UI: <http://127.0.0.1:28081/docs/>

Press enter to clean up; the script tears `bitcoind` and the daemons
down and removes the temp datadir.

If your `bitcoind`/`bitcoin-cli` aren't on `$PATH`, point the script
at them explicitly:

```bash
BITCOIND_PREFIX=/opt/bitcoin/bin ./scripts/regtest-demo.sh
```

---

## macOS (Intel or Apple Silicon)

### 1. Install prerequisites

Install [Homebrew](https://brew.sh) if you don't have it, then:

```bash
brew install rustup-init bitcoin git
rustup-init -y
source "$HOME/.cargo/env"
```

You also need the Apple developer toolchain (for the `secp256k1` C
build):

```bash
xcode-select --install   # no-op if already installed
```

### 2. Clone and build

Same as Linux:

```bash
git clone https://github.com/AdamISZ/hodlchain
cd hodlchain
cargo build --release
cargo test --workspace
```

### 3. Run the regtest demo

Homebrew installs `bitcoind` and `bitcoin-cli` to `/opt/homebrew/bin`
(Apple Silicon) or `/usr/local/bin` (Intel); either is on `$PATH` by
default. So:

```bash
./scripts/regtest-demo.sh
```

…works out of the box. Same `--keep-running` flag and same Swagger
URLs as on Linux.

---

## What you get

After `cargo build --release` the binaries are at:

```
target/release/hodl-regtest      # local regtest orchestrator (start/stop/mine/...)
target/release/hodl-sequencer
target/release/hodl-node
target/release/hodl-wallet       # CLI wallet
```

### Two ways to run a local regtest backend

- **`./scripts/regtest-demo.sh`** — scripted end-to-end smoke test.
  Spins up a fresh chain, drives two wallets through deposit / mint
  / transfer / light-verify in ~15 seconds, then either tears down
  or stays paused (`--keep-running`). Best for verifying the build
  works after a code change.
- **`./target/release/hodl-regtest start`** — persistent backend
  with subcommands (`mine`, `fund`, `stop`, `status`, `reset`,
  `logs`). Best for iterative testing (e.g. driving the desktop
  wallet against it). The full subcommand reference lives in
  [`run-the-gui.md`](./run-the-gui.md); the binary is identical to
  the one shipped in release artifacts.

The demo script uses debug binaries from `target/debug/`, so
`cargo build` (without `--release`) is enough for it. `hodl-regtest`
locates its sibling binaries either in the same directory as itself
or on `$PATH`, so running it straight out of `target/release/` works
out of the box.

For the desktop GUI, either grab a [release
binary](https://github.com/AdamISZ/hodlchain/releases) (recommended,
see [`run-the-gui.md`](./run-the-gui.md)) or build it from source
following the instructions in
[`crates/hodl-desktop/README.md`](../crates/hodl-desktop/README.md).
