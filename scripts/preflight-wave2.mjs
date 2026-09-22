#!/usr/bin/env node
/**
 * Read-only pre-flight check for the Wave 1 / Wave 2 GAR rollout.
 *
 * Run this IMMEDIATELY BEFORE and IMMEDIATELY AFTER every program upgrade and
 * every migration batch, on the cluster you are about to touch. It answers one
 * question the rollout cannot proceed without:
 *
 *     Is it safe to upgrade right now?
 *
 * The check that matters is the ADR-0034 halt gate. After Wave 2,
 * `create_epoch(N+1)` refuses while epoch N is unfinished — so an epoch that
 * can never be distributed stops the ENTIRE lifecycle network-wide: no epochs,
 * no rewards, no observations, for everyone, until an operator writes it off
 * with `admin_close_stale_epoch`.
 *
 * That state is reachable TODAY (pre-Wave-2) and is harmless today, because
 * the current program happily supersedes a broken epoch. Upgrading while the
 * latest epoch is in it converts a dormant problem into an immediate halt. So
 * this script reproduces `distribute_epoch`'s own `EpochWeightsClobbered`
 * predicate against live accounts and refuses to say "clear" if any gateway
 * would trip it.
 *
 * Usage:
 *   node scripts/preflight-wave2.mjs --cluster staging
 *   AR_IO_RPC_URL=<rpc> node scripts/preflight-wave2.mjs --cluster mainnet
 *   ... --json out.json     # also write the full findings as JSON
 *
 * Exit status:
 *   0 = clear to proceed
 *   1 = at least one finding (details printed; NOT clear to proceed)
 *   2 = usage / RPC / decode error (nothing was verified — do NOT proceed)
 *
 * Read-only: issues no transaction and holds no key.
 *
 * Decoding uses the generated client in `clients/ts/lib` rather than
 * hand-rolled byte offsets, so a schema change cannot silently desync this
 * script from the program. Run `cd clients/ts && yarn build:tsc` if it is
 * missing.
 */
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

// Exit-status contract: 1 means "I checked and found problems", 2 means "I
// could not check". An uncaught throw at module scope would otherwise exit 1
// and be read as a verified finding, which is the opposite of the truth — so
// every unexpected failure is funnelled to 2. Registered before any work
// starts so even an early import or RPC failure is covered.
const bail = (e) => {
  // EPIPE just means the reader went away (`| head`, `| less` and quit). That
  // is not a verification failure, and reporting it as status 2 would teach
  // operators to distrust the status codes.
  if (e?.code === 'EPIPE' || e?.errno === -32) process.exit(0);
  console.error(`\n  ERROR: ${e?.stack ?? e?.message ?? e}`);
  console.error('  NOTHING WAS VERIFIED — do NOT proceed.\n');
  process.exit(2);
};
process.on('uncaughtException', bail);
process.on('unhandledRejection', bail);

