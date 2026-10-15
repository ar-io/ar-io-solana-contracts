#!/usr/bin/env node
/**
 * Reclaim the rent locked in migrated ANTs that are still in AR.IO custody
 * and whose ArNS name has expired.
 *
 * At migration the unclaimed ANTs were minted to the cold custody wallet
 * (45ZuEb…) for the claims service to hand out. Once an ANT's name has
 * expired and been pruned, no ArNS record points at it any more, and the
 * ANT is ours to retire (decided 2026-10-15).
 *
 * A candidate is an MPL Core asset that:
 *   - is owned (Core owner field) by the custody wallet,
 *   - carries a non-empty "ArNS Name" trait, so it once had a name,
 *   - is referenced by no ArnsRecord (`record.ant`) at all, which excludes
 *     names still active and names in their grace period,
 *   - has its AntConfig + AntControllers PDAs (so it is an ario-ant ANT),
 *   - is not in the --keep list.
 *
 * Per ANT, using only owner-gated instructions that need no admin key and
 * no migration window (rent from every closed account goes to the owner):
 *   1. ario_ant::close_ant_record_metadata_for_owner for each metadata PDA
 *      (none existed on 2026-10-15, but handled for completeness)
 *   2. ario_ant::close_ant_record for each AntRecord PDA
 *   3. ario_ant::close_ant_controllers + close_ant_config
 *   4. MPL Core BurnV1 (asset rent to the owner)
 * packed into as few transactions as fit, burn last. For almost every ANT
 * that is a single transaction, so the ANT is reclaimed atomically.
 *
 * The state is closed BEFORE the burn, while the asset still exists: the
 * owner-gated closes read the owner from the live asset. Burning first would
 * strand the state behind the admin-only admin_close_orphaned_ant_state.
 *
 * IDEMPOTENT. Every run re-reads chain state and sends only what is still
 * needed. A crash mid-ANT leaves some records closed and the asset alive; the
 * next run finishes it. An ANT whose asset is already burned is reported and
 * skipped (its leftover state, if any, needs admin_close_orphaned_ant_state).
 *
 * DRY-RUN BY DEFAULT. Without --execute nothing is signed or sent: every
 * transaction is built and simulated against current state, and the expected
 * rent is reported. With --execute each transaction is simulated again and
 * sent only if the simulation succeeds; the run stops at the first failure.
 *
 * Usage:
 *   node reclaim.mjs                                   # dry run, all candidates
 *   node reclaim.mjs --limit 5                         # dry run, first 5
 *   node reclaim.mjs --asset <ADDR> --execute          # one ANT for real
 *   node reclaim.mjs --execute                         # all candidates
 *   node reclaim.mjs --cluster fork --execute          # Surfpool rehearsal
 *
 * Options:
 *   --cluster mainnet|fork   default mainnet
 *   --execute                sign and send (otherwise simulate only)
 *   --limit N                process at most N candidates
 *   --asset ADDR             restrict to these assets (repeatable)
 *   --keep FILE              JSON array of asset addresses never to touch
 *   --owner ADDR             custody wallet for a dry run (default 45ZuEb…);
 *                            with --execute the signer IS the owner
 *   --scan-owner ADDR        fork only: owner to scan mainnet for (default
 *                            45ZuEb…). The fork rehearsal patches selected
 *                            assets to a throwaway owner on the fork; this
 *                            lets the mainnet scan still find them.
 *   --json FILE              write the per-ANT plan/result as JSON
 *
 * Env:
 *   RPC_URL            RPC for reads, simulation and sends. Default: public
 *                      mainnet, or http://127.0.0.1:18899 with --cluster fork.
 *   DISCOVERY_RPC_URL  RPC for the getProgramAccounts scans (default RPC_URL).
 *                      For a fork, point it at mainnet: the fork mirrors
 *                      mainnet, and scans of a fork are not reliable.
 *   AUTHORITY_KEYPAIR  keypair JSON path, read only with --execute. Must be
 *                      the custody wallet. Never printed.
 *   CU_PRICE           priority fee, micro-lamports per CU (default 10000)
 */
import fs from 'node:fs';

