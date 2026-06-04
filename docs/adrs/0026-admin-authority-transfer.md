# ADR-026: Single-Step `transfer_authority` for the Admin Authority Across All Programs

* **Status:** accepted
* **Date:** 2026-06-04
* **Deciders:** @vilenarios

> **TL;DR:** Add a dedicated single-step `transfer_authority(new_authority)`
> instruction to `ario-gar`, `ario-arns`, and `ario-ant` (and a matching
> wrapper on `ario-core`, which already rotates via `update_config`) so the
> admin `authority` can be rotated post-deploy — gated on the *current* admin
> authority, guarded against the null pubkey, emitting an
> `AuthorityTransferred` event. Single-step (not propose/accept) because
> two-step would force a 32-byte `pending_authority` field onto every config
> and thus a `grow-then-deserialize` migration (ADR-020 risk surface) for a
> capability whose only load-bearing check is "the current authority signs."

## Context and problem statement

Each program stores two independent authorities (see "Two authority planes"):

* `migration_authority` — a hot key for the bulk migration import. Every
  instruction it gates is also gated on `migration_active`, so
  `finalize_migration` permanently neutralizes it. It self-retires; nothing
  to rotate.
* `authority` — the **ongoing admin/governance** key. It is *not* tied to
  `migration_active`, so it survives migration close and keeps real,
  sometimes treasury-adjacent powers:
  * `ario-core`: `admin_set_gar_program` (repoints the treasury-release CPI
    target), `admin_repair_config`, vault-duration / primary-name tuning.
  * `ario-gar`: `admin_repair_settings` (can repoint `stake_token_account` /
    `protocol_token_account`, which `distribute_epoch` pays into),
    epoch/withdrawal tuning.
  * `ario-arns`: reserved-name reserve/unreserve/claim.
  * `ario-ant`: `admin_close_orphaned_ant_state` (asset-bound, low risk).

Today only `ario-core` can rotate its admin `authority` — via
`update_config`'s `new_authority: Option<Pubkey>`, which has existed since
the initial bring-up commit (`08d1a92`). `ario-gar`, `ario-arns`, and
`ario-ant` were modularized later with a piecemeal `admin_set_*` convention
and never gained a rotation path: their `authority` is written **only** in
`initialize` and is therefore **fixed for the life of the deployment**.

This is a problem for two reasons:

1. **No recovery.** A lost or compromised `gar`/`arns`/`ant` admin key is
   permanent. For `gar` (and `core`) that key is treasury-routing-capable, so
   "unrecoverable" is a material risk.
2. **No clean launch hand-off.** We want a hot key to run the heavy migration
   import and the `authority`-gated `finalize_migration`, then move admin
   governance to the Squads V3 multisig vault. Without rotation, the only
   options are "commit to the vault at init" (every `finalize_*` becomes a
   multisig transaction during launch) or "keep the hot key forever."

Mainnet upgrade authority is a Squads V3 multisig (separate concern; the
program *binary* upgrade path is `BPFLoaderUpgradeable` and needs no contract
changes). This ADR is strictly about the **admin/governance authority plane**,
not the upgrade-authority plane.

## Decision

Add a dedicated `transfer_authority(new_authority: Pubkey)` instruction to
`ario-gar`, `ario-arns`, and `ario-ant`, and a matching wrapper to
`ario-core`. Semantics (identical across programs):

```
Accounts: { <config/settings> PDA (mut, seeds, bump = self.bump,
            has_one = authority @ <Prog>Error::Unauthorized),
            authority: Signer }
Handler:
  require!(new_authority != Pubkey::default(), <Prog>Error::InvalidParameter)
  let old = cfg.authority;
  cfg.authority = new_authority;
  emit!(AuthorityTransferred { admin: old, old_authority: old,
                               new_authority, timestamp });
```

* **Single-step.** No `pending_authority`, no accept step.
* **Gated on the admin `authority`** — the same field the other `admin_*`
  instructions use — **never** `migration_authority`.
* **Null guard.** Rejects `Pubkey::default()` (the System program / all-zero)
  so a fat-fingered renounce can't brick admin into an unspendable address.
  Any other pubkey is allowed, **including off-curve PDAs** (the Squads vault
  is a PDA), so no on-curve check.
* **`ario-core` keeps `update_config.new_authority`** unchanged (no ABI
  churn); the new wrapper is an additional, equivalent path so all five
  programs expose one instruction name. The redundancy is benign — both paths
  require the current `authority` to sign and only assign the same field.
