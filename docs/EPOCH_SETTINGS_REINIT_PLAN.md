# Epoch Settings Reinit + Fast-Test Devnet — Implementation Plan

**Status:** queued. **Target:** devnet (`ario-gar` program ID
`AF8QAEaR4hzsqeUDwEdeTXMYtdyFegTENBdnJro6WVLR`). **Driver:** observer/cranker
iteration is impractical with production-realistic defaults (1-day epochs,
180-day tenure ramp). Reduce to 5-min epochs / 1-hour tenure ramp for
end-to-end testing.

## Why "close + reinit"

Two pieces can't be applied by redeploying alone:

1. `tenure_weight_duration` is set inside `initialize_epochs`
   (`programs/ario-gar/src/instructions/initialize.rs:157`). Redeploying
   the BPF doesn't rewrite already-stored account state, so the existing
   PDA keeps its `180 * 86_400` value.
2. `initialize_epochs` uses `init` (not `init_if_needed`); calling it
   again on the existing PDA fails.

The on-chain EpochSettings PDA today
(`9iUPjk2kGWvUfrnPLobYrhH92Kef76L5jC3XHSKYHuaY`, owned by `ario-gar`) must
be closed and re-initialized with new params. There is no
`update_epoch_settings_params` ix and no `close_epoch_settings` ix today;
both `set_epochs_enabled` and `admin_repair_settings` only touch other
fields.

Add `close_epoch_settings`. Keeps "init params are set at init time" as
the only invariant — no permanent admin-mutation surface.

## Param targets

`InitializeEpochParams` field changes (passed at init time):

| Field                                          | Devnet test  | Mainnet target |
| ---------------------------------------------- | ------------ | -------------- |
| `epoch_duration`                               | `300` (5 min)| `86400` (1 day)|
| `observer_count` / `prescribed_observer_count` | `5`          | `50`           |
| `name_count` / `prescribed_name_count`         | `10`         | `50`           |
| `min_observer_stake`                           | `0`          | `50_000_000_000` (50k ARIO) |
| `slash_rate`                                   | leave as-is  | —              |

Source-level edit (NOT in `InitializeEpochParams`):

- `programs/ario-gar/src/instructions/initialize.rs:157`:
  `settings.tenure_weight_duration = 180 * 86_400` → `1 * 3600` (1 hour).
- `:158` (`max_tenure_weight = 4`) — leave alone.

Long-term: parameterize `tenure_weight_duration` via
`InitializeEpochParams` as a separate ticket. Don't block on it.

## Implementation

### Rust source (this repo)

Branch: `feat/close-epoch-settings` off `develop`.

- `programs/ario-gar/src/instructions/initialize.rs:157` — flip
  `tenure_weight_duration`. Update inline comment.
- `programs/ario-gar/src/instructions/epoch.rs` — new ix
  `close_epoch_settings` next to `set_epochs_enabled`. Modeled on
  `close_drained_withdrawal` (`programs/ario-gar/src/instructions/withdrawal.rs:164`):

  ```rust
  #[derive(Accounts)]
  pub struct CloseEpochSettings<'info> {
      #[account(
          mut,
          seeds = [EPOCH_SETTINGS_SEED],
          bump = epoch_settings.bump,
          has_one = authority @ GarError::Unauthorized,
          close = authority,
      )]
      pub epoch_settings: Account<'info, EpochSettings>,
      #[account(mut)]
      pub authority: Signer<'info>,
  }
  pub fn close_epoch_settings(_ctx: Context<CloseEpochSettings>) -> Result<()> {
      Ok(())
  }
  ```

- `programs/ario-gar/src/lib.rs` — register the new ix.

### Semantic call-out (review carefully)

**Not** gating `close_epoch_settings` on `!enabled`.

`set_epochs_enabled(false)` is a 7-day timelock
(`programs/ario-gar/src/instructions/epoch.rs:21-26`); the `enabled` flag
doesn't actually flip until the timelock elapses + the next `create_epoch`
runs (`epoch.rs:52-54`). A `!enabled` guard on close would force an
authority-led recovery to wait 7 days. The close is destructive (orphans
Epoch PDAs, halts cranker) regardless of when it runs, and the authority
is omnipotent. Doc comment must say:

> Authority-only. Closes the EpochSettings PDA. Intended for fresh-init /
> authority-led recovery; mid-cycle closure orphans Epoch PDAs and halts
> the cranker.

Alternative if mainnet hygiene is preferred: gate on
`disable_at > 0 && now >= disable_at` (mirrors `create_epoch`'s
effective-disabled check). Decide before merge.

### Tests (this repo)

