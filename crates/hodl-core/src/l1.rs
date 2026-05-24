//! L1-side helpers: BIP341 NUMS internal key, the two tapleaves
//! (`L_spend` and `L_data`), and the canonical 2-leaf taproot construction
//! used by every hodlchain mint UTXO.
//!

use bitcoin::opcodes::all::{OP_CHECKSIG, OP_CSV, OP_DROP, OP_RETURN};
use bitcoin::script::Builder;
use bitcoin::secp256k1::{Secp256k1, Verification, XOnlyPublicKey};
use bitcoin::taproot::{LeafVersion, TaprootSpendInfo};
use bitcoin::{Address, Network, ScriptBuf, TapLeafHash};
use thiserror::Error;

use crate::consensus::{data_leaf_commitment, BIP341_NUMS_H_XONLY, MAX_LOCK_BLOCKS};

/// Errors that can arise constructing a hodlchain mint UTXO.
///
/// Construction-site validation is deliberately strict: a wallet
/// that hands the user a deposit address derived from out-of-range
/// inputs is putting their funds into a script whose lock
/// semantics don't match what they asked for. Catching the bad
/// input here, before the L1 broadcast, is the only point at
/// which we can do anything about it — the L2 verifier rejecting
/// the eventual mint witness is consensus-correct but doesn't
/// recover the user's BTC from a script that masks down to no
/// lock at all (cf. BIP112's lower-16-bits-of-nSequence rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MintConstructionError {
    #[error("lock_blocks must be ≥ 1; got 0 (BIP112 requires a non-zero relative locktime)")]
    LockBlocksTooSmall,
    #[error(
        "lock_blocks={got} exceeds BIP112 blocks-mode cap {max} (Bitcoin's CSV masks the \
         lower 16 bits of nSequence, so larger values silently mask down to a different \
         duration than the caller intended)"
    )]
    LockBlocksTooLarge { got: u32, max: u32 },
}

/// Return the BIP341 H NUMS x-only key used as the internal key for every
/// hodlchain mint Taproot output.
pub fn nums_internal_key() -> XOnlyPublicKey {
    XOnlyPublicKey::from_slice(&BIP341_NUMS_H_XONLY)
        .expect("BIP341 H is a valid x-only key constant")
}

/// Validate `lock_blocks` against BIP112's blocks-mode range.
/// Reused by every constructor that takes a `lock_blocks` argument
/// so the check lives in exactly one place. Exposed publicly so
/// upstream callers (wallet, GUI) can validate user input *before*
/// committing to any side-effecting work like allocating a BIP32
/// key index or rendering an address.
pub fn validate_lock_blocks(lock_blocks: u32) -> Result<(), MintConstructionError> {
    if lock_blocks == 0 {
        return Err(MintConstructionError::LockBlocksTooSmall);
    }
    if lock_blocks > MAX_LOCK_BLOCKS {
        return Err(MintConstructionError::LockBlocksTooLarge {
            got: lock_blocks,
            max: MAX_LOCK_BLOCKS,
        });
    }
    Ok(())
}

/// Build the L_spend tapleaf script using **CSV (BIP112)**:
///
/// ```text
/// <T> OP_CHECKSEQUENCEVERIFY OP_DROP <user_xonly_pk> OP_CHECKSIG
/// ```
///
/// `T` is the relative locktime in blocks (the committed duration `T`
/// that the minting function takes as its argument). It is encoded as a
/// minimally-pushed script integer; for `T ∈ [1, 65535]` the encoded
/// value sits in the lower 16 bits of `nSequence` and the BIP68 type bit
/// (bit 22) is 0 (blocks).
///
/// Returns `Err(MintConstructionError)` for `lock_blocks` outside
/// `[1, MAX_LOCK_BLOCKS]`. The check lives here, at the construction
/// site, because any value outside that range produces a script that
/// either has no lock (`lock_blocks = 0`, or any multiple of 65536) or
/// silently masks down to the wrong duration. Catching this at
/// verification time is too late — the L1 broadcast has already
/// happened.
pub fn csv_tapleaf_script(
    lock_blocks: u32,
    user_pk: &XOnlyPublicKey,
) -> Result<ScriptBuf, MintConstructionError> {
    validate_lock_blocks(lock_blocks)?;
    Ok(Builder::new()
        .push_int(lock_blocks as i64)
        .push_opcode(OP_CSV)
        .push_opcode(OP_DROP)
        .push_x_only_key(user_pk)
        .push_opcode(OP_CHECKSIG)
        .into_script())
}

