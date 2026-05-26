//! Direct-verification light client (sparse, incremental).
//!
//! The wallet persists a `VerifiedHead`: its own SMT leaf, its 256-deep
//! SMT path, the cumulative consumed-nullifier set, retargeting state,
//! and the L1-anchor outpoint that the next attestation tx will spend.
//! From that head, `walk_forward` extends the verified chain to the
//! current L1 tip:
//!
//!   1. Walk the L1 attestation chain via Esplora from the persisted
//!      anchor outpoint to the current L1 tip.
//!   2. For each new attestation: fetch the L2 block body + the
//!      sequencer/node's BlockWitness; verify pre-state proofs against
//!      the persisted prior accounts_root; re-verify every transfer
//!      signature and mint witness; run smt::apply_updates to
//!      sparsely compute the new accounts_root; recompute the full
//!      state_root and check it matches the L1-attested value.
//!
//! On cold-start (no persisted head), `bootstrap` uses option 1 from
//! the design discussion: walk the L1 chain to derive the
//! state_root, query the node for an inclusion proof + cumulative
//! nullifier set, verify the inclusion proof, and persist as the
//! initial head. This single step *trusts* that the node-served
//! prior state is correct; every subsequent block is verified
//! statelessly.
//!
//! See `docs/zk-design-discussion.md` for the trust-model rationale
//! and the path to ZK validity proofs once throughput justifies them.

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::secp256k1::{Secp256k1, XOnlyPublicKey};
use bitcoin::{OutPoint, ScriptBuf};
use hodl_core::block::L2Block;
use hodl_core::op_return::Attestation;
use hodl_core::proof::{verify_mint_entry, L1Output, L1View, MintProofEnvelope};
use hodl_core::smt::{self, LeafKind};
use hodl_core::state::{nullifiers_hash_of, Account, LedgerState, StateComponents};
use hodl_core::tx::{L2Address, L2Tx};
use hodl_core::witness::touched_addresses;
use std::collections::{BTreeSet, HashMap};

use crate::api::ApiClient;
use crate::esplora::{walk_attestation_chain, EsploraClient};
use crate::wallet::VerifiedHead;

/// A wallet-touching event observed while replaying an L2 block.
///
/// Emitted by `walk_forward`; consumed by `ops::light_balance` to
/// update the transaction-history side-cache on the wallet file.
/// `walk_forward` itself stays purely concerned with state-root
/// verification — no wallet-file mutation happens here.
#[derive(Clone, Debug)]
pub struct TxEvent {
    pub l2_height: u32,
    pub l1_height: u32,
    /// L2 block header timestamp (unix-seconds). Used as `created_ts`
    /// for backfilled records so cold-start restores produce a
    /// realistic-looking history rather than a stack at "now".
    pub block_ts: u64,
    pub kind: ObservedTxKind,
}

#[derive(Clone, Debug)]
pub enum ObservedTxKind {
    /// A transfer landed with `to == own_address` (inbound).
    TransferIn {
        from: L2Address,
        amount: u64,
        body_sighash_hex: String,
    },
    /// A transfer landed with `from == own_address` (outbound). Used
    /// to flip a previously-submitted Soft record to InBlock, or to
    /// reconstruct the record on a cold-start where no Soft record
    /// existed.
    TransferOut {
        to: L2Address,
        amount: u64,
        body_sighash_hex: String,
    },
    /// A mint event credited our address. Could be one we initiated
    /// (match by nullifier_hex against any pending Soft L2MintApply
    /// and flip to InBlock) or one we received from elsewhere
    /// (no matching record → create a new L2MintApply).
    MintIn {
        amount: u64,
        nullifier_hex: String,
    },
}

/// L1View backed by a pre-fetched map of outpoint → L1Output.
struct BatchL1View {
    outputs: HashMap<OutPoint, L1Output>,
    tip: u32,
}

impl L1View for BatchL1View {
    fn get_output(&self, op: &OutPoint) -> Option<L1Output> {
        self.outputs.get(op).cloned()
    }
    fn tip_height(&self) -> u32 {
        self.tip
    }
}

