# ADR-0031: `EpochSettings.authority` Must Be Transferable

- Status: proposed
- Date: 2026-09-02
- Deciders: protocol engineering
- Related: ADR-026 (admin authority → Squads — **blocked by this**),
  ADR-0029 (`admin_close_orphaned_epoch_rent_receipt`, one of the stranded
  instructions), M3 `observer_address`, ADR-0030 (bundle candidate)

## Context and problem statement

`ario-gar` is the only program with **two** authority-bearing accounts:

| program | authority-bearing account | `transfer_authority` moves it? |
|---|---|---|
| `ario-core` | `ArioConfig` | ✅ `CONFIG_SEED` |
| `ario-arns` | `ArnsConfig` | ✅ `ARNS_CONFIG_SEED` |
| `ario-ant` | `AntMigrationConfig` | ✅ `ANT_MIGRATION_CONFIG_SEED` |
| `ario-gar` | `GatewaySettings` | ✅ `SETTINGS_SEED` |
| `ario-gar` | **`EpochSettings`** | ❌ **nothing moves it** |

`gar::transfer_authority` is hardcoded to one account:

```rust
#[account(mut, seeds = [SETTINGS_SEED], bump = settings.bump,
          has_one = authority @ GarError::Unauthorized)]
pub settings: Account<'info, GatewaySettings>,
```

`EpochSettings.authority` is assigned once, in `initialize_epochs`:

```rust
settings.authority = params.authority;
```

and **no instruction writes it again**. It is immutable for the life of the
deployment.

### Why this blocks ADR-026

Seven instructions gate on `EpochSettings.authority`:

```
set_epochs_enabled            admin_set_current_epoch_index
admin_set_epoch_duration      close_epoch_settings
admin_set_reward_ratios       admin_close_stale_epoch
                              admin_close_orphaned_epoch_rent_receipt
```

Running the ADR-026 handoff today moves `GatewaySettings` to the Squads vault and
leaves all seven **permanently on the hot key**. The handoff would appear to
succeed while silently failing to transfer half of gar's admin surface — and the
half it strands includes the two recovery instructions
(`admin_close_stale_epoch`, `admin_close_orphaned_epoch_rent_receipt`) that were
used for real during the ADR-0029 rollout to recover stranded epochs.

### This is already visible on staging

Staging went through the handoff. The split is live:

```
GatewaySettings  authority = 4sBzyU2P…   <- Squads vault
EpochSettings    authority = FHgQn4W9…   <- still the deploy key
```

It was mistaken for a deliberate separation of duties — "epoch admin needs no
multisig, only program upgrades do" — and was even *convenient* during ADR-0029
validation, because `admin_set_epoch_duration` could be called without a
ceremony. It is not a design choice. It is the absence of a transfer path, and
mainnet would reproduce it exactly.

## Decision drivers

- ADR-026 must move **all** admin authority, or it does not achieve its purpose.
- A partial handoff is worse than none: it looks complete while leaving a hot key
  load-bearing, so nobody goes looking for the remainder.
- No destructive migration. `close_epoch_settings` + re-`initialize_epochs`
  technically re-seeds the authority but resets `current_epoch_index` to 0 and
  orphans every live `Epoch` PDA — unusable on a running network.
- The fix should be uninteresting. This is a missing setter, not a redesign.

## Considered options

1. **Add `transfer_epoch_settings_authority`** (chosen) — a second instruction
   mirroring the existing one, gated on the current `EpochSettings.authority`.
2. **Extend `transfer_authority` to take both accounts and move them together.**
   Rejected: changes an existing instruction's account list, breaking every
   client that already calls it, for no gain — the two authorities may
   legitimately differ (an operations multisig for epoch tuning, a colder one for
   staking parameters).
3. **Make `EpochSettings` read its authority from `GatewaySettings`.** Rejected:
   couples two accounts, requires passing `GatewaySettings` into all seven
   instructions, and forecloses ever separating them.
4. **`close_epoch_settings` + re-init.** Rejected as above — destructive on a
   live network.
5. **Do nothing; accept the split.** Rejected: leaves a hot key permanently
   authoritative over epoch cadence, reward ratios and the epoch recovery path,
   which is precisely what ADR-026 exists to end.

## Decision

**Add `transfer_epoch_settings_authority(new_authority: Pubkey)` to `ario-gar`,**
mirroring `transfer_authority` exactly:

```rust
#[derive(Accounts)]
pub struct TransferEpochSettingsAuthority<'info> {
    #[account(mut, seeds = [EPOCH_SETTINGS_SEED], bump = epoch_settings.bump,
              has_one = authority @ GarError::Unauthorized)]
    pub epoch_settings: Account<'info, EpochSettings>,
    pub authority: Signer<'info>,
}
```

Same semantics as the existing setter: single-step, gated on the current
authority, **rejects `Pubkey::default()`**, emits an `AuthorityTransferred`-style
event (ADR-018).

Naming it distinctly rather than overloading `transfer_authority` keeps both
instructions' account lists byte-stable, so existing clients are unaffected.

## Consequences

### Positive

- ADR-026 can actually complete. All five programs and all five
  authority-bearing accounts become transferable.
- Staging's split becomes fixable rather than permanent.
- The two authorities may still be set independently, which is a feature: epoch
  tuning and staking parameters can sit behind different signer sets.

### Negative / risks

- One more instruction in an already large program.
- **Ordering hazard during the handoff.** Transferring `EpochSettings.authority`
  to the vault means every subsequent epoch admin action needs a multisig
  ceremony — including `admin_set_epoch_duration` and the two recovery
  instructions. Do it *last*, and confirm the vault can execute before relying
  on it.
- Anyone who read staging's split as intentional may have built around it.

### Neutral

- Purely additive: a new instruction, no layout change, no existing account list
  touched. Un-upgraded clients are unaffected.
- `EpochSettings` gains no field, so no `realloc` and no schema migration.

## Implementation notes

- Mirror `initialize.rs::transfer_authority` line for line, including the
  null-pubkey guard.
- Test matrix: current authority succeeds; a non-authority is rejected
  `Unauthorized`; `Pubkey::default()` is rejected; after transfer the old
  authority is rejected and the new one succeeds on a representative gated
  instruction (`admin_set_epoch_duration`).
- **Fix staging too** once deployed — it is the reference environment for the
  mainnet ceremony, and leaving it split means the rehearsal does not match the
  real thing.
- Bundle with ADR-0030 (`operations_address`). Both are `ario-gar`, and the
  ADR-0029 rollout showed a mainnet gar upgrade costs a full cycle: `extend`,
  buffer, byte-verify, deploy, verify. One cycle is enough for both.

## Audit note

Every other authority-bearing account was checked and **is** transferable —
`ArioConfig`, `ArnsConfig`, `AntMigrationConfig` and `GatewaySettings` each have a
working `transfer_authority`. `EpochSettings` is the only gap. `ario-ant-escrow`
carries no admin authority at all and is out of scope (unused since the
centralized claim pivot).
