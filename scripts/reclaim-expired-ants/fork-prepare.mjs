#!/usr/bin/env node
/**
 * Surfpool fork only: hand selected ANTs to a throwaway key so reclaim.mjs
 * can be rehearsed end to end without the custody key.
 *
 * For each --asset it rewrites the MPL Core AssetV1 owner field (bytes
 * [1,33)) to the throwaway pubkey, leaving every other byte as it was, then
 * airdrops the throwaway key. Refuses to talk to anything but localhost.
 *
 * Surfpool charges more rent than mainnet (897,840 vs 655,320 lamports for a
 * 1-byte account on 2026-10-15). MPL Core BurnV1 leaves a 1-byte tombstone
 * funded at the fork's rate, which the fork then rejects as under-funded. So
 * each patched asset is also topped up by the fork/mainnet rent difference
 * for its size; reclaim.mjs reports the delta.
 *
 *   node fork-prepare.mjs --keypair /tmp/throwaway.json --asset A --asset B
 *   RPC_URL=http://127.0.0.1:18899 (default)
 */
import fs from 'node:fs';
import { createKeyPairSignerFromBytes, getAddressEncoder, address } from '@solana/kit';
import { webcrypto } from 'node:crypto';

const argv = process.argv.slice(2);
const values = (n) => argv.flatMap((a, i) => (a === n ? [argv[i + 1]] : []));
const RPC = process.env.RPC_URL ?? 'http://127.0.0.1:18899';
if (!/^http:\/\/(127\.0\.0\.1|localhost):\d+/.test(RPC)) {
  console.error('ABORT: fork-prepare only talks to a local Surfpool fork');
  process.exit(2);
}
const KP = values('--keypair').at(-1);
const ASSETS = values('--asset');
if (!KP || !ASSETS.length) {
  console.error('usage: fork-prepare.mjs --keypair FILE --asset ADDR [--asset ADDR ...]');
  process.exit(2);
}

const call = async (method, params) => {
  const r = await (await fetch(RPC, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify({ jsonrpc: '2.0', id: 1, method, params }) })).json();
  if (r.error) throw new Error(`${method}: ${JSON.stringify(r.error)}`);
  return r.result;
};

// Load or create the throwaway keypair (64-byte secret+public JSON array).
let signer;
if (fs.existsSync(KP)) {
  signer = await createKeyPairSignerFromBytes(Uint8Array.from(JSON.parse(fs.readFileSync(KP, 'utf8'))));
} else {
  const pair = await webcrypto.subtle.generateKey({ name: 'Ed25519' }, true, ['sign', 'verify']);
  const pkcs8 = new Uint8Array(await webcrypto.subtle.exportKey('pkcs8', pair.privateKey));
  const pub = new Uint8Array(await webcrypto.subtle.exportKey('raw', pair.publicKey));
  const bytes = Uint8Array.from([...pkcs8.slice(-32), ...pub]);
  fs.writeFileSync(KP, JSON.stringify([...bytes]), { mode: 0o600 });
  signer = await createKeyPairSignerFromBytes(bytes);
}
const owner = getAddressEncoder().encode(address(signer.address));
console.log(`throwaway owner ${signer.address}`);

for (const a of ASSETS) {
  const acct = (await call('getAccountInfo', [a, { encoding: 'base64' }])).value;
  if (!acct || acct.owner !== 'CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d') throw new Error(`${a} is not an MPL Core account on the fork`);
  const data = Buffer.from(acct.data[0], 'base64');
  if (data[0] !== 1) throw new Error(`${a} is not an AssetV1`);
  const patched = Buffer.from(data);
  patched.set(owner, 1);
  // Fork-only top-up: Surfpool's rent schedule is higher than mainnet's.
  const forkRent = (await call('getMinimumBalanceForRentExemption', [data.length]));
  const topup = Math.max(0, forkRent - acct.lamports);
  await call('surfnet_setAccount', [a, { lamports: acct.lamports + topup, data: patched.toString('hex'), owner: acct.owner, executable: false }]);
  if (topup) console.log(`${a}: topped up ${topup} lamports (fork rent artifact)`);
  const after = Buffer.from((await call('getAccountInfo', [a, { encoding: 'base64' }])).value.data[0], 'base64');
  const ok = after.subarray(1, 33).equals(Buffer.from(owner)) && after.subarray(0, 1).equals(data.subarray(0, 1)) && after.subarray(33).equals(data.subarray(33));
  console.log(`${a}: owner patched ${ok ? 'OK (only bytes 1..33 changed)' : 'MISMATCH'}`);
  if (!ok) process.exit(1);
}
await call('requestAirdrop', [signer.address, 10_000_000_000]);
await new Promise((r) => setTimeout(r, 1500));
console.log(`balance ${(await call('getBalance', [signer.address])).value / 1e9} SOL`);