- `programs/ario-gar/tests/integration.rs` — one new test
  `test_close_epoch_settings`: init → close → reinit roundtrip + auth
  failure case (~50 LoC).

### TypeScript / migration tooling (lives in
`ar-io/solana-ar-io` — separate repo)

Cross-repo references; not maintained here. Implementation lives at:

- `migration/import/src/devnet-setup.ts:605-615` — make all 5
  `InitializeEpochParams` fields env-overridable. **Production-realistic
  fallbacks baked in** so a future mainnet run with no envs set lands on
  `epoch_duration=86400`, `observer_count=50`, etc. Read sites:
  - `EPOCH_DURATION_SECS` (already exists)
  - `OBSERVER_COUNT` (new)
  - `NAME_COUNT` (new)
  - `MIN_OBSERVER_STAKE` (new — value in mARIO)
  - `SLASH_RATE` (new)

- `migration/import/devnet.env` — add the 5 envs with fast-test values +
  comment header pointing back at this plan.

- New script `migration/import/reinit-epoch-settings.ts`:
  1. Confirm authority signer matches on-chain `epoch_settings.authority`.
  2. CPI sequence: `close_epoch_settings` → `initialize_epochs` (in one
     tx if size allows; otherwise two).
  3. Does **not** call `set_epochs_enabled(true)` — leaves the new PDA
     disabled. Cranker work re-enables when ready.

### Codegen (lives in `ar-io/solana-ar-io/sdk/`)

After this repo's PR merges and the IDL refreshes:

```bash
cd /path/to/solana-ar-io/sdk
yarn codegen
```

Picks up the new ix automatically — no hand edits to generated clients.

### Docs (this repo)

- `docs/DEVNET_RUNBOOK.md` (or wherever the mainnet/devnet matrix is
  documented in this repo — confirm during implementation) — new "Devnet
  vs mainnet epoch-settings deltas" subsection. Includes the 5-row table
  + a callout on `tenure_weight_duration` being a source-level edit (not
  in `InitializeEpochParams`).

## Devnet ops sequence (post-merge)

Two paths — pick based on iteration speed needs:

**A. CI-driven (preferred for the merged change):**
1. Open PR `feat/close-epoch-settings` → review → merge to `develop`.
2. `release-devnet.yml` auto-builds + auto-upgrades the live `ario-gar`
   on devnet using `DEVNET_AUTHORITY_KEY_JSON`. No local action needed.
3. From `solana-ar-io/migration/import`, run:
   ```bash
   yarn tsx reinit-epoch-settings.ts
   ```
4. Verify:
   ```bash
   ./scripts/devnet-cli.sh get-epoch-settings \
     --rpc-url <RPC> \
     --gar-program-id AF8QAEaR4hzsqeUDwEdeTXMYtdyFegTENBdnJro6WVLR
   # expect: epoch_duration=300, prescribed_observer_count=5,
   #         prescribed_name_count=10, min_observer_stake=0
   ```

**B. Manual (faster iteration before PR is ready to merge):**
```bash
# This repo
anchor build
solana program deploy target/deploy/ario_gar.so \
  --program-id target/deploy/ario_gar-keypair.json \
  --keypair <devnet-authority-keypair.json> \
  --url <RPC>

# solana-ar-io repo
cd /path/to/solana-ar-io/sdk && yarn codegen
cd ../migration/import && yarn tsx reinit-epoch-settings.ts
```

CI never holds the migration tooling, so the reinit step is always run
manually regardless of which deploy path you pick.

After verification, ping observer team to flip
`ENABLE_EPOCH_CRANKING=true` in `ar-io-observer/.env` and run the cranker
continuously.

## Side notes

- The current devnet Epoch 0 PDA stays orphaned after this — that's
  fine. Lifecycle stops at `submit_observation` anyway (synthetic
  operators have no signing keys); the cranker can't close it. Reset
  will create Epoch 1 fresh under the new params.
- Existing `program-ids/devnet.json` program IDs unchanged.
- Migration authority unchanged
  (`FkkABY7WLYpET8jKdbknCArYab44M88TkjmJmut8TcRF`).

## Open questions to confirm at execution time

1. Add `close_epoch_settings` ix unconditionally vs. env-gated?
   **Default: unconditional** — already authority-gated.
2. Should reinit script also `set_epochs_enabled(true)`?
   **Default: no** — leave for cranker work.
3. Purge orphaned Epoch 0 PDA? **Default: leave** — costs ~0.002 SOL of
   stranded rent; full cluster reset is overkill.

## Related work in this repo

- The migration-tooling counterpart of this plan (devnet-setup.ts, the
  reinit script, devnet.env) lives in `ar-io/solana-ar-io` — see that
  repo's `PLAN.md` for the running checklist.
