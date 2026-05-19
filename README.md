# hodlchain

A proof-of-concept Layer 2 anchored to Bitcoin, exploring a fair-launch
issuance primitive: users provably commit Bitcoin via a relative
timelock (`OP_CHECKSEQUENCEVERIFY`) on a taproot output for a chosen
duration, and the L2 mints new tokens in return — bounded by the value
locked and superlinear in lock duration, saturating at the locked value
as the duration approaches infinity. The BTC is recoverable; nothing
is destroyed.

> **Status**: research POC. Single-sequencer L2 on Bitcoin regtest /
> signet. Not for mainnet, not audited, not stable. The protocol design
> is the deliverable; everything in this repo runs but is shaped to
> teach, not to ship.

The conceptual spec lives in `docs/issuancev3.tex` (the working paper);
the implementation notes are in `docs/design.md`.

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
| `docs/`                | `design.md` is the implementation companion to the paper.        |
|                        | `issuancev*.tex` are the working paper drafts (untracked).       |
| `scripts/regtest-demo.sh` | End-to-end demo against a temp bitcoind on regtest.           |

## Build

```bash
cargo build       # headless crates (hodl-core/wallet/sequencer/node)
cargo test
```

You need a recent Rust (edition 2021+). The headless daemons use
tokio + axum; no proof-system dependencies (see
`docs/zk-design-discussion.md` for the rationale).

For the Tauri desktop app, install the extra system / JS toolchain
prerequisites (libwebkit2gtk-4.1-dev, libsoup-3.0-dev, Node 20+,
pnpm, `cargo install tauri-cli --version "^2"`) and then build via
`cd crates/hodl-desktop && cargo tauri dev`.

## See it run

The full end-to-end flow against a fresh regtest bitcoind:

```bash
./scripts/regtest-demo.sh
```

What it does (~15 seconds):

1. Spins up `bitcoind` in `/tmp/hodl-regtest`, creates a sequencer-funding
   wallet and a user wallet, mines 102 blocks.
2. Starts `hodl-sequencer` (port 28080) and `hodl-node` (port 28081).
3. Runs two `hodl-wallet`s (Alice + Bob) through:
   - Alice creates a CSV-locked taproot mint UTXO (P2TR, NUMS internal
     key, two-leaf tap tree per the paper, 0.1 BTC, T = 10000 blocks).
   - Submits a mint message; sequencer verifies the witness against L1,
     credits Alice ≈ 564,057 L2 atoms.
   - Alice transfers 141,014 atoms to Bob; both balances settle.
4. Verifies Alice's balance three ways:
   - **`balance`** — trusts the response.
   - **`verify-balance`** — re-derives `state_root` from the response's
     state-components, verifies the SMT inclusion proof, checks the
     leaf matches the reported balance/nonce.
   - **`light-balance`** — walks the L1 attestation chain via the
     node's Esplora-compatible endpoints, then *replays every L2
     block from genesis*: re-verifies every transfer signature, every
     mint witness (against L1 via Esplora), and recomputes the
     state_root at every height. The reported balance is read out of
     the locally-rebuilt `LedgerState`. End-to-end light-client
     verification with no bitcoind RPC from the wallet.

To leave the daemons running so you can browse the API docs:

```bash
./scripts/regtest-demo.sh --keep-running
```

Then while it's paused at "press enter to tear down":

- Sequencer Swagger UI: <http://127.0.0.1:28080/docs/>
- Node Swagger UI: <http://127.0.0.1:28081/docs/>
- Raw OpenAPI: `/openapi.json` on either daemon.

Press enter when done; the script tears bitcoind and the daemons down.

### Bitcoin Core path

The demo defaults to `~/code/bitcoin-28.0/bin/` for the `bitcoind` and
`bitcoin-cli` binaries. If yours is elsewhere, set:

```bash
BITCOIND_PREFIX=/path/to/dir ./scripts/regtest-demo.sh
```

or supply both binaries explicitly via `BITCOIND_BIN` and
`BITCOIN_CLI_BIN`. Any Bitcoin Core >= v22 should work.

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

Or read `docs/design.md` front-to-back for the design rationale.

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

## Roadmap (rough)

Working through:

1. **README + housekeeping** ✓.
2. **Direct block verification in the light client** ✓. The
   `light-balance` command replays every L2 block from genesis,
   verifying each signature + mint witness + state-root continuity
   itself. See `docs/zk-design-discussion.md` for the design-space
   survey that led to picking this over ZK validity proofs.
3. **End-to-end desktop client** (next). A polished `egui` app
   bundling the L1 mint flow and the L2 state-light-validating
   wallet — the demo target for non-developer users.

Beyond that: anonymity v1 via aut-ct ring proofs for mints,
decentralised DA for block bodies, multi-sequencer.

## License

Not yet specified. To be determined before any wider release.
