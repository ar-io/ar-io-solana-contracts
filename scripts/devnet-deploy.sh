#!/usr/bin/env bash
# ============================================================
# AR.IO Solana Contracts — Devnet Deployment (upgrade-only)
# ============================================================
#
# Builds the AR.IO programs from source, syncs declare_id!() values from
# the committed program-ids/devnet.json manifest, and pushes upgrades
# against the existing on-chain program IDs. Idempotent across runs;
# leaves on-chain state untouched.
#
# THIS SCRIPT NEVER MINTS NEW PROGRAM IDS. It only does upgrades against
# IDs that are already populated in program-ids/devnet.json. The single
# secret in CI is the upgrade authority keypair (DEVNET_AUTHORITY_KEY_JSON);
# program keypairs live in the original deployer's offline custody and
# are only needed for first deploys.
#
# In CI the authority key is passed in via AUTHORITY_KEY_JSON (raw JSON
# array contents of the keypair file). It is fed to solana via bash
# process substitution (`--keypair <(printf %s "$AUTHORITY_KEY_JSON")`)
# so the bytes never get written to the runner's filesystem. Locally,
# operators point AUTHORITY_KEYPAIR at a file on disk instead.
#
# First deploys (e.g. ario-ant-escrow before its initial bring-up):
#   * Done manually from a maintainer laptop with the program keypair.
#   * After deploy, transfer upgrade authority to the same key as the
#     other programs:
#        solana program set-upgrade-authority <new_program_id> \
#          --new-upgrade-authority <DEVNET_AUTHORITY_PUBKEY>
#   * Add the resulting program ID to program-ids/devnet.json and commit.
#   * Subsequent upgrades will then flow through this script automatically
#     (DEPLOY_ORDER is derived from the manifest).
#
# Prerequisites:
#   - Solana CLI 2.1+ (Agave) installed (solana --version)
#   - Anchor CLI 0.31.1 installed (anchor --version) — used by build only
#   - jq installed (parse program-ids manifest)
#   - cargo test passes locally
#
# Run from the repo root:
#   bash scripts/devnet-deploy.sh
#
# Env knobs:
#   DEPLOY_CLUSTER="https://api.devnet.solana.com"   # rpc to deploy to
#   AUTHORITY_KEY_JSON="[1,2,...,64]"                 # raw JSON keypair
#                                                     #   contents (CI mode;
#                                                     #   preferred — keypair
#                                                     #   never touches disk).
#                                                     #   Takes precedence
#                                                     #   over AUTHORITY_KEYPAIR
#                                                     #   if both are set.
#   AUTHORITY_KEYPAIR="target/deploy/devnet-authority-keypair.json"
#                                                     # fallback for local
#                                                     #   runs: path to the
#                                                     #   keypair file on
#                                                     #   disk.
#   BUILD_NETWORK="devnet"                            # mainnet | devnet
#                                                     #   (compile-time
#                                                     #    network-* feature
#                                                     #    on ario-ant-escrow)
#   SKIP_BUILD=1                                      # reuse target/deploy/*.so
#   SKIP_AIRDROP=1                                    # don't request devnet SOL
#   PROGRAM_IDS_PATH="program-ids/devnet.json"        # input manifest
#                                                       (program IDs are READ
#                                                       from here, never
#                                                       overwritten)
#
# This script does NOT initialize on-chain state — it only deploys the
# .so files. Initial config setup (creating the ARIO mint, GatewayRegistry,
# NameRegistry, ArioConfig, etc.) is operator runbook material and lives
# in the migration tooling repo (`migration/import` in solana-ar-io).
#
# For mainnet, use `scripts/mainnet-prepare-upgrade.sh` — DO NOT run this
# script against mainnet (the mainnet upgrade authority is a Squads V4
# multisig, not a single keypair).
# ============================================================

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

