# ADR-0034: An Unfinished Epoch Must Not Be Superseded

* **Status:** proposed
* **Date:** 2026-09-15 (decision resolved 2026-09-16)
* **Deciders:** @vilenarios
* **Consulted:** mainnet incident review (epochs 540, 542, 543); observer-operator
  report on epoch 542
* **Informed:** gateway operators running crankers/observers

> **TL;DR:** `create_epoch(N+1)` is refused unless epoch N is **finished**:
> `rewards_distributed == 1`, or its account no longer exists (written off). The
> write-off path, `admin_close_stale_epoch`, becomes permanent and gains its own
> guard (ended + undistributed). Clients ship first; the program change follows.
> Companion: [ADR-0036](0036-registry-positions-frozen-while-epoch-unfinished.md)
> uses the same predicate to stop registry reordering mid-epoch.

## Context and problem statement

`weights_epoch` and `GatewaySlot.composite_weight` are **per-tally, not
per-epoch**: single shared fields on each Gateway, re-stamped by whichever epoch
was tallied most recently. An epoch's payout therefore depends on state that the
*next* epoch's tally overwrites.

[ADR-0033](0033-epoch-weights-are-destroyed-by-the-next-tally.md) closed one
direction of this — tallying an epoch *older* than the live one. The opposite
direction is still open, and it is the one that has actually cost money:

> While epoch N is unfinished, anyone may `create_epoch` (N+1) and then
> `tally_weights` (N+1). That tally re-stamps every gateway in N's undistributed
> range, and N can never be paid correctly again.

Both instructions are permissionless (their only signer is `payer`), and neither
is gated on N's state:

| instruction | its only preconditions today |
|---|---|
| `create_epoch` | `epoch_settings.enabled`, `disable_at` not reached, `clock >= epoch_start` |
| `tally_weights` | `weights_tallied == 0`, epoch is the live one (ADR-0033) |

**This is not hypothetical.** Mainnet epoch **540** stalled, epoch 541 was created
and tallied while it sat, and 540's remaining rewards became unrecoverable — two
gateways in its reward set, written off on 2026-09-15 via
`admin_close_stale_epoch`. Mainnet **543** later stalled for 3h20m in the same
shape and survived only because every client in play happened to wait for
distribution before creating 544.

### What currently prevents it is client convention, not the program

The SDK's `crankEpochStep` creates the next epoch only after the live one reports
`rewards_distributed == 1`, and since ar-io-sdk#726 a stalled cursor throws and
stops the tick *before* `createEpoch`. On 2026-09-16, epoch 545's last
distribute batch landed at 00:32:16 UTC and the reference cranker created 546 at
00:55:17. That is the correct
behaviour — and it is a convention held in client code, in a system whose whole
premise is that these instructions are permissionless. At least one operator
already runs a patched fork of the SDK; a fork that advances differently is
enough to destroy an epoch.

### ADR-0032 narrows the exposure but does not close it

ADR-0032 removed the *mid-epoch-joiner* stall, and SDK #726 removes the
*zeroed-slot* stall. Fewer stalls means fewer windows. But any future stall — a
cranker outage, an RPC failure mid-distribution, an unforeseen revert — reopens
exactly the same window, and the loss is permanent and silent.

## Decision drivers

* **The epoch lifecycle is a strict chain.** `prescribe_epoch` requires
  `weights_tallied != 0`; `save_observations` requires `prescriptions_done != 0`
  and `clock < end_timestamp`; `distribute_epoch` requires
  `prescriptions_done != 0`, `clock >= end_timestamp` and
  `rewards_distributed == 0`. Blocking any link blocks everything after it.
* **A written-off epoch must not freeze the network.** A naive "previous epoch
  must be distributed" gate would have frozen mainnet from 2026-09-11 until 540
  was closed on 2026-09-15. Any gate must treat *written-off* as finished.
* **The lifecycle instructions must stay permissionless.** Recovery from a
  genuinely stuck epoch may use the admin authority — it is an exceptional path,
  not the hot path.
* **Recovery must not expire.** Whatever unblocks a stuck network must keep
  working after `finalize_migration`.
