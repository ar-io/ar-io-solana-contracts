# ADR-021: Escrow Vault Re-locks Are Non-Revocable

- Status: accepted
- Date: 2026-05-27
- Deciders: protocol engineering
- Related: ADR-014 (trustless escrow), ADR-017 (off-chain attestor), BD-052 (revoke-vault matches Lua)

## Context

`ario-ant-escrow` custodies time-locked ARIO vault positions for the AO →
Solana migration. An **active** (not-yet-expired) vault claim re-locks the
funds for the claimant by requiring a sibling `ario_core::vaulted_transfer`
instruction in the same transaction (verified via `vault_introspect::
verify_vaulted_transfer_in_tx`).

A security review (Codex) found a theft vector in the **revocable** active
path:

- `ario_core::vaulted_transfer` sets `vault.controller = sender` when
  `revocable == true`, and a `revocable` vault can be `revoke_vault`'d by its
  controller **while still locked** (BD-052), sending the balance to the
  controller.
- The sibling `vaulted_transfer`'s `sender` is the **claim-transaction payer**
  — attacker-choosable (the escrow UI explicitly allows any fee payer).
- The claim attestation binds claimant / amount / asset-id / nonce but **not**
  the controller, and `vaulted_transfer` forbids `sender == recipient`, so the
  controller can **never** be the claimant.

⇒ an untrusted payer/relayer submitting an active *revocable* vault claim
becomes the vault's revocation controller and can `revoke_vault` to steal the
re-locked funds before expiry.

Two structural facts make this unfixable *within the revocable model*:

1. **`EscrowToken` has no field for a legitimate revoker** — only `depositor`,
   `recipient`, `vault_revocable`, `vault_end_timestamp`. There is nothing to
   bind the controller to.
2. **The migration importer already deposits every vault as non-revocable**
   (`solana-ar-io` `batch-escrow.ts` → `buildDepositVaultIx(..., revocable=false)`,
   *"migration recovery vaults are non-revocable"*). So no revocable vault is
   actually migrated; the `revocable=true` escrow path was only ever reachable
   as the exploit (and in smoke tests).

## Decision

**The escrow neither accepts nor produces revocable vaults.**

- `deposit_vault` rejects `revocable == true` with `RevocableVaultUnsupported`
  (the unhonorable flag can't be stored). `EscrowToken.vault_revocable` is
  therefore always `false` (kept for layout/ABI stability).
- `verify_vaulted_transfer_in_tx` accepts only **non-revocable** re-locks
  (dropped its `expected_revocable` parameter; a revocable-but-otherwise-matching
  sibling returns `RevocableVaultUnsupported`). Applies to both
  `claim_vault_arweave_attested` and `claim_vault_ethereum`.

The claimant still owns the re-locked vault and withdraws via
`ario_core::release_vault` at expiry. The relayer/gasless model is preserved
(any payer can fund a non-revocable re-lock; `controller = None`, so the payer
gains nothing).

**Scope:** escrow-only. `ario_core::vaulted_transfer(revocable=true)` and
`revoke_vault` remain valid primitives for direct, non-escrow use; the escrow
simply stops being a producer/consumer of revocable vaults. BD-052 is unchanged.

## Rejected alternatives

- **Bind the original AO grantor as controller (faithful preservation).**
  Would require capturing the grantor in `EscrowToken`, the attestor signing it,
  and — because the grantor is an Arweave identity — a *second* Arweave→Solana
  attestation so that party can act as the Solana controller. Disproportionate,
  and moot since the importer migrates everything non-revocable.
- **Make the AR.IO authority wallet the universal revoker.** Either (way 1) the
  authority must co-sign every active revocable claim and be the funding
  intermediate (hot key on routine claims; kills the relayer model), or (way 2)
  change `vaulted_transfer` to take a `controller` distinct from `sender`
  (ario-core API change, diverges from BD-052) — both introduce a new
  "protocol can claw back any migrated vault" trust assumption that isn't the
  AO semantic, for a path the migration never uses.

These remain the design starting points **if** escrow-mediated revocable vaults
are ever wanted as a deliberate product feature (not a migration concern).

## Consequences

- The Codex theft vector is eliminated by construction (no controller exists on
  a re-locked vault). Regression tests: `deposit_vault` rejects revocable;
  active claim with a revocable sibling → `RevocableVaultUnsupported`; both
  active happy paths re-lock non-revocably and the claimant releases at expiry.
- AO revocability is not carried to Solana — consistent with the importer's
  existing behavior; documented as BD-105.
- Sister-repo follow-ups (`solana-ar-io`): smoke tests that deposit
  `revocable=true` must flip to `false`; the escrow web app should build
  `revocable=false` re-locks (on-chain already enforces; `sender=payer` is now
  harmless).