const REPO = join(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const opt = (n) => {
  const i = args.indexOf(n);
  return i >= 0 ? args[i + 1] : undefined;
};

const cluster = opt('--cluster');
if (!['mainnet', 'staging'].includes(cluster)) {
  console.error(
    'usage: preflight-wave2.mjs --cluster mainnet|staging [--json out.json] [--program-ids <path>]',
  );
  process.exit(2);
}
const RPC =
  process.env.AR_IO_RPC_URL ??
  (cluster === 'staging' ? 'https://api.devnet.solana.com' : undefined);
if (!RPC) {
  console.error(
    'set AR_IO_RPC_URL for mainnet (the public endpoint rejects getProgramAccounts)',
  );
  process.exit(2);
}

// Program IDs. Defaults to the in-repo manifest, but `--program-ids` lets an
// operator run this from a gateway box that has no checkout.
const idsPath = opt('--program-ids') ?? join(REPO, `program-ids/${cluster}.json`);
let GAR;
try {
  GAR = JSON.parse(readFileSync(idsPath)).programs.ario_gar;
  if (!GAR) throw new Error('programs.ario_gar missing');
} catch (e) {
  console.error(`cannot read program ids from ${idsPath}: ${e.message}`);
  console.error('pass --program-ids <path to program-ids/<cluster>.json>');
  process.exit(2);
}

/**
 * Import the first candidate that resolves.
 *
 * This script is a SAFETY GATE, so it has to run where the operator is —
 * including a gateway box with no Rust toolchain, where `clients/ts/lib` was
 * never built. It therefore falls back from the in-repo build to the published
 * `@ar.io/solana-contracts` package, which contains the same generated code.
 */
async function importFirst(candidates, what) {
  const tried = [];
  for (const c of candidates) {
    try {
      return { mod: await import(c), from: c };
    } catch (e) {
      tried.push(`  ${c}\n    -> ${e.message.split('\n')[0]}`);
    }
  }
  console.error(`cannot load ${what}. Tried:\n${tried.join('\n')}`);
  console.error(
    '\nEither build the in-repo client (cd clients/ts && yarn build:tsc),',
  );
  console.error(
    'or install the published one:  npm i @ar.io/solana-contracts@<version>',
  );
  process.exit(2);
}

const kitSrc = await importFirst(
  [
    join(REPO, 'clients/ts/node_modules/@solana/kit/dist/index.node.mjs'),
    '@solana/kit',
  ],
  '@solana/kit',
);
const { getProgramDerivedAddress } = kitSrc.mod;

const garSrc = await importFirst(
  [join(REPO, 'clients/ts/lib/gar/index.js'), '@ar.io/solana-contracts/gar'],
  'the generated ario-gar client',
);
const g = garSrc.mod;
for (const fn of [
  'getGatewayDecoder',
  'getEpochDecoder',
  'getEpochSettingsDecoder',
  'getGatewaySettingsDecoder',
]) {
  if (typeof g[fn] !== 'function') {
    console.error(`the client at ${garSrc.from} does not export ${fn}()`);
    console.error('it is too old or not the ario-gar client — nothing was verified');
    process.exit(2);
  }
}
const dec = {
  gateway: g.getGatewayDecoder(),
  epoch: g.getEpochDecoder(),
  epochSettings: g.getEpochSettingsDecoder(),
  gatewaySettings: g.getGatewaySettingsDecoder(),
};

async function rpc(method, params) {
  const res = await fetch(RPC, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
  });
  const body = await res.json();
  if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error)}`);
  return body.result;
}

const B58 = '123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz';
const b58 = (bytes) => {
  let n = 0n;
  for (const b of bytes) n = (n << 8n) | BigInt(b);
  let out = '';
  while (n > 0n) {
    out = B58[Number(n % 58n)] + out;
    n /= 58n;
  }
  for (const b of bytes) {
    if (b !== 0) break;
    out = '1' + out;
  }
  return out || '1';
};
const pda = async (seeds) => {
  const [addr] = await getProgramDerivedAddress({ programAddress: GAR, seeds });
  return addr;
};

const { createHash } = await import('node:crypto');
const accountDiscB58 = (name) =>
  b58(createHash('sha256').update(`account:${name}`).digest().subarray(0, 8));

const findings = [];
const fail = (section, msg) => {
  findings.push({ section, msg });
  console.log(`  ✗ ${msg}`);
};
const ok = (msg) => console.log(`  ✓ ${msg}`);
const info = (msg) => console.log(`    ${msg}`);

const enc = new TextEncoder();
// Record only the ORIGIN. Managed RPC endpoints carry their API key in the
// path (…/<key>/) or the query, not just in userinfo, so a userinfo-only
// redaction would write a live credential into --json output — and that file
// gets attached to rollout tickets and CI artifacts.
const redactRpc = (u) => {
  try {
    const { protocol, host } = new URL(u);
    return `${protocol}//${host}`;
  } catch {
    return '<unparseable rpc url>';
  }
};
const report = { cluster, rpc: redactRpc(RPC), at: new Date().toISOString() };

console.log(`\n=== Wave 2 pre-flight — ${cluster} ===\n`);
// Provenance matters for a safety gate: the operator must be able to tell
// whether the verdict came from the in-repo build or a published package, and
// which version of it.
let clientVersion = 'unknown';
{
  // Resolve the module that actually loaded, then walk up to its package.json.
  // Reading `@ar.io/solana-contracts/package.json` directly does not work: the
  // package's `exports` map does not list it, so Node refuses the subpath.
  let start = garSrc.from;
  if (!start.startsWith('/')) {
    try {
      const { createRequire } = await import('node:module');
      start = createRequire(import.meta.url).resolve(garSrc.from);
    } catch {
      start = '';
    }
  }
  let d = start ? dirname(start) : '';
  for (let i = 0; i < 8 && d && d !== '/'; i++) {
    try {
      const pkg = JSON.parse(readFileSync(join(d, 'package.json')));
      if (pkg.version) {
        clientVersion = `${pkg.name ?? '?'}@${pkg.version}`;
        break;
      }
    } catch {
      /* keep walking */
    }
    d = dirname(d);
  }
}
console.log(`decoders : ${garSrc.from}`);
console.log(`version  : ${clientVersion}`);
console.log(`ids      : ${idsPath}\n`);
report.decoderSource = garSrc.from;
report.decoderVersion = clientVersion;

