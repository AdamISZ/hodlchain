// TypeScript mirrors of the Rust types the backend serializes.
// Hand-maintained — keep in sync with hodl-wallet/src/{ops,wallet}.rs
// and hodl-core's serde shapes. A `cargo run --bin gen-bindings`-style
// generator would be nicer, but bare TS mirrors are tractable while
// the surface is small.

export type Network = "bitcoin" | "testnet" | "signet" | "regtest";

// ---------- ops::keygen ----------

/**
 * Backend-flattened shape: `name` selects the wallet filename
 * (`<name>.json` in the wallets dir); the rest is the inner
 * `ops::KeygenInput`. We mirror the flat structure here.
 */
export interface KeygenInput {
  name: string;
  network: Network;
  sequencer_url: string;
  node_url?: string | null;
  /** Required. Mempool.space / electrs / hodl-node URL. */
  esplora_url: string;
  /**
   * Optional BIP39 mnemonic to *restore* from. When omitted/null,
   * the backend generates a fresh 24-word phrase. When supplied,
   * the backend validates it (full checksum check via the bip39
   * crate) and uses it to derive the wallet's keys.
   */
  mnemonic?: string | null;
  force: boolean;
}

export interface KeygenOutput {
  l2_address: string;     // x-only pubkey, 32-byte hex
  mnemonic: string;       // BIP39 phrase (echoed back)
  was_fresh: boolean;     // true = newly generated, false = restored from input
}

// ---------- mints ----------

export interface MintRecord {
  /** Bech32m P2TR deposit address — stable identifier in the UI. */
  mint_address: string;
  lock_blocks: number;
  bip32_index: number;
  /** "txid:vout"; populated once a funding UTXO is observed. */
  outpoint?: string | null;
  value_sat?: number | null;
  funded_at_height?: number | null;
  minted: boolean;
  reclaimed: boolean;
}

export interface MintUtxoInput {
  lock_blocks: number;
}

export interface MintUtxoOutput {
  bip32_index: number;
  lock_blocks: number;
  /** The deposit address to show the user. */
  mint_address: string;
}

export type MintFundingState = "unfunded" | "pending" | "confirmed";

export interface CheckMintFundingInput {
  bip32_index: number;
}

export interface CheckMintFundingOutput {
  bip32_index: number;
  mint_address: string;
  state: MintFundingState;
  outpoint?: string | null;
  value_sat?: number | null;
  funded_at_height?: number | null;
}

export interface MintMessageInput {
  bip32_index: number;
  to?: string | null;
}

export interface MintMessageOutput {
  accepted: boolean;
  mint_amount?: number | null;
  nullifier_hex?: string | null;
  error?: string | null;
  /** Sequencer-signed soft-confirmation receipt. Present on accept. */
  soft_conf?: SoftConf | null;
}

// ---------- transfer + balance ----------

export interface TransferInput {
  to: string;
  amount: number;
}

export interface TransferOutput {
  accepted: boolean;
  error?: string | null;
  /** Protocol fee deducted (`max(MIN_FEE, amount * FEE_BPS / 10_000)`). */
  fee: number;
  /** `amount + fee` — what's deducted from the sender's balance. */
  total: number;
  /** Sequencer-signed soft-confirmation receipt. Present on accept. */
  soft_conf?: SoftConf | null;
}

/**
 * Sequencer-signed promise that an accepted tx will land in L2
 * block `target_l2_height`. Mirror of hodl_core::rpc::SoftConf.
 */
export interface SoftConf {
  tx_hash: string;
  target_l2_height: number;
  accepted_at_unix: number;
  /** 64-byte hex Schnorr sig over the canonical sighash. */
  sequencer_sig: string;
}

export interface BalanceInput {
  addr?: string | null;
}

export interface BalanceOutput {
  address: string;
  balance: number;
  nonce: number;
}

// ---------- light verification ----------

export type LightBalanceMode = "cold_start" | "warm_start";

export interface LightBalanceInput {
  addr?: string | null;
}

export interface LightBalanceOutput {
  mode: LightBalanceMode;
  blocks_verified: number;
  l2_height: number;
  state_root: string;
  accounts_root: string;
  block_hash: string;
  l1_height: number;
  address: string;
  balance: number;
  nonce: number;
  is_own_address: boolean;
  /**
   * Total atoms ever minted. Light-verified on warm-start; on cold-start
   * the bootstrap snapshot value is sequencer-trusted.
   */
  total_minted_atoms: number;
}

// ---------- reclaim ----------

export type ReclaimStatus = "pending" | "locked" | "ready" | "reclaimed";

export interface ReclaimableMint {
  bip32_index: number;
  mint_address: string;
  lock_blocks: number;
  outpoint?: string | null;
  value_sat?: number | null;
  funded_at_height?: number | null;
  minted: boolean;
  status: ReclaimStatus;
  blocks_remaining?: number | null;
}

export interface ReclaimMintInput {
  bip32_index: number;
  dest_address: string;
  fee_sat: number;
}

export interface ReclaimMintOutput {
  txid: string;
  value_sat_in: number;
  value_sat_out: number;
  fee_sat: number;
}
