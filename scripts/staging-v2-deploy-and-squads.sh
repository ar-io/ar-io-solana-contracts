#!/usr/bin/env bash
# ============================================================
# AR.IO Solana Contracts — Staging v2: fresh deploy + Squads V3 handoff
# ============================================================
#
# Staging v1 (program-ids/staging.json) was CLOSED on 2026-06-03 (its
# ProgramData accounts were reclaimed), so those IDs are permanently dead
# and cannot be reused. This script stands up staging *v2* as a clean,
# upgradeable deployment and then hands each program's upgrade authority to
# a Squads **V3** vault — a devnet dress-rehearsal of the mainnet upgrade
# ceremony. MAINNET ALSO USES SQUADS V3 (the "SMPL" program
# SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu), NOT V4 — so the target of
# the handoff is a V3 *authority-index-1* PDA, and the upgrade itself is
# driven from the legacy Squads app, not app.squads.so (V4). See
# `scripts/mainnet-prepare-upgrade.sh` for the mainnet variant.
#
# SQUADS V3 PRIMER (what you must get right):
#   * A Squads V3 squad has a config account (the "multisig") owned by the
#     SMPL program, and a set of authority PDAs derived from it.
#   * authorityIndex 1 is the DEFAULT VAULT — and per Squads' own docs it is
#     also what program upgrade authority is set to. authorityIndex 0 is
#     reserved for internal multisig instructions; never target it.
#   * Vault PDA = find_program_address(
#         ["squad", <multisig>, u32_le(1), "authority"], SMPL_PROGRAM).
#     The vault is a System-owned signing PDA. The multisig config account
#     is owned by the SMPL program. We hand authority to the VAULT, never to
#     the config account, and never send tokens to the config account.
#
# ORDER MATTERS — INIT BEFORE HANDOFF:
#   ario-core/gar/arns/ant bind their one-time genesis `initialize*`
#   instructions to the program's *upgrade authority*
#   (`program_data.upgrade_authority_address == Some(payer.key())`, an
#   anti-front-run guard; audit H1 / Theme C). Once you hand upgrade
#   authority to the Squads vault, the deployer can no longer run those.
#   So network-init (mint + config + registries + epoch settings + ant
#   migration config) MUST complete while the deployer still holds upgrade
#   authority — i.e. BEFORE `handoff`. Program *upgrades* themselves need no
#   contract changes (pure BPFLoaderUpgradeable), and no routine/migration
#   instruction is upgrade-authority-gated.
#
# It deliberately mirrors:
#   * scripts/devnet-deploy.sh        — build + key handling + manifest
#   * scripts/mainnet-prepare-upgrade.sh — write-buffer + Squads handoff
#
# SUBCOMMANDS
#   deploy           Generate fresh program keypairs (NEW IDs), sync
#                    declare_id!() to them, build, first-deploy all 5
#                    programs with the deployer as the *initial* upgrade
#                    authority, and write the new IDs to the manifest.
#                    GUARD: refuses if the manifest IDs are already live
#                    on-chain (set FORCE_REDEPLOY=1 to mint a new generation).
#   handoff          Transfer each program's upgrade authority from the
#                    deployer to the Squads V3 vault (SQUADS_VAULT). ONE-WAY:
#                    afterwards only the multisig can upgrade (or hand it
#                    back). Recoverable on staging; rehearse it here.
#                    GUARD: requires INIT_CONFIRMED=1 (see "INIT BEFORE
#                    HANDOFF" above) and verifies the vault looks like a V3
#                    vault before signing.
#   prepare-upgrade  Build a fresh .so set, write a buffer per program, set
#                    each buffer's authority to the Squads V3 vault, and
#                    print the legacy-app steps to propose/approve/execute.
#   status           Print each program's current on-chain upgrade authority.
#
# ENV
#   DEPLOY_CLUSTER     default https://api.devnet.solana.com
#   AUTHORITY_KEY_JSON raw JSON keypair contents of the DEPLOYER (CI mode;
#                      materialized to a chmod-600 tempfile, deleted on exit).
#                      Takes precedence over AUTHORITY_KEYPAIR.
#   AUTHORITY_KEYPAIR  fallback: path to the deployer keypair on disk
#                      (default target/deploy/devnet-authority-keypair.json)
#   SQUADS_VAULT       the Squads V3 *vault* PDA (authority index 1) that will
#                      own upgrade authority (required for handoff /
#                      prepare-upgrade). This is the vault (System-owned
#                      signing PDA), NOT the multisig config account.
#   SQUADS_V3_MULTISIG optional: the multisig config account. When set,
#                      handoff/prepare-upgrade verify it is owned by the SMPL
#                      program (i.e. genuinely V3, not V4) before proceeding.
#   SQUADS_V3_PROGRAM  default SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu
#   INIT_CONFIRMED=1   required for handoff — your attestation that
#                      network-init already ran (see "INIT BEFORE HANDOFF").
#   FORCE_REDEPLOY=1   allow `deploy` to mint a new generation even though the
#                      manifest IDs are already live (creates a v3 — rare).
#   PROGRAM_IDS_PATH   default program-ids/staging.json (REWRITTEN by deploy)
#   PROGRAMS           default "ario_core ario_ant ario_gar ario_arns ario_ant_escrow"
#   KEYS_DIR           where fresh program keypairs are written
#                      (default target/deploy; gitignored). BACK THESE UP.
#   BUILD_NETWORK      default devnet (staging maps to network-devnet)
#   SKIP_BUILD=1       reuse existing target/deploy/*.so
#
# Run from repo root, e.g.:
#   AUTHORITY_KEYPAIR=~/keys/staging-deployer.json bash scripts/staging-v2-deploy-and-squads.sh status
#   INIT_CONFIRMED=1 \
#     SQUADS_VAULT=4sBzyU2P14jhvit6ckjqAzy1VB5kymtsSqh2rQsjMPSv \
#     SQUADS_V3_MULTISIG=G8Wja3zCGk4dqdK1Me5RwEw5zVvcjs3ZFy646ePZJPtM \
#     AUTHORITY_KEYPAIR=~/keys/staging-deployer.json \
#     bash scripts/staging-v2-deploy-and-squads.sh handoff
# ============================================================
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DEPLOY_CLUSTER="${DEPLOY_CLUSTER:-https://api.devnet.solana.com}"
AUTHORITY_KEY_JSON="${AUTHORITY_KEY_JSON:-}"
AUTHORITY_KEYPAIR="${AUTHORITY_KEYPAIR:-target/deploy/devnet-authority-keypair.json}"
SQUADS_VAULT="${SQUADS_VAULT:-}"
SQUADS_V3_MULTISIG="${SQUADS_V3_MULTISIG:-}"
SQUADS_V3_PROGRAM="${SQUADS_V3_PROGRAM:-SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu}"
SYSTEM_PROGRAM="11111111111111111111111111111111"
PROGRAM_IDS_PATH="${PROGRAM_IDS_PATH:-program-ids/staging.json}"
PROGRAMS="${PROGRAMS:-ario_core ario_ant ario_gar ario_arns ario_ant_escrow}"
KEYS_DIR="${KEYS_DIR:-target/deploy}"
BUILD_NETWORK="${BUILD_NETWORK:-devnet}"

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; NC='\033[0m'
step() { echo -e "\n${GREEN}=== $1 ===${NC}"; }
warn() { echo -e "${YELLOW}!  $1${NC}"; }
fail() { echo -e "${RED}x  $1${NC}" >&2; exit 1; }

