# ADR-0032: A Gateway With Stale Weights Is Ineligible, Not a Distribution Halt

* **Status:** proposed
* **Date:** 2026-09-11
* **Deciders:** @vilenarios
* **Consulted:** vilenarios.com gateway operator (incident report), permagate.io (corroborating cranker)
* **Informed:** AR.IO gateway operators, cranker/observer maintainers

> **TL;DR:** `distribute_epoch` reverts the whole batch when a `joined` gateway
> carries a stale `weights_epoch`, which lets one mid-epoch join freeze reward
> distribution network-wide; fold staleness into the existing eligibility test
> so such a gateway earns 0 and the cursor advances past it.

## Context and problem statement

`distribute_epoch` walks the live `GatewayRegistry` by position. For each slot
it requires the corresponding `Gateway` account to have been tallied for the
epoch being distributed
([`distribution.rs:176-180`](../../programs/ario-gar/src/instructions/distribution.rs)):

```rust
let is_leaving = gateway.status == GatewayStatus::Leaving;
if !is_leaving {
    require!(
        gateway.weights.weights_epoch == epoch.epoch_index,
        GarError::WeightsNotTallied
    );
}
```

`weights_epoch` has exactly **one writer** —
[`epoch.rs:776`](../../programs/ario-gar/src/instructions/epoch.rs), inside
`tally_weights` — and exactly **one reader**, the `require!` above.
[`state/mod.rs:548`](../../programs/ario-gar/src/state/mod.rs) documents the
contract inline: `weights_epoch: 0, // set by tally_weights caller`.

`tally_weights` refuses to re-run once `epoch.weights_tallied == 1`
(`WeightsAlreadyTallied`). So any `joined` gateway that enters the registry
**after** an epoch's tally has completed can never acquire that epoch's
`weights_epoch`, and the `require!` above is unsatisfiable for it for the
remainder of that epoch's lifecycle.

Because the guard is a `require!` and not a skip, it does not merely exclude
that one gateway — it **aborts the entire batch**. And because
`distribute_epoch` validates each account positionally against the cursor
(`registry.gateways[dist_idx].address == gateway.operator`,
[`distribution.rs:168`](../../programs/ario-gar/src/instructions/distribution.rs)),
the blocking position cannot be skipped at any batch size or offset. One
untalliable gateway is therefore a **permanent, network-wide halt** of that
epoch's distribution.

### The mainnet incident (2026-09-11, epoch 540)

Verified by direct on-chain reads against mainnet:

| | |
|---|---|
| Epoch 540 PDA | `kMHy4xUScrVj5fLydyPrbu7Bzhz1sFR1a1tun8qjXQq` |
| `tally_index` / `weights_tallied` | 649 / 1 — tally complete |
| `distribution_index` | **615** — cursor stuck |
| `active_gateway_count` | 649 |
| live `registry.count` | 647 |
| blocking slot | index **622**, operator `BFifSxnx7LewF3kojug6PykEammDeft7hfGki6E6XyMX` (`lazygiraffe.io`) |
| that slot's `composite_weight` | **0** |

`lazygiraffe.io` called `join_network` 16.6 h into epoch 540, after that
epoch's tally had been flagged done. Every distribution transaction from
cursor 615 spans position 622 and reverts with `WeightsNotTallied (6048)`.
Distribution — and, through a separate client-side defect, epoch progression —
was frozen network-wide for ~15 h. 32 registry positions (615–646) went
undistributed; the reporting operator counted 23 still-earning gateways among
them, ≈3,646 ARIO at `per_gateway_reward = 158.513746 ARIO`.

### The guard is already redundant for this case

Reward eligibility is *independently* gated three lines further down
([`distribution.rs:209`](../../programs/ario-gar/src/instructions/distribution.rs)):

```rust
let is_eligible = registry.gateways[dist_idx].composite_weight > 0;
```

`join_network` writes `composite_weight: 0` into the new registry slot
([`gateway.rs:118`](../../programs/ario-gar/src/instructions/gateway.rs)), so a
gateway in this state is **already ineligible and would receive 0**. The
`require!` protects no funds in this scenario. It only removes liveness.

The comment above `is_eligible` states the assumption that made this invisible:

> *"a late-joiner (joined after epoch_start → composite forced to 0 at tally,
> SHOULD-13 in epoch.rs) is excluded from that divisor. **Its weights ARE fresh
> (it was tallied)** so the freshness gate above passes…"*

