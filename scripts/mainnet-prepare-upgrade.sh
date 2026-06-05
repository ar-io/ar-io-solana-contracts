#!/usr/bin/env bash
# ============================================================
# AR.IO Solana Contracts — Mainnet Upgrade Preparation
# ============================================================
#
# Mainnet program-upgrade authority is a Squads **V3** multisig (the SMPL
# program SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu), NOT V4. The thing
# that actually signs the BPFLoaderUpgradeable Upgrade is the V3 **vault =
# authority index 1** PDA (the default vault, also what program upgrade
# authority is set to per Squads' docs; index 0 is reserved). The multisig
# *config* account (SMPL-owned) never signs and never holds assets — we set
# buffer authority to the VAULT, never the config account. CI never holds
# the upgrade key, and we never run `anchor deploy` against mainnet directly.
# Instead we:
#
#   1. Build the .so files with mainnet feature flags.
#   2. If a new .so is larger than the program's on-chain ProgramData
#      capacity, `program extend` it first (permissionless — no upgrade
#      authority needed), so the multisig's Execute can't fail on size.
#   3. Pre-upload each .so to a write buffer using a transient buffer
#      authority that this script controls (a hot wallet that pays rent).
#   4. Transfer buffer authority to the VAULT.
#   5. Print the parameters for the multisig signers to propose / approve /
#      execute the upgrade from the legacy (V3) Squads app.
#
# Pattern reference: solana program write-buffer + set-buffer-authority +
# Squads V3 (legacy) program upgrade:
#   https://docs.solanalabs.com/cli/examples/deploy-a-program#redeploy-a-program
#   https://docs.squads.so/main/squads-legacy/navigating-your-squad/developers/programs
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
#   SQUADS_V3_VAULT             # the V3 vault PDA (authority index 1) that
#                                 holds upgrade authority and receives buffer
#                                 authority. Falls back to the legacy
#                                 SQUADS_MULTISIG_PUBKEY var if unset. Verified
#                                 System-owned before use (a multisig CONFIG
#                                 account is SMPL-owned and would be rejected).
#   SQUADS_V3_MULTISIG          # optional: the multisig config account. When
#                                 set, verified SMPL-owned (i.e. genuinely V3).
#   SQUADS_V3_PROGRAM           # default SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu
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
# Buffer-authority target = the V3 vault (authority index 1). Prefer
# SQUADS_V3_VAULT; fall back to the legacy SQUADS_MULTISIG_PUBKEY var (which
# the upgrade-mainnet workflow still passes) so existing CI keeps working.
SQUADS_V3_VAULT="${SQUADS_V3_VAULT:-${SQUADS_MULTISIG_PUBKEY:-}}"
[[ -n "$SQUADS_V3_VAULT" ]] || { echo "ERROR: set SQUADS_V3_VAULT (the V3 vault PDA, authority index 1) — or the legacy SQUADS_MULTISIG_PUBKEY." >&2; exit 1; }
SQUADS_V3_MULTISIG="${SQUADS_V3_MULTISIG:-}"
SQUADS_V3_PROGRAM="${SQUADS_V3_PROGRAM:-SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu}"
SYSTEM_PROGRAM="11111111111111111111111111111111"
PROGRAM_IDS_PATH="${PROGRAM_IDS_PATH:-program-ids/mainnet.json}"
RPC_URL="${RPC_URL:-https://api.mainnet-beta.solana.com}"
PROGRAMS="${PROGRAMS:-ario_core ario_gar ario_arns ario_ant ario_ant_escrow}"

# Read-only owner lookup (no signer needed).
account_owner() { solana --url "$RPC_URL" account "$1" --output json 2>/dev/null | jq -r '.account.owner // ""' 2>/dev/null || true; }