# --- deployer keypair: materialize CI JSON to a chmod-600 tempfile -------
# (Same rationale as devnet-deploy.sh: `solana program deploy` re-opens the
# keypair path, so a one-shot process-substitution pipe returns EOF on the
# second read. A real file deleted on exit preserves "never persisted".)
if [[ -n "$AUTHORITY_KEY_JSON" ]]; then
  AUTHORITY_KEYPAIR="$(mktemp -t ar-io-staging-auth.XXXXXXXX.json)"
  chmod 600 "$AUTHORITY_KEYPAIR"
  printf '%s' "$AUTHORITY_KEY_JSON" > "$AUTHORITY_KEYPAIR"
  trap 'rm -f "$AUTHORITY_KEYPAIR"' EXIT INT TERM HUP
  unset AUTHORITY_KEY_JSON
fi

solana_auth() { solana --keypair "$AUTHORITY_KEYPAIR" --url "$DEPLOY_CLUSTER" "$@"; }
deployer_pubkey() { solana-keygen pubkey "$AUTHORITY_KEYPAIR"; }

require_tools() {
  command -v solana >/dev/null || fail "Solana CLI not found (Agave 2.1.x)."
  command -v jq     >/dev/null || fail "jq required."
  if [[ "${1:-}" == "build" ]]; then
    command -v anchor >/dev/null || fail "Anchor CLI 0.31.1 required."
  fi
}
require_deployer() {
  [[ -f "$AUTHORITY_KEYPAIR" ]] || fail "No deployer key. Set AUTHORITY_KEYPAIR (file) or AUTHORITY_KEY_JSON (CI)."
  DEPLOYER="$(deployer_pubkey 2>/dev/null)" || fail "$AUTHORITY_KEYPAIR is not a valid keypair JSON."
}
require_vault() {
  [[ -n "$SQUADS_VAULT" ]] || fail "SQUADS_VAULT is required (the Squads V3 vault PDA — authority index 1 — that will hold upgrade authority)."
}
manifest_id() { jq -r --arg k "$1" '.programs[$k] // ""' "$PROGRAM_IDS_PATH"; }

