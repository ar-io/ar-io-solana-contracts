#!/usr/bin/env node
// Bridge balance monitor (Solana <-> Base).
//
// Confirms the AR.IO Solana bridge custody wallet holds enough ARIO to
// back the circulating bridged ARIO ERC-20 supply on Base. Solana is the
// canonical custody side (it replaced AO when the token was forked from AO
// to Solana); Base holds the wrapped/bridged representation.
//
// Invariant (must hold at rest):
//   sum(ARIO token accounts owned by BRIDGE_WALLET on Solana, in mARIO)
//     == ERC-20 totalSupply() of the Base ARIO contract (in mARIO)
//
// Both sides use 6 decimals, so the smallest-unit ("mARIO") integers are
// directly comparable. This script only *fetches and reports*; the GitHub
// workflow decides the alert policy (currently exact equality) and retries
// to ride out transient in-flight bridging.
//
// Output: a single JSON object on stdout (progress/errors go to stderr) so
// the workflow can parse it with jq. Exits non-zero only on a fetch/RPC
// failure -- a balance mismatch still exits 0 with `"match": false`.
//
// Zero runtime dependencies: raw JSON-RPC via global fetch (Node >= 18).

// --- Defaults (overridable via env so the workflow can wire repo vars) ---
const SOLANA_RPC_URL =
  envOr('SOLANA_RPC_URL', 'https://api.mainnet-beta.solana.com');
const BRIDGE_WALLET =
  envOr('BRIDGE_WALLET', '3mDzV8NHqRZ8864FFr6BFQ49wYJbKCTPNd5RhDYRgEkm');
const ARIO_MINT =
  envOr('ARIO_MINT', 'DcNnMuFxwhgV4WY1HVSaSEgr92bv2b1vUvEKiNxWqHdF');

const BASE_RPC_URL = envOr('BASE_RPC_URL', 'https://mainnet.base.org');
const BASE_TOKEN_ADDRESS =
  envOr('BASE_TOKEN_ADDRESS', '0x138746adfA52909E5920def027f5a8dc1C7EfFb6');

// Expected steady-state delta of (Solana custody - Base supply), in mARIO.
// A known, intentional offset is expected (e.g. a reconciliation/buffer
// amount), so a *healthy* bridge sits at exactly this delta rather than at
// zero. The check alerts when the observed delta drifts away from this value.
// Default 0 = strict parity. May be negative.
const EXPECTED_DELTA_MARIO = parseBigIntEnv('EXPECTED_DELTA_MARIO', '0');

function parseBigIntEnv(name, fallback) {
  const raw = envOr(name, fallback);
  try {
    return BigInt(raw);
  } catch {
    throw new Error(`${name} is not a valid integer (mARIO): "${raw}"`);
  }
}

// ERC-20 selectors.
const SELECTOR_TOTAL_SUPPLY = '0x18160ddd'; // totalSupply()
const SELECTOR_DECIMALS = '0x313ce567'; // decimals()

function envOr(name, fallback) {
  const v = process.env[name];
  return v && v.trim() !== '' ? v.trim() : fallback;
}

function log(...args) {
  console.error(...args);
}

async function rpc(url, body, { retries = 2 } = {}) {
  let lastErr;
  for (let attempt = 0; attempt <= retries; attempt++) {
    try {
      const res = await fetch(url, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });
      if (!res.ok) {
        throw new Error(`HTTP ${res.status} ${res.statusText}`);
      }
      const json = await res.json();
      if (json.error) {
        throw new Error(`RPC error: ${JSON.stringify(json.error)}`);
      }
      return json.result;
    } catch (err) {
      lastErr = err;
      log(`  RPC attempt ${attempt + 1} failed: ${err.message}`);
      if (attempt < retries) await sleep(1000 * (attempt + 1));
    }
  }
  throw lastErr;
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// --- Solana custody side ---------------------------------------------------
async function getSolanaCustody() {
  log('Fetching Solana ARIO mint decimals...');
  const mintInfo = await rpc(SOLANA_RPC_URL, {
    jsonrpc: '2.0',
    id: 1,
    method: 'getAccountInfo',
    params: [ARIO_MINT, { encoding: 'jsonParsed' }],
  });
  const decimals = mintInfo?.value?.data?.parsed?.info?.decimals;
  if (decimals === undefined) {
    throw new Error(`Could not read decimals for mint ${ARIO_MINT}`);
  }

  log(`Fetching Solana bridge custody (${BRIDGE_WALLET})...`);
  const accounts = await rpc(SOLANA_RPC_URL, {
    jsonrpc: '2.0',
    id: 1,
    method: 'getTokenAccountsByOwner',
    params: [
      BRIDGE_WALLET,
      { mint: ARIO_MINT },
      { encoding: 'jsonParsed' },
    ],
  });

  // Sum every ARIO token account the wallet owns (usually just the ATA, but
  // be robust to more than one).
  let total = 0n;
  for (const acc of accounts?.value ?? []) {
    const amount = acc?.account?.data?.parsed?.info?.tokenAmount?.amount;
    if (amount !== undefined) total += BigInt(amount);
  }

  return { mARIO: total, decimals };
}

