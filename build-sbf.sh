#!/usr/bin/env bash
# Build all BPF artifacts into target/deploy/.
#
# WARNING — does NOT regenerate IDLs under target/idl/. For deploys that the
# SDK codegen will consume, use `anchor build` instead. This script's
# `cargo build-sbf` invocation only updates .so files, leaving IDLs from the
# previous `anchor build` in place. The SDK codegen (`yarn codegen`) reads
# IDLs to generate event decoders — running it against stale IDLs silently
# drops events. The codegen tool now hard-errors with `STALE-IDL DRIFT
# DETECTED` when this is detected, but the cheaper prevention is to use
# `anchor build` for any flow that ends in `yarn codegen` or a deploy.
#
# Use this script for: contract iteration loops where you only need the .so.
# Use `anchor build` for: deploys, IDL/SDK refresh, anything that downstream
# consumers will see.
#
# History: an earlier toolchain combo (Anchor 0.30 + Solana 1.18) left ario_gar.so
# truncated when invoked as a single workspace `cargo build-sbf`, so this script
# used to loop `cargo build-sbf -- -p <crate>`. Under Anchor 0.31.1 + Agave 2.1.0
# that workaround is BROKEN: cargo-build-sbf 2.1.0 unconditionally tries to copy
# every workspace member's .so into target/deploy/ at the end of the build, and
# fails on the first member you didn't pass to -p. A plain workspace build with
# the current toolchain produces all four .so's correctly (verified ario_gar.so
# at ~895 KB, well above the 100 KB truncation guard in run-surfpool-local.sh).
#
# DECLARE_ID DRIFT GUARD
# ----------------------
# Each program's `declare_id!()` macro and target/deploy/<crate>-keypair.json
# must agree. When they don't, deployments compile fine but fail at runtime
# with DeclaredProgramIdMismatch (Anchor #4100) on the first CPI. This script
# checks for drift before building and refuses to produce stale artifacts.
#
# Modes:
#   ./build-sbf.sh             # check + build; abort on drift with instructions
#   ./build-sbf.sh --sync      # auto-sync declare_id!() to keypairs, build,
#                              # then restore source on EXIT (safe for CI; the
#                              # restore happens even on Ctrl-C / failure)
#   ./build-sbf.sh --sync-from-manifest
#                              # patch source declare_id!() from
#                              # program-ids/<cluster>.json (path read from
#                              # PROGRAM_IDS_PATH env var, default
#                              # program-ids/devnet.json), build, restore
#                              # source on EXIT. Used by CI flows that don't
#                              # have program keypair files (the deploy
#                              # authority is the only key in CI; program IDs
#                              # come from the committed manifest).
#   ./build-sbf.sh --skip-check# build without checking (matches old behaviour)
#   ./build-sbf.sh --check-only# fail with non-zero exit on drift; never build
#                              # (suitable as a pre-commit / CI guard)
set -euo pipefail
cd "$(dirname "$0")"
export PATH="${HOME}/.local/share/solana/install/active_release/bin:${PATH}"

MODE="check"
for arg in "$@"; do
  case "$arg" in
    --sync) MODE="sync" ;;
    --sync-from-manifest) MODE="sync-from-manifest" ;;
    --skip-check) MODE="skip" ;;
    --check-only) MODE="check-only" ;;
    -h|--help)
      sed -n '2,40p' "$0" | sed 's/^# \?//'
      exit 0
      ;;
    *) echo "unknown flag: $arg" >&2; exit 2 ;;
  esac
done