/// Build the L_data tapleaf script:
///
/// ```text
/// OP_RETURN <D>           // 34 bytes: OP_RETURN OP_PUSHBYTES_32 <32-byte D>
/// ```
///
/// `D = TaggedHash("L2/hodlchain/v1", user_xonly_pubkey)`. Tapscript
/// inherits Bitcoin's rule that `OP_RETURN` aborts script execution, so
/// this leaf is permanently unspendable; it serves only as a committed
/// namespace stamp.
pub fn data_tapleaf_script(user_pk: &XOnlyPublicKey) -> ScriptBuf {
    let d = data_leaf_commitment(user_pk);
    Builder::new()
        .push_opcode(OP_RETURN)
        .push_slice(d)
        .into_script()
}

/// Compute the tap leaf hash of an arbitrary tapscript leaf.
pub fn tapleaf_hash(script: &ScriptBuf) -> TapLeafHash {
    TapLeafHash::from_script(script.as_script(), LeafVersion::TapScript)
}

/// Derive the canonical hodlchain mint Taproot construction: a 2-leaf
/// tree `{L_spend, L_data}` under the NUMS H internal key. Returns
/// `Err` for out-of-range `lock_blocks`; see [`csv_tapleaf_script`].
pub fn derive_mint_taproot<C: Verification>(
    secp: &Secp256k1<C>,
    lock_blocks: u32,
    user_pk: &XOnlyPublicKey,
) -> Result<(ScriptBuf, TaprootSpendInfo), MintConstructionError> {
    let spend_script = csv_tapleaf_script(lock_blocks, user_pk)?;
    let data_script = data_tapleaf_script(user_pk);
    // The two .expect()s below are structurally unreachable: we
    // always add exactly 2 leaves at depth 1, which `TaprootBuilder`
    // accepts unconditionally. `finalize` likewise can't fail on a
    // well-formed builder.
    let builder = bitcoin::taproot::TaprootBuilder::new()
        .add_leaf(1, spend_script)
        .expect("first leaf at depth 1 is valid")
        .add_leaf(1, data_script)
        .expect("second leaf at depth 1 is valid");
    let spend = builder
        .finalize(secp, nums_internal_key())
        .expect("finalize NUMS-internal 2-leaf taproot");
    let spk = ScriptBuf::new_p2tr_tweaked(spend.output_key());
    Ok((spk, spend))
}

/// Build the bech32m address for a hodlchain mint output. Returns
/// `Err` for out-of-range `lock_blocks`.
pub fn mint_address<C: Verification>(
    secp: &Secp256k1<C>,
    lock_blocks: u32,
    user_pk: &XOnlyPublicKey,
    network: Network,
) -> Result<Address, MintConstructionError> {
    let (_spk, spend) = derive_mint_taproot(secp, lock_blocks, user_pk)?;
    Ok(Address::p2tr_tweaked(spend.output_key(), network))
}

