#!/usr/bin/env node
/**
 * Read-only audit of GAR delegated-stake accounting. See ADR-0037.
 *
 * For every Gateway it compares `total_delegated_stake` (the counter) with
 * the Delegation accounts that actually exist for that gateway, and it
 * decomposes the stake pool:
 *
 *   pool = Σ operator_stake + Σ Delegation.amount + Σ Withdrawal.amount
 *        + unsettled rewards owed to real delegates
 *        + rewards credited to phantom stake      (counter − Σ Delegation) × accumulator
 *        + anything else                          (should be dust)
 *
 * Both counters only ever hold *settled* principal (`settle_delegate_rewards`
 * adds the same amount to a Delegation and to its gateway's counter), so
 * `counter − Σ Delegation.amount` does not move when rewards are settled.
 * On mainnet and staging it is exactly the stake of the AO delegators the
 * import skipped because they had no Solana address — see ADR-0037.
 *
 * Usage:
 *   node scripts/delegated-stake-audit.mjs --cluster staging
 *   AR_IO_RPC_URL=<mainnet rpc> node scripts/delegated-stake-audit.mjs --cluster mainnet
 *   ... --json out.json      # also write the per-gateway reconcile plan
 *
 * Exit status: 0 = no gateway is UNDER-counted; 1 = at least one counter is
 * below the Delegation accounts behind it (a liability the counter hides);
 * 2 = usage / RPC / decode error. Over-counted gateways are reported, not fatal.
 *
 * No dependencies beyond Node >= 18 (global fetch).
 */
import { createHash } from 'node:crypto';
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const REPO = join(dirname(fileURLToPath(import.meta.url)), '..');
const args = process.argv.slice(2);
const opt = (name) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : undefined;
};
const cluster = opt('--cluster');
if (!['mainnet', 'staging'].includes(cluster)) {
  console.error('usage: delegated-stake-audit.mjs --cluster mainnet|staging [--json out.json]');
  process.exit(2);
}
const RPC =
  process.env.AR_IO_RPC_URL ?? (cluster === 'staging' ? 'https://api.devnet.solana.com' : undefined);
if (!RPC) {
  console.error('set AR_IO_RPC_URL for mainnet (the public endpoint rejects getProgramAccounts)');
  process.exit(2);
}
const GAR = JSON.parse(readFileSync(join(REPO, `program-ids/${cluster}.json`))).programs.ario_gar;

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
function b58(bytes) {
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
  return out;
}
const accountDisc = (name) => b58(createHash('sha256').update(`account:${name}`).digest().subarray(0, 8));
const u128 = (d, o) => d.readBigUInt64LE(o) | (d.readBigUInt64LE(o + 8) << 64n);
const REWARD_PRECISION = 10n ** 18n;

async function all(name) {
  const res = await rpc('getProgramAccounts', [
    GAR,
    { encoding: 'base64', commitment: 'finalized', filters: [{ memcmp: { offset: 0, bytes: accountDisc(name) } }] },
  ]);
  return res.map((a) => ({ key: a.pubkey, d: Buffer.from(a.account.data[0], 'base64') }));
}

// Gateway: borsh walk up to `bump` (field order in programs/ario-gar/src/state/mod.rs).
function decodeGateway(key, d) {
  let o = 8;
  const operator = b58(d.subarray(o, (o += 32)));
  const str = () => {
    const len = d.readUInt32LE(o);
    const s = d.subarray(o + 4, o + 4 + len).toString('utf8');
    o += 4 + len;
    return s;
  };
  const label = str();
  str(); // fqdn
  o += 2 + 1; // port, protocol
  str(); // properties
  str(); // note
  const operatorStake = d.readBigUInt64LE(o);
  const counter = d.readBigUInt64LE(o + 8);
  const status = ['joined', 'leaving', 'gone'][d[o + 16]];
  o += 8 + 8 + 1 + 8; // operator_stake, total_delegated_stake, status, start_timestamp
  o += 1 + (d[o] === 1 ? 8 : 0); // leave_timestamp: Option<i64>
  o += 8; // leave_epoch_duration
  o += 4 * 5 + 1 + 1; // GatewayStats
  o += 7 * 8; // GatewayWeights
  o += 1 + 2 + 8 + 1; // GatewaySettings2 fixed part
  o += 1 + (d[o] === 1 ? 2 : 0); // pending_delegate_reward_share_ratio: Option<u16>
  o += 1 + (d[o] === 1 ? 8 : 0); // delegation_disabled_at: Option<i64>
  o += 4 + 1; // RegistryIndex
  o += 32; // observer_address
  const accumulator = u128(d, o);
  const bump = d[o + 16];
  // Canonical bumps are near 255; a small value means the walk is misaligned.
  if (bump < 200) throw new Error(`gateway ${key}: layout walk misaligned (bump=${bump})`);
  return { key, operator, label, status, operatorStake, counter, accumulator, delegations: [], delegated: 0n, unsettled: 0n };
}