// --- Base bridged-supply side ----------------------------------------------
async function getBaseSupply() {
  log(`Fetching Base ARIO totalSupply (${BASE_TOKEN_ADDRESS})...`);
  const supplyHex = await rpc(BASE_RPC_URL, {
    jsonrpc: '2.0',
    id: 1,
    method: 'eth_call',
    params: [{ to: BASE_TOKEN_ADDRESS, data: SELECTOR_TOTAL_SUPPLY }, 'latest'],
  });
  const decimalsHex = await rpc(BASE_RPC_URL, {
    jsonrpc: '2.0',
    id: 1,
    method: 'eth_call',
    params: [{ to: BASE_TOKEN_ADDRESS, data: SELECTOR_DECIMALS }, 'latest'],
  });

  if (!supplyHex || supplyHex === '0x') {
    throw new Error('Empty totalSupply() result from Base RPC');
  }
  return {
    mARIO: BigInt(supplyHex),
    decimals: Number(BigInt(decimalsHex)),
  };
}

function formatArio(mARIO, decimals) {
  const base = 10n ** BigInt(decimals);
  const whole = mARIO / base;
  const frac = (mARIO % base).toString().padStart(decimals, '0');
  return `${whole}.${frac}`;
}

async function main() {
  const [solana, base] = await Promise.all([
    getSolanaCustody(),
    getBaseSupply(),
  ]);

  // Both sides must be 6-decimal to compare as mARIO. Equality alone isn't
  // enough: if both RPCs were pointed at a 9- or 18-decimal asset, the units
  // would line up with each other but mean something other than mARIO.
  if (solana.decimals !== 6 || base.decimals !== 6) {
    throw new Error(
      `Unexpected decimals: Solana=${solana.decimals} Base=${base.decimals} ` +
        `-- expected both sides to use 6-decimal mARIO units`,
    );
  }
  const decimals = solana.decimals;

  const deltaMARIO = solana.mARIO - base.mARIO; // >0 => over-collateralized
  // "Drift" is how far the observed delta is from the expected steady-state
  // delta; a healthy bridge has zero drift.
  const driftMARIO = deltaMARIO - EXPECTED_DELTA_MARIO;
  const match = driftMARIO === 0n;

  const abs = (v) => (v < 0n ? -v : v);

  log(
    `Solana custody: ${formatArio(solana.mARIO, decimals)} ARIO ` +
      `(${solana.mARIO} mARIO)`,
  );
  log(
    `Base supply   : ${formatArio(base.mARIO, decimals)} ARIO ` +
      `(${base.mARIO} mARIO)`,
  );
  log(
    `Delta (Solana - Base): ${deltaMARIO} mARIO  |  ` +
      `expected: ${EXPECTED_DELTA_MARIO} mARIO`,
  );
  log(
    match
      ? '✅ Delta matches expected'
      : `❌ Drift from expected delta = ${formatArio(
          abs(driftMARIO),
          decimals,
        )} ARIO (${driftMARIO} mARIO)`,
  );

  // Machine-readable result for the workflow (stdout only).
  process.stdout.write(
    JSON.stringify(
      {
        match,
        decimals,
        deltaMARIO: deltaMARIO.toString(),
        expectedDeltaMARIO: EXPECTED_DELTA_MARIO.toString(),
        driftMARIO: driftMARIO.toString(),
        solana: {
          wallet: BRIDGE_WALLET,
          mint: ARIO_MINT,
          mARIO: solana.mARIO.toString(),
          ARIO: formatArio(solana.mARIO, decimals),
        },
        base: {
          token: BASE_TOKEN_ADDRESS,
          mARIO: base.mARIO.toString(),
          ARIO: formatArio(base.mARIO, decimals),
        },
      },
      null,
      2,
    ) + '\n',
  );
}

main().catch((err) => {
  log(`Fatal: ${err.message}`);
  process.exit(1);
});
