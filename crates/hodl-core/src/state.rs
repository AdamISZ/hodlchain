//! Account-based L2 state.
//!
//! Two maps:
//!
//!   * `accounts`         — L2 address → (balance, nonce)
//!   * `consumed_nullifiers` — set of nullifier bytes seen so far
//!
//! State root: sha256 over a canonical, deterministic encoding of both maps.
//! Real Merkle tree (so light clients can produce inclusion proofs) is left
//! for later; the on-chain commitment already exists.

use crate::consensus::{
    INITIAL_R, RETARGET_MAX_FACTOR, RETARGET_WINDOW_BLOCKS, TARGET_ATOMS_PER_BLOCK,
};
use crate::hash::H256;
use crate::smt;
use crate::tx::{Amount, L2Address, L2Tx, MintEntry, SignedTransfer};
use bitcoin::secp256k1::{Message, Secp256k1, Verification};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Account {
    pub balance: Amount,
    pub nonce: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerState {
    pub accounts: BTreeMap<L2Address, Account>,
    /// Nullifier bytes, hex-encoded for portability.
    pub consumed_nullifiers: BTreeSet<String>,
    /// Current mint-function rate parameter. Adjusted at retarget windows.
    pub current_r: f64,
    /// Atoms minted in the currently-open retarget window, used to drive
    /// the next retarget.
    pub current_window_atoms: u64,
    /// First L2 block height of the currently-open retarget window.
    pub current_window_start_height: u32,
}

impl Default for LedgerState {
    fn default() -> Self {
        Self {
            accounts: BTreeMap::new(),
            consumed_nullifiers: BTreeSet::new(),
            current_r: INITIAL_R,
            current_window_atoms: 0,
            current_window_start_height: 1,
        }
    }
}

#[derive(Debug, Error)]
pub enum ApplyError {
    #[error("nullifier already consumed")]
    DoubleMint,
    #[error("transfer signature invalid")]
    BadSignature,
    #[error("nonce mismatch: expected {expected}, got {got}")]
    BadNonce { expected: u64, got: u64 },
    #[error("insufficient balance")]
    InsufficientBalance,
    #[error("self-transfer not allowed")]
    SelfTransfer,
    #[error("zero-amount tx")]
    ZeroAmount,
}

impl LedgerState {
    pub fn new() -> Self { Self::default() }

    pub fn balance_of(&self, addr: &L2Address) -> Amount {
        self.accounts.get(addr).map(|a| a.balance).unwrap_or(0)
    }

    pub fn nonce_of(&self, addr: &L2Address) -> u64 {
        self.accounts.get(addr).map(|a| a.nonce).unwrap_or(0)
    }

    /// Apply a single L2 transaction. Mutates self only on success.
    pub fn apply<C: Verification>(
        &mut self,
        secp: &Secp256k1<C>,
        tx: &L2Tx,
    ) -> Result<(), ApplyError> {
        match tx {
            L2Tx::Mint(entry) => self.apply_mint(entry),
            L2Tx::Transfer(t) => self.apply_transfer(secp, t),
        }
    }

    /// Apply the mint's *event* to ledger state. The witness is NOT
    /// re-verified here; callers that don't already trust the source
    /// of `entry` should run `hodl_core::proof::verify_mint_entry`
    /// before this. The node's follower does so; the sequencer's
    /// producer relies on submit-time verification.
    ///
    /// Side effect: the mint's amount is added to
    /// `current_window_atoms`, which feeds the next retarget.
    fn apply_mint(&mut self, entry: &MintEntry) -> Result<(), ApplyError> {
        let ev = &entry.event;
        if ev.amount == 0 { return Err(ApplyError::ZeroAmount); }
        if self.consumed_nullifiers.contains(&ev.nullifier_hex) {
            return Err(ApplyError::DoubleMint);
        }
        self.consumed_nullifiers.insert(ev.nullifier_hex.clone());
        let acct = self.accounts.entry(ev.l2_destination).or_default();
        acct.balance = acct.balance.saturating_add(ev.amount);
        self.current_window_atoms = self.current_window_atoms.saturating_add(ev.amount);
        Ok(())
    }

    /// To be called by producer / node AFTER all txs in the L2 block at
    /// `l2_height` have been applied. If `l2_height` is a window
    /// boundary, runs the retarget: clamps `actual / target` to
    /// `[1/MAX_FACTOR, MAX_FACTOR]`, divides `r` by that, resets the
    /// window counter.
    ///
    /// Direction: more issuance than target → ratio > 1 → r shrinks →
    /// future mints earn less per (V,T). Bitcoin-style sign convention.
    pub fn end_of_block(&mut self, l2_height: u32) {
        if l2_height == 0 { return; }
        if (l2_height as u64) % (RETARGET_WINDOW_BLOCKS as u64) != 0 { return; }

        let target_total = (TARGET_ATOMS_PER_BLOCK as f64) * (RETARGET_WINDOW_BLOCKS as f64);
        let actual_total = self.current_window_atoms as f64;
        let raw_ratio = actual_total / target_total;
        let clamped = raw_ratio
            .max(1.0 / RETARGET_MAX_FACTOR)
            .min(RETARGET_MAX_FACTOR);
        self.current_r /= clamped;
        self.current_window_atoms = 0;
        self.current_window_start_height = l2_height + 1;
    }

    fn apply_transfer<C: Verification>(
        &mut self,
        secp: &Secp256k1<C>,
        t: &SignedTransfer,
    ) -> Result<(), ApplyError> {
        if t.body.amount == 0 { return Err(ApplyError::ZeroAmount); }
        if t.body.from == t.body.to { return Err(ApplyError::SelfTransfer); }

        let expected_nonce = self.nonce_of(&t.body.from);
        if t.body.nonce != expected_nonce {
            return Err(ApplyError::BadNonce { expected: expected_nonce, got: t.body.nonce });
        }

        let digest = t.body.sighash().0;
        let msg = Message::from_digest(digest);
        secp.verify_schnorr(&t.signature, &msg, &t.body.from)
            .map_err(|_| ApplyError::BadSignature)?;

        let from_balance = self.balance_of(&t.body.from);
        if from_balance < t.body.amount { return Err(ApplyError::InsufficientBalance); }

        let from = self.accounts.entry(t.body.from).or_default();
        from.balance -= t.body.amount;
        from.nonce += 1;
        let to = self.accounts.entry(t.body.to).or_default();
        to.balance = to.balance.saturating_add(t.body.amount);
        Ok(())
    }

    /// Compute the SMT root over the accounts table. Light clients use
    /// this — combined with an `InclusionProof` for their account — to
    /// verify their balance against the on-chain `state_root`.
    pub fn accounts_root(&self) -> H256 {
        let accts: Vec<(L2Address, &Account)> =
            self.accounts.iter().map(|(a, c)| (*a, c)).collect();
        smt::compute_root(&accts)
    }

    /// Build an SMT inclusion (or non-inclusion, if absent) proof for
    /// `addr`. The proof verifies against `accounts_root()`.
    pub fn account_inclusion_proof(&self, addr: L2Address) -> smt::InclusionProof {
        let accts: Vec<(L2Address, &Account)> =
            self.accounts.iter().map(|(a, c)| (*a, c)).collect();
        smt::compute_proof(&accts, addr)
    }

    /// Hash of the consumed-nullifier set. Cheap sorted-list hash — no
    /// inclusion proofs needed for v0 (nullifier set is used to prevent
    /// double-mint at apply time; users don't query it).
    pub fn nullifiers_hash(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-nullifiers-v0");
        for n in &self.consumed_nullifiers {
            h.update(n.as_bytes());
        }
        H256(h.finalize().into())
    }

    /// Snapshot the inputs to `state_root`. A light client given a
    /// `StateComponents` can re-derive the L1-committed state_root and
    /// independently verify it.
    pub fn components(&self) -> StateComponents {
        StateComponents {
            accounts_root: self.accounts_root(),
            nullifiers_hash: self.nullifiers_hash(),
            current_r: self.current_r,
            current_window_atoms: self.current_window_atoms,
            current_window_start_height: self.current_window_start_height,
        }
    }

    /// Deterministic state root: hash over (accounts_root, nullifiers_hash,
    /// retarget_blob). Replaces the previous flat-serialisation hash;
    /// `accounts_root` is now Merkle-structured so light clients can
    /// produce inclusion proofs.
    pub fn state_root(&self) -> H256 {
        self.components().state_root()
    }
}

/// The inputs that `state_root` hashes together. Producer / node /
/// light-client all agree on this struct; making it explicit lets a
/// light client receiving (accounts_root, nullifiers_hash, retarget
/// scalars) recompute the `state_root` and compare against the value
/// it pulled off L1.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StateComponents {
    pub accounts_root: H256,
    pub nullifiers_hash: H256,
    pub current_r: f64,
    pub current_window_atoms: u64,
    pub current_window_start_height: u32,
}

