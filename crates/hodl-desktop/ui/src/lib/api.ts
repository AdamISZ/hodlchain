// Typed wrappers around Tauri commands. The single place the rest of
// the app talks to the Rust backend. Anything that wants to call
// hodl_wallet::ops::* goes through here.

import { invoke } from "@tauri-apps/api/core";
import type {
  KeygenInput,
  KeygenOutput,
  MintRecord,
  MintUtxoInput,
  MintUtxoOutput,
  MintMessageInput,
  MintMessageOutput,
  TransferInput,
  TransferOutput,
  BalanceInput,
  BalanceOutput,
  LightBalanceInput,
  LightBalanceOutput,
  ReclaimableMint,
  ReclaimMintInput,
  ReclaimMintOutput,
} from "./types";

// ---------- Session / wallet existence ----------

export const walletPath = (): Promise<string> => invoke("wallet_path");

export const walletExists = (): Promise<boolean> => invoke("wallet_exists");

// ---------- Keys / addresses ----------

export const keygen = (input: KeygenInput): Promise<KeygenOutput> =>
  invoke("keygen", { input });

export const address = (): Promise<string> => invoke("address");

// ---------- L1 mints ----------

export const listMints = (): Promise<MintRecord[]> => invoke("list_mints");

export const mintUtxo = (input: MintUtxoInput): Promise<MintUtxoOutput> =>
  invoke("mint_utxo", { input });

export const mintMessage = (
  input: MintMessageInput,
): Promise<MintMessageOutput> => invoke("mint_message", { input });

// ---------- Balance / transfer ----------

export const balance = (input: BalanceInput): Promise<BalanceOutput> =>
  invoke("balance", { input });

export const transfer = (input: TransferInput): Promise<TransferOutput> =>
  invoke("transfer", { input });

// ---------- Light verification (the sparse-stateless path) ----------

export const lightBalance = (
  input: LightBalanceInput,
): Promise<LightBalanceOutput> => invoke("light_balance", { input });

// ---------- Reclaim ----------

export const listReclaimableMints = (): Promise<ReclaimableMint[]> =>
  invoke("list_reclaimable_mints");

export const reclaimMint = (
  input: ReclaimMintInput,
): Promise<ReclaimMintOutput> => invoke("reclaim_mint", { input });
