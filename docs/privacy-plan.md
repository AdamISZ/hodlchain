# Privacy roadmap: Confidential Transactions on hodlchain L2

**Status:** plan / RFC. Not implemented. Open questions remain in
§5; we should resolve them before starting Phase 0.

This document plans the addition of **Confidential Transactions
(CT)** — Pedersen-committed, range-proven amounts à la Liquid — to
the hodlchain L2. CT hides transaction amounts; it does NOT hide
which output is spent (that's ring sigs / shielded pools — out of
scope here) or who is sending to whom (that needs stealth
addresses, also deferred).

## 1. Why this privacy layer first?

Privacy splits roughly three ways:

| Approach | Hides | Cost | Reference |
|---|---|---|---|
| **Confidential Transactions** | amount only | ~600 B range proof per output | Liquid / Elements |
| **Ring sigs (CT + RingCT)** | amount + which input was spent | ~1–4 KB per input | Monero |
| **Full shielded (zk-SNARK)** | amount + sender + recipient | 200–1000 B + trusted setup or aggregable proofs | Zcash |

CT alone covers a large fraction of the practical privacy value at
a fraction of the implementation cost. The cryptographic primitives
are already in production via `secp256k1-zkp` + Elements, so we
don't need to invent anything — just integrate.

The other two layers can be added on top later: ring signatures sit
*over* committed outputs; SNARK shielding is a parallel pool that
can co-exist with a transparent + CT pool. CT is the natural first
step.

## 2. What stays public

CT hides amounts in transfers. A few things remain visible on chain
even after this work:

- **L1 mint amounts.** The lock value `V` and duration `T` are
  observable on Bitcoin L1; `r` is consensus state; `mint_fn(V, T,
  r)` is therefore computable by any observer. The mint *event* on
  L2 must publish a clear amount so the chain can verify the
  commitment opens correctly. We can launder this later by spending
  the mint output forward through a CT transfer.
- **Reclaim amounts.** The L1 spend of a mint UTXO is necessarily
  public on Bitcoin.
- **Graph topology.** Sender and recipient addresses are unchanged
  from today. Stealth addresses (Monero-style one-time addresses)
  are a separate future item.
- **Transfer existence + timing.** Every transfer still appears as
  an L2 block tx; we hide amounts, not the act of transferring.

This is the same shape as Liquid: peg-in / peg-out is public,
intra-network transfers are confidential.

## 3. Three architectural models — comparison

There are three ways to add CT to an account-style L2 like this
one. Each has the same cryptographic primitives (Pedersen +
Bulletproofs+ + ECDH-encrypted amount payload) but stores L2 state
very differently.

| Model | One-line summary | Code churn |
|---|---|---|
| **A. UTXO + CT** | Liquid-style: each tx consumes outpoints, produces new commitments. State is a UTXO set + nullifier set. | Largest — replaces `accounts` map with two new sets. |
| **B. One-shot accounts + CT** | Each receive creates a *new* L2 account leaf identified by `H(txid \|\| output_index)` or similar. Spending marks the account spent. Functionally isomorphic to UTXO; cosmetically still "accounts". | Medium — reuses SMT keyed by L2Address with a new leaf shape and a new identity scheme. |
| **C. Mutable accounts + CT** | Account balance becomes a Pedersen commitment. Receives are homomorphically *added* to the recipient's existing leaf. Senders publish a new balance commitment + output commitment + range proofs + excess sig. | Smallest — only the leaf payload changes; account identity / nonce / SMT machinery all stays. |

Model **C** looks most appealing on raw code-churn metrics, and it's
where I initially leaned. But it has a sharp edge — see §5 — that
may force us to **A** or **B**.

## 4. Cryptographic primitives (common to all three)

Available in `libsecp256k1-zkp` and bound to Rust via `secp256k1-zkp`.
Higher-level CT semantics (rewindable rangeproof construction,
confidential address format, the blinding-key derivation
conventions) live in **`rust-elements` and Elements Core** — read
those for the engineering details, not just the lower-level crate.

**Pedersen commitment to an amount `v`:**
```
C = v · H + r · G
```
`G` is the secp256k1 generator, `H` is an independent NUMS-like
generator, `r` is a per-output blinding scalar. Additively
homomorphic: `C_a + C_b = (v_a + v_b) · H + (r_a + r_b) · G`. This
is what lets the chain check input/output balance without learning
the amounts.

**Range proof** (Bulletproofs+):
- Proves `0 ≤ v < 2^n`; we'd use `n = 64` like Bitcoin / Liquid.
- ~600 bytes per output. Aggregation across a tx cuts per-output
  cost.