* **Bounded compute**, and no per-gateway work added to `create_epoch`.

### No option is invisible to old clients — but the rollout can be client-first

Neither `create_epoch` nor `tally_weights` receives the previous epoch's
account, and `distribute_epoch` holds `epoch_settings` **read-only**, so every
gate needs an extra account or a writability change. An un-upgraded cranker will
therefore be refused once the program enforces the gate — which is the point.
But `create_epoch` reads only the *first* remaining account (the ADR-029 rent
receipt). So if the previous Epoch is appended **after** the receipt, a *new*
client is also accepted by the *old* program. That lets clients ship first and
removes the flag day.

## Considered options

1. **Do nothing.** Keep relying on client ordering.
2. **Gate `create_epoch` on the previous Epoch account** — it must be finished
   (`rewards_distributed == 1`) or **absent** (written off).
3. **Gate `create_epoch` on a marker in `EpochSettings`** — e.g.
   `last_finished_epoch_index`, advanced by `distribute_epoch` on completion and
   by the write-off. Needs `epoch_settings` writable in `distribute_epoch` and a
   schema migration.
4. **Gate `tally_weights(N+1)` instead**, on N being finished. It permits the
   next epoch to be created but stops the destructive step.
5. **Make weights per-epoch** — snapshot composite weights into the Epoch
   account at tally instead of onto the Gateway/registry slot. Removes the class
   entirely rather than ordering around it.
6. **A permissionless write-off** of a stuck epoch after a grace period, instead
   of relying on the admin path.

## Decision

**Option 2, with a permanent, guarded admin write-off. Option 5 remains the
structural end-state.**

### Why creation, not tally (option 4 rejected)

The two fail in opposite directions, and the lifecycle chain decides it.

Under option 4, epoch N+1 exists but cannot be tallied while N is stuck. Because
prescription requires a tally and observation requires prescription, **N+1
collects no observations at all**. Epoch creation is not gated, so once the
schedule reaches N+2, N+2 is created too — and N+1 is no longer the live epoch.
ADR-0033 then forbids tallying it **ever**. It can never be prescribed or
distributed, and its payout is forfeited (the tokens stay in the treasury; the
period simply goes unpaid). Every epoch created during the stall becomes a
further write-off. That is the 540 failure mode reproduced systematically, and
silently.

Under option 2, nothing is created while N is stuck. The stop is loud, N's
weights stay intact because nothing tallies over them, and N remains
distributable by anyone. Recovery is completing N's distribution — which is
permissionless — or writing N off.

With option 2 in place, `EpochWeightsClobbered` (6097) becomes unreachable in
normal operation: the only tally that could overwrite N's weights is N+1's, and
N+1 cannot exist until N is finished.

Option 3 is rejected as before: a second writer of `EpochSettings` on the hot
distribution path, a schema migration, and a marker that can drift from
reality, whereas an account either exists or does not.

### What "finished" means

**Finished = `rewards_distributed == 1`, or the Epoch account does not exist.**
Whether the epoch's Observation PDAs have been closed is **not** part of it:

* `close_observation` is permissionless but requires `rewards_distributed != 0`,
  so observations can only ever be closed *after* distribution.
* Once `rewards_distributed == 1` the incentive protocol has done its job:
  `failure_counts` were consumed and every payout was made. Closing the
  Observation PDAs afterwards is rent reclamation with no effect on any payout.
* Requiring it would add up to 50 extra transactions between distribution and
  the next epoch's creation, for no correctness gain.

The asymmetry with `close_epoch` is deliberate and stays: `close_epoch` still
requires `observations_closed == observations_submitted`, because it destroys
the account that holds `failure_counts` and `has_observed`. Gating *advancement*
on a rent sweep is a different — and unnecessary — requirement.

### The write-off path becomes permanent and guarded

Option 2 makes `admin_close_stale_epoch` the release valve for a stuck network.
Today it is unfit for that role:

* It is gated on `GatewaySettings.migration_active`, so it goes inert when
  `finalize_migration` runs. With option 2 deployed, that would leave a stuck
  epoch with **no** recovery path — a permanent halt.
