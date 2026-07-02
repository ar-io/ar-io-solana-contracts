# ADR-027: Restore the Escrow Active-Vault Re-lock via Direct CPI into ario-core's Existing ABI

- Status: accepted
- Date: 2026-07-01
- Deciders: protocol engineering
- Related: ADR-014 (trustless escrow), ADR-017 (off-chain attestor), ADR-021
  (escrow vault re-locks are non-revocable — preserved by this design),
  ADR-022 (disabled the introspection-based re-lock — its *disable decision*
  is superseded by this ADR; its security analysis stands), BD-107, BD-113

## Context

ADR-022 removed the escrow's active-vault re-lock path: the old design
released still-locked tokens to a wallet and merely *introspected* the
transaction for a sibling `ario_core::vaulted_transfer`, with no 1:1
binding between a claim and the re-lock it credited — one sibling could
satisfy N claims (lock bypass / relayer skim). Since then, a still-locked
vault claim has been rejected (`VaultStillLocked`) and vaults were
claimable liquid only after `vault_end_timestamp`.

The product need has changed: the escrow program is intended to be
**temporary**, so forcing users to come back after their vault expires is
undesirable. Every vault escrow should be claimable at any moment, with
still-locked positions carried into **native ario-core vaults** that
preserve the original unlock time.

ADR-022's "direct CPI" rejected-alternative (and the committed playbook,
`RESTORE_ACTIVE_VAULT_RELOCK.md`, now archived) assumed a **new ario-core
instruction** (`vaulted_transfer_for_escrow`) splitting the rent payer
from the token sender, because `vaulted_transfer` hardcodes
`payer = sender` on its `init`s and the escrow PDA (a program-owned data
account) cannot fund rent through the system program.

New hard constraint: **ario-core must not be modified or upgraded.** The
restoration must work against ario-core's existing, deployed ABI.

## Decision

**Restore the active path via a direct CPI into ario-core's existing
`vaulted_transfer` / `create_vault` instructions, using an atomic payer
pass-through inside the claim handler.** Settlement of a verified vault
claim (`remaining = escrow.vault_end_timestamp - now`) branches three
ways (shared helper `claim_vault_common::settle_vault_claim`, used by
both `claim_vault_arweave_attested` and `claim_vault_ethereum`):

