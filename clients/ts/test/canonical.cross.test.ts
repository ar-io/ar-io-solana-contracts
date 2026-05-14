/**
 * Cross-language canonical-message parity test.
 *
 * Pins `clients/ts/src/canonical/ant-escrow.ts` byte-for-byte against
 * the Rust `cargo run --example canonical -p ario-ant-escrow` binary
 * built from the same workspace. Both sides live in this repo, so any
 * drift surfaces in the same PR that introduces it.
 *
 * Self-bootstrapping: builds the Rust example on demand. Skips when
 * cargo isn't on PATH so plain `yarn test` works on machines without
 * the Rust toolchain. CI installs Rust before invoking the test, so
 * skips never reach origin.
 *
 * Successor to the attestor-side `canonical.cross.test.ts` that used
 * to live in `solana-ar-io/migration/attestor/`. Moving it here
 * unified the parity check with the canonical-message TS module it
 * pins; the attestor now imports
 * `@ar.io/solana-contracts/canonical/ant-escrow` and ships no
 * canonical impl of its own.
 */
import { strict as assert } from "node:assert";
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { before, describe, it } from "node:test";

import bs58 from "bs58";

import {
  buildAntEscrowClaimMessage,
  buildEscrowClaimMessage,
} from "../src/canonical/ant-escrow.js";

// Repo root is two `..` up from `clients/ts/test/`.
const REPO_ROOT = resolve(import.meta.dirname ?? ".", "..", "..", "..");
const RUST_BIN = resolve(REPO_ROOT, "target/debug/examples/canonical");

let cargoAvailable = true;

before(() => {
  if (existsSync(RUST_BIN)) return;
  // Try to build. If cargo isn't on PATH, mark all tests as skipped
  // (vs failing) so this test doesn't block runs on machines without
  // the Rust toolchain. CI installs Rust before the test step, so
  // skips can't reach origin under the standard CI path.
  const probe = spawnSync("cargo", ["--version"], { stdio: "ignore" });
  if (probe.error || probe.status !== 0) {
    cargoAvailable = false;
    return;
  }
  const build = spawnSync(
    "cargo",
    ["build", "--example", "canonical", "-p", "ario-ant-escrow"],
    { cwd: REPO_ROOT, stdio: "inherit" },
  );
  if (build.error || build.status !== 0 || !existsSync(RUST_BIN)) {
    throw new Error(
      `Failed to build the Rust canonical example for byte-parity verification. ` +
        `Manual build:\n  cd ${REPO_ROOT} && cargo build --example canonical -p ario-ant-escrow`,
    );
  }
});

function rustAntCanonical(
  antMintBase58: string,
  claimantBase58: string,
  nonceHex: string,
  recipientPubkeyHex: string,
): Uint8Array {
  const out = execFileSync(
    RUST_BIN,
    [antMintBase58, claimantBase58, nonceHex, recipientPubkeyHex],
    { encoding: "buffer" },
  );
  return new Uint8Array(out);
}

function rustEscrowCanonical(
  assetType: "token" | "vault",
  assetIdHex: string,
  amount: string,
  claimantBase58: string,
  nonceHex: string,
  recipientPubkeyHex: string,
): Uint8Array {
  const out = execFileSync(
    RUST_BIN,
    [
      "--escrow",
      assetType,
      assetIdHex,
      amount,
      claimantBase58,
      nonceHex,
      recipientPubkeyHex,
    ],
    { encoding: "buffer" },
  );
  return new Uint8Array(out);
}

// Stable Arweave-shaped recipient bytes: 512 bytes of 0xAB. Same value
// the Rust `canonical_message_changes_with_recipient_pubkey` unit test
// uses, so cross-language drift here surfaces alongside the Rust-side
// regression.
const RECIPIENT_AB512_HEX = "ab".repeat(512);
const RECIPIENT_AB512 = decodeHexLong(RECIPIENT_AB512_HEX);

function decodeHexLong(s: string): Uint8Array {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

function decodeHex(s: string): Uint8Array {
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) {
    out[i] = parseInt(s.slice(i * 2, i * 2 + 2), 16);
  }
  return out;
}

