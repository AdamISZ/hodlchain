# Sub-L1 block cadence + small percentage fees

**Status:** plan / RFC. Not implemented. Review before proceeding.

This document plans the next development cycle. Two coupled
changes:

1. **30s L2 block cadence**, decoupling L2 block production from L1
   anchoring. Soft confirmations within seconds; L1-hard
   confirmations within ~10 min.
2. **Small percentage fees** (default 1 basis point = 0.01%) on
   transfers, paid to the sequencer's L2 account. Provides
   anti-DoS without a fee market.

Together they position the L2 as: *near-instant soft confirmations,
L1-final in ~10 min, 0.01% fee* — competitive with Lightning on UX
and cheaper on fees, while preserving the permissionless-mint and
Bitcoin-anchored properties.

## 1. Goals + rationale

- **A real "what's the L2 for?" answer.** The dual-user analysis
  (yield-seeking hodler + payments user) points at User B as the
  beneficiary of speed-and-cost work. This cycle serves them.
- **Match industry practice.** L2BEAT's soft/hard-confirmation
  taxonomy, Arbitrum's sequencer-posted-to-L1-inbox flow, and the
  Ethereum research community's "based preconfirmations" all
  describe roughly the design here. We aren't inventing — we're
  implementing a documented pattern.
- **Modest engineering lift.** ~3–5 weeks total, vs. months for any
  meaningful privacy direction. Sequencing decision: ship this
  *before* committing to privacy work, so privacy work can be
  prioritised against real usage data.

## 2. Scope

**In scope:**
- Producer block cadence becomes time-based (default 30s).
- L1 attestation cadence remains L1-block-driven (one per L1
  block); each attestation covers all L2 blocks since the previous.
- Sequencer signs per-tx soft-confirmation receipts.
- L1 reorg robustness: attestation re-post on reorg, L2 state
  never reorgs (sequencer's persisted L2 chain is canonical).
- Percentage fees on transfers; fees credit a designated sequencer
  L2 address.
- Wallet + GUI distinguish "soft (sequencer-acked)" from
  "L1-confirmed" tx states.

**Explicitly out of scope:**
- Force-inclusion via direct L1 tx (sequencer-failure escape
  hatch). Recognised pattern from Arbitrum; defer.
- Sequencer rotation, slashing, or multi-sequencer. Single trusted
  sequencer remains the trust posture.
- Fee market / sender-specified fees. Fixed protocol percentage.
- Mint fees. Mints remain fee-free; volume is low and the entry
  experience should stay clean.

## 3. Part A — Fees

### Parameters

```rust
// Demo / regtest values. Mainnet TBD.
pub const FEE_BPS: u64 = 1;       // 0.01% (1 basis point)
pub const MIN_FEE: u64 = 100;     // 100 atoms minimum (anti-spam floor)
```

Fee formula on a transfer of `amount`:

```
fee   = max(MIN_FEE, amount * FEE_BPS / 10_000)
total = amount + fee
```

### Sequencer fee account

A designated L2 address, derived from the sequencer's signing key
(see §4 for that key's introduction). Configured at sequencer-init
time; written into the L2 genesis block header so light clients
know which address receives fees.

The sequencer holds a real L2 token balance like any other
account. They can transfer or hold; they aren't a privileged
account economically, just operationally.

### Where the changes land

- `hodl-core::consensus`: add `FEE_BPS`, `MIN_FEE` constants
  (parallel to `INITIAL_R` etc).
- `hodl-core::state::apply_transfer`: subtract `total` from
  sender, credit `amount` to recipient, credit `fee` to sequencer
  account.
- `hodl-core::block`: genesis header gains a
  `sequencer_fee_address: L2Address` field.
- `hodl-wallet::ops::transfer`: display computed fee to user
  before submission.
- GUI transfer view: show "amount", "fee (auto)", "total" rows.

Effort: ~2–3 days, dominated by test updates that assume current
balance arithmetic.

## 4. Part B — Sub-L1 block cadence

### Producer cadence

The producer's tick loop changes from event-driven (poll bitcoind,
react) to timer-driven plus an L1-event handler:

- Every `BLOCK_INTERVAL_MS` (default 30_000): drain mempool, build
  L2 block, persist, emit.
- On each new L1 block: post L1 attestation covering all L2 blocks
  since the previous attestation, advance the anchor chain.

Block header changes:

- Keep `l1_height` / `l1_block_hash` as "L1 view at production
  time" (useful for mint-witness verification and retarget
  bookkeeping). Default: keep, with semantics clarified.
- **Add `producer: L2Address`** — the identity that produced this
  block. Today there's only one sequencer so this field is
  redundant in practice; tomorrow under multi-sequencer (whatever
  form that takes) it identifies the responsible party for soft
  confs, fee routing, and attribution. Cheap to add now (32 bytes
  per block, but the field defaults to a fixed value so a single
  copy in the chain config covers most uses); a hard fork to add
  later. Worth paying the cost now.

