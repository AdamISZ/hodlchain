//! Network-wide consensus constants and the mint function.
//!
//! See `~/code/hodlchain-paper/hodlchainv2.tex` for the derivation of
//! `mint_fn` and the two-leaf taproot construction (NUMS internal key
//! + L_spend + L_data).

use bitcoin::secp256k1::XOnlyPublicKey;
use sha2::{Digest, Sha256};

/// hodlchain chain identifier. Used to derive the tagged-hash tag that
/// binds each mint UTXO's L_data leaf to this L2's namespace.
///
/// Single value across networks: cross-network UTXO reuse is impossible
/// because regtest/signet/mainnet have disjoint chain histories.
pub const CHAIN_ID: &str = "hodlchain";

/// BIP340 tagged-hash tag for the `L_data` payload `D`.
///
/// `D = TaggedHash(DATA_LEAF_TAG, user_xonly_pubkey)`.
pub const DATA_LEAF_TAG: &str = "L2/hodlchain/v1";

/// Compute `D = TaggedHash(DATA_LEAF_TAG, user_xonly_pubkey)`.
///
/// BIP340 tagged hash: `sha256(sha256(tag) || sha256(tag) || data)`.
pub fn data_leaf_commitment(user_pk: &XOnlyPublicKey) -> [u8; 32] {
    let tag_hash = Sha256::digest(DATA_LEAF_TAG.as_bytes());
    let mut h = Sha256::new();
    h.update(tag_hash);
    h.update(tag_hash);
    h.update(user_pk.serialize());
    h.finalize().into()
}

/// Magic bytes prefixing every hodlchain OP_RETURN attestation.
pub const MAGIC: [u8; 4] = *b"HODL";

/// Attestation payload version byte. Bumped on any layout change.
pub const ATTESTATION_VERSION: u8 = 0;

/// Total attestation payload size in bytes.
///
///   magic(4) + version(1) + height(4) + l2_block_hash(32) + state_root(32)
pub const ATTESTATION_LEN: usize = 4 + 1 + 4 + 32 + 32;

/// BIP341 `H`, the recommended NUMS ("nothing-up-my-sleeve") x-only
/// public key. Used as the internal key of every hodlchain mint
/// Taproot output, so the only spend path is the CSV tapleaf
/// `L_spend` (no key-path escape).
///
/// Source: <https://github.com/bitcoin/bips/blob/master/bip-0341.mediawiki>
/// — "Constructing and spending Taproot outputs" section. BIP341
/// specifies `H = lift_x(0x50929b74c1a04954b78b4b6035e97a5e078a5a0f28ec96d547bfee9ace803ac0)`,
/// where the x-coordinate is the SHA256 hash of the standard
/// (uncompressed) encoding of the secp256k1 generator point G. That
/// construction is what makes `H` provably-no-known-discrete-log
/// w.r.t. G: anyone who could derive `dlog_G(H)` would also have
/// inverted SHA256.
///
/// The 32 bytes below are the raw x-coordinate (big-endian).
/// `rust-bitcoin` doesn't expose this as a constant, so we keep our
/// own copy; the value is fixed by the BIP and will not change.
pub const BIP341_NUMS_H_XONLY: [u8; 32] = [
    0x50, 0x92, 0x9b, 0x74, 0xc1, 0xa0, 0x49, 0x54, 0xb7, 0x8b, 0x4b, 0x60, 0x35, 0xe9, 0x7a, 0x5e,
    0x07, 0x8a, 0x5a, 0x0f, 0x28, 0xec, 0x96, 0xd5, 0x47, 0xbf, 0xee, 0x9a, 0xce, 0x80, 0x3a, 0xc0,
];

/// One year measured in Bitcoin blocks, assuming a 10-minute target
/// block interval: `6 blocks/hour × 24 × 365 = 52_560`.
///
/// This is the inflection point of `mint_fn`: lock durations below
/// this are in the convex (anti-splitting) regime; longer locks are
/// in the concave (diminishing-return) regime.
pub const BLOCKS_PER_YEAR: u32 = 52_560;

/// The rate parameter of `mint_fn`, in units of 1 / L1-block.
///
/// Fixed at `1 / BLOCKS_PER_YEAR`, i.e. the inflection point of the
/// mint sigmoid is at `T = 1 year`. There is no retargeting in this
/// design — `r` is a hard-coded consensus constant. The paper's
/// trilemma argument (anti-gaming, immediate-mint, supply-control:
/// pick two) motivates dropping supply-rate control in exchange for
/// the other two; see §"Setting r" of `hodlchainv2.tex`.
pub const R: f64 = 1.0 / (BLOCKS_PER_YEAR as f64);

/// L2 native-token atomic unit per BTC sat. The mint function returns
/// L2 atoms; we use a 1:1 mapping for the POC so that f(V,T) <= V trivially.
pub const ATOMS_PER_SAT: u64 = 1;

/// Per-transfer protocol fee, in basis points (hundredths of a
/// percent). Computed as `amount * FEE_BPS / 10_000`. Paid to the
/// sequencer's L2 fee address.
///
/// 1 bp = 0.01%. Anti-DoS-first, revenue-second — at this rate a
/// transfer of 1M atoms costs 100 atoms in fee, enough to make spam
/// economically meaningful but cheap for any real use case.
pub const FEE_BPS: u64 = 1;