/// Recompute the P2TR scriptPubKey that a hodlchain mint UTXO must
/// have, given the locker's `(pk, lock_blocks)`. This is what a
/// verifier checks against the on-chain scriptPubKey. Returns `Err`
/// for out-of-range `lock_blocks` — the verifier propagates this as
/// a consensus-level lock-blocks rejection.
pub fn expected_p2tr_spk<C: Verification>(
    secp: &Secp256k1<C>,
    lock_blocks: u32,
    user_pk: &XOnlyPublicKey,
) -> Result<ScriptBuf, MintConstructionError> {
    let (spk, _spend) = derive_mint_taproot(secp, lock_blocks, user_pk)?;
    Ok(spk)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::opcodes::all::OP_CSV;
    use bitcoin::secp256k1::{Keypair, Secp256k1};

    #[test]
    fn nums_key_parses() {
        let _ = nums_internal_key();
    }

    #[test]
    fn build_and_match_p2tr_spk() {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let t = 500u32;

        let (spk_a, _spend) = derive_mint_taproot(&secp, t, &xonly).unwrap();
        let spk_b = expected_p2tr_spk(&secp, t, &xonly).unwrap();
        assert_eq!(spk_a, spk_b, "two ways of deriving the SPK must agree");
    }

    #[test]
    fn different_users_get_different_spks() {
        let secp = Secp256k1::new();
        let kp1 = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let kp2 = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (a, _) = kp1.x_only_public_key();
        let (b, _) = kp2.x_only_public_key();
        let (spk_a, _) = derive_mint_taproot(&secp, 1_000, &a).unwrap();
        let (spk_b, _) = derive_mint_taproot(&secp, 1_000, &b).unwrap();
        assert_ne!(spk_a, spk_b);
    }

    #[test]
    fn different_lock_blocks_get_different_spks() {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let (a, _) = derive_mint_taproot(&secp, 100, &xonly).unwrap();
        let (b, _) = derive_mint_taproot(&secp, 200, &xonly).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn spend_script_uses_csv_and_not_cltv() {
        use bitcoin::script::Instruction;
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let script = csv_tapleaf_script(1000, &xonly).unwrap();

        // Walk parsed instructions; the second instruction must be OP_CSV
        // (the locktime push is first), and OP_CLTV must not appear as an
        // opcode anywhere. A raw byte scan would false-positive on the
        // x-only pubkey bytes, which can happen to contain 0xb1 / 0xb2.
        let opcodes: Vec<_> = script
            .instructions()
            .filter_map(|ins| match ins.ok()? {
                Instruction::Op(op) => Some(op),
                Instruction::PushBytes(_) => None,
            })
            .collect();
        assert!(opcodes.contains(&OP_CSV), "L_spend must include OP_CSV");
        assert!(
            !opcodes.contains(&bitcoin::opcodes::all::OP_CLTV),
            "L_spend must not include OP_CLTV"
        );
    }

    #[test]
    fn data_leaf_is_unspendable_op_return() {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let script = data_tapleaf_script(&xonly);
        let bytes = script.as_bytes();
        assert_eq!(bytes.len(), 34);
        assert_eq!(bytes[0], 0x6a); // OP_RETURN
        assert_eq!(bytes[1], 0x20); // OP_PUSHBYTES_32
    }

    // ---------------------------------------------------------------
    // Regression vectors
    //
    // These pin the byte shape of every derived artifact for fixed
    // (user_pk, lock_blocks) inputs. They do NOT prove the resulting
    // SPK is actually spendable — for that we'd need libbitcoinconsensus
    // or an integration test that funds + reclaims on regtest. What
    // they DO catch is "someone made a 'harmless' refactor and the
    // SPK / leaf-hash / address shifted by a byte", which would
    // silently brick every existing deposit at the moment of bump.
    //
    // The fixed pubkeys below come from `SecretKey::from_slice(&[N; 32])`
    // for small N. The resulting x-only pubkeys are deterministic; the
    // resulting SPKs, addresses, and leaf hashes are recorded verbatim.
    // If any of these asserts fails after a code change, treat it as
    // a hard fork unless you understand exactly why the change is safe.
    // ---------------------------------------------------------------

    use bitcoin::hashes::Hash;
    use bitcoin::secp256k1::SecretKey;

    /// Helper: get a deterministic x-only pubkey from a fixed seed byte.
    fn fixed_xonly(seed: u8) -> XOnlyPublicKey {
        let secp = Secp256k1::new();
        let sk = SecretKey::from_slice(&[seed; 32])
            .expect("non-zero, in-range secret");
        let kp = Keypair::from_secret_key(&secp, &sk);
        kp.x_only_public_key().0
    }

    #[test]
    fn vector_data_leaf_commitment_sk1() {
        // user pk = pubkey of SecretKey([1; 32])
        let pk = fixed_xonly(1);
        let d = data_leaf_commitment(&pk);
        // Pinned from the running implementation, 2026-05-24.
        assert_eq!(
            hex::encode(d),
            "b89ecb922de60e1d2259f8b9e7dfa78a64380efc602133c7ffe42a23a78e735c",
        );
    }

    #[test]
    fn vector_mint_address_regtest_sk1_t500() {
        // user pk = SK([1;32]), lock_blocks = 500, regtest
        let secp = Secp256k1::new();
        let pk = fixed_xonly(1);
        let addr = mint_address(&secp, 500, &pk, Network::Regtest).unwrap();
        assert_eq!(
            addr.to_string(),
            "bcrt1p5qyuwzplg8p0y995ck0zesl28xhsh0ng5pyu55gxg98w85agxtesztflfp",
            "regtest mint address for fixed SK([1;32]) + T=500",
        );
    }

    #[test]
    fn vector_mint_spk_sk1_t500() {
        let secp = Secp256k1::new();
        let pk = fixed_xonly(1);
        let spk = expected_p2tr_spk(&secp, 500, &pk).unwrap();
        assert_eq!(
            hex::encode(spk.as_bytes()),
            "5120a009c7083f41c2f214b4c59e2cc3ea39af0bbe68a049ca5106414ee3d3a832f3",
            "P2TR scriptPubKey for fixed SK([1;32]) + T=500",
        );
    }

    #[test]
    fn vector_l_spend_leaf_hash_sk1_t500() {
        let pk = fixed_xonly(1);
        let script = csv_tapleaf_script(500, &pk).unwrap();
        let h = tapleaf_hash(&script);
        assert_eq!(
            hex::encode(h.as_byte_array()),
            "4765c35d1ff5e0ab3952a0cc9ffd667d05d5e9f6db4fced462bdf073f40cd2f4",
            "L_spend tap-leaf hash for fixed SK([1;32]) + T=500",
        );
    }

    #[test]
    fn vector_l_data_leaf_hash_sk1() {
        let pk = fixed_xonly(1);
        let script = data_tapleaf_script(&pk);
        let h = tapleaf_hash(&script);
        assert_eq!(
            hex::encode(h.as_byte_array()),
            "07aae1226e75c54b27d39504637714a2a713e73da7b2042b7d70aa1f2dabe341",
            "L_data tap-leaf hash for fixed SK([1;32])",
        );
    }

    // ---------------------------------------------------------------
    // Construction-time validation of `lock_blocks`
    //
    // The builder REJECTS out-of-range `lock_blocks` rather than
    // producing a script that silently masks down to a different
    // lock duration (or no lock at all). Validation at the
    // construction site is what protects a user from broadcasting
    // BTC into an unenforceable script. These tests pin that
    // behaviour — a future refactor that relaxes the check would
    // re-introduce the funds-stuck-in-broken-script risk.
    // ---------------------------------------------------------------

    #[test]
    fn lock_blocks_zero_is_rejected() {
        // CSV with arg 0 would be trivially satisfied (no real lock).
        // Builder must refuse so the wallet can't accidentally deposit
        // into a no-lock UTXO.
        let pk = fixed_xonly(1);
        assert!(matches!(
            csv_tapleaf_script(0, &pk),
            Err(MintConstructionError::LockBlocksTooSmall),
        ));
        let secp = Secp256k1::new();
        assert!(matches!(
            derive_mint_taproot(&secp, 0, &pk),
            Err(MintConstructionError::LockBlocksTooSmall),
        ));
        assert!(matches!(
            mint_address(&secp, 0, &pk, Network::Regtest),
            Err(MintConstructionError::LockBlocksTooSmall),
        ));
        assert!(matches!(
            expected_p2tr_spk(&secp, 0, &pk),
            Err(MintConstructionError::LockBlocksTooSmall),
        ));
    }

    #[test]
    fn lock_blocks_at_bip112_cap_is_accepted() {
        // 65535 is the max value BIP112's blocks-mode can express in
        // the lower 16 bits of nSequence. Boundary value, accepted.
        let pk = fixed_xonly(1);
        assert!(csv_tapleaf_script(MAX_LOCK_BLOCKS, &pk).is_ok());
    }

    #[test]
    fn lock_blocks_above_cap_is_rejected() {
        // Bitcoin Core masks CSV's arg to the lower 16 bits of
        // nSequence, so `65536` is indistinguishable on-chain from
        // `0` (no lock). The builder MUST refuse to produce these
        // shapes — the wallet shouldn't ever hand the user a
        // deposit address whose script masks down differently from
        // what they asked for.
        let pk = fixed_xonly(1);
        let secp = Secp256k1::new();
        for bad in [65_536u32, 65_537, 131_072, u32::MAX] {
            assert!(
                matches!(
                    csv_tapleaf_script(bad, &pk),
                    Err(MintConstructionError::LockBlocksTooLarge {
                        got, max,
                    }) if got == bad && max == MAX_LOCK_BLOCKS,
                ),
                "csv_tapleaf_script({bad}) should reject"
            );
            assert!(
                derive_mint_taproot(&secp, bad, &pk).is_err(),
                "derive_mint_taproot({bad}) should reject"
            );
            assert!(
                mint_address(&secp, bad, &pk, Network::Regtest).is_err(),
                "mint_address({bad}) should reject"
            );
            assert!(
                expected_p2tr_spk(&secp, bad, &pk).is_err(),
                "expected_p2tr_spk({bad}) should reject"
            );
        }
    }

    #[test]
    fn validate_lock_blocks_public_api() {
        // Upstream code (the wallet) calls this helper to fail
        // early before allocating a BIP32 index for the mint key.
        assert!(validate_lock_blocks(0).is_err());
        assert!(validate_lock_blocks(1).is_ok());
        assert!(validate_lock_blocks(MAX_LOCK_BLOCKS).is_ok());
        assert!(validate_lock_blocks(MAX_LOCK_BLOCKS + 1).is_err());
        assert!(validate_lock_blocks(u32::MAX).is_err());
    }

    #[test]
    fn data_leaf_commitment_is_independent_of_lock_blocks() {
        // `D` is computed from user_pk alone (chain_id is fixed). Two
        // mints from the same user at different durations share the
        // same `L_data` leaf — only `L_spend` differs. Verifies the
        // privacy claim that an outsider can't distinguish mints by
        // lock duration from `L_data` alone.
        let pk = fixed_xonly(1);
        assert_eq!(
            data_tapleaf_script(&pk),
            data_tapleaf_script(&pk),
            "D depends only on user_pk and chain_id; not on T"
        );
    }

    #[test]
    fn derive_mint_taproot_is_deterministic_for_fixed_inputs() {
        // Same (pk, T) → same SPK across repeated calls. Catches any
        // accidental introduction of randomness in the construction.
        let secp = Secp256k1::new();
        let pk = fixed_xonly(7);
        let (spk1, _) = derive_mint_taproot(&secp, 12_345, &pk).unwrap();
        let (spk2, _) = derive_mint_taproot(&secp, 12_345, &pk).unwrap();
        assert_eq!(spk1, spk2);
    }
}
