// TypeScript mirror of hodl-core::consensus. Kept tiny on purpose —
// only the bits the UI needs (mint formula + fixed r).
// MUST stay in sync with crates/hodl-core/src/consensus.rs.

/**
 * One year in Bitcoin blocks at the 10-minute target interval.
 * `6 blocks/hour × 24 × 365 = 52_560`.
 */
export const BLOCKS_PER_YEAR = 52_560;

/**
 * The fixed mint-function rate parameter, in 1 / L1-block. Inflection
 * of `mint_fn` sits at `T = 1 year`. No retargeting in this design.
 */
export const R = 1 / BLOCKS_PER_YEAR;

/**
 * f_mint(V, T) = V * (1 - (1 + rT) * e^{-rT}), with r = R fixed.
 *
 * `valueSat` is the BTC value locked (sat). `lockBlocks` is T (the gap
 * between the funding L1 block and the CSV unlock). Returns the L2
 * atom amount.
 *
 * Matches the Rust implementation: clamps the ratio into [0, 1) and
 * floors the result so the JS preview agrees with what the sequencer
 * will credit. (ATOMS_PER_SAT = 1 for the POC so the multiplication
 * is dropped here.)
 */
export function mintFn(valueSat: number, lockBlocks: number): number {
  if (lockBlocks === 0 || valueSat === 0) return 0;
  const rt = R * lockBlocks;
  let ratio = 1 - (1 + rt) * Math.exp(-rt);
  // Defensive clamp — matches the Rust `clamp(0.0, 1.0 - f64::EPSILON)`.
  if (ratio < 0) ratio = 0;
  if (ratio > 1 - Number.EPSILON) ratio = 1 - Number.EPSILON;
  return Math.floor(valueSat * ratio);
}

/** Per-transfer protocol fee in basis points. */
export const FEE_BPS = 1;
/** Minimum per-transfer fee in atoms (floor when 1 bp rounds to zero). */
export const MIN_FEE = 100;

/**
 * Predict the fee the chain will deduct from a transfer of `amount`
 * atoms. Mirror of hodl_core::state::apply_transfer's formula; UIs
 * use this to show "amount + fee = total" before submit so users
 * don't get surprised by the post-submit deduction.
 */
export function transferFee(amount: number): number {
  return Math.max(MIN_FEE, Math.floor((amount * FEE_BPS) / 10_000));
}
