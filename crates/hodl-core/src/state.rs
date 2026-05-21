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
    INITIAL_R, RETARGET_MAX_FACTOR, RETARGET_MINT_WINDOW_ATOMS, TARGET_ATOMS_PER_BLOCK,
};
use crate::hash::H256;
use crate::smt;
use crate::tx::{Amount, L2Address, L2Tx, MintEntry, SignedTransfer};
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;
use bitcoin::secp256k1::{Message, Secp256k1, Verification};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct Account {
    pub balance: Amount,
    pub nonce: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LedgerState {
    pub accounts: BTreeMap<L2Address, Account>,
    /// Nullifier bytes, hex-encoded for portability.
    pub consumed_nullifiers: BTreeSet<String>,
    /// Current mint-function rate parameter. Adjusted at retarget
    /// boundaries (cumulative atoms reaching `RETARGET_MINT_WINDOW_ATOMS`).
    pub current_r: f64,
    /// Atoms minted in the currently-open retarget window, used to
    /// drive the next retarget. Retargeting is mint-paced (paper §7):
    /// during quiet periods this stays at 0 and the loop does not
    /// advance, so `current_r` is preserved across quiescence.
    pub current_window_atoms: u64,
    /// L1 height at which the currently-open retarget window began
    /// (= L1 height of the first L2 block in this window that
    /// contained a mint). `None` during quiet periods — set when a
    /// window's first mint lands; cleared when the window retargets.
    pub current_window_start_l1_height: Option<u32>,
}

impl Default for LedgerState {
    fn default() -> Self {
        Self {
            accounts: BTreeMap::new(),
            consumed_nullifiers: BTreeSet::new(),
            current_r: INITIAL_R,
            current_window_atoms: 0,
            current_window_start_l1_height: None,
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

    /// Called by producer / node AFTER all txs in an L2 block have
    /// applied. `l2_height` is the L2 block's height; `l1_height` is
    /// the L1 block this L2 block is anchored to.
    ///
    /// Mint-paced retargeting (paper §7): once cumulative atoms minted
    /// in the current window reach `RETARGET_MINT_WINDOW_ATOMS`, the
    /// protocol measures Δ_actual (elapsed L1 blocks from window start
    /// to now), computes the observed atoms-per-L1-block rate, and
    /// adjusts `r` toward the target rate, clamped to ±MAX_FACTOR.
    ///
    /// Direction: observed > target → ratio < 1 → r shrinks → future
    /// mints earn less per (V,T). Symmetric the other way.
    ///
    /// Window-start bookkeeping: the window opens with its first
    /// mint, so `current_window_start_l1_height` is set lazily here
    /// (the first time this function is called with `current_window_atoms
    /// > 0` and the field still `None`). Quiet periods leave both the
    /// field None and the window counter 0, so retargeting genuinely
    /// pauses — `r` is not perturbed at all.
    pub fn end_of_block(&mut self, l2_height: u32, l1_height: u32) {
        if l2_height == 0 {
            return;
        }
        // No mints anywhere yet → nothing to do.
        if self.current_window_atoms == 0 {
            return;
        }
        // First-mint-of-window bookkeeping. The mint(s) that pushed
        // `current_window_atoms` above zero happened in this block,
        // so this block's L1 height is the window's L1-start.
        if self.current_window_start_l1_height.is_none() {
            self.current_window_start_l1_height = Some(l1_height);
        }
        // Retarget if this block pushed cumulative atoms past M_w.
        if self.current_window_atoms < RETARGET_MINT_WINDOW_ATOMS {
            return;
        }
        let start = self
            .current_window_start_l1_height
            .expect("set immediately above if it wasn't");
        let delta_actual = l1_height.saturating_sub(start);
        if delta_actual == 0 {
            // Threshold crossed in the same L1 block the window
            // opened in. Δ_actual = 0 — defer to the next block
            // boundary, where Δ_actual will be at least 1.
            return;
        }
        let m_obs = self.current_window_atoms as f64 / delta_actual as f64;
        let m_star = TARGET_ATOMS_PER_BLOCK as f64;
        let raw_ratio = m_star / m_obs;
        let ratio = raw_ratio
            .max(1.0 / RETARGET_MAX_FACTOR)
            .min(RETARGET_MAX_FACTOR);
        self.current_r *= ratio;
        self.current_window_atoms = 0;
        self.current_window_start_l1_height = None;
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
        nullifiers_hash_of(&self.consumed_nullifiers)
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
            current_window_start_l1_height: self.current_window_start_l1_height,
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

/// Hash a sorted consumed-nullifier set. Exposed so the light wallet
/// can compute it without depending on sha2 directly. Must stay in
/// sync with `LedgerState::nullifiers_hash`.
pub fn nullifiers_hash_of(set: &BTreeSet<String>) -> H256 {
    let mut h = Sha256::new();
    h.update(b"hodl-nullifiers-v0");
    for n in set {
        h.update(n.as_bytes());
    }
    H256(h.finalize().into())
}

/// The inputs that `state_root` hashes together. Producer / node /
/// light-client all agree on this struct; making it explicit lets a
/// light client receiving (accounts_root, nullifiers_hash, retarget
/// scalars) recompute the `state_root` and compare against the value
/// it pulled off L1.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct StateComponents {
    pub accounts_root: H256,
    pub nullifiers_hash: H256,
    pub current_r: f64,
    pub current_window_atoms: u64,
    /// L1 height at which the currently-open mint-paced retarget
    /// window began. `None` during quiet periods (no mints in
    /// progress).
    pub current_window_start_l1_height: Option<u32>,
}

impl StateComponents {
    /// Canonical state-root hash. v2 — the retarget tail encodes the
    /// window-start L1 height as a tag + payload so the `None` case
    /// has a distinct byte representation.
    pub fn state_root(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-state-v2");
        h.update(&self.accounts_root.0);
        h.update(&self.nullifiers_hash.0);
        h.update(b"|retarget|");
        h.update(&self.current_r.to_le_bytes());
        h.update(&self.current_window_atoms.to_be_bytes());
        match self.current_window_start_l1_height {
            Some(h1) => {
                h.update([1u8]);
                h.update(&h1.to_be_bytes());
            }
            None => {
                h.update([0u8]);
            }
        }
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
        let claimed_h = 2u32;
        let sighash = OutpointProof::sighash(&outpoint, claimed_h, &dest);
        let msg = bitcoin::secp256k1::Message::from_digest(sighash);
        let sig: schnorr::Signature = secp.sign_schnorr(&msg, signing_kp);
        let witness = MintProofEnvelope::V0Outpoint(OutpointProof {
            outpoint,
            user_xonly_pubkey: pk,
            lock_blocks: 99,
            claimed_block_height: claimed_h,
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
    fn retarget_quiet_period_does_not_advance() {
        // The whole point of mint-paced retargeting: with no mints,
        // end_of_block must be a no-op. r is preserved across any
        // number of empty blocks.
        let mut state = LedgerState::new();
        let r0 = state.current_r;
        for l1 in 1..=10_000 {
            state.end_of_block(l1, l1);
        }
        assert_eq!(state.current_r, r0);
        assert_eq!(state.current_window_atoms, 0);
        assert_eq!(state.current_window_start_l1_height, None);
    }

    #[test]
    fn retarget_genesis_block_is_noop() {
        let mut state = LedgerState::new();
        let r0 = state.current_r;
        state.current_window_atoms = RETARGET_MINT_WINDOW_ATOMS * 10;
        state.end_of_block(0, 999);
        assert_eq!(state.current_r, r0);
    }

    #[test]
    fn retarget_first_mint_anchors_window_start() {
        // Cumulative atoms goes from 0 → below-threshold at L1=200.
        // No retarget yet, but window_start gets set.
        let mut state = LedgerState::new();
        state.current_window_atoms = RETARGET_MINT_WINDOW_ATOMS / 4; // below
        let r0 = state.current_r;
        state.end_of_block(1, 200);
        assert_eq!(state.current_r, r0);
        assert_eq!(state.current_window_start_l1_height, Some(200));
        assert_eq!(state.current_window_atoms, RETARGET_MINT_WINDOW_ATOMS / 4);
    }

    #[test]
    fn retarget_defers_when_delta_is_zero() {
        // The threshold is crossed in the same L1 block the window
        // opened in. Δ_actual = 0; must defer to the next block.
        let mut state = LedgerState::new();
        state.current_window_atoms = RETARGET_MINT_WINDOW_ATOMS * 2;
        let r0 = state.current_r;
        state.end_of_block(1, 200);
        // Window-start has been set to 200 but retarget deferred.
        assert_eq!(state.current_window_start_l1_height, Some(200));
        assert_eq!(state.current_r, r0);
        assert_eq!(state.current_window_atoms, RETARGET_MINT_WINDOW_ATOMS * 2);
    }

    #[test]
    fn retarget_observed_at_target_leaves_r_unchanged() {
        // Δ_actual chosen so M_obs = M*. Ratio = 1, r stays.
        let mut state = LedgerState::new();
        state.current_window_atoms = RETARGET_MINT_WINDOW_ATOMS;
        state.current_window_start_l1_height = Some(100);
        let r0 = state.current_r;
        // delta_actual such that current_window_atoms / delta_actual = M*
        let delta = (RETARGET_MINT_WINDOW_ATOMS / TARGET_ATOMS_PER_BLOCK) as u32;
        state.end_of_block(2, 100 + delta);
        assert!((state.current_r - r0).abs() < 1e-15);
        assert_eq!(state.current_window_atoms, 0);
        assert_eq!(state.current_window_start_l1_height, None);
    }

    #[test]
    fn retarget_observed_above_target_decreases_r() {
        // delta_actual = half the healthy figure → M_obs = 2 × M* →
        // ratio = 0.5 → r halves (and that's within clamp).
        let mut state = LedgerState::new();
        state.current_window_atoms = RETARGET_MINT_WINDOW_ATOMS;
        state.current_window_start_l1_height = Some(100);
        let r0 = state.current_r;
        let delta = (RETARGET_MINT_WINDOW_ATOMS / TARGET_ATOMS_PER_BLOCK / 2) as u32;
        state.end_of_block(2, 100 + delta);
        assert!((state.current_r - r0 * 0.5).abs() < 1e-12);
    }

    #[test]
    fn retarget_observed_below_target_increases_r() {
        // delta_actual = double the healthy figure → M_obs = M*/2 →
        // ratio = 2 → r doubles (right at the clamp).
        let mut state = LedgerState::new();
        state.current_window_atoms = RETARGET_MINT_WINDOW_ATOMS;
        state.current_window_start_l1_height = Some(100);
        let r0 = state.current_r;
        let delta = (RETARGET_MINT_WINDOW_ATOMS / TARGET_ATOMS_PER_BLOCK * 2) as u32;
        state.end_of_block(2, 100 + delta);
        assert!((state.current_r - r0 * 2.0).abs() < 1e-12);
    }

    #[test]
    fn retarget_clamp_pins_at_max_factor() {
        // Extreme overshoot (mints happened in 1 block) → ratio ~tiny
        // → clamped to 1/MAX_FACTOR → r divides by MAX_FACTOR exactly.
        let mut state = LedgerState::new();
        state.current_window_atoms = RETARGET_MINT_WINDOW_ATOMS * 1000;
        state.current_window_start_l1_height = Some(100);
        let r0 = state.current_r;
        state.end_of_block(2, 101);
        assert!((state.current_r - r0 / RETARGET_MAX_FACTOR).abs() < 1e-12);
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