import {
  ANT_RECORD_DISCRIMINATOR,
  ANT_RECORD_METADATA_DISCRIMINATOR,
  getAntRecordDecoder,
  getAntRecordMetadataDecoder,
  getCloseAntConfigInstruction,
  getCloseAntControllersInstruction,
  getCloseAntRecordInstruction,
  getCloseAntRecordMetadataForOwnerInstruction,
} from '@ar.io/solana-contracts/ant';
import { ARNS_RECORD_DISCRIMINATOR } from '@ar.io/solana-contracts/arns';
import {
  AccountRole,
  address,
  appendTransactionMessageInstructions,
  compileTransaction,
  createKeyPairSignerFromBytes,
  createNoopSigner,
  createSolanaRpc,
  createTransactionMessage,
  getAddressEncoder,
  getBase58Decoder,
  getBase64EncodedWireTransaction,
  getProgramDerivedAddress,
  getUtf8Encoder,
  pipe,
  setTransactionMessageFeePayerSigner,
  setTransactionMessageLifetimeUsingBlockhash,
  signTransactionMessageWithSigners,
} from '@solana/kit';
import { createHash } from 'node:crypto';

// ---------------------------------------------------------------- constants

const PROGRAMS = {
  ant: address('2MWexMHfMhGJwMHv9Qm9YAVCqjUFUJwDJAysW4oCUGk5'),
  arns: address('2yCUx5edFvUrkibYaUa2ZXWyx9kuJkS8CwyzsgHPWdZZ'),
  core: address('CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d'),
};
const SYSTEM_PROGRAM = address('11111111111111111111111111111111');
const COMPUTE_BUDGET = address('ComputeBudget111111111111111111111111111111');
const CUSTODY = '45ZuEb1Jk7pbjshD1BVasBekAXhimdWuJjyswQzMyTB1';
const MPL_KEY_ASSET_V1 = 1;
const ARNS_RECORD_ANT_OFFSET = 72; // 8 disc + 32 owner + 32 name_hash
const MAX_TX_BYTES = 1232;

// ---------------------------------------------------------------- arguments

const argv = process.argv.slice(2);
const flag = (n) => argv.includes(n);
const values = (n) => argv.flatMap((a, i) => (a === n ? [argv[i + 1]] : []));
const value = (n, d) => values(n).at(-1) ?? d;

const CLUSTER = value('--cluster', 'mainnet');
if (!['mainnet', 'fork'].includes(CLUSTER)) die('--cluster must be mainnet or fork');
const EXECUTE = flag('--execute');
const LIMIT = value('--limit') ? Number(value('--limit')) : Infinity;
const ONLY = new Set(values('--asset'));
const KEEP = new Set(value('--keep') ? JSON.parse(fs.readFileSync(value('--keep'), 'utf8')) : []);
const JSON_OUT = value('--json');
const CU_PRICE = BigInt(process.env.CU_PRICE ?? 10_000);

const RPC_URL =
  process.env.RPC_URL ??
  (CLUSTER === 'fork' ? 'http://127.0.0.1:18899' : 'https://api.mainnet-beta.solana.com');
const DISCOVERY_RPC_URL = process.env.DISCOVERY_RPC_URL ?? RPC_URL;
if (CLUSTER === 'fork' && DISCOVERY_RPC_URL === RPC_URL) {
  die('--cluster fork needs DISCOVERY_RPC_URL pointing at mainnet (a fork cannot be scanned)');
}

const rpc = createSolanaRpc(RPC_URL);
const b58 = getBase58Decoder();
const addrEnc = getAddressEncoder();
const utf8 = getUtf8Encoder();

function die(msg) {
  console.error(`ABORT: ${msg}`);
  process.exit(2);
}
const redact = (u) => u.replace(/(quiknode\.pro\/)[^/]+/, '$1<redacted>').replace(/(api-key=)[^&]+/, '$1<redacted>');
const sol = (lamports) => (Number(lamports) / 1e9).toFixed(6);

// ------------------------------------------------------------- rpc helpers

async function raw(url, method, params) {
  for (let attempt = 0; ; attempt++) {
    try {
      const res = await fetch(url, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }),
      });
      const body = await res.json();
      if (body.error) throw new Error(`${method}: ${JSON.stringify(body.error).slice(0, 300)}`);
      return body.result;
    } catch (e) {
      if (attempt >= 4) throw e;
      await new Promise((r) => setTimeout(r, 500 * 2 ** attempt));
    }
  }
}

