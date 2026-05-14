//! TypeScript mirror of the on-chain `ario-ant-escrow` canonical claim
//! message builders. Single source of TS truth for the bytes the
//! off-chain attestor signs and the on-chain program reconstructs to
//! verify Ed25519 attestations (ADR-017).
//!
//! Pinned byte-for-byte to:
//!   `programs/ario-ant-escrow/src/canonical.rs`
//!
//! The cross-language byte parity is asserted by
//! `clients/ts/test/canonical.cross.test.ts`, which runs the Rust
//! `cargo build --example canonical -p ario-ant-escrow` binary against
//! the same vectors and compares the output. If those two ever drift,
//! every attestation will fail to verify on-chain even though it's
//! valid in the abstract sense, so we keep both implementations under
//! one roof here and assert parity on every test run.
//!
//! Two formats, one per claim category — different message shapes, not
//! different versions of the same shape:
//!   - `buildAntEscrowClaimMessage` — ANT-escrow claims (mirrors Rust
//!     `build_ant_escrow_claim_message`).
//!   - `buildEscrowClaimMessage` — token / vault escrow claims (mirrors
//!     Rust `build_escrow_claim_message`).

import bs58 from "bs58";
import { sha256 } from "@noble/hashes/sha2";

/** Header anchoring the ANT-escrow claim message format. */
const ANT_ESCROW_CLAIM_HEADER = "ar.io ant-escrow claim";

/** Header anchoring the token + vault escrow claim message format. */
const ESCROW_CLAIM_HEADER = "ar.io escrow claim";

/**
 * SHA-256 the recipient's identity bytes (RSA modulus for Arweave,
 * 20-byte address for Ethereum) and base64url-encode (no padding).
 *
 * Pinned byte-for-byte to the on-chain
 * `canonical::derive_recipient_id_b64url` — closes F-1 (the off-chain
 * attestor was previously not binding the user-supplied RSA modulus
 * to the on-chain `escrow.recipient_pubkey`). See
 * `docs/ATTESTOR_SECURITY_REVIEW.md` in this repo.
 */
export function deriveRecipientIdB64Url(
  recipientPubkeyActive: Uint8Array,
): string {
  const digest = sha256(recipientPubkeyActive);
  return bytesToBase64UrlNoPad(digest);
}

function bytesToBase64UrlNoPad(bytes: Uint8Array): string {
  // Node 18+: Buffer.toString('base64url') is no-pad by default. Match
  // exactly so we don't drift from the on-chain Rust output.
  return Buffer.from(bytes).toString("base64url");
}

/**
 * Build the canonical claim message bytes for an ANT escrow claim.
 *
 * Inputs are the same fields the on-chain `build_ant_escrow_claim_message`
 * receives:
 * - `antMint`: 32-byte Solana pubkey of the ANT (Metaplex Core asset)
 * - `claimant`: 32-byte Solana pubkey of the destination wallet
 * - `nonce`: 32-byte anti-replay nonce stored in the escrow PDA
 * - `network`: byte string identifying the deploy ("solana-mainnet",
 *              "solana-devnet", or "localnet"). Must match what the
 *              on-chain program was compiled with.
 * - `recipientPubkey`: identity bytes — RSA modulus (512 bytes) for
 *              Arweave, 20-byte address for Ethereum. Hashed into the
 *              canonical message to bind the attestation to the
 *              deposit-time recipient. Wrong bytes → divergent
 *              canonical → on-chain Ed25519 verify fails.
 *
 * Output is UTF-8 bytes, line-feed separated, no trailing newline:
 *
 *     ar.io ant-escrow claim
 *     network: <network>
 *     recipient: <sha256(recipientPubkey) base64url-no-pad>
 *     ant: <ant_mint_base58>
 *     claimant: <claimant_solana_pubkey_base58>
 *     nonce: <nonce_hex_lowercase>
 */
