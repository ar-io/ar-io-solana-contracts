#!/usr/bin/env bash
# ============================================================
# AR.IO Solana Contracts — Release Bundle
# ============================================================
#
# Packages everything a downstream consumer needs to integrate with a
# specific build of the contracts:
#
#   release/ar-io-solana-contracts-<version>/
#     program-ids.json                # cluster + program IDs
#     idl/                            # Anchor 0.31 IDL JSON, one per program
#       ario_core.json
#       ario_gar.json
#       ario_arns.json
#       ario_ant.json
#       ario_ant_escrow.json
#     so/                             # compiled BPF binaries (downstream
#                                     #   may load these into their own
#                                     #   Surfpool / test-validator instance)
#       ario_core.so
#       ario_gar.so
#       ario_arns.so
#       ario_ant.so
#       ario_ant_escrow.so
#     SHA256SUMS                      # checksums for every bundled file
#     VERSION                         # semver / git ref / build metadata
#
# Outputs both the directory and a tarball at
# `release/ar-io-solana-contracts-<version>.tar.gz`. The tarball is what
# CI uploads to the GitHub release.
#
# NOTE: program keypairs are intentionally NOT bundled, on either cluster.
# They're treated as secrets and live only in the original deployer's
# offline custody — CI does not hold them and cannot mint new program IDs.
# Anyone with a program keypair can squat the program ID on a fresh
# cluster and stand up a malicious .so at the canonical address — for a
# protocol with downstream SDK / indexer / dApp consumers that's an
# attack surface we don't want to publish in a release tarball.
#
# Usage:
#   bash scripts/package-release.sh                       # auto version from git
#   VERSION=v0.2.0 bash scripts/package-release.sh        # explicit version
#   CLUSTER=devnet bash scripts/package-release.sh
# ============================================================

set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

VERSION="${VERSION:-$(git describe --tags --always --dirty 2>/dev/null || echo "unknown")}"
CLUSTER="${CLUSTER:-devnet}"
PROGRAM_IDS_PATH="${PROGRAM_IDS_PATH:-program-ids/${CLUSTER}.json}"

if [[ -n "${INCLUDE_KEYPAIRS:-}" ]]; then
  echo "ERROR: INCLUDE_KEYPAIRS is no longer supported. Program keypairs are" >&2
  echo "       treated as secrets and never bundled into release artifacts." >&2
  echo "       They live in the original deployer's offline custody and are" >&2
  echo "       only needed for first deploys (a manual operator action)." >&2
  exit 2
fi

OUT_BASE="$REPO_ROOT/release"
OUT_DIR="$OUT_BASE/ar-io-solana-contracts-${VERSION}"

mkdir -p "$OUT_DIR/idl" "$OUT_DIR/so"

require() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "ERROR: missing $path" >&2
    echo "Run \`anchor build\` (and \`scripts/devnet-deploy.sh\` for program-ids/$CLUSTER.json) first." >&2
    exit 1
  fi
}

# Read the manifest first so we can skip programs that aren't on this
# cluster yet (their program ID is null in program-ids/<cluster>.json).
# Bundling a .so whose declare_id!() doesn't correspond to any live
# program ID would just hand downstream consumers a footgun.
require "$PROGRAM_IDS_PATH"
PROGRAMS=()
for prog in ario_core ario_gar ario_arns ario_ant ario_ant_escrow; do
  if command -v jq >/dev/null 2>&1; then
    pid="$(jq -r --arg k "$prog" '.programs[$k] // ""' "$PROGRAM_IDS_PATH")"
  else
    pid="present" # without jq, fall back to "include everything we can"
  fi
  if [[ -z "$pid" || "$pid" == "null" ]]; then
    echo "  - skipping $prog (no program ID in $PROGRAM_IDS_PATH for $CLUSTER)"
    continue
  fi
  PROGRAMS+=("$prog")
done

if [[ ${#PROGRAMS[@]} -eq 0 ]]; then
  echo "ERROR: no programs in $PROGRAM_IDS_PATH have a non-null program ID." >&2
  echo "Bundle would be empty. Run scripts/devnet-deploy.sh first." >&2
  exit 1
fi

# 1. IDLs (from `anchor build`).
for prog in "${PROGRAMS[@]}"; do
  require "target/idl/${prog}.json"
  cp "target/idl/${prog}.json" "$OUT_DIR/idl/"
done

# 2. Compiled BPF.
for prog in "${PROGRAMS[@]}"; do
  require "target/deploy/${prog}.so"
  cp "target/deploy/${prog}.so" "$OUT_DIR/so/"
done

# 3. Program IDs manifest. (Already require()d above.)
cp "$PROGRAM_IDS_PATH" "$OUT_DIR/program-ids.json"

# 4. VERSION / metadata.
cat > "$OUT_DIR/VERSION" <<EOF
version=${VERSION}
cluster=${CLUSTER}
git_ref=$(git rev-parse HEAD 2>/dev/null || echo unknown)
git_describe=$(git describe --tags --always --dirty 2>/dev/null || echo unknown)
built_at=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
anchor_version=$(anchor --version 2>/dev/null || echo unknown)
solana_version=$(solana --version 2>/dev/null || echo unknown)
EOF

# 5. Checksums.
(cd "$OUT_DIR" && find . -type f ! -name SHA256SUMS -print0 | sort -z | \
  xargs -0 shasum -a 256) > "$OUT_DIR/SHA256SUMS"

# 6. Tarball.
TAR_PATH="$OUT_BASE/ar-io-solana-contracts-${VERSION}.tar.gz"
tar -C "$OUT_BASE" -czf "$TAR_PATH" "ar-io-solana-contracts-${VERSION}"

echo
echo "Release bundle ready:"
echo "  dir:     $OUT_DIR"
echo "  tarball: $TAR_PATH"
echo "  size:    $(du -h "$TAR_PATH" | awk '{print $1}')"
echo
echo "Contents:"
ls -la "$OUT_DIR"