// ---------------------------------------------------------------- 1. program
console.log('[1] GAR program');
const progAcc = await rpc('getAccountInfo', [GAR, { encoding: 'base64' }]);
if (!progAcc?.value) {
  console.error(`  GAR program ${GAR} not found`);
  process.exit(2);
}
const progDataAddr = b58(
  Buffer.from(progAcc.value.data[0], 'base64').subarray(4, 36),
);
const pd = await rpc('getAccountInfo', [progDataAddr, { encoding: 'base64' }]);
const pdBuf = Buffer.from(pd.value.data[0], 'base64');
const slot = pdBuf.readBigUInt64LE(4);
const hasAuth = pdBuf[12] === 1;
const upgradeAuthority = hasAuth ? b58(pdBuf.subarray(13, 45)) : null;
info(`program        : ${GAR}`);
info(`deployed slot  : ${slot}`);
info(`data length    : ${pdBuf.length}`);
report.program = { id: GAR, slot: String(slot), upgradeAuthority, dataLen: pdBuf.length };
if (upgradeAuthority === null) {
  fail('program', 'UPGRADE AUTHORITY IS REVOKED — the program is immutable and a halted epoch could never be patched');
} else {
  ok(`upgrade authority present: ${upgradeAuthority}`);
}

// --------------------------------------------------------- 2. epoch settings
console.log('\n[2] EpochSettings');
const esPda = await pda([enc.encode('epoch_settings')]);
const esAcc = await rpc('getAccountInfo', [esPda, { encoding: 'base64' }]);
if (!esAcc?.value) {
  console.error('  EpochSettings not found');
  process.exit(2);
}
const es = dec.epochSettings.decode(
  Buffer.from(esAcc.value.data[0], 'base64'),
);
info(`authority          : ${es.authority}`);
info(`enabled            : ${es.enabled}`);
info(`currentEpochIndex  : ${es.currentEpochIndex}`);
report.epochSettings = {
  authority: es.authority,
  enabled: es.enabled,
  currentEpochIndex: String(es.currentEpochIndex),
};
if (es.authority === '11111111111111111111111111111111') {
  fail('epoch-settings', 'EpochSettings.authority is the null pubkey — admin_close_stale_epoch is unreachable, so a halted epoch could never be written off');
} else {
  ok('an authority exists that can run admin_close_stale_epoch');
}

// ------------------------------------------------- 3. latest epoch / halt risk
console.log('\n[3] Latest epoch — the ADR-0034 halt gate');
const latestIndex = Number(es.currentEpochIndex) - 1;
let epoch = null;
if (latestIndex < 0) {
  ok('no epoch has ever been created — nothing can be unfinished');
} else {
  const epPda = await pda([
    enc.encode('epoch'),
    new Uint8Array(new BigUint64Array([BigInt(latestIndex)]).buffer),
  ]);
  const epAcc = await rpc('getAccountInfo', [epPda, { encoding: 'base64' }]);
  if (!epAcc?.value) {
    ok(`epoch ${latestIndex} account is absent — reads as written-off, so create_epoch proceeds`);
  } else {
    epoch = dec.epoch.decode(Buffer.from(epAcc.value.data[0], 'base64'));
    const now = Math.floor(Date.now() / 1000);
    const ended = now >= Number(epoch.endTimestamp);
    const distributed = Number(epoch.rewardsDistributed) === 1;
    info(`epoch              : ${latestIndex}`);
    info(`rewards_distributed: ${epoch.rewardsDistributed}`);
    info(`weights_tallied    : ${epoch.weightsTallied}`);
    info(`obs_submitted      : ${epoch.observationsSubmitted}`);
    info(`distribution_index : ${epoch.distributionIndex}/${epoch.activeGatewayCount}`);
    info(`ended              : ${ended}`);
    report.latestEpoch = {
      index: latestIndex,
      rewardsDistributed: Number(epoch.rewardsDistributed),
      weightsTallied: Number(epoch.weightsTallied),
      observationsSubmitted: Number(epoch.observationsSubmitted),
      distributionIndex: Number(epoch.distributionIndex),
      activeGatewayCount: Number(epoch.activeGatewayCount),
      ended,
    };
    if (distributed) {
      ok('latest epoch is distributed — create_epoch will proceed after the upgrade');
    } else if (!ended) {
      ok('latest epoch is still live — it will distribute normally before the next create_epoch');
    } else {
      fail(
        'halt-gate',
        `epoch ${latestIndex} has ENDED and is UNDISTRIBUTED — after the Wave 2 upgrade create_epoch is blocked until it distributes or is written off`,
      );
    }
  }
}