/** Read accounts (base64) in chunks of 100; null for missing. */
async function readAccounts(addrs) {
  const out = [];
  for (let i = 0; i < addrs.length; i += 100) {
    const r = await raw(RPC_URL, 'getMultipleAccounts', [
      addrs.slice(i, i + 100),
      { encoding: 'base64', commitment: 'confirmed' },
    ]);
    for (const a of r.value) {
      out.push(a && { lamports: BigInt(a.lamports), owner: a.owner, data: Buffer.from(a.data[0], 'base64') });
    }
  }
  return out;
}

const pda = async (program, ...seeds) => (await getProgramDerivedAddress({ programAddress: program, seeds }))[0];

// --------------------------------------------------------- asset parsing

/** Core owner of an AssetV1 (key byte 1, owner at [1,33)). */
function coreOwner(data) {
  if (data.length < 33 || data[0] !== MPL_KEY_ASSET_V1) return null;
  return b58.decode(data.subarray(1, 33));
}

/**
 * Value of a string trait in the Attributes plugin. Attributes are borsh
 * (u32 len + utf8) key/value pairs; we look for the length-prefixed key, so
 * a name that merely contains the text cannot match.
 */
function trait(data, key) {
  const k = Buffer.from(key, 'utf8');
  const needle = Buffer.concat([Buffer.from(Uint32Array.of(k.length).buffer), k]);
  const i = data.indexOf(needle);
  if (i < 0) return null;
  const j = i + needle.length;
  if (j + 4 > data.length) return null;
  const n = data.readUInt32LE(j);
  if (n > 256 || j + 4 + n > data.length) return null;
  return data.subarray(j + 4, j + 4 + n).toString('utf8');
}

// ------------------------------------------------------------- discovery

async function discover(owner) {
  const [assets, arnsRefs, records, metas] = await Promise.all([
    raw(DISCOVERY_RPC_URL, 'getProgramAccounts', [
      PROGRAMS.core,
      {
        encoding: 'base64',
        commitment: 'confirmed',
        filters: [
          { memcmp: { offset: 0, bytes: b58.decode(Uint8Array.of(MPL_KEY_ASSET_V1)) } },
          { memcmp: { offset: 1, bytes: owner } },
        ],
      },
    ]),
    raw(DISCOVERY_RPC_URL, 'getProgramAccounts', [
      PROGRAMS.arns,
      {
        encoding: 'base64',
        commitment: 'confirmed',
        dataSlice: { offset: ARNS_RECORD_ANT_OFFSET, length: 32 },
        filters: [{ memcmp: { offset: 0, bytes: b58.decode(ARNS_RECORD_DISCRIMINATOR) } }],
      },
    ]),
    raw(DISCOVERY_RPC_URL, 'getProgramAccounts', [
      PROGRAMS.ant,
      {
        encoding: 'base64',
        commitment: 'confirmed',
        filters: [{ memcmp: { offset: 0, bytes: b58.decode(ANT_RECORD_DISCRIMINATOR) } }],
      },
    ]),
    raw(DISCOVERY_RPC_URL, 'getProgramAccounts', [
      PROGRAMS.ant,
      {
        encoding: 'base64',
        commitment: 'confirmed',
        filters: [{ memcmp: { offset: 0, bytes: b58.decode(ANT_RECORD_METADATA_DISCRIMINATOR) } }],
      },
    ]),
  ]);

  // Every ANT any ArnsRecord points at — active or in grace — is off limits.
  const referenced = new Set(arnsRefs.map((a) => b58.decode(Buffer.from(a.account.data[0], 'base64'))));

  const recordDec = getAntRecordDecoder();
  const recordsByMint = new Map();
  const undernameByHash = new Map(); // `${mint}:${hex(hash)}` -> undername
  for (const r of records) {
    const d = recordDec.decode(Buffer.from(r.account.data[0], 'base64'));
    const mint = String(d.mint);
    if (!recordsByMint.has(mint)) recordsByMint.set(mint, []);
    recordsByMint.get(mint).push({ address: r.pubkey, undername: d.undername });
    const h = createHash('sha256').update(d.undername.toLowerCase()).digest('hex');
    undernameByHash.set(`${mint}:${h}`, d.undername);
  }
  const metaDec = getAntRecordMetadataDecoder();
  const metasByMint = new Map();
  for (const m of metas) {
    const d = metaDec.decode(Buffer.from(m.account.data[0], 'base64'));
    const mint = String(d.mint);
    if (!metasByMint.has(mint)) metasByMint.set(mint, []);
    metasByMint.get(mint).push({
      address: m.pubkey,
      undername: undernameByHash.get(`${mint}:${Buffer.from(d.undernameHash).toString('hex')}`) ?? null,
    });
  }

  const owned = assets.map((a) => ({ asset: a.pubkey, data: Buffer.from(a.account.data[0], 'base64') }));
  const counts = { owned: owned.length, referenced: 0, neverNamed: 0, kept: 0, notSelected: 0 };
  const candidates = [];
  for (const { asset, data } of owned) {
    if (ONLY.size && !ONLY.has(asset)) { counts.notSelected++; continue; }
    if (KEEP.has(asset)) { counts.kept++; continue; }
    if (referenced.has(asset)) { counts.referenced++; continue; }
    const name = trait(data, 'ArNS Name');
    if (!name) { counts.neverNamed++; continue; }
    candidates.push({
      asset,
      name,
      records: recordsByMint.get(asset) ?? [],
      metas: metasByMint.get(asset) ?? [],
    });
  }
  candidates.sort((a, b) => a.name.localeCompare(b.name));
  return { counts, candidates };
}