const [gatewayAccs, delegationAccs, withdrawalAccs, settingsAccs] = await Promise.all(
  ['Gateway', 'Delegation', 'Withdrawal', 'GatewaySettings'].map(all),
);
const gateways = new Map();
for (const { key, d } of gatewayAccs) {
  const g = decodeGateway(key, d);
  gateways.set(g.operator, g);
}

let orphans = 0;
for (const { key, d } of delegationAccs) {
  // Delegation: gateway(32) delegator(32) amount(8) start_timestamp(8) reward_debt(16)
  const g = gateways.get(b58(d.subarray(8, 40)));
  if (!g) {
    orphans++;
    continue;
  }
  const amount = d.readBigUInt64LE(72);
  const debt = u128(d, 88);
  g.delegations.push(key);
  g.delegated += amount;
  if (amount > 0n && g.accumulator > debt) g.unsettled += (amount * (g.accumulator - debt)) / REWARD_PRECISION;
}

let withdrawn = 0n;
for (const { d } of withdrawalAccs) withdrawn += d.readBigUInt64LE(80); // owner(32) id(8) gateway(32) amount

const s = settingsAccs[0].d;
// GatewaySettings: ... migration_active @124, migration_authority, stake_token_account @157 ...
// total_staked @253, total_delegated @261, total_withdrawn @269
const stakeTokenAccount = b58(s.subarray(157, 189));
const pool = BigInt((await rpc('getTokenAccountBalance', [stakeTokenAccount, { commitment: 'finalized' }])).value.amount);

const G = [...gateways.values()];
const sum = (f, xs = G) => xs.reduce((a, g) => a + f(g), 0n);
const phantomOf = (g) => g.counter - g.delegated;
const ario = (x) => (Number(x) / 1e6).toLocaleString('en-US', { minimumFractionDigits: 6, maximumFractionDigits: 6 });
const pad = (x) => ario(x).padStart(22);

const over = G.filter((g) => phantomOf(g) > 0n);
const under = G.filter((g) => phantomOf(g) < 0n);
const phantomCredited = sum((g) => (phantomOf(g) > 0n ? (phantomOf(g) * g.accumulator) / REWARD_PRECISION : 0n));
const surplus = pool - sum((g) => g.operatorStake) - sum((g) => g.delegated) - withdrawn;

console.log(`cluster ${cluster}  gar ${GAR}`);
console.log(`gateways ${G.length}  delegations ${delegationAccs.length} (orphaned ${orphans})  withdrawals ${withdrawalAccs.length}`);
console.log(`migration_active ${s[124]}`);
for (const status of ['joined', 'leaving']) {
  const xs = G.filter((g) => g.status === status);
  const o = xs.filter((g) => phantomOf(g) > 0n);
  console.log(
    `${status.padEnd(8)} ${String(xs.length).padStart(4)}  over-counted ${String(o.length).padStart(4)}` +
      ` (Σ ${ario(sum(phantomOf, o))}; ${o.filter((g) => g.delegated === 0n).length} with no Delegation at all)`,
  );
}
console.log(`under-counted gateways: ${under.length}`);
console.log('\nsupply counters (GatewaySettings vs per-account sums)');
console.log(`  total_staked    ${pad(s.readBigUInt64LE(253))}   Σ operator_stake     ${pad(sum((g) => g.operatorStake))}`);
console.log(`  total_delegated ${pad(s.readBigUInt64LE(261))}   Σ gateway counters   ${pad(sum((g) => g.counter))}`);
console.log(`                  ${' '.repeat(22)}   Σ Delegation.amount  ${pad(sum((g) => g.delegated))}`);
console.log(`  total_withdrawn ${pad(s.readBigUInt64LE(269))}   Σ Withdrawal.amount  ${pad(withdrawn)}`);
console.log('\nstake pool');
console.log(`  balance                                         ${pad(pool)}`);
console.log(`  − operator stake − Delegations − Withdrawals =  ${pad(surplus)}  (surplus)`);
console.log(`    unsettled rewards owed to real delegates      ${pad(sum((g) => g.unsettled))}`);
console.log(`    rewards credited to phantom stake             ${pad(phantomCredited)}`);
console.log(`    remainder                                     ${pad(surplus - sum((g) => g.unsettled) - phantomCredited)}`);

const out = opt('--json');
if (out) {
  const plan = over
    .sort((a, b) => (phantomOf(b) > phantomOf(a) ? 1 : -1))
    .map((g) => ({
      gateway: g.key,
      operator: g.operator,
      label: g.label,
      status: g.status,
      expected_counter: g.counter.toString(),
      expected_removed: phantomOf(g).toString(),
      real_delegated: g.delegated.toString(),
      phantom_credited_rewards: ((phantomOf(g) * g.accumulator) / REWARD_PRECISION).toString(),
      delegations: g.delegations.sort(),
    }));
  writeFileSync(out, JSON.stringify({ cluster, gar: GAR, gateways: plan, under_counted: under.map((g) => g.key) }, null, 2));
  console.log(`\nwrote ${plan.length} reconcile entries to ${out}`);
}
process.exit(under.length ? 1 : 0);