* **No schema change.** The `authority` field already exists; the instruction
  only reassigns it. No `version` bump, no realloc, no `migrate_*`.
* **`migration_authority` is intentionally NOT rotatable** — it self-retires
  at `finalize_migration`; adding rotation for it is needless surface.

### Recommended mainnet flow

1. `initialize` with `authority = a hot deployer key`,
   `migration_authority = a hot deployer key`.
2. Run the migration import; `finalize_migration` / `finalize_supply` with the
   hot key.
3. `transfer_authority(new_authority = the Squads V3 vault)` on each program
   → governance is now multisig-held; subsequent rotations require multisig
   consensus.

## Why not two-step (propose / accept)

Two-step protects against rotating to a valid-but-wrong key (the accept step
proves the new key is controllable). But it requires storing
`pending_authority: Pubkey` (32 bytes + presence). The reserved tails on these
configs (`field_1: u64`, `field_3: bool`, …) are scalar-sized — too small for
a Pubkey — and `AntMigrationConfig` has no reserved tail at all. So two-step
would force a **`grow-then-deserialize` `migrate_*` on all four configs**,
which is the single most error-prone pattern we maintain (ADR-020:
deserialize-before-realloc EOF bomb, genuine-pre-version-account test
requirement, `write_account` temp-buffer discipline). Trading the project's
highest-risk migration surface for typo-protection is a poor bargain when:

* the realistic target is the Squads vault — a known, echo-verifiable address;
* the null guard blocks the catastrophic renounce-to-unspendable case; and
* clients echo-verify the target before signing.

If a future need justifies it, two-step can be layered on later via a
multisig-executed program upgrade.

## Security analysis

The instruction adds one privileged state mutation. Threats and mitigations:

| Threat | Mitigation |
|---|---|
| **Unauthorized rotation / role seizure** | `has_one = authority` — the current admin must sign. The single load-bearing check; covered by a negative test. |
| **Privilege confusion** (hot, retiring `migration_authority` grabbing governance) | Gate references the admin `authority` field only, never `migration_authority`. Negative test asserts the migration key is rejected. |
| **Renounce-to-unspendable (brick)** | `require new_authority != Pubkey::default()`; retrofit the same guard onto `core::update_config`. Client echo-verification. |
| **Schema/layout corruption** | Single-step ⇒ no layout change ⇒ ADR-020 surface not touched. Test asserts every sibling config field is byte-identical after rotation. |
| **Reentrancy / CPI abuse** | Pure state write, no CPI. |
| **Event-ABI break** | `AuthorityTransferred` is a *new, additive* event (ADR-018 compliant); `idl-event-snapshots.json` updated in the same change. |
| **Stale-authority replay** | After rotation the old key fails every admin call (negative test). |

**Accepted, inherent trade-off (documented, not closed):** rotation lets
whoever holds the key today hand it away, so a **compromised single-signature
authority** could rotate it to an attacker and lock out the legitimate owner.
Two-step does not close this (the attacker accepts too). It is acceptable
because the entire purpose is to move `authority` to the **2-of-N Squads
vault**: once there, abusing rotation requires multisig consensus — the same
bar as any other admin action. The exposure window is only while `authority`
is a hot key during migration, and in that window the hot key already holds
treasury-routing powers, so rotation is not the weakest link. Net: rotation is
strongly positive for a multisig-held authority and marginally double-edged
for a hot key.

## Consequences

**Positive**

* Recoverable admin keys on all five programs; uniform `transfer_authority`
  for SDK/tooling.
* Clean launch: hot key through migration, then a single rotation to the vault.

**Negative / costs**

* One new privileged instruction per program to audit; `ario-core` gains a
  second (redundant) rotation path.
* New event ⇒ snapshot bump (two-commit dance) + `clients/ts` regen + a
  downstream `ar-io-sdk` surface.
* The compromised-hot-key trade-off above.

**Downstream**

* IDL regen; `idl-event-snapshot.mjs --update`; `clients/ts` `yarn codegen`.
* `docs/EVENTS.md` + BD-103 event surface; `ar-io-sdk` adds `transferAuthority`.
* Land before mainnet genesis so it ships in the initial binary (no upgrade
  needed); on staging it is a normal deployer-signed upgrade.
* In scope for the next internal/independent audit pass.

## Related

* ADR-018 — Anchor `#[event]` ABI policy (append-only).
* ADR-020 — schema-migration grow-then-deserialize (the surface two-step would
  have forced).
* Squads V3 upgrade-authority handoff — the *binary* plane; orthogonal to this
  ADR's admin plane.
