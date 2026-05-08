#!/usr/bin/env bash
# ============================================================
# AR.IO Solana — Devnet Authority Funding
# ============================================================
# Drips devnet SOL into whichever keypair `solana config get`
# is pointing at, until balance >= TARGET_SOL.
#
# Devnet airdrop is rate-limited (2 SOL per request, throttled
# after a few rapid requests). This script handles backoff
# automatically and resumes safely if interrupted.
#
# Run from: solana-ar-io/contracts (or anywhere — uses absolute
# config). Verify before running:
#   solana config get      # keypair = devnet-authority, RPC = devnet
#   solana address         # the address that will be funded
#
# If this stalls on rate limiting, fall back to:
#   - https://faucet.solana.com  (paste the authority address)
#   - solana transfer <authority> <amount>  (from another devnet wallet)
# ============================================================

set -uo pipefail

TARGET_SOL=${TARGET_SOL:-115}
SLEEP_BASE=${SLEEP_BASE:-10}
SLEEP_BACKOFF=${SLEEP_BACKOFF:-30}
MAX_ATTEMPTS=${MAX_ATTEMPTS:-200}
ATTEMPT=0

get_balance() {
  solana balance 2>/dev/null | awk '{print int($1)}'
}

BALANCE=$(get_balance)
echo "Authority: $(solana address)"
echo "Cluster:   $(solana config get | awk '/RPC URL/ {print $3}')"
echo "Starting balance: ${BALANCE} SOL, target: ${TARGET_SOL} SOL"

while [ "${BALANCE:-0}" -lt "${TARGET_SOL}" ] && [ "${ATTEMPT}" -lt "${MAX_ATTEMPTS}" ]; do
  ATTEMPT=$((ATTEMPT + 1))
  if solana airdrop 2 2>/dev/null; then
    echo "[${ATTEMPT}] OK airdrop succeeded"
    sleep "${SLEEP_BASE}"
  else
    echo "[${ATTEMPT}] -- rate limited, waiting ${SLEEP_BACKOFF}s"
    sleep "${SLEEP_BACKOFF}"
  fi
  BALANCE=$(get_balance)
  echo "    Balance: ${BALANCE} SOL"
done

if [ "${BALANCE:-0}" -lt "${TARGET_SOL}" ]; then
  echo ""
  echo "ERROR: Could not reach ${TARGET_SOL} SOL after ${MAX_ATTEMPTS} attempts."
  echo "Current balance: ${BALANCE} SOL"
  echo ""
  echo "Options:"
  echo "  1. Wait a few minutes and re-run this script (it resumes from current balance)"
  echo "  2. Use https://faucet.solana.com (5 SOL/req with captcha)"
  echo "  3. Fund from another devnet wallet: solana transfer $(solana address) <amount>"
  exit 1
fi

echo ""
echo "OK Funded: ${BALANCE} SOL (target was ${TARGET_SOL})"
