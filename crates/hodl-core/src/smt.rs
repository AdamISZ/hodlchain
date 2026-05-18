//! Sparse Merkle Tree over 256-bit L2Address keys, leaves over
//! `(L2Address, Account)`. The tree commits to the L2 account set in a
//! way that supports `O(log N)` inclusion (and non-inclusion) proofs.
//!
//! Conceptually a binary tree of depth 256: every 32-byte address is a
//! root-to-leaf path (MSB first). Most of the tree is empty; we never
//! materialise it. Empty subtrees of every depth are precomputed at
//! load time and reused.
//!
//! Hash domain tags isolate the SMT's hashes from anything else in the
//! protocol that uses sha256.
//!
//!   leaf(addr, balance, nonce) := H("hodl-smt-leaf-v0" || addr || balance_be || nonce_be)
//!   empty_leaf                 := H("hodl-smt-empty-leaf-v0")
//!   branch(left, right)        := H("hodl-smt-branch-v0" || left || right)
//!
//! Proofs are leaf-to-root order (proof[0] = sibling at the deepest
//! level, proof[255] = sibling adjacent to the root). The leaf kind
//! tells the verifier which preimage to start from.

use crate::hash::H256;
use crate::state::Account;
use alloc::boxed::Box;
use alloc::vec::Vec;
use bitcoin::secp256k1::XOnlyPublicKey;
use once_cell::race::OnceBox;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const TREE_DEPTH: usize = 256;

/// 256 + 1 = 257 hashes: empty[d] is the hash of an empty subtree of
/// height `d`. empty[0] = empty leaf, empty[256] = root of an entirely
/// empty SMT.
fn empty_subtree_hashes() -> &'static [H256; TREE_DEPTH + 1] {
    static CACHE: OnceBox<[H256; TREE_DEPTH + 1]> = OnceBox::new();
    CACHE.get_or_init(|| {
        let mut h = [H256::ZERO; TREE_DEPTH + 1];
        h[0] = {
            let mut s = Sha256::new();
            s.update(b"hodl-smt-empty-leaf-v0");
            H256(s.finalize().into())
        };
        for d in 0..TREE_DEPTH {
            h[d + 1] = branch_hash(h[d], h[d]);
        }
        Box::new(h)
    })
}

pub fn empty_root() -> H256 {
    empty_subtree_hashes()[TREE_DEPTH]
}

fn branch_hash(left: H256, right: H256) -> H256 {
    let mut s = Sha256::new();
    s.update(b"hodl-smt-branch-v0");
    s.update(&left.0);
    s.update(&right.0);
    H256(s.finalize().into())
}

/// Hash of a populated leaf for `(addr, balance, nonce)`.
pub fn populated_leaf_hash(addr: &XOnlyPublicKey, balance: u64, nonce: u64) -> H256 {
    let mut s = Sha256::new();
    s.update(b"hodl-smt-leaf-v0");
    s.update(&addr.serialize());
    s.update(&balance.to_be_bytes());
    s.update(&nonce.to_be_bytes());
    H256(s.finalize().into())
}

/// Hash of an empty leaf (no account at this key).
pub fn empty_leaf_hash() -> H256 {
    empty_subtree_hashes()[0]
}

/// Bit at `depth` of an x-only pubkey, where depth=0 is the MSB of byte 0.
fn bit_at(addr: &XOnlyPublicKey, depth: usize) -> u8 {
    debug_assert!(depth < TREE_DEPTH);
    let bytes = addr.serialize();
    let byte = bytes[depth / 8];
    (byte >> (7 - (depth % 8))) & 1
}

/// Tells the verifier whether the leaf preimage at the proof's
/// terminal slot is a populated `(balance, nonce)` or "no such
/// account". Both are equally valid Merkle leaves; the difference is
/// only what they prove about the live state.
/// Tells the verifier whether the leaf preimage at the proof's terminal
/// slot is a populated `(balance, nonce)` or "no such account". Both
/// are equally valid Merkle leaves; only the meaning to the application
/// differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeafKind {
    /// An account exists at this address.
    Account { balance: u64, nonce: u64 },
    /// No account exists at this address.
    Empty,
}

impl LeafKind {
    pub fn leaf_hash(&self, addr: &XOnlyPublicKey) -> H256 {
        match self {
            LeafKind::Account { balance, nonce } => populated_leaf_hash(addr, *balance, *nonce),
            LeafKind::Empty => empty_leaf_hash(),
        }
    }
}

