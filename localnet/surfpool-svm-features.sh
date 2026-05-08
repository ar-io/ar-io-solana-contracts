#!/usr/bin/env bash
# Surfpool SVM feature gates — shared policy for repo bring-up scripts.
#
# Surfpool ships `surfpool start --disable-feature <token>` where `<token>` is
# resolved only via `lookup_feature_by_name()` in Surfpool’s `features.rs`
# (kebab / snake identifiers hard‑coded there), not arbitrary `solana feature
# status` base58 ids.
#
# Compared to mainnet‑beta **`solana feature status --output json`** inactive
# rows (Surfpool v1.1.2 toolchain, `agave-feature-set ~3.1`):
#
# - **Seven** inactive rows match a Surfpool CLI token literally (resolved
#   pubkey ∩ RPC inactive): `remaining-compute-units-syscall-enabled`,
#   `increase-tx-account-lock-limit`, `enable-big-mod-exp-syscall`,
#   `blake3-syscall-enabled`, `deprecate-legacy-vote-ixs`,
#   `disable-sbpf-v0-execution`, `reenable-sbpf-v0-execution`.
# - **`stake-raise-minimum-delegation-to-1-sol`** appears in upstream Surfpool
#   source but may hit “Unknown SVM feature” on some binaries (agave pin drift).
#
# Seventeen+ other inactive rows (loader‑v2 superfeature, turbine experiments,
# partitioned rewards, ZK token gates, libsecp256k1 count check, fee‑size row,
# etc.) have **no** symbol in Surfpool 1.1.2’s lookup table — they cannot be
# toggled from the CLI until Surfpool adds aliases.
#
# **Legacy harness trio** — still accepted by Surfpool and kept here because
# we previously shipped them for “mainnet‑ish” stress; note the **pinned**
# pubkey for `enable-get-epoch-stake-syscall` / `enable-loader-v4` /
# `account-data-direct-mapping` may no longer match the inactive **row** your
# `solana` CLI prints (feature ids move across agave releases even when the
# string stays stable).
#
# Opt-in / opt-out:
#   SURFPOOL_ENABLE_ALL_SVM_FEATURES=1  — `--features-all` (explicit disables
#                                           below still deactivate each token)
#   SURFPOOL_SKIP_MAINNET_INACTIVE_DISABLES=1 — skip this symbolic disable list
#
# This file is meant to be `source`'d by bash scripts; it is not an entrypoint.

# Maximum practical `--disable-feature` snapshot for Surfnet vs mainnet-style stress.
ARIO_SURFPOOL_DISABLE_MAINNET_INACTIVE_FEATURES=(
  # Strict overlap: Surfpool token resolves to an id listed inactive on RPC.
  remaining-compute-units-syscall-enabled
  increase-tx-account-lock-limit
  enable-big-mod-exp-syscall
  blake3-syscall-enabled
  deprecate-legacy-vote-ixs
  disable-sbpf-v0-execution
  reenable-sbpf-v0-execution
  # Legacy harness trio (pin vs RPC inactive row may differ — see header).
  enable-get-epoch-stake-syscall
  enable-loader-v4
  account-data-direct-mapping
)

ario_surfpool_svm_extra_start_args() {
  SURFPOOL_EXTRA_START_ARGS=()
  if [[ "${SURFPOOL_ENABLE_ALL_SVM_FEATURES:-0}" == "1" ]]; then
    SURFPOOL_EXTRA_START_ARGS+=(--features-all)
  fi

  if [[ "${SURFPOOL_SKIP_MAINNET_INACTIVE_DISABLES:-0}" != "1" ]]; then
    local token
    for token in "${ARIO_SURFPOOL_DISABLE_MAINNET_INACTIVE_FEATURES[@]}"; do
      SURFPOOL_EXTRA_START_ARGS+=(--disable-feature "$token")
    done
  fi
}