DEPLOY_CLUSTER="${DEPLOY_CLUSTER:-https://api.devnet.solana.com}"
AUTHORITY_KEY_JSON="${AUTHORITY_KEY_JSON:-}"
AUTHORITY_KEYPAIR="${AUTHORITY_KEYPAIR:-target/deploy/devnet-authority-keypair.json}"
BUILD_NETWORK="${BUILD_NETWORK:-devnet}"
PROGRAM_IDS_PATH="${PROGRAM_IDS_PATH:-program-ids/devnet.json}"

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

step() { echo -e "\n${GREEN}=== Step $1: $2 ===${NC}"; }
warn() { echo -e "${YELLOW}!  $1${NC}"; }
fail() { echo -e "${RED}x  $1${NC}"; exit 1; }

# solana_auth: run a solana CLI command with the upgrade authority signer
# and the deploy cluster URL. In CI mode (AUTHORITY_KEY_JSON set), the
# keypair is fed in via process substitution so its bytes are only ever
# resident in shell + child-process memory and an anonymous pipe — they
# never get written to the runner's filesystem. In local mode
# (AUTHORITY_KEY_JSON unset), the file at AUTHORITY_KEYPAIR is used.
#
# Both --keypair and --url are passed as global flags per call; this
# script intentionally does NOT mutate ~/.config/solana/cli/config.yml,
# so it leaves zero on-disk traces of either the key or the cluster.
solana_auth() {
  if [[ -n "$AUTHORITY_KEY_JSON" ]]; then
    solana --keypair <(printf '%s' "$AUTHORITY_KEY_JSON") --url "$DEPLOY_CLUSTER" "$@"
  else
    solana --keypair "$AUTHORITY_KEYPAIR" --url "$DEPLOY_CLUSTER" "$@"
  fi
}

# Pubkey of the authority key, derived from whichever source we have.
authority_pubkey() {
  if [[ -n "$AUTHORITY_KEY_JSON" ]]; then
    solana-keygen pubkey <(printf '%s' "$AUTHORITY_KEY_JSON")
  else
    solana-keygen pubkey "$AUTHORITY_KEYPAIR"
  fi
}

# ------------------------------------------------------------
step 0 "Environment check"
# ------------------------------------------------------------
command -v solana >/dev/null || fail "Solana CLI not found. Install Agave 2.1.x (https://docs.anza.xyz/cli/install)."
command -v anchor >/dev/null || fail "Anchor CLI not found. Install: cargo install --git https://github.com/coral-xyz/anchor avm && avm install 0.31.1"
command -v jq     >/dev/null || fail "jq required (parse $PROGRAM_IDS_PATH). brew install jq / apt-get install jq."
echo "Solana: $(solana --version)"
echo "Anchor: $(anchor --version)"
echo "Cluster: $DEPLOY_CLUSTER"

# Refuse to deploy with the test attestor pubkey baked in.
"$REPO_ROOT/scripts/check-attestor-pubkey.sh" --strict

# ------------------------------------------------------------
step 1 "Load authority keypair and resolve program IDs from manifest"
# ------------------------------------------------------------
[[ -f "$PROGRAM_IDS_PATH" ]] || fail "$PROGRAM_IDS_PATH not found. CI cannot bootstrap a manifest — first deploys are a manual operator action."

# Resolve the upgrade-authority key. CI mode (env): set AUTHORITY_KEY_JSON
# to the raw JSON keypair contents — the script feeds it to solana via
# process substitution and the bytes never touch the filesystem. Local
# mode (file): set AUTHORITY_KEYPAIR to a path on disk.
if [[ -n "$AUTHORITY_KEY_JSON" ]]; then
  WALLET="$(authority_pubkey 2>/dev/null)" \
    || fail "AUTHORITY_KEY_JSON is set but its contents are not a valid Solana keypair JSON array."
  AUTHORITY_SOURCE="AUTHORITY_KEY_JSON env var (in-memory; not written to disk)"
