# Stake & Treasury Invariants

This document records the on-chain accounting invariants the
[`ario-gar`](programs/ario-gar) program maintains across all
state-changing instructions, plus the design decisions that look
unusual (delegate-reward staleness, slash semantics, no lost-key
recovery) so future readers don't have to re-derive them from the
code.

The invariants here are the ones an indexer, a tokenomics dashboard,
an external auditor, or a property-test suite should rely on. They
are stated against the on-chain account schema; field names and line
numbers match the canonical implementation in this repository.

## The token accounts

Two SPL token accounts are pinned in
[`GatewaySettings`](programs/ario-gar/src/state/mod.rs#L55) and
validated on every instruction that touches them:

| Account                  | Role                                                                      | Authority                  |
| ------------------------ | ------------------------------------------------------------------------- | -------------------------- |
| `stake_token_account`    | Shared custodial pool — holds every staker's principal + accrued rewards. | `GatewaySettings` PDA      |
| `protocol_token_account` | Treasury — pays rewards, receives slashes / payment / instant-wd fees.    | `GatewaySettings` PDA      |

`stake_token_account` is **not** protocol-owned. Every token in it
is owed back to a user via on-chain accounting (gateway operators,
delegates, pending withdrawals). Tokenomics dashboards that report
the pool balance as "AR.IO holdings" are wrong. Use the invariant
below to compute the protocol's actual treasury position.

## Invariant 1 — Stake-pool conservation

The SPL balance of `stake_token_account` is fully accounted for by the
sum of on-chain accounting fields that claim it:

```
stake_token_account.balance
  = Σ Gateway.operator_stake               // settled operator principal
  + Σ Gateway.total_delegated_stake        // settled delegation principal (aggregate)
  + Σ Withdrawal.amount                    // pending exits, including exit vaults
  + Σ pending_delegate_rewards             // unsettled rewards in the per-share accumulator
```

where the first three terms range over every `Gateway` PDA in `Joined`
or `Leaving` status and every `Withdrawal` PDA the program owns, and
the fourth term is the sum of unsettled delegate rewards computed from
the per-share accumulator (see ["Pending delegate rewards term"](#pending-delegate-rewards-term)
below).

> **The first three terms alone are NOT enough.** Between the moment
> `distribute_epoch` completes and each delegate's next settlement,
> the SPL pool is **ahead** of the three settled-state sums by exactly
> the unsettled delegate rewards — distribute_epoch transfers tokens
> into the pool but intentionally leaves `total_delegated_stake`
> unchanged (the per-share accumulator pattern is O(1) per epoch
> regardless of delegate count). Off-chain readers that omit the
> fourth term will false-alarm on any cluster where `distribute_epoch`
> has run more recently than every delegate has settled — i.e.,
> essentially every production cluster, every epoch. See
> ["Stale-by-design"](#stale-by-design-delegate-rewards) for the
> mechanics.

### Pending delegate rewards term

`pending_delegate_rewards` is not stored as a field. It is computed
per-`Delegation` from the per-share accumulator:

```
pending(delegation) = ((gateway.cumulative_reward_per_token - delegation.reward_debt) * delegation.amount) / REWARD_PRECISION
```

with the same `u128` overflow-safe quotient/remainder split and
`u64::MAX` saturating cap that [`settle_delegate_rewards`](programs/ario-gar/src/state/mod.rs#L444)
applies on-chain. `REWARD_PRECISION = 10^18` (state/mod.rs:8). Sum
across every `Delegation` PDA in the program.

The AR.IO SDK's [`computeLiveDelegationBalance`](https://github.com/ar-io/ar-io-sdk/blob/solana/src/solana/delegation-math.ts)
helper (TypeScript, `@ar.io/sdk@solana`) is a 1:1 port of the on-chain
math and is what indexers / wallets / the network portal should use
for both display and invariant verification.

### Why it holds

Every code path that moves SPL tokens into or out of
`stake_token_account` updates the corresponding accounting field in
the same instruction, atomically. The only exception is
`distribute_epoch`, which updates `Gateway.cumulative_reward_per_token`
(accumulator pattern) rather than `Gateway.total_delegated_stake` for
the delegate share — the fourth invariant term above captures those
unsettled rewards.

Full mapping:

| Instruction                            | Token flow                                       | Accounting delta                                                                |
| -------------------------------------- | ------------------------------------------------ | ------------------------------------------------------------------------------- |
| `join_network`                         | operator ATA → stake pool                        | `Gateway.operator_stake ↑`                                                      |
| `increase_operator_stake`              | operator ATA → stake pool                        | `Gateway.operator_stake ↑`                                                      |
| `decrease_operator_stake`              | (none — tokens stay in pool)                     | `Gateway.operator_stake ↓` ; `Withdrawal.amount ↑`                              |
| `delegate_stake`                       | delegator ATA → stake pool                       | `Gateway.total_delegated_stake ↑` ; `Delegation.amount ↑`                       |
| `decrease_delegate_stake`              | (none)                                           | `Gateway.total_delegated_stake ↓` ; `Delegation.amount ↓` ; `Withdrawal.amount ↑` |
| `claim_delegate_from_leaving_gateway`  | (none)                                           | `Gateway.total_delegated_stake ↓` ; `Delegation.amount → 0` ; `Withdrawal.amount ↑` |
| `distribute_epoch`                     | treasury → stake pool                            | `Gateway.operator_stake ↑` (operator share). For the delegate share: `Gateway.cumulative_reward_per_token ↑` only — `total_delegated_stake` is intentionally NOT touched; the rewards live in the accumulator until each delegate settles (see [Stale-by-design](#stale-by-design-delegate-rewards)). |
| `cancel_withdrawal`                    | (none)                                           | `Withdrawal` closed ; `Gateway.operator_stake ↑` or `Delegation.amount ↑` (+ `total_delegated_stake ↑`) |
| `claim_withdrawal`                     | stake pool → owner ATA                           | `Withdrawal` closed                                                             |
| `instant_withdrawal`                   | stake pool → owner ATA (payout) + treasury (fee) | `Withdrawal` closed                                                             |
| `prune_gateway` (slash)                | stake pool → treasury (slash portion)            | `Gateway.operator_stake ↓` (full); slash leaves system, remainder vaulted        |
| `deduct_operator_stake_for_payment`    | stake pool → treasury                            | `Gateway.operator_stake ↓`                                                      |
| `deduct_delegation_for_payment`        | stake pool → treasury                            | `Gateway.total_delegated_stake ↓` ; `Delegation.amount ↓`                       |
| `deduct_withdrawal_for_payment`        | stake pool → treasury                            | `Withdrawal.amount ↓`                                                           |

`Withdrawal.amount` covers all four withdrawal-PDA flavors that the
program currently mints: standard withdrawals
([`operator_stake.rs::decrease_operator_stake`](programs/ario-gar/src/instructions/operator_stake.rs)),
delegate withdrawals
([`delegate.rs::decrease_delegate_stake`](programs/ario-gar/src/instructions/delegate.rs)),
and the protected/excess exit vaults created by
[`leave_network`](programs/ario-gar/src/instructions/gateway.rs#L154)
and the post-slash flow inside
[`prune_gateway`](programs/ario-gar/src/instructions/gateway.rs#L488).

### Conservation through cross-flows

Two paths move tokens *out* of `stake_token_account` to
destinations other than a user's wallet, and both still preserve
the invariant because they decrement accounting in lockstep:

1. **Slash on prune** — `slash_amount = min(min_operator_stake,
   operator_stake)` is transferred to the treasury
   ([`gateway.rs:639-653`](programs/ario-gar/src/instructions/gateway.rs#L639)).
   `Gateway.operator_stake` is reduced by the full pre-slash amount
   (`slash_amount + post_slash`); `post_slash` becomes withdrawal
   vaults, `slash_amount` leaves the staking ledger entirely.
2. **Payment from stake** — the
   [`payment.rs`](programs/ario-gar/src/instructions/payment.rs)
   `deduct_*_for_payment` instructions move tokens
   `stake_token_account → protocol_token_account` while decrementing
   the matching accounting field. Used by `ario-arns` for ArNS
   purchases funded from stake / delegation / pending withdrawals.

### Instant withdrawal fee

[`instant_withdrawal`](programs/ario-gar/src/instructions/withdrawal.rs#L61)
performs two SPL transfers in the same instruction:

- `payout` from `stake_token_account` to the owner's ATA
- `fee` from `stake_token_account` to `protocol_token_account`

The `Withdrawal.amount` PDA value covers `payout + fee` — the entire
amount that leaves the staking ledger. So `Withdrawal.amount ↓` by
the full original amount, the pool balance drops by `payout + fee`,
and the invariant holds.

## Invariant 2 — Supply-counter shadow

[`GatewaySettings`](programs/ario-gar/src/state/mod.rs#L55) carries
three supply counters maintained alongside the per-entity state:

```
GatewaySettings.total_staked      = Σ Gateway.operator_stake
GatewaySettings.total_delegated   = Σ Gateway.total_delegated_stake
GatewaySettings.total_withdrawn   = Σ Withdrawal.amount
```

These counters are updated atomically in the same instructions that
mutate per-entity state. They serve as a redundant accounting layer:
any disagreement between the supply counter and the per-entity sum
is a real bug. Combined with Invariant 1 this gives two
independent paths to verify the system:

```
stake_token_account.balance
  = Σ Gateway.operator_stake + Σ Gateway.total_delegated_stake + Σ Withdrawal.amount      // Invariant 1
  = GatewaySettings.total_staked + GatewaySettings.total_delegated + GatewaySettings.total_withdrawn   // Invariant 2
```

A drift between Invariants 1 and 2 indicates a missed-update bug in
some instruction. A drift between either side and
`stake_token_account.balance` indicates a missed token-transfer or
missed accounting update.

## Invariant 3 — Treasury monotonicity (with documented inflows)

`protocol_token_account` is the protocol's actual treasury balance.
In the absence of operator-driven inflows it would decrease
monotonically as `distribute_epoch` pays rewards. In practice three
inflows offset reward payouts:

- **`prune_gateway`** transfers `slash_amount` from stake pool to
  treasury.
- **`deduct_*_for_payment`** transfers ArNS purchase fees from stake
  pool to treasury.
- **`instant_withdrawal`** transfers the early-exit fee from stake
  pool to treasury.

Refilling the treasury (e.g., from a future protocol mint or fee
sweep) is not part of `ario-gar`'s public surface today; any future
top-up instruction must be added here when introduced.

## Invariant 4 — A gateway's delegated-stake counter equals its delegations

```
Gateway.total_delegated_stake = Σ Delegation.amount   (over that gateway's Delegation PDAs)
```

Both sides hold **settled principal** only: `distribute_epoch` credits
delegates by raising `Gateway.cumulative_reward_per_token`, never by
touching either side, and `settle_delegate_rewards` raises the
Delegation **and** the counter together. Every other mover —
`delegate_stake`, `decrease_delegate_stake`, `redelegate_stake`, both
`claim_delegate_from_*` cranks, `cancel_withdrawal`, and the
fund-from-stake paths — moves one Delegation and the counter by the same
amount. A Delegation can only be closed at amount 0.

So `counter − Σ Delegation.amount` is **invariant under every
instruction**: whatever value a gateway is created with, it keeps
forever. That is what makes a violation permanent rather than
self-healing, and it is why this invariant is stated separately from
Invariant 2 — the supply counter can drift and be resynced, but this one
cannot be repaired by any ordinary instruction.

**This invariant was violated at genesis and went unnoticed for months
because it was not written down here.** The AO import wrote each
counter from AO's total but created Delegation accounts only for
delegators who had a Solana address; the rest of the stake went to the
migration authority's pot and never entered the stake pool. On mainnet
that is **2,226,210.675676 ARIO** over-counted across 132 gateways,
reproduced to the mARIO from the genesis snapshot for 622 of 622
gateways. Consequences: `finalize_gone` is permanently blocked on 65
leaving gateways, delegation cannot be re-enabled on an over-counted
gateway, 67 joined gateways draw stake weight and observer-selection
odds for stake they do not hold, and every epoch carves a delegate
share that is divided by the inflated counter.

The only repair is the authority-only
`admin_reconcile_delegated_stake` (ADR-0037), which lowers a counter to
a sum proven from the Delegation accounts it is shown. **The program
cannot enumerate those accounts**, so this invariant is not enforceable
on-chain in the `>=` direction — it must be checked off-chain.
`scripts/delegated-stake-audit.mjs --cluster mainnet|staging` does that;
exit status 1 means a gateway is under-counted, which is the dangerous
direction (a counter below the real sum lets `finalize_gone` close a
Gateway PDA that live delegates still need in order to claim).

## Stale-by-design: delegate rewards

This is the part most easily mis-coded by integrators.

`distribute_epoch` does **not** increment `Delegation.amount` for
each delegate. Doing so per-delegate per-epoch would be O(delegates)
per epoch, which doesn't fit Solana's compute budget. Instead the
program uses a per-share accumulator:

- [`Gateway.cumulative_reward_per_token`](programs/ario-gar/src/state/mod.rs#L214)
  — a `u128` running sum, in scaled units, of rewards-per-staked-token.
  Incremented by `distribute_epoch` once per gateway per epoch.
- [`Delegation.reward_debt`](programs/ario-gar/src/state/mod.rs#L434)
  — a `u128` snapshot of `cumulative_reward_per_token` taken when
  the delegate's position was last settled.

The delegate's *true* current balance is:

```
live_balance = Delegation.amount
             + ((Gateway.cumulative_reward_per_token - Delegation.reward_debt) * Delegation.amount) / SCALE
```

[`settle_delegate_rewards`](programs/ario-gar/src/state/mod.rs#L444)
realizes this difference into `Delegation.amount` and updates
`reward_debt` to the gateway's current accumulator. It is called
automatically on every delegation interaction
(`increase`, `decrease`, `redelegate`, `claim_delegate_from_leaving_gateway`)
and via the permissionless
[`compound_delegation_rewards`](programs/ario-gar/src/instructions/delegate.rs#L485)
instruction.

**Consequences:**

- Reading `Delegation.amount` directly from on-chain state without
  settling gives a stale value — possibly many epochs behind reality.
  This is correct behavior, not a bug.
- Indexers, wallets, and dashboards that display delegate balances
  must compute `live_balance` from the accumulator + reward_debt,
  not read the raw field. The AR.IO SDK does this automatically in
  `getGatewayDelegates` / `getDelegations` / `getAllDelegates` since
  `@ar.io/sdk@solana` >= 4.0.0-solana.6.
- Operators don't have this problem — `distribute_epoch` writes
  directly into `Gateway.operator_stake`, so a snapshot of that
  field is always the live operator balance.

### Invariant 1 violation window

Between `distribute_epoch` completing and each delegate's next
settlement (via any delegation IX or via permissionless
`compound_delegation_rewards`), the three settled-state terms in
Invariant 1 (`Σ operator_stake + Σ total_delegated_stake +
Σ Withdrawal.amount`) are **strictly less than** the SPL pool
balance — by exactly the unsettled delegate-reward amount.

What happens during `distribute_epoch`
([`distribution.rs:35-337`](programs/ario-gar/src/instructions/distribution.rs)):

1. SPL transfer: `protocol_token_account → stake_token_account` for
   the full batch reward (operator + delegate shares combined,
   line 285-299).
2. `Gateway.operator_stake ↑` (line 222) and `settings.total_staked ↑`
   (line 330-334) — operator share.
3. `Gateway.cumulative_reward_per_token ↑` (line 240-243) — delegate
   share goes into the accumulator.
4. `Gateway.total_delegated_stake` and `settings.total_delegated` are
   **not** touched.

The fourth term in Invariant 1 (`Σ pending_delegate_rewards`)
captures exactly this gap; it is what restores the equation.

In practice every production cluster is in this window almost
continuously — `distribute_epoch` runs every epoch, dormant
delegations may never settle for weeks. The fourth term is therefore
mandatory for any off-chain health check; omitting it produces a
false-positive after every epoch.

`Σ pending_delegate_rewards == 0` only when the accumulator has not
advanced since every delegation last settled, which is fleeting in
production.

The supply-counter shadow ([Invariant 2](#invariant-2--supply-counter-shadow))
is unaffected by the *accumulator* staleness: while nothing settles, both
`settings.total_delegated` and `Σ Gateway.total_delegated_stake` are equally
stale, so they agree. Only the SPL-pool-vs-accounting equation depends on the
fourth term.

> **Corrected 2026-09-18 (ADR-0037).** The "equally stale" argument held only
> until the first *settlement*. `settle_delegate_rewards` raises
> `Gateway.total_delegated_stake`, and before ADR-0037 no caller raised
> `settings.total_delegated` to match — `compound_delegation_rewards` had no
> `settings` account at all. So every settled reward widened a permanent gap,
> which reached **80,442.894868 ARIO** on mainnet. The property test could not
> catch it because it never settled a reward.
>
> Fixed in the Wave 2 GAR upgrade: `settle_delegate_rewards` now returns the
> settled amount and every one of its callers adds it to the supply counter.
> `admin_resync_supply_counters` corrects the accumulated drift once. Any health
> check that ran before that upgrade should expect this gap on mainnet rather
> than treat it as a fresh bug.

## Known limits

Two protocol behaviors look surprising but are deliberate.

### Lost-key stranding

There is no governance instruction to reclaim stake from an operator
or delegator who has lost their keypair. The accounting credit stays
on-chain forever; the corresponding SPL tokens sit in
`stake_token_account` permanently un-withdrawable. Over the lifetime
of the protocol this fraction grows monotonically. The trade-off is
the strong custody guarantee: no admin role can move user funds.

`prune_gateway` is sometimes mistaken for a recovery mechanism — it
isn't. It only slashes `min_operator_stake` from a failing gateway
and forwards the remainder into withdrawal vaults the operator still
has to sign for. A gateway with a lost operator key will accumulate
failed observations, eventually be pruned, lose `min_operator_stake`
to the treasury, and leave the rest stranded in vaults indefinitely.

### Slash rate is 100% of min, not 100% of stake

[`prune_gateway`](programs/ario-gar/src/instructions/gateway.rs#L488)
slashes exactly `min(min_operator_stake, gateway.operator_stake)` —
not the operator's full position. The remainder (`post_slash`) is
split into a "protected" vault sized at `min(min_stake, post_slash)`
and an "excess" vault for everything above min. Both vaults are
owned by the operator and claimable after the standard
`GATEWAY_LEAVE_PERIOD` cooldown.

This matches the Lua source's `failedGatewaySlashRate = 1.0` applied
only to the protocol-mandated minimum stake. Delegators are not
slashed; their positions flow through
`claim_delegate_from_leaving_gateway` like a normal voluntary leave.

## Verifying the invariants

### Property tests

The invariants are asserted by four tests in
[`programs/ario-gar/tests/integration.rs`](programs/ario-gar/tests/integration.rs)
(grep for `test_stake_conservation_`):

| Test                                       | Coverage                                                                                       |
| ------------------------------------------ | ---------------------------------------------------------------------------------------------- |
| `test_stake_conservation_across_operations`| Single gateway: join → decrease → invariant after each. The original regression check.         |
| `test_stake_conservation_global`           | 3 gateways + 3 delegations + decreases. Asserts Invariants 1 and 2 at every checkpoint.        |
| `test_stake_conservation_slash_path`       | `prune_gateway` slash: verifies stake-pool conservation across the slash + Invariant 3 (treasury inflow equals `MIN_OPERATOR_STAKE`) + delegations untouched. |
| `test_stake_conservation_payment_paths`    | `deduct_operator_stake_for_payment` and `deduct_delegation_for_payment`: stake → treasury cross-flow preserves both invariants and treasury inflow matches the deduction amount. |

The shared helpers `assert_global_stake_invariants` and
`create_funded_actor` (defined near the first invariant test) are
reusable for any future scenario test.

> **Coverage caveat.** None of these tests call `distribute_epoch`,
> so none exercise the [Invariant 1 violation window](#invariant-1-violation-window) directly. The helper
> `assert_global_stake_invariants` checks the three settled-state
> terms only and is sufficient for the scenarios it's run against
> (join / delegate / decrease / slash / payment — all of which leave
> `sumPendingRewards == 0`).
>
> **Partly closed 2026-09-18 (ADR-0037).**
> `test_compound_keeps_supply_counter_in_step` now credits a gateway's
> reward accumulator, funds the pool to match, compounds, and then runs
> `assert_global_stake_invariants` — a settle-then-check that the earlier
> scenarios could not perform. That is what exposed the Invariant 2
> defect: settlement raised the gateway counter while nothing raised
> `settings.total_delegated`. The full epoch-cycle test described below is
> still outstanding. Extending the helper and adding a
> `test_stake_conservation_distribute_window` test that exercises a
> full epoch cycle (create → tally → prescribe → save_observations
> → distribute → assert with the fourth term) is the obvious
> follow-up; it's left as future work because the epoch-cycle setup
> in integration tests is substantial.

### Off-chain monitoring

A periodic health-check job that sums on-chain state and compares it
to the pool balance is the canonical operational tool. The fourth
invariant term is mandatory — omitting it produces false positives
after every `distribute_epoch` (see [Invariant 1 violation window](#invariant-1-violation-window)).

```typescript
import { computeLiveDelegationBalance, REWARD_PRECISION } from '@ar.io/sdk/solana';

const settings    = await fetchGatewaySettings(rpc);
const gateways    = await fetchAllGateways(rpc);            // includes cumulative_reward_per_token
const delegations = await fetchAllDelegations(rpc);          // includes reward_debt
const withdrawals = await fetchAllWithdrawals(rpc);
const poolBalance = BigInt(
  (await rpc.getTokenAccountBalance(settings.stake_token_account)).value.amount,
);

const gatewayByAddr = new Map(gateways.map((g) => [g.address, g]));

const sumOperator  = gateways.reduce((s, g) => s + BigInt(g.operator_stake), 0n);
const sumDelegated = gateways.reduce((s, g) => s + BigInt(g.total_delegated_stake), 0n);
const sumWithdrawn = withdrawals.reduce((s, w) => s + BigInt(w.amount), 0n);

// Fourth term: unsettled delegate rewards, computed from each delegation's
// reward_debt vs its gateway's cumulative_reward_per_token. The on-chain
// `settle_delegate_rewards` does exactly this when a delegate next interacts.
const sumPendingRewards = delegations.reduce((s, d) => {
  const gw = gatewayByAddr.get(d.gateway);
  if (!gw) return s;
  const live = computeLiveDelegationBalance({
    delegatedStake: d.amount,
    rewardDebt: d.reward_debt,
    cumulativeRewardPerToken: gw.cumulative_reward_per_token,
  });
  return s + BigInt(live - d.amount); // pending = live - settled
}, 0n);

// Invariant 1 — stake-pool conservation with all four terms.
assert(poolBalance === sumOperator + sumDelegated + sumWithdrawn + sumPendingRewards);

// Invariant 2 — supply-counter shadow. Independent of distribute_epoch
// staleness, because while nothing settles both sides are equally stale.
// NOTE (ADR-0037): before the Wave 2 GAR upgrade, settlement raised the gateway
// counter without raising settings.total_delegated, so this assertion legitimately
// fails on any cluster that settled rewards under the old code until
// admin_resync_supply_counters has run.
assert(sumOperator  === BigInt(settings.total_staked));
assert(sumDelegated === BigInt(settings.total_delegated));
assert(sumWithdrawn === BigInt(settings.total_withdrawn));

// Invariant 4 — per gateway, the counter equals the delegations behind it.
// This is the check whose absence let 2,226,210.675676 ARIO of phantom
// delegated stake sit unnoticed on mainnet from genesis. It is per-gateway:
// the totals above net out and will NOT reveal it.
const delegatedByGateway = new Map();
for (const d of delegations) {
  delegatedByGateway.set(
    d.gateway,
    (delegatedByGateway.get(d.gateway) ?? 0n) + BigInt(d.amount),
  );
}
for (const g of gateways) {
  const backed = delegatedByGateway.get(g.address) ?? 0n;
  const counter = BigInt(g.total_delegated_stake);
  // counter > backed: phantom stake — blocks finalize_gone, inflates weight.
  // counter < backed: DANGEROUS — finalize_gone could close a Gateway PDA
  // that live delegates still need in order to claim.
  assert(counter === backed, `${g.address}: counter ${counter} vs delegations ${backed}`);
}
```

`scripts/delegated-stake-audit.mjs --cluster mainnet|staging` is the
maintained implementation of the Invariant 4 loop, with snapshot
cross-checking; exit status 1 means a gateway is under-counted.

Run this against every cluster (devnet, mainnet) on a schedule.
Drift indicates a real bug and should page.

**Sanity tip:** if Invariant 2 holds but Invariant 1 fails by a small
positive amount, you're almost certainly missing the
`sumPendingRewards` term. Compute it (or use the SDK's
`computeLiveDelegationBalance`) and re-check.
