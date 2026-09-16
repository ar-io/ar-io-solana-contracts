#!/usr/bin/env node
/**
 * Anchor error-code ABI stability check.
 *
 * Anchor assigns error codes POSITIONALLY: the Nth variant of an
 * `#[error_code] enum` becomes `6000 + N`. The code appears nowhere in
 * the source, so inserting a variant in the middle of an enum silently
 * renumbers every variant after it. A "+4 lines" source diff can change
 * the meaning of a dozen codes that off-chain consumers already match on.
 *
 * Those consumers are load-bearing: the cranker and the observer branch
 * on numeric GAR codes to decide whether an epoch tick is a retryable
 * no-op or a real failure. Renumbering has already cost this network
 * twice:
 *
 *   - mainnet epoch 523 lost EVERY observation to a +2 drift between the
 *     deployed program and the observer's error table.
 *   - PR #128, as originally written, would have shifted
 *     EpochWeightsClobbered 6097 -> 6098 and EpochNoLongerLive
 *     6098 -> 6099 hours after both went live on mainnet — making 6098
 *     mean one thing to the program and another to every 4.3.1 client.
 *
 * So `code -> name` is a published ABI, exactly like an event's borsh
 * layout (ADR-018). This script enforces it. See ADR-035.
 *
 * WHY IT READS RUST SOURCE, NOT THE IDL: the ordering rule is purely
 * syntactic, so no toolchain is needed. That lets this run in the fast
 * `build-test.yml` PR job, which deliberately does no `anchor build`.
 * When built IDLs happen to be present the script cross-checks its
 * parse against them, so a parser bug cannot go unnoticed.
 *
 * Modes:
 *   --update    Write current error tables into error-code-snapshots.json
 *   (no flag)   Compare current error tables to the snapshot.
 *               - New codes ABOVE the snapshot high-water mark are allowed
 *                 (appending to the end of the enum)
 *               - Any existing code whose name changed, any variant that
 *                 moved, and any new code below the high-water mark are
 *                 rejected
 *
 *   --baseline <f>  Additionally require the committed snapshot to EXTEND
 *                   the snapshot in <f> (the target branch's copy) rather
 *                   than rewrite it. CI passes this on pull requests: the
 *                   snapshot is the guard, so a PR must not be able to
 *                   regenerate or truncate it to launder a renumbering.
 *
 * A missing snapshot, or a snapshot missing any guarded program's entry, is
 * a FAILURE rather than a pass -- both would otherwise disable the guard
 * silently while still reporting success.
 *
 * Usage:
 *   node scripts/error-code-snapshot.mjs                        # check
 *   node scripts/error-code-snapshot.mjs --baseline base.json   # check + integrity
 *   node scripts/error-code-snapshot.mjs --update               # bless APPENDS only
 *
 * NOTE: ario_ant_escrow is deliberately absent from PROGRAMS. It is
 * never deployed to any cluster, so its codes have no off-chain
 * consumers and guarding them would only produce false CI failures. If
 * it ever ships, add it here in the same PR.
 */

import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '..');
const SNAPSHOT_PATH = resolve(REPO_ROOT, 'error-code-snapshots.json');
const ANCHOR_ERROR_OFFSET = 6000;

/** program IDL name -> crate directory */
const PROGRAMS = {
  ario_core: 'ario-core',
  ario_gar: 'ario-gar',
  ario_arns: 'ario-arns',
  ario_ant: 'ario-ant',
};

/**
 * Extract variant names, in declaration order, from the single
 * `#[error_code]` enum in a Rust source file.
 *
 * Handles doc comments, line comments, blank lines, and multi-line
 * attributes such as a wrapped `#[msg("…")]` (tracked by paren depth).
 */