- Non-interactive (Fiat-Shamir).
- **Rewindable**: the prover can embed an encrypted payload
  containing `(v, r)` inside the proof's nonce space, recoverable
  by someone holding the ECDH-shared secret. This is the mechanism
  by which a recipient learns the opening of an output sent to them.

**ECDH for amount transport:**
- Recipient has a separate **blinding pubkey** `B = b · G`,
  alongside their normal spend pubkey. Both derived from the same
  BIP32 seed on different branches.
- Sender picks ephemeral `e`; publishes `E = e · G` with the tx.
- Shared secret `s = e · B = b · E`.
- `r_output` for the new commitment is derived from `s` via HKDF.
- Range proof is constructed so it rewinds correctly under `s`.

**Excess signature.** Standard Schnorr over the tx sighash, using
the residual blinding as the key. Replaces (or complements) the
existing per-input Schnorr sig. Chain checks both signature
validity and commitment balance, which together imply correct
amount accounting.

## 5. The receive-decodability problem with model C

This is the part I want to be **very careful** about. The mutable-
account model has an asymmetry the other two don't.

**Setup:** Alice sends `v` to Bob. Alice creates an output
commitment `C_send = v · H + r · G`. The chain updates Bob's
account leaf: `C_Bob_new = C_Bob_old + C_send`. Alice publishes a
rewindable range proof carrying the encrypted `(v, r)` payload to
Bob.

**The risk:** if Alice's encryption is malformed (intentionally or
by bug), Bob cannot recover `(v, r)`. In a **UTXO** or **one-shot
account** model, Bob's wallet would simply *not see* this output
as theirs (it can't be rewound, so it's marked as not-mine — and
the unspendable bytes just sit in the UTXO set). Bob's *other*
funds are unaffected.

In the **mutable-account** model, Bob's account leaf has *already
been updated* to include the bad output. To spend, Bob needs the
new `(v_total, r_total)` matching `C_Bob_new`. If even one receive
is undecodable, Bob's `r_total` is unknown for the rest of time and
**the entire account is unspendable**.

This is qualitatively worse than UTXO: a single malicious send
bricks the recipient's entire balance. Mitigations exist but
require care:

- **Rangeproof-binds-to-blinding-pubkey.** If the rangeproof's
  construction lets the chain publicly verify that the proof is
  "rewindable by `B`" — without itself knowing `b` — the chain can
  reject any output to Bob's account whose rangeproof is not
  Bob-rewindable. This eliminates the bricking attack. **Whether
  Bulletproofs+ as implemented in `secp256k1-zkp` exposes this
  property publicly is the key open question; needs verification
  in the source.**
- **Receiver opt-in.** Outputs aren't auto-added to a leaf; the
  recipient must publish a tx accepting them. Kills the
  unilateral-send property of CT. Not viable.
- **Accept the risk.** Document the failure mode and rely on
  wallet implementations being correct. Borderline acceptable for
  a research POC, not acceptable for anything user-facing.

**This is the deciding question** for whether model C is sound.
If the rangeproof-rewindability property is publicly verifiable,
model C is the right answer (smallest code churn, simplest mental
model). If not, we should go with model B (one-shot accounts) —
which IS functionally UTXO but lets us keep the SMT-keyed-by-
address machinery we have.

## 6. The mutable-account scheme in detail (model C)

Conditional on §5 being resolved in favour of model C.

**Account leaf change:**
```rust
// Today:
struct Account { balance: u64, nonce: u64 }

// Under CT:
struct Account {
    balance_commit: PedersenCommitment,
    nonce: u64,
}
```

**Receive (chain side):** the producer applies a `TransferCT` tx
whose output is addressed to Bob. The chain reads Bob's existing
`C_Bob_old` from the SMT, computes `C_Bob_new = C_Bob_old +
C_output`, and writes it back. No private info revealed.

**Receive (recipient side):** Bob scans new blocks, attempts to
rewind every range proof in every `TransferCT` against his blinding
key `b`. On success, he learns `(v_send, r_send)` and updates his
local `(v_Bob_total, r_Bob_total)` by addition.

**Send:** Alice wants to spend `v_send`. She publishes:
- `C_Alice_new = (v_Alice - v_send) · H + r_Alice_new · G` (new
  Alice balance commitment, with `r_Alice_new` freshly sampled)
- `C_send` for Bob (with ECDH-derived `r_send`)
- Range proofs on `C_Alice_new` (≥ 0 → Alice has enough) and
  `C_send` (≥ 0 → no negative-amount attack)
- Excess signature on the tx sighash, using residual blinding
  `e = r_Alice - r_Alice_new - r_send` as the key
- `E = e_ephemeral · G` for ECDH

The chain reads Alice's `C_Alice_old`, recovers excess pubkey via
`C_Alice_old - C_Alice_new - C_send`, verifies the sig under that
pubkey, verifies range proofs, replaces Alice's leaf with
`C_Alice_new`, and updates Bob's leaf as in "Receive (chain side)".