/// Cold-start: build a VerifiedHead from the L1 chain + the node's
/// state snapshot. Trusts the node for the initial state; every
/// subsequent block is verified statelessly.
pub async fn bootstrap(
    api: &ApiClient,
    esplora: &EsploraClient,
    own_address: L2Address,
) -> Result<VerifiedHead> {
    // 1. Walk the L1 attestation chain. Determines L1-attested state_root,
    //    block_hash, L1 height, and the next-anchor outpoint.
    let genesis = api
        .get_block(0)
        .await
        .context("fetch L2 genesis (block 0) for anchor_0")?;
    let anchor_0 = genesis
        .header
        .anchor_outpoint
        .ok_or_else(|| anyhow!("L2 genesis header has no anchor_outpoint"))?;

    let chain = walk_attestation_chain(esplora, anchor_0).await?;

    let (l1_state_root, l2_height, block_hash, l1_height, anchor_outpoint) = if chain.is_empty() {
        // Head is genesis. There are no attestation txs yet.
        (
            genesis.header.state_root,
            0u32,
            genesis.hash(),
            genesis.header.l1_height,
            anchor_0,
        )
    } else {
        let last = chain.last().unwrap();
        (
            last.attestation.state_root,
            last.attestation.height,
            last.attestation.l2_block_hash,
            last.l1_height,
            last.new_anchor,
        )
    };

    // 2. Fetch the node's view of `own_address` at its current head.
    let bal = api.balance(&own_address).await?;
    if bal.state_root != l1_state_root {
        bail!(
            "node state_root {} disagrees with L1-attested state_root {}; \
             the node may be behind. Try again in a moment.",
            bal.state_root,
            l1_state_root
        );
    }
    if bal.l2_height != l2_height {
        bail!(
            "node l2_height {} disagrees with L1-attested l2_height {}",
            bal.l2_height,
            l2_height
        );
    }
    if bal.address != own_address {
        bail!("balance response is for the wrong address");
    }
    let components_root = bal.state_components.state_root();
    if components_root != bal.state_root {
        bail!(
            "node-returned state_components do not hash to its reported state_root \
             ({} != {})",
            components_root,
            bal.state_root
        );
    }
    if !bal.proof.verify(bal.state_components.accounts_root) {
        bail!("node inclusion proof does not verify against accounts_root");
    }
    // Sanity-check the leaf payload agrees with reported balance/nonce.
    match &bal.proof.leaf {
        LeafKind::Account { balance, nonce } => {
            if *balance != bal.balance || *nonce != bal.nonce {
                bail!(
                    "inclusion-proof leaf disagrees with reported balance/nonce \
                     ({}, {}) vs ({}, {})",
                    balance,
                    nonce,
                    bal.balance,
                    bal.nonce
                );
            }
        }
        LeafKind::Empty => {
            if bal.balance != 0 || bal.nonce != 0 {
                bail!("empty leaf but reported balance/nonce are non-zero");
            }
        }
    }

    // 3. Fetch and sanity-check the cumulative nullifier set.
    let nullifiers_vec = api.get_nullifiers().await?;
    let consumed_nullifiers: BTreeSet<String> = nullifiers_vec.into_iter().collect();
    // Hash these the same way LedgerState does and check against the
    // node's reported nullifiers_hash, so a node lying about either is
    // caught here.
    let derived_nf_hash = nullifiers_hash_of(&consumed_nullifiers);
    if derived_nf_hash != bal.state_components.nullifiers_hash {
        bail!(
            "cumulative nullifier set hashes to {} but state_components.nullifiers_hash \
             is {} — node is inconsistent",
            derived_nf_hash,
            bal.state_components.nullifiers_hash
        );
    }

    Ok(VerifiedHead {
        state_root: bal.state_root,
        accounts_root: bal.state_components.accounts_root,
        l2_height,
        block_hash,
        l1_height,
        anchor_outpoint,
        own_address,
        own_leaf: bal.proof.leaf,
        own_path: bal.proof.siblings,
        consumed_nullifiers,
        total_minted_atoms: bal.total_minted_atoms,
        sequencer_fee_address: bal.state_components.sequencer_fee_address,
    })
}

