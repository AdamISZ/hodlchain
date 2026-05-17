//! L1-side helpers: BIP341 NUMS internal key, the two tapleaves
//! (`L_spend` and `L_data`), and the canonical 2-leaf taproot construction
//! used by every hodlcoin mint UTXO.
//!
//! See `docs/issuancev2.tex` §2 + §5.

use bitcoin::opcodes::all::{OP_CHECKSIG, OP_CSV, OP_DROP, OP_RETURN};
use bitcoin::script::Builder;
use bitcoin::secp256k1::{Secp256k1, Verification, XOnlyPublicKey};
use bitcoin::taproot::{LeafVersion, TaprootSpendInfo};
use bitcoin::{Address, Network, ScriptBuf, TapLeafHash};

use crate::consensus::{data_leaf_commitment, BIP341_NUMS_H_XONLY};

/// Return the BIP341 H NUMS x-only key used as the internal key for every
/// hodlcoin mint Taproot output.
pub fn nums_internal_key() -> XOnlyPublicKey {
    XOnlyPublicKey::from_slice(&BIP341_NUMS_H_XONLY)
        .expect("BIP341 H is a valid x-only key constant")
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
/// (bit 22) is 0 (blocks). The caller is responsible for keeping `T` in
/// the valid range.
pub fn csv_tapleaf_script(lock_blocks: u32, user_pk: &XOnlyPublicKey) -> ScriptBuf {
    Builder::new()
        .push_int(lock_blocks as i64)
        .push_opcode(OP_CSV)
        .push_opcode(OP_DROP)
        .push_x_only_key(user_pk)
        .push_opcode(OP_CHECKSIG)
        .into_script()
}

/// Build the L_data tapleaf script:
///
/// ```text
/// OP_RETURN <D>           // 34 bytes: OP_RETURN OP_PUSHBYTES_32 <32-byte D>
/// ```
///
/// `D = TaggedHash("L2/hodlcoin/v1", user_xonly_pubkey)`. Tapscript
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

/// Derive the canonical hodlcoin mint Taproot construction: a 2-leaf tree
/// `{L_spend, L_data}` under the NUMS H internal key.
pub fn derive_mint_taproot<C: Verification>(
    secp: &Secp256k1<C>,
    lock_blocks: u32,
    user_pk: &XOnlyPublicKey,
) -> (ScriptBuf, TaprootSpendInfo) {
    let spend_script = csv_tapleaf_script(lock_blocks, user_pk);
    let data_script = data_tapleaf_script(user_pk);
    let builder = bitcoin::taproot::TaprootBuilder::new()
        .add_leaf(1, spend_script)
        .expect("first leaf at depth 1 is valid")
        .add_leaf(1, data_script)
        .expect("second leaf at depth 1 is valid");
    let spend = builder
        .finalize(secp, nums_internal_key())
        .expect("finalize NUMS-internal 2-leaf taproot");
    let spk = ScriptBuf::new_p2tr_tweaked(spend.output_key());
    (spk, spend)
}

/// Build the bech32m address for a hodlcoin mint output.
pub fn mint_address<C: Verification>(
    secp: &Secp256k1<C>,
    lock_blocks: u32,
    user_pk: &XOnlyPublicKey,
    network: Network,
) -> Address {
    let (_spk, spend) = derive_mint_taproot(secp, lock_blocks, user_pk);
    Address::p2tr_tweaked(spend.output_key(), network)
}

/// Recompute the P2TR scriptPubKey that a hodlcoin mint UTXO must have,
/// given the locker's `(pk, lock_blocks)`. This is what a verifier checks
/// against the on-chain scriptPubKey.
pub fn expected_p2tr_spk<C: Verification>(
    secp: &Secp256k1<C>,
    lock_blocks: u32,
    user_pk: &XOnlyPublicKey,
) -> ScriptBuf {
    let (spk, _spend) = derive_mint_taproot(secp, lock_blocks, user_pk);
    spk
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

        let (spk_a, _spend) = derive_mint_taproot(&secp, t, &xonly);
        let spk_b = expected_p2tr_spk(&secp, t, &xonly);
        assert_eq!(spk_a, spk_b, "two ways of deriving the SPK must agree");
    }

    #[test]
    fn different_users_get_different_spks() {
        let secp = Secp256k1::new();
        let kp1 = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let kp2 = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (a, _) = kp1.x_only_public_key();
        let (b, _) = kp2.x_only_public_key();
        let (spk_a, _) = derive_mint_taproot(&secp, 1_000, &a);
        let (spk_b, _) = derive_mint_taproot(&secp, 1_000, &b);
        assert_ne!(spk_a, spk_b);
    }

    #[test]
    fn different_lock_blocks_get_different_spks() {
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let (a, _) = derive_mint_taproot(&secp, 100, &xonly);
        let (b, _) = derive_mint_taproot(&secp, 200, &xonly);
        assert_ne!(a, b);
    }

    #[test]
    fn spend_script_uses_csv_and_not_cltv() {
        use bitcoin::script::Instruction;
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (xonly, _) = kp.x_only_public_key();
        let script = csv_tapleaf_script(1000, &xonly);

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
}
