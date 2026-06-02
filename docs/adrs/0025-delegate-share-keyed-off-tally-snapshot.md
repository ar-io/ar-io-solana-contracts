# ADR-025: Delegate Reward Share Is Keyed Off the Tally Snapshot, Not Live Delegated Stake

* **Status:** accepted
* **Date:** 2026-06-02
* **Deciders:** @vilenarios

> **TL;DR:** `distribute_epoch` now decides the delegate reward carve-out
> from a per-gateway flag snapshotted at `tally_weights`
> (`GatewaySlot.delegated_at_tally`), not from the live
> `total_delegated_stake` — closing a race where an operator disables
> delegation and cranks every delegate out between tally and distribution to
> redirect the delegate share into `operator_stake`.

> _Note: ADR-0024 is reserved by the concurrent "retire devnet-shrunk" PR
> (#93). The two are independent; whichever merges second keeps its number._

## Context and problem statement

Epoch rewards are split between operator and delegates in two phases:

* **Tally** (`instructions/epoch.rs::tally_weights`) computes a gateway's
  epoch weight from `gateway.total_stake()` =
  `operator_stake + total_delegated_stake` (`state/mod.rs::total_stake`).
  Delegated stake therefore inflates the gateway's share of the epoch
  reward pool. Only the *combined* `composite_weight` was cached in the
  registry slot; the delegated portion was not recorded.
* **Distribute** (`instructions/distribution.rs::distribute_epoch`) split the
  scaled reward with `delegate_pool = scaled × ratio` **gated on the live
  `total_delegated_stake > 0`**, then credited `operator_reward =
  scaled − delegate_pool` to `operator_stake` and `delegate_pool` to the
  per-token accumulator.

Fix #6 (PR #88) added `claim_delegate_from_disabled_gateway`: a
permissionless crank that, once an operator flips
`allow_delegated_staking = false` on a *still-Joined* gateway, moves each
delegation into a withdrawal vault and decrements
`gateway.total_delegated_stake`. Disabling has no
tallied-but-undistributed-epoch guard (`instructions/gateway.rs`).

These compose into a reward-theft race:

1. Delegated stake is present at tally → inflates the gateway's weight →
   larger `scaled_reward`.
2. Operator disables delegation after tally, before distribution.
3. The permissionless crank empties the gateway: each delegate is settled
   against the *pre-distribution* accumulator (so they miss the current
   epoch) and `total_delegated_stake` reaches 0.
4. Distribution sees `total_delegated_stake == 0` → `delegate_pool = 0` →
   the entire `scaled_reward` is credited to `operator_stake`.

The operator captures the delegate share that delegated stake *earned* via
weight; delegators keep principal (in vaults) but lose that epoch's reward.
A program-test PoC showed `operator_reward = 1_000_000_000` where the secure
value is `900_000_000` (10% share ratio), with
`cumulative_reward_per_token = 0`. Severity: **High** — bounded to the
operator's own gateway/delegators and current-epoch rewards; no treasury
drain. Reported by a Codex security pass; verified against the live
`develop` code (PR #88, commit `26defc3`); not yet on mainnet.

The linchpin is leg 4: the split keyed off a value the operator can drive to
zero *after* the weight (and thus the reward) was already earned.

## Decision drivers

* The delegate/operator split must reflect the stake composition that
  *earned* the reward (i.e. at tally), not a value mutable afterward.
* No reward may be redirected to the operator by removing delegates
  mid-epoch.
* Preserve `INVARIANTS.md` Invariant 1 (stake-pool balance == Σ credited).
* Minimal blast radius and schema risk this close to mainnet; the normal
  path must be unchanged.

## Considered options

1. **Snapshot the split decision at tally (chosen).** Record at tally
   whether the gateway carried delegated stake; key the distribution
   carve-out off that snapshot, independent of live stake.
2. **Block the race.** Forbid disabling delegation / forced-claim while the
   gateway has a tallied-but-undistributed epoch. Needs per-gateway
   epoch-distribution tracking (or a coarse global gate that hurts
   liveness).
3. **Do nothing / off-chain monitoring.** Rejected — it's a direct on-chain
   economic-integrity failure.

## Decision

> Adopt **Option 1**. `tally_weights` writes
> `GatewaySlot.delegated_at_tally = u8::from(gateway.total_delegated_stake > 0)`
> alongside `composite_weight` (and `0` on the cleared-slot / non-Joined skip
> paths). `distribute_epoch` carries that flag into `PendingDistribution`
> and gates the delegate carve-out on it via the new `split_scaled_reward`
> helper — **not** on the live `total_delegated_stake`. When the share is owed
> (flag set) but no live delegator remains to receive it, the pool is **held
> back from the treasury transfer** (it stays in `protocol_token_account`)
> rather than credited to the operator.

This satisfies the drivers: the split reflects tally-time composition, the
operator can never capture the delegate share by emptying the gateway, and
Invariant 1 holds because the held-back amount is excluded from the SPL
transfer (so Σ credited == amount transferred). The flag is **carved from
`GatewaySlot._padding`** (`[u8; 7] → delegated_at_tally: u8 + [u8; 6]`), so
`GatewaySlot::SIZE` and `GatewayRegistry::SIZE` are unchanged — **no
zero-copy resize, no rent change, no migration**. Reversible in principle,
but reopening would require reintroducing the live-stake split that caused
the theft.

## Consequences

### Positive

* The reward-theft race is closed at its linchpin; the operator cannot
  redirect the delegate share by disabling delegation + cranking delegates
  out.
* Normal path is byte-for-byte unchanged: when no one removes delegates
  mid-epoch, `delegated_at_tally` reflects the same delegated stake the
  distribution would have seen live.
* No schema-size change (flag lives in former padding); no migration.

### Negative / risks

* In the rare *legitimate* case where every delegator exits between tally
  and distribution, that epoch's delegate share goes to the **treasury**
  (held back), not the operator. This is intentional and conservative.
* Force-claimed delegators still forfeit *that one epoch's* reward (they
  settle against the pre-distribution accumulator). The fix guarantees the
  operator does not receive it; making those delegators whole would require
  Option 2 (blocking the race) and is deliberately out of scope.

### Neutral

* `delegate_reward_share_ratio` is already frozen at tally by Fix #7
  (`pending_…ratio` is applied in `tally_weights` and persisted to the
  gateway), so the live ratio read at distribution equals the tallied ratio
  — the related ratio-front-run vector stays closed.

## Implementation notes

* `state/mod.rs` — `GatewaySlot.delegated_at_tally: u8` (carved from
  padding); all three slot-construction sites init it (`0`), and the
  `finalize_gone` swap-remove preserves it via full-struct copy.
* `instructions/epoch.rs::tally_weights` — write the flag on every slot path.
* `instructions/distribution.rs` — `split_scaled_reward` helper (unit-tested),
  `had_delegation_at_tally` on `PendingDistribution`, orphan-to-treasury
  hold-back, and a corrected "insufficient balance" warning condition.
* Tests: `split_scaled_reward` unit tests (incl. the 9e8-not-1e9 PoC value);
  `test_tally_snapshots_delegated_at_tally` (flag == 1 with delegation) and a
  negative assertion in `test_epoch_create_and_tally` (flag == 0). The full
  end-to-end distribution PoC remains gated behind the `#[ignore]`'d
  distribute-CPI suite (needs ario-core loaded).

## Related

* Code: `programs/ario-gar/src/instructions/distribution.rs`,
  `programs/ario-gar/src/instructions/epoch.rs`,
  `programs/ario-gar/src/state/mod.rs`,
  `programs/ario-gar/src/instructions/delegate.rs`,
  `programs/ario-gar/src/instructions/gateway.rs`
* Behavioral diff: BD-111
* Origin: Fix #6 (PR #88, commit `26defc3`); Codex security pass
* Related decision: Fix #7 deferred reward-share ratio (freezes the ratio at
  tally); ADR-023 (`prescribe_epoch` live-sum) is the analogous
  snapshot-vs-live distinction on the prescription path.