# Programs to check. Each entry: <crate-name>:<lib.rs path>
PROGRAMS=(
  "ario_core:programs/ario-core/src/lib.rs"
  "ario_gar:programs/ario-gar/src/lib.rs"
  "ario_arns:programs/ario-arns/src/lib.rs"
  "ario_ant:programs/ario-ant/src/lib.rs"
  "ario_ant_escrow:programs/ario-ant-escrow/src/lib.rs"
)
# Extra files that hardcode a program ID and must be patched alongside
# declare_id!() (anchor keys sync only handles the macro itself).
# Entry: <pubkey-source-keypair>:<file>:<placeholder>
#
# Currently empty — the prior `epoch.rs` hardcoded ArNS ID was eliminated
# by storing arns_program_id in GatewaySettings (set at initialize time).
# See refactor/gar-arns-program-id-in-settings. New entries should be
# added here only as a last resort; prefer storing the value in on-chain
# state.
EXTRA_HARDCODED=()
# `${arr[@]}` errors under `set -u` for empty arrays on bash 3.x (macOS).
# All loops use the `${EXTRA_HARDCODED[@]+"${EXTRA_HARDCODED[@]}"}` guard
# below to expand to nothing when empty rather than tripping the unbound
# variable check.

# Returns the pubkey from a Solana keypair JSON file (calls solana-keygen).
keypair_pubkey() {
  solana-keygen pubkey "$1" 2>/dev/null
}
# Returns the declare_id!() string literal from a Rust file.
declare_id_in() {
  grep -oE 'declare_id!\("[^"]+"\)' "$1" | head -n1 | sed -E 's/.*"([^"]+)".*/\1/'
}

check_drift() {
  local drift=0
  for entry in "${PROGRAMS[@]}"; do
    local crate="${entry%%:*}"
    local lib="${entry##*:}"
    local kp="target/deploy/${crate}-keypair.json"
    if [[ ! -f "$kp" ]]; then
      # First-ever build — keypair not generated yet. cargo build-sbf will
      # create it; the next invocation will catch any drift.
      continue
    fi
    local source_id; source_id="$(declare_id_in "$lib")"
    local keypair_id; keypair_id="$(keypair_pubkey "$kp")"
    if [[ "$source_id" != "$keypair_id" ]]; then
      echo "  drift: $lib"
      echo "    declare_id!()  = $source_id"
      echo "    keypair pubkey = $keypair_id"
      drift=1
    fi
  done
  for entry in "${EXTRA_HARDCODED[@]+"${EXTRA_HARDCODED[@]}"}"; do
    local kp="${entry%%:*}"; local rest="${entry#*:}"
    local file="${rest%%:*}"; local placeholder="${rest##*:}"
    [[ -f "$kp" && -f "$file" ]] || continue
    local keypair_id; keypair_id="$(keypair_pubkey "$kp")"
    # Drift if either (a) the placeholder is still present, OR (b) some
    # OTHER pubkey is hardcoded (i.e. drifted from the current keypair).
    if grep -q "\"$placeholder\"" "$file"; then
      echo "  drift: $file (still has placeholder $placeholder; needs $keypair_id)"
      drift=1
    elif ! grep -q "\"$keypair_id\"" "$file"; then
      echo "  drift: $file (hardcoded ID does not match $kp pubkey $keypair_id)"
      drift=1
    fi
  done
  return $drift
}

sync_keys() {
  command -v anchor >/dev/null 2>&1 || {
    echo "anchor CLI required for --sync; install via avm install 0.31.1 && avm use 0.31.1" >&2
    exit 1
  }
  # Snapshot every file `sync_keys` may mutate so the EXIT trap can restore
  # the developer's exact pre-sync content (including any uncommitted edits).
  # Using `git checkout` for restore would clobber work-in-progress.
  SYNC_BACKUP_DIR="$(mktemp -d)"
  export SYNC_BACKUP_DIR
  local snapshot_files=("Anchor.toml")
  for entry in "${PROGRAMS[@]}"; do snapshot_files+=("${entry##*:}"); done
  for entry in "${EXTRA_HARDCODED[@]+"${EXTRA_HARDCODED[@]}"}"; do
    local rest="${entry#*:}"; snapshot_files+=("${rest%%:*}")
  done
  for f in "${snapshot_files[@]}"; do
    [[ -f "$f" ]] || continue
    local backup
    backup="$SYNC_BACKUP_DIR/$(echo "$f" | tr '/' '_')"
    cp "$f" "$backup"
    echo "$f" > "$backup.path"
  done

  echo "[build-sbf] anchor keys sync (temporary; restored on EXIT)"
  anchor keys sync >/dev/null
  for entry in "${EXTRA_HARDCODED[@]+"${EXTRA_HARDCODED[@]}"}"; do
    local kp="${entry%%:*}"; local rest="${entry#*:}"
    local file="${rest%%:*}"; local placeholder="${rest##*:}"
    [[ -f "$kp" && -f "$file" ]] || continue
    local keypair_id; keypair_id="$(keypair_pubkey "$kp")"
    sed -i.bak "s|\"$placeholder\"|\"$keypair_id\"|" "$file"
    rm -f "${file}.bak"
    echo "[build-sbf] patched $file → $keypair_id"
  done
}