// Fixed test vectors that exercise the full byte width of each field.
const ANT_VECTORS: Array<{
  name: string;
  antMintBase58: string;
  claimantBase58: string;
  nonceHex: string;
}> = [
  {
    name: "design doc example",
    antMintBase58: "9PnRFwk2Yp7QyU3sQzXwUhJj6tVyM4nN2KqL5fT8RbAW",
    claimantBase58: "Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A",
    nonceHex: "a3f1c8d92e0b4f7a8e1d6c5b4a3920817f6e5d4c3b2a19188776655443322110",
  },
  {
    name: "all-zero nonce",
    antMintBase58: "F1ipQp4Bz9rYy3o9nz28sR8XqGXpKj7aXQH9aT8z2pn1",
    claimantBase58: "GpRq5C5cAaR1nL2A8bJh9kE3yz6T2sP4MqVxKn9wB8jF",
    nonceHex: "00".repeat(32),
  },
  {
    name: "all-ff nonce",
    antMintBase58: "F1ipQp4Bz9rYy3o9nz28sR8XqGXpKj7aXQH9aT8z2pn1",
    claimantBase58: "GpRq5C5cAaR1nL2A8bJh9kE3yz6T2sP4MqVxKn9wB8jF",
    nonceHex: "ff".repeat(32),
  },
];

const ESCROW_VECTORS: Array<{
  name: string;
  assetType: "token" | "vault";
  assetIdHex: string;
  amount: string;
  claimantBase58: string;
  nonceHex: string;
}> = [
  {
    name: "token, 1 mARIO",
    assetType: "token",
    assetIdHex: "01".repeat(32),
    amount: "1",
    claimantBase58: "GpRq5C5cAaR1nL2A8bJh9kE3yz6T2sP4MqVxKn9wB8jF",
    nonceHex: "aa".repeat(32),
  },
  {
    name: "vault, max u64",
    assetType: "vault",
    assetIdHex: "de".repeat(32),
    amount: "18446744073709551615",
    claimantBase58: "Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A",
    nonceHex: "cc".repeat(32),
  },
  {
    name: "token, mid-range amount",
    assetType: "token",
    assetIdHex: "7f".repeat(32),
    amount: "500000000",
    claimantBase58: "GpRq5C5cAaR1nL2A8bJh9kE3yz6T2sP4MqVxKn9wB8jF",
    nonceHex: "11".repeat(32),
  },
];

describe("@ar.io/solana-contracts/canonical/ant-escrow ↔ Rust parity (ANT)", () => {
  // The Rust example compiles with whatever feature set `cargo build
  // --example canonical` resolves to. Default workspace features pick
  // `network-mainnet` for `ario-ant-escrow` per its Cargo.toml; pass
  // the same network to the TS side for byte parity.
  const NETWORK = "solana-mainnet";

  for (const v of ANT_VECTORS) {
    it(`byte-equals Rust for: ${v.name}`, () => {
      if (!cargoAvailable) return;
      const tsBytes = buildAntEscrowClaimMessage({
        antMint: bs58.decode(v.antMintBase58),
        claimant: bs58.decode(v.claimantBase58),
        nonce: decodeHex(v.nonceHex),
        network: NETWORK,
        recipientPubkey: RECIPIENT_AB512,
      });
      const rustBytes = rustAntCanonical(
        v.antMintBase58,
        v.claimantBase58,
        v.nonceHex,
        RECIPIENT_AB512_HEX,
      );
      assert.deepEqual(tsBytes, rustBytes);
    });
  }
});

describe("@ar.io/solana-contracts/canonical/ant-escrow ↔ Rust parity (token/vault)", () => {
  const NETWORK = "solana-mainnet";

  for (const v of ESCROW_VECTORS) {
    it(`byte-equals Rust for: ${v.name}`, () => {
      if (!cargoAvailable) return;
      const tsBytes = buildEscrowClaimMessage({
        assetType: v.assetType,
        assetId: decodeHex(v.assetIdHex),
        amount: BigInt(v.amount),
        claimant: bs58.decode(v.claimantBase58),
        nonce: decodeHex(v.nonceHex),
        network: NETWORK,
        recipientPubkey: RECIPIENT_AB512,
      });
      const rustBytes = rustEscrowCanonical(
        v.assetType,
        v.assetIdHex,
        v.amount,
        v.claimantBase58,
        v.nonceHex,
        RECIPIENT_AB512_HEX,
      );
      assert.deepEqual(tsBytes, rustBytes);
    });
  }
});
