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
}