/// An inclusion proof (or non-inclusion proof if `leaf == Empty`).
/// Verifies against an `accounts_root` SMT hash.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct InclusionProof {
    /// L2 address (BIP340 x-only public key) the proof is for.
    /// Hex-encoded, 32 bytes.
    #[cfg_attr(
        feature = "std",
        schema(value_type = String, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
    pub address: XOnlyPublicKey,
    pub leaf: LeafKind,
    /// Sibling hashes, leaf-to-root order; always length 256.
    pub siblings: Vec<H256>,
}

impl InclusionProof {
    pub fn verify(&self, expected_root: H256) -> bool {
        if self.siblings.len() != TREE_DEPTH {
            return false;
        }
        let mut current = self.leaf.leaf_hash(&self.address);
        for d in (0..TREE_DEPTH).rev() {
            let sibling = self.siblings[TREE_DEPTH - 1 - d];
            current = if bit_at(&self.address, d) == 0 {
                branch_hash(current, sibling)
            } else {
                branch_hash(sibling, current)
            };
        }
        current == expected_root
    }
}

/// Compute the SMT root over a populated-accounts set.
pub fn compute_root(accounts: &[(XOnlyPublicKey, &Account)]) -> H256 {
    let populated: Vec<(XOnlyPublicKey, H256)> = accounts
        .iter()
        .map(|(a, acct)| (*a, populated_leaf_hash(a, acct.balance, acct.nonce)))
        .collect();
    let empty = empty_subtree_hashes();
    root_recursive(&populated, 0, empty)
}

fn root_recursive(
    populated: &[(XOnlyPublicKey, H256)],
    depth: usize,
    empty: &[H256; TREE_DEPTH + 1],
) -> H256 {
    if populated.is_empty() {
        return empty[TREE_DEPTH - depth];
    }
    if depth == TREE_DEPTH {
        // Single populated leaf at full depth.
        return populated[0].1;
    }
    let (left, right): (Vec<_>, Vec<_>) = populated
        .iter()
        .copied()
        .partition(|(a, _)| bit_at(a, depth) == 0);
    let lh = root_recursive(&left, depth + 1, empty);
    let rh = root_recursive(&right, depth + 1, empty);
    branch_hash(lh, rh)
}

/// Compute an inclusion (or non-inclusion) proof for `target`.
pub fn compute_proof(
    accounts: &[(XOnlyPublicKey, &Account)],
    target: XOnlyPublicKey,
) -> InclusionProof {
    let populated: Vec<(XOnlyPublicKey, H256)> = accounts
        .iter()
        .map(|(a, acct)| (*a, populated_leaf_hash(a, acct.balance, acct.nonce)))
        .collect();
    let leaf = accounts
        .iter()
        .find(|(a, _)| *a == target)
        .map(|(_, acct)| LeafKind::Account { balance: acct.balance, nonce: acct.nonce })
        .unwrap_or(LeafKind::Empty);
    let empty = empty_subtree_hashes();
    let mut siblings = Vec::with_capacity(TREE_DEPTH);
    proof_recursive(&populated, &target, 0, empty, &mut siblings);
    InclusionProof { address: target, leaf, siblings }
}

fn proof_recursive(
    populated: &[(XOnlyPublicKey, H256)],
    target: &XOnlyPublicKey,
    depth: usize,
    empty: &[H256; TREE_DEPTH + 1],
    out: &mut Vec<H256>,
) -> H256 {
    if depth == TREE_DEPTH {
        if populated.is_empty() {
            return empty[0];
        }
        return populated[0].1;
    }
    let (left, right): (Vec<_>, Vec<_>) = populated
        .iter()
        .copied()
        .partition(|(a, _)| bit_at(a, depth) == 0);
    let target_bit = bit_at(target, depth);
    let (this_branch, this_sibling_root) = if target_bit == 0 {
        let lh = proof_recursive(&left, target, depth + 1, empty, out);
        let rh = root_recursive(&right, depth + 1, empty);
        (branch_hash(lh, rh), rh)
    } else {
        let rh = proof_recursive(&right, target, depth + 1, empty, out);
        let lh = root_recursive(&left, depth + 1, empty);
        (branch_hash(lh, rh), lh)
    };
    out.push(this_sibling_root);
    this_branch
}

// ---------- Sparse multi-leaf update ----------