// ----------------------------------------------------------- instructions

const computeBudget = (units) => [
  { programAddress: COMPUTE_BUDGET, accounts: [], data: Uint8Array.from([2, ...u32(units)]) },
  { programAddress: COMPUTE_BUDGET, accounts: [], data: Uint8Array.from([3, ...u64(CU_PRICE)]) },
];
function u32(n) { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; }
function u64(n) { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; }

/** MPL Core BurnV1: data [12, 0] (no compression proof). Asset rent -> payer. */
const burnV1 = (asset, signer) => ({
  programAddress: PROGRAMS.core,
  accounts: [
    { address: address(asset), role: AccountRole.WRITABLE },
    { address: PROGRAMS.core, role: AccountRole.READONLY }, // collection: None
    { address: signer.address, role: AccountRole.WRITABLE_SIGNER, signer }, // payer
    { address: signer.address, role: AccountRole.READONLY_SIGNER, signer }, // authority (owner)
    { address: SYSTEM_PROGRAM, role: AccountRole.READONLY },
    { address: PROGRAMS.core, role: AccountRole.READONLY }, // log wrapper: None
  ],
  data: Uint8Array.from([12, 0]),
});

const opts = { programAddress: PROGRAMS.ant };

// -------------------------------------------------------------- tx plumbing

async function build(instructions, signer, units) {
  const { value: bh } = await rpc.getLatestBlockhash({ commitment: 'confirmed' }).send();
  const msg = pipe(
    createTransactionMessage({ version: 0 }),
    (m) => setTransactionMessageFeePayerSigner(signer, m),
    (m) => setTransactionMessageLifetimeUsingBlockhash(bh, m),
    (m) => appendTransactionMessageInstructions([...computeBudget(units), ...instructions], m),
  );
  return msg;
}

async function simulate(msg) {
  const wire = getBase64EncodedWireTransaction(compileTransaction(msg));
  const size = Buffer.from(wire, 'base64').length;
  if (size > MAX_TX_BYTES) return { ok: false, size, err: `transaction too large (${size} B)`, logs: [] };
  const r = await raw(RPC_URL, 'simulateTransaction', [
    wire,
    { encoding: 'base64', sigVerify: false, replaceRecentBlockhash: true, commitment: 'confirmed' },
  ]);
  return { ok: r.value.err === null, size, err: r.value.err, logs: r.value.logs ?? [], units: r.value.unitsConsumed };
}

async function send(msg) {
  const signed = await signTransactionMessageWithSigners(msg);
  const wire = getBase64EncodedWireTransaction(signed);
  const sig = await raw(RPC_URL, 'sendTransaction', [
    wire,
    { encoding: 'base64', skipPreflight: false, preflightCommitment: 'confirmed', maxRetries: 5 },
  ]);
  for (let i = 0; i < 90; i++) {
    const s = await raw(RPC_URL, 'getSignatureStatuses', [[sig], { searchTransactionHistory: true }]);
    const st = s.value[0];
    if (st?.err) throw new Error(`tx ${sig} failed: ${JSON.stringify(st.err)}`);
    if (st && (st.confirmationStatus === 'confirmed' || st.confirmationStatus === 'finalized')) return sig;
    await new Promise((r) => setTimeout(r, 1000));
  }
  throw new Error(`tx ${sig} not confirmed in 90s — re-run to resume from chain state`);
}

