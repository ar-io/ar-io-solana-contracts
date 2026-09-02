# ADR-0031: Transferable `EpochSettings.authority`

* **Status:** proposed
* **Date:** 2026-09-02
* **Deciders:** protocol engineering

> **TL;DR:** `ario-gar` has two authority-bearing accounts but only one
> `transfer_authority`, leaving `EpochSettings.authority` immutable and stranding
> seven epoch admin instructions on the deploy key — so ADR-026 cannot actually
> complete until a second transfer instruction exists.

## Context and problem statement

`ario-gar` is the only program with **two** authority-bearing accounts. Every
other program has one, and each has a working transfer path:

| program | authority-bearing account | moved by `transfer_authority`? |
|---|---|---|
| `ario-core` | `ArioConfig` | yes — `CONFIG_SEED` |
| `ario-arns` | `ArnsConfig` | yes — `ARNS_CONFIG_SEED` |
| `ario-ant` | `AntMigrationConfig` | yes — `ANT_MIGRATION_CONFIG_SEED` |
| `ario-gar` | `GatewaySettings` | yes — `SETTINGS_SEED` |
| `ario-gar` | **`EpochSettings`** | **no — nothing moves it** |

`gar::transfer_authority` is hardcoded to one account
(`instructions/initialize.rs`):

```rust
#[account(mut, seeds = [SETTINGS_SEED], bump = settings.bump,
          has_one = authority @ GarError::Unauthorized)]
pub settings: Account<'info, GatewaySettings>,
```

`EpochSettings.authority` is assigned once, in `initialize_epochs`
(`instructions/initialize.rs`):

```rust
settings.authority = params.authority;
```

and no instruction writes it again. It is immutable for the life of a
deployment.

Seven instructions gate on it: `set_epochs_enabled`,
`admin_set_epoch_duration`, `admin_set_reward_ratios`,
`admin_set_current_epoch_index`, `close_epoch_settings`,
`admin_close_stale_epoch`, `admin_close_orphaned_epoch_rent_receipt`.

Running the ADR-026 handoff today therefore moves `GatewaySettings` to the
multisig and leaves all seven **permanently on the deploy key** — including the
two recovery instructions used during the ADR-0029 rollout to reclaim stranded
epochs. The handoff would appear to succeed while transferring half of gar's
admin surface.

**This is already observable.** Staging went through the handoff and came out
split: `GatewaySettings.authority` is the Squads vault
(`4sBzyU2P14jhvit6ckjqAzy1VB5kymtsSqh2rQsjMPSv`), `EpochSettings.authority` is
still the deploy key (`FHgQn4W9oFUR9GNzq4yprpvkPdNipVbFnmxEFfknxFMy`). The split
was initially read as a deliberate separation of duties — and was *convenient*
during ADR-0029 validation, since `admin_set_epoch_duration` needed no ceremony.
It is not a design choice. It is the absence of a transfer path, and mainnet
would reproduce it exactly.

**Assumption worth flagging:** this ADR assumes the two authorities *should* be
independently settable. If the project later decides gar must have exactly one
admin authority, this decision should be reopened rather than extended.

## Decision drivers

* ADR-026 must move **all** admin authority, or it does not achieve its purpose.
* A partial handoff is worse than none — it looks complete, so nobody goes
  looking for the remainder.
* No destructive migration on a running network.
* The fix should be uninteresting: this is a missing setter, not a redesign.
* Existing clients calling `transfer_authority` must keep working.

## Considered options

1. **Add a separate `transfer_epoch_settings_authority`** — a second instruction
   mirroring the existing one.
2. **Extend `transfer_authority` to move both accounts** in one call.
3. **Have `EpochSettings` read its authority from `GatewaySettings`** rather than
   storing its own.
4. **`close_epoch_settings` + re-`initialize_epochs`** to re-seed the authority.
5. **Do nothing** — accept the split.

## Decision

> Add `transfer_epoch_settings_authority(new_authority: Pubkey)` to `ario-gar`,
> gated on the current `EpochSettings.authority`, mirroring `transfer_authority`
> in every respect.