**Nonce.** Kept for replay protection. The mutable model is more
exposed to certain race conditions (a receive and send in the same
block — see §9) so the nonce earns its keep.

**Mint integration.** Mints produce an account leaf with `r = 0`,
so `C = v · H` is openable by anyone. Spending forward through a
`TransferCT` mixes the public amount into the confidential graph.

## 7. Wallet changes

Conditional on model C.

The wallet must:

1. **Maintain `(v_total, r_total)` locally** for the user's
   account. Updated on every receive.
2. **Derive blinding keys deterministically** from the BIP39 seed.
   Add a `blind` branch to the existing BIP32 derivation:
   `m/HODL'/coin_type'/account'/blind/0` is the (single, stable)
   blinding privkey `b`.
3. **Scan for received outputs.** On each new L2 block, walk every
   `TransferCT` output and try to rewind the range proof with `b`.
   If it rewinds, the output is for us; add `(v, r)` to our
   running totals.
4. **Build CT transfers.** Sample `r_Alice_new`, derive `r_send`
   from the per-output ECDH, generate range proofs, encrypt amount
   payloads, sign.
5. **Display.** The "balance" shown in the UI is `v_total` (read
   from local state, not the chain). Functionally identical to
   today's account balance display.

**Recovery from seed.** If a wallet loses its local state but
retains the BIP39 phrase, recovery requires re-scanning every L2
block ever produced, attempting to rewind every `TransferCT`
output. Linear in block count but unavoidable (the chain stores
only commitments, not openings).

## 8. Light client implications

Conditional on model C.

Today the light client walks the L1 attestation chain, fetches each
L2 block + witness, and sparsely updates its own SMT path against
the L1-attested `state_root`. Under model C this changes minimally:

- **State commitment** changes from `(accounts_root, nullifiers_hash,
  retarget_blob)` to `(accounts_root, retarget_blob)`. Nullifier set
  goes away — replays are caught by the nonce.
- **Per-block witness** must include enough data to recompute the
  new `accounts_root`. The current scheme — touched-set inclusion
  proofs + post-state — generalises directly. Witness size grows
  modestly (account leaf is bigger by ~33B for the commitment, and
  per-tx range proofs land in the block body, not the witness).
- **Light client verification** adds range proof + excess sig
  verification per `TransferCT`. CPU cost: ~5 ms per BP+ range
  proof on modern hardware; manageable for a POC.
- **Sparse forward walk** continues to work. The wallet still only
  needs *its own* leaf and Merkle path; updating it requires
  knowing the chain-side homomorphic addition that occurred at its
  leaf, which the witness must include.

Compared to model A or B, this is much simpler — no UTXO inclusion
proofs, no nullifier non-inclusion proofs. Confirms the per-line
intuition that model C is the smallest change *if §5 is resolved*.

## 9. Open questions / decisions to make

1. **Receive-decodability mitigation in model C (§5).** Highest
   priority. Requires reading the Bulletproofs+ construction in
   `secp256k1-zkp` and confirming whether public verifiability of
   "rangeproof rewinds for blinding pubkey B" is achievable.
   Without a clean answer, model C is unsafe.
