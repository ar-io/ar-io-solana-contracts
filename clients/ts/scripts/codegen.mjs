#!/usr/bin/env node
/**
 * Codama codegen for the AR.IO Solana typed-client package.
 *
 * Two IDL source families:
 *
 *   - In-tree Anchor builds at `../../target/idl/<program>.json` — the 5
 *     ario_* programs built by `anchor build` in this repo.
 *   - Vendored snapshots under `../idls/<program>.json` — external programs
 *     we don't build ourselves (e.g. Metaplex Core). Pin a known version by
 *     overwriting the JSON with a fresh snapshot from the upstream release.
 *
 * Renders kit-native TypeScript clients into `src/<program>/`. The generated
 * tree is gitignored — regenerated on `yarn install` (postinstall) and on
 * `yarn build`.
 *
 * Post-processing:
 *   - Flatten Codama's `<out>/src/generated/...` to `<out>/...`
 *   - Add `.js` extensions to relative imports (Codama omits them, but
 *     TS `nodenext` + Node ESM both require them at runtime)
 *   - Forward `programAddress` through `await find*Pda(...)` calls in
 *     async instruction builders (works around a Codama bug where the
 *     PDA helper otherwise derives against the IDL placeholder address)
 *   - For vendored programs: prune the `programs/` folder (kit Client
 *     plugin not used), extract the program-address constant into a shim,
 *     and rewrite imports accordingly.
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
const VENDORED_IDL_DIR = resolve(CLIENT_ROOT, 'idls');
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

/**
 * @typedef {{
 *   idl: string,
 *   out: string,
 *   vendored?: boolean,
 *   rename?: string,
 *   patch?: (idl: object) => void,
 *   prunePrograms?: boolean,
 * }} ProgramSpec
 *
 *   `idl`:            basename of the IDL JSON (no extension)
 *   `out`:            subdirectory under `src/`
 *   `vendored`:       if true, IDL is read from `idls/` (not `target/idl/`)
 *   `rename`:         override the IDL `name` field in-memory before lowering
 *   `patch`:          optional in-memory IDL surgery before Codama lowering
 *   `prunePrograms`:  if true, drop the `programs/` folder and extract a
 *                     `program-address.ts` shim (SDK-style slim output)
 */
const PROGRAMS = [
  { idl: 'ario_core', out: 'core' },
  { idl: 'ario_gar', out: 'gar' },
  { idl: 'ario_arns', out: 'arns' },
  { idl: 'ario_ant', out: 'ant' },
  { idl: 'ario_ant_escrow', out: 'ant-escrow' },
  {
    idl: 'mpl_core',
    out: 'mpl-core',
    vendored: true,
    rename: 'mpl_core',
    patch: patchMplCoreIdl,
    prunePrograms: true,
  },
];

const anchorPrograms = PROGRAMS.filter((p) => !p.vendored);
const vendoredPrograms = PROGRAMS.filter((p) => p.vendored);

const missingAnchor = anchorPrograms.filter(
  (p) => !existsSync(resolve(ANCHOR_IDL_DIR, `${p.idl}.json`)),
);
if (missingAnchor.length > 0) {
  console.error('[codegen] Missing Anchor IDLs:');
  for (const p of missingAnchor) {
    console.error(`  - ${ANCHOR_IDL_DIR}/${p.idl}.json`);
  }
  console.error(`Run \`anchor build\` in ${REPO_ROOT} first.`);
  process.exit(1);
}
const missingVendored = vendoredPrograms.filter(
  (p) => !existsSync(resolve(VENDORED_IDL_DIR, `${p.idl}.json`)),
);
if (missingVendored.length > 0) {
  console.error('[codegen] Missing vendored IDLs:');
  for (const p of missingVendored) {
    console.error(`  - ${VENDORED_IDL_DIR}/${p.idl}.json`);
  }
  process.exit(1);
}