restore_keys() {
  # Restore from the per-file pre-sync snapshots taken in sync_keys /
  # sync_from_manifest. Preserves any work-in-progress edits in those
  # files (using `git checkout` instead would clobber uncommitted changes).
  [[ -n "${SYNC_BACKUP_DIR:-}" && -d "$SYNC_BACKUP_DIR" ]] || return 0
  # Iterate only the data files; each one has a sibling `.path` sidecar
  # written by sync_keys that records the original path.
  for backup in "$SYNC_BACKUP_DIR"/*; do
    case "$backup" in *.path) continue ;; esac
    [[ -f "$backup" && -f "$backup.path" ]] || continue
    local original_path
    original_path="$(head -n1 "$backup.path")"
    [[ -n "$original_path" ]] || continue
    cp "$backup" "$original_path"
  done
  rm -rf "$SYNC_BACKUP_DIR"
  echo "[build-sbf] restored declare_id-bearing files from pre-sync snapshot"
}

# Patch source declare_id!() from a committed program-ids/<cluster>.json.
# Used by CI flows that DO NOT have program keypair files (the only key in
# CI is the upgrade authority; program IDs are sacred / committed once at
# original-deploy time and live in program-ids/<cluster>.json from then on).
#
# Programs whose manifest entry is null (i.e. not yet deployed on this
# cluster — typically `ario_ant_escrow` until its first deploy lands) are
# skipped silently; their source declare_id!() is left untouched. Any
# subsequent deploy attempt for those programs would have to come from a
# maintainer laptop with the program keypair, not CI.
sync_from_manifest() {
  local manifest="${PROGRAM_IDS_PATH:-program-ids/devnet.json}"
  command -v jq >/dev/null 2>&1 || {
    echo "jq required for --sync-from-manifest" >&2
    exit 1
  }
  [[ -f "$manifest" ]] || {
    echo "manifest not found: $manifest" >&2
    exit 1
  }

  SYNC_BACKUP_DIR="$(mktemp -d)"
  export SYNC_BACKUP_DIR

  for entry in "${PROGRAMS[@]}"; do
    local crate="${entry%%:*}"
    local lib="${entry##*:}"
    local prog_id
    prog_id="$(jq -r --arg k "$crate" '.programs[$k] // ""' "$manifest")"
    if [[ -z "$prog_id" || "$prog_id" == "null" ]]; then
      echo "[build-sbf] skipping $crate (no program ID in $manifest — first deploy must happen offline)"
      continue
    fi
    [[ -f "$lib" ]] || {
      echo "[build-sbf] WARN: $lib missing for $crate; skipping" >&2
      continue
    }
    local backup
    backup="$SYNC_BACKUP_DIR/$(echo "$lib" | tr '/' '_')"
    cp "$lib" "$backup"
    echo "$lib" > "$backup.path"
    # Replace the entire declare_id!("...") string literal. Anchor's macro
    # accepts only a single string arg so a regex over the literal is safe;
    # this also handles any pre-existing value (placeholder or real).
    sed -i.bak -E "s|declare_id!\(\"[^\"]+\"\)|declare_id!(\"$prog_id\")|" "$lib"
    rm -f "${lib}.bak"
    echo "[build-sbf] patched $lib → declare_id!(\"$prog_id\")"
  done
}

case "$MODE" in
  check-only)
    # Pre-commit / CI hook mode: only check drift, never build.
    if check_drift; then
      echo "[build-sbf] declare_id source matches deploy keypairs (or no keypairs yet)."
      exit 0
    else
      echo "[build-sbf] declare_id drift detected (see above)." >&2
      exit 1
    fi
    ;;
  check)
    if ! check_drift; then
      cat >&2 <<EOF

[build-sbf] declare_id drift detected (see above).

Programs deployed under the current target/deploy/*-keypair.json files use
pubkeys that don't match the placeholders in source. Building now would
produce .so files that fail with DeclaredProgramIdMismatch (#4100) on the
first CPI after deploy.

Pick one:
  ./build-sbf.sh --sync         # patch source to match keypairs, build,
                                # restore source on exit (canonical localnet flow)
  anchor keys sync              # mutate source persistently
  ./build-sbf.sh --skip-check   # build anyway (mainnet/devnet — see Anchor.toml)

EOF
      exit 1
    fi
    ;;
  sync)
    # EXIT covers normal exit + most signals; INT/TERM/HUP add coverage for
    # shells that don't propagate to EXIT. SIGKILL bypasses all traps —
    # in that case, `git checkout -- programs/*/src/lib.rs Anchor.toml
    # programs/ario-gar/src/instructions/epoch.rs` will restore the tree.
    trap restore_keys EXIT INT TERM HUP
    sync_keys
    ;;
  sync-from-manifest)
    # Same restore semantics as `sync`. SIGKILL bypass: `git checkout --
    # programs/*/src/lib.rs` will restore the tree.
    trap restore_keys EXIT INT TERM HUP
    sync_from_manifest
    ;;
  skip)
    : # build whatever's in source; user asserts they know
    ;;
