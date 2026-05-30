//! MintProof trait + V0 OutpointProof implementation, plus the wire-form
//! `MintProofEnvelope` that travels both over the submit-mint HTTP request
//! and inside L2 block bodies as part of `L2Tx::Mint(MintEntry)`.
//!
//! The envelope is the seam for adding aut-ct ring proofs later:
//!
//!   v0: V0Outpoint(OutpointProof)   → nullifier = serialised L1 outpoint
//!   v1: V1Ring(RingProof)           → nullifier = LSAG key image
//!   v2: V2Zk(ZkProof)               → nullifier = ZK-derived
//!
//! Each variant has its own `nullifier()` and its own L1-side verification
//! procedure; all produce a `MintCredit`. Nodes re-run the witness when
//! they apply a block (`verify_mint_entry`), so block validity is
//! independent of trusting the sequencer.

use crate::consensus::{mint_fn, ATOMS_PER_SAT, MAX_LOCK_BLOCKS, MINT_CONFIRMATIONS};
use crate::l1::expected_p2tr_spk;
use crate::tx::{L2Address, MintEntry, MintEvent};
use alloc::vec::Vec;
use bitcoin::secp256k1::{schnorr, Message, Secp256k1, Verification, XOnlyPublicKey};
use bitcoin::{OutPoint, ScriptBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

/// The wire-form mint witness. Used both:
///   - as the body of the `POST /mint` submit request, and
///   - as the `witness` field of `MintEntry` inside an L2 block.
///
/// Adding a new proof family (ring sig, ZK) means adding a variant here
/// and implementing its `MintProof` impl; block format and consumed-set
/// schema are unchanged.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
#[serde(tag = "variant", rename_all = "snake_case")]
pub enum MintProofEnvelope {
    V0Outpoint(OutpointProof),
    // V1Ring(RingProof) — added when aut-ct integration lands.
}

impl MintProof for MintProofEnvelope {
    fn nullifier(&self) -> Vec<u8> {
        match self {
            MintProofEnvelope::V0Outpoint(p) => p.nullifier(),
        }
    }

    fn verify<C: Verification>(
        &self,
        secp: &Secp256k1<C>,
        l1: &dyn L1View,
        l2_destination: L2Address,
    ) -> Result<MintCredit, MintError> {
        match self {
            MintProofEnvelope::V0Outpoint(p) => p.verify(secp, l1, l2_destination),
        }
    }
}

/// A view onto L1 that the verifier can consult. Implementations: a thin
/// wrapper around bitcoincore-rpc on the daemon side; a fake in tests.
pub trait L1View {
    fn get_output(&self, outpoint: &OutPoint) -> Option<L1Output>;
    fn tip_height(&self) -> u32;
}

#[derive(Clone, Debug)]
pub struct L1Output {
    pub value_sat: u64,
    pub script_pubkey: ScriptBuf,
    pub confirmed_height: u32,
    pub confirmations: u32,
}

#[derive(Debug, Error)]
pub enum MintError {
    #[error("declared mint event disagrees with witness-derived credit: {field}")]
    EventMismatch { field: &'static str },
    #[error("L1 outpoint not found")]
    OutpointNotFound,
    #[error("insufficient confirmations: {got} < {need}")]
    NotEnoughConfirmations { got: u32, need: u32 },
    #[error("scriptPubKey does not match the revealed tapleaf under NUMS H")]
    ScriptMismatch,
    #[error("invalid Schnorr signature over the mint message")]
    BadSignature,
    #[error("invalid lock duration: lock_blocks={got}, must be in [1, {max}]")]
    BadLockBlocks { got: u32, max: u32 },
    #[error("value mismatch: on-chain {onchain} vs claimed {claimed}")]
    ValueMismatch { onchain: u64, claimed: u64 },
    #[error("mint amount underflowed/overflowed")]
    AmountOverflow,
    #[error("claimed height {claimed} is before lock creation height {create}")]
    ClaimedHeightBeforeCreate { claimed: u32, create: u32 },
    #[error("lock expired: claimed height {claimed} >= unlock height {unlock}")]
    LockExpired { claimed: u32, unlock: u32 },
    #[error("claimed height {claimed} is in the future (L1 tip is {tip})")]
    ClaimedHeightInFuture { claimed: u32, tip: u32 },
}

/// The credit a successful mint produces, ready to be embedded into a block.
#[derive(Clone, Debug)]
pub struct MintCredit {
    pub event: MintEvent,
    /// Opaque nullifier bytes (also encoded into `event.nullifier_hex`).
    pub nullifier: Vec<u8>,
}

/// A witness that authorises a mint. Implementations:
///   - v0: `OutpointProof` (this module)
///   - v1: aut-ct ring proof (out of scope)
pub trait MintProof {
    /// Bytes used for dedup. Sequencer rejects any proof whose nullifier
    /// already appears in the consumed set.
    fn nullifier(&self) -> Vec<u8>;

    /// Verify the proof against the current L1 view and a chosen L2 dest.
    /// On success, return the credit to apply.
    fn verify<C: Verification>(
        &self,
        secp: &Secp256k1<C>,
        l1: &dyn L1View,
        l2_destination: L2Address,
    ) -> Result<MintCredit, MintError>;
}

// ---------- v0: OutpointProof ----------

/// V0 mint witness. Carries only the values the verifier cannot recompute:
/// the outpoint being claimed, the locker's BIP340 x-only pubkey, the
/// relative locktime `T` baked into L_spend's CSV, and a Schnorr
/// signature binding the mint to (outpoint, l2_destination).
///
/// The verifier reconstructs `L_spend`, `L_data` (with the hodlchain
/// chain_id namespace stamp), the 2-leaf Merkle root and the tweaked
/// NUMS-H output key from `(user_xonly_pubkey, lock_blocks)` alone, and
/// compares the resulting scriptPubKey to the on-chain SPK of `outpoint`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "std", derive(utoipa::ToSchema))]
pub struct OutpointProof {
    /// L1 outpoint funding this mint. Serialised as `{txid, vout}`.
    #[cfg_attr(feature = "std", schema(value_type = crate::schemas::OutPointWire))]
    pub outpoint: OutPoint,
    /// Locker's BIP340 x-only public key (hex-encoded).
    #[cfg_attr(
        feature = "std",
        schema(value_type = String, example = "0000000000000000000000000000000000000000000000000000000000000001")
    )]
    pub user_xonly_pubkey: XOnlyPublicKey,
    /// Relative locktime T baked into L_spend's CSV.
    pub lock_blocks: u32,
    /// L1 block height the locker claims to be submitting at. The
    /// verifier requires `T_create ≤ claimed_height < T_create + T`
    /// (the active lock period defined by the design paper) and
    /// `claimed_height ≤ L1 tip`. Bound into the sighash so a stale
    /// request can't be replayed outside its claimed window.
    pub claimed_block_height: u32,
    /// Schnorr signature over `sha256("hodl-mint-v1" || outpoint || claimed_block_height_be || l2_destination)`.
    #[cfg_attr(feature = "std", schema(value_type = String))]
    pub signature: schnorr::Signature,
}