for (const { idl, out, vendored, rename, patch, prunePrograms } of PROGRAMS) {
  const idlPath = vendored
    ? resolve(VENDORED_IDL_DIR, `${idl}.json`)
    : resolve(ANCHOR_IDL_DIR, `${idl}.json`);
  const outPath = resolve(OUT_ROOT, out);

  console.log(`[codegen] ${idl} -> src/${out}/`);

  const anchorIdl = JSON.parse(readFileSync(idlPath, 'utf-8'));
  if (rename) anchorIdl.name = rename;
  if (patch) patch(anchorIdl);
  if (!vendored) dedupeCfgConditionalFields(anchorIdl);
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

  // For vendored programs that don't use the kit Client plugin pattern:
  // extract the program-address constant from `programs/` into a slim shim,
  // drop the `programs/` folder, and rewrite imports in instructions/ and
  // errors/ to point at the shim. Keeps generated output lean.
  if (prunePrograms) {
    const programsDir = resolve(outPath, 'programs');
    const programAddressShim = resolve(outPath, 'program-address.ts');
    if (existsSync(programsDir)) {
      const constants = [];
      for (const file of readdirSync(programsDir)) {
        if (!file.endsWith('.ts')) continue;
        const src = readFileSync(resolve(programsDir, file), 'utf-8');
        const match = src.match(
          /export\s+const\s+([A-Z][A-Z0-9_]*_PROGRAM_ADDRESS)\s*=\s*('[^']+'|"[^"]+")\s+as\s+Address<('[^']+'|"[^"]+")>/,
        );
        if (match) {
          constants.push(
            `export const ${match[1]} = ${match[2]} as Address<${match[3]}>;`,
          );
        }
      }
      writeFileSync(
        programAddressShim,
        `/**\n * Program address constant lifted from Codama's pruned \`programs/\` output.\n */\nimport type { Address } from '@solana/kit';\n\n${constants.join('\n')}\n`,
      );
    }
    rmSync(resolve(outPath, 'programs'), { recursive: true, force: true });

    const errorsDir = resolve(outPath, 'errors');
    if (existsSync(errorsDir)) rewriteProgramsImport(errorsDir);

    // Rewrite barrel to only the surviving surface.
    const indexPath = resolve(outPath, 'index.ts');
    if (existsSync(indexPath)) {
      const banner =
        '/**\n * AUTOGENERATED by clients/ts/scripts/codegen.mjs.\n * Do not edit by hand — re-run `yarn codegen`.\n */\n\n';
      const dirExports = ['accounts', 'instructions', 'pdas', 'errors', 'types']
        .filter((d) => existsSync(resolve(outPath, d)))
        .map((d) => `export * from './${d}/index.js';`);
      const fileExports = existsSync(programAddressShim)
        ? [`export * from './program-address.js';`]
        : [];
      writeFileSync(
        indexPath,
        banner + [...dirExports, ...fileExports].join('\n') + '\n',
      );
    }
  }

  // Forward `programAddress` through Codama's PDA helper invocations in
  // async instruction builders. Codama emits `await findFooPda({ seeds })`
  // without threading `programAddress` — that derives PDAs against the
  // IDL placeholder instead of the deployed program ID. Patch in place.
  const instructionsDir = resolve(outPath, 'instructions');
  if (existsSync(instructionsDir)) {
    if (prunePrograms) rewriteProgramsImport(instructionsDir);
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
 * Rewrite every `from '../programs'` (and `from '../programs/...'`) to point
 * at the slim `program-address.ts` shim synthesised in place of the dropped
 * `programs/` folder. Idempotent.
 */
function rewriteProgramsImport(dir) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = resolve(dir, entry.name);
    if (entry.isDirectory()) {
      rewriteProgramsImport(full);
    } else if (entry.name.endsWith('.ts')) {
      const src = readFileSync(full, 'utf-8');
      const fixed = src.replace(
        /from\s+(['"])\.\.\/programs(?:\/[^'"]*)?\1/g,
        `from $1../program-address.js$1`,
      );
      if (fixed !== src) writeFileSync(full, fixed);
    }
  }
}

/**
 * Patch known issues in the upstream Metaplex Core IDL so it lowers cleanly
 * to a Codama tree. Only normalizes a small number of upstream defects
 * in-memory — we don't fork the IDL on disk.
 */
function patchMplCoreIdl(idl) {
  // `CreateGroupV1Args.relationships` references a non-existent type `crate`
  // (Shank macro emitted Rust's literal `crate::` prefix). Drop the Group
  // instruction family entirely — we don't use Groups in the AR.IO ANT
  // integration, and keeping them forces a stub or upstream type fix.
  const droppedInstructions = new Set([
    'CreateGroupV1',
    'CloseGroupV1',
    'UpdateGroupV1',
    'AddAssetsToGroupV1',
    'RemoveAssetsFromGroupV1',
    'AddCollectionsToGroupV1',
    'RemoveCollectionsFromGroupV1',
    'AddGroupsToGroupV1',
    'RemoveGroupsFromGroupV1',
  ]);
  const droppedTypes = new Set([
    'CreateGroupV1Args',
    'GroupV1',
    'RelationshipEntry',
    'RelationshipType',
  ]);
  const droppedAccounts = new Set(['GroupV1']);
  if (Array.isArray(idl.instructions)) {
    idl.instructions = idl.instructions.filter(
      (i) => !droppedInstructions.has(i.name),
    );
  }
  if (Array.isArray(idl.types)) {
    idl.types = idl.types.filter((t) => !droppedTypes.has(t.name));
  }
  if (Array.isArray(idl.accounts)) {
    idl.accounts = idl.accounts.filter((a) => !droppedAccounts.has(a.name));
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