# account_owner <pubkey> -> the owning program id (empty if not found)
account_owner() {
  solana account "$1" --url "$DEPLOY_CLUSTER" --output json 2>/dev/null \
    | jq -r '.account.owner // ""' 2>/dev/null || true
}

# verify_vault: best-effort sanity checks before the irreversible handoff.
# We cannot derive the index-1 PDA here without an ed25519 lib, so we assert
# the cheap-but-high-signal invariants and tell the operator to cross-check
# the vault against what the Squads V3 app shows. Offline derivation (record):
#   node -e 'const{PublicKey}=require("@solana/web3.js");const i=Buffer.alloc(4);i.writeUInt32LE(1);
#     console.log(PublicKey.findProgramAddressSync([Buffer.from("squad"),
#       new PublicKey(process.argv[1]).toBuffer(),i,Buffer.from("authority")],
#       new PublicKey("SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu"))[0].toBase58())' <MULTISIG>
verify_vault() {
  local vowner; vowner="$(account_owner "$SQUADS_VAULT")"
  if [[ "$vowner" != "$SYSTEM_PROGRAM" ]]; then
    fail "SQUADS_VAULT $SQUADS_VAULT is owned by '${vowner:-<not found>}', expected the System program ($SYSTEM_PROGRAM).
     A Squads V3 vault is a System-owned signing PDA. If this is owned by the SMPL program it is the
     multisig CONFIG account, not the vault — handing authority there would brick upgrades."
  fi
  if [[ -n "$SQUADS_V3_MULTISIG" ]]; then
    local mowner; mowner="$(account_owner "$SQUADS_V3_MULTISIG")"
    [[ "$mowner" == "$SQUADS_V3_PROGRAM" ]] || \
      fail "SQUADS_V3_MULTISIG $SQUADS_V3_MULTISIG is owned by '${mowner:-<not found>}', expected the Squads V3 SMPL program ($SQUADS_V3_PROGRAM). Are you pointing at a V4 multisig?"
    warn "Multisig $SQUADS_V3_MULTISIG confirmed V3 (SMPL-owned). CONFIRM $SQUADS_VAULT is its authority-index-1 vault (see app / offline derivation in verify_vault comment)."
  else
    warn "SQUADS_V3_MULTISIG not set — skipping V3-ownership cross-check. Vault is System-owned (good), but verify it is authority index 1 of the intended multisig in the Squads V3 app."
  fi
}

