# ADR-0038: A Departing Operator's Unlock Schedule Is Set by the Vault, Not by the Exit Path

* **Status:** accepted
* **Date:** 2026-09-17
* **Deciders:** @vilenarios
* **Consulted:** network-portal operator report (a pruned operator could not claim
  after 30 days); `ar-io-network-process` `gar.lua` / `constants.lua` at
  `d111500`; on-chain audit of every mainnet exit vault
* **Informed:** gateway operators, Network Portal, SDK consumers

> **TL;DR:** An operator leaving the network gets two vaults, and each one's lock
> belongs to the vault, not to the reason for leaving: the protected minimum-stake
> vault always unlocks after `GATEWAY_LEAVE_PERIOD` (90 days), and the excess
> vault always unlocks after `GatewaySettings.withdrawal_period` (30 days by
> default). `leave_network` and `prune_gateway` must be identical in this respect,
> and were not — prune locked the excess for 90 days too (fixed with this ADR;
> BD-102). The 100% slash of the minimum stake on prune, the 90-day protected
> lock, and prune's re-protection of a second minimum-stake slice are all
> **unchanged and accepted as they stand**. The 30-day excess lock is a deliberate
> divergence from Lua's 90 days.

## Context and problem statement

An operator can stop being a gateway in two ways:

* `leave_network` — voluntary;
* `prune_gateway` — permissionless, after
  `max_consecutive_failures` (30) failed epochs, which first slashes 100% of
  `min_operator_stake` to the protocol.

Both then split whatever stake remains into the same two `Withdrawal` PDAs:

| vault | holds | `is_protected` | expedite-able |
|---|---|---|---|
| protected exit vault | `min(min_operator_stake, remaining)` | `true` | no — `instant_withdrawal` rejects with `ProtectedVault` |
| excess vault | `remaining − min_operator_stake` | `false` | yes, with the decaying penalty |

Each vault stores its own `available_at`, written once at exit. Nothing on chain
recomputes it, so whatever is written is what the operator lives with.

### What went wrong

`leave_network` writes two different unlock times, and its doc comment says so.
`prune_gateway` wrote one — `GATEWAY_LEAVE_PERIOD` — into **both** vaults. A
pruned operator's above-minimum stake was therefore locked for 90 days where a
voluntary leaver's is locked for 30, with no comment, test or ADR asserting that
difference. It came from the 2026-05-18 change (BD-102) that introduced the
split lock: it updated `leave_network` and `claim_delegate_from_leaving_gateway`
and missed the prune path.

Nothing pinned it, either: before this ADR no test asserted the `available_at`
of either vault on either path.

### Lua is 90/90, and BD-102 said otherwise

`gar.lua::pruneGateways` slashes and then calls `gar.leaveNetwork`, so in Lua the
two paths cannot differ — that much the port had right. But `constants.lua` has
set `operators.withdrawLengthMs = daysToMs(90)` since 2025-02-07 (`d111500`), the
same as `leaveLengthMs`: **Lua locks both vaults for 90 days.** BD-102's
2026-05-18 note ("the Lua source has it as 30 days") misread the source, so the
change it justified was not a parity restoration.

The port's 30-day excess lock stands on its own footing instead: Solana
deliberately shortens the regular withdrawal lock from Lua's 90 days to 30
(`WITHDRAWAL_LOCK_PERIOD`, already recorded in the BD constants table), and the
excess vault is exactly "what `withdraw_operator_stake` would have produced".

### What operators were told

`@ar.io/sdk`'s `getGatewayRegistrySettings()` reports `leaveLengthMs` as the
30-day withdrawal period and `failedGatewaySlashRate` as `0`, both hardcoded and
both wrong. That is what the reporting operator relied on, and it is why they
expected to claim after 30 days.

## Decision drivers

* An operator must be able to predict their own unlock date from published
  values, and the two exit paths must not disagree about it.
* The minimum stake's 90-day lock is the network's security bond and must stay
  long and non-expedite-able.
* Above-minimum stake is ordinary stake; it should behave the same however the
  operator left.
* The slash is the penalty for failing. Time is not a second penalty.
* Whatever is chosen must be asserted by tests, because this drifted once
  already inside a change that was itself documenting the rule.

## Considered options

1. **The lock belongs to the vault; both paths write the same two periods** (chosen)
2. **Lua-faithful: 90 days for both vaults on both paths**
3. **Keep the paths different on purpose: a pruned operator waits 90 days for everything**
4. **Make the protected period configurable too**

## Decision