impl StateComponents {
    /// Canonical state-root hash. Same byte layout as
    /// `LedgerState::state_root`.
    pub fn state_root(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-state-v1");
        h.update(&self.accounts_root.0);
        h.update(&self.nullifiers_hash.0);
        h.update(b"|retarget|");
        h.update(&self.current_r.to_le_bytes());
        h.update(&self.current_window_atoms.to_be_bytes());
        h.update(&self.current_window_start_height.to_be_bytes());
        H256(h.finalize().into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proof::{MintProofEnvelope, OutpointProof};
    use crate::tx::{MintEntry, MintEvent, SignedTransfer, TransferBody};
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::{schnorr, Keypair, Secp256k1};
    use bitcoin::{OutPoint, Txid};

    /// Build a stub MintEntry. The witness is structurally well-formed
    /// but its `verify()` would not succeed against a real L1 (no funding
    /// tx exists). `apply_mint` doesn't run the witness so this is fine
    /// for state-machine tests.
    fn stub_entry(
        nullifier_hex: &str,
        amount: Amount,
        dest: L2Address,
        signing_kp: &Keypair,
    ) -> MintEntry {
        let outpoint = OutPoint::new(Txid::all_zeros(), 0);
        let secp = Secp256k1::new();
        let (pk, _) = signing_kp.x_only_public_key();
        let sighash = OutpointProof::sighash(&outpoint, &dest);
        let msg = bitcoin::secp256k1::Message::from_digest(sighash);
        let sig: schnorr::Signature = secp.sign_schnorr(&msg, signing_kp);
        let witness = MintProofEnvelope::V0Outpoint(OutpointProof {
            outpoint,
            user_xonly_pubkey: pk,
            lock_blocks: 99,
            signature: sig,
        });
        MintEntry {
            event: MintEvent {
                nullifier_hex: nullifier_hex.into(),
                amount,
                l2_destination: dest,
                l1_create_height: 1,
                lock_blocks: 99,
                l1_value_sat: 1_000_000,
            },
            witness,
        }
    }

    #[test]
    fn retarget_at_window_boundary() {
        let target_total =
            (TARGET_ATOMS_PER_BLOCK as f64) * (RETARGET_WINDOW_BLOCKS as f64);

        // Overshoot by 2x: ratio = 2, r halves.
        let mut state = LedgerState::new();
        let r0 = state.current_r;
        state.current_window_atoms = (target_total * 2.0) as u64;
        state.end_of_block(RETARGET_WINDOW_BLOCKS);
        assert!((state.current_r - r0 / 2.0).abs() < 1e-15);
        assert_eq!(state.current_window_atoms, 0);
        assert_eq!(state.current_window_start_height, RETARGET_WINDOW_BLOCKS + 1);

        // Overshoot by 100x: clamped to 4x, r divides by 4.
        let mut state = LedgerState::new();
        let r0 = state.current_r;
        state.current_window_atoms = (target_total * 100.0) as u64;
        state.end_of_block(RETARGET_WINDOW_BLOCKS);
        assert!((state.current_r - r0 / 4.0).abs() < 1e-15);

        // Undershoot to zero: clamped to 0.25, r multiplies by 4.
        let mut state = LedgerState::new();
        let r0 = state.current_r;
        state.current_window_atoms = 0;
        state.end_of_block(RETARGET_WINDOW_BLOCKS);
        assert!((state.current_r - r0 * 4.0).abs() < 1e-15);

        // Mid-window: no retarget.
        let mut state = LedgerState::new();
        let r0 = state.current_r;
        state.current_window_atoms = 999_999_999;
        state.end_of_block(RETARGET_WINDOW_BLOCKS - 1);
        assert_eq!(state.current_r, r0);
        assert_eq!(state.current_window_atoms, 999_999_999);

        // Genesis (height 0): no retarget even if height % W == 0.
        let mut state = LedgerState::new();
        let r0 = state.current_r;
        state.end_of_block(0);
        assert_eq!(state.current_r, r0);
    }

    #[test]
    fn account_inclusion_proof_verifies_against_accounts_root() {
        let secp = Secp256k1::new();
        let alice_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (alice, _) = alice_kp.x_only_public_key();
        let bob_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (bob, _) = bob_kp.x_only_public_key();
        let mallory_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (mallory, _) = mallory_kp.x_only_public_key();

        let mut state = LedgerState::new();
        state.apply(&secp, &L2Tx::Mint(stub_entry("aa", 1000, alice, &alice_kp))).unwrap();
        state.apply(&secp, &L2Tx::Mint(stub_entry("bb", 500, bob, &bob_kp))).unwrap();

        let root = state.accounts_root();

        // Alice: inclusion proof.
        let p = state.account_inclusion_proof(alice);
        assert!(matches!(p.leaf, crate::smt::LeafKind::Account { balance: 1000, nonce: 0 }));
        assert!(p.verify(root));

        // Bob: inclusion proof.
        let p = state.account_inclusion_proof(bob);
        assert!(matches!(p.leaf, crate::smt::LeafKind::Account { balance: 500, nonce: 0 }));
        assert!(p.verify(root));

        // Mallory: non-inclusion proof (no account).
        let p = state.account_inclusion_proof(mallory);
        assert_eq!(p.leaf, crate::smt::LeafKind::Empty);
        assert!(p.verify(root));
    }

    #[test]
    fn mint_then_transfer() {
        let secp = Secp256k1::new();
        let alice_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (alice, _) = alice_kp.x_only_public_key();
        let bob_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (bob, _) = bob_kp.x_only_public_key();

        let mut state = LedgerState::new();
        state.apply(&secp, &L2Tx::Mint(stub_entry("deadbeef", 1000, alice, &alice_kp)))
            .unwrap();
        assert_eq!(state.balance_of(&alice), 1000);

        // Double-mint rejected
        let dup = L2Tx::Mint(stub_entry("deadbeef", 1, bob, &alice_kp));
        assert!(matches!(state.apply(&secp, &dup), Err(ApplyError::DoubleMint)));

        // Transfer 300 alice -> bob
        let body = TransferBody { from: alice, to: bob, amount: 300, nonce: 0 };
        let msg = Message::from_digest(body.sighash().0);
        let sig = secp.sign_schnorr(&msg, &alice_kp);
        let xfer = L2Tx::Transfer(SignedTransfer { body, signature: sig });
        state.apply(&secp, &xfer).unwrap();
        assert_eq!(state.balance_of(&alice), 700);
        assert_eq!(state.balance_of(&bob), 300);
        assert_eq!(state.nonce_of(&alice), 1);
    }
}
