#!/usr/bin/env bash
# ============================================================
# AR.IO Solana Contracts — Mainnet Upgrade Preparation
# ============================================================
#
# Mainnet program-upgrade authority is a Squads V4 multisig (see
# repo README → "Release flow"). CI never holds the upgrade key, and
# we never run `anchor deploy` against mainnet directly. Instead we:
#
#   1. Build the .so files with mainnet feature flags.
#   2. Pre-upload each .so to a write buffer using a transient buffer
#      authority that this script controls (a hot wallet that pays
#      buffer rent).
#   3. Transfer buffer authority to the multisig.
#   4. Print the parameters needed to draft an
#      `Upgrade(program, buffer, spill, authority)` instruction inside
#      the multisig — signers then approve and execute.
#
# Pattern reference: solana program write-buffer + solana program
# set-buffer-authority + Squads V4 transaction proposal:
#   https://docs.solanalabs.com/cli/examples/deploy-a-program#redeploy-a-program
#   https://docs.squads.so/main/development/dapps-integrations/squads-v4-protocol
#
# This script does the offline / preparation pieces. The proposal step
# is intentionally manual / done from the multisig dashboard — we want
# a human to verify the buffer hash before voting.
#
# Inputs (env):
#   BUFFER_AUTHORITY_KEY_JSON   # raw JSON keypair contents of the hot
#                                 wallet that pays buffer rent (CI mode;
#                                 preferred — bytes are fed to solana via
#                                 process substitution and never written
#                                 to disk). Takes precedence over
#                                 BUFFER_AUTHORITY_KEYPAIR if both set.
#   BUFFER_AUTHORITY_KEYPAIR    # fallback for local runs: path to the
#                                 hot wallet keypair file on disk.
#   SQUADS_MULTISIG_PUBKEY      # the multisig PDA that owns the upgrade
#                                 authority on each program
#   PROGRAM_IDS_PATH            # default: program-ids/mainnet.json
#   RPC_URL                     # default: https://api.mainnet-beta.solana.com
#   PROGRAMS                    # space-separated subset (default: all 5)
#
# Output:
#   release/upgrade-<commit>/buffer-manifest.json  with one entry per program:
#     { "program": "<id>",
#       "buffer":  "<id>",
#       "buffer_sha256": "<hex>",
#       "so_size_bytes": N }
#
# Operators copy that file into the multisig issue tracker / proposal
# notes so signers can verify locally before voting.
# ============================================================

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

BUFFER_AUTHORITY_KEY_JSON="${BUFFER_AUTHORITY_KEY_JSON:-}"
BUFFER_AUTHORITY_KEYPAIR="${BUFFER_AUTHORITY_KEYPAIR:-}"
SQUADS_MULTISIG_PUBKEY="${SQUADS_MULTISIG_PUBKEY:?must set SQUADS_MULTISIG_PUBKEY (transferred buffer authority)}"
PROGRAM_IDS_PATH="${PROGRAM_IDS_PATH:-program-ids/mainnet.json}"
RPC_URL="${RPC_URL:-https://api.mainnet-beta.solana.com}"
PROGRAMS="${PROGRAMS:-ario_core ario_gar ario_arns ario_ant ario_ant_escrow}"

[[ -f "$PROGRAM_IDS_PATH" ]] || { echo "ERROR: $PROGRAM_IDS_PATH missing — populate program IDs before mainnet runs" >&2; exit 1; }
command -v solana >/dev/null || { echo "solana CLI required" >&2; exit 1; }
command -v jq >/dev/null    || { echo "jq required (parse program-ids manifest)" >&2; exit 1; }

if [[ -z "$BUFFER_AUTHORITY_KEY_JSON" && ! -f "$BUFFER_AUTHORITY_KEYPAIR" ]]; then
  echo "ERROR: no buffer authority key available." >&2
  echo "  CI:    set BUFFER_AUTHORITY_KEY_JSON env to the raw JSON keypair contents" >&2
  echo "         (the script feeds it to solana via process substitution; the bytes" >&2
  echo "         never touch disk)." >&2
  echo "  Local: set BUFFER_AUTHORITY_KEYPAIR to a keypair file path on disk." >&2
  exit 1
fi

# solana_buf: run a solana CLI command as the buffer-authority signer
# against $RPC_URL. In CI mode (BUFFER_AUTHORITY_KEY_JSON set), the
# keypair is passed via bash process substitution — its bytes only ever
# live in shell + child-process memory and an anonymous pipe, never on
# the runner's filesystem. In local mode, the file at
# BUFFER_AUTHORITY_KEYPAIR is used. Both --keypair and --url are passed
# per call; this script does NOT mutate ~/.config/solana/cli/config.yml.
solana_buf() {
  if [[ -n "$BUFFER_AUTHORITY_KEY_JSON" ]]; then
    solana --keypair <(printf '%s' "$BUFFER_AUTHORITY_KEY_JSON") --url "$RPC_URL" "$@"
  else
    solana --keypair "$BUFFER_AUTHORITY_KEYPAIR" --url "$RPC_URL" "$@"
  fi
}

