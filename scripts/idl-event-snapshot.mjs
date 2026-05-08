#!/usr/bin/env node
/**
 * IDL event ABI stability check.
 *
 * Once an event ships in an SDK release, its borsh layout (field name +
 * type + order) is permanent — every indexer / decoder that subscribed
 * to the event depends on it. This script enforces that.
 *
 * Modes:
 *   --update         Write current IDL events into idl-event-snapshots.json
 *   (no flag)        Compare current IDL events to the snapshot.
 *                    - Additions to the events list are allowed
 *                    - ANY change to an existing event's name/fields/types
 *                      is rejected (shipped events are append-only)
 *
 * Usage:
 *   # Run after `anchor build` to verify no breaking event changes
 *   node scripts/idl-event-snapshot.mjs
 *
 *   # Bless the current IDL (use sparingly — only when intentionally
 *   # adding new events, never to "fix" a failing check):
 *   node scripts/idl-event-snapshot.mjs --update
 *
 * Per ADR-017 (Anchor `#[event]` ABI policy): if an existing event
 * needs to change shape, ship a new `*EventV2` and deprecate the old
 * one — never mutate in place.
 */

import { readFileSync, writeFileSync, existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = resolve(__dirname, '..');
const IDL_DIR = resolve(REPO_ROOT, 'target', 'idl');
const SNAPSHOT_PATH = resolve(REPO_ROOT, 'idl-event-snapshots.json');

const PROGRAMS = [
  'ario_core',
  'ario_gar',
  'ario_arns',
  'ario_ant',
  'ario_ant_escrow',
];

/**
 * Extract a normalized snapshot of every event in an IDL.
 *
 * Anchor 0.31 IDL events look like:
 *   { name: "TransferEvent", discriminator: [u8;8], ... }
 * with field shapes living in `idl.types[]` keyed by name.
 *
 * We capture (name, fields[name+type+order]) so any rename / reorder /
 * type change in an EXISTING event surfaces as a diff.
 */
function snapshotEvents(idl) {
  const events = idl.events ?? [];
  const types = idl.types ?? [];
  const typeIndex = new Map(types.map((t) => [t.name, t]));

  return events
    .map((ev) => {
      const t = typeIndex.get(ev.name);
      const fields = t?.type?.fields ?? [];
      return {
        name: ev.name,
        discriminator: ev.discriminator,
        fields: fields.map((f) => ({ name: f.name, type: f.type })),
      };
    })
    .sort((a, b) => a.name.localeCompare(b.name));
}

function loadIdls() {
  const out = {};
  for (const program of PROGRAMS) {
    const p = resolve(IDL_DIR, `${program}.json`);
    if (!existsSync(p)) {
      console.error(
        `ERROR: ${program}.json missing at ${p}. Run 'anchor build' first.`,
      );
      process.exit(2);
    }
    out[program] = snapshotEvents(JSON.parse(readFileSync(p, 'utf8')));
  }
  return out;
}

function loadSnapshot() {
  if (!existsSync(SNAPSHOT_PATH)) return null;
  return JSON.parse(readFileSync(SNAPSHOT_PATH, 'utf8'));
}

function fmtField(f) {
  return `${f.name}: ${JSON.stringify(f.type)}`;
}

/**
 * Compare current IDL state vs snapshot. Returns array of issue strings;
 * empty array = all good.
 */
function check(current, snapshot) {
  const issues = [];

  for (const program of PROGRAMS) {
    const cur = current[program] ?? [];
    const snap = snapshot?.[program] ?? [];
    const snapByName = new Map(snap.map((e) => [e.name, e]));

    for (const snapEv of snap) {
      const curEv = cur.find((e) => e.name === snapEv.name);
      if (!curEv) {
        issues.push(
          `[${program}] EVENT REMOVED: ${snapEv.name}. ` +
            `Shipped events are append-only — deprecate in docs, don't delete.`,
        );
        continue;
      }
      // Check field-by-field stability
      const snapFields = snapEv.fields;
      const curFields = curEv.fields;
      if (snapFields.length !== curFields.length) {
        issues.push(
          `[${program}.${snapEv.name}] FIELD COUNT CHANGED: ` +
            `was ${snapFields.length}, now ${curFields.length}. ` +
            `Adding/removing fields breaks ABI — ship a *EventV2 instead.`,
        );
        continue;
      }
      for (let i = 0; i < snapFields.length; i++) {
        const sf = snapFields[i];
        const cf = curFields[i];
        if (sf.name !== cf.name) {
          issues.push(
            `[${program}.${snapEv.name}] field[${i}] RENAMED: ` +
              `was '${sf.name}', now '${cf.name}'.`,
          );
        }
        if (JSON.stringify(sf.type) !== JSON.stringify(cf.type)) {
          issues.push(
            `[${program}.${snapEv.name}.${sf.name}] TYPE CHANGED: ` +
              `was ${JSON.stringify(sf.type)}, now ${JSON.stringify(cf.type)}.`,
          );
        }
      }
      // discriminator is auto-derived from name; if name didn't change,
      // discriminator can't change either, but we sanity-check anyway.
      if (
        JSON.stringify(snapEv.discriminator) !==
        JSON.stringify(curEv.discriminator)
      ) {
        issues.push(
          `[${program}.${snapEv.name}] DISCRIMINATOR CHANGED. ` +
            `Anchor derives this from the type name — did the type get renamed?`,
        );
      }
    }

    // New events are reported (informational, not errors)
    const added = cur
      .filter((e) => !snapByName.has(e.name))
      .map((e) => e.name);
    if (added.length > 0) {
      console.log(
        `[${program}] NEW EVENTS (allowed): ${added.join(', ')}`,
      );
    }
  }

  return issues;
}

const update = process.argv.includes('--update');
const current = loadIdls();

if (update) {
  writeFileSync(
    SNAPSHOT_PATH,
    JSON.stringify(current, null, 2) + '\n',
    'utf8',
  );
  const totalEvents = Object.values(current).reduce(
    (n, evs) => n + evs.length,
    0,
  );
  console.log(
    `Wrote snapshot to ${SNAPSHOT_PATH} (${totalEvents} events across ${PROGRAMS.length} programs).`,
  );
  process.exit(0);
}

const snapshot = loadSnapshot();
if (!snapshot) {
  console.log(
    `No snapshot at ${SNAPSHOT_PATH} yet — bootstrapping with current IDL.\n` +
      `Run with --update once you've verified the current event set is intentional.`,
  );
  process.exit(0);
}

const issues = check(current, snapshot);
if (issues.length === 0) {
  const totalEvents = Object.values(current).reduce(
    (n, evs) => n + evs.length,
    0,
  );
  console.log(
    `IDL event ABI stable. ${totalEvents} events checked across ${PROGRAMS.length} programs.`,
  );
  process.exit(0);
}

console.error('IDL event ABI BROKEN:');
for (const issue of issues) console.error(`  - ${issue}`);
console.error(
  '\nPer ADR-017: shipped events are append-only. To intentionally ' +
    'change a shipped event, ship a *EventV2 alongside the original.',
);
process.exit(1);
