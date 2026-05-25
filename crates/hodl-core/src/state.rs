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

use crate::consensus::{FEE_BPS, MIN_FEE};
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct LedgerState {
    pub accounts: BTreeMap<L2Address, Account>,
    /// Nullifier bytes, hex-encoded for portability.
    pub consumed_nullifiers: BTreeSet<String>,
    /// Running sum of every mint amount this ledger has ever applied.
    /// Equal to the total L2 supply at any point in time (transfers
    /// are conservative). Not part of `StateComponents` / state_root
    /// — exposed via RPC for stats / UI panels only; light clients
    /// accumulate it independently during block walks.
    pub total_minted_atoms: u64,
    /// L2 address that receives per-transfer protocol fees. `None`
    /// means fees are burned (subtracted from sender but credited to
    /// no one). Set at chain init from genesis; immutable thereafter.
    /// Part of `StateComponents` so the state_root commits to it.
    pub sequencer_fee_address: Option<L2Address>,
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
    fn apply_mint(&mut self, entry: &MintEntry) -> Result<(), ApplyError> {
        let ev = &entry.event;
        if ev.amount == 0 { return Err(ApplyError::ZeroAmount); }
        if self.consumed_nullifiers.contains(&ev.nullifier_hex) {
            return Err(ApplyError::DoubleMint);
        }
        self.consumed_nullifiers.insert(ev.nullifier_hex.clone());
        let acct = self.accounts.entry(ev.l2_destination).or_default();
        acct.balance = acct.balance.saturating_add(ev.amount);
        self.total_minted_atoms = self.total_minted_atoms.saturating_add(ev.amount);
        Ok(())
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

        // Compute the protocol fee. `fee = max(MIN_FEE, amount * FEE_BPS
        // / 10_000)`. Sender pays `amount + fee`; recipient gets
        // `amount`; the fee credits the sequencer fee address (or is
        // burned if the chain was bootstrapped without one).
        let fee = core::cmp::max(MIN_FEE, t.body.amount.saturating_mul(FEE_BPS) / 10_000);
        let total = t.body.amount.saturating_add(fee);

        let from_balance = self.balance_of(&t.body.from);
        if from_balance < total { return Err(ApplyError::InsufficientBalance); }

        let from = self.accounts.entry(t.body.from).or_default();
        from.balance -= total;
        from.nonce += 1;
        let to = self.accounts.entry(t.body.to).or_default();
        to.balance = to.balance.saturating_add(t.body.amount);
        if let Some(fee_addr) = self.sequencer_fee_address {
            // Sender == fee recipient would have routed fee back to
            // self — fine, the math is consistent (subtracted as part
            // of `total`, added back to fee_addr). No special case
            // needed.
            let fee_acct = self.accounts.entry(fee_addr).or_default();
            fee_acct.balance = fee_acct.balance.saturating_add(fee);
        }
        // If `sequencer_fee_address` is None, the fee atoms are
        // burned (total supply decreases). This is the placeholder
        // case used before the sequencer identity is wired through
        // genesis in Phase 3.
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
            sequencer_fee_address: self.sequencer_fee_address,
        }
    }

    /// Deterministic state root: hash over (accounts_root,
    /// nullifiers_hash, sequencer_fee_address). With the v2 design
    /// dropping retargeting, the state no longer commits to any
    /// `current_r` / retarget-window scalars.
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
/// light client receiving (accounts_root, nullifiers_hash, fee
/// address) recompute the `state_root` and compare against the value
/// it pulled off L1.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct StateComponents {
    pub accounts_root: H256,
    pub nullifiers_hash: H256,
    /// L2 address that receives per-transfer fees. `None` means
    /// fees are burned. Set at chain init from genesis; immutable.
    #[cfg_attr(
        feature = "std",
        schema(value_type = Option<String>, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
    pub sequencer_fee_address: Option<L2Address>,
}

