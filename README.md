# hodlchain

A proof-of-concept Layer 2 anchored to Bitcoin, exploring a fair-launch
issuance primitive: users provably commit Bitcoin via a relative
timelock (`OP_CHECKSEQUENCEVERIFY` and/or `OP_CHECKLOCKTIMEVERIFY`) on a taproot output for a chosen
duration, and the L2 mints new tokens in return — bounded by the value
locked and superlinear in lock duration, saturating at the locked value
as the duration approaches infinity. The BTC is recoverable; nothing
is destroyed.

> **Status**: research POC. Single-sequencer L2 on Bitcoin regtest /
> signet. Not for mainnet, not audited, not stable. The protocol design
> is the deliverable; everything in this repo runs but is shaped to
> teach, not to ship.

![hodlchain desktop wallet — blockchain overview tab showing chain head, total minted supply, current r, and retarget-window progress](docs/overviewtab.png)

## Quick start

- **Run the headless daemons + CLI wallet from source** →
  [`docs/build-from-source.md`](docs/build-from-source.md)
- **Use the desktop wallet (download release artifact)** →
  [`docs/run-the-gui.md`](docs/run-the-gui.md)

Both Linux and macOS are covered in each doc.

## What's in here

| Crate                  | What it is                                                       |
|------------------------|------------------------------------------------------------------|
| `crates/hodl-core`     | Library: consensus types, mint function, retargeting, SMT,       |
|                        | block + tx + proof types, OP_RETURN attestation codec,           |
|                        | Esplora wire types, shared RPC DTOs.                             |
| `crates/hodl-sequencer`| Single-sequencer L2 producer. Builds one L2 block per L1 block,  |
|                        | posts a chained OP_RETURN attestation per block, serves an HTTP  |
|                        | API for tx submission and queries.                               |
| `crates/hodl-node`     | Passive L2 validator. Follows the L1 attestation chain via       |
|                        | bitcoind, replays L2 blocks, re-verifies every mint witness      |
|                        | against L1. Exposes a slim Esplora-compatible HTTP subset so     |
|                        | light wallets can walk the chain without their own bitcoind.    |
| `crates/hodl-wallet`   | CLI wallet + reusable library. `ops::*` is the typed surface     |
|                        | every UI (CLI + desktop) calls into; `main.rs` is a thin clap    |
|                        | shim. Handles BIP39 mnemonic, BIP32-derived per-mint L1 keys,    |
|                        | sparse stateless light-balance verification, and L1 reclaim.     |
| `crates/hodl-desktop`  | Tauri v2 + Svelte 5 + TypeScript desktop wallet. Thin            |
|                        | `#[tauri::command]` wrappers around `hodl_wallet::ops::*`.       |
|                        | Excluded from `default-members` because it needs                 |
|                        | libwebkit2gtk-4.1; see `crates/hodl-desktop/README.md`.          |
| `crates/hodl-regtest`  | Cross-platform CLI that orchestrates a local regtest backend     |
|                        | (bitcoind + sequencer + node) with start/stop/mine/fund/reset    |
|                        | subcommands. Persistent datadir. Ships in releases alongside     |
|                        | the GUI.                                                         |
| `docs/`                | `design.md`, build instructions, GUI instructions, ZKP           |
|                        | discussion, paper PDFs.                                          |
| `scripts/regtest-demo.sh` | End-to-end demo against a temp bitcoind on regtest.           |

## Build and run

For full setup instructions on Linux and macOS, see
[`docs/build-from-source.md`](docs/build-from-source.md) (headless
daemons + CLI) and [`docs/run-the-gui.md`](docs/run-the-gui.md)
(desktop wallet against a local regtest backend).

The 30-second version, if you already have Rust and `bitcoind` v22+
installed:

```bash
git clone https://github.com/AdamISZ/hodlchain
cd hodlchain
cargo build --release

# scripted end-to-end smoke test (alice + bob, 15 seconds):
./scripts/regtest-demo.sh

# OR persistent local backend you can drive with the desktop wallet:
./target/release/hodl-regtest start
```

`hodl-regtest start` brings up `bitcoind` (regtest) plus
`hodl-sequencer` (port 28080) and `hodl-node` (port 28081), mines 102
blocks so the local user wallet has spendable funds, and persists
state across restarts. See `hodl-regtest --help` for the full
subcommand list (`mine`, `fund`, `stop`, `status`, `reset`, `logs`).

## Reading the code

If you want to follow the protocol from the bottom up:

1. **`crates/hodl-core/src/consensus.rs`** — `mint_fn`, retargeting
   constants, BIP341 NUMS H, chain_id tag.
2. **`crates/hodl-core/src/l1.rs`** — the canonical mint-UTXO Taproot
   construction (NUMS internal key + 2-leaf tap tree: CSV spend leaf
   and `OP_RETURN <D>` namespace-binding data leaf).
3. **`crates/hodl-core/src/proof.rs`** — `MintProof` trait, `MintProofEnvelope` enum (v0 = transparent outpoint proof; future
   variants slot in here), `verify_mint_entry` glue.
4. **`crates/hodl-core/src/state.rs`** — `LedgerState`, retargeting,
   `state_root` computation via `StateComponents`.
5. **`crates/hodl-core/src/smt.rs`** — 256-level sparse Merkle tree
   over accounts, inclusion/non-inclusion proofs.
6. **`crates/hodl-core/src/op_return.rs`** — 73-byte attestation codec.
7. **`crates/hodl-sequencer/src/{producer,bitcoind,api}.rs`** — block
   production, chained attestation tx construction, HTTP intake.
8. **`crates/hodl-node/src/{follower,bitcoind,api}.rs`** — L1 chain
   walk, block replay, Esplora endpoints.
9. **`crates/hodl-wallet/src/{ops,verify,reclaim,wallet,esplora}.rs`** —
   the wallet library. `ops` is the UI-agnostic typed surface;
   `main.rs` is a thin CLI shim over it. The Tauri desktop app in
   `crates/hodl-desktop` is a parallel consumer.

Or read `docs/design.md`.

## Trust posture (today)

What `light-balance` verifies cryptographically (the direct-verify
path):

- The L1 chain of OP_RETURN attestations from a known `anchor_0`
  outpoint (via any Esplora — mempool.space, electrs, or our own
  hodl-node serving the same wire shape).
- Every L2 block body referenced by an attestation: header agreement
  with the L1 attestation, txs_root, chain continuity, every transfer
  signature, every mint witness against L1, and a recomputed
  `state_root` matching the header at every height. The balance is
  read out of the locally-rebuilt `LedgerState`.

What's still trusted:

- Block-body availability — currently served by the sequencer (or a
  follower node) over HTTP. Without a body we cannot replay.
- The chosen Esplora endpoint for L1 honesty. The wallet does not do
  Bitcoin SPV / merkle-path verification of the attestation tx; the
  endpoint is implicitly trusted not to lie about which txs exist or
  what they contain. A locally-run electrs eliminates this.
- Sequencer liveness — single sequencer; no rotation, no fallback.
- For *mint anonymity*, full nodes also see which L1 UTXO funded a
  given mint. Future work: anonymous mint via aut-ct ring proofs.


## License

Not yet specified. To be determined before any wider release.
