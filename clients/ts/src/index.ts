/**
 * `@ar.io/solana-contracts` — Codama-generated TypeScript client for the
 * AR.IO Solana programs.
 *
 * Each program is exposed as a sub-path so consumers can import only the
 * surface they need:
 *
 *   import { getBalanceEncoder } from '@ar.io/solana-contracts/core';
 *   import { PurchaseType }      from '@ar.io/solana-contracts/arns';
 *
 * The root barrel below re-exports everything; per-program imports avoid
 * pulling unused programs into the bundle.
 */
export * as core from './core/index.js';
export * as gar from './gar/index.js';
export * as arns from './arns/index.js';
export * as ant from './ant/index.js';
export * as antEscrow from './ant-escrow/index.js';