* Its handler body is empty, and its only constraints are the authority check
  and `migration_active`. It will close **any** epoch named — including the live
  one mid-observation, or a distributed one before its observations are closed.

Decision:

* **Remove the `migration_active` constraint.** Its sibling
  `admin_close_orphaned_epoch_rent_receipt` is already deliberately permanent
  for the same reason, and ADR-0031 keeps `EpochSettings.authority` usable for
  the other epoch admin instructions indefinitely.
* **Add guards:** the epoch must have ended (`clock >= end_timestamp`) and must
  be undistributed (`rewards_distributed == 0`). A distributed epoch goes
  through `close_epoch`, which refunds the creator; a live one must not be
  closeable at all.
* **No on-chain grace period.** The authority already holds strictly greater
  power (the program upgrade key), so a grace period would not constrain a
  malicious operator; the two guards above remove the two costly mistakes. The
  remaining one — writing off an epoch whose distribution is still advancing —
  is an operational check: confirm `distribution_index` has stopped moving before
  writing off. Distribution can legitimately run for hours — epoch 542's
  distribute transactions spanned 00:04–11:39 UTC on 2026-09-13.

**Option 6 is rejected.** A permissionless write-off needs a grace period long
enough that a slow-but-progressing distribution is never killed; any third party
could otherwise forfeit a day of payouts for the whole network the moment the
grace period lapses during a cranker outage. It also adds an instruction.
Keeping the write-off with the transferable epoch authority (ADR-0031, e.g. a
Squads vault) avoids both.

## Consequences

### Positive

* The 540 loss mode becomes unrepresentable rather than merely unlikely, and
  `EpochWeightsClobbered` stops being reachable in normal operation.
* The protection stops depending on every client — including forks — behaving.
* The admin write-off can no longer destroy a live epoch or bypass `close_epoch`.
* The gate creates a well-defined interval — *N distributed, N+1 not yet
  created* — in which no epoch snapshot is in use. ADR-0036 relies on it.

### Negative / risks

* **Un-upgraded crankers are refused** once the program enforces the gate. The
  client-first rollout below makes this a non-event for anyone who upgrades.
* **A stuck epoch now stops the network until it is distributed or written
  off.** That converts a silent loss into a visible, admin-resolved stop — the
  intended trade. It makes the epoch authority's availability part of liveness,
  which is one more reason to move it to the Squads vault (ADR-0031).
* **Catch-up after a stop.** Epoch start times are schedule-based
  (`genesis_timestamp + index × epoch_duration`), not "now". After a stop longer
  than one epoch, the next epochs are created back-to-back with windows that have
  partly or fully elapsed; an epoch whose window has fully elapsed can be tallied
  and prescribed but accepts no observations. This already happens today after a
  cranker outage — the gate does not introduce it — but it becomes the recovery
  shape.

  **What those epochs pay.** `save_observations` requires
  `clock < end_timestamp`, and `distribute_epoch` marks a gateway failed only when
  `observations_submitted > 0`. An epoch that accepted no observations therefore
  pays **every** eligible gateway, including ones that were down during the
  outage. Under this ADR every such epoch must be distributed (or written off)
  before the next can be created, so after a k-epoch stop, k unobserved epochs
  pay out in sequence. The reference cranker already behaves this way; the gate
  makes it mandatory. **Open decision before implementation:** accept this, or
  have the crank write off epochs that were created after their own
  `end_timestamp` (the authority's `admin_close_stale_epoch` already allows it,
  and a program-side rule could make it automatic).

### Neutral

* The reference cranker already orders distribute → create, so it gains no
  delay.
* The predicate cannot be sidestepped by moving the epoch counter:
  `admin_set_current_epoch_index` works only while epochs are disabled and
  `current_epoch_index == 0`, so it cannot later skip past an unfinished epoch.
* Option 5 (per-epoch weights) is still the only change that removes the shared
  state entirely, and should be scheduled deliberately.

## Implementation notes

### `create_epoch`

* **`remaining_accounts` is already in use.** ADR-029 reads
  `ctx.remaining_accounts.first()` as the epoch rent receipt, and sets
  `has_rent_receipt` from whether *any* remaining account was passed. The SDK
  always passes the receipt there.