/// Walk forward from the head to the current L1 tip. Returns the
/// number of L2 blocks newly verified and the updated head.
///
/// Each L1 attestation now commits to a *batch* of L2 blocks (the
/// L2 head at the time the attestation was posted). For each
/// attestation step we walk every L2 block in the range from
/// `head.l2_height + 1` to `step.attestation.height`, verifying
/// each block's L2-side state transition. The final block in the
/// range is additionally checked against the L1 attestation's
/// `(l2_block_hash, state_root)`.
pub async fn walk_forward(
    head: VerifiedHead,
    api: &ApiClient,
    esplora: &EsploraClient,
) -> Result<(VerifiedHead, usize, Vec<TxEvent>)> {
    let chain = walk_attestation_chain(esplora, head.anchor_outpoint).await?;
    if chain.is_empty() {
        return Ok((head, 0, Vec::new()));
    }
    let tip = esplora
        .tip_height()
        .await
        .context("query L1 tip height")?;
    let secp = Secp256k1::verification_only();
    let mut head = head;
    let mut count = 0;
    let mut events: Vec<TxEvent> = Vec::new();
    for step in &chain {
        let target = step.attestation.height;
        if target <= head.l2_height {
            // This attestation refers to a head we've already
            // verified (or earlier). Advance the anchor we watch
            // from next time and continue.
            head.anchor_outpoint = step.new_anchor;
            continue;
        }
        let start = head.l2_height + 1;
        for h in start..=target {
            // Intermediate blocks get full L2-side verification but
            // are not L1-pinned. Only the final block (h == target)
            // is checked against the attestation's hash + root.
            let end_att = if h == target { Some(&step.attestation) } else { None };
            head =
                verify_one_l2_block(head, h, end_att, tip, api, esplora, &secp, &mut events).await?;
            count += 1;
        }
        // Advance the anchor outpoint to this attestation's spend.
        head.anchor_outpoint = step.new_anchor;
        head.l1_height = step.l1_height;
    }
    Ok((head, count, events))
}