# ============================================================
cmd_deploy() {
  require_tools build; require_deployer
  step "Staging v2 DEPLOY (fresh IDs) on $DEPLOY_CLUSTER"
  # Guard: staging v2 is already live (committed in PR #98). Re-running deploy
  # mints brand-new keypairs/IDs and stands up a *v3*, orphaning the live v2.
  local live=0
  for prog in $PROGRAMS; do
    pid="$(manifest_id "$prog")"; [[ -n "$pid" ]] || continue
    if solana program show "$pid" --url "$DEPLOY_CLUSTER" 2>/dev/null | grep -qiE 'Last Deployed In Slot'; then
      live=$((live+1))
    fi
  done
  if [[ "$live" -gt 0 && "${FORCE_REDEPLOY:-0}" != "1" ]]; then
    fail "$live/$(echo $PROGRAMS | wc -w) manifest program IDs are already LIVE on $DEPLOY_CLUSTER.
     Deploy would mint a NEW generation (v3) and orphan the live deployment.
     If that is truly what you want, re-run with FORCE_REDEPLOY=1. Otherwise you want 'handoff'."
  fi
  warn "v1 IDs in $PROGRAM_IDS_PATH are CLOSED and will be overwritten with fresh v2 IDs."
  "$REPO_ROOT/scripts/check-attestor-pubkey.sh" --strict --cluster devnet

  mkdir -p "$KEYS_DIR"
  declare -A NEW_ID
  step "1. Generate fresh program keypairs (BACK THESE UP: $KEYS_DIR/<prog>-keypair.json)"
  for prog in $PROGRAMS; do
    kp="$KEYS_DIR/${prog}-keypair.json"
    if [[ -f "$kp" ]]; then
      warn "$kp exists — reusing (delete it to mint a brand-new ID)"
    else
      solana-keygen new --no-bip39-passphrase -o "$kp" --silent
    fi
    NEW_ID[$prog]="$(solana-keygen pubkey "$kp")"
    echo "  $prog -> ${NEW_ID[$prog]}"
  done

  step "2. Write new IDs to $PROGRAM_IDS_PATH (.programs)"
  local prog_json="{}"
  for prog in $PROGRAMS; do
    prog_json="$(jq --arg k "$prog" --arg v "${NEW_ID[$prog]}" '. + {($k): $v}' <<<"$prog_json")"
  done
  jq --arg cluster "$DEPLOY_CLUSTER" --arg deployer "$DEPLOYER" --argjson progs "$prog_json" \
     '. + {cluster: $cluster, deployer: $deployer, programs: $progs}' \
     "$PROGRAM_IDS_PATH" > "$PROGRAM_IDS_PATH.tmp" && mv "$PROGRAM_IDS_PATH.tmp" "$PROGRAM_IDS_PATH"

  step "3. Build (sync declare_id!() from the new manifest IDs)"
  if [[ "${SKIP_BUILD:-0}" == "1" ]]; then
    warn "SKIP_BUILD=1 — reusing target/deploy/*.so (must already match the new IDs)"
  else
    PROGRAM_IDS_PATH="$PROGRAM_IDS_PATH" BUILD_NETWORK="$BUILD_NETWORK" \
      bash "$REPO_ROOT/build-sbf.sh" --sync-from-manifest
  fi

  step "4. First-deploy each program (deployer = initial upgrade authority)"
  for prog in $PROGRAMS; do
    so="target/deploy/${prog}.so"; kp="$KEYS_DIR/${prog}-keypair.json"
    [[ -f "$so" ]] || fail "Missing $so"
    echo; echo "Deploying $prog (${NEW_ID[$prog]})..."
    # First deploy: --program-id takes the KEYPAIR (creates the account).
    # Upgrade authority defaults to the --keypair signer (the deployer).
    solana_auth program deploy --program-id "$kp" "$so"
  done

  jq --argjson now "\"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"" \
     '.deployed_at = (.programs | map_values($now))' \
     "$PROGRAM_IDS_PATH" > "$PROGRAM_IDS_PATH.tmp" && mv "$PROGRAM_IDS_PATH.tmp" "$PROGRAM_IDS_PATH"

  echo -e "\n${GREEN}Staging v2 deployed.${NC} Next: network-init (mint/config/registries/epochs/ant-migration)"
  echo "via the migration import tooling pointed at these IDs — MUST run before handoff (genesis"
  echo "initialize* instructions are bound to the deployer's upgrade authority). Then:"
  echo "  INIT_CONFIRMED=1 SQUADS_VAULT=$SQUADS_VAULT SQUADS_V3_MULTISIG=<multisig> \\"
  echo "    bash scripts/staging-v2-deploy-and-squads.sh handoff"
}