elif [[ -f "$AUTHORITY_KEYPAIR" ]]; then
  WALLET="$(authority_pubkey 2>/dev/null)" \
    || fail "$AUTHORITY_KEYPAIR exists but is not a valid Solana keypair JSON file."
  AUTHORITY_SOURCE="$AUTHORITY_KEYPAIR (local file)"
else
  fail "No upgrade-authority key available.

CI: set the AUTHORITY_KEY_JSON env var to the raw JSON keypair contents
(from secrets.DEVNET_AUTHORITY_KEY_JSON). The script reads it via process
substitution; the bytes never touch the runner's disk.

Local: copy your existing devnet upgrade-authority keypair to:

  cp /path/to/devnet-authority.json $AUTHORITY_KEYPAIR
  chmod 600 $AUTHORITY_KEYPAIR

This script REFUSES to mint a new authority — losing one would orphan the
on-chain programs (their upgrade authority would point at a key nobody has)."
fi

# Build DEPLOY_ORDER from the manifest. Programs whose .programs.<name>
# entry is null are skipped — their first deploy must happen offline. This
# means ario_ant_escrow is naturally excluded today; the moment its first
# deploy lands and the resulting ID is committed to program-ids/devnet.json,
# it'll start flowing through CI without any code change here.
#
# Order matters for runtime CPI dependencies (ario-arns CPIs into ario-gar;
# ario-ant CPIs into mpl_core). The canonical order below is honored,
# filtered down to what's in the manifest.
CANONICAL_ORDER=(ario_core ario_ant ario_gar ario_arns ario_ant_escrow)
DEPLOY_ORDER=()
declare -A PROGRAM_ID_OF
echo
echo "Program IDs (from $PROGRAM_IDS_PATH):"
for prog in "${CANONICAL_ORDER[@]}"; do
  pid="$(jq -r --arg k "$prog" '.programs[$k] // ""' "$PROGRAM_IDS_PATH")"
  if [[ -z "$pid" || "$pid" == "null" ]]; then
    echo "  $prog: (null — first deploy is an offline operator action; skipping)"
    continue
  fi
  echo "  $prog: $pid"
  PROGRAM_ID_OF[$prog]="$pid"
  DEPLOY_ORDER+=("$prog")
done
echo "  authority: $WALLET"
echo "  authority source: $AUTHORITY_SOURCE"

