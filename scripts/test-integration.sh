#!/usr/bin/env bash
#
# Run integration tests reliably regardless of prior `target/deploy/*.so` state.
#
# Why this exists:
#
#   `solana-program-test`'s `ProgramTest::new(name, id, processor!(...))`
#   uses the in-process processor by default, but the framework still
#   loads `name.so` from BPF_OUT_DIR (or target/deploy/) when found —
#   preferring the BPF artifact over the in-process processor. The .so's
#   `declare_id!()` is embedded at build time. Two sources of drift:
#
#     1. `build-sbf.sh --sync` builds with the keypair-derived program ID
#        embedded in the .so, then restores source to the placeholder ID
#        on EXIT. After: lib (placeholder) ≠ .so (synced). Tests load
#        the synced .so → `Custom(4100) DeclaredProgramIdMismatch` on
#        the first CPI.
#
#     2. Builds across branches with different program IDs leave a stale
#        .so from the prior branch's keypair lying in target/deploy.
#
#   Tests also need mpl_core.so for ANT-related CPI verification
#   (CreateV1, TransferV1, UpdateV1, UpdatePluginV1, BurnV1).
#
# What this script does:
#
#   1. Stages mpl_core.so into `target/test-fixtures/` (the test-only
#      BPF_OUT_DIR).
#   2. **Rebuilds every ario-*.so via plain `cargo build-sbf`** (NO
#      --sync) so the .so files match the source's *current*
#      declare_id values. Whether source is in placeholder mode or
#      synced mode, the freshly-built .so will match the lib that
#      `cargo test` compiles. Built via `--sbf-out-dir target/test-fixtures`
#      so artifacts never land in `target/deploy` — the escrow test .so
#      embeds the PUBLIC test attestor key and must not be reused by a
#      deploy flow (see the SECURITY note at the build step below).
#   3. Runs the requested cargo test target with BPF_OUT_DIR pointed at
#      `target/test-fixtures`.
#
# Usage:
#   ./scripts/test-integration.sh ario-arns
#   ./scripts/test-integration.sh ario-ant-escrow test_admin_purge
#   ./scripts/test-integration.sh --all
#   FAST=1 ./scripts/test-integration.sh ario-arns    # skip the rebuild
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEPLOY_DIR="$REPO_ROOT/target/deploy"
FIXTURES_DIR="$REPO_ROOT/target/test-fixtures"
MPL_CORE_SRC="$REPO_ROOT/programs/ario-ant-escrow/tests/fixtures/mpl_core.so"

if [[ ! -f "$MPL_CORE_SRC" ]]; then
    echo "ERROR: mpl_core.so fixture missing at $MPL_CORE_SRC" >&2
    exit 1
fi

# SECURITY: test artifacts are built into a test-only directory
# ($FIXTURES_DIR), never into target/deploy. The escrow .so under test is
# compiled with `unsafe-allow-test-attestor-pubkey`, which bakes in the
# PUBLIC deterministic test attestor key (seed [1u8; 32]). target/deploy is
# the canonical deploy path that build-sbf.sh / anchor build write to and
# that devnet-deploy.sh + mainnet-prepare-upgrade.sh reuse (incl. with
# SKIP_BUILD=1). If a test-key escrow .so landed there, a reuse-the-artifact
# deploy could ship a binary that accepts forged Arweave attestations. So we
# keep the two directories disjoint and point BPF_OUT_DIR at the test dir.
mkdir -p "$FIXTURES_DIR"

# Stage mpl_core.so into the test-only lookup dir. Idempotent.
src_size=$(stat -c%s "$MPL_CORE_SRC")
dst_size=$(stat -c%s "$FIXTURES_DIR/mpl_core.so" 2>/dev/null || echo 0)
if [[ "$src_size" != "$dst_size" ]]; then
    cp "$MPL_CORE_SRC" "$FIXTURES_DIR/mpl_core.so"
fi

# Rebuild .so files to match current source's declare_id. Skip via
# FAST=1 when iterating tests against an already-fresh build.
if [[ "${FAST:-0}" != "1" ]]; then
    echo "==> Rebuilding ario-*.so into $FIXTURES_DIR (FAST=1 to skip)"
    # Build all 5 programs into the test dir (NOT target/deploy) via
    # --sbf-out-dir. devnet-shrunk so registry sizes match the in-tree
    # integration-test fixture expectations.
    cargo build-sbf --features devnet-shrunk --sbf-out-dir "$FIXTURES_DIR" 2>&1 | tail -3
    # Re-build escrow ALONE with the opt-in test attestor key, overwriting
    # the prod-key escrow .so the workspace build just produced — again into
    # the test dir only.
    #
    # `unsafe-allow-test-attestor-pubkey` is deliberately NOT in escrow's
    # default features (it would bake the public test attestor key into
    # deploy artifacts — see programs/ario-ant-escrow/Cargo.toml). The
    # claim_*_attested tests sign with the deterministic test seed
    # `[1u8; 32]`, so the escrow .so under test must opt into it. A
    # workspace-level `cargo build-sbf --features <name>` is rejected by
    # cargo-build-sbf 2.1.0 unless every selected package declares <name>,
    # so we scope it to escrow via --manifest-path.
    echo "==> Rebuilding ario_ant_escrow.so with unsafe-allow-test-attestor-pubkey (test dir only)"
    cargo build-sbf --manifest-path programs/ario-ant-escrow/Cargo.toml \
        --features unsafe-allow-test-attestor-pubkey \
        --sbf-out-dir "$FIXTURES_DIR" 2>&1 | tail -3
fi

export BPF_OUT_DIR="$FIXTURES_DIR"

# When `--features devnet-shrunk` is on for the .so build, the lib
# compiled by `cargo test` must use the same feature so struct sizes
# match. ario-core, ario-ant, ario-ant-escrow don't use this feature
# but it doesn't hurt to pass it (they declare it as a no-op).
TEST_FEATURES="--features devnet-shrunk"

# escrow's claim_*_attested tests need the test attestor key, which lives
# behind the opt-in `unsafe-allow-test-attestor-pubkey` feature (kept out of
# default so it never reaches a deploy artifact). Append it for escrow ONLY —
# `cargo test -p ario-core --features unsafe-allow-test-attestor-pubkey` would
# error because only escrow declares the feature.
features_for() {
    if [[ "$1" == "ario-ant-escrow" ]]; then
        echo "${TEST_FEATURES},unsafe-allow-test-attestor-pubkey"
    else
        echo "${TEST_FEATURES}"
    fi
}

if [[ "${1:-}" == "--all" ]]; then
    overall=0
    for prog in ario-core ario-ant ario-gar ario-arns ario-ant-escrow; do
        echo ""
        echo "=== $prog integration ==="
        if ! cargo test -p "$prog" --release $(features_for "$prog") --test integration 2>&1 | grep -E "test result"; then
            overall=1
        fi
    done
    exit $overall
elif [[ -n "${1:-}" ]]; then
    prog="$1"
    shift || true
    cargo test -p "$prog" --release $(features_for "$prog") --test integration "$@"
else
    echo "Usage: $0 <ario-core|ario-ant|ario-gar|ario-arns|ario-ant-escrow> [test-filter]" >&2
    echo "       $0 --all" >&2
    echo "       FAST=1 $0 ...  (skip the .so rebuild)" >&2
    exit 1
fi