# Verify the buffer-authority target is a real V3 vault before we hand any
# buffer to it: the vault is a System-owned signing PDA; a multisig CONFIG
# account is SMPL-owned and handing a buffer there would brick the upgrade.
verify_v3_vault() {
  local vowner; vowner="$(account_owner "$SQUADS_V3_VAULT")"
  if [[ "$vowner" != "$SYSTEM_PROGRAM" ]]; then
    echo "ERROR: SQUADS_V3_VAULT $SQUADS_V3_VAULT is owned by '${vowner:-<not found>}', expected the System program ($SYSTEM_PROGRAM)." >&2
    echo "       A V3 vault is System-owned. If this is SMPL-owned it is the multisig CONFIG account, not the vault — abort." >&2
    exit 1
  fi
  if [[ -n "$SQUADS_V3_MULTISIG" ]]; then
    local mowner; mowner="$(account_owner "$SQUADS_V3_MULTISIG")"
    [[ "$mowner" == "$SQUADS_V3_PROGRAM" ]] || { echo "ERROR: SQUADS_V3_MULTISIG $SQUADS_V3_MULTISIG is owned by '${mowner:-<not found>}', expected the Squads V3 SMPL program ($SQUADS_V3_PROGRAM). A V4 multisig?" >&2; exit 1; }
    echo "[mainnet-prepare] V3 multisig $SQUADS_V3_MULTISIG confirmed SMPL-owned; vault $SQUADS_V3_VAULT is System-owned. Confirm the vault is its authority index 1."
  else
    echo "[mainnet-prepare] WARN: SQUADS_V3_MULTISIG not set — vault is System-owned (good) but not cross-checked against the multisig. Verify it is authority index 1 in the Squads V3 app."
  fi
}

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

# Refuse unless ATTESTOR_PUBKEY is not the test key AND matches the mainnet
# key pinned in program-ids/mainnet.json. This blocks shipping the devnet
# attestor key to mainnet; pin the mainnet attestor pubkey there first.
"$REPO_ROOT/scripts/check-attestor-pubkey.sh" --strict --cluster mainnet

# Verify the V3 vault target before staging any buffers.
verify_v3_vault

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

  # A Squads/BPFLoaderUpgradeable Upgrade CANNOT grow the program — it fails
  # to EXECUTE if the new .so exceeds the on-chain ProgramData capacity (Agave
  # deploy allocates exactly the program size). Extend FIRST. `program extend`
  # is permissionless (no upgrade-authority signer), so the buffer-authority
  # hot wallet can pay/run it even though the vault holds upgrade authority.
  new_size="$(stat -f%z "$so" 2>/dev/null || stat -c%s "$so")"
  pd_addr="$(solana --url "$RPC_URL" program show "$prog_id" 2>/dev/null | awk '/ProgramData Address/{print $NF}')"
  if [[ -n "$pd_addr" ]]; then
    pd_cap="$(solana --url "$RPC_URL" account "$pd_addr" 2>/dev/null | awk '/Length/{print $2}')"
    need=$(( new_size + 45 ))   # 45-byte ProgramData header
    if [[ -n "$pd_cap" && "$need" -gt "$pd_cap" ]]; then
      extra=$(( need - pd_cap + 4096 ))
      echo "[mainnet-prepare]   new .so ($new_size B) > ProgramData capacity ($pd_cap B); extending by $extra B (buffer-authority-paid)."
      solana_buf program extend "$prog_id" "$extra"
    fi
  fi

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

  echo "[mainnet-prepare]   set-buffer-authority -> $SQUADS_V3_VAULT (V3 vault)"
  solana_buf program set-buffer-authority "$buffer_id" \
    --new-buffer-authority "$SQUADS_V3_VAULT"

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
NEXT STEPS for the multisig signers (Squads V3 / legacy app)
------------------------------------------------------------
Use the LEGACY Squads app (V3), NOT app.squads.so (that is V4). Each
program's upgrade authority is the V3 vault ($SQUADS_V3_VAULT). Buffer
authority was already set to that vault above. For EACH program:

  1. Open the squad -> Developers -> Programs. If the program isn't listed,
     "Add Program" (it should "Verify authority" immediately since the vault
     already holds upgrade authority).

  2. Click the program -> "Add upgrade":
        buffer = <buffer from manifest>
        spill  = the buffer-authority / treasury address (reclaimed lamports)
     Buffer authority is already the vault -> "Verify authority" passes.

  3. Independently verify the buffer .so before voting:
        solana program dump <buffer> /tmp/<prog>.so
        shasum -a 256 /tmp/<prog>.so          # must match buffer_sha256 above

  4. Click "Upgrade" to create the transaction; approve to threshold;
     Execute. The program now runs the new .so.

If the upgrade is canceled / abandoned, reclaim the buffer rent:
        solana program close <buffer> --recipient <treasury>
NEXT