// ------------------------------------------------------------------ per ANT

let TOMBSTONE_RENT = 0n;

async function planFor(c, owner) {
  const config = await pda(PROGRAMS.ant, utf8.encode('ant_config'), addrEnc.encode(address(c.asset)));
  const controllers = await pda(PROGRAMS.ant, utf8.encode('ant_controllers'), addrEnc.encode(address(c.asset)));
  const addrs = [c.asset, config, controllers, ...c.metas.map((m) => m.address), ...c.records.map((r) => r.address)];
  const read = await readAccounts(addrs);
  // A closed account can read back as a 0-lamport, System-owned shell rather
  // than as missing; only a funded ario-ant account counts as open state.
  const live = (a) => (a && a.owner === PROGRAMS.ant && a.lamports > 0n ? a : null);
  const asset = read[0];
  const accts = [asset, ...read.slice(1).map(live)];
  const [, cfg, ctl] = accts;
  const metaAccts = accts.slice(3, 3 + c.metas.length);
  const recAccts = accts.slice(3 + c.metas.length);

  const plan = {
    asset: c.asset, name: c.name, config, controllers,
    metas: c.metas.filter((_, i) => metaAccts[i]),
    records: c.records.filter((_, i) => recAccts[i]),
    hasConfig: !!cfg, hasControllers: !!ctl,
    lamports: 0n, status: 'pending',
  };

  // Live, freshly-read guards. Any failure skips the ANT.
  if (!asset || asset.owner !== PROGRAMS.core || asset.data[0] !== MPL_KEY_ASSET_V1) {
    const leftover = !!cfg || !!ctl || plan.records.length > 0 || plan.metas.length > 0;
    plan.status = leftover ? 'skip:asset-gone-state-left' : 'done';
    return plan;
  }
  const liveOwner = coreOwner(asset.data);
  if (liveOwner !== owner) { plan.status = `skip:owner-is-${liveOwner}`; return plan; }
  if (!trait(asset.data, 'ArNS Name')) { plan.status = 'skip:no-arns-name-trait'; return plan; }
  if (!cfg || !ctl) { plan.status = 'skip:missing-config-or-controllers'; return plan; }
  if (plan.metas.some((m) => m.undername === null)) { plan.status = 'skip:orphan-metadata'; return plan; }

  // Targeted re-check right before acting: nothing may point at this ANT.
  const refs = await raw(DISCOVERY_RPC_URL, 'getProgramAccounts', [
    PROGRAMS.arns,
    {
      encoding: 'base64',
      dataSlice: { offset: 0, length: 0 },
      filters: [
        { memcmp: { offset: 0, bytes: b58.decode(ARNS_RECORD_DISCRIMINATOR) } },
        { memcmp: { offset: ARNS_RECORD_ANT_OFFSET, bytes: c.asset } },
      ],
    },
  ]);
  if (refs.length) { plan.status = 'skip:referenced-by-arns-record'; return plan; }

  // Asset + every state account still open, less the 1-byte tombstone MPL
  // Core BurnV1 leaves behind (it keeps the 1-byte rent minimum and stays
  // Core-owned, so that part is not reclaimable).
  plan.lamports = accts.reduce((t, a) => t + (a ? a.lamports : 0n), 0n) - TOMBSTONE_RENT;
  return plan;
}

/**
 * Pack the ANT's instructions into as few transactions as fit, in order:
 * metadata/record closes first, then controllers + config + burn. Usually
 * everything fits in ONE transaction, which makes the ANT atomic. When it
 * does not, earlier transactions carry only closes and the burn is last, so
 * an interruption never burns an asset with state still open.
 */