esac

# Check whether the test ATTESTOR_PUBKEY is still in place.  Not
# --strict here: deploy scripts enforce that separately.  We use the
# exit code to decide whether ario-ant-escrow can be compiled for a
# real-network target — if the test key is present the compile-time
# guard in state.rs would reject a network-mainnet/network-devnet build,
# so we skip escrow and warn instead of aborting the whole build.
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
if "${SCRIPT_DIR}/scripts/check-attestor-pubkey.sh" 2>/dev/null; then
  _attestor_ok=1
else
  _attestor_ok=0
fi

# Default network is mainnet; override with `BUILD_NETWORK=devnet`.
build_network="${BUILD_NETWORK:-mainnet}"
case "${build_network}" in
  mainnet) escrow_features="network-mainnet" ;;
  devnet)  escrow_features="network-devnet"  ;;
  *)
    echo "[build-sbf] BUILD_NETWORK must be 'mainnet' or 'devnet', got '${build_network}'" >&2
    exit 1
    ;;
esac

# `ario-ant-escrow` defaults to `unsafe-allow-test-attestor-pubkey` for
# non-SBF dev convenience (`cargo test` Just Works with the test seed).
# Real-network SBF builds MUST opt out — F-4 compile-time guard refuses
# a real-network build that still has the test ATTESTOR_PUBKEY.
# Skip escrow compilation when the test key is present so binary-only
# release builds succeed; deploy scripts block on --strict before any
# cluster upgrade, providing the real safety gate.
if [[ "${_attestor_ok}" -eq 1 ]]; then
  # `cargo build-sbf` doesn't expose a per-package feature override
  # directly. Use `--no-default-features --features` scoped to escrow.
  cargo build-sbf -- --package ario-ant-escrow --no-default-features --features "${escrow_features}"
else
  echo "[build-sbf] WARN: skipping ario-ant-escrow — test ATTESTOR_PUBKEY in state.rs." >&2
  echo "[build-sbf]       Replace it before running a real cluster deploy." >&2
fi

# Build the rest of the workspace with default features.
cargo build-sbf -- \
  --package ario-core --package ario-gar --package ario-arns --package ario-ant
ls -la target/deploy/*.so
