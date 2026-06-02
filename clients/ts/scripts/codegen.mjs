#!/usr/bin/env node
/**
 * Codama codegen for the AR.IO Solana typed-client package.
 *
 * Reads Anchor IDLs from `../../target/idl/<program>.json` (the canonical
 * Anchor build output of this repo) and renders kit-native TypeScript
 * clients into `src/<program>/`. The generated tree is gitignored —
 * regenerated on `yarn install` (postinstall) and on `yarn build`.
 *
 * Adapted from `solana-ar-io/sdk/scripts/codegen.mjs` (same Codama version
 * pin and same post-processing rules), trimmed to:
 *   - the 5 ario_* programs (no MPL Core vendored IDL — consumers pull
 *     mpl-core from `@metaplex-foundation/mpl-core` directly)
 *   - no `events-codegen.mjs` pass (Codama renderers-js 2.x has no events
 *     pass; event decoders are a downstream concern — the SDK keeps its
 *     own copy)
 *   - standard Codama output (we KEEP `programs/`, `instructions/`,
 *     `pdas/`, `errors/` so consumers get the full surface mpl-core +
 *     spl-token style)
 *
 * Post-processing that DOES run:
 *   - Flatten Codama's `<out>/src/generated/...` to `<out>/...`
 *   - Add `.js` extensions to relative imports (Codama omits them, but
 *     TS `nodenext` + Node ESM both require them at runtime)
 *   - Forward `programAddress` through `await find*Pda(...)` calls in
 *     async instruction builders (works around a Codama bug where the
 *     PDA helper otherwise derives against the IDL placeholder address)
 */