/// Floor on the per-transfer protocol fee, in atoms. Ensures that
/// even very small transfers pay a non-zero fee (preventing
/// zero-fee spam at the low end where `amount * FEE_BPS / 10_000`
/// rounds to zero).
///
/// `fee = max(MIN_FEE, amount * FEE_BPS / 10_000)`.
pub const MIN_FEE: u64 = 100;

/// Number of L1 confirmations required before a mint message is credited.
pub const MINT_CONFIRMATIONS: u32 = 1;

/// Maximum allowed CSV relative locktime, in blocks. BIP112's block-based
/// form encodes the value in the lower 16 bits of `nSequence`, capping it
/// at 65535 blocks (~454 days). T = 0 disables the locktime (CSV no-op),
/// so we require T >= 1 as well.
///
/// At `R = 1/BLOCKS_PER_YEAR`, this cap corresponds to `rT ≈ 1.247`,
/// just past the inflection — meaning every practical lock duration
/// in the POC sits within or just-past the convex regime. A
/// production deployment that wants longer locks would need
/// BIP118-style ANYPREVOUT or BIP119 CTV to break out of BIP112's
/// 16-bit `nSequence` window; out of scope here.
pub const MAX_LOCK_BLOCKS: u32 = 0xFFFF;

/// f_mint(V, T) = V * (1 - (1 + rT) e^{-rT}), with `r = R` fixed.
///
/// `value_sat` is the BTC value locked. `lock_blocks` is T (the gap between
/// the funding L1 block and the CSV unlock height). Returns the L2 token
/// amount, in atoms.
pub fn mint_fn(value_sat: u64, lock_blocks: u32) -> u64 {
    if lock_blocks == 0 || value_sat == 0 {
        return 0;
    }
    let rt = R * (lock_blocks as f64);
    let ratio = 1.0 - (1.0 + rt) * libm::exp(-rt);
    // Clamp into [0, 1) defensively.
    let ratio = ratio.clamp(0.0, 1.0 - f64::EPSILON);
    let atoms = (value_sat as f64) * ratio * (ATOMS_PER_SAT as f64);
    libm::floor(atoms) as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use bitcoin::secp256k1::{Keypair, Secp256k1};

    #[test]
    fn data_leaf_commitment_is_keyed_by_pk() {
        let secp = Secp256k1::new();
        let kp_a = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let kp_b = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (pk_a, _) = kp_a.x_only_public_key();
        let (pk_b, _) = kp_b.x_only_public_key();
        let d_a = data_leaf_commitment(&pk_a);
        let d_b = data_leaf_commitment(&pk_b);
        assert_ne!(d_a, d_b, "different pubkeys must produce different D");
        // Determinism
        assert_eq!(data_leaf_commitment(&pk_a), d_a);
    }

    #[test]
    fn data_leaf_commitment_matches_bip340_tagged_hash() {
        // Spot-check the BIP340 construction by comparing against an
        // independent re-implementation.
        let secp = Secp256k1::new();
        let kp = Keypair::new(&secp, &mut bitcoin::secp256k1::rand::thread_rng());
        let (pk, _) = kp.x_only_public_key();
        let tag_h = Sha256::digest(DATA_LEAF_TAG.as_bytes());
        let mut hh = Sha256::new();
        hh.update(tag_h);
        hh.update(tag_h);
        hh.update(pk.serialize());
        let expected: [u8; 32] = hh.finalize().into();
        assert_eq!(data_leaf_commitment(&pk), expected);
    }

    #[test]
    fn mint_fn_zero_t_is_zero() {
        assert_eq!(mint_fn(100_000_000, 0), 0);
    }

    #[test]
    fn mint_fn_bounded_by_v() {
        let v = 100_000_000u64;
        // Very long lock: ratio approaches 1 but never reaches it.
        let out = mint_fn(v, 10_000_000);
        assert!(out < v, "mint must be strictly less than V even at huge T");
    }

    #[test]
    fn mint_fn_superlinear_short() {
        // Doubling T near the origin should more than double the reward
        // (anti-splitting / convex regime). For demo r=1/52560, T in the
        // single-thousands of blocks is comfortably below the inflection
        // at T = 1 year.
        let v = 100_000_000u64;
        let a = mint_fn(v, 1_000);
        let b = mint_fn(v, 2_000);
        assert!(2 * a <= b + 1, "expected superlinearity for short T: 2*{} <= {}", a, b);
    }

    #[test]
    fn mint_fn_monotone_in_t() {
        let v = 100_000_000u64;
        let mut prev = 0u64;
        for t in [100u32, 500, 1000, 5000, BLOCKS_PER_YEAR, MAX_LOCK_BLOCKS] {
            let cur = mint_fn(v, t);
            assert!(cur >= prev, "mint_fn must be non-decreasing in T");
            prev = cur;
        }
    }

    #[test]
    fn inflection_point_at_one_year() {
        // sanity check: rT = 1 at T = BLOCKS_PER_YEAR
        let rt = R * (BLOCKS_PER_YEAR as f64);
        assert!((rt - 1.0).abs() < 1e-12);
    }
}
