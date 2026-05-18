# Light clients, ZK validity proofs, and throughput

A scoping document for *future* work, not what the POC does today. The
POC is shipping with **no ZK**; light clients verify L2 blocks
directly. This doc captures the design space we explored, the
trade-offs, and the conditions under which the calculation shifts
toward adding ZK validity proofs back in.

The reference workload throughout: **100 transactions per second
sustained**, our lofty-but-not-arbitrary throughput target. At an L2
block cadence of one per L1 block (~10 minutes), that's
**60,000 transactions per L2 block**.

## What we're actually trying to optimise

Three properties a light client needs to do its job:

1. **Believe the current state_root.** Without this, balances are
   meaningless.
2. **Verify its own balance** under that state_root.
3. **Submit transactions** and have reasonable confidence they'll be
   included.

Three trust models a light client can choose between:

| Model                                    | What the client verifies                                                                       | What it trusts                                                |
|------------------------------------------|------------------------------------------------------------------------------------------------|---------------------------------------------------------------|
| **Inclusion-proof only**                 | L1 attestation chain → state_root committed on L1; own balance via SMT inclusion proof.        | Honest-majority of full validators (`hodl-node`-class) to ensure state_root reflects valid txs. |
| **Direct block verification** (today)    | All of the above, *plus* downloads every block body and verifies every signature itself.       | Nothing about state transitions; client is its own validator.  |
| **ZK validity proof**                    | All of the above, *plus* verifies a small ZK proof per block.                                  | The trusted setup of the proof system (a one-time concern).    |

Today's `verify-balance` is the inclusion-proof-only path (kept for
debugging / dev). `light-balance` is the direct block verification
path — see the conclusion at the bottom. The ZK path is deferred.

## What 100 tx/s implies

The single most important question for any of these designs: *what
does each L2 block cost a light client?*

Per L2 block at 100 tx/s sustained:

| Work item                                | Cost per tx | × 60,000 txs    | Notes                                          |
|------------------------------------------|-------------|-----------------|-------------------------------------------------|
| BIP340 Schnorr verify (laptop)           | ~50 µs      | ~3 seconds      | secp256k1 verify in plain Rust                  |
| BIP340 Schnorr verify (phone)            | ~250 µs     | ~15 seconds     | factor of 5 slower than desktop a fair guess    |
| SMT path verify (256 levels × sha256)    | ~50 µs      | ~3 seconds      | one inclusion proof per touched account         |
| Bandwidth (signed tx body bytes)         | ~150 B      | ~9 MB           | sig + sender + receiver + amount + nonce        |

So a phone catching up *one* block at full 100 tx/s throughput costs
roughly:
- **9 MB download**.
- **15 s of signature verification CPU time** (plus another few
  seconds of SMT updates).

Over a day (144 blocks): **~1.3 GB of bandwidth, ~30 minutes of CPU
time**. That's a lot for a phone.

At more modest throughput (1 tx/s sustained, 600 tx/block), every line
in that table is 100× smaller. Tens of KB per block, sub-second CPU per
block. Trivially affordable. Days of downtime catch up in seconds.

**This is the throughput gradient.** The "no ZK" approach scales fine
through three orders of magnitude of activity, and breaks down somewhere
around 100 tx/s. ZK proofs flatten the cost: a few-KB proof per block
and a few-ms verify, regardless of how many transactions were in the
block.

## Six axes of design choice

Within "if we did add ZK", six largely-orthogonal decisions to make.

### 1. Account model: pubkey-indexed vs ID-indexed

**Pubkey-indexed** (current). The SMT key is the 32-byte
`L2Address`/x-only pubkey. Tree depth: 256.

**ID-indexed** (production zkRollups). Users get assigned a sequential
account_id (24 bits, ≈ 16M accounts max) at first appearance. The SMT
key is the ID. The pubkey is stored *inside* the leaf. Tree depth: 24.

Implication for ZK cost: the per-transfer SMT walk is *~10× cheaper*
under ID-indexing. Single biggest design lever.

UX implication: L2 addresses become small integers. Slightly less
intuitive than 32-byte hex but is what users of zkSync, Aztec etc.
already experience.

### 2. Hash function for state commitments

**sha256** (current, Bitcoin-native). ~hundreds of constraints per call
in a SNARK. Cheap on a CPU outside ZK.

**Poseidon (over BN254)**. ~250 constraints per call in BN254-KZG
SNARKs (well-supported, halo2 gadgets exist).