**Option 1.** Each vault's lock is a property of the vault:

* protected exit vault → `GATEWAY_LEAVE_PERIOD`, 90 days, a constant;
* excess vault → `GatewaySettings.withdrawal_period`, 30 days by default, read
  from settings so `admin_set_withdrawal_period` applies;

and `leave_network` and `prune_gateway` write exactly these. `prune_gateway` is
corrected accordingly. This is a fix to the prune path, not a change of policy:
the policy is the one `leave_network` has implemented since 2026-05-18 and the
one the SDK, the portal and `withdraw_operator_stake` all describe.

**Option 2** would lengthen every voluntary leaver's excess lock to satisfy a
parity that the Solana port deliberately does not keep for withdrawals; it would
also contradict what operators have been told since mainnet. **Option 3** makes
time a second penalty on top of a 100% slash of the bond, and nothing in the
protocol design asks for that. **Option 4** adds an admin lever over the security
bond itself, which is the one period that should not be movable.

### Explicitly unchanged

These were reviewed in the same pass and are **accepted as they stand**. None is
a defect; do not "fix" them:

* **100% slash of `min_operator_stake` on prune.** Mirrors Lua's
  `failedGatewaySlashRate = 1`.
* **The protected vault's 90-day lock**, and `instant_withdrawal` /
  `deduct_withdrawal_for_payment` refusing it (`ProtectedVault`).
* **Prune re-protecting a second minimum-stake slice out of the remainder.**
  After the bond is slashed there is no longer a minimum stake to protect, so
  protecting an equally sized slice for 90 days is arguably not the same thing —
  but it is what `gar.lua::pruneGateways` does by delegating to `leaveNetwork`,
  and changing it would alter operator economics. Left alone deliberately. A
  future change here needs its own ADR.
* **Delegates of a pruned gateway** keep the 30-day delegate path via
  `claim_delegate_from_leaving_gateway`, which anyone can crank.

### Not retroactive

`available_at` is written once per vault, so already-created vaults keep their
stored date. Three mainnet operators were pruned under the old behaviour
(`2MKD2JLJ…` 36,177.88 ARIO, `Cn79LdBT…` 57,212.67, `FrRJpBnFbg…` 878,549.93;
created 2026-08-04, 08-14 and 08-05). Their excess vaults are `is_protected:
false`, so `instant_withdrawal` is available to them at the **minimum 10%**
penalty — the decay runs over `settings.withdrawal_period`, which has fully
elapsed for all three. Whether to compensate that 10% is an operations decision,
not a protocol one; no migration is needed to give them access.

## Consequences

### Positive

* One rule, stated once, that an operator can check: 90 days for the bond,
  `withdrawal_period` for everything above it, regardless of how they left.
* A pruned operator is punished by the slash, not additionally by time.
* `admin_set_withdrawal_period` now moves the excess lock on both paths, so the
  lever means one thing everywhere.
* Three tests pin all four (path × vault) periods, so the next change to either
  path cannot quietly diverge again.

### Negative / risks

* A failed operator recovers their excess stake sooner than before. That is the
  intended reading of "the slash is the penalty", but it is a real change in
  prune's outcome.
* The divergence from Lua (30 vs 90 on the excess) is now a deliberate, documented
  choice rather than an accident, which means the AO and Solana networks answer
  "when do I get my stake back?" differently. Anything comparing the two must
  read each network's own settings.

### Neutral

* No layout, instruction or account change; only a stored timestamp differs.
* Existing vaults are untouched.

## Implementation notes

* Fix: `prune_gateway` computes `excess_available_at` from
  `settings.withdrawal_period` and writes it to the excess vault. The protected
  vault keeps `GATEWAY_LEAVE_PERIOD`.
* Tests: `test_leave_network_vault_lock_periods`,
  `test_prune_gateway_vault_lock_periods`,
  `test_prune_gateway_excess_follows_withdrawal_period_setting`.
* BD-102 carries the correction row and marks the superseded rows.
* Ships in the Wave 2 GAR upgrade, with ADR-0034, ADR-0036 and ADR-0037. It is
  **not** in the Wave 1 staging artifact built from `ce5831b`.
* **Downstream, required:** `@ar.io/sdk` must stop reporting
  `leaveLengthMs: withdrawalPeriodMs` and `failedGatewaySlashRate: 0`. The
  correct values are `GATEWAY_LEAVE_PERIOD` (90 days, a program constant, not in
  settings) and a 100% slash rate applied to the minimum stake. The Network
  Portal reads these, and the wrong values are what prompted this review.