* Ordering contract, for compatibility with the **current** program: the
  receipt stays first; the previous Epoch is appended **after** it. The current
  program reads only `first()`, so it ignores the extra account.
* In the **new** program, identify both accounts **by key**, not by position:
  the receipt is the entry whose key is `PDA(["epoch_rent_receipt", idx])`, and
  `has_rent_receipt` is set from that match rather than from "list non-empty".
  Otherwise a caller that passes the previous Epoch but no receipt would have
  the Epoch misread as a receipt (today that fails with
  `InvalidEpochRentReceipt`, from the key check in `init_epoch_rent_receipt`).
* Let `idx = epoch_settings.current_epoch_index`. If `idx == 0`, no check.
* Otherwise derive `prev = PDA(["epoch", (idx - 1).to_le_bytes()])` and find it
  among `remaining_accounts` by key. Missing → error.
* If `prev.owner == program_id`: load it as `Epoch` (discriminator checked) and
  require `rewards_distributed == 1`.
* Otherwise treat it as **absent**. Only this program can create an account at
  its own PDA, so a non-program-owned account at that address — including a
  system account someone sent lamports to — cannot be an Epoch.
* Implement the check once and share it with `finalize_gone` (ADR-0036): one
  error appended at the end of `GarError` (ADR-035), e.g.
  `LatestEpochUnfinished`, blessed into `error-code-snapshots.json`.

### `admin_close_stale_epoch`

* Drop `constraint = settings.migration_active` (and the `settings` account if
  nothing else needs it — an account-list change, so this ships with the client
  release).
* Handler: `require!(clock >= epoch.end_timestamp)` and
  `require!(epoch.rewards_distributed == 0)`.
* Unchanged: it still orphans the ADR-029 rent receipt, collected by
  `admin_close_orphaned_epoch_rent_receipt`; and the epoch's Observation PDAs
  remain unclosable afterwards (`close_observation` needs the Epoch account).
  Both are pre-existing — epoch 540 left 23 stranded Observation PDAs.

### Rollout (client-first)

1. SDK: on `create_epoch`, keep the receipt as the first remaining account and
   append the previous Epoch PDA after it. The current program reads only the
   first entry and ignores the rest.
2. Release; cranker and observer pin it; operators upgrade.
3. Upgrade the program. Un-upgraded crankers now get the new error on
   `create_epoch` and nothing else changes for them.

### Tests

* `create_epoch(N+1)` refused while N is undistributed; accepted once N is
  distributed; accepted when N has been written off; `idx == 0` needs no account.
* Refused when the previous-epoch account is missing, or when a different
  account is supplied in its place.
* `admin_close_stale_epoch`: refused for a live epoch and for a distributed
  epoch; accepted for an ended, undistributed one; still works with
  `migration_active == false`.
* End to end: stall N → N+1 refused → write N off → N+1 created, tallied,
  prescribed, distributed.
* Staging: both client orders (new client + old program, new client + new
  program) before the mainnet upgrade.

## Related

* [ADR-0036](0036-registry-positions-frozen-while-epoch-unfinished.md) — the
  companion registry-ordering rule, which depends on this gate
* [ADR-0033](0033-epoch-weights-are-destroyed-by-the-next-tally.md) — closed the
  belated-tally direction of the same shared-state hazard
* [ADR-0032](0032-distribution-skips-untallied-gateways.md) — removed the
  mid-epoch-joiner stall trigger
* [ADR-0031](0031-transferable-epoch-settings-authority.md) — makes the write-off
  authority transferable to a multisig
* [ADR-029](0029-epoch-rent-refunds-creator.md) — epoch rent receipts, which the
  write-off orphans
* [ADR-035](0035-anchor-error-codes-are-append-only.md) — the new error variant
  must be appended
* ar-io-sdk#726 — the client-side stall fix that removes the most common window

## Addendum — 2026-09-17: an epoch with no observations pays nothing and credits nothing

*Appended after merge; the body above is unchanged. This resolves the open
decision left in "Catch-up after a stop".*

**Decision (2026-09-17): `distribute_epoch` short-circuits when
`observations_submitted == 0`.** It makes no payout, writes no gateway stats,
and marks the epoch distributed so the chain advances. The period's reward share
stays in the treasury.