function parseErrorEnum(path) {
  const lines = readFileSync(path, 'utf8').split('\n');
  let i = 0;

  while (i < lines.length && !lines[i].includes('#[error_code]')) i++;
  if (i === lines.length) {
    throw new Error(`no #[error_code] enum found in ${path}`);
  }
  while (i < lines.length && !/pub enum\s+\w+\s*\{/.test(lines[i])) i++;
  i++;

  const names = [];
  let attrDepth = 0;

  for (; i < lines.length; i++) {
    const line = lines[i];
    if (/^\}/.test(line)) break;
    const s = line.trim();

    // Continuation of a multi-line attribute.
    if (attrDepth > 0) {
      attrDepth += countChar(line, '(') - countChar(line, ')');
      continue;
    }
    if (s.startsWith('#[')) {
      attrDepth = countChar(line, '(') - countChar(line, ')');
      continue;
    }
    if (s === '' || s.startsWith('//')) continue;

    const m = /^([A-Z][A-Za-z0-9_]*)\s*(,|\{|\()/.exec(s);
    if (m) names.push(m[1]);
  }

  return names;
}

function countChar(s, c) {
  let n = 0;
  for (const ch of s) if (ch === c) n++;
  return n;
}

function currentTables() {
  const out = {};
  for (const [idlName, crate] of Object.entries(PROGRAMS)) {
    const path = resolve(REPO_ROOT, 'programs', crate, 'src', 'error.rs');
    if (!existsSync(path)) {
      console.error(`ERROR: ${path} not found.`);
      process.exit(2);
    }
    out[idlName] = parseErrorEnum(path).map((name, n) => ({
      code: ANCHOR_ERROR_OFFSET + n,
      name,
    }));
  }
  return out;
}

/**
 * If built IDLs are lying around, report whether the source parse agrees
 * with what Anchor actually emitted.
 *
 * ADVISORY ONLY — deliberately never fatal. Editing `error.rs` without
 * re-running `anchor build` is the normal local state, and a stale IDL
 * combined with a mid-enum insertion diverges at the insertion point in
 * a way that is indistinguishable from a parser bug. Failing there would
 * fire on precisely the case this script exists to catch.
 *
 * The real parser guard is error-code-snapshots.json itself: it was
 * generated at a commit where the parse matched all four IDLs exactly
 * (256/256 codes), so any later parser regression shows up as a loud
 * snapshot mismatch instead.
 */
function crossCheckAgainstIdls(tables) {
  const notes = [];

  for (const idlName of Object.keys(PROGRAMS)) {
    const p = resolve(REPO_ROOT, 'target', 'idl', `${idlName}.json`);
    if (!existsSync(p)) continue;
    const idl = JSON.parse(readFileSync(p, 'utf8'));
    const fromIdl = (idl.errors ?? [])
      .slice()
      .sort((a, b) => a.code - b.code)
      .map((e) => ({ code: e.code, name: e.name }));
    if (fromIdl.length === 0) continue;

    const mine = tables[idlName];
    if (JSON.stringify(mine) !== JSON.stringify(fromIdl)) {
      notes.push(
        `[${idlName}] target/idl differs from source (${fromIdl.length} ` +
          `codes vs ${mine.length}); source is authoritative. Usually just a ` +
          `stale IDL — re-run 'anchor build' to refresh it.`,
      );
    }
  }

  return notes;
}

function check(current, snapshot) {
  const issues = [];

  for (const program of Object.keys(PROGRAMS)) {
    const cur = current[program] ?? [];
    const snap = snapshot?.[program] ?? [];
    if (snap.length === 0) continue;

    const curByCode = new Map(cur.map((e) => [e.code, e.name]));
    const curByName = new Map(cur.map((e) => [e.name, e.code]));
    const highWater = Math.max(...snap.map((e) => e.code));

    for (const { code, name } of snap) {
      const nowAtCode = curByCode.get(code);

      if (nowAtCode === undefined) {
        issues.push(
          `[${program}] CODE ${code} DISAPPEARED (was '${name}'). Error ` +
            `variants are append-only — keep the variant and stop returning ` +
            `it rather than deleting it.`,
        );
        continue;
      }

      if (nowAtCode !== name) {
        const movedTo = curByName.get(name);
        issues.push(
          `[${program}] CODE ${code} REASSIGNED: was '${name}', now ` +
            `'${nowAtCode}'` +
            (movedTo !== undefined
              ? ` ('${name}' moved to ${movedTo})`
              : ` ('${name}' is gone entirely)`) +
            `. A variant was inserted or reordered mid-enum; every off-chain ` +
            `consumer matching on ${code} now misreads it.`,
        );
      }
    }

    for (const e of cur) {
      if (e.code <= highWater && !snap.some((s) => s.code === e.code)) {
        issues.push(
          `[${program}] CODE ${e.code} ('${e.name}') APPEARED BELOW THE ` +
            `HIGH-WATER MARK (${highWater}). New variants must append to the ` +
            `END of the enum.`,
        );
      }
    }

    const appended = cur.filter((e) => e.code > highWater);
    if (appended.length > 0) {
      console.log(
        `[${program}] NEW ERRORS (allowed, appended): ` +
          appended.map((e) => `${e.code}=${e.name}`).join(', '),
      );
    }
  }

  return issues;
}

const update = process.argv.includes('--update');
const baselineFlag = process.argv.indexOf('--baseline');
const baselinePath =
  baselineFlag >= 0 ? process.argv[baselineFlag + 1] : null;
const current = currentTables();

for (const n of crossCheckAgainstIdls(current)) console.log(`note: ${n}`);

const total = Object.values(current).reduce((n, es) => n + es.length, 0);
const nprog = Object.keys(PROGRAMS).length;

if (update) {
  writeFileSync(
    SNAPSHOT_PATH,
    JSON.stringify(current, null, 2) + '\n',
    'utf8',
  );
  console.log(
    `Wrote snapshot to ${SNAPSHOT_PATH} (${total} error codes across ${nprog} programs).`,
  );
  process.exit(0);
}

const snapshot = existsSync(SNAPSHOT_PATH)
  ? JSON.parse(readFileSync(SNAPSHOT_PATH, 'utf8'))
  : null;

if (!snapshot) {
  console.error(
    `Error-code ABI UNVERIFIABLE: no snapshot at ${SNAPSHOT_PATH}.\n` +
      `The snapshot IS the guard, so a missing one is a failure, not a pass. ` +
      `If you are genuinely bootstrapping, run with --update and commit the ` +
      `result in the same PR.`,
  );
  process.exit(1);
}

// A snapshot that is present but missing (or has emptied) a program's entry
// would otherwise be skipped by check() and reported as "stable" — a false
// pass. Require every guarded program to be represented.
const missing = Object.keys(PROGRAMS).filter(
  (p) => !Array.isArray(snapshot[p]) || snapshot[p].length === 0,
);
if (missing.length > 0) {
  console.error(
    `Error-code ABI UNVERIFIABLE: snapshot has no entries for ` +
      `${missing.join(', ')}. Removing or emptying a program's entry ` +
      `silently disables its guard.`,
  );
  process.exit(1);
}

// When a baseline (the target branch's snapshot) is supplied, the committed
// snapshot must EXTEND it, never rewrite it. Without this, a PR could edit or
// regenerate the snapshot to launder a renumbering past the check.
if (baselinePath) {
  if (!existsSync(baselinePath)) {
    console.error(
      `Error-code ABI UNVERIFIABLE: --baseline ${baselinePath} does not exist.`,
    );
    process.exit(1);
  }
  const baseline = JSON.parse(readFileSync(baselinePath, 'utf8'));
  const drift = [];
  for (const program of Object.keys(PROGRAMS)) {
    const base = baseline[program] ?? [];
    const head = snapshot[program] ?? [];
    if (base.length === 0) continue; // program newly guarded by this PR
    if (head.length < base.length) {
      drift.push(
        `[${program}] snapshot SHRANK vs the target branch ` +
          `(${base.length} -> ${head.length} codes).`,
      );
      continue;
    }
    for (let i = 0; i < base.length; i++) {
      if (base[i].code !== head[i].code || base[i].name !== head[i].name) {
        drift.push(
          `[${program}] snapshot REWRITTEN at index ${i}: target branch has ` +
            `${base[i].code}='${base[i].name}', this PR has ` +
            `${head[i].code}='${head[i].name}'. The committed snapshot must ` +
            `extend the target's, not modify it.`,
        );
        break;
      }
    }
  }
  if (drift.length > 0) {
    console.error('Committed snapshot diverges from the target branch:');
    for (const d of drift) console.error(`  - ${d}`);
    console.error(
      '\nOnly tail additions are allowed. Re-run --update on top of the ' +
        'target branch rather than regenerating from scratch.',
    );
    process.exit(1);
  }
  console.log(
    `Snapshot extends the target branch's without rewriting it.`,
  );
}

const issues = check(current, snapshot);
if (issues.length === 0) {
  console.log(
    `Error-code ABI stable. ${total} codes checked across ${nprog} programs.`,
  );
  process.exit(0);
}

console.error('Error-code ABI BROKEN:');
for (const issue of issues) console.error(`  - ${issue}`);
console.error(
  '\nPer ADR-035: Anchor error codes are positional and published. Append ' +
    'new variants to the END of the enum; never insert, reorder, or delete.',
);
process.exit(1);