# Pubkey of the buffer authority, derived from whichever source we have.
buffer_authority_pubkey() {
  if [[ -n "$BUFFER_AUTHORITY_KEY_JSON" ]]; then
    solana-keygen pubkey <(printf '%s' "$BUFFER_AUTHORITY_KEY_JSON")
  else
    solana-keygen pubkey "$BUFFER_AUTHORITY_KEYPAIR"
  fi
}

# Refuse on dirty test attestor.
"$REPO_ROOT/scripts/check-attestor-pubkey.sh" --strict

# Build with mainnet feature flags.
if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
  echo "[mainnet-prepare] building (BUILD_NETWORK=mainnet)..."
  BUILD_NETWORK=mainnet bash "$REPO_ROOT/build-sbf.sh" --skip-check
fi

PAYER_PUBKEY="$(buffer_authority_pubkey 2>/dev/null)" \
  || { echo "ERROR: buffer authority key is not a valid Solana keypair JSON." >&2; exit 1; }

if [[ -n "$BUFFER_AUTHORITY_KEY_JSON" ]]; then
  AUTH_SOURCE="BUFFER_AUTHORITY_KEY_JSON env (in-memory; not written to disk)"
else
  AUTH_SOURCE="$BUFFER_AUTHORITY_KEYPAIR (local file)"
fi

echo "[mainnet-prepare] cluster: $RPC_URL"
echo "[mainnet-prepare] payer:   $PAYER_PUBKEY"
echo "[mainnet-prepare] payer source: $AUTH_SOURCE"
echo "[mainnet-prepare] balance: $(solana_buf balance "$PAYER_PUBKEY") (must cover ~rent for each .so)"

COMMIT="$(git rev-parse --short HEAD 2>/dev/null || date -u +%Y%m%d%H%M%S)"
OUT_DIR="$REPO_ROOT/release/upgrade-${COMMIT}"
mkdir -p "$OUT_DIR"
MANIFEST="$OUT_DIR/buffer-manifest.json"
echo "[]" > "$MANIFEST"

for prog in $PROGRAMS; do
  so="target/deploy/${prog}.so"
  [[ -f "$so" ]] || { echo "ERROR: $so missing" >&2; exit 1; }

  prog_id="$(jq -r --arg k "$prog" '.programs[$k]' "$PROGRAM_IDS_PATH")"
  if [[ -z "$prog_id" || "$prog_id" == "null" ]]; then
    echo "ERROR: program-id for $prog missing in $PROGRAM_IDS_PATH" >&2
    exit 1
  fi

  echo
  echo "[mainnet-prepare] $prog ($prog_id)"
  echo "[mainnet-prepare]   write-buffer..."
  # The buffer keypair is a single-use signer for the buffer account
  # itself (not the authority). After set-buffer-authority transfers
  # control to the multisig the buffer keypair is functionally inert
  # — it can no longer change buffer authority or close the account.
  # We keep it in OUT_DIR purely so operators can audit the staging.
  buffer_kp="$OUT_DIR/${prog}-buffer.json"
  solana-keygen new --no-bip39-passphrase -o "$buffer_kp" --silent
  buffer_id="$(solana-keygen pubkey "$buffer_kp")"

  solana_buf program write-buffer "$so" \
    --buffer "$buffer_kp" \
    --output json > "$OUT_DIR/${prog}-write-buffer.json"

  echo "[mainnet-prepare]   set-buffer-authority -> $SQUADS_MULTISIG_PUBKEY"
  solana_buf program set-buffer-authority "$buffer_id" \
    --new-buffer-authority "$SQUADS_MULTISIG_PUBKEY"

  buffer_sha="$(shasum -a 256 "$so" | awk '{print $1}')"
  size="$(stat -f%z "$so" 2>/dev/null || stat -c%s "$so")"

  jq --arg prog "$prog" \
     --arg pid  "$prog_id" \
     --arg buf  "$buffer_id" \
     --arg sha  "$buffer_sha" \
     --argjson size "$size" \
     '. += [{program_name:$prog, program_id:$pid, buffer:$buf, buffer_sha256:$sha, so_size_bytes:$size}]' \
     "$MANIFEST" > "$MANIFEST.tmp" && mv "$MANIFEST.tmp" "$MANIFEST"
done

echo
echo "[mainnet-prepare] Buffers staged. Manifest: $MANIFEST"
echo
jq . "$MANIFEST"
echo
cat <<NEXT
NEXT STEPS for the multisig signers
-----------------------------------
For EACH program in the manifest above:

  1. In Squads (https://app.squads.so), open the multisig and propose
     an "Upgrade Program" transaction with parameters:
        program     = <program_id from manifest>
        buffer      = <buffer from manifest>
        spill       = the authority address (gets reclaimed lamports)
        authority   = the multisig itself

  2. Independently verify the buffer .so before voting:
        solana program dump <buffer> /tmp/<prog>.so
        shasum -a 256 /tmp/<prog>.so          # must match buffer_sha256 above

  3. Approve once threshold reached. Execute. The program is now
     running the new .so.

If the upgrade is canceled / abandoned, reclaim the buffer rent:
        solana program close <buffer> --recipient <treasury>
NEXT