/// A single touched account in a block.
///
/// `pre_proof` is the inclusion (or non-inclusion) proof of `pre_state`
/// against the *prior* state_root. `post_state` is the account's value
/// after the block applies. Both states' leaf hashes are computed from
/// `pre_proof.address`.
#[derive(Clone, Debug)]
pub struct Update {
    pub pre_proof: InclusionProof,
    pub post_state: LeafKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SparseUpdateError {
    /// A pre-state proof failed verification against the prior root.
    InvalidPreProof { addr_hex: alloc::string::String },
    /// Two updates supplied for the same address.
    DuplicateAddress { addr_hex: alloc::string::String },
    /// Empty update set with a non-empty prior root. The caller should
    /// short-circuit when there are no touched accounts (new_root == prior_root).
    EmptyUpdates,
}

/// Given pre-state proofs and post-state values for every touched
/// account in a block, return the new SMT root.
///
/// Soundness: each `pre_proof` is verified against `prior_root`. If
/// any pre-state proof fails, the function returns an error. When all
/// pre-state proofs verify, the recursion uses their sibling hashes
/// as the prior hashes of the untouched subtrees; those subtrees are
/// unchanged at the new root by definition (no touched key lives
/// inside them), so the new root computed here is the unique root
/// consistent with `prior_root` after the listed updates.
pub fn apply_updates(updates: &[Update], prior_root: H256) -> Result<H256, SparseUpdateError> {
    if updates.is_empty() {
        return Err(SparseUpdateError::EmptyUpdates);
    }
    // Detect duplicate addresses up front. Allowing dupes would let an
    // attacker pass two contradictory pre-states for the same key.
    for (i, u) in updates.iter().enumerate() {
        for u2 in &updates[i + 1..] {
            if u.pre_proof.address == u2.pre_proof.address {
                return Err(SparseUpdateError::DuplicateAddress {
                    addr_hex: hex::encode(u.pre_proof.address.serialize()),
                });
            }
        }
    }
    for u in updates {
        if !u.pre_proof.verify(prior_root) {
            return Err(SparseUpdateError::InvalidPreProof {
                addr_hex: hex::encode(u.pre_proof.address.serialize()),
            });
        }
    }
    let refs: Vec<&Update> = updates.iter().collect();
    Ok(post_root_recursive(&refs, 0))
}

fn post_root_recursive(updates: &[&Update], depth: usize) -> H256 {
    if depth == TREE_DEPTH {
        debug_assert_eq!(updates.len(), 1);
        let u = updates[0];
        return u.post_state.leaf_hash(&u.pre_proof.address);
    }
    let (left, right): (Vec<&Update>, Vec<&Update>) = updates
        .iter()
        .copied()
        .partition(|u| bit_at(&u.pre_proof.address, depth) == 0);
    let lh = if !left.is_empty() {
        post_root_recursive(&left, depth + 1)
    } else {
        // No touched key in the left subtree → its hash at the new
        // root equals its hash at the prior root. Any right-going
        // proof's sibling-at-depth gives us that hash.
        right.first().unwrap().pre_proof.siblings[TREE_DEPTH - 1 - depth]
    };
    let rh = if !right.is_empty() {
        post_root_recursive(&right, depth + 1)
    } else {
        left.first().unwrap().pre_proof.siblings[TREE_DEPTH - 1 - depth]
    };
    branch_hash(lh, rh)
}

/// After a sparse update, recompute the inclusion proof for an
/// observer address `observer` whose pre-update inclusion proof was
/// `observer_pre`. Returns the observer's post-state and its new
/// sibling path.
///
/// The observer can be either inside or outside the touched set. If
/// inside, its post_state is read from the update list. If outside,
/// the post_state equals the pre_state (untouched accounts keep their
/// value). In both cases the sibling path is updated to reflect any
/// changes to subtrees on the observer's path.
pub fn refresh_observer(
    observer: &InclusionProof,
    updates: &[Update],
    prior_root: H256,
) -> Result<InclusionProof, SparseUpdateError> {
    if !observer.verify(prior_root) {
        return Err(SparseUpdateError::InvalidPreProof {
            addr_hex: hex::encode(observer.address.serialize()),
        });
    }
    if updates.is_empty() {
        return Ok(observer.clone());
    }
    // Validate updates against prior_root too.
    for (i, u) in updates.iter().enumerate() {
        for u2 in &updates[i + 1..] {
            if u.pre_proof.address == u2.pre_proof.address {
                return Err(SparseUpdateError::DuplicateAddress {
                    addr_hex: hex::encode(u.pre_proof.address.serialize()),
                });
            }
        }
    }
    for u in updates {
        if !u.pre_proof.verify(prior_root) {
            return Err(SparseUpdateError::InvalidPreProof {
                addr_hex: hex::encode(u.pre_proof.address.serialize()),
            });
        }
    }

    let post_state = updates
        .iter()
        .find(|u| u.pre_proof.address == observer.address)
        .map(|u| u.post_state.clone())
        .unwrap_or_else(|| observer.leaf.clone());

    // Compute the observer's new siblings: at each depth, the sibling
    // is either the new hash of a subtree that contains touched keys,
    // or unchanged (= the observer's old sibling).
    let mut new_siblings = observer.siblings.clone();
    let refs: Vec<&Update> = updates.iter().collect();
    for depth in 0..TREE_DEPTH {
        // The observer goes to (bit_at(observer, depth)) at this depth.
        // The sibling is the opposite side. We want the new hash of the
        // opposite side, but only if any touched key lives there.
        let obs_bit = bit_at(&observer.address, depth);
        // Filter updates to those that match the observer's path so
        // far (same first `depth` bits), then partition by bit at
        // `depth`. The "opposite side" is what we may need to recompute.
        let on_path: Vec<&Update> = refs
            .iter()
            .copied()
            .filter(|u| {
                (0..depth).all(|d| bit_at(&u.pre_proof.address, d) == bit_at(&observer.address, d))
            })
            .collect();
        if on_path.is_empty() {
            // No touched key shares the observer's prefix this far →
            // none of the subtrees on its path from here are modified.
            break;
        }
        let opposite_side: Vec<&Update> = on_path
            .iter()
            .copied()
            .filter(|u| bit_at(&u.pre_proof.address, depth) != obs_bit)
            .collect();
        if !opposite_side.is_empty() {
            // Recompute the opposite subtree at this depth.
            new_siblings[TREE_DEPTH - 1 - depth] = post_root_recursive(&opposite_side, depth + 1);
        }
        // If opposite_side is empty, the sibling is unchanged.
    }

    Ok(InclusionProof {
        address: observer.address,
        leaf: post_state,
        siblings: new_siblings,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::secp256k1::{Keypair, Secp256k1};

    fn rand_xonly() -> XOnlyPublicKey {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        kp.x_only_public_key().0
    }

    #[test]
    fn empty_tree_root_is_stable() {
        let r1 = compute_root(&[]);
        let r2 = compute_root(&[]);
        assert_eq!(r1, r2);
        assert_eq!(r1, empty_root());
    }

    #[test]
    fn single_account_inclusion_verifies() {
        let addr = rand_xonly();
        let acct = Account { balance: 1000, nonce: 5 };
        let accounts = vec![(addr, &acct)];
        let root = compute_root(&accounts);
        let proof = compute_proof(&accounts, addr);
        assert!(matches!(proof.leaf, LeafKind::Account { balance: 1000, nonce: 5 }));
        assert!(proof.verify(root));
        assert_eq!(proof.siblings.len(), TREE_DEPTH);
    }

    #[test]
    fn non_existent_address_proves_empty() {
        let addr_a = rand_xonly();
        let acct_a = Account { balance: 7, nonce: 0 };
        let other = rand_xonly();
        let accounts = vec![(addr_a, &acct_a)];
        let root = compute_root(&accounts);
        let proof = compute_proof(&accounts, other);
        assert_eq!(proof.leaf, LeafKind::Empty);
        assert!(proof.verify(root));
    }

    #[test]
    fn different_balances_change_root() {
        let addr = rand_xonly();
        let acct1 = Account { balance: 1, nonce: 0 };
        let acct2 = Account { balance: 2, nonce: 0 };
        let r1 = compute_root(&[(addr, &acct1)]);
        let r2 = compute_root(&[(addr, &acct2)]);
        assert_ne!(r1, r2);
    }

    #[test]
    fn different_addresses_change_root() {
        let acct = Account { balance: 1000, nonce: 0 };
        let r1 = compute_root(&[(rand_xonly(), &acct)]);
        let r2 = compute_root(&[(rand_xonly(), &acct)]);
        // Two random addresses, same account body — different keys, different roots.
        assert_ne!(r1, r2);
    }

    #[test]
    fn corrupted_proof_fails_verify() {
        let addr = rand_xonly();
        let acct = Account { balance: 1000, nonce: 0 };
        let root = compute_root(&[(addr, &acct)]);
        let mut proof = compute_proof(&[(addr, &acct)], addr);
        // Flip a bit in one sibling.
        proof.siblings[7].0[0] ^= 0x80;
        assert!(!proof.verify(root));
    }

    #[test]
    fn many_accounts_root_independent_of_input_order() {
        let mut accts: Vec<(XOnlyPublicKey, Account)> = (0..16)
            .map(|i| (rand_xonly(), Account { balance: i * 100, nonce: i }))
            .collect();
        let refs1: Vec<(XOnlyPublicKey, &Account)> =
            accts.iter().map(|(a, c)| (*a, c)).collect();
        let r1 = compute_root(&refs1);
        // Reverse and recompute.
        accts.reverse();
        let refs2: Vec<(XOnlyPublicKey, &Account)> =
            accts.iter().map(|(a, c)| (*a, c)).collect();
        let r2 = compute_root(&refs2);
        assert_eq!(r1, r2);
    }

    // ---------- sparse-update tests ----------
    //
    // Strategy: build a "prior" accounts table, compute its root and
    // inclusion proofs for some subset; apply changes to the same
    // table to produce a "post" accounts table with its own root.
    // Then run apply_updates with the prior proofs + post states and
    // assert it produces the post root. This treats compute_root as
    // the ground truth.

    fn make_account(balance: u64, nonce: u64) -> Account {
        Account { balance, nonce }
    }

    fn build_table(n: usize) -> Vec<(XOnlyPublicKey, Account)> {
        (0..n)
            .map(|i| (rand_xonly(), make_account((i as u64 + 1) * 100, i as u64)))
            .collect()
    }

    fn refs_of<'a>(t: &'a [(XOnlyPublicKey, Account)]) -> Vec<(XOnlyPublicKey, &'a Account)> {
        t.iter().map(|(a, c)| (*a, c)).collect()
    }

    fn make_update(
        prior: &[(XOnlyPublicKey, Account)],
        addr: XOnlyPublicKey,
        post_state: LeafKind,
    ) -> Update {
        let prior_refs = refs_of(prior);
        Update {
            pre_proof: compute_proof(&prior_refs, addr),
            post_state,
        }
    }

    #[test]
    fn sparse_update_single_modification_matches_compute_root() {
        let mut table = build_table(8);
        let prior_root = compute_root(&refs_of(&table));
        let addr = table[3].0;

        let updates = vec![make_update(
            &table,
            addr,
            LeafKind::Account { balance: 999_999, nonce: 42 },
        )];

        // Apply same modification to the table.
        table[3].1 = make_account(999_999, 42);
        let expected_post_root = compute_root(&refs_of(&table));

        let got = apply_updates(&updates, prior_root).unwrap();
        assert_eq!(got, expected_post_root);
    }

    #[test]
    fn sparse_update_multiple_modifications_match_compute_root() {
        let mut table = build_table(10);
        let prior_root = compute_root(&refs_of(&table));

        let updates = vec![
            make_update(&table, table[1].0, LeafKind::Account { balance: 11, nonce: 2 }),
            make_update(&table, table[4].0, LeafKind::Account { balance: 99, nonce: 7 }),
            make_update(&table, table[7].0, LeafKind::Account { balance: 1, nonce: 1 }),
        ];

        table[1].1 = make_account(11, 2);
        table[4].1 = make_account(99, 7);
        table[7].1 = make_account(1, 1);
        let expected_post_root = compute_root(&refs_of(&table));

        let got = apply_updates(&updates, prior_root).unwrap();
        assert_eq!(got, expected_post_root);
    }

    #[test]
    fn sparse_update_new_account_via_non_inclusion_proof() {
        // Start with N accounts; "touched" key is fresh (not in the
        // table), pre-state proof is non-inclusion (Empty leaf).
        let mut table = build_table(8);
        let prior_root = compute_root(&refs_of(&table));
        let new_addr = rand_xonly();
        let new_acct = make_account(500_000, 0);

        let updates = vec![make_update(
            &table,
            new_addr,
            LeafKind::Account { balance: new_acct.balance, nonce: new_acct.nonce },
        )];

        table.push((new_addr, new_acct));
        let expected_post_root = compute_root(&refs_of(&table));

        let got = apply_updates(&updates, prior_root).unwrap();
        assert_eq!(got, expected_post_root);
    }

    #[test]
    fn sparse_update_drain_to_empty_via_post_state_empty() {
        // Drain an account by setting its post-state to Empty.
        let mut table = build_table(6);
        let prior_root = compute_root(&refs_of(&table));
        let addr = table[2].0;

        let updates = vec![make_update(&table, addr, LeafKind::Empty)];

        table.remove(2);
        let expected_post_root = compute_root(&refs_of(&table));

        let got = apply_updates(&updates, prior_root).unwrap();
        assert_eq!(got, expected_post_root);
    }

    #[test]
    fn sparse_update_rejects_bad_pre_proof() {
        let table = build_table(4);
        let prior_root = compute_root(&refs_of(&table));
        let addr = table[0].0;

        let mut bad_update = make_update(
            &table,
            addr,
            LeafKind::Account { balance: 1, nonce: 1 },
        );
        // Corrupt a sibling so the pre-proof no longer verifies.
        bad_update.pre_proof.siblings[10].0[0] ^= 0xFF;
        let err = apply_updates(&[bad_update], prior_root).unwrap_err();
        assert!(matches!(err, SparseUpdateError::InvalidPreProof { .. }));
    }

    #[test]
    fn sparse_update_rejects_duplicate_addresses() {
        let table = build_table(4);
        let prior_root = compute_root(&refs_of(&table));
        let addr = table[0].0;
        let u1 = make_update(&table, addr, LeafKind::Account { balance: 1, nonce: 1 });
        let u2 = make_update(&table, addr, LeafKind::Account { balance: 2, nonce: 2 });
        let err = apply_updates(&[u1, u2], prior_root).unwrap_err();
        assert!(matches!(err, SparseUpdateError::DuplicateAddress { .. }));
    }

    #[test]
    fn refresh_observer_inside_touched_set() {
        let mut table = build_table(8);
        let prior_root = compute_root(&refs_of(&table));
        let prior_refs = refs_of(&table);

        // Observer's account is among the touched.
        let observer_addr = table[3].0;
        let observer_proof = compute_proof(&prior_refs, observer_addr);

        let updates = vec![make_update(
            &table,
            observer_addr,
            LeafKind::Account { balance: 12_345, nonce: 9 },
        )];

        let refreshed = super::refresh_observer(&observer_proof, &updates, prior_root).unwrap();
        table[3].1 = make_account(12_345, 9);
        let post_root = compute_root(&refs_of(&table));
        assert!(refreshed.verify(post_root));
        assert!(matches!(
            refreshed.leaf,
            LeafKind::Account { balance: 12_345, nonce: 9 }
        ));
    }

    #[test]
    fn refresh_observer_outside_touched_set() {
        // Observer is unchanged, but its sibling path changes because
        // a different account gets updated.
        let mut table = build_table(8);
        let prior_root = compute_root(&refs_of(&table));
        let prior_refs = refs_of(&table);

        let observer_addr = table[2].0;
        let observer_proof = compute_proof(&prior_refs, observer_addr);

        // Touch some other account.
        let updates = vec![make_update(
            &table,
            table[5].0,
            LeafKind::Account { balance: 999, nonce: 99 },
        )];

        let refreshed = super::refresh_observer(&observer_proof, &updates, prior_root).unwrap();
        table[5].1 = make_account(999, 99);
        let post_root = compute_root(&refs_of(&table));
        assert!(refreshed.verify(post_root));
        // Observer's leaf is unchanged from pre-state.
        assert_eq!(refreshed.leaf, observer_proof.leaf);
    }

    #[test]
    fn refresh_observer_for_brand_new_address() {
        // Observer's address doesn't exist in the prior state — its
        // proof is a non-inclusion proof. A different account gets
        // updated; observer's path should still refresh correctly.
        let mut table = build_table(8);
        let prior_root = compute_root(&refs_of(&table));
        let prior_refs = refs_of(&table);

        let observer_addr = rand_xonly();
        let observer_proof = compute_proof(&prior_refs, observer_addr);
        assert_eq!(observer_proof.leaf, LeafKind::Empty);

        let updates = vec![make_update(
            &table,
            table[3].0,
            LeafKind::Account { balance: 1_000_000, nonce: 1 },
        )];

        let refreshed = super::refresh_observer(&observer_proof, &updates, prior_root).unwrap();
        table[3].1 = make_account(1_000_000, 1);
        let post_root = compute_root(&refs_of(&table));
        assert!(refreshed.verify(post_root));
        assert_eq!(refreshed.leaf, LeafKind::Empty);
    }
}
