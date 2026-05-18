// TypeScript mirrors of the Rust types the backend serializes.
// Hand-maintained — keep in sync with hodl-wallet/src/{ops,wallet}.rs
// and hodl-core's serde shapes. A `cargo run --bin gen-bindings`-style
// generator would be nicer, but bare TS mirrors are tractable while
// the surface is small.

export type Network = "bitcoin" | "testnet" | "signet" | "regtest";

export type BitcoindAuth =
  | { kind: "cookie"; path: string }
  | { kind: "user_pass"; user: string; password: string };

export interface BitcoindConfig {
  url: string;
  auth: BitcoindAuth;
}

// ---------- ops::keygen ----------

export interface KeygenInput {
  network: Network;
  bitcoind: BitcoindConfig;
  sequencer_url: string;
  node_url?: string | null;
  esplora_url?: string | null;
  force: boolean;
}

export interface KeygenOutput {
  l2_address: string;     // x-only pubkey, 32-byte hex
  mnemonic: string;       // BIP39 24-word phrase
}

// ---------- mints (L1 side) ----------

export interface MintRecord {
  outpoint: string;       // "txid:vout"
  value_sat: number;
  lock_blocks: number;
  bip32_index: number;
  minted: boolean;
  reclaimed: boolean;
}

export interface MintUtxoInput {
  lock_blocks: number;
  value_btc: number;
}

export interface MintUtxoOutput {
  l1_tip: number;
  lock_blocks: number;
  mint_address: string;
  txid: string;
  vout: number;
  value_sat: number;
}

export interface MintMessageInput {
  outpoint: string;
  to?: string | null;     // L2 dest as x-only hex; null = own address
}

export interface MintMessageOutput {
  accepted: boolean;
  mint_amount?: number | null;
  nullifier_hex?: string | null;
  error?: string | null;
}

// ---------- transfer + balance ----------

export interface TransferInput {
  to: string;             // x-only pubkey hex
  amount: number;
}

export interface TransferOutput {
  accepted: boolean;
  error?: string | null;
}

export interface BalanceInput {
  addr?: string | null;   // x-only hex; null = own
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
}

// ---------- reclaim ----------

export type ReclaimStatus = "pending" | "locked" | "ready" | "reclaimed";

export interface ReclaimableMint {
  outpoint: string;
  value_sat: number;
  lock_blocks: number;
  bip32_index: number;
  minted: boolean;
  status: ReclaimStatus;
  funded_at_height?: number | null;
  blocks_remaining?: number | null;
}

export interface ReclaimMintInput {
  outpoint: string;
  dest_address: string;
  fee_sat: number;
}

export interface ReclaimMintOutput {
  txid: string;
  value_sat_in: number;
  value_sat_out: number;
  fee_sat: number;
}