impl OutpointProof {
    /// Canonical sighash bound by the v1 proof:
    /// `sha256("hodl-mint-v1" || outpoint || claimed_block_height_be || l2_destination)`.
    /// Paper §3 defines the message as
    /// `m = (outpoint(u), h, L2-destination)`; the v1 tag-bump from
    /// v0 reflects the addition of `h`.
    pub fn sighash(
        outpoint: &OutPoint,
        claimed_block_height: u32,
        l2_destination: &L2Address,
    ) -> [u8; 32] {
        let mut h = Sha256::new();
        h.update(b"hodl-mint-v1");
        h.update(AsRef::<[u8]>::as_ref(&outpoint.txid));
        h.update(&outpoint.vout.to_le_bytes());
        h.update(&claimed_block_height.to_be_bytes());
        h.update(&l2_destination.serialize());
        h.finalize().into()
    }

    /// Serialize the L1 outpoint into a stable nullifier-bytes form.
    pub fn outpoint_nullifier(outpoint: &OutPoint) -> Vec<u8> {
        let mut v = Vec::with_capacity(36);
        v.extend_from_slice(AsRef::<[u8]>::as_ref(&outpoint.txid));
        v.extend_from_slice(&outpoint.vout.to_le_bytes());
        v
    }
}

impl MintProof for OutpointProof {
    fn nullifier(&self) -> Vec<u8> {
        Self::outpoint_nullifier(&self.outpoint)
    }

