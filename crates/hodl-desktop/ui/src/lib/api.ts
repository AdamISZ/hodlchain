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
  CheckMintFundingInput,
  CheckMintFundingOutput,
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

// ---------- Wallet management ----------

export const listWallets = (): Promise<string[]> => invoke("list_wallets");

export const currentWallet = (): Promise<string | null> =>
  invoke("current_wallet");

export const selectWallet = (name: string): Promise<void> =>
  invoke("select_wallet", { name });

export const deselectWallet = (): Promise<void> => invoke("deselect_wallet");

// ---------- Keys / addresses ----------

export const keygen = (input: KeygenInput): Promise<KeygenOutput> =>
  invoke("keygen", { input });

export const address = (): Promise<string> => invoke("address");

// ---------- L1 mints ----------

export const listMints = (): Promise<MintRecord[]> => invoke("list_mints");

export const mintUtxo = (input: MintUtxoInput): Promise<MintUtxoOutput> =>
  invoke("mint_utxo", { input });

export const checkMintFunding = (
  input: CheckMintFundingInput,
): Promise<CheckMintFundingOutput> => invoke("check_mint_funding", { input });

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
