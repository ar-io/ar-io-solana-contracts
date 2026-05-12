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

At every point between transactions, the SPL balance of
`stake_token_account` is exactly the sum of the on-chain accounting
fields that claim it:

```
stake_token_account.balance
  = Σ Gateway.operator_stake             // active operator principal
  + Σ Gateway.total_delegated_stake      // active delegation principal (aggregate)
  + Σ Withdrawal.amount                  // pending exits, including exit vaults
```

The sums range over every `Gateway` PDA in `Joined` or `Leaving`
status and every `Withdrawal` PDA the program owns.

### Why it holds

Every code path that moves SPL tokens into or out of
`stake_token_account` updates the corresponding accounting field in
the same instruction, atomically. The full mapping:

| Instruction                            | Token flow                                       | Accounting delta                                                                |
| -------------------------------------- | ------------------------------------------------ | ------------------------------------------------------------------------------- |
| `join_network`                         | operator ATA → stake pool                        | `Gateway.operator_stake ↑`                                                      |
| `increase_operator_stake`              | operator ATA → stake pool                        | `Gateway.operator_stake ↑`                                                      |
| `decrease_operator_stake`              | (none — tokens stay in pool)                     | `Gateway.operator_stake ↓` ; `Withdrawal.amount ↑`                              |
| `delegate_stake`                       | delegator ATA → stake pool                       | `Gateway.total_delegated_stake ↑` ; `Delegation.amount ↑`                       |
| `decrease_delegate_stake`              | (none)                                           | `Gateway.total_delegated_stake ↓` ; `Delegation.amount ↓` ; `Withdrawal.amount ↑` |
| `claim_delegate_from_leaving_gateway`  | (none)                                           | `Gateway.total_delegated_stake ↓` ; `Delegation.amount → 0` ; `Withdrawal.amount ↑` |
| `distribute_epoch`                     | treasury → stake pool                            | `Gateway.operator_stake ↑` (operator share) ; `Gateway.cumulative_reward_per_token ↑` (delegate share) |
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
  not read the raw field.
- Invariant 1 still holds at all times because every delegate's
  pending rewards are tracked at the gateway level via the
  accumulator. The unsettled rewards live in
  `stake_token_account` (they were transferred there by
  `distribute_epoch`) and are accounted for under
  `Gateway.total_delegated_stake`, which `distribute_epoch`
  increments alongside `cumulative_reward_per_token`.
- Operators don't have this problem — `distribute_epoch` writes
  directly into `Gateway.operator_stake`, so a snapshot of that
  field is always the live operator balance.

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

### Property test

[`programs/ario-gar/tests/integration.rs::test_stake_conservation_global`](programs/ario-gar/tests/integration.rs)
exercises Invariants 1 and 2 across a multi-gateway / multi-delegate
scenario with mixed operations (joins, delegations, decreases,
withdrawals, reward distribution). The existing
`test_stake_conservation_across_operations` is a narrower
single-gateway version of the same check. Future paths worth adding
as their own focused tests:

- Slash flow via `prune_gateway` after triggering enough failed
  observations.
- Cross-program payment via `deduct_*_for_payment` invoked through
  `ario-arns`.

### Off-chain monitoring

A periodic health-check job that sums on-chain state and compares it
to the pool balance is the canonical operational tool. Pseudocode:

```typescript
const settings    = await fetchGatewaySettings(rpc);
const gateways    = await fetchAllGateways(rpc);
const withdrawals = await fetchAllWithdrawals(rpc);
const poolBalance = (await rpc.getTokenAccountBalance(settings.stake_token_account)).value.amount;

const sumOperator  = gateways.reduce((s, g) => s + g.operator_stake, 0n);
const sumDelegated = gateways.reduce((s, g) => s + g.total_delegated_stake, 0n);
const sumWithdrawn = withdrawals.reduce((s, w) => s + w.amount, 0n);

assert(BigInt(poolBalance) === sumOperator + sumDelegated + sumWithdrawn);                  // Invariant 1
assert(sumOperator  === settings.total_staked);                                              // Invariant 2 (split)
assert(sumDelegated === settings.total_delegated);
assert(sumWithdrawn === settings.total_withdrawn);
```

Run this against every cluster (devnet, mainnet) on a schedule.
Drift indicates a real bug and should page.