async function txsFor(plan, signer) {
  const asset = address(plan.asset);
  const closes = [
    ...plan.metas.map((m) =>
      getCloseAntRecordMetadataForOwnerInstruction(
        { asset, recordMetadata: address(m.address), caller: signer, undername: m.undername },
        opts,
      ),
    ),
    ...plan.records.map((r) =>
      getCloseAntRecordInstruction({ asset, record: address(r.address), caller: signer, undername: r.undername }, opts),
    ),
  ];
  const finals = [
    getCloseAntControllersInstruction({ asset, controllers: address(plan.controllers), caller: signer }, opts),
    getCloseAntConfigInstruction({ asset, config: address(plan.config), caller: signer }, opts),
    burnV1(plan.asset, signer),
  ];
  const units = (n) => 20_000 * n + 40_000;
  const fits = (ixs) => sizeOf(ixs, signer) <= MAX_TX_BYTES;

  const txs = [];
  let cur = [];
  for (const ix of closes) {
    if (cur.length && !fits([...cur, ix])) {
      txs.push({ kind: 'records', ixs: cur, units: units(cur.length) });
      cur = [];
    }
    cur.push(ix);
  }
  if (fits([...cur, ...finals])) {
    txs.push({ kind: cur.length ? `${cur.length} closes+controllers+config+burn` : 'controllers+config+burn', ixs: [...cur, ...finals], units: units(cur.length + finals.length) });
  } else {
    if (cur.length) txs.push({ kind: 'records', ixs: cur, units: units(cur.length) });
    txs.push({ kind: 'controllers+config+burn', ixs: finals, units: units(finals.length) });
  }
  txs.forEach((t, i) => { if (t.kind === 'records') t.kind = `records (${t.ixs.length})`; t.kind = `tx ${i + 1}/${txs.length} ${t.kind}`; });
  return txs;
}

/** Wire size of a transaction carrying `ixs`, with a placeholder blockhash. */
function sizeOf(ixs, signer) {
  const msg = pipe(
    createTransactionMessage({ version: 0 }),
    (m) => setTransactionMessageFeePayerSigner(signer, m),
    (m) => setTransactionMessageLifetimeUsingBlockhash({ blockhash: '11111111111111111111111111111111', lastValidBlockHeight: 0n }, m),
    (m) => appendTransactionMessageInstructions([...computeBudget(0), ...ixs], m),
  );
  return Buffer.from(getBase64EncodedWireTransaction(compileTransaction(msg)), 'base64').length;
}

async function verifyDone(plan) {
  const addrs = [plan.asset, plan.config, plan.controllers, ...plan.metas.map((m) => m.address), ...plan.records.map((r) => r.address)];
  const accts = await readAccounts(addrs);
  const [asset, ...rest] = accts;
  const assetBurned = !asset || asset.owner !== PROGRAMS.core || asset.data[0] !== MPL_KEY_ASSET_V1;
  const stateGone = rest.every((a) => !a || (a.owner === SYSTEM_PROGRAM && a.data.length === 0));
  return { assetBurned, stateGone, assetAfter: asset && { owner: asset.owner, len: asset.data.length, lamports: String(asset.lamports) } };
}

// --------------------------------------------------------------------- main

