#!/usr/bin/env node
/**
 * Postinstall hook for `@ar.io/solana-contracts`.
 *
 * Two modes:
 *   1. Source-repo install (this script runs inside `clients/ts/` of the
 *      `ar-io-solana-contracts` checkout, after `yarn install`): regenerate
 *      the Codama clients from `../../target/idl/` so a fresh clone is
 *      immediately compilable.
 *   2. Consumer install (the published tarball is being installed as a
 *      dependency in someone else's node_modules): the IDL directory
 *      doesn't exist relative to this file. Exit silently — consumers
 *      get the pre-built `lib/` from the tarball.
 */

import { existsSync, statSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const HERE = dirname(fileURLToPath(import.meta.url));
const ANCHOR_IDL_DIR = resolve(HERE, '..', '..', '..', 'target', 'idl');

if (!existsSync(ANCHOR_IDL_DIR) || !statSync(ANCHOR_IDL_DIR).isDirectory()) {
  // Consumer install path — pre-built artifacts in lib/ are sufficient.
  process.exit(0);
}

console.log('[postinstall] ar-io-solana-contracts checkout detected → running codegen');
const codegen = spawnSync(process.execPath, [resolve(HERE, 'codegen.mjs')], {
  cwd: resolve(HERE, '..'),
  stdio: 'inherit',
});
process.exit(codegen.status ?? 0);