# ============================================================
cmd_handoff() {
  require_tools run; require_deployer; require_vault
  step "Hand upgrade authority -> Squads V3 vault $SQUADS_VAULT"
  # Guard: the genesis initialize* instructions are bound to the upgrade
  # authority. Handing it off before network-init makes them unrunnable.
  if [[ "${INIT_CONFIRMED:-0}" != "1" ]]; then
    fail "Refusing to hand off before network-init is confirmed.
     ario-core/gar/arns/ant bind their one-time initialize* instructions to the program's UPGRADE
     authority. After handoff the deployer can no longer run them, so the mint/config/registries/epoch
     settings/ant-migration-config MUST already exist. Re-run with INIT_CONFIRMED=1 once network-init
     (migration import tooling, deployer-signed) has completed and the ARIO mint is recorded."
  fi
  verify_vault
  warn "ONE-WAY: after this, only the 2-of-N multisig can upgrade these programs (or hand authority back)."
  warn "Confirm $SQUADS_VAULT is the Squads V3 VAULT (authority index 1), and the multisig members/threshold are correct."
  for prog in $PROGRAMS; do
    pid="$(manifest_id "$prog")"
    [[ -n "$pid" ]] || { warn "$prog: no ID in manifest, skipping"; continue; }
    echo; echo "set-upgrade-authority $prog ($pid) -> $SQUADS_VAULT"
    # The vault is a PDA and cannot sign, so skip the new-authority signer
    # check (standard for handing a program to a multisig).
    solana_auth program set-upgrade-authority "$pid" \
      --new-upgrade-authority "$SQUADS_VAULT" \
      --skip-new-upgrade-authority-signer-check
  done
  echo -e "\n${GREEN}Handoff complete.${NC} Verify: bash scripts/staging-v2-deploy-and-squads.sh status"
}