### Why

The open question was framed as "does a catch-up epoch pay everyone?", and a
second consequence decided it. `distribute_epoch` does not only pay: per
gateway it also increments `stats.total_epochs`, and for any gateway it does not
mark failed it increments `passed_epochs` and `passed_consecutive` **and resets
`failed_consecutive` to 0**. A gateway is only marked failed when
`observations_submitted > 0`.

So under the previous behaviour an epoch that accepted no observations would
have recorded a **pass for every gateway**, which:

* wipes the failure streak of a gateway heading for the 30-consecutive-failure
  prune, deferring or cancelling its removal; and
* lifts its epoch pass rate, which gates ArNS operator-discount eligibility at
  90% (`try_apply_gateway_discount`).

An outage would have laundered the record of exactly the gateways the incentive
protocol exists to catch, on top of paying them. Skipping the epoch entirely
costs the honest operators that period's rewards — the tokens stay in the
treasury and fund later epochs — and that is the lesser harm.

The rule also keeps the ADR's own properties intact:

* **Permissionless.** Recovery does not touch `EpochSettings.authority`, so
  liveness does not depend on an admin key — which matters on staging, where
  that authority is a 2-of-4 Squads vault (ADR-0031).
* **Cheap.** A backlog epoch becomes `create_epoch` + one `distribute_epoch`
  call, instead of ~36 tally transactions and ~50 distribute batches. After a
  multi-day stop that is the difference between hours of cranking and minutes.
* **Consistent.** ADR-0032 and ADR-0033 already accept that a period which
  cannot be paid correctly simply goes unpaid; mainnet epoch 540 is that case.

### Consequence to accept

A **live** epoch in which every prescribed observer failed to submit also pays
nobody and credits nobody. That is the same evidence vacuum as a catch-up epoch,
and the alternative — paying blind and crediting a pass to everyone — is worse.
It is a real behaviour change, not only a catch-up rule.

### Implementation notes

* Trigger on `observations_submitted == 0` at `distribute_epoch`, after the
  existing `clock >= end_timestamp` check. Do not add a separate instruction and
  do not depend on how late the epoch was created: the evidence vacuum is the
  condition, not the schedule.
* Set `rewards_distributed = 1` and advance `distribution_index` to
  `active_gateway_count` so the epoch reads as complete and satisfies this ADR's
  predicate and ADR-0036's.
* **Events — emit both, in this order.** `EpochDistributedEvent` cannot carry a
  discriminator: its shape is frozen (ADR-018), and a normal distribution can
  legitimately emit `gateways_processed: 0, total_eligible_rewards: 0` when no
  gateway was eligible, so zero totals do not identify the skip.
  1. A **new** `EpochSkippedNoObservationsEvent { epoch_index: u64,
     active_gateway_count: u32, timestamp: i64 }`, appended to the event surface
     (ADR-018 allows new events; bless it into `idl-event-snapshots.json` and
     document it in `docs/EVENTS.md`). This is the stable discriminator.
  2. Then `EpochDistributedEvent { epoch_index, gateways_processed: 0,
     total_eligible_rewards: 0, timestamp }`, unchanged in shape, so every
     existing consumer that tracks "this epoch finished" keeps working without
     an upgrade.

  An indexer that wants to tell the two apart keys on the presence of the new
  event in the same transaction; one that only needs "finished" ignores it. Do
  not skip the `EpochDistributedEvent`: it is what the cranker, the observer and
  the SDK already watch to advance.
* Touch no `Gateway.stats`, no `cumulative_reward_per_token`, and no treasury
  transfer.
* `close_epoch` still works afterwards: it requires
  `observations_closed == observations_submitted`, which holds trivially at 0.
* Tests: an ended epoch with zero observations pays nothing, leaves
  `failed_consecutive` intact (the laundering case), leaves pass rates intact,
  emits both events, and still satisfies `create_epoch`'s gate; one epoch with a
  single observation still distributes normally; and a normal distribution whose
  eligible set is empty emits `EpochDistributedEvent` with zero totals and **no**
  skip event, which is the case the discriminator exists to separate.