**Pedersen** (with custom parameters). ~70 constraints per call.
Cheapest. Less convenient to compute outside circuits.

**MiMC / Rescue**. Other SNARK-friendly hashes; similar ballpark to
Poseidon.

Implication: ZK proving cost is dominated by SMT updates. Switching
sha256 → Poseidon is ~2 orders of magnitude per call inside the
circuit. Outside circuits (in `hodl-node`, on a phone) Poseidon is
*slower* than sha256, by 10–50× depending on implementation. Trade-off
is genuine.

### 3. L2 signature scheme

**BIP340 Schnorr / secp256k1** (current). Reuses L1 keys. ~50–100k
cycles per verify in SP1 zkVM with precompiles. ~hundreds of thousands
of constraints in a SNARK over BN254 (non-native field arithmetic).
Outside ZK: fast (~50 µs per verify).

**EdDSA over Baby Jubjub** (the curve embedded in BN254's scalar
field). ~5k constraints per verify in halo2-KZG. Cheap because the
curve math happens natively in the SNARK field. **Requires a separate
L2-only key** — users would manage both their Bitcoin L1 keys and an
L2 EdDSA key.

**EdDSA over Pasta curves** (Pallas / Vesta). Analogous, paired with
Pasta-based SNARKs.

Implication: SNARK-native signatures are ~100× cheaper inside the
circuit but cost UX complexity. The mint flow naturally registers the
L2 key (locker signs their first L2 key with their L1 key when they
mint, binding the two identities).

### 4. Proof system

**zkVM (SP1, Risc0)**. Run regular Rust code; the prover proves the
RISC-V execution trace. *Probed: 575k cycles per BIP340 verify with
SP1 + secp256k1 + sha2 precompile patches.* Fast to develop, slow to
prove. ~minutes per 100-tx block on a CPU. Acceptable for POC, painful
for production.

**Halo2 (Zcash original, Pasta + IPA)**. Hand-written circuits. **No
trusted setup**. Pure crates.io install. Production-mature ecosystem
but proofs are larger and slower than KZG variants.

**Halo2 (PSE fork, BN254 + KZG)**. Hand-written circuits. **Trusted
setup required**, but reusable via Ethereum's KZG ceremony (or
Perpetual Powers of Tau). The Ethereum KZG SRS supports ~16M
constraints, comfortably enough for our needs. Tiny proofs (~few KB),
fast verification (~ms). The choice we'd pick if committing to ZK.

**Plonky3**. Newer, FRI-based, no trusted setup. Modular field
choices. Comparable proving speed to halo2-KZG, slightly less mature
ecosystem.

**StarkNet / Cairo**. Production-mature but tied to its own VM and
language. Probably not for us.

### 5. Trusted setup

**None** (halo2-Pasta, Plonky3). Slightly larger proofs, slower
proving. No ceremony concern.

**KZG with reused ceremony** (halo2-PSE). Inherit Ethereum's KZG
ceremony (~140k contributors, May 2023). Standard practice for new
KZG-based projects. The trust assumption: *some* participant of the
ceremony was honest. Widely considered acceptable.

**Project-specific ceremony**. Multi-party MPC ceremony for our own
setup parameters. Pointless overhead given reusable options exist.

### 6. Mint witness scope (within the validity proof)

**In proof, full SPV**. The validity proof itself verifies Bitcoin SPV
chain + mint UTXO Merkle proof + chain_id binding. Multi-month
project; closes all trust gaps.

**In proof as input**. Validity proof takes MintEvents as facts and
shows transfers + state transitions are valid given those. Mints
themselves verified separately (by full nodes today, by a different
proof system later). This is what we settled on for the SP1 path.

**Out of proof entirely** (today). Light clients trust full nodes for
mint validity. No ZK gadget required.

## Cost matrix

Approximate per-transfer constraint counts under different stack
choices, for the dominant work (SMT update path + signature):

| Stack                                                                  | Per transfer | 100 tx | 1000 tx |
|------------------------------------------------------------------------|--------------|--------|---------|
| Naive: 256-level sha256 SMT + BIP340 in halo2-KZG                       | ~5M          | 500M   | 5G      |
| Realistic ZK: 24-level Poseidon SMT + EdDSA-Jubjub                      | ~17k         | 1.7M   | 17M     |
| zkSync Lite reference (Pedersen + tuned EdDSA + custom circuit)        | ~3k          | 300k   | 3M      |