# ============================================================
cmd_prepare_upgrade() {
  require_tools build; require_deployer; require_vault
  step "Prepare a Squads V3 upgrade (buffers -> vault $SQUADS_VAULT)"
  verify_vault
  if [[ "${SKIP_BUILD:-0}" != "1" ]]; then
    BUILD_NETWORK="$BUILD_NETWORK" bash "$REPO_ROOT/build-sbf.sh" --skip-check
  fi
  COMMIT="$(git rev-parse --short HEAD 2>/dev/null || date -u +%Y%m%d%H%M%S)"
  OUT_DIR="$REPO_ROOT/release/staging-upgrade-${COMMIT}"; mkdir -p "$OUT_DIR"
  MANIFEST="$OUT_DIR/buffer-manifest.json"; echo "[]" > "$MANIFEST"
  echo "Payer/buffer authority (deployer): $DEPLOYER  balance: $(solana_auth balance "$DEPLOYER")"

  for prog in $PROGRAMS; do
    so="target/deploy/${prog}.so"; [[ -f "$so" ]] || fail "Missing $so"
    pid="$(manifest_id "$prog")"; [[ -n "$pid" ]] || fail "$prog: no ID in manifest"
    echo; echo "[$prog $pid] write-buffer..."
    buffer_kp="$OUT_DIR/${prog}-buffer.json"
    solana-keygen new --no-bip39-passphrase -o "$buffer_kp" --silent
    buffer_id="$(solana-keygen pubkey "$buffer_kp")"
    solana_auth program write-buffer "$so" --buffer "$buffer_kp" --output json \
      > "$OUT_DIR/${prog}-write-buffer.json"
    echo "[$prog] set-buffer-authority -> $SQUADS_VAULT"
    solana_auth program set-buffer-authority "$buffer_id" --new-buffer-authority "$SQUADS_VAULT"
    sha="$(shasum -a 256 "$so" | awk '{print $1}')"
    size="$(stat -c%s "$so" 2>/dev/null || stat -f%z "$so")"
    jq --arg p "$prog" --arg pid "$pid" --arg buf "$buffer_id" --arg sha "$sha" --argjson sz "$size" \
       '. += [{program_name:$p, program_id:$pid, buffer:$buf, buffer_sha256:$sha, so_size_bytes:$sz}]' \
       "$MANIFEST" > "$MANIFEST.tmp" && mv "$MANIFEST.tmp" "$MANIFEST"
  done

  echo; jq . "$MANIFEST"; cat <<NEXT

SQUADS V3 UPGRADE — for EACH program above, in the LEGACY Squads app (V3),
NOT app.squads.so (that is V4). Open the squad, Developers -> Programs:
  1. If the program isn't listed yet: "Add Program" (name + program_id), then
     run the CLI command the app shows to set upgrade authority to the vault,
     and click "Verify authority". (One-time per program; handoff already did
     this if you set the vault as upgrade authority.)
  2. Click the program -> "Add upgrade":
        buffer  = <buffer from manifest>
        spill   = $DEPLOYER          (reclaimed buffer lamports go here)
     The app shows a 'set-buffer-authority -> vault' command; this script
     already set buffer authority to $SQUADS_VAULT, so just "Verify authority".
  3. Verify the buffer bytes before voting:
        solana program dump <buffer> /tmp/<prog>.so -u $DEPLOY_CLUSTER
        shasum -a 256 /tmp/<prog>.so   # must equal buffer_sha256 above
  4. Click "Upgrade" to create the transaction; approve to threshold; Execute.
     Confirm the upgrade landed:
        solana program show <program_id> -u $DEPLOY_CLUSTER   # "Last Deployed In Slot" bumps
Abandon? reclaim rent: solana program close <buffer> --recipient $DEPLOYER -u $DEPLOY_CLUSTER
NEXT
}

# ============================================================
cmd_status() {
  require_tools run
  step "Current upgrade authority of staging programs ($DEPLOY_CLUSTER)"
  for prog in $PROGRAMS; do
    pid="$(manifest_id "$prog")"; [[ -n "$pid" ]] || { echo "  $prog: (no ID)"; continue; }
    auth="$(solana program show "$pid" --url "$DEPLOY_CLUSTER" 2>&1 | grep -iE 'Authority' | head -1 || true)"
    echo "  $prog ($pid): ${auth:-<closed or not found>}"
  done
}

case "${1:-}" in
  deploy)          cmd_deploy ;;
  handoff)         cmd_handoff ;;
  prepare-upgrade) cmd_prepare_upgrade ;;
  status)          cmd_status ;;
  *) echo "usage: $0 {deploy|handoff|prepare-upgrade|status}" >&2; exit 1 ;;
esac