2. **Same-block receive + send ordering.** If Alice sends to Bob
   in tx_i and Bob sends in tx_j in the same block, Bob's tx_j was
   built against Bob's pre-tx_i state. The chain applies tx_i
   first, then tries tx_j with a stale `C_Bob_old`. The excess sig
   verification fails (residual blinding doesn't match). Options:
   sequencer reorders to put receives before sends in the same
   block (best-effort, doesn't fully solve); or sender provides an
   explicit "expected `C_old`" and the chain rejects on mismatch
   so Bob's wallet retries. **Default: explicit expected-C_old**,
   nonce-bumped on retry.
3. **Range proof aggregation per block.** Saves bandwidth at some
   verifier complexity. Default yes if `secp256k1-zkp` exposes
   aggregation cleanly.
4. **Drop transparent transfers entirely once CT lands?** Default
   yes after a one-release deprecation window. Simpler chain.
5. **Hard fork vs. soft migration.** Default hard fork — wipe
   regtest, redeploy.
6. **View-key server.** Should a node offer a `/scan_for/:viewkey`
   endpoint to spare wallets a per-block scan? Convenience vs.
   privacy. Default: no server-side index; wallets scan locally.
7. **Mint anonymity / stealth addresses.** Both deferred; both
   compose cleanly with CT later.

## 10. Phased implementation plan (assuming model C is sound)

If model C survives §5, the plan is:

- **Phase 0 — Resolve §5.** Read Bulletproofs+ rangeproof
  construction in `secp256k1-zkp`. Confirm whether
  rewindability-binding to `B` is publicly verifiable. If yes, go
  to Phase 1. If no, **switch to model B (one-shot accounts);
  rewrite this doc accordingly.** Timeline: a few days of careful
  reading + small Rust prototype.
- **Phase 1 — Cryptographic prototype.** Pure-Rust crate exercising
  Pedersen + BP+ + ECDH + rangeproof rewind on a contrived
  one-sender-one-receiver scenario. No L2 integration. (~1 week)
- **Phase 2 — Core data model.** Add `TransferCT` to `hodl-core`;
  update `Account` leaf shape; add range proof + excess sig types.
  Maintain transparent `Transfer` alongside during migration.
  (~3–5 days)
- **Phase 3 — Sequencer.** Apply `TransferCT` in producer; verify
  range proofs + excess sigs + balance arithmetic. Update
  `state_root`. (~1 week)
- **Phase 4 — Wallet.** BIP32 `blind` branch; per-block scan;
  `(v_total, r_total)` bookkeeping; CT transfer construction.
  (~1.5 weeks)
- **Phase 5 — Light client.** Witness format update; per-block
  homomorphic-add verification at own leaf. (~1 week)
- **Phase 6 — GUI integration + transparent-transfer deprecation.**
  (~3 days)
- **Phase 7 — Migration.** Hard-fork regtest. (~1 day)

Total: ~4 weeks of focused work after §5 is resolved. Calendar
time: a quarter, given parallel commitments.

## 11. Fallback: model B if §5 doesn't work out

If we can't make model C safe, switch to model B (one-shot
accounts):

- Each receive creates a new account leaf, identified by
  `H(txid || output_index || recipient_pubkey)` or similar.
- Spending marks the account spent (nonce = 1 → unusable).
- The SMT-keyed-by-address machinery stays; only the leaf identity
  scheme changes from "user pubkey" to "per-output hash".
- A nullifier set is unnecessary — the SMT itself records which
  account-leaves are spent.

This is functionally UTXO (per-output identity, no aggregation,
nullifier semantics via leaf state) but cosmetically still
"accounts". The wallet has to track many leaves instead of one,
and balance is the sum across owned unspent leaves. Bricking is
limited to one leaf at a time.

Skip a fresh plan document for model B until / unless we need it —
the engineering is well-understood (Liquid implements roughly
this), and the differences from model C are mechanical.

## 12. Out of scope (for this plan)

- **Mint anonymity** (which L1 UTXO funded which L2 mint) — the
  ring-proof / aut-ct future item referenced in the README's
  "Trust posture" section. Separate workstream.
- **Stealth addresses.** Compose cleanly later.
- **Confidential assets (multi-asset).** Hodlchain has one asset.
- **Pegged-out withdrawals.** Reclaim is L1 and necessarily public.
- **SNARK shielded pool.** Future work; CT is necessary
  scaffolding either way.

## 13. References

- **Liquid Network whitepaper** (Poelstra et al., 2017): the
  reference design for Bitcoin-anchored CT.
- **Bulletproofs** (Bünz et al., 2018) and **Bulletproofs+** (Chung
  et al., 2020): range proof constructions.
- **Mimblewimble** (Tom Elvis Jedusor, 2016) and Grin: pure-CT
  chain design; the excess-signature / Pedersen-balance accounting
  pattern comes from here.
- **Elements Core** — the reference C++ implementation that proves
  CT works in production: <https://github.com/ElementsProject/elements>
- **`rust-elements`** — the Rust binding for Elements / Liquid
  transactions, including confidential address handling and
  `TxOut` blinding helpers:
  <https://github.com/ElementsProject/rust-elements>
- **`secp256k1-zkp`** (the crate) and **`libsecp256k1-zkp`** (the C
  library underneath) for the cryptographic primitives:
  <https://github.com/BlockstreamResearch/secp256k1-zkp> ,
  <https://github.com/rust-bitcoin/rust-secp256k1-zkp>

## 14. Pre-Phase-0 checklist

Before any implementation work starts:

1. **Read `secp256k1-zkp`'s Bulletproofs+ rangeproof code** and
   confirm or deny that rewindability under a specified `B` is
   publicly verifiable.
2. **Build the §10 Phase 1 prototype** in a throwaway crate to
   exercise the primitives end-to-end. Even if §5 forces model B,
   most of this prototype is reusable.
3. **Cross-check against Elements Core / rust-elements** —
   confirm our scheme doesn't deviate from their well-tested
   design in any way we haven't justified.
4. **Decide model A / B / C explicitly** in a follow-up to this
   doc and update §6–§10 to match. Today's recommendation is **C
   pending §5**, with **B as the documented fallback**.