1. **`remaining >= ArioConfig.min_vault_duration`** (read live from
   ario-core's config at claim time — never hardcoded): **re-lock**.
   - (a) escrow-PDA-signed SPL transfer of exactly `amount` from
     `escrow_token_account` to `payer_token_account`;
   - (b) same-instruction CPI `ario_core::vaulted_transfer(amount,
     remaining, revocable = false)` with `sender = payer` (the tx
     signer's privilege propagates through the CPI) and
     `recipient = claimant`. When `payer == claimant`
     (`vaulted_transfer` rejects sender == recipient with
     `SelfTransfer`), the handler CPIs `create_vault(amount, remaining)`
     instead — its self-owner twin with identical amount/duration checks
     and structurally non-revocable output.
   The new vault is owned by the claimant and unlocks at **exactly** the
   escrow's original `vault_end_timestamp` (ario-core recomputes
   `now + remaining` from the same clock).
2. **`0 < remaining < min_vault_duration`**: **liquid fallback** — direct
   SPL transfer to the claimant. ario-core would reject the re-lock
   (`LockDurationTooShort`); rejecting the claim instead would strand
   users in a dead zone and force a return visit, which the product
   explicitly wants to avoid.
3. **`remaining <= 0`** (expired): liquid — byte-for-byte the ADR-022-era
   path.

The re-lock account set rides as **six trailing `Option<...>` accounts**
on both claim instructions (`payer_token_account`, `ario_core_config`,
`recipient_vault_counter`, `vault`, `vault_token_account`,
`ario_core_program` pinned `address = ario_core::ID`). **Expired** claims
omit them entirely, so the pre-existing claim ABI keeps working
unchanged. Any **still-locked** claim must carry them — even one that
settles liquid via the sub-minimum fallback, since the handler reads
`ario_core_config.min_vault_duration` to decide the branch. A
still-locked claim submitted without them fails with the new appended
error `RelockAccountsMissing` before any token movement.
`VaultStillLocked` becomes unreachable but is retained (append-only error
ABI).

### Why the pass-through is safe where the introspection wasn't

ADR-022's vulnerability was **structural to introspection**: nothing tied
a sibling `vaulted_transfer` to a specific claim, and nothing marked it
consumed. Here there is no sibling:

- **1:1 by construction** — each claim performs its *own* CPI; the credit
  (escrow → payer ATA) and the debit (payer ATA → vault, exact same
  `amount`) happen inside one instruction. N claims necessarily produce N
  vaults (pinned by `test_n_claims_produce_n_vaults`).
- **Atomic** — if the CPI fails for any reason, the pass-through transfer
  reverts with it; the payer never ends the instruction holding
  unencumbered escrow funds (pinned by the wrong-vault-PDA test and the
  net-zero payer assertions).
- **Non-revocable preserved (ADR-021)** — `revocable` is hardwired
  `false` at the CPI call site; the `create_vault` branch cannot produce
  a revocable vault at all. The claimant is bound into the vault's
  `owner`/`recipient` by the same attested canonical message that
  authorizes the claim.

## Rejected alternatives

- **New ario-core instruction** (the archived playbook's design) —
  cleaner (escrow PDA as token source, no pass-through), but violates the
  do-not-touch-ario-core constraint.
- **Reject claims when `remaining < min_vault_duration`** — most faithful
  to the original lock, but re-creates the "come back later" dead zone
  for a temporary program.
- **Round sub-minimum remainders up to `min_vault_duration`** — preserves
  "never unlocks early" but silently over-locks claimants by up to ~14
  days they never agreed to.

## Consequences

- **Deliberate, bounded early-liquidity window:** a vault escrow becomes
  liquid-claimable up to `min_vault_duration` (default 14 days) before
  its nominal `vault_end_timestamp` — the depositor's effective lock is
  `vault_end_timestamp - min_vault_duration`. Because deposits already
  require a ≥14-day lock, the shortest-lived vaults are liquid-claimable
  as soon as a claim attestation exists. Accepted intentionally
  (claiming is never worse than waiting; nobody must return).
  Documented as BD-113.
- `min_vault_duration` is **admin-mutable** on ario-core's config; the
  fallback window tracks the live value. A config change between claim
  preparation and landing can flip a prepared re-lock into the liquid
  branch — the claim still succeeds.
- `remaining > max_vault_duration` fails inside the CPI
  (`LockDurationTooLong`) until the remainder decays under the cap.
  Self-healing; a non-issue at the 200-year default. Documented, not
  guarded.
- **Caller contract (SDK):** for a still-locked claim, read the
  claimant's `VaultCounter.next_id` (0 if absent), derive
  `vault = [VAULT_SEED, claimant, next_id_le]`, pre-create the vault's
  token account (owner = the not-yet-initialized vault PDA) in an earlier
  ix of the same tx, and pass the six accounts. If the claimant gains
  another vault between read and land, ario-core fails `ConstraintSeeds`
  and the claim reverts atomically — re-derive and retry. Recommend a
  400k `SetComputeUnitLimit` on re-lock claims.
- **Supply accounting** moves `amount` from circulating to locked at
  claim time (inside `vaulted_transfer`/`create_vault`), matching what
  the pre-ADR-022 sibling did.
- **Cross-repo lockstep** (reverse of `ACTIVE_VAULT_DISABLE_ROLLOUT.md`):
  contract release republishes `@ar.io/solana-contracts` with the
  extended (backward-compatible) claim ABI → `ar-io-sdk` restores active
  claim construction and drops the "locked until `vault_end`" guard →
  `ar-io-solana-escrow-app` restores "claim early, stay locked" UX
  (showing the projected unlock date, and that near-expiry claims deliver
  liquid) → `solana-ar-io`'s claim runner extends Phase A to exercise the
  active happy path. Downstreams currently *expect* `VaultStillLocked`
  on active claims and must update.
- No event ABI changes: `EscrowClaimedEvent` is reused unchanged;
  re-lock claims additionally surface ario-core's existing
  `VaultCreatedEvent`.