impl StateComponents {
    /// Canonical state-root hash. v4 — drops the retarget tail
    /// (current_r, current_window_atoms, current_window_start_l1)
    /// since the v2 design fixes `r` as a consensus constant. The
    /// fee-address tail is retained.
    pub fn state_root(&self) -> H256 {
        let mut h = Sha256::new();
        h.update(b"hodl-state-v4");
        h.update(&self.accounts_root.0);
        h.update(&self.nullifiers_hash.0);
        h.update(b"|fee|");
        match &self.sequencer_fee_address {
            Some(addr) => {
                h.update([1u8]);
                h.update(addr.serialize());
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

        // Transfer 300 alice -> bob. Fee is max(MIN_FEE=100,
        // 300*1/10000=0) = 100 atoms. With sequencer_fee_address
        // = None (the default in LedgerState::new()), fees are
        // burned. So Alice pays 400, Bob receives 300, the
        // remaining 100 vanishes.
        let body = TransferBody { from: alice, to: bob, amount: 300, nonce: 0 };
        let msg = Message::from_digest(body.sighash().0);
        let sig = secp.sign_schnorr(&msg, &alice_kp);
        let xfer = L2Tx::Transfer(SignedTransfer { body, signature: sig });
        state.apply(&secp, &xfer).unwrap();
        assert_eq!(state.balance_of(&alice), 600);
        assert_eq!(state.balance_of(&bob), 300);
        assert_eq!(state.nonce_of(&alice), 1);
    }

    #[test]
    fn transfer_credits_fee_to_sequencer_address() {
        // With sequencer_fee_address = Some(seq), the fee is
        // credited to seq's account rather than burned.
        let secp = Secp256k1::new();
        let alice_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (alice, _) = alice_kp.x_only_public_key();
        let bob_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (bob, _) = bob_kp.x_only_public_key();
        let seq_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (seq, _) = seq_kp.x_only_public_key();

        let mut state = LedgerState::new();
        state.sequencer_fee_address = Some(seq);
        state.apply(&secp, &L2Tx::Mint(stub_entry("aaaa", 1000, alice, &alice_kp)))
            .unwrap();

        let body = TransferBody { from: alice, to: bob, amount: 300, nonce: 0 };
        let msg = Message::from_digest(body.sighash().0);
        let sig = secp.sign_schnorr(&msg, &alice_kp);
        state.apply(&secp, &L2Tx::Transfer(SignedTransfer { body, signature: sig })).unwrap();

        assert_eq!(state.balance_of(&alice), 600);
        assert_eq!(state.balance_of(&bob), 300);
        assert_eq!(state.balance_of(&seq), 100); // MIN_FEE
        // Total supply preserved (no burn).
        assert_eq!(
            state.balance_of(&alice) + state.balance_of(&bob) + state.balance_of(&seq),
            1000,
        );
    }

    #[test]
    fn transfer_fee_scales_with_amount_above_min() {
        // Amount large enough that 1 bp exceeds MIN_FEE.
        // 2_000_000 atoms * 1 / 10_000 = 200 > MIN_FEE (100).
        let secp = Secp256k1::new();
        let alice_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (alice, _) = alice_kp.x_only_public_key();
        let bob_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (bob, _) = bob_kp.x_only_public_key();
        let seq_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (seq, _) = seq_kp.x_only_public_key();

        let mut state = LedgerState::new();
        state.sequencer_fee_address = Some(seq);
        state.apply(&secp, &L2Tx::Mint(stub_entry("bbbb", 5_000_000, alice, &alice_kp)))
            .unwrap();

        let body = TransferBody { from: alice, to: bob, amount: 2_000_000, nonce: 0 };
        let msg = Message::from_digest(body.sighash().0);
        let sig = secp.sign_schnorr(&msg, &alice_kp);
        state.apply(&secp, &L2Tx::Transfer(SignedTransfer { body, signature: sig })).unwrap();

        assert_eq!(state.balance_of(&seq), 200);
        assert_eq!(state.balance_of(&alice), 5_000_000 - 2_000_000 - 200);
    }

    #[test]
    fn transfer_rejected_when_balance_below_amount_plus_fee() {
        // Alice has exactly `amount` worth of balance — insufficient
        // because she also owes the fee.
        let secp = Secp256k1::new();
        let alice_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (alice, _) = alice_kp.x_only_public_key();
        let bob_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (bob, _) = bob_kp.x_only_public_key();

        let mut state = LedgerState::new();
        state.apply(&secp, &L2Tx::Mint(stub_entry("cccc", 300, alice, &alice_kp)))
            .unwrap();
        let body = TransferBody { from: alice, to: bob, amount: 300, nonce: 0 };
        let msg = Message::from_digest(body.sighash().0);
        let sig = secp.sign_schnorr(&msg, &alice_kp);
        assert!(matches!(
            state.apply(&secp, &L2Tx::Transfer(SignedTransfer { body, signature: sig })),
            Err(ApplyError::InsufficientBalance)
        ));
    }
}
