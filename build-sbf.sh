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
#                              # program-ids/staging.json), build, restore
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
# Prefer storing program IDs in on-chain state (see the `arns_program_id`
# field on `GatewaySettings` for the canonical pattern) rather than
# adding entries here. Entries here exist only when the cross-program
# target must be pinned at COMPILE TIME (compile-time `address = ...`
# constraints on a typed `Account<>`, etc.) and on-chain storage isn't
# viable.
#
# ario-gar pins `ARIO_CORE_PROGRAM_ID` as a `pub const` in lib.rs
# (single-line, `#[rustfmt::skip]`-tagged) to use as the `address = ...`
# on the `ario_core_program` account in the `release_treasury_to_recipient`
# hand-rolled invoke_signed CPI (see CLAUDE.md "Treasury release CPI").
# Without this entry the placeholder could ship to a real cluster and
# only fail at runtime on the first epoch distribution with
# `InvalidProgramId`. Audit M-5 (2026-05-29).
EXTRA_HARDCODED=(
  "target/deploy/ario_core-keypair.json:programs/ario-gar/src/lib.rs:ARioCoreProgramXXXXXXXXXXXXXXXXXXXXXXXXXXXX"
)
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
  local manifest="${PROGRAM_IDS_PATH:-program-ids/staging.json}"
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

  # Patch the `ARIO_CORE_PROGRAM_ID` const in ario-gar's lib.rs so
  # distribute_epoch's CPI target matches the deployed ario-core. We
  # can't pull in ario_core::ID directly because the Cargo graph already
  # has ario-core → ario-gar (cpi feature), and adding the reverse arc
  # would be circular. The const lives in ario-gar source as a
  # placeholder; this sed rewrites it per cluster.
  local core_prog_id
  core_prog_id="$(jq -r '.programs.ario_core // ""' "$manifest")"
  local gar_lib="programs/ario-gar/src/lib.rs"
  if [[ -n "$core_prog_id" && "$core_prog_id" != "null" && -f "$gar_lib" ]]; then
    # Match only the ARIO_CORE_PROGRAM_ID const line; declare_id!() was
    # already handled in the loop above against a different macro
    # surface, no risk of double-matching. The const is kept on one
    # line in source for this regex to work.
    sed -i.bak -E \
      "s|(ARIO_CORE_PROGRAM_ID: Pubkey = anchor_lang::solana_program::pubkey!\(\")[^\"]+(\"\))|\1$core_prog_id\2|" \
      "$gar_lib"
    rm -f "${gar_lib}.bak"
    echo "[build-sbf] patched $gar_lib → ARIO_CORE_PROGRAM_ID = \"$core_prog_id\""
  fi

  # Note: previous revisions of this function patched Cargo.toml to remove
  # ario-ant-escrow from the workspace `members` array (PR #6) and later
  # also add it to `workspace.exclude` (PR #45). That approach was meant
  # to work around `cargo build-sbf` 2.1.0's "copy every workspace
  # member's .so" pass failing when escrow's .so didn't exist — escrow
  # has a `compile_error!` requiring exactly one of `network-mainnet` /
  # `network-devnet` features, and the default workspace build didn't
  # supply either. The exclude approach broke escrow's
  # `xxx.workspace = true` inheritance (`error inheriting edition from
  # workspace root manifest`), forcing a separate `cargo build-sbf
  # --manifest-path` step which then failed on the same inheritance.
  #
  # We now solve this at the build invocation: passing
  # `--no-default-features --features network-${BUILD_NETWORK:-devnet}`
  # to the workspace build (see end of script) satisfies escrow's
  # compile_error!, produces escrow.so as part of the normal workspace
  # build, and keeps workspace-inheritance intact. The other four crates
  # have `default = []` so `--no-default-features` is a no-op for them.
  # Escrow's `unsafe-allow-test-attestor-pubkey` default also gets turned
  # off, which is correct — the F-4 const-eval guard then fires
  # appropriately on real-network builds.
  :
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

# Workspace build — one invocation produces all 5 .so files in target/deploy/.
#
# `--no-default-features` is necessary because ario-ant-escrow's defaults
# (`["network-mainnet", "unsafe-allow-test-attestor-pubkey"]`) would
# trigger its `compile_error!` (mutual exclusivity with whatever
# `--features network-<cluster>` we add). It's a no-op for the other 4
# crates (they all declare `default = []`). Disabling
# `unsafe-allow-test-attestor-pubkey` also re-enables the F-4 const-eval
# guard in state.rs — desirable for any real-network build.
#
# `--features network-${BUILD_NETWORK:-devnet}` satisfies escrow's
# `compile_error!` requiring exactly one of network-mainnet /
# network-devnet to be enabled. The feature is only declared on escrow,
# so it activates there only; the other crates ignore it.
#
# `--features devnet-shrunk` (BUILD_NETWORK=devnet only) shrinks the
# zero-copy registry account sizes on ario-gar + ario-arns to fit on
# devnet. Mainnet builds leave it off so production sizes are baked in.
# It's declared as a no-op feature on every workspace member (PR #18) so
# the workspace-level `--features` flag finds it without erroring.
SBF_FEATURE_ARGS=(--no-default-features --features "network-${BUILD_NETWORK:-devnet}")
if [[ "${BUILD_NETWORK:-}" == "devnet" ]]; then
    SBF_FEATURE_ARGS+=(--features devnet-shrunk)
    echo "[build-sbf] BUILD_NETWORK=devnet → enabling devnet-shrunk feature on ario-gar + ario-arns"
fi
# Use `anchor build` (not `cargo build-sbf`) so we get IDLs alongside
# the .so files. anchor build wraps cargo-build-sbf internally + adds
# IDL extraction via the `idl-build` cargo feature, and its
# `address` field in target/idl/*.json picks up the
# already-patched declare_id!() values (essential — the package-release.sh
# step bundles those IDLs for downstream SDK consumers).
#
# anchor build forwards args after `--` to cargo build-sbf; our feature
# combo (--no-default-features --features network-...,devnet-shrunk)
# passes through unchanged, in addition to anchor's own
# `--features idl-build`. Cargo unions multiple --features flags.
echo "[build-sbf] anchor build -- ${SBF_FEATURE_ARGS[*]}"
anchor build -- "${SBF_FEATURE_ARGS[@]}"

ls -la target/deploy/*.so target/idl/*.json 2>/dev/null || ls -la target/deploy/*.so