```rust
#[derive(Accounts)]
pub struct TransferEpochSettingsAuthority<'info> {
    #[account(mut, seeds = [EPOCH_SETTINGS_SEED], bump = epoch_settings.bump,
              has_one = authority @ GarError::Unauthorized)]
    pub epoch_settings: Account<'info, EpochSettings>,
    pub authority: Signer<'info>,
}
```

Single-step, rejects `Pubkey::default()`, emits a dedicated
`EpochSettingsAuthorityTransferredEvent` (ADR-018) so subscribers can tell which
of gar's two authorities moved.

**Option 2 was rejected** because it changes an existing instruction's account
list, breaking every client already calling `transfer_authority` — and it buys
nothing, since the two authorities may legitimately differ (an operations
multisig for epoch tuning, a colder one for staking parameters). Keeping them
separate satisfies the "existing clients keep working" driver outright.

**Option 3** couples two accounts, forces `GatewaySettings` into all seven
instruction contexts, and forecloses ever separating them.

**Option 4** technically re-seeds the authority but resets
`current_epoch_index` to 0 and orphans every live `Epoch` PDA — unusable on a
running network, and it violates the no-destructive-migration driver.

**Option 5** leaves a hot key permanently authoritative over epoch cadence,
reward ratios and epoch recovery, which is precisely what ADR-026 exists to end.

This decision is reversible: if gar later collapses to a single admin authority,
the instruction becomes dead code and can be removed in a subsequent ADR.

## Consequences

### Positive

* ADR-026 can complete. All five programs and all five authority-bearing
  accounts become transferable.
* Staging's split becomes fixable rather than permanent.
* The two authorities remain independently settable, so epoch tuning and staking
  parameters can sit behind different signer sets.

### Negative / risks

* One more instruction in an already large program.
* **Ordering hazard during the handoff.** Once `EpochSettings.authority` is on
  the vault, every epoch admin action needs a ceremony — including
  `admin_set_epoch_duration` and both recovery instructions. Transfer it
  **last**, and confirm the vault can execute before relying on it.
* Anyone who read staging's split as intentional may have built around it.

### Neutral

* Purely additive: a new instruction, no layout change, no `realloc`, no schema
  migration, no existing account list touched. Un-upgraded clients are
  unaffected.
* `EpochSettings` gains no field, so account sizes are unchanged.

## Implementation notes

* Mirror `initialize.rs::transfer_authority` line for line, including the
  null-pubkey guard.
* Test matrix: current authority succeeds; non-authority rejected
  `Unauthorized`; `Pubkey::default()` rejected; `GatewaySettings.authority` does
  **not** move when `EpochSettings` rotates; after transfer the old authority is
  rejected and the new one can drive a gated instruction
  (`admin_set_epoch_duration`) — proving the field changed is not the same as
  proving the gate follows it.
* **Fix staging's split** once deployed. It is the reference environment for the
  mainnet ceremony; leaving it split means the rehearsal does not match.
* **Ship separately from ADR-0030, deployed before it.** Independent changes
  with very different risk: this is additive with no migration, while ADR-0030
  grows `Gateway` by 32 bytes and migrates 646 live accounts. Bundling would gate
  a zero-migration fix behind a migration.
* Downstream: `@ar.io/sdk` needs a `transferEpochSettingsAuthority` method; the
  typed client picks the instruction up automatically from the IDL.
  `ar-io-network-portal` only if it surfaces admin authority.

## Related

* Code: `programs/ario-gar/src/instructions/initialize.rs`,
  `programs/ario-gar/src/instructions/epoch.rs`
* ADR: [ADR-026](0026-admin-authority-transfer.md) — blocked by this
* ADR: [ADR-0029](0029-epoch-rent-refunds-creator.md) — added
  `admin_close_orphaned_epoch_rent_receipt`, one of the stranded instructions
* ADR: [ADR-0030](0030-gateway-operations-address.md) — ships after this