The `producer` field is the **L2 identity address** (matching the
sequencer-identity pubkey from genesis). Under future multi-
sequencer designs that use threshold signing (FROST, MuSig2), the
field can still hold a single aggregated L2 address — so single-
key-shaped headers and single-signer-shaped L1 attestation
chains are forward-compatible with collaborative production. The
field doesn't constrain us to one signer; it just gives us a place
to name whoever was responsible.

### L1 attestation format

Today's `Attestation` OP_RETURN commits to one L2 block:
```
magic | version | height | l2_block_hash | state_root
```

New format commits to a *batch* of L2 blocks (from the previously-
attested height to now):
```
magic | version | end_height | end_l2_block_hash | end_state_root
```

Light clients walk the L1 attestation chain (unchanged), then walk
the L2 chain forward from the previously-attested head to the
newly-attested head. The L2-side walk verifies signatures, state
transitions, and that the chain's tip matches `end_l2_block_hash` /
`end_state_root`.

Alternative we considered and rejected for now: commit to a Merkle
root over the batch of L2 blocks, allowing inclusion proofs for
intermediate blocks. Useful for fraud-proof-style claims; not
useful yet.

### Sequencer identity + soft confs

The sequencer needs a **stable identity key**, separate from its
bitcoind wallet key:

- Generated at sequencer init; persisted in the seq config.
- Published in the L2 genesis header.
- Used to sign per-tx soft-confirmation receipts.

Soft confirmation receipt returned from `POST /transfer`:
```rust
struct SoftConf {
    tx_hash: H256,
    target_l2_height: u32,
    accepted_at_unix: u64,
    sequencer_sig: SchnorrSignature,
}
```

The signature is over `(tx_hash, target_l2_height, accepted_at_unix)`.
Wallet stores the receipt; can show it to a recipient as evidence
that the sequencer committed to inclusion.

**Equivocation detection (built-in but not punished yet):** if the
sequencer ever produces two conflicting receipts for the same
`tx_hash` with different `target_l2_height`s, anyone holding both
can prove sequencer misbehaviour. We're not building slashing in
this cycle, but the cryptographic primitives are in place for
future work.

### L1 reorg handling

Today the producer fire-and-forgets attestations. We need explicit
state tracking:

- Track the L1 confirmation depth of every attestation tx posted.
- Treat an attestation as "L1-soft" until N=2 confs, "L1-hard"
  after.
- If an attestation gets reorged out (the L1 block holding it
  disappears from the canonical chain), re-post automatically. The
  L2 chain doesn't reorg; only the L1 attestation reference does.

L2 chain reorg policy: **sequencer's persisted L2 chain is
canonical**, period. Across sequencer crash/restart, the persisted
state is treated as truth and re-attestation happens to L1. The
sequencer never serves a different L2 chain than the one it
previously committed to (this is what soft-conf signatures
prevent).

### Wallet + GUI changes

Two visible states for any tx:

1. **Pending → Soft-confirmed.** Sequencer accepted, signed
   receipt held by wallet. Visible icon: green check with "soft"
   tag.
2. **Soft → L1-confirmed.** Attestation covering this tx's L2
   block has reached N=2 L1 confs. Icon: green check, no tag.

Wallet polling: default 10s. Configurable.

Effort breakdown:
- Producer cadence + attestation batching: ~1 week.
- Sequencer identity + signed receipts: ~3–5 days.
- L1 reorg robustness + tests: ~1 week (highest-risk item).
- Wallet/GUI updates: ~3–5 days.
- Light client adaptation: ~3 days.

## 5. Open questions

1. **Genesis-block format change.** Adding `sequencer_fee_address`
   and the sequencer identity pubkey to genesis is a hard-fork
   change. Easiest on regtest where we can reset. Worth committing
   to *now* since we'll need it for any post-POC migration too.
2. **Retargeting math under new cadence — no paper change.**
   Verified: the §7 algorithm is parameterised entirely in L1
   blocks (`m_obs`, `m_star`, `delta_actual` all in
   atoms-per-L1-block). L2 block cadence never enters the formula.
   The `end_of_block` retarget check still fires per L2 block but
   short-circuits via the existing `delta_actual == 0` branch
   whenever multiple L2 blocks land at the same L1 height. The
   first L2 block following a new L1 height is where the retarget
   can actually trigger — identical behaviour to today's
   "threshold crossed within one L1 block" case. **No derivation
   change; only retarget unit-test fixtures need updating** to
   exercise the multi-L2-block-per-L1-height path. Half a day.
3. **What's in the soft-conf receipt?** Minimal:
   `(tx_hash, target_l2_height, timestamp, sig)`. Alternative:
   include a projected state-root-after-inclusion. The latter
   makes the receipt fully self-contained for the recipient (they
   can verify state without waiting for the L2 block), at the cost
   of the sequencer having to compute the proposed state ahead of
   block-build. Default: minimal; revisit if recipients ask for
   more.