/// Verify one L2 block against the current verified head.
///
/// `end_attestation` is `Some` only for the final L2 block in an
/// L1-attested range, and triggers two additional checks beyond
/// the per-block L2-side verification: the computed block hash and
/// the block's claimed state_root must match the L1 attestation.
/// For intermediate blocks (where the wallet still trusts the L2
/// chain to advance honestly to the eventual L1-attested head),
/// only L2-side checks run.
async fn verify_one_l2_block<C: bitcoin::secp256k1::Verification>(
    head: VerifiedHead,
    height: u32,
    end_attestation: Option<&Attestation>,
    l1_tip: u32,
    api: &ApiClient,
    esplora: &EsploraClient,
    secp: &Secp256k1<C>,
    events: &mut Vec<TxEvent>,
) -> Result<VerifiedHead> {
    let block = api
        .get_block(height)
        .await
        .with_context(|| format!("fetch L2 block at height {height}"))?;
    let witness = api
        .get_witness(height)
        .await
        .with_context(|| format!("fetch witness at height {height}"))?;

    // ---- 1. Structural cross-checks (body ↔ witness; attestation
    //         only for the end-of-range block) ----
    if block.header.height != height {
        bail!(
            "block.header.height {} != requested height {height}",
            block.header.height
        );
    }
    let computed_block_hash = block.hash();
    if let Some(att) = end_attestation {
        if computed_block_hash != att.l2_block_hash {
            bail!(
                "end-of-range block hash {} != attestation hash {}",
                computed_block_hash,
                att.l2_block_hash
            );
        }
        if block.header.state_root != att.state_root {
            bail!(
                "end-of-range block.header.state_root {} != attestation state_root {}",
                block.header.state_root,
                att.state_root
            );
        }
    }
    let computed_txs_root = L2Block::compute_txs_root(&block.txs);
    if computed_txs_root != block.header.txs_root {
        bail!(
            "txs_root {} != block.header.txs_root {} (block {height})",
            computed_txs_root,
            block.header.txs_root
        );
    }
    if block.header.prev_hash != head.block_hash {
        bail!(
            "prev_hash mismatch at L2 height {height}: \
             block.prev_hash={} != head.block_hash={}",
            block.header.prev_hash,
            head.block_hash
        );
    }
    if witness.height != height {
        bail!(
            "witness.height {} != block height {height}",
            witness.height
        );
    }
    if witness.prior_accounts_root != head.accounts_root {
        bail!(
            "witness.prior_accounts_root {} != head.accounts_root {}",
            witness.prior_accounts_root,
            head.accounts_root
        );
    }
    // Cross-check witness's touched set matches the block's. Catches a
    // node that under-reports (or over-reports) touched accounts.
    let expected: Vec<L2Address> = touched_addresses(&block.txs, head.sequencer_fee_address);
    let mut got: Vec<L2Address> = witness.pre_proofs.iter().map(|p| p.address).collect();
    got.sort();
    if expected != got {
        bail!(
            "witness touched-set ({}) disagrees with block touched-set ({})",
            got.len(),
            expected.len()
        );
    }

    // ---- 2. Mint witnesses (against L1 via Esplora) ----
    let l1_view = prefetch_l1_for_block(esplora, &block, l1_tip).await?;
    for (i, tx) in block.txs.iter().enumerate() {
        if let L2Tx::Mint(entry) = tx {
            verify_mint_entry(entry, secp, &l1_view).map_err(|e| {
                anyhow!("mint entry #{i} in block {height} invalid: {e}")
            })?;
        }
    }

    // ---- 3. Replay txs in a sparse LedgerState ----
    let mut sparse_ls = LedgerState::new();
    for p in &witness.pre_proofs {
        if let LeafKind::Account { balance, nonce } = &p.leaf {
            sparse_ls
                .accounts
                .insert(p.address, Account { balance: *balance, nonce: *nonce });
        }
    }
    sparse_ls.consumed_nullifiers = head.consumed_nullifiers.clone();
    sparse_ls.total_minted_atoms = head.total_minted_atoms;
    sparse_ls.sequencer_fee_address = head.sequencer_fee_address;

    for (i, tx) in block.txs.iter().enumerate() {
        sparse_ls.apply(secp, tx).map_err(|e| {
            anyhow!("tx #{i} in block {height} failed apply on sparse state: {e}")
        })?;
    }

    // ---- 3b. Emit wallet-touching events for the tx-history side cache ----
    //
    // This is a pure projection — every event is something `apply`
    // already validated, so we just inspect tx-by-tx and push records
    // when own_address is the from/to/destination. Cheap, no extra
    // verification work.
    for tx in &block.txs {
        match tx {
            L2Tx::Mint(entry) => {
                if entry.event.l2_destination == head.own_address {
                    events.push(TxEvent {
                        l2_height: height,
                        l1_height: block.header.l1_height,
                        block_ts: block.header.timestamp,
                        kind: ObservedTxKind::MintIn {
                            amount: entry.event.amount,
                            nullifier_hex: entry.event.nullifier_hex.clone(),
                        },
                    });
                }
            }
            L2Tx::Transfer(t) => {
                let sighash_hex = hex::encode(t.body.sighash().0);
                if t.body.to == head.own_address {
                    events.push(TxEvent {
                        l2_height: height,
                        l1_height: block.header.l1_height,
                        block_ts: block.header.timestamp,
                        kind: ObservedTxKind::TransferIn {
                            from: t.body.from,
                            amount: t.body.amount,
                            body_sighash_hex: sighash_hex.clone(),
                        },
                    });
                }
                if t.body.from == head.own_address {
                    events.push(TxEvent {
                        l2_height: height,
                        l1_height: block.header.l1_height,
                        block_ts: block.header.timestamp,
                        kind: ObservedTxKind::TransferOut {
                            to: t.body.to,
                            amount: t.body.amount,
                            body_sighash_hex: sighash_hex,
                        },
                    });
                }
            }
        }
    }

    // ---- 4. Sparse SMT update for the new accounts_root ----
    let updates: Vec<smt::Update> = witness
        .pre_proofs
        .iter()
        .map(|p| {
            let post_state = match sparse_ls.accounts.get(&p.address) {
                Some(acct) => LeafKind::Account {
                    balance: acct.balance,
                    nonce: acct.nonce,
                },
                None => LeafKind::Empty,
            };
            smt::Update {
                pre_proof: p.clone(),
                post_state,
            }
        })
        .collect();

    let new_accounts_root = if updates.is_empty() {
        // No touched accounts → accounts_root unchanged. (Empty
        // blocks are vanishingly rare today but the producer can in
        // principle emit them.)
        head.accounts_root
    } else {
        smt::apply_updates(&updates, head.accounts_root)
            .map_err(|e| anyhow!("sparse update failed at block {height}: {e:?}"))?
    };

    // ---- 5. Recompute full state_root ----
    let new_components = StateComponents {
        accounts_root: new_accounts_root,
        nullifiers_hash: sparse_ls.nullifiers_hash(),
        sequencer_fee_address: sparse_ls.sequencer_fee_address,
    };
    let new_state_root = new_components.state_root();
    if new_state_root != block.header.state_root {
        bail!(
            "computed state_root {} != block.header.state_root {} at L2 height {height}",
            new_state_root,
            block.header.state_root
        );
    }

    // ---- 6. Refresh wallet's own (leaf, path) at the new accounts_root ----
    let observer_pre = hodl_core::smt::InclusionProof {
        address: head.own_address,
        leaf: head.own_leaf.clone(),
        siblings: head.own_path.clone(),
    };
    let observer_post = if updates.is_empty() {
        observer_pre
    } else {
        smt::refresh_observer(&observer_pre, &updates, head.accounts_root)
            .map_err(|e| anyhow!("observer refresh failed at block {height}: {e:?}"))?
    };
    if !observer_post.verify(new_accounts_root) {
        bail!("refreshed observer proof does not verify against new accounts_root");
    }

    Ok(VerifiedHead {
        state_root: new_state_root,
        accounts_root: new_accounts_root,
        l2_height: height,
        block_hash: computed_block_hash,
        // L1 anchor / height are updated by walk_forward at the end
        // of each attestation step (not per L2 block, since many
        // L2 blocks share the same L1 view).
        l1_height: block.header.l1_height,
        anchor_outpoint: head.anchor_outpoint,
        own_address: head.own_address,
        own_leaf: observer_post.leaf,
        own_path: observer_post.siblings,
        consumed_nullifiers: sparse_ls.consumed_nullifiers,
        total_minted_atoms: sparse_ls.total_minted_atoms,
        sequencer_fee_address: sparse_ls.sequencer_fee_address,
    })
}

