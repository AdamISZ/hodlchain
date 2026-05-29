// Client-side bech32m L2 address validation. Mirror of
// hodl-core's `address` module — kept tight so the same checksum/HRP
// rules apply in the form before the backend round-trips.
//
// The canonical implementation lives in Rust (`hodl-core::address`).
// This TS port exists so the Transfer form can validate synchronously
// while the user types, instead of round-tripping every keystroke
// through Tauri. If the two ever drift, the backend is authoritative —
// the worst the UI can do is reject something the backend would accept
// (annoying but not a footgun).

import { bech32m } from "bech32";
import type { Network } from "./types";

const HRP_MAINNET = "hc";
const HRP_TEST = "thc"; // testnet + signet share an HRP
const HRP_REGTEST = "hcrt";

export type AddressClass = "mainnet" | "test" | "regtest";

export function addressClassOf(net: Network): AddressClass {
  switch (net) {
    case "bitcoin":
      return "mainnet";
    case "testnet":
    case "signet":
      return "test";
    case "regtest":
      return "regtest";
  }
}

function hrpFor(klass: AddressClass): string {
  switch (klass) {
    case "mainnet":
      return HRP_MAINNET;
    case "test":
      return HRP_TEST;
    case "regtest":
      return HRP_REGTEST;
  }
}

function classFromHrp(hrp: string): AddressClass | null {
  switch (hrp) {
    case HRP_MAINNET:
      return "mainnet";
    case HRP_TEST:
      return "test";
    case HRP_REGTEST:
      return "regtest";
    default:
      return null;
  }
}

export type DecodeResult =
  | { ok: true; pubkeyHex: string; klass: AddressClass }
  | { ok: false; error: string };

/**
 * Decode a bech32m L2 address. Returns the 32-byte payload as hex and
 * the address-class encoded in the HRP. Does not check network
 * agreement — use `decodeForNetwork` for that.
 */
export function decode(s: string): DecodeResult {
  let decoded;
  try {
    // bech32m.decode throws on bad checksum / mixed case / unsupported chars.
    decoded = bech32m.decode(s.trim());
  } catch (e) {
    return { ok: false, error: `bech32m decode failed: ${e instanceof Error ? e.message : e}` };
  }
  const klass = classFromHrp(decoded.prefix);
  if (klass === null) {
    return {
      ok: false,
      error: `unknown HRP ${JSON.stringify(decoded.prefix)}; expected one of hc, thc, hcrt`,
    };
  }
  // 5-bit groups → 8-bit bytes. The reference impl uses bech32m.fromWords;
  // expect exactly 32 bytes for an x-only pubkey payload.
  const bytes = bech32m.fromWords(decoded.words);
  if (bytes.length !== 32) {
    return { ok: false, error: `payload must be 32 bytes, got ${bytes.length}` };
  }
  const pubkeyHex = Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return { ok: true, pubkeyHex, klass };
}

/**
 * Decode and check the address matches the wallet's network class.
 * Testnet and signet are interchangeable (same HRP). Mainnet and
 * regtest are isolated.
 */
export function decodeForNetwork(
  s: string,
  net: Network,
): DecodeResult {
  const r = decode(s);
  if (!r.ok) return r;
  const expected = addressClassOf(net);
  if (r.klass !== expected) {
    return {
      ok: false,
      error: `wrong network: expected ${hrpFor(expected)}1…, got ${hrpFor(r.klass)}1…`,
    };
  }
  return r;
}
