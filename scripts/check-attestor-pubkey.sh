#!/usr/bin/env bash
#
# Pre-deploy guardrail: fail if `ATTESTOR_PUBKEY` in
# programs/ario-ant-escrow/src/state.rs still equals the deterministic
# test value (derived from secret seed `[1u8; 32]`, base58
# `AKnL4NNf3DGWZJS6cPknBuEGnVsV4A4m5tgebLHaRSZ9`).
#
# Why this exists: the test value is intentionally checked into source
# so localnet integration tests can construct valid Ed25519Program
# sigverify ixs without external setup. Deploying that constant to
# devnet/mainnet would let anyone with the test secret seed (i.e.,
# anyone reading the source) mint valid attestations and drain
# escrows.
#
# Usage:
#   ./check-attestor-pubkey.sh         # warn-only (suitable for CI / pre-build)
#   ./check-attestor-pubkey.sh --strict # exit 1 on test-value detection
#
# `devnet-deploy.sh` and any future mainnet-deploy script MUST call
# this with `--strict` before any `solana program deploy` step.
#
# Replacement runbook (when this fails):
#   1. Clone ar-io/ar-io-solana-attestor, then `yarn install && yarn keygen`
#      → records ATTESTOR_SECRET_BASE58 in your secret manager
#      → prints ATTESTOR_PUBKEY_BASE58 to stdout
#   2. Replace `pub const ATTESTOR_PUBKEY: Pubkey = ...` in
#      programs/ario-ant-escrow/src/state.rs with the printed pubkey.
#   3. Rebuild (`./build-sbf.sh --sync` or `anchor build`).
#   4. Re-run this script — it should pass.
#   5. Provision the secret to the attestor service's secret manager
#      and restart it.

set -euo pipefail

STRICT=0
if [[ "${1:-}" == "--strict" ]]; then
  STRICT=1
fi

# Resolve to the state.rs file regardless of where the script was invoked.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
STATE_RS="${SCRIPT_DIR}/../programs/ario-ant-escrow/src/state.rs"

if [[ ! -f "${STATE_RS}" ]]; then
  echo "[check-attestor-pubkey] ERROR: cannot find ${STATE_RS}" >&2
  exit 2
fi

TEST_VALUE='AKnL4NNf3DGWZJS6cPknBuEGnVsV4A4m5tgebLHaRSZ9'

# Match the constant's declaration and its value on the next line(s),
# tolerating either single- or multi-line `pub const ATTESTOR_PUBKEY ... pubkey!("...");`.
if grep -A 2 'pub const ATTESTOR_PUBKEY' "${STATE_RS}" | grep -q "${TEST_VALUE}"; then
  if [[ "${STRICT}" -eq 1 ]]; then
    cat >&2 <<EOF
========================================================================
  DEPLOY ABORTED — ATTESTOR_PUBKEY is still the test value
========================================================================

  state.rs has:
    pub const ATTESTOR_PUBKEY: Pubkey =
        solana_program::pubkey!("${TEST_VALUE}");

  This is the deterministic test pubkey derived from secret seed
  [1u8; 32] — public to anyone reading the source. Deploying it to
  any cluster that holds real value would let anyone forge attestations.

  Replace before deploying:
    1. Clone ar-io/ar-io-solana-attestor, then yarn install && yarn keygen
    2. Paste the printed ATTESTOR_PUBKEY_BASE58 into state.rs
    3. Rebuild and re-run this script
    4. Store the secret in your secret manager (KMS / Vault / etc.)

  See the ar-io/ar-io-solana-attestor repo's README § "Key rotation" for the full runbook.
========================================================================
EOF
    exit 1
  else
    echo "[check-attestor-pubkey] WARN: ATTESTOR_PUBKEY is the test value." >&2
    echo "[check-attestor-pubkey]       OK for local dev / tests; MUST be replaced before deploying." >&2
    exit 0
  fi
fi

echo "[check-attestor-pubkey] OK: ATTESTOR_PUBKEY is not the test value."
exit 0