/// Pre-fetch every L1 outpoint referenced by mints in `block`. The
/// resulting `BatchL1View` is a sync `L1View` that `verify_mint_entry`
/// can use without needing async access.
async fn prefetch_l1_for_block(
    esplora: &EsploraClient,
    block: &L2Block,
    tip: u32,
) -> Result<BatchL1View> {
    let mut outputs: HashMap<OutPoint, L1Output> = HashMap::new();
    for (i, tx) in block.txs.iter().enumerate() {
        let L2Tx::Mint(entry) = tx else { continue };
        let MintProofEnvelope::V0Outpoint(proof) = &entry.witness;
        let op = proof.outpoint;
        if outputs.contains_key(&op) {
            continue;
        }
        let info = esplora.get_tx(&op.txid).await.with_context(|| {
            format!(
                "fetch L1 tx {} for mint #{i} in block {}",
                op.txid, block.header.height
            )
        })?;
        let vout = info.vout.get(op.vout as usize).ok_or_else(|| {
            anyhow!(
                "vout {} not present in L1 tx {} (only {} outputs)",
                op.vout,
                op.txid,
                info.vout.len()
            )
        })?;
        let confirmed_height = info.status.block_height.ok_or_else(|| {
            anyhow!(
                "L1 tx {} is unconfirmed; mint witness cannot be verified",
                op.txid
            )
        })?;
        let confirmations = tip.saturating_sub(confirmed_height).saturating_add(1);
        let spk_bytes = hex::decode(&vout.scriptpubkey)
            .with_context(|| format!("decode scriptpubkey of {}", op.txid))?;
        outputs.insert(
            op,
            L1Output {
                value_sat: vout.value,
                script_pubkey: ScriptBuf::from_bytes(spk_bytes),
                confirmed_height,
                confirmations,
            },
        );
    }
    Ok(BatchL1View { outputs, tip })
}

/// Re-derive own balance/nonce from a `VerifiedHead`.
pub fn balance_from(head: &VerifiedHead, addr: &XOnlyPublicKey) -> (u64, u64) {
    if addr == &head.own_address {
        match &head.own_leaf {
            LeafKind::Account { balance, nonce } => (*balance, *nonce),
            LeafKind::Empty => (0, 0),
        }
    } else {
        // Different address: can't read from our sparse state. Caller
        // should query the node and verify the inclusion proof against
        // head.accounts_root.
        (0, 0)
    }
}