if [[ ${#DEPLOY_ORDER[@]} -eq 0 ]]; then
  fail "$PROGRAM_IDS_PATH has no non-null programs. CI cannot do a first deploy."
fi

# ------------------------------------------------------------
step 2 "Top up authority on $DEPLOY_CLUSTER"
# ------------------------------------------------------------
if [[ "${SKIP_AIRDROP:-0}" == "1" ]]; then
  warn "SKIP_AIRDROP=1 — skipping airdrop loop"
else
  TARGET_SOL=${TARGET_SOL:-30} bash "$REPO_ROOT/scripts/fund-devnet.sh" || \
    warn "fund-devnet.sh exited non-zero — top up manually before re-running"
fi

# ------------------------------------------------------------
step 3 "Build (BUILD_NETWORK=$BUILD_NETWORK; sync declare_id!() from manifest)"
# ------------------------------------------------------------
# build-sbf.sh --sync-from-manifest patches each program's declare_id!()
# in source to match program-ids/devnet.json, builds the BPF artifacts,
# then restores source on EXIT. The .so files in target/deploy/ have the
# correct (live) program IDs baked in; the source tree is unchanged.
if [[ "${SKIP_BUILD:-0}" == "1" ]]; then
  warn "SKIP_BUILD=1 — reusing existing target/deploy/*.so (caller asserts declare_id!() matches manifest)"
else
  PROGRAM_IDS_PATH="$PROGRAM_IDS_PATH" BUILD_NETWORK="$BUILD_NETWORK" \
    bash "$REPO_ROOT/build-sbf.sh" --sync-from-manifest
fi

for prog in "${DEPLOY_ORDER[@]}"; do
  so_file="target/deploy/${prog}.so"
  [[ -f "$so_file" ]] || fail "Missing $so_file"
  echo "  ✓ $so_file ($(du -h "$so_file" | awk '{print $1}'))"
done

# ------------------------------------------------------------
step 4 "Deploy upgrades (signed by upgrade authority)"
# ------------------------------------------------------------
# Raw `solana program deploy --program-id <PUBKEY>` (not `anchor deploy`)
# because we don't have program keypair files in CI. The pubkey comes from
# the manifest; the upgrade authority signs (loaded by solana_auth from
# AUTHORITY_KEY_JSON env via process substitution in CI, or from
# AUTHORITY_KEYPAIR file locally). For an existing program, --program-id
# accepts an address rather than a keypair. See `solana program deploy --help`.
declare -A DEPLOYED_AT
for prog in "${DEPLOY_ORDER[@]}"; do
  pid="${PROGRAM_ID_OF[$prog]}"
  echo
  echo "Deploying $prog ($pid) ..."
  solana_auth program deploy \
    --program-id "$pid" \
    "target/deploy/${prog}.so"
  DEPLOYED_AT[$prog]="$(date -u +"%Y-%m-%dT%H:%M:%SZ")"
done

# ------------------------------------------------------------
step 5 "Update deployed_at timestamps in $PROGRAM_IDS_PATH"
# ------------------------------------------------------------
# We only ever write to .deployer + .deployed_at. .programs is sacred —
# it's a committed input to this script and only changes when an operator
# does a first deploy and edits it by hand.
mkdir -p "$(dirname "$PROGRAM_IDS_PATH")"

# Build the per-program timestamps as a JSON object via jq, then merge.
TS_FRAGMENT_ARGS=()
for prog in "${DEPLOY_ORDER[@]}"; do
  TS_FRAGMENT_ARGS+=(--arg "ts_${prog}" "${DEPLOYED_AT[$prog]}")
done

# Construct the jq merge expression dynamically so we only touch programs
# we actually deployed (preserves any existing nulls for programs not in
# DEPLOY_ORDER, e.g. ario_ant_escrow).
TS_MERGE=""
for prog in "${DEPLOY_ORDER[@]}"; do
  [[ -n "$TS_MERGE" ]] && TS_MERGE+=", "
  TS_MERGE+="${prog}: \$ts_${prog}"
done

jq \
  --arg cluster "$DEPLOY_CLUSTER" \
  --arg deployer "$WALLET" \
  "${TS_FRAGMENT_ARGS[@]}" \
  ". + {cluster: \$cluster, deployer: \$deployer}
     | .deployed_at = ((.deployed_at // {}) + { ${TS_MERGE} })
     | .external = ((.external // {}) + {
         mpl_core: ((.external.mpl_core // \"CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d\"))
       })" \
  "$PROGRAM_IDS_PATH" > "${PROGRAM_IDS_PATH}.tmp" && \
  mv "${PROGRAM_IDS_PATH}.tmp" "$PROGRAM_IDS_PATH"

echo "Updated $PROGRAM_IDS_PATH (deployer + deployed_at; .programs untouched)"

# ------------------------------------------------------------
echo
echo -e "${GREEN}Devnet deployment complete.${NC}"
echo "Program IDs (input):     $PROGRAM_IDS_PATH"
echo "Programs upgraded:       ${DEPLOY_ORDER[*]}"
echo
echo "Next steps:"
echo "  1. Commit $PROGRAM_IDS_PATH if deployer / deployed_at changed."
echo "  2. Run network-init (config / mint / registries) from the migration"
echo "     repo's import tooling, pointing at \$DEPLOY_CLUSTER and these IDs."
echo "  3. Tag the deploy: git tag devnet/\$(date -u +%Y%m%d-%H%M)"
