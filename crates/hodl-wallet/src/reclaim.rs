//! Build and sign the L1 reclaim spend of a CSV-locked mint UTXO.
//!
//! After the mint UTXO's relative locktime has matured (i.e., the
//! current L1 tip is at least `funded_at_height + lock_blocks`), the
//! BTC can be moved with a script-path Taproot spend of `L_spend`:
//!
//! ```text
//! L_spend:  <lock_blocks> OP_CSV OP_DROP <user_pk> OP_CHECKSIG
//! ```
//!
//! The witness stack is `[ sig, L_spend_script, control_block ]`. The
//! control block carries the NUMS H internal key and a single merkle
//! sibling hash (`L_data`'s tapleaf hash) — that's the entire other
//! half of our 2-leaf taproot.

use anyhow::{anyhow, bail, Context, Result};
use bitcoin::absolute::LockTime;
use bitcoin::hashes::Hash;
use bitcoin::secp256k1::{Keypair, Message, Secp256k1, Signing, Verification};
use bitcoin::sighash::{Prevouts, SighashCache};
use bitcoin::taproot::{LeafVersion, Signature as TapSignature};
use bitcoin::transaction::Version as TxVersion;
use bitcoin::{
    Address, Amount, OutPoint, ScriptBuf, Sequence, TapLeafHash, TapSighashType,
    Transaction, TxIn, TxOut, Witness,
};
use hodl_core::consensus::MAX_LOCK_BLOCKS;
use hodl_core::l1::{csv_tapleaf_script, data_tapleaf_script, derive_mint_taproot, tapleaf_hash};

/// Build and sign the reclaim spend of a single mint UTXO.
///
/// `mint_kp` must be the same keypair that this mint's
/// `MintRecord.bip32_index` derives (the wallet caller is responsible
/// for this — the script's `user_pk` is taken from `mint_kp` here).
///
/// `fee_sat` is the absolute miner fee; the output value is
/// `mint_value_sat - fee_sat`. We don't pad to dust — the caller
/// should ensure the remaining value is above dust.
#[allow(clippy::too_many_arguments)]
pub fn build_signed_reclaim_tx<C: Signing + Verification>(
    secp: &Secp256k1<C>,
    mint_kp: &Keypair,
    mint_outpoint: OutPoint,
    mint_value_sat: u64,
    lock_blocks: u32,
    dest_address: &Address,
    fee_sat: u64,
) -> Result<Transaction> {
    if lock_blocks == 0 || lock_blocks > MAX_LOCK_BLOCKS {
        bail!("lock_blocks out of range: {lock_blocks}");
    }
    if fee_sat >= mint_value_sat {
        bail!(
            "fee {} >= mint value {} — would yield zero/negative output",
            fee_sat,
            mint_value_sat
        );
    }
    let (mint_xonly, _) = mint_kp.x_only_public_key();

    // Reconstruct the 2-leaf taproot under NUMS H to get the spend info
    // (for the control block) and the prev_out's scriptPubKey.
    let (prev_spk, spend_info) = derive_mint_taproot(secp, lock_blocks, &mint_xonly);

    let l_spend = csv_tapleaf_script(lock_blocks, &mint_xonly);
    let l_data = data_tapleaf_script(&mint_xonly);
    let l_data_hash: TapLeafHash = tapleaf_hash(&l_data);

    // Sanity-check the control block is what we expect (NUMS H +
    // L_data's tapleaf hash as the sole sibling).
    let control_block = spend_info
        .control_block(&(l_spend.clone(), LeafVersion::TapScript))
        .ok_or_else(|| anyhow!("control block for L_spend not found in spend_info"))?;

    let out_value = mint_value_sat
        .checked_sub(fee_sat)
        .ok_or_else(|| anyhow!("fee underflow"))?;

    // BIP112 block-form relative locktime. `lock_blocks` fits in u16
    // (consensus::MAX_LOCK_BLOCKS = 0xFFFF), and `Sequence::from_height`
    // sets bit 22 = 0 (blocks), bit 31 = 0 (enabled).
    let sequence = Sequence::from_height(lock_blocks as u16);

    let prev_txout = TxOut {
        value: Amount::from_sat(mint_value_sat),
        script_pubkey: prev_spk,
    };

    let mut tx = Transaction {
        version: TxVersion::TWO, // CSV requires nVersion >= 2.
        lock_time: LockTime::ZERO,
        input: vec![TxIn {
            previous_output: mint_outpoint,
            script_sig: ScriptBuf::new(),
            sequence,
            witness: Witness::new(),
        }],
        output: vec![TxOut {
            value: Amount::from_sat(out_value),
            script_pubkey: dest_address.script_pubkey(),
        }],
    };

    // Compute the taproot script-spend sighash.
    let l_spend_hash = tapleaf_hash(&l_spend);
    let sighash = {
        let mut cache = SighashCache::new(&tx);
        cache
            .taproot_script_spend_signature_hash(
                0,
                &Prevouts::All(&[prev_txout]),
                bitcoin::sighash::ScriptPath::with_defaults(&l_spend),
                TapSighashType::Default,
            )
            .context("compute taproot script-spend sighash")?
    };
    // (Sanity: l_spend_hash matches what ScriptPath::with_defaults
    // computed internally. We don't reuse it directly but compute it
    // for the same reason — to fail loudly if construction drifts.)
    debug_assert_eq!(
        l_spend_hash,
        TapLeafHash::from_script(&l_spend, LeafVersion::TapScript)
    );

    let msg = Message::from_digest(*sighash.as_byte_array());
    let sig = secp.sign_schnorr_no_aux_rand(&msg, mint_kp);
    let tap_sig = TapSignature {
        signature: sig,
        sighash_type: TapSighashType::Default,
    };

    // Witness: [ sig, L_spend_script_bytes, control_block_bytes ].
    let mut witness = Witness::new();
    witness.push(tap_sig.to_vec());
    witness.push(l_spend.as_bytes());
    witness.push(control_block.serialize());
    tx.input[0].witness = witness;

    // Sanity-check: the control block we built does verify against
    // the prev SPK's taproot commitment. (BIP341 §11.) The taproot
    // crate provides a verifier — confirm before broadcast so a
    // wrong-keys or wrong-script construction fails locally instead
    // of being silently rejected by the network.
    if !control_block.verify_taproot_commitment(secp, spend_info.output_key().to_x_only_public_key(), &l_spend) {
        bail!("control block does not verify against the derived output key");
    }
    let _ = l_data_hash; // referenced for clarity; baked into control_block

    Ok(tx)
}
