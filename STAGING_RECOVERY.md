# `staging-recovery` branch — DO NOT MERGE to `develop`

This branch carries one purpose: ship a one-time, multi-sig-gated
`admin_post_finalize_repair_*` ix surface to **staging-v1 only**, so the
~1,000 silent batched-import gaps surfaced by verify on 2026-05-24 can
be filled. After the repair is done, staging-v1 will be re-locked by
re-deploying the default-features `develop` build over the recovery
build.

The recovery ixs **must never ship to mainnet.**

## What this branch adds (delta from `develop`)

- `programs/ario-core/src/recovery.rs` — generic `admin_post_finalize_repair_account` + `admin_post_finalize_repair_balance` (with the SPL treasury → recipient ATA transfer)
- `programs/ario-gar/src/recovery.rs` — `admin_post_finalize_repair_account`
- `programs/ario-arns/src/recovery.rs` — `admin_post_finalize_repair_account`
- `programs/ario-ant/src/recovery.rs` — `admin_post_finalize_repair_account` (mirrors `validate_ant_account_borsh` from the import path)
- `recovery = []` feature in each program's `Cargo.toml`
- Each program's `lib.rs` adds `#[cfg(feature = "recovery")] pub mod recovery;`, `#[cfg(feature = "recovery")] pub use recovery::*;`, and `#[cfg(feature = "recovery")]` dispatch arms inside the `#[program]` block
- `AccountAlreadyExists` error variant on ario-gar / ario-arns / ario-ant (ario-core already had it). Always-compiled (Anchor's `#[error_code]` macro rejects extra attributes on variants) but only raised by the recovery ixs
- `pub fn validate_ant_account_borsh` made public in `programs/ario-ant/src/migration.rs` so the recovery module can reuse it
- `.github/workflows/recovery-feature-guard.yml` — CI guard that fails any `develop` / release build that compiles a `recovery.rs` file or lists `recovery` in a default feature set, and any PR to `develop` whose diff touches `recovery.rs`

The recovery ixs are:

- **Multi-sig-gated** (`config.authority` / `settings.authority` /
  `migration_config.authority`), NOT migration_authority — the
  migration hot key is considered exposed by the long import run.
- **NOT `migration_active`-gated** — that's the whole point.
- **Fail-if-exists.** They explicitly `require!(data_is_empty, …)` so a
  stale gap list can't silently overwrite working state.

## Lifecycle

1. **Build** with the feature on (from this branch):
   ```bash
   git checkout staging-recovery
   anchor build -- --features recovery
   ```
   IDLs land in `target/idl/`; `.so`s in `target/deploy/`.

2. **Deploy upgrade** on staging-v1's 4 program IDs via BPFLoaderUpgradeable
   (multi-sig signs each):
   ```bash
   solana program deploy \
     --program-id <staging-v1 ario-core ID> \
     --upgrade-authority <multi-sig> \
     target/deploy/ario_core.so
   # …repeat for ario-gar, ario-arns, ario-ant
   ```

3. **Repair**: the monorepo's `migration/import/src/repair.ts` consumes
   the verify-derived `missing-accounts.json` and dispatches each entry
   to the corresponding `admin_post_finalize_repair_*` ix.

4. **Re-verify** until 100% clean.

5. **Re-lock**: rebuild from `develop` without `--features recovery`
   and `solana program deploy` again at the same program IDs. The
   deployed `.so` no longer contains the repair ixs — staging-v1 ends
   matching the exact mainnet binary.

## Why the gates are belt-and-braces

The cost of "oops, recovery shipped to mainnet" is that anyone with
multi-sig access can write arbitrary state into any PDA forever. That
contradicts ADR-015's "finalize is final" guarantee and undermines the
decentralization story.

Defenses in layers:

1. **Branch isolation.** Code lives on `staging-recovery`, not on
   `develop`. Never merged.
2. **Feature gate.** `#[cfg(feature = "recovery")]` on the module,
   the `pub use`, the dispatch arms. Default build literally doesn't
   contain the discriminator.
3. **CI guard.** `recovery-feature-guard.yml`:
   - any default-features build compiling `recovery.rs` → fail
   - any `default = [..., "recovery", ...]` in a program Cargo.toml → fail
   - any PR to `develop` that diffs `recovery.rs` → fail
4. **Tagged release.** When deploying for the recovery run, tag as
   `vX.Y.Z-staging-recovery` so the on-chain binary's provenance is
   traceable.

## When to delete this branch

After staging-v1 is re-locked (step 5 above) AND verify confirms the
deployed binary's IDL no longer contains `admin_post_finalize_repair_*`.
Until then, the branch stays put as the audit trail.