async function main() {
  console.log(`=== reclaim expired ANTs — cluster ${CLUSTER} — ${EXECUTE ? 'EXECUTE' : 'DRY RUN'} ===`);
  console.log(`rpc ${redact(RPC_URL)}${DISCOVERY_RPC_URL !== RPC_URL ? `  (scans: ${redact(DISCOVERY_RPC_URL)})` : ''}`);

  let signer;
  let owner;
  if (EXECUTE) {
    const kp = process.env.AUTHORITY_KEYPAIR;
    if (!kp) die('--execute needs AUTHORITY_KEYPAIR');
    signer = await createKeyPairSignerFromBytes(Uint8Array.from(JSON.parse(fs.readFileSync(kp, 'utf8'))));
    owner = signer.address;
    if (CLUSTER === 'mainnet' && owner !== CUSTODY) die(`signer ${owner} is not the custody wallet ${CUSTODY}`);
  } else {
    owner = value('--owner', CUSTODY);
    signer = createNoopSigner(address(owner));
  }
  const startBal = BigInt((await raw(RPC_URL, 'getBalance', [owner, { commitment: 'confirmed' }])).value);
  TOMBSTONE_RENT = BigInt(await raw(RPC_URL, 'getMinimumBalanceForRentExemption', [1]));
  console.log(`owner ${owner}  balance ${sol(startBal)} SOL`);

  // On a fork the scans read mainnet, where the custody wallet still owns the
  // assets; every live check below still requires the fork owner == signer.
  const scanOwner = CLUSTER === 'fork' ? value('--scan-owner', CUSTODY) : owner;
  if (scanOwner !== owner) console.log(`scanning mainnet for assets of ${scanOwner} (fork rehearsal)`);
  const { counts, candidates } = await discover(scanOwner);
  console.log(
    `assets owned ${counts.owned} | still referenced by an ArNS record ${counts.referenced} | never named ${counts.neverNamed} | kept ${counts.kept}` +
      `${ONLY.size ? ` | outside --asset ${counts.notSelected}` : ''} | candidates ${candidates.length}`,
  );

  const results = [];
  let expected = 0n;
  let n = 0;
  for (const c of candidates) {
    if (n >= LIMIT) break;
    n++;
    const plan = await planFor(c, owner);
    const tag = `[${n}/${Math.min(candidates.length, LIMIT)}] ${plan.name} ${plan.asset}`;
    if (plan.status !== 'pending') {
      console.log(`${tag}  ${plan.status}`);
      results.push({ ...plan, lamports: String(plan.lamports) });
      continue;
    }
    expected += plan.lamports;
    const txs = await txsFor(plan, signer);
    console.log(`${tag}  records ${plan.records.length} metas ${plan.metas.length}  reclaimable ${sol(plan.lamports)} SOL  txs ${txs.length}`);

    const sent = [];
    for (const tx of txs) {
      const msg = await build(tx.ixs, signer, tx.units);
      const sim = await simulate(msg);
      if (!sim.ok) {
        console.log(`    ${tx.kind}: SIMULATION FAILED (${sim.size} B) ${JSON.stringify(sim.err)}`);
        for (const l of sim.logs.slice(-8)) console.log(`      ${l}`);
        plan.status = 'simulation-failed';
        break;
      }
      if (!EXECUTE) {
        console.log(`    ${tx.kind}: simulates ok (${sim.size} B, ${sim.units} CU)`);
        // In a dry run later transactions are simulated against state that
        // does not yet reflect earlier ones. Record closes are independent of
        // each other and of the final transaction, so each still simulates.
        continue;
      }
      const sig = await send(msg);
      sent.push({ kind: tx.kind, sig });
      console.log(`    ${tx.kind}: ${sig}`);
    }

    if (plan.status === 'simulation-failed') {
      results.push({ ...plan, lamports: String(plan.lamports), sent });
      console.log('Stopping at the first failure. Nothing after it was sent.');
      break;
    }
    if (EXECUTE) {
      const v = await verifyDone(plan);
      plan.status = v.assetBurned && v.stateGone ? 'done' : 'incomplete';
      console.log(`    verify: asset burned ${v.assetBurned}  state closed ${v.stateGone}  asset after ${JSON.stringify(v.assetAfter)}`);
      results.push({ ...plan, lamports: String(plan.lamports), sent, verify: v });
      if (plan.status !== 'done') { console.log('Stopping: ANT not fully reclaimed. Re-run to resume.'); break; }
    } else {
      plan.status = 'dry-run-ok';
      results.push({ ...plan, lamports: String(plan.lamports) });
    }
  }

  const summary = {};
  for (const r of results) summary[r.status] = (summary[r.status] ?? 0) + 1;
  console.log(`\nprocessed ${results.length}: ${JSON.stringify(summary)}`);
  console.log(`reclaimable rent in the processed ANTs: ${sol(expected)} SOL (each burn leaves a ${TOMBSTONE_RENT}-lamport 1-byte tombstone; fees not included)`);
  if (EXECUTE) {
    const endBal = BigInt((await raw(RPC_URL, 'getBalance', [owner, { commitment: 'confirmed' }])).value);
    console.log(`owner balance ${sol(startBal)} -> ${sol(endBal)} SOL (net ${sol(endBal - startBal)}, after fees)`);
  }
  if (JSON_OUT) fs.writeFileSync(JSON_OUT, JSON.stringify(results, null, 1));
  const failed = results.some((r) => r.status === 'simulation-failed' || r.status === 'incomplete');
  process.exit(failed ? 1 : 0);
}

main().catch((e) => {
  console.error(`ERROR: ${e.message}`);
  process.exit(2);
});