export function buildAntEscrowClaimMessage(args: {
  antMint: Uint8Array;
  claimant: Uint8Array;
  nonce: Uint8Array;
  network: string;
  recipientPubkey: Uint8Array;
}): Uint8Array {
  if (args.antMint.length !== 32) {
    throw new Error(`antMint must be 32 bytes, got ${args.antMint.length}`);
  }
  if (args.claimant.length !== 32) {
    throw new Error(`claimant must be 32 bytes, got ${args.claimant.length}`);
  }
  if (args.nonce.length !== 32) {
    throw new Error(`nonce must be 32 bytes, got ${args.nonce.length}`);
  }
  if (args.recipientPubkey.length === 0) {
    throw new Error("recipientPubkey must be non-empty");
  }

  const lines = [
    ANT_ESCROW_CLAIM_HEADER,
    `network: ${args.network}`,
    `recipient: ${deriveRecipientIdB64Url(args.recipientPubkey)}`,
    `ant: ${bs58.encode(args.antMint)}`,
    `claimant: ${bs58.encode(args.claimant)}`,
    `nonce: ${hexLowercase(args.nonce)}`,
  ];
  return new TextEncoder().encode(lines.join("\n"));
}

/**
 * Build the canonical claim message bytes for a token or vault escrow claim.
 *
 * Inputs match the on-chain `build_escrow_claim_message`:
 * - `assetType`: "token" | "vault"
 * - `assetId`: 32-byte deposit identifier (hex-encoded into the message)
 * - `amount`: u64, formatted as decimal
 * - `claimant`: 32-byte Solana pubkey (base58 in the message)
 * - `nonce`: 32-byte anti-replay nonce (hex)
 * - `network`: matches the on-chain NETWORK constant
 * - `recipientPubkey`: identity bytes — same shape as
 *                     `buildAntEscrowClaimMessage`.
 *
 * Output (UTF-8, line-feed separated, no trailing newline):
 *
 *     ar.io escrow claim
 *     network: <network>
 *     recipient: <sha256(recipientPubkey) base64url-no-pad>
 *     type: <token|vault>
 *     asset: <asset_id_hex>
 *     amount: <decimal>
 *     claimant: <claimant_base58>
 *     nonce: <nonce_hex>
 */
export function buildEscrowClaimMessage(args: {
  assetType: "token" | "vault";
  assetId: Uint8Array;
  amount: bigint;
  claimant: Uint8Array;
  nonce: Uint8Array;
  network: string;
  recipientPubkey: Uint8Array;
}): Uint8Array {
  if (args.assetId.length !== 32) {
    throw new Error(`assetId must be 32 bytes, got ${args.assetId.length}`);
  }
  if (args.claimant.length !== 32) {
    throw new Error(`claimant must be 32 bytes, got ${args.claimant.length}`);
  }
  if (args.nonce.length !== 32) {
    throw new Error(`nonce must be 32 bytes, got ${args.nonce.length}`);
  }
  if (args.amount < 0n || args.amount > 0xffff_ffff_ffff_ffffn) {
    throw new Error(`amount must fit in u64, got ${args.amount}`);
  }
  if (args.recipientPubkey.length === 0) {
    throw new Error("recipientPubkey must be non-empty");
  }

  const lines = [
    ESCROW_CLAIM_HEADER,
    `network: ${args.network}`,
    `recipient: ${deriveRecipientIdB64Url(args.recipientPubkey)}`,
    `type: ${args.assetType}`,
    `asset: ${hexLowercase(args.assetId)}`,
    `amount: ${args.amount.toString()}`,
    `claimant: ${bs58.encode(args.claimant)}`,
    `nonce: ${hexLowercase(args.nonce)}`,
  ];
  return new TextEncoder().encode(lines.join("\n"));
}

function hexLowercase(bytes: Uint8Array): string {
  let out = "";
  for (const b of bytes) {
    out += b.toString(16).padStart(2, "0");
  }
  return out;
}