// ---------------------------------------------------------- 4. gateway fleet
console.log('\n[4] Gateway fleet');
const gwAccounts = await rpc('getProgramAccounts', [
  GAR,
  {
    encoding: 'base64',
    filters: [{ memcmp: { offset: 0, bytes: accountDiscB58('Gateway') } }],
  },
]);
const gateways = gwAccounts.map((a) => ({
  pubkey: a.pubkey,
  raw: Buffer.from(a.account.data[0], 'base64'),
}));
// An empty result means the discriminator, the program id or the endpoint is
// wrong — NOT that there are no gateways. Continuing would run every
// downstream check over an empty list and print CLEAR TO PROCEED having
// verified nothing, which is the one failure mode this script must never have.
if (gateways.length === 0) {
  console.error('  getProgramAccounts returned NO gateway accounts');
  console.error('  the discriminator, program id or RPC endpoint is wrong');
  bail(new Error('gateway scan returned nothing; refusing to report a verdict'));
}

const sizes = {};
for (const g of gateways) sizes[g.raw.length] = (sizes[g.raw.length] ?? 0) + 1;
info(`total gateways     : ${gateways.length}`);
for (const [sz, n] of Object.entries(sizes)) {
  info(`  ${sz} bytes: ${n}  (${sz === '964' ? 'pre-Wave-1 schema 1.1.0' : sz === '996' ? 'Wave-1 migrated 1.2.0' : 'UNEXPECTED SIZE'})`);
  if (sz !== '964' && sz !== '996') {
    fail('fleet', `${n} gateway(s) have an unexpected account size of ${sz} bytes`);
  }
}
report.fleet = { total: gateways.length, sizes };

const decoded = [];
for (const g of gateways) {
  try {
    decoded.push({ pubkey: g.pubkey, len: g.raw.length, ...dec.gateway.decode(g.raw) });
  } catch (e) {
    fail('fleet', `gateway ${g.pubkey} (${g.raw.length} bytes) FAILED TO DECODE: ${e.message}`);
  }
}
if (decoded.length === gateways.length && gateways.length > 0) {
  ok(`all ${gateways.length} gateways decode with the current client`);
}

// operations_address audit — only meaningful once migrated (ADR-0030).
const migrated = decoded.filter((g) => g.len === 996);
if (migrated.length > 0) {
  const nulls = migrated.filter((g) => g.operationsAddress === '11111111111111111111111111111111');
  // ADR-0030's stale-tail hazard is `operations_address` having inherited a
  // LIVE key from the bytes a 52-byte shrink left behind — i.e. it equals
  // `observer_address` while NOT being the operator. Migration sets
  // `operations_address = operator`, and many operators legitimately run
  // `observer_address == operator`, so a naive `ops == observer` test flags
  // hundreds of benign rows (534 of 617 on staging). The operator exclusion is
  // what makes this predicate mean what it claims.
  const sameAsObserver = migrated.filter(
    (g) =>
      g.operationsAddress === g.observerAddress &&
      g.operationsAddress !== g.operator,
  );
  const notOperator = migrated.filter((g) => g.operationsAddress !== g.operator);
  info(`migrated gateways  : ${migrated.length}`);
  report.operationsAddress = {
    migrated: migrated.length,
    nulls: nulls.length,
    sameAsObserver: sameAsObserver.length,
  };
  if (nulls.length > 0) {
    fail('ops-address', `${nulls.length} migrated gateway(s) have a NULL operations_address`);
  } else ok('no migrated gateway has a null operations_address');
  if (sameAsObserver.length > 0) {
    fail(
      'ops-address',
      `${sameAsObserver.length} migrated gateway(s) have operations_address == observer_address AND != operator — the ADR-0030 stale-tail hazard (a live key someone else may hold)`,
    );
  } else {
    ok('no migrated gateway inherited a stale observer_address as its operations_address');
  }
  // Informational: an operator may legitimately delegate operations to another
  // key, so this is not a finding — but right after a migration it should be 0,
  // because `migrate_gateway` defaults it to the operator.
  info(`operations_address != operator: ${notOperator.length} (expect 0 immediately after migration; non-zero later means operators delegated)`);
}