    fn verify<C: Verification>(
        &self,
        secp: &Secp256k1<C>,
        l1: &dyn L1View,
        l2_destination: L2Address,
    ) -> Result<MintCredit, MintError> {
        let output = l1
            .get_output(&self.outpoint)
            .ok_or(MintError::OutpointNotFound)?;

        if output.confirmations < MINT_CONFIRMATIONS {
            return Err(MintError::NotEnoughConfirmations {
                got: output.confirmations,
                need: MINT_CONFIRMATIONS,
            });
        }

        // 1. lock_blocks range check (BIP112 block-form bounds).
        if self.lock_blocks == 0 || self.lock_blocks > MAX_LOCK_BLOCKS {
            return Err(MintError::BadLockBlocks {
                got: self.lock_blocks,
                max: MAX_LOCK_BLOCKS,
            });
        }

        // 2. Active lock period (per the design paper): the
        //    claimed_block_height must lie in the half-open interval
        //    [T_create, T_create + T), and must not exceed the
        //    current L1 tip (no future-dated mints).
        let create = output.confirmed_height;
        let unlock = create.saturating_add(self.lock_blocks);
        if self.claimed_block_height < create {
            return Err(MintError::ClaimedHeightBeforeCreate {
                claimed: self.claimed_block_height,
                create,
            });
        }
        if self.claimed_block_height >= unlock {
            return Err(MintError::LockExpired {
                claimed: self.claimed_block_height,
                unlock,
            });
        }
        let tip = l1.tip_height();
        if self.claimed_block_height > tip {
            return Err(MintError::ClaimedHeightInFuture {
                claimed: self.claimed_block_height,
                tip,
            });
        }

        // 3. scriptPubKey check: reconstruct the canonical hodlchain
        //    2-leaf taproot (L_spend with this user's pk + relative
        //    locktime T, plus L_data binding to chain_id "hodlchain")
        //    under NUMS H and compare. A single SPK-equality check
        //    simultaneously verifies: NUMS internal key, both leaves
        //    present, both well-formed, pk matches the signing key,
        //    `T` matches `lock_blocks`, and the namespace stamp
        //    resolves to hodlchain.
        // `expected_p2tr_spk` re-validates `lock_blocks` and would
        // return an error for out-of-range input — but step 1 above
        // already returns `BadLockBlocks` for that case, so by the
        // time we reach here it can only succeed. Belt and braces.
        let expected = expected_p2tr_spk(secp, self.lock_blocks, &self.user_xonly_pubkey)
            .map_err(|_| MintError::BadLockBlocks {
                got: self.lock_blocks,
                max: MAX_LOCK_BLOCKS,
            })?;
        if expected != output.script_pubkey {
            return Err(MintError::ScriptMismatch);
        }

        // 4. Signature check. The sighash binds the claimed_block_height,
        //    so a stale request can't be replayed outside the window
        //    the signer asserted.
        let sighash = Self::sighash(
            &self.outpoint,
            self.claimed_block_height,
            &l2_destination,
        );
        let msg = Message::from_digest(sighash);
        secp.verify_schnorr(&self.signature, &msg, &self.user_xonly_pubkey)
            .map_err(|_| MintError::BadSignature)?;

        // 4. Compute credit. T comes directly from the script-committed
        //    value, not from `unlock_height - create_height`.
        let _ = ATOMS_PER_SAT; // referenced inside mint_fn
        let amount = mint_fn(output.value_sat, self.lock_blocks);
        if amount == 0 {
            return Err(MintError::AmountOverflow);
        }

        let nullifier = self.nullifier();
        let event = MintEvent {
            nullifier_hex: hex::encode(&nullifier),
            amount,
            l2_destination,
            l1_create_height: output.confirmed_height,
            lock_blocks: self.lock_blocks,
            l1_value_sat: output.value_sat,
        };
        Ok(MintCredit { event, nullifier })
    }
}