Approximate proving wall-clock at those scales (halo2-KZG on a single
modern CPU core; GPU divides by ~10–50×):

| Constraints | CPU wall-clock     | GPU wall-clock   |
|-------------|---------------------|-------------------|
| 300k        | ~2 s                | <1 s             |
| 1.7M        | ~10 s               | ~1 s             |
| 17M         | ~1 min              | ~5 s             |
| 100M+       | memory-bound        | minutes          |

## Three light-client architectures, compared

| Property                                        | Inclusion-proof only      | Direct block verify            | ZK validity proof              |
|-------------------------------------------------|---------------------------|---------------------------------|---------------------------------|
| **Verifies state-transition validity itself?** | No (trusts full nodes)    | Yes (verifies every sig + transition) | Yes (via the proof) |
| **Per-block CPU at 100 tx/s**                  | ~ms                       | seconds (phone) — tens of s     | ms                              |
| **Per-block bandwidth at 100 tx/s**            | tiny (one proof, one path)| ~9 MB (full block body)         | few KB (proof) + path           |
| **Catches up after long offline period**       | trivial                   | linear in blocks-missed         | linear (verify each proof) but bounded by proof size |
| **Implementation complexity**                  | done                      | done (shipped)                  | weeks to months                 |
| **Production maturity (in payment rollups)**   | not common                | not common                      | mature (zkSync, Loopring, Aztec)|

## Conclusion: defer ZK, ship direct verification

For the POC and the foreseeable next stretch:

- **ZK deferred.** We validated the toolchain (SP1 baseline
  measurement) and explored the design space (this doc). The work
  required to actually ship a halo2-KZG transfer-batch circuit is on
  the order of weeks-to-months and the payoff only matters above a
  throughput threshold we're not near.
- **Direct block verification shipped.** `hodl-wallet light-balance`
  replays every L2 block from genesis, re-verifies every signature +
  mint witness + state transition, and reads balances from the
  locally-rebuilt `LedgerState`. See `crates/hodl-wallet/src/verify.rs`.
  At low-to-moderate transaction volumes this is single-digit-millisecond
  per-block work, well within budget for any device. The trust posture
  is now identical to a full node, *without* a bitcoind dependency.
- **Current primitives unchanged** (BIP340 / sha256 / 256-level
  pubkey-indexed SMT). They're fine for direct verification; only ZK
  would need them swapped.

This is consistent with the POC scope. We're shipping a system that
demonstrates the protocol end-to-end with strong-enough trust
guarantees for evaluation, *not* one that scales to mainnet payment
volumes.

## When the ZK conversation resumes

If we ever revisit ZK proving, the conditions that would trigger it:

- **Throughput approaching 50–100 tx/s sustained.** At that point
  per-block work and per-block bandwidth start exceeding what a phone
  can realistically handle.
- **Privacy features** (shielded transfers, ring signatures for
  mints). Once we want those, we're inside a SNARK anyway and may as
  well prove transitions too.
- **A serious deployment target** that needs to be evaluable by a user
  in seconds (rather than minutes of catch-up).

The non-ZK-affecting precursor refactors that *would* help any
eventual ZK path, and that we could do incrementally:

1. **Switch to ID-indexed accounts.** ~few days of work. Cuts the
   SMT depth from 256 to ~24 for everyone, ZK or not. Slightly cheaper
   inclusion proofs at every layer.
2. **Stay on sha256 for the SMT hash** for now; Poseidon would only
   help inside a SNARK and slows down non-ZK verifiers.
3. **Keep L2 signatures BIP340 / secp256k1** while there's no ZK
   pressure. Move to EdDSA-Jubjub only if/when committing to ZK.

If/when the time comes, the production-shaped MVP stack is:

```
Proof system:  halo2 (PSE fork) over BN254 + KZG, reusing
               Ethereum's Perpetual Powers of Tau / KZG ceremony.
Hash:          Poseidon for the SMT, sha256 for L1 interop.
Signatures:    EdDSA over Baby Jubjub for L2 transfers
               (BIP340 / secp256k1 stays only for L1 mint binding).
Accounts:      24-level SMT indexed by account_id.
Scope:         Proof covers transfers + retargeting + SMT updates.
               Mints stay outside the proof (or get their own proof,
               via a Bitcoin SPV gadget, later).
```

That stack has been validated by zkSync Lite (~3000 tx batches,
~30s proving on a server). Replicating it for hodlcoin's specific
protocol is feasible but explicitly out of scope for the POC.