// ------------------------------- 5. clobber check (only if the gate is at risk)
if (epoch && Number(epoch.rewardsDistributed) !== 1) {
  console.log('\n[5] EpochWeightsClobbered risk for the undistributed latest epoch');
  // Mirrors distribute_epoch:
  //   require!(is_leaving || !weights_stale || outside_earning_set, EpochWeightsClobbered)
  // A gateway trips it iff  !is_leaving && weights_stale && !outside_earning_set.
  const tripping = decoded.filter((g) => {
    const isLeaving = String(g.status) === 'Leaving' || Number(g.status) === 1;
    const weightsStale = BigInt(g.weights.weightsEpoch) !== BigInt(latestIndex);
    const outsideEarningSet = BigInt(g.startTimestamp) > BigInt(epoch.startTimestamp);
    return !isLeaving && weightsStale && !outsideEarningSet;
  });
  report.clobber = { tripping: tripping.length };
  if (tripping.length > 0) {
    fail(
      'clobber',
      `${tripping.length} gateway(s) would revert distribute_epoch with EpochWeightsClobbered — epoch ${latestIndex} can NEVER be distributed and must be written off with admin_close_stale_epoch BEFORE the Wave 2 upgrade`,
    );
    for (const g of tripping.slice(0, 5)) {
      info(`  ${g.pubkey}  weights_epoch=${g.weights.weightsEpoch} (expected ${latestIndex})`);
    }
    if (tripping.length > 5) info(`  ... and ${tripping.length - 5} more`);
  } else {
    ok(`no gateway trips EpochWeightsClobbered — epoch ${latestIndex} is distributable`);
  }
}

// -------------------------------------------------------- 6. supply counters
console.log('\n[6] Supply counters (ADR-0037)');
const gsPda = await pda([enc.encode('gar_settings')]);
const gsAcc = await rpc('getAccountInfo', [gsPda, { encoding: 'base64' }]);
const gs = dec.gatewaySettings.decode(Buffer.from(gsAcc.value.data[0], 'base64'));
const sumStaked = decoded.reduce((a, g) => a + BigInt(g.operatorStake), 0n);
const sumDelegated = decoded.reduce((a, g) => a + BigInt(g.totalDelegatedStake), 0n);
info(`total_staked    : counter ${gs.totalStaked}  vs  Σ gateways ${sumStaked}`);
info(`total_delegated : counter ${gs.totalDelegated}  vs  Σ gateways ${sumDelegated}`);
report.counters = {
  totalStaked: String(gs.totalStaked),
  sumOperatorStake: String(sumStaked),
  totalDelegated: String(gs.totalDelegated),
  sumTotalDelegatedStake: String(sumDelegated),
};
// Drift here is the ADR-0037 condition the reconcile exists to fix, so it is
// reported but is NOT a reason to block a program upgrade.
if (BigInt(gs.totalStaked) !== sumStaked) {
  info(`  note: total_staked drifts by ${BigInt(gs.totalStaked) - sumStaked} (ADR-0037 resync territory, not an upgrade blocker)`);
}
if (BigInt(gs.totalDelegated) !== sumDelegated) {
  info(`  note: total_delegated drifts by ${BigInt(gs.totalDelegated) - sumDelegated} (ADR-0037 resync territory, not an upgrade blocker)`);
}

// ------------------------------------------------------------------- verdict
const jsonOut = opt('--json');
if (jsonOut) {
  report.findings = findings;
  writeFileSync(jsonOut, `${JSON.stringify(report, null, 2)}\n`);
  console.log(`\nwrote ${jsonOut}`);
}

console.log('');
if (findings.length === 0) {
  console.log('=== CLEAR TO PROCEED ===\n');
  process.exit(0);
}
console.log(`=== ${findings.length} FINDING(S) — NOT CLEAR TO PROCEED ===`);
for (const f of findings) console.log(`  [${f.section}] ${f.msg}`);
console.log('');
process.exit(1);