import {
  cpSync,
  existsSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { rootNodeFromAnchor } from '@codama/nodes-from-anchor';
import { renderVisitor } from '@codama/renderers-js';
import { createFromRoot } from 'codama';

const HERE = dirname(fileURLToPath(import.meta.url));
const CLIENT_ROOT = resolve(HERE, '..');
const REPO_ROOT = resolve(CLIENT_ROOT, '..', '..');
const ANCHOR_IDL_DIR = resolve(REPO_ROOT, 'target/idl');
const OUT_ROOT = resolve(CLIENT_ROOT, 'src');

// Target cluster — retained only for logging and the defensive dedup
// tie-break. Since devnet-shrunk was retired (ADR-024) all clusters share
// full-size registries, so the IDL no longer emits cfg-conditional
// duplicate fields and CLUSTER no longer changes the generated output.
//
// 'staging' is a mainnet dress-rehearsal deployed against Solana devnet
// (per `release-clients-ts.yml` workflow input description). It carries
// the same production-sized registries as 'mainnet' but routes to its
// own `@staging` npm dist-tag so consumers tracking `@latest` aren't
// pulled to the pre-mainnet artifact unexpectedly.
const CLUSTER = (process.env.CLUSTER ?? 'staging').toLowerCase();
if (CLUSTER !== 'staging' && CLUSTER !== 'mainnet') {
  console.error(
    `[codegen] CLUSTER must be 'staging' or 'mainnet'; got '${CLUSTER}'`,
  );
  process.exit(1);
}
console.log(`[codegen] CLUSTER=${CLUSTER}`);

const PROGRAMS = [
  { idl: 'ario_core', out: 'core' },
  { idl: 'ario_gar', out: 'gar' },
  { idl: 'ario_arns', out: 'arns' },
  { idl: 'ario_ant', out: 'ant' },
  { idl: 'ario_ant_escrow', out: 'ant-escrow' },
];

const missing = PROGRAMS.filter(
  (p) => !existsSync(resolve(ANCHOR_IDL_DIR, `${p.idl}.json`)),
);
if (missing.length > 0) {
  console.error('[codegen] Missing IDLs:');
  for (const p of missing) {
    console.error(`  - ${ANCHOR_IDL_DIR}/${p.idl}.json`);
  }
  console.error(`Run \`anchor build\` in ${REPO_ROOT} first.`);
  process.exit(1);
}

for (const { idl, out } of PROGRAMS) {
  const idlPath = resolve(ANCHOR_IDL_DIR, `${idl}.json`);
  const outPath = resolve(OUT_ROOT, out);

  console.log(`[codegen] ${idl} -> src/${out}/`);

  const anchorIdl = JSON.parse(readFileSync(idlPath, 'utf-8'));
  dedupeCfgConditionalFields(anchorIdl);
  const codama = createFromRoot(rootNodeFromAnchor(anchorIdl));

  rmSync(outPath, { recursive: true, force: true });

  await codama.accept(renderVisitor(outPath, { formatCode: false }));

  // Codama emits `<outPath>/src/generated/...` and a placeholder package.json.
  // Flatten to `<outPath>/...` so consumers can `import '@ar.io/solana-contracts/<out>'`.
  const nestedDir = resolve(outPath, 'src/generated');
  if (existsSync(nestedDir)) {
    cpSync(nestedDir, outPath, { recursive: true });
    rmSync(resolve(outPath, 'src'), { recursive: true, force: true });
  }
  const placeholderPkg = resolve(outPath, 'package.json');
  if (existsSync(placeholderPkg)) rmSync(placeholderPkg, { force: true });

  // Forward `programAddress` through Codama's PDA helper invocations in
  // async instruction builders. Codama emits `await findFooPda({ seeds })`
  // without threading `programAddress` — that derives PDAs against the
  // IDL placeholder instead of the deployed program ID. Patch in place.
  const instructionsDir = resolve(outPath, 'instructions');
  if (existsSync(instructionsDir)) {
    forwardProgramAddressToPdaHelpers(instructionsDir);
  }

  // Codama omits `.js` extensions on relative imports (`export * from './balance'`),
  // which TS `nodenext` + Node ESM both reject. Rewrite filesystem-aware.
  addJsExtensions(outPath);

  console.log(`[codegen] ${idl} ✓`);
}

console.log(`[codegen] ${PROGRAMS.length} programs ✓`);

/**
 * Defensive no-op in the common case. Anchor's IDL generator emits every
 * `#[cfg]`-conditional field as a SEPARATE entry regardless of the active
 * feature, which historically produced duplicate field names (e.g. two
 * `names` entries, 200_000 vs 200 slots) under the `devnet-shrunk` feature
 * — tsc rejected those with `TS2300: Duplicate identifier`.
 *
 * `devnet-shrunk` was retired (ADR-024): all clusters now compile the same
 * full-size registries, so no `ario_*` account carries a cfg-conditional
 * size field and there are normally NO duplicates to drop. This pass is
 * kept as a guard — if any future `#[cfg]` field reintroduces a duplicate,
 * it deduplicates by name within each struct, keeping the LARGER (i.e.
 * production-sized) variant.
 */
function dedupeCfgConditionalFields(idl) {
  // No cluster prefers the smaller variant anymore; keep production sizes.
  const preferSmaller = false;
  const dedupe = (type) => {
    if (!type || !type.fields || !Array.isArray(type.fields)) return;
    const seen = new Map(); // name -> { idx, size }
    const toDrop = new Set();
    type.fields.forEach((field, idx) => {
      const size = arraySize(field.type);
      const prev = seen.get(field.name);
      if (prev === undefined) {
        seen.set(field.name, { idx, size });
      } else if (size != null && prev.size != null && size !== prev.size) {
        const keepCurrent = preferSmaller ? size < prev.size : size > prev.size;
        if (keepCurrent) {
          toDrop.add(prev.idx);
          seen.set(field.name, { idx, size });
        } else {
          toDrop.add(idx);
        }
      } else {
        // Sizes equal or non-fixed-array — keep the FIRST occurrence.
        toDrop.add(idx);
      }
    });
    if (toDrop.size > 0) {
      type.fields = type.fields.filter((_, idx) => !toDrop.has(idx));
    }
  };
  for (const t of idl.types ?? []) {
    if (t.type) dedupe(t.type);
  }
  for (const a of idl.accounts ?? []) {
    if (a.type) dedupe(a.type);
  }
}

function arraySize(t) {
  if (t && typeof t === 'object' && Array.isArray(t.array) && t.array.length === 2) {
    return typeof t.array[1] === 'number' ? t.array[1] : null;
  }
  return null;
}

/**
 * In Codama's generated async instruction builders, every PDA-resolve call
 * looks like `await findFooPda({ seedA, seedB })` (no second arg). Rewrite
 * each to `await findFooPda({ seedA, seedB }, { programAddress })` so the
 * PDA derives against the same program ID the surrounding instruction is
 * sent to (rather than the IDL placeholder). Lifted unchanged from the
 * SDK's codegen.
 */
function forwardProgramAddressToPdaHelpers(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = resolve(dir, entry.name);
    if (entry.isDirectory()) {
      forwardProgramAddressToPdaHelpers(full);
    } else if (entry.name.endsWith('.ts')) {
      const src = readFileSync(full, 'utf-8');
      let out = '';
      let i = 0;
      let changed = false;
      while (i < src.length) {
        const m = /await\s+find\w+Pda\(\s*\{/g;
        m.lastIndex = i;
        const hit = m.exec(src);
        if (!hit) {
          out += src.slice(i);
          break;
        }
        out += src.slice(i, hit.index);
        let depth = 1;
        let j = m.lastIndex;
        while (j < src.length && depth > 0) {
          const ch = src[j];
          if (ch === '{') depth += 1;
          else if (ch === '}') depth -= 1;
          j += 1;
        }
        let k = j;
        while (k < src.length && /\s/.test(src[k])) k += 1;
        if (src[k] === ')') {
          if (src.slice(j, k).includes(',')) {
            out += src.slice(hit.index, k + 1);
          } else {
            out +=
              src.slice(hit.index, j) +
              ', { programAddress }' +
              src.slice(j, k + 1);
            changed = true;
          }
          i = k + 1;
        } else if (src[k] === ',') {
          out += src.slice(hit.index, k);
          i = k;
        } else {
          out += src.slice(hit.index, j);
          i = j;
        }
      }
      if (changed) writeFileSync(full, out);
    }
  }
}

/**
 * Filesystem-aware rewrite: every relative `from '...'` gets `.js` added
 * where the resolved target is a `.ts` file, or `/index.js` where it's a
 * directory with an `index.ts`. External package imports left alone.
 * Idempotent — safe to re-run. Lifted from the SDK's codegen.
 */
function addJsExtensions(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = resolve(dir, entry.name);
    if (entry.isDirectory()) {
      addJsExtensions(full);
    } else if (entry.name.endsWith('.ts')) {
      const fileDir = dirname(full);
      const src = readFileSync(full, 'utf-8');
      let fixed = src;
      fixed = fixed.replace(
        /(from\s+['"])(\.\.?)(['"])/g,
        (_m, prefix, spec, suffix) => `${prefix}${spec}/index.js${suffix}`,
      );
      fixed = fixed.replace(
        /(from\s+['"])(\.\.?\/[^'"]+?)(?<!\.(?:js|json|ts|tsx|jsx|mjs|cjs))(['"])/g,
        (_match, prefix, spec, suffix) => {
          const resolved = resolve(fileDir, spec);
          if (existsSync(`${resolved}.ts`)) return `${prefix}${spec}.js${suffix}`;
          if (
            existsSync(resolved) &&
            statSync(resolved).isDirectory() &&
            existsSync(resolve(resolved, 'index.ts'))
          ) {
            return `${prefix}${spec}/index.js${suffix}`;
          }
          return `${prefix}${spec}${suffix}`;
        },
      );
      if (fixed !== src) writeFileSync(full, fixed);
    }
  }
}
