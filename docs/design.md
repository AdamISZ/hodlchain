# hodlchain POC — design notes

Implementation details, starting from [the paper](https://github.com/AdamISZ/hodlchain-paper). Captures the v0 implementation decisions; the paper specifies the issuance primitive itself.

## Scope

A proof-of-concept of the issuance mechanism described in the paper, on Bitcoin **regtest** and **signet**, (though mainnet will not require any changes) comprising:

- A minimal Bitcoin CLI wallet that can build CSV-locked mint UTXOs and produce mint messages.
- A primitive **single-sequencer** L2 that orders mint outcomes and transfers into blocks.
- L2 nodes that follow the sequencer by reading L1 OP_RETURN attestations and fetching block bodies from the sequencer.

Out of scope for v0:

- Multi-sequencer consensus, fault proofs, decentralised sequencing.
- Fee market or sender-specified fees on L2. A flat percentage
  protocol fee paid to the sequencer's L2 address is implemented;
  see "Fees" below.
- Anonymity (deferred; the mint code is structured so a future ring-proof or ZK-proof variant of `MintProofEnvelope` can plug in without changing block format — see "Mint witness pluggability" below).

In scope (now implemented):

- Mint-based retargeting of `r` (see "Retargeting" below). `r` is consensus state.
- **Sub-L1 L2 block cadence** (30s by default), decoupled from L1.
  Each L1 attestation now commits to a *batch* of L2 blocks. See
  "L2 block structure" + "L1 attestation chain".
- **Per-transfer protocol fee** (1 basis point by default,
  100-atom floor). Credited to a sequencer L2 address committed
  in genesis. See "Fees".
- **Sequencer L2 identity key** + **signed soft-confirmation
  receipts**. The sequencer publishes a Schnorr pubkey in the L2
  genesis header (as both `producer` and `sequencer_fee_address`)
  and signs an inclusion receipt for every accepted submission.
  See "Sequencer identity and soft confirmations".
- **L1 reorg recovery for the attestation chain.** The sequencer
  tracks every posted attestation until 2 L1 confirmations and
  reverts the chain anchor if the tx is evicted. L2 state never
  reorgs. See "L1 reorg recovery".

## Stack

- **Language**: Rust (edition 2021), single Cargo workspace.
- **L1 access**: The sequencer and node use `bitcoincore-rpc`
    against a local `bitcoind` (regtest / signet / mainnet — the
    sequencer to post attestation OP_RETURNs and own the anchor
    UTXO; the node to walk the L1 attestation chain and serve a
    bitcoind-backed Esplora subset to light clients). The wallet
    (CLI + GUI) has **no bitcoind dependency** — it walks the
    attestation chain, polls deposit funding, and broadcasts
    reclaim transactions via an Esplora HTTP endpoint (mempool.space,
    an electrs, or the hodl-node's own Esplora-compatible subset).
- **Bitcoin primitives**: `bitcoin` 0.32 (Taproot, Schnorr, scripts), `secp256k1` 0.29.
- **L2 daemons**: tokio + axum for HTTP, reqwest for client calls between wallet/node and sequencer.
- **Storage**: SQLite (`rusqlite`, bundled) — the sequencer's tx-pool and consumed-nullifier set, the node's view of L2 state.
- **L2 sig scheme**: BIP340 Schnorr / secp256k1, same as Bitcoin. This is for simplicity, not because we're restricted to using the same scheme.

## Crate layout

```
crates/
├── hodl-core       # lib: consensus, types, codecs, MintProof trait
├── hodl-wallet     # bin: end-user CLI (keygen, mint UTXO, mint message, transfer, balance)
├── hodl-sequencer  # bin: HTTP intake, block production, OP_RETURN poster
└── hodl-node       # bin: L1 scanner, sequencer follower, balance query
```

`hodl-core` is the only library; the three binaries each depend on it. Once `hodl-rpc` DTOs grow beyond ~5 types we'll split them into their own crate; for now they live next to whichever daemon owns them.

## L1 mint UTXO format (P2TR, NUMS internal key, two tapleaves)

Matches the paper §3 ("The Mechanism").

A hodlchain mint UTXO is a Taproot output whose internal key is BIP341 NUMS H, with a tap tree containing **exactly two leaves**:

```text
L_spend = <T> OP_CHECKSEQUENCEVERIFY OP_DROP <user_xonly_pubkey> OP_CHECKSIG
L_data  = OP_RETURN <D>          where  D = TaggedHash("L2/hodlchain/v1", user_xonly_pubkey)
```

- `L_spend` uses **CSV (BIP112)**, not CLTV. `T` is the **relative** locktime in blocks — the committed duration. The leaf is spendable `T` blocks after the funding UTXO confirms; the spend tx must set its input `nSequence` accordingly. Per the paper, CLTV is a valid alternative and will be required for very long locks, so this is for now a TODO. Locks of up to a year and slightly more are supported with CSV, which should be fine for now.
- `L_data` is permanently unspendable (tapscript inherits Bitcoin's rule that `OP_RETURN` aborts script execution). It exists only as a 32-byte committed payload binding the UTXO to hodlchain's namespace.
- `D` being keyed by `user_xonly_pubkey` (not a protocol constant) makes each locker's `L_data` leaf hash a unique-looking random 32-byte value; an outsider with one locker's leaf hash cannot identify others'.

### Lock-duration ceiling (v0 limitation)

As mentioned, for now, we are CSV/BIP112 only. BIP112's block-based relative locktime uses the lower 16 bits of `nSequence`, so `T ∈ [1, 65535]` (≈ 1 to 454 days at 10 minute blocks). This is a hard CSV-blocks-mode limit, not a hodlchain design choice. At the cap with `r = 1/26280` the locker still receives `f_mint(V, 65535, 1/26280) ≈ 0.71 V`, so the cap is well above the useful regime.

The output scriptPubKey is `OP_1 <Q>` where `Q` is the BIP341 taproot output key:

```text
Q       = P + t·G,    t = H_TapTweak(P ‖ R)
R       = TapBranch(sort(h(L_spend), h(L_data)))
P       = BIP341 NUMS H
h(L)    = TapLeafHash(L)    // tagged hash with tag "TapLeafHash"
TapBranch(a, b) = TaggedHash("TapBranch", a‖b)   if a ≤ b else (b‖a)
```

### chain_id

Single string `"L2/hodlchain/v1"` across all networks, with the version identifier having the obvious use in the future. This is specified in `DATA_LEAF_TAG` in consensus.rs. TODO: chain_id in consensus is not used; it should be removed, or it should be pointed at the same string.


### Mint proof wire format

Minimal: the prover sends only what the verifier cannot recompute deterministically.

```rust
struct OutpointProof {
    outpoint:             OutPoint,
    user_xonly_pubkey:    XOnlyPublicKey,    // pk
    lock_blocks:          u32,               // T, the relative locktime baked into L_spend
    claimed_block_height: u32,               // h — L1 height the locker claims to be at
    signature:            schnorr::Signature,// BIP340 over the mint sighash
}
```

The verifier reconstructs `L_spend`, `L_data`, both leaf hashes, the Merkle root, and the tweaked output key from `(pk, lock_blocks)` and the hardcoded chain_id. If our tree shape ever changes (decoy leaves, alternate data formats), this struct will gain explicit script + path fields per the paper §3; for v0 the minimal form suffices.

The witness is paired with a `MintEvent` (the declared outcome) into
a `MintEntry { event, witness }`. Whereas `OutpointProof` is the
proof, `MintEvent` is the credit the chain records once the proof
verifies:

```rust
struct MintEvent {
    nullifier_hex:    String,       // dedup id; v0 = serialised outpoint
    amount:           Amount,       // = mint_fn(V, T, r) at the producing block's r
    l2_destination:   L2Address,    // x-only pubkey that gets credited
    l1_create_height: u32,          // T_create: L1 height where the funding UTXO confirmed
    lock_blocks:      u32,          // T (matches L_spend's CSV)
    l1_value_sat:     u64,          // V
}
```

Producers and followers re-derive `event` from the witness + current
state (so the on-chain `amount` reflects the `r` at the producing
block, not at the user's submit time). A mismatch between
witness-derived event and the event the producer wrote into the
block fails validation.

### Verification by the L2

1. Look up `outpoint` on L1 → `(value_sat, scriptPubKey, confirmed_height, confirmations)`.
2. Check `lock_blocks ∈ [1, 65535]` (BIP112 block-mode range).
3. **Active lock period** (paper §3, `m = (outpoint, h, L2-destination)`):
   - `claimed_block_height >= confirmed_height` (= T_create)
   - `claimed_block_height < confirmed_height + lock_blocks` (lock not expired)
   - `claimed_block_height <= L1_tip` (no future-dated mints)
4. Reconstruct `L_spend`, `L_data` from `(pk, lock_blocks)` and chain_id.
5. Build the 2-leaf tap tree with NUMS-H as internal key, derive `expected_spk = OP_1 <Q_x>`.
6. Assert `expected_spk == scriptPubKey`. (This is the single check that simultaneously verifies: SPK matches, internal key is NUMS, both leaves are present, both are well-formed, `pk` is the one bound, `T` is the one bound, and chain_id matches hodlchain.)
7. Verify the Schnorr signature over `sha256("hodl-mint-v1" || outpoint.txid || vout_le || claimed_block_height_be || l2_destination)` under `pk`.
8. Require `confirmations >= MINT_CONFIRMATIONS` (= 1 in v0).
9. Compute `amount = mint_fn(value_sat, lock_blocks, r)`. No `T_create` arithmetic needed — `T = lock_blocks` is what the script committed to.
10. Check `proof.nullifier() (= serialised outpoint)` is not in the consumed set.

## Mint function

```text
f_mint(V, T) = V * (1 - (1 + rT) * e^{-rT})
```

- `V` in satoshis; `f_mint` in L2 atoms; `ATOMS_PER_SAT = 1` for v0 so the bound `f_mint <= V` is trivially preserved.
- Initial `r = 1 / 26_280` per L1 block → inflection at T ≈ 6 months of blocks.
- `r` is **consensus state**, not a config knob: it lives in `LedgerState::current_r`, is committed to by `state_root`, and shifts at retarget windows (see "Retargeting").

## Retargeting

**Mint-paced**, per paper §7. The retarget window is measured in
*cumulative L2 atoms minted*, not in L1 blocks. During quiet periods
(no mints) the loop does not advance — `r` is preserved across
quiescence, eliminating the pathology where a block-paced design
would ratchet `r` upward through any empty window.

Consensus constants (`hodl-core::consensus`):

```text
                              demo (current)   planned mainnet
TARGET_ATOMS_PER_BLOCK     =   1_000_000     |   50_000_000        // M*: atoms / L1-block
RETARGET_MINT_WINDOW_ATOMS = 100_000_000     | 216_000_000_000     // M_w: window-close threshold
RETARGET_MAX_FACTOR        = 2.0             | 2.0                 // C: clamp ±factor (paper §7)
INITIAL_R                  = 1 / 1_000       | 1 / 26_280          // initial inflection
```

The shipping constants are demo-tuned so a regtest session can
actually cross retarget boundaries inside an interactive demo. Each
`pub const` in `consensus.rs` carries a `// Demo / regtest value`
comment plus a `// Planned mainnet value` note. The two columns
above match those code comments; both columns have the same
`M_w / M*` ratio (≈ 100 and ≈ 4320 respectively).

At the **mainnet** rate, the window completes in `M_w / M*` = 4320
L1 blocks ≈ 1 month at 10 min/block — long enough that
locks-in-flight have time to respond to `r` changes before the next
retarget (paper §7's "windows of months rather than weeks"). At the
**demo** rate the window completes in ~100 L1 blocks so the retarget
loop is reachable in a few minutes of regtest mining.

State (`LedgerState`):

```text
current_r:                        f64           // active r; consensus state
current_window_atoms:             u64           // atoms minted in the open window so far
current_window_start_l1_height:   Option<u32>   // L1 height the window opened at;
                                                // None during quiet periods
```

Algorithm (`LedgerState::end_of_block(l2_height, l1_height)`, called after every block):

```text
if l2_height == 0 || current_window_atoms == 0 { return; }  // genesis / quiet

// First-mint-of-window bookkeeping. Set lazily so the field is None during quiescence.
if current_window_start_l1_height.is_none() {
    current_window_start_l1_height = Some(l1_height)
}

// Retarget condition.
if current_window_atoms < M_w { return; }

let delta_actual = l1_height - current_window_start_l1_height.unwrap()
if delta_actual == 0 { return; }   // threshold crossed in same block window opened in;
                                   // defer until next block when delta ≥ 1

let m_obs = current_window_atoms / delta_actual           // atoms per L1 block this window
let ratio = clamp(M* / m_obs, 1/C, C)
current_r *= ratio                                        // observed > target ⇒ ratio < 1 ⇒ r shrinks
current_window_atoms = 0
current_window_start_l1_height = None
```

Direction: `f_mint` is monotone increasing in `r` (derivative is `V · r · T² · e^{-rT} > 0`), so multiplying `r` by `ratio < 1` slows future issuance. Same sign convention as the paper's `r_new = r_old · (M*/M_obs)`.

### Producer / node lock-step

The producer snapshots `state.current_r` at block start, re-runs every `MintEntry`'s witness with that `r`, and stamps the freshly-derived credit's `event` into the block. The node, replaying with the same starting state, uses the same `r` value when calling `verify_mint_entry`, so the witness-derived credit on the node side matches what the producer put in the block. `end_of_block` then runs on both sides, shifting `r` for the next block in lock-step.

A user who submitted a mint at a different `r` (e.g., a window straddled their submission and the producer's inclusion) gets the new `r`'s amount when they actually mint — submit-time `mint_amount` in `/mint` responses is best-effort.

## L2 block structure

L2 blocks are produced on the sequencer's own timer (default 30s,
configurable per chain). L1 attestation runs on its own cadence —
one attestation per L1 block, each committing to the *latest* L2
head. So many L2 blocks share the same `l1_height` while Bitcoin
is between blocks, and each L1 attestation effectively batches a
range of L2 blocks. This decouples L2 throughput / latency from
L1, while keeping L1 as the trust root.

```text
L2BlockHeader {
    height:                 u32,
    prev_hash:              H256,
    l1_block_hash:          H256,
    l1_height:              u32,                  // L1 tip observed at production time
    txs_root:               H256,
    state_root:             H256,
    timestamp:              u64,
    anchor_outpoint:        Option<OutPoint>,     // Some only in the genesis header
    producer:               Option<L2Address>,    // sequencer L2 identity; None pre-Phase-3
    sequencer_fee_address:  Option<L2Address>,    // Some only in the genesis header
}
L2Block { header, txs: Vec<L2Tx> }
```

The block hash is `sha256("hodl-block-v3" || canonical(header))`,
where the canonical encoding is a deterministic field-by-field byte
layout (all multi-byte values big-endian):

```text
"hodl-block-v3"
  || height(4)            || prev_hash(32)        || l1_block_hash(32)
  || l1_height(4)         || txs_root(32)         || state_root(32)
  || timestamp(8)         || has_anchor(1)
  || if has_anchor: anchor_outpoint.txid(32) || anchor_outpoint.vout(4)
  || has_producer(1)
  || if has_producer: producer.serialize(32)
  || has_fee_address(1)
  || if has_fee_address: sequencer_fee_address.serialize(32)
```

`producer` is set on every non-genesis block to the sequencer's L2
identity pubkey; `sequencer_fee_address` is set only at genesis
(chain-wide, immutable). Both fields are committed to the block hash,
so a future multi-sequencer / threshold-signing design that names a
different responsible party per block doesn't need a hard fork.

`L2Tx` is either:

- `Mint(MintEntry { event, witness })` — both the declared outcome (amount, nullifier, destination, lock parameters, L1 value) AND the proof. Nodes re-run `verify_mint_entry(entry, &secp, &l1, r)` for every mint in every block; a mismatch between what the witness authorises and what the event declares fails block validation. Block validity is therefore independent of trusting the sequencer.
- `Transfer(SignedTransfer)` — `(from, to, amount, nonce, schnorr_sig)`. Sighash:
  ```text
  sha256("hodl-transfer-v2" || from(32) || to(32) || amount_be(8) || nonce_be(8))
  ```
  Strict byte layout (no JSON), no_std-compatible so the state-transition
  zk-program can reconstruct the same hash. The v2 tag distinguishes this
  encoding from the v0 JSON-based hash that earlier code used.

Each tx hashes into `txs_root` (the header field) via `L2Tx::hash`:

```text
hash(L2Tx) = sha256(
    "hodl-tx-v2" || kind(1) || encode_body
)
  kind = 0x01 (Mint)     || nullifier_len_be(4) || nullifier_bytes
                         || amount_be(8) || l2_destination(32)
                         || l1_create_height_be(4) || lock_blocks_be(4)
                         || l1_value_sat_be(8)
                         || witness_len_be(4) || witness_canonical_bytes

  kind = 0x02 (Transfer) || transfer_sighash(32) || sig(64)

txs_root = sha256("hodl-txs-v2" || hash(tx_0) || hash(tx_1) || ...)
```

`witness_canonical_bytes` is a per-`MintProofEnvelope`-variant byte
layout — v0 (`OutpointProof`) encodes `(outpoint, pk, lock_blocks,
claimed_block_height, sig)`. A future variant (ring proof, ZK proof)
gets its own variant tag here without disturbing the outer hash
shape.

Alongside each block the sequencer publishes a **block witness**, a
sparse pre-state snapshot the light client uses to verify the L2
state transition without holding the full state:

```rust
struct BlockWitness {
    height:              u32,
    prior_accounts_root: H256,                // == previous block's accounts_root
    pre_proofs:          Vec<InclusionProof>, // one per address touched by the block
}
```

`pre_proofs` contains an SMT inclusion-or-non-inclusion proof at the
*prior* accounts root for every address the block's txs touch
(senders, recipients, mint destinations, and the sequencer fee
address if any transfer is present). Light clients walk these
proofs, replay the block's txs against the resulting sparse
LedgerState, run `apply_updates` to derive the new accounts_root,
recompute `state_root`, and check it equals the block header's
`state_root`. See `crates/hodl-core/src/witness.rs`.

State is a pair of maps: `accounts: BTreeMap<L2Address, Account>`
and `consumed_nullifiers: BTreeSet<String>`, plus the retarget
scalars `current_r`, `current_window_atoms`,
`current_window_start_l1_height` (the last being `Option<u32>` —
`None` during quiet periods), the chain-wide
`sequencer_fee_address: Option<L2Address>` (`None` means fees are
burned), and a non-committed `total_minted_atoms` counter (exposed
for stats, not in the state root). The `state_root` is

```text
sha256(
  "hodl-state-v3" ||
  accounts_root  ||             // 256-level sparse Merkle tree
  nullifiers_hash ||
  "|retarget|" ||
  current_r_le_bytes(8) ||
  current_window_atoms_be(8) ||
  window_start_tag(1) ||        // 0x00 = None, 0x01 = Some(h)
  [window_start_l1_height_be(4)] ||  // only present when tag = 0x01
  "|fee|" ||
  fee_addr_tag(1) ||             // 0x00 = None, 0x01 = Some(addr)
  [fee_addr_xonly(32)]           // only present when tag = 0x01
)
```

where `accounts_root` is a sparse Merkle tree over the accounts map, keyed by the 32-byte x-only L2 address. Each populated leaf hashes `(addr, balance_be, nonce_be)` under tag `"hodl-smt-leaf-v0"`; unpopulated subtrees use a precomputed default-hash cache. This structure supports `O(log N)` inclusion proofs (and `O(log N)` non-inclusion proofs for addresses that don't exist), which is what enables the light-client model in the next section. `nullifiers_hash` is just a flat sorted-list hash — users don't query the nullifier set, and intra-L2 anti-reuse is enforced at apply time.

## Fees

A flat percentage protocol fee is deducted from every transfer:

```text
fee   = max(MIN_FEE, amount * FEE_BPS / 10_000)
total = amount + fee
```

Defaults: `FEE_BPS = 1` (1 basis point = 0.01%), `MIN_FEE = 100`
atoms. The sender's balance decreases by `total`; the recipient
receives `amount`; `fee` credits the L2 account named in genesis
as `sequencer_fee_address`. If that address is `None` (the
default pre-Phase-3 placeholder), the fee atoms are burned —
total supply decreases.

Mints don't pay fees. The lock + L1 broadcast already costs the
user real BTC fees, and mint volume is low; adding L2-side fees
on entry buys nothing.

Anti-DoS rationale: with zero fees, an attacker can loop transfers
between two accounts they own at no marginal cost. The bottleneck
under attack is signature verification (~50 μs each on a laptop)
× block-build state mutation. Even a sub-cent fee bound makes the
attack economically meaningful while staying invisible for real
users.

The fee parameters are demo-tuned to keep the regtest experience
sensible; mainnet values will be re-derived alongside the rest of
the consensus constants.

## Sequencer identity and soft confirmations

On first chain init the sequencer generates a BIP340 Schnorr
keypair and persists the secret in its store. The public key is
embedded in the L2 genesis block header as both `producer` (the
identity of who built the block — also stamped into every
subsequent non-genesis header) and `sequencer_fee_address` (the
recipient of all per-transfer fees). The chain commits to both
via the genesis state_root, so a follower starting cold from
L1 can derive the canonical pubkey without an out-of-band
config delivery.

On every accepted `POST /mint` and `POST /transfer`, the
sequencer returns a `SoftConf` receipt:

```rust
struct SoftConf {
    tx_hash:           H256,
    target_l2_height:  u32,       // current head + 1 at acceptance
    accepted_at_unix:  u64,
    sequencer_sig:     schnorr::Signature,
}
```

The signature is BIP340 Schnorr over the canonical sighash
`sha256("hodl-softconf-v1" || tx_hash || target_l2_height_be ||
accepted_at_unix_be)`, made with the sequencer's identity key.

Trust posture today: the receipt is informational. The sequencer
*can* later drop the tx at block-build (insufficient balance after
a parallel transfer, etc.) or include it at a later height. What
the receipt *does* give us is a cryptographic basis for
equivocation detection — if a sequencer ever signs two
conflicting receipts (same `tx_hash` → different
`target_l2_height`s, or includes the tx past the committed
height) anyone holding both can prove misbehaviour against the
known pubkey. Slashing on top of this is future work; the
primitives are in place.

In the UI: the wallet shows `[SOFT]` on every accepted submission,
poll-watches the verified head, and flips to `[L1-CONFIRMED]` once
`verified_head.l2_height >= soft_conf.target_l2_height`.

## L1 attestation chain

L2 blocks are committed on L1 via OP_RETURN attestation
transactions. Each attestation tx's vout=0 carries the 73-byte
payload below and vout=1 is a change output that becomes the next
chain anchor. Attestations form an explicit on-chain linked list
rooted at `anchor_0`, which the sequencer creates at cold-start and
embeds in the L2 genesis block header.

Under the sub-L1 cadence (Phase 2 onward), each L1 attestation
commits to the **latest** L2 head at posting time — not the
unique L2 block "for this L1 block". A single attestation
therefore batches every L2 block produced since the previous
attestation. Followers walk the L1 attestation chain as before,
but for each chain step they now iterate every L2 block in the
range `(prev_attested_height, current_attested_height]`,
verifying state-transition continuity on the way and pinning only
the final block of the range against the L1 attestation's
`(l2_block_hash, state_root)` pair.

### Attestation payload (73 bytes, vout=0)

```text
magic(4) = "HODL"
version(1) = 0
height(4 BE)              // L2 head height attested by this tx (see semantic note)
l2_block_hash(32) = sha256("hodl-block-v3" || canonical(header))
state_root(32)
```

Fits comfortably under the 80-byte standard OP_RETURN limit.

**Semantic note on `version` and `height`.** The wire format is
unchanged from earlier revisions — `version = 0` here is the
*format* version, not a protocol-revision counter. Under the Phase-2
sub-L1 cadence the *meaning* of `height` shifted from "the unique L2
block for this L1 block" to "the latest L2 head at attestation time",
batching every L2 block produced since the previous attestation.
Followers handle this transparently by walking the L2 chain forward
from the previously-attested height (see "L1 attestation chain"
intro above); no decoder change required.

`l2_block_hash` is the v3 block-hash form — `sha256("hodl-block-v3"
|| canonical(header))`. See "L2 block structure" for the canonical
header byte layout.

### Attestation transaction shape

```text
vin[0]  = anchor_{n-1}                              // previous chain link
vout[0] = OP_RETURN <73-byte attestation_n payload>
vout[1] = value(anchor_{n-1}) − fee → wallet addr    // == anchor_n
locktime = 0
```

Sequencer builds it via bitcoind's `send` RPC with
`options.inputs=[anchor_{n-1}]`, `add_inputs=false`,
`change_position=1`. bitcoind funds, signs, broadcasts atomically.

### Chain root

On cold start, the sequencer:

1. Calls `listunspent` and picks its largest UTXO as `anchor_0`.
2. Records `anchor_0`'s outpoint into the L2 genesis header
   (`L2BlockHeader.anchor_outpoint: Option<OutPoint>`, populated only
   at height 0).
3. Persists `anchor_0` as the active chain anchor in its store.

Nodes cold-start by fetching the L2 genesis block from the sequencer's
`GET /block/0` endpoint, reading `anchor_outpoint` out of its header,
and persisting it as their own chain anchor.

### Sequencer authentication

Implicit, by chain inheritance. A node only follows the unique tx that
spends its currently-tracked anchor. An impostor who broadcasts a HODL
OP_RETURN with unrelated inputs is invisible; an impostor who tries to
spend the anchor with a different tx loses to whichever spend bitcoind
mines first. No `SEQUENCER_PK` is needed — the protocol is identified
by the (genesis-embedded) `anchor_0` outpoint and "chain inherited
from it" semantics.

### Equivocation

Each anchor is a single UTXO on L1, and a UTXO can be spent at most
once. The sequencer therefore cannot publish two competing attestations
at the same L2 height; whichever spend bitcoind mines wins, and the
other is invalidated by the L1 mempool / consensus.

### L1 reorg recovery

The sequencer tracks every posted attestation tx in
`pending_attestations` (a JSON blob in its kv store) until the tx
reaches `REORG_FINALITY_DEPTH = 2` L1 confirmations. Each pending
entry records `(txid, spent_anchor, new_anchor, l2_head_height,
posted_at_l1_height)`. On every L1 poll the sequencer asks
bitcoind for each tx's status:

- **Confirmed at ≥ 2 confs** → drop from pending. The post is
  past reorg risk and the new anchor is canonical.
- **Mempool** (0 confs) → keep tracking. bitcoind will mine it
  on the next block.
- **Reorged** (negative confs — tx was once mined, now back in
  mempool) → keep tracking. bitcoind will re-mine it in the new
  canonical chain. Logged but no recovery needed.
- **Missing** (not in chain, not in mempool — extreme case
  where the anchor was double-spent or evicted) → revert the
  chain anchor to `spent_anchor` and rewind
  `last_attested_l1_height` by 1, so the next L1 tick posts a
  fresh attestation chained from the original anchor.

L2 chain state never reorgs. The sequencer's persisted
`LedgerState` is treated as canonical across L1 reorgs;
re-attestation just re-publishes the same L2 head under a new L1
parent.

### Failure modes (v0)

- **L1 reorg of the chain anchor**: covered by the recovery path
  above. Survivable when the tx stays in bitcoind's mempool;
  recovered structurally when the anchor input is evicted. Bitcoin
  reorgs of depth ≥ 2 are historically single-digit per year.
- **RBF / fee-bumping the attestation tx**: would temporarily fork the
  chain in mempool until one wins. The sequencer does not RBF in v0.
- **Dust**: the anchor UTXO shrinks by `fee` per attestation. With a
  1 BTC seed at the regtest fallback rate (~1000 sat/vB), this lasts
  ~500 attestations; at signet rates (~1 sat/vB), ~3500 days of
  attestations. Production deployment would need an operator top-up
  mechanism (out of POC scope).

## Light clients

The UX target is: heavy setup (locking BTC, minting UTXOs) lives on a
desktop with full L1 access; everyday L2 usage (querying balance,
sending transfers) runs on a phone with nothing but HTTP. The chain-of-
anchors construction makes this almost-free; Merkleized state is the
last ingredient.

### Trust tiers a client can choose

1. **Full validator** (`hodl-node` today): owns a bitcoind, replays
   every L2 block, re-verifies every mint witness against L1, recomputes
   `state_root`. Trusts nothing.
2. **L1-light validator** (future): same L2 logic, but reads L1 via an
   Esplora server instead of running bitcoind. Still does full L2 replay,
   so desktop-class.
3. **State-light**: doesn't replay L2 at all. Walks the L1 attestation
   chain via a block-explorer API to learn the current `state_root`, then
   verifies its own account via an SMT inclusion proof against that
   root. Phone-class. Trust gap (v0): state-correctness of the
   transition itself. Closed later by a ZK validity proof.
4. **Pure RPC**: polls the sequencer for balance; verifies nothing.

### L1 walk via Esplora

A state-light client takes the genesis anchor outpoint (read once from
the L2 genesis header at install time or shipped in client config) and
walks the L1 chain by alternating two Esplora endpoints:

```text
GET /tx/:txid/outspend/:vout      → next tx that spent this outpoint
GET /tx/:txid                     → that tx's full structure
```

For each step, parse `vout[0]` as a hodlchain OP_RETURN attestation
payload, record `(height, block_hash, state_root)`, advance to
`new_anchor = (txid, 1)`. Two cheap HTTP calls per L2 block. No
bitcoind required. The Esplora endpoint is a single dependency on a
third-party (or self-hosted) service; well-established phone-Bitcoin-
wallet pattern.

### State-light balance verification

The sequencer / node exposes `GET /balance/:addr` returning the L2
account plus an SMT inclusion proof against the latest `state_root`
they have. The light client checks:

1. The returned `state_root` matches the one it pulled off L1 for the
   declared L2 height.
2. The inclusion proof (256 sibling hashes, leaf-to-root) reconstructs
   `state_root` from the claimed `(balance, nonce)` leaf.

Non-existent accounts return a non-inclusion proof (`LeafKind::Empty`)
which verifies the same way; a light client whose first action is
"check that my balance is zero before I do anything" gets a meaningful
answer.

### Why no live L1 watching during the lock

Worth stating explicitly: between a user's mint and their unlock, there
is no on-chain event they must react to. The funding UTXO is provably
unspendable by anyone (NUMS internal key + CSV) for `lock_blocks`
blocks. There is no escape hatch, no challenge window, no fee bumping
they need to do. A phone-only user who's been offline for a week
catches up on a week of attestations in one Esplora-walking burst when
they come back online.

L1 is only needed for: minting a new UTXO; reclaiming after the lock;
being a full validator (re-verifying mint witnesses themselves).

### What's wired today

POC implementation (`hodl-wallet`):

- `verify-balance` — query `/balance/:addr` (now carrying the SMT
  inclusion proof + `state_components`), re-derive `state_root` from
  components, verify the SMT proof, check the leaf payload. Optional
  `--against <hex>` for binding to an externally-supplied state_root.
- `light-head` — fetch L2 genesis from the L2 RPC for `anchor_0`,
  walk the attestation chain via Esplora endpoints
  (`/tx/:txid/outspend/:vout` + `/tx/:txid`), report the latest
  `state_root` derived from L1.
- `light-balance` — walk the L1 chain to enumerate every L2 block;
  fetch each body from the node/sequencer; re-verify every transfer
  signature, every mint witness (via Esplora, no bitcoind), and the
  state_root at every height; then read the balance from the
  locally-rebuilt `LedgerState`. No honest-validator assumption.

The demo runs all of `verify-balance`, `light-head`, `light-balance`
against `hodl-node`, which exposes the Esplora-compatible subset
(`GET /tx/:txid`, `GET /tx/:txid/outspend/:vout`, and `GET
/blocks/tip/height`) backed by its own `anchor_spends` SQLite index
plus bitcoind's `getrawtransaction` (`txindex=1` required). In
production the wallet's `esplora_url` would point at mempool.space /
an electrs deployment / the user's own electrs, *not* at hodl-node —
the API surface is the same.

### Where light-client trust still leaks (v0)

- **Block-body availability** — to replay, the wallet must download
  every L2 block body from *someone*. The sequencer is the obvious
  candidate; nodes can also serve them. A malicious sequencer that
  refuses to serve a particular block body halts every light client.
  Migrating to a public DA layer (e.g. Celestia) for block bodies
  closes this.
- **Esplora honesty** — the wallet does not do Bitcoin SPV /
  merkle-path verification of the L1 attestation tx or of mint
  outpoints. The chosen Esplora endpoint is trusted not to lie about
  what L1 contains. Pointing the wallet at a locally-run electrs
  removes this.
- **Sequencer liveness** — the sequencer can stop producing blocks at
  will. Replaced later by multi-sequencer / sequencer rotation.
- **Mint anonymity** — full nodes (and any Esplora endpoint the wallet
  pulls mint outpoints from) see which L1 UTXO funded a given L2
  mint. Future work: anonymous mint via aut-ct ring proofs.

## API reference

Both daemons serve an OpenAPI 3 spec generated by `utoipa` from the
handler signatures and `hodl-core` types, and an interactive Swagger UI
on top of it:

```text
hodl-sequencer (default port 28080 in the demo):
  GET  /docs/           — Swagger UI
  GET  /openapi.json    — raw OpenAPI spec
  Paths:
    POST /mint              — submit a mint witness
    POST /transfer          — submit a signed transfer
    GET  /head              — L2 head height + block_hash + state_root
    GET  /balance/:addr     — incl. on-chain nonce + mempool-aware effective_nonce
    GET  /block/:height     — full L2 block (header + txs)
    GET  /witness/:height   — BlockWitness for that L2 height (light-client input)

hodl-node (default port 28081):
  GET  /docs/           — Swagger UI
  GET  /openapi.json    — raw OpenAPI spec
  Paths:
    GET  /head                              — node's L1-attested L2 head
    GET  /balance/:addr                     — node's view (effective_nonce == nonce here)
    GET  /block/:height                     — block body (served from local store)
    GET  /witness/:height                   — block witness (light-client input)
    GET  /nullifiers                        — full consumed-nullifier set (bootstrap)
  Esplora-compatible subset (light-wallet L1 walk):
    GET  /tx/:txid                          — Bitcoin tx info
    GET  /tx/:txid/outspend/:vout           — spender of an outpoint, if any
    POST /tx                                — broadcast L1 tx
    GET  /address/:addr/utxo                — UTXOs at an L1 address
    GET  /blocks/tip/height                 — L1 tip height
```

Request and response schemas are derived from the `#[derive(ToSchema)]`
on each type in `hodl-core` (`SubmitMintRequest`, `BalanceResponse`,
`L2Block`, `MintEntry`, `MintProofEnvelope`, `InclusionProof`,
`StateComponents`, `H256`, …). Adding a new type or field flows
through to the rendered docs without any separate doc maintenance.

External Bitcoin types are documented via `#[schema(value_type = …)]`
overrides: hash-shaped ones (`Txid`, `ScriptBuf`, `XOnlyPublicKey`,
`schnorr::Signature`, `H256`) are documented as hex strings;
`bitcoin::OutPoint` shows up as `{txid, vout}` via the
`hodl_core::schemas::OutPointWire` doc stub.

### Key response shapes worth pinning down by hand

The OpenAPI doc is authoritative, but two responses get referenced
often enough across the rest of this doc that listing them inline is
worth the redundancy:

`POST /mint` / `POST /transfer` accept return:

```rust
struct SubmitTransferResponse {
    accepted:  bool,
    error:     Option<String>,         // Some on reject
    soft_conf: Option<SoftConf>,       // Some on accept — sequencer-signed receipt
}
struct SubmitMintResponse {
    accepted:      bool,
    error:         Option<String>,
    mint_amount:   Option<Amount>,     // f_mint(V, T, r) at submit time, best-effort
    nullifier_hex: Option<String>,
    soft_conf:     Option<SoftConf>,
}
```

`GET /balance/:addr`:

```rust
struct BalanceResponse {
    address:             L2Address,
    balance:             Amount,
    nonce:               u64,             // on-chain nonce (use for light verification)
    effective_nonce:     u64,             // mempool-aware (sequencer only); == nonce on node
    l2_height:           u32,
    state_root:          H256,
    state_components:    StateComponents, // inputs that hash to state_root
    proof:               InclusionProof,  // SMT (non-)inclusion against accounts_root
    total_minted_atoms:  u64,             // Σ-of-all-mints; not in state_root, stats only
}
```

`effective_nonce` is the field powering the wallet's nonce-race fix
(Phase 5): the sequencer reports `state.nonce_of(addr) +
mempool_count_from(addr)` so a wallet submitting back-to-back transfers
gets a fresh nonce even before the prior one has landed in a block.
The node, lacking a mempool view, sets `effective_nonce = nonce`.

## Mint witness pluggability

The mint pipeline is structured around two abstractions:

```rust
trait MintProof {
    fn nullifier(&self) -> Vec<u8>;
    fn verify<C>(
        &self, secp: &Secp256k1<C>, l1: &dyn L1View,
        l2_destination: L2Address, r: f64,
    ) -> Result<MintCredit, MintError>;
}

enum MintProofEnvelope {
    V0Outpoint(OutpointProof),
    // V1Ring(RingProof) — later
    // V2Zk(ZkProof)    — later still
}
```

Variants:

- **v0** `OutpointProof` — nullifier = serialised L1 outpoint, witness = (user_pubkey, lock_blocks, schnorr_sig over (outpoint, l2_dest)).
- **v1 (later)** `RingProof` — nullifier = LSAG key image, witness = (ring_sig over equal-denom CSV UTXOs, key_image).
- **v2 (later)** `ZkProof` — a succinct proof of the §5.4 statement; nullifier is one of the proof's public outputs.

The envelope is committed into the L2 block as `MintEntry { event, witness }`. Adding a future variant means:

- Adding the enum variant + implementing `MintProof` for it.
- The sequencer's `submit_mint` already speaks `MintProofEnvelope`, so a new variant slots in without touching that handler.
- The node's follower already calls `verify_mint_entry`, which delegates to the envelope's `MintProof::verify`, so a new variant slots in there too.
- Block format is unchanged: `MintEntry` is generic over the envelope variant.
- The consumed set is keyed on opaque `Vec<u8>` nullifiers, not on `OutPoint`, so the v1 ring-image variant fits without schema change.

What v1 will additionally need (deferred): fixed allowed `(V, T)` denominations (to maximise ring size), an `aut-ct` integration, and a small change to the wallet to produce ring signatures.

## Trust model

- The sequencer is **trusted not to censor** in v0. **Equivocation is structurally prevented** by the L1 attestation chain: each anchor UTXO can be spent only once.
- Bitcoin's L1 trust model is unchanged: CSV locks behave per consensus, L1 finality is L1 finality.
- L2 nodes do *not* trust the sequencer about mint validity. Each `L2Tx::Mint(MintEntry)` in every block carries its own witness; the node re-runs `verify_mint_entry` against L1 and rejects the block if the witness-derived credit disagrees with the declared event in any field.

## What we deliberately did not build

- No fee market on L2.
- No reorg handling (1 confirmation; on regtest/signet reorgs are vanishingly rare for a demo).
- No P2P among nodes — everyone follows the sequencer's HTTP endpoint directly.
- No fraud proofs, no DA layer beyond "the sequencer serves bodies".
- No pegging of any kind.

Each of these is a known follow-up; none is required to demonstrate the issuance primitive end-to-end.

## Open POC tasks (informational)

After the foundation lands (this design + `hodl-core`):

1. **hodl-wallet** — keygen, derive P2TR CSV-locked mint addresses, observe funding via Esplora, generate mint messages, submit transfers, reclaim matured UTXOs, query balance.
2. **hodl-sequencer** — HTTP intake (`POST /mint`, `POST /transfer`, `GET /block/:height`, `GET /head`), per-L1-block tick, OP_RETURN poster (uses its own bitcoind L1 wallet for fee inputs).
3. **hodl-node** — L1 scanner, sequencer follower, replay + state, balance/query HTTP.
4. **End-to-end demo script** — spin up regtest, mine some BTC, run the wallet through a mint + transfer.