That holds for a gateway that joins *before* tally. It is false for one that
joins *after* tally, which is the case nobody modelled.

### The guard is vacuous at epoch index 0

Surfaced while writing the regression tests, and worth recording because it
shapes how this must be tested: `weights_epoch` is initialised to 0, so for
**epoch index 0** an untallied gateway satisfies `weights_epoch == epoch_index`
trivially and the guard never fires. A test written against epoch 0 therefore
passes identically before and after this change — the first draft of the
regression test did exactly that, and had to be retargeted to epoch index 1 to
reproduce the failure at all. No mainnet consequence (the incident was epoch
540), but any future test of this path must run at a non-zero epoch index.

### This was observed once already

[`solana-ar-io/docs/EPOCH_RENT_TO_CREATOR_PLAN.md` §6.3](https://github.com/ar-io/solana-ar-io)
records staging epochs 790 and 791 rendered permanently undistributable on
2026-08-29 by this same `require!`, correctly identifying it as *"a pre-existing
protocol property, unrelated to ADR-0029"*. The trigger there was two crankers
racing under a compressed cadence, each re-stamping the shared `weights_epoch`
field. That write-up concluded **"This cannot arise on mainnet as planned"**,
reasoning that a 24 h cadence makes all crankers converge on the same epoch.

That reasoning was sound for the cranker-race trigger and missed the
mid-epoch-join trigger entirely. The lesson for this ADR is that the defect is
in the guard, not in any particular way of reaching it — so the fix must
address the guard rather than the trigger.

### What this ADR does *not* fix

A second, independent defect surfaced in the same incident and is explicitly
**out of scope** here: `active_gateway_count` is snapshotted at epoch creation
while the cursor indexes a *mutable* registry, so removals between tally and
distribution leave the completion test
(`dist_idx >= active_count`,
[`distribution.rs:475`](../../programs/ario-gar/src/instructions/distribution.rs))
pointing beyond the live tail. On-chain this is already survivable — the
cleared-slot skip at
[`distribution.rs:139`](../../programs/ario-gar/src/instructions/distribution.rs)
(the GAR-009 / audit M-1 mitigation) traverses zeroed slots — but each skipped
slot consumes one `remaining_account`, so **clients must supply accounts up to
`active_gateway_count`, not up to `registry.count`.** That is a client-side fix
and a candidate for a future epoch-scoped-snapshot ADR; see *Considered
options, option 4*.

## Decision drivers

* **A single gateway's state must never be able to halt the network.** Liveness
  of reward distribution is a protocol-level property; it cannot depend on the
  behaviour of an arbitrary unaffiliated operator.
* **No untallied gateway may ever be paid.** Whatever replaces the guard must
  preserve its actual protective purpose.
* **Minimise rollout cost.** The mainnet GAR program is live; anything that
  changes an account layout or an instruction ABI drags in a migration and a
  lockstep client cutover (see the Phase 0 classification in
  `MAINNET_UPGRADE_ROLLOUT_PLAN.md`).
* **Preserve the single-writer contract on `weights_epoch`.** One writer, one
  reader, documented in the struct. Additional writers make the field's meaning
  ambiguous and are hard to reason about after the fact.
* **The fix must be provable on staging** against a reproduction of the real
  trigger, not only against a synthetic unit test.

## Considered options

1. **Status quo + operational recovery.** Leave the guard; recover each
   occurrence by persuading the blocking operator to `leave_network`, or by
   `admin_close_stale_epoch`.
2. **Fold staleness into `is_eligible`; delete the `require!`.** An untallied
   `joined` gateway is treated exactly like a leaver: traversed, paid 0.
3. **Option 2 plus stamping `weights_epoch` at `join_network`.** Belt and
   braces — make late joiners explicitly "fresh with zero weights".
4. **Key distribution to an epoch-scoped snapshot** of the registry taken at
   tally time, decoupling the cursor from the live registry entirely.

## Decision

> **Option 2, with staleness split by entitlement.** A gateway that was
> outside this epoch's earning set is traversed and earns 0, exactly as a
> `leaving` gateway already is. A gateway that was *inside* it and had its
> weights destroyed still fails loudly — under a new, distinct error.

```rust
let weights_stale = gateway.weights.weights_epoch != epoch.epoch_index;

// Same test tally_weights uses to force effective_composite = 0 (SHOULD-13),
// hence the same population prescribe_epoch excluded from the joined_count
// divisor. Read off the Gateway account, not the registry slot, so it cannot
// drift from the copy tally consulted.
let outside_earning_set = gateway.start_timestamp > epoch.start_timestamp;

require!(
    is_leaving || !weights_stale || outside_earning_set,
    GarError::EpochWeightsClobbered
);

let is_eligible = registry.gateways[dist_idx].composite_weight > 0 && !weights_stale;
```

**Why staleness must be split, and why not by the direction of the stamp.**
`weights_epoch` is a single shared field and `tally_weights`' only epoch
precondition is `weights_tallied == 0` — no time gate, no ordering gate. So the
stamp can be overwritten in *either* direction: by a later epoch's tally, or by
a belated tally of an older, partially-tallied epoch (an epoch left that way is
permanent — `close_epoch` requires `rewards_distributed != 0`, which an
untallied epoch can never reach). Staleness therefore conflates two
populations:

* **Outside the earning set** — joined after this epoch started, already
  excluded from the `joined_count` divisor by `prescribe_epoch`, owed nothing.
  Skip and pay 0. This is the mainnet 540 case.
* **Inside the earning set, stamp overwritten** — tallied for this epoch and
  baked into `per_gateway_reward`, but its weights are gone. Paying 0 would
  under-allocate the pool, permanently forfeit that gateway's *and its
  delegates'* rewards, and still set `rewards_distributed = 1` while emitting
  `EpochDistributedEvent` — indistinguishable from success and unrecoverable,
  since `RewardsAlreadyDistributed` blocks any retry.

An earlier revision of this ADR discriminated on `weights_epoch > epoch_index`.
That is wrong: it misses the downward re-stamp, which reaches the second
population just as effectively. Testing entitlement directly, via state a
belated tally cannot forge, covers both directions.

This satisfies the drivers better than the alternatives:

* **Liveness.** No single gateway account can abort a batch. The only remaining
  revert paths are genuine account-validity failures.
* **No untallied payouts.** Staleness now *directly* gates eligibility rather
  than relying on `composite_weight` being 0 as a side effect. This is strictly
  stronger than the status quo: it also covers the case the `require!` was
  really written for — a gateway that missed tally while carrying a **nonzero**
  `composite_weight` from a previous epoch, which today reverts the batch and
  afterwards would simply earn 0.
* **Rollout cost.** Body-only change to one instruction. No account-layout
  change, no instruction-ABI change, no new accounts, no new events. Measured by
  rebuilding and diffing the IDL: instructions, accounts, events and types
  identical; `distribute_epoch`'s account list and args identical; **one
  additive error variant** (`EpochWeightsClobbered`, code 6097); no errors
  removed; all pre-existing error codes unchanged; `idl-event-snapshot.mjs`
  stable. Diagnostics otherwise go to `msg!` to keep the surface this small.
  _(An earlier revision of this ADR claimed the IDL was "byte-identical". That
  was true of the first draft and became false when the guard below was added;
  corrected here rather than left to be discovered during Phase 2.)_
* **Single-writer contract.** `weights_epoch` keeps one writer and one reader.

**Option 1** is rejected: it makes protocol liveness contingent on an
unaffiliated third party, and its only self-service remedy —
`admin_close_stale_epoch` — is destructive (it bypasses the M8 gate, orphaning
every Observation PDA for the epoch and stranding their rent permanently) and
disappears at `finalize_migration`.

**Option 3 is rejected, and specifically because it is harmful.** Stamping
`weights_epoch` at join adds a second writer to a field whose whole contract is
"written by `tally_weights`", and makes a never-tallied gateway report as
*fresh*. Under option 2 that inverts the fix: `weights_stale` becomes `false`,
so `is_eligible` collapses back to `composite_weight > 0`. It is safe today only
because `join_network` happens to write `composite_weight: 0` — two unrelated
code paths agreeing by coincidence, with no stated invariant binding them. Any
future change that seeded a nonzero weight at join would silently begin paying
untallied gateways. The combination is strictly worse than option 2 alone.

**Option 4** is the correct long-term design and is deferred, not dismissed.
The live-registry/frozen-count split is the root cause of *both* defects in this
incident and of the `save_observations` `gatewayCount` defect fixed in SDK
4.2.0. It is a larger change touching account layout and the client contract,
so it should not gate a liveness hotfix. It gets its own ADR.

**Reopening trigger:** if option 4 lands, the staleness term here becomes
redundant (a snapshot cursor cannot address an untallied gateway) and should be
revisited rather than carried forward by inertia.

## Consequences

### Positive

* A mid-epoch join can no longer halt distribution. The incident class is
  closed at the protocol level rather than by operator cooperation.
* Strictly stronger than the status quo against untallied payouts, including the
  nonzero-stale-weight case the original guard targeted.
* Ships without a migration, a client republish, or a cutover window.

### Negative / risks

* **`WeightsNotTallied (6048)` stops being raised by `distribute_epoch`.** The
  variant is *not* retired — `prescribe_epoch` still raises it
  ([`epoch.rs:916`](../../programs/ario-gar/src/instructions/epoch.rs),
  `require!(epoch.weights_tallied != 0)`), which is a different and legitimate
  condition (prescribing before tally). Tooling that keys off 6048 should be
  checked for an assumption that it came from distribution; the operator
  advisory should say so.
* **A gateway in this state silently earns 0 for one epoch** instead of loudly
  failing. That is the intended trade, but it is a silent economic outcome, so
  it must be surfaced via `msg!` and should be documented for operators: *join
  before an epoch's tally, or expect no reward for that epoch.*
* Does **not** fix the `active_gateway_count` / `registry.count` divergence.
  Distribution can still stall at the tail if a client supplies accounts only up
  to the live registry count. Tracked separately.

### Neutral

* CU cost is unchanged — one `u64` comparison replaces a `require!` on the same
  comparison.
* The `is_leaving` exemption is now a special case of a general rule
  (stale ⇒ ineligible). The separate `is_leaving` branch is retained because it
  also suppresses the reward calculation itself.

## Implementation notes

* Change is confined to
  `programs/ario-gar/src/instructions/distribution.rs`; update the stale comment
  above `is_eligible` (the "its weights ARE fresh" assumption) in the same diff.
* Tests to add in `programs/ario-gar/tests/integration.rs`:
  1. `test_distribute_epoch_untallied_joiner_does_not_block` — join after
     `weights_tallied == 1` → `distribute_epoch` completes; the joiner's
     `operator_stake` is unchanged and its stats do not tick; the tallied
     gateway is still paid; the cursor reaches `active_gateway_count` and
     `rewards_distributed` flips.
  2. `test_distribute_epoch_stale_weights_with_nonzero_composite_earns_zero` —
     `joined` + stale `weights_epoch` + **nonzero** `composite_weight` → earns 0
     (the case the old `require!` existed for).
  3. `leaving` + stale weights → already covered by the existing
     `test_distribute_epoch_leaving_gateway_zero_rewards`.

  Both new tests must be verified to **fail on the pre-fix program** with
  `Custom(6048)`, not merely to pass on the fixed one. Placing a
  joined-but-untallied gateway *inside* `[0, active_gateway_count)` requires a
  slot below the count to be freed after tally — the tests use
  `leave_network` + `finalize_gone`, mirroring the three removals that relocated
  `lazygiraffe.io` to index 622 on mainnet.
* Assert **zero IDL drift**: `node scripts/idl-event-snapshot.mjs` must pass
  without `--update`.
* Staging must **reproduce the failure on the pre-fix program first**, then
  demonstrate the fix — per `MAINNET_UPGRADE_ROLLOUT_PLAN.md` Phase 2. Do not
  compress `epoch_duration` while an independent cranker is running (§6.3).
* Rollout tracked in `solana-ar-io/docs/EPOCH_540_DISTRIBUTION_DEADLOCK_PLAN.md`.

## Related

* Code: `programs/ario-gar/src/instructions/distribution.rs` (:139, :168,
  :176-180, :209, :475), `programs/ario-gar/src/instructions/epoch.rs` (:776),
  `programs/ario-gar/src/instructions/gateway.rs` (:118),
  `programs/ario-gar/src/state/mod.rs` (:548)
* Prior art: [ADR-025](0025-delegate-share-keyed-off-tally-snapshot.md) — same
  class of defect (live state read at distribution time vs. the tally snapshot),
  fixed there with `delegated_at_tally`.
* [ADR-029](0029-epoch-rent-refunds-creator.md) — the epoch whose distribution
  is blocked also strands its `EpochRentReceipt`.
* Behavioral diff entry: [BD-115](../BEHAVIORAL_DIFFERENCES.md)
* Incident report: vilenarios.com gateway operator, 2026-09-11 14:45Z
* Staging precedent: `solana-ar-io/docs/EPOCH_RENT_TO_CREATOR_PLAN.md` §6.3