/// Validate a mint entry as it appears in an L2 block.
///
/// Runs the witness against L1 and checks that the produced `MintCredit`
/// agrees with the declared `event`. A mismatch means the sequencer
/// inserted a `MintEvent` whose claimed outcome (amount, destination,
/// lock parameters, on-chain value, create height, or nullifier) is not
/// what the witness actually authorises — which is a fatal block-level
/// fault for the node.
pub fn verify_mint_entry<C: Verification>(
    entry: &MintEntry,
    secp: &Secp256k1<C>,
    l1: &dyn L1View,
) -> Result<(), MintError> {
    let credit = entry
        .witness
        .verify(secp, l1, entry.event.l2_destination)?;
    let ev = &entry.event;
    let derived = &credit.event;

    if derived.nullifier_hex != ev.nullifier_hex {
        return Err(MintError::EventMismatch { field: "nullifier_hex" });
    }
    if derived.amount != ev.amount {
        return Err(MintError::EventMismatch { field: "amount" });
    }
    if derived.l2_destination != ev.l2_destination {
        return Err(MintError::EventMismatch { field: "l2_destination" });
    }
    if derived.l1_create_height != ev.l1_create_height {
        return Err(MintError::EventMismatch { field: "l1_create_height" });
    }
    if derived.lock_blocks != ev.lock_blocks {
        return Err(MintError::EventMismatch { field: "lock_blocks" });
    }
    if derived.l1_value_sat != ev.l1_value_sat {
        return Err(MintError::EventMismatch { field: "l1_value_sat" });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::l1::derive_mint_taproot;
    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::{Keypair, Secp256k1};
    use bitcoin::Txid;
    use std::cell::RefCell;
    use std::collections::HashMap;

    struct FakeL1 {
        outputs: RefCell<HashMap<OutPoint, L1Output>>,
        tip: u32,
    }
    impl L1View for FakeL1 {
        fn get_output(&self, outpoint: &OutPoint) -> Option<L1Output> {
            self.outputs.borrow().get(outpoint).cloned()
        }
        fn tip_height(&self) -> u32 { self.tip }
    }

    fn fake_l1_with(outpoint: OutPoint, value_sat: u64, spk: ScriptBuf, create_h: u32) -> FakeL1 {
        FakeL1 {
            outputs: RefCell::new(HashMap::from_iter([(
                outpoint,
                L1Output {
                    value_sat,
                    script_pubkey: spk,
                    confirmed_height: create_h,
                    confirmations: 6,
                },
            )])),
            tip: 200,
        }
    }

    #[test]
    fn outpoint_proof_happy_path() {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let lock_blocks = 900u32;
        let create_h = 100u32;

        let (spk, _) = derive_mint_taproot(&secp, lock_blocks, &xonly).unwrap();

        let outpoint = OutPoint::new(Txid::all_zeros(), 0);
        let value_sat = 1_000_000_000u64;
        let l1 = fake_l1_with(outpoint, value_sat, spk, create_h);

        let dest_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (l2_dest, _) = dest_kp.x_only_public_key();

        let claimed_h = create_h + 1; // inside the active lock period
        let msg_digest = OutpointProof::sighash(&outpoint, claimed_h, &l2_dest);
        let msg = Message::from_digest(msg_digest);
        let sig = secp.sign_schnorr(&msg, &kp);

        let proof = OutpointProof {
            outpoint,
            user_xonly_pubkey: xonly,
            lock_blocks,
            claimed_block_height: claimed_h,
            signature: sig,
        };

        let credit = proof
            .verify(&secp, &l1, l2_dest)
            .expect("verify");
        assert!(credit.event.amount > 0);
        assert_eq!(credit.event.l1_value_sat, value_sat);
        assert_eq!(credit.event.lock_blocks, lock_blocks);
        assert_eq!(credit.event.l1_create_height, create_h);
    }

    /// A UTXO built for user A's pk cannot be claimed by user B even if B
    /// somehow obtained a valid Schnorr signature: the SPK check fails
    /// because L_spend (and L_data) embed user A's pk, so the verifier
    /// reconstructs a different SPK.
    #[test]
    fn outpoint_proof_rejects_wrong_user() {
        let secp = Secp256k1::new();
        let kp_a = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let kp_b = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (pk_a, _) = kp_a.x_only_public_key();
        let (pk_b, _) = kp_b.x_only_public_key();
        let lock_blocks = 1000u32;

        // UTXO built for A
        let (spk_a, _) = derive_mint_taproot(&secp, lock_blocks, &pk_a).unwrap();

        let outpoint = OutPoint::new(Txid::all_zeros(), 0);
        let l1 = fake_l1_with(outpoint, 1_000_000_000, spk_a, 100);

        let dest_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (l2_dest, _) = dest_kp.x_only_public_key();

        // B forges a (well-formed) proof claiming the SPK is for B.
        let claimed_h = 101u32;
        let msg = Message::from_digest(OutpointProof::sighash(&outpoint, claimed_h, &l2_dest));
        let sig_b = secp.sign_schnorr(&msg, &kp_b);
        let proof = OutpointProof {
            outpoint,
            user_xonly_pubkey: pk_b,
            lock_blocks,
            claimed_block_height: claimed_h,
            signature: sig_b,
        };
        let err = proof
            .verify(&secp, &l1, l2_dest)
            .unwrap_err();
        assert!(matches!(err, MintError::ScriptMismatch));
    }

    #[test]
    fn outpoint_proof_rejects_zero_and_oversize_lock() {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let (spk, _) = derive_mint_taproot(&secp, 1000, &xonly).unwrap();

        let outpoint = OutPoint::new(Txid::all_zeros(), 0);
        let l1 = fake_l1_with(outpoint, 1_000_000_000, spk, 100);
        let dest_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (l2_dest, _) = dest_kp.x_only_public_key();

        let claimed_h = 101u32;
        let msg = Message::from_digest(OutpointProof::sighash(&outpoint, claimed_h, &l2_dest));
        let sig = secp.sign_schnorr(&msg, &kp);

        for bad in [0u32, MAX_LOCK_BLOCKS + 1, u32::MAX] {
            let proof = OutpointProof {
                outpoint,
                user_xonly_pubkey: xonly,
                lock_blocks: bad,
                claimed_block_height: claimed_h,
                signature: sig,
            };
            let err = proof
                .verify(&secp, &l1, l2_dest)
                .unwrap_err();
            assert!(matches!(err, MintError::BadLockBlocks { .. }), "bad={bad}");
        }
    }

    #[test]
    fn outpoint_proof_rejects_after_lock_expiry() {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let lock_blocks = 50u32;
        let create_h = 100u32;
        let (spk, _) = derive_mint_taproot(&secp, lock_blocks, &xonly).unwrap();
        let outpoint = OutPoint::new(Txid::all_zeros(), 0);
        let l1 = fake_l1_with(outpoint, 1_000_000_000, spk, create_h);

        let dest_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (l2_dest, _) = dest_kp.x_only_public_key();

        // Unlock height is create_h + lock_blocks = 150; claim *at* 150
        // which is the first block the lock would be releasable.
        let claimed_h = create_h + lock_blocks;
        let msg = Message::from_digest(OutpointProof::sighash(&outpoint, claimed_h, &l2_dest));
        let sig = secp.sign_schnorr(&msg, &kp);
        let proof = OutpointProof {
            outpoint,
            user_xonly_pubkey: xonly,
            lock_blocks,
            claimed_block_height: claimed_h,
            signature: sig,
        };
        let err = proof
            .verify(&secp, &l1, l2_dest)
            .unwrap_err();
        assert!(matches!(err, MintError::LockExpired { .. }));
    }

    #[test]
    fn outpoint_proof_rejects_future_dated_claim() {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let lock_blocks = 1000u32;
        let create_h = 100u32;
        let (spk, _) = derive_mint_taproot(&secp, lock_blocks, &xonly).unwrap();
        let outpoint = OutPoint::new(Txid::all_zeros(), 0);
        // Fake L1 tip is 200; claim a height after that.
        let l1 = fake_l1_with(outpoint, 1_000_000_000, spk, create_h);

        let dest_kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (l2_dest, _) = dest_kp.x_only_public_key();
        let claimed_h = 500u32; // > tip=200, still < create_h+lock=1100
        let msg = Message::from_digest(OutpointProof::sighash(&outpoint, claimed_h, &l2_dest));
        let sig = secp.sign_schnorr(&msg, &kp);
        let proof = OutpointProof {
            outpoint,
            user_xonly_pubkey: xonly,
            lock_blocks,
            claimed_block_height: claimed_h,
            signature: sig,
        };
        let err = proof
            .verify(&secp, &l1, l2_dest)
            .unwrap_err();
        assert!(matches!(err, MintError::ClaimedHeightInFuture { .. }));
    }
}