4. **N for L1 confirmation depth.** Default 2. Bitcoin reorgs of
   depth ≥ 2 are rare (single-digit per year historically), but
   conservative for the POC. Configurable.
5. **L1-attestation cadence.** We're proposing one attestation per
   L1 block (unchanged from today). Could attest more rarely (e.g.
   every 6 L1 blocks) to save fees, at the cost of longer
   soft-to-hard latency. Default: one per L1 block.
6. **Multi-sequencer forward compatibility — items to think about
   even though they're out of scope here:**
   - **The `producer` field in the block header** (added under §4
     for exactly this reason). Single-sequencer today, but the
     field is there so multi-sequencer doesn't require a header
     hard fork later.
   - **Threshold-signed L1 anchor + producer field stays
     single-key-shaped.** FROST / MuSig2 produces a single
     aggregated Schnorr signature indistinguishable from a
     single-signer one. So adding a threshold-signed committee
     later doesn't require changing the L1 anchor model or the
     `producer` field's type — it just means the underlying signing
     ceremony is collaborative. The on-chain shape is unchanged.
   - **Sequencer fee address.** Single L2 address today. Under
     multi-sequencer, fees naturally route to whoever's named in
     each block's `producer` field — so we don't need a per-block
     fee-address field; the producer field carries that
     information.
   - **Things that *would* genuinely need new protocol work for
     multi-sequencer:** off-chain coordination (mempool gossip,
     leader election or rotation, signing-ceremony coordination),
     and L1-reorg-handling responsibility ("who re-attests?").
     These are well-understood BFT problems and we accept that
     they'll need design work at the time. Nothing about today's
     plan precludes them.

## 6. Risks

- **L1 reorg handling correctness.** Highest engineering risk.
  Mitigations: explicit state machine, test harness that simulates
  reorgs against a regtest bitcoind, exhaustive case enumeration.
- **Retargeting test fixtures.** The math is unchanged (see §5.2),
  but existing tests implicitly use one L2 block per L1 height.
  Add fixtures that exercise multi-L2-block-per-L1-height
  sequences so the Δ=0 deferral branch is covered under new
  cadence.
- **Light client compatibility.** Existing wallets won't speak the
  new attestation format. Since this is a hard fork on regtest,
  the wallet versions move together. No live-deployment
  compatibility constraint yet.
- **Wallet UX confusion.** "Soft" vs "L1-confirmed" is a real
  distinction users will hit. Risk: people treat soft as final and
  get surprised later. Mitigation: clear labelling and a
  documented trust posture in `run-the-gui.md`.

## 7. Phased plan

Approximate calendar order. Each phase is independently mergeable.

- **Phase 1 — Fees (~2–3 days).** Smallest, most isolated change.
  Lands the constants, the apply_transfer arithmetic, the
  sequencer fee account scaffolding (the address itself can be a
  placeholder until Phase 3 adds the real sequencer identity key).
- **Phase 2 — Producer cadence + attestation batching (~1 week).**
  Decouple L2 block production from L1 events. New attestation
  format. Light client walk pattern updated. End of phase:
  regtest produces 30s blocks; L1 attestation covers a batch.
- **Phase 3 — Sequencer identity + signed soft confs (~3–5 days).**
  Adds the identity key. Soft-conf receipts plumbed through
  HTTP and wallet. Sequencer fee address derived from this key.
- **Phase 4 — L1 reorg robustness (~1 week).** Attestation
  re-post on reorg, confirmation-depth tracking. Most of the
  testing weight lands here.
- **Phase 5 — Wallet + GUI UX (~3–5 days).** Show
  pending/soft/hard distinctly. Fee display in transfer view.
- **Phase 6 — Docs + retarget test fixtures (~2–3 days).** Update
  `docs/design.md`, `run-the-gui.md` to reflect cadence + fees.
  Add unit-test fixtures exercising multi-L2-block-per-L1-height
  retarget paths.

Total: ~3–5 weeks of focused work.

## 8. Decision points to confirm before Phase 1

1. **`FEE_BPS = 1`, `MIN_FEE = 100`** as starting values?
2. **30s as the L2 block interval?** Could be 10s or 60s; tradeoff
   is sequencer load (lower interval → more block-production
   overhead) vs UX (higher interval → slower soft conf). 30s is a
   reasonable middle ground.
3. **One attestation per L1 block, N=2 L1 confs for hard?**
   Defaults reasonable; flag if you'd prefer different.
4. **Soft-conf receipts: minimal payload, not state-root
   projection?** Default minimal.
5. **Hard fork on regtest (wipe and redeploy), not a soft
   migration?** Default hard fork — only regtest users exist.
