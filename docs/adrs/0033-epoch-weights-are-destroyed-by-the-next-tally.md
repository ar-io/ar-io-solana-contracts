# ADR-0033: An Epoch's Weights Survive Only Until the Next Tally

* **Status:** proposed
* **Date:** 2026-09-11
* **Deciders:** @vilenarios
* **Consulted:** security review of [ADR-0032](0032-distribution-skips-untallied-gateways.md)
* **Informed:** AR.IO gateway operators, cranker/observer maintainers

> **TL;DR:** `weights_epoch` and the registry's `composite_weight` are both
> per-*tally*, not per-*epoch*, and `tally_weights` has no ordering gate — so
> tallying epoch N+1 destroys undistributed epoch N's weights, which is ordinary
> cranker behaviour and has already cost ~3,487 ARIO on mainnet. Close the
> adversarial half with a cheap tally window gate, handle the operational half
> with cranker ordering, and defer the structural fix.

## Context and problem statement

`distribute_epoch` computes each gateway's payout from two pieces of state
written by `tally_weights`:

* `Gateway.weights.weights_epoch` — the freshness stamp
  ([`epoch.rs:776`](../../programs/ario-gar/src/instructions/epoch.rs))
* `GatewaySlot.composite_weight` — the weight itself
  ([`epoch.rs:753`](../../programs/ario-gar/src/instructions/epoch.rs))

**Both are single-valued per gateway, not per epoch.** Every `tally_weights`
run overwrites them for whichever epoch it is tallying. And `tally_weights`'
*only* epoch precondition is `require!(epoch.weights_tallied == 0)` — there is
no time gate, and no ordering gate against any other epoch.

`create_epoch` and `tally_weights` are both permissionless.

So the invariant the design silently assumes — *at most one epoch's weights are
live at a time* — is not enforced anywhere. Tallying epoch N+1 while epoch N is
still undistributed destroys N's weights irrecoverably. There is then no correct
payout for N to compute: the numbers it needed are gone.

### This is ordinary operation, not an attack

Mainnet epoch 540, measured with `check-epoch-distributable.mjs`:

```
epoch 540: 34 slots left to cross (615..648)
  payable                          : 0
  leaving (exempt, earn 0)         : 9
  zeroed tail                      : 2
  outside earning set (skippable)  : 1    <- slot 622, lazygiraffe
  IN earning set, weights clobbered: 22   <- weights_epoch=541, real composites
  => UNRECOVERABLE
```

Sequence, with no attacker in it:

1. A mid-epoch join wedged 540's distribution at cursor 615 (ADR-0032's defect).
2. Distribution stayed stuck ~15 h.
3. Crankers did the normal thing and tallied epoch 541.
4. 541's tally re-stamped every gateway in 540's undistributed range.
5. 540's remaining rewards became uncomputable: **22 gateways × ~158.51 ARIO
   ≈ 3,487 ARIO**, plus their delegates' shares. One of the 22 is our own node.

Staging epochs 790/791 reached the same state on 2026-08-29 by a different
route — two crankers racing under a compressed cadence. That write-up called it
*"a pre-existing protocol property"*, which was correct, and concluded it could
not arise on mainnet, which was not.

### A second, adversarial route: the belated tally

Surfaced by the security review of ADR-0032. An epoch left *partially* tallied
is permanent:

* `close_epoch` requires `rewards_distributed != 0`
  ([`epoch.rs:1190`](../../programs/ario-gar/src/instructions/epoch.rs)), which
  an untallied epoch can never reach.
* `admin_close_stale_epoch` is authority-gated **and** `migration_active`-gated,
  so after `finalize_migration` there is no removal path at all.

Because `tally_weights` has no time gate, that stranded epoch stays tallyable
forever. Anyone can later call `tally_weights` on it and stamp `weights_epoch`
*downward*, onto gateways that are in the live epoch's reward divisor — with
partial batches accepted, so a chosen prefix of the registry can be hit for one
transaction fee. Staging epoch 818 sat at `tally_index 630/647,
weights_tallied = 0` while this was being written, so the precondition arises
routinely from nothing worse than a cranker restart.

The unpaid share is not stolen — it stays in `protocol_token_account` and
socialises into later epochs. The harm is targeted, permanent forfeiture of one
epoch of rewards for a chosen set of gateways **and their delegates**, who have
no recourse.

### Why ADR-0032 is not the fix, but does change the risk

ADR-0032 makes this loud instead of silent: a clobbered epoch now fails with
`EpochWeightsClobbered` rather than completing with a zero payout and an
`EpochDistributedEvent`. That is detection, not prevention.

It also removes the *main trigger*. Step 1 above — a wedged distribution — was
what gave 541's tally time to overtake 540. With ADR-0032 shipped, distribution
does not wedge on a mid-epoch join, so the window in which N+1 can overtake N
shrinks from hours to the minutes a normal batch run takes. That materially
lowers the likelihood while leaving the mechanism intact.

## Decision drivers

* **Operator rewards must not be silently destroyed by routine operation.**
* **Proportionality.** Every contract-level prevention here costs either an
  account-layout migration or a client cutover (see options). The hazard's
  main trigger is already removed by ADR-0032, so the fix should not be more
  disruptive than the risk it retires.
* **Close the deliberate path first.** An adversarial primitive that needs one
  transaction fee deserves a higher priority than a race that requires
  distribution to stall.
* **Don't strand epochs.** A gate that prevents clobbering must not create a
  new class of permanently untallied — hence unclosable — epochs.

## Considered options

1. **Status quo + operational ordering.** Crankers distribute epoch N fully
   before tallying N+1; monitor with `check-epoch-distributable.mjs`.
2. **Tally window gate.** Refuse `tally_weights` once the epoch ended more than
   one `epoch_duration` ago. No layout change, no ABI change.
3. **Distribution watermark.** Append `last_distributed_epoch_index` to
   `EpochSettings`; `distribute_epoch` writes it on completion and
   `tally_weights` refuses to run more than one epoch ahead of it.
4. **Epoch-scoped weight snapshot.** Move the weights out of shared per-gateway
   state into per-epoch storage, so no tally can destroy another epoch's inputs.

## Decision

> **Options 1 + 2 now; option 4 deferred; option 3 rejected.**

**Option 2 is the cheap, correct half.** `tally_weights` already has `epoch`
and the clock in scope, so the gate is a `require!` with no new accounts:

```rust
let epoch_span = epoch.end_timestamp.checked_sub(epoch.start_timestamp)?;
let tally_deadline = epoch.end_timestamp.checked_add(epoch_span)?;
require!(
    clock.unix_timestamp <= tally_deadline,
    GarError::EpochTallyWindowClosed
);
```

**The span comes from the epoch's own timestamps, not from
`epoch_settings.epoch_duration`.** An earlier revision of this ADR specified the
setting, which is wrong: `admin_set_epoch_duration` can change it afterwards and
would retroactively move the window for every epoch created under the old
cadence. Staging compressed the duration to 60 s in Aug 2026 — under a
settings-derived deadline that would have locked in-flight 24 h epochs out of
tally almost immediately. Deriving the span from `end - start` makes the deadline
a property of the epoch itself and immune to later governance changes.
`test_tally_window_uses_the_epochs_own_span_not_current_settings` was verified to
fail against the settings-derived variant, so the distinction is held by a test
rather than by this paragraph.

This makes a stranded partially-tallied epoch **inert** rather than a permanent
weapon, which retires the adversarial route entirely. It cannot strand anything
that was not already stranded: an epoch nobody tallied within a full extra
duration was already unclosable, so the gate removes a capability without
removing a recovery path. One additive error variant; no layout change, no ABI
change — the same rollout class as ADR-0032, and it can ship alongside it.

**Option 1 carries the operational half — and reading the client changed what
it is.** `crankEpochStep` is **already correctly ordered**: it works the live
epoch (`currentIndex - 1`) through tally → prescribe → distribute, and reaches
`createEpoch` only after `rewardsDistributed === 1`. Its own comment says so:
*"Lazy-state maintenance … reached only once the live epoch is fully distributed
(rewardsDistributed === 1 here) … run BEFORE creating the next epoch."* A
conforming crank therefore **cannot** create N+1 while N is undistributed, and
cannot cause this race at all.

So option 1 is not a cranker-ordering change. It is two different things:

* **Gate out-of-band epoch creation.** The race requires someone to create N+1
  outside the crank while N is undistributed. That is exactly how mainnet 540
  was lost: epoch 541 was created by hand to unfreeze a wedged network, and
  541's tally then clobbered 540's weights. Any tool or runbook that creates an
  epoch manually must first establish that the previous epoch is distributed, or
  record the loss as an accepted trade. `check-epoch-distributable.mjs` computes
  exactly that.

* **Do NOT "fix" the missing try/catch on the distribute branch naively.** When
  `distribute_epoch` throws, the exception escapes `crankEpochStep` and the tick
  stops *before* `createEpoch`. That freeze is **load-bearing protection**: it is
  what preserves the pending epoch's weights. Wrapping the branch in a plain
  try/catch — previously proposed as the highest-leverage fix for the mainnet
  freeze — would let every operator's crank fall through to create-next, and the
  following tally would then destroy the stuck epoch's rewards automatically. If
  that branch is ever wrapped, it must **catch and stop**, surfacing the failure,
  never catch and continue. Only an epoch established as unrecoverable should be
  allowed to fall through.

**Option 3 is rejected on cost.** `DistributeEpoch.epoch_settings` is **not**
`mut` ([`distribution.rs`](../../programs/ario-gar/src/instructions/distribution.rs)),
so writing a watermark there requires flipping the account writable — an
IDL-visible change that breaks every client still passing it read-only, i.e. a
lockstep cutover — *plus* an `EpochSettings` layout append and its
grow-then-deserialize migration (ADR-020). That is two disruptive changes to
enforce an ordering that clients we own can simply observe.

**Option 4 is the correct end state and is deferred, not dismissed.** It is the
only option that makes the guarantee structural rather than procedural. But
per-epoch weight storage is a significant layout change: `Epoch` is already
9,408 bytes with `failure_counts: [u16; 3000]`, and per-gateway weights would
add roughly 24 KB and ~0.23 SOL of rent per epoch. Note that carving an epoch
marker out of `GatewaySlot._padding` (as ADR-025 did for `delegated_at_tally`)
would improve *detection* only — the weight itself still lives in a
single-valued field, so it does not solve this.

**Reopening trigger:** if a clobbered epoch occurs again after ADR-0032 and the
cranker ordering discipline are both live, option 4 should be scheduled rather
than re-litigated.

## Consequences

### Positive

* The adversarial belated-tally primitive is closed outright, cheaply.
* Combined with ADR-0032, a clobbered epoch is now both far less likely and
  impossible to mistake for success.
* No migration and no client cutover in this ADR.

### Negative / risks

* **The N+1 race remains possible in principle**, but only through out-of-band
  epoch creation, since `crankEpochStep` cannot produce it. Mitigated
  procedurally, not prevented: anyone who creates an epoch by hand while the
  previous one is undistributed still destroys that epoch's weights — without
  profit, but with loss to others.
* **The protective freeze is preserved by doing nothing**, which is an uneasy
  place to leave it: the network stalling on an undistributed epoch is what
  currently keeps that epoch's rewards alive, and someone reading the missing
  try/catch as a plain bug will remove that protection in good faith. This is
  why it is written down here rather than left to code review.
* **`EpochTallyWindowClosed` is a new way for a tally to fail.** A cranker fleet
  offline for more than one full epoch duration will find the missed epoch
  permanently untallied — hence never prescribed, distributed or closed by any
  permissionless path. That epoch was already effectively lost, but the failure
  mode becomes explicit and needs to be in the operator advisory.
* **A new permanent-loss case, which should be a conscious acceptance.** The
  stranded epoch's rent stays reclaimable through `admin_close_stale_epoch` —
  verified to carry no tally or distribution requirement, only authority plus
  `migration_active`. But that instruction goes inert at `finalize_migration`,
  after which such an epoch's ~0.0664 SOL is stranded for good. The window is
  two full epoch spans, so reaching it requires a total cranker outage longer
  than a day at mainnet cadence; the trade is accepted because the capability
  removed was net-harmful either way.
* Accepting option 1 means the guarantee depends on client behaviour, which is
  exactly the kind of dependency ADR-0032's own decision drivers argue against.
  It is accepted here only because the alternative costs a cutover and the
  window is now small.

### Neutral

* `admin_close_stale_epoch` remains the write-off path for an epoch that reaches
  the clobbered state, including mainnet 540.

## Implementation notes

* Gate in `tally_weights` (`programs/ario-gar/src/instructions/epoch.rs`), plus
  an additive `EpochTallyWindowClosed` variant in `error.rs`.
* Tests: tally inside the window succeeds; tally one second past
  `end_timestamp + epoch_duration` fails; a partially-tallied epoch cannot be
  resumed after the window; and a regression test that the normal
  create → tally → prescribe → distribute → close cycle is unaffected.
* **No cranker/observer ordering change is needed** — `crankEpochStep` already
  enforces it. What is needed is a guard on any out-of-band epoch creation, and
  a comment on the distribute branch recording that its un-caught throw is
  deliberate protection rather than an oversight.
* Operator advisory must cover both new failure modes —
  `EpochWeightsClobbered` (ADR-0032) and `EpochTallyWindowClosed` (here).
* Pre-flight before any distribution:
  `node check-epoch-distributable.mjs --cluster <c> --epoch <n>`.

## Related

* Code: `programs/ario-gar/src/instructions/epoch.rs` (`tally_weights`,
  `close_epoch`, `admin_close_stale_epoch`),
  `programs/ario-gar/src/instructions/distribution.rs`
* [ADR-0032](0032-distribution-skips-untallied-gateways.md) — makes this state
  detectable; its security review surfaced the belated-tally route.
* [ADR-025](0025-delegate-share-keyed-off-tally-snapshot.md) — same class of
  defect (live state read at distribution time vs. the tally snapshot), and the
  precedent for carving a marker out of `GatewaySlot._padding`.
* Incident record: `solana-ar-io/docs/EPOCH_540_DISTRIBUTION_DEADLOCK_PLAN.md`;
  staging precedent in `EPOCH_RENT_TO_CREATOR_PLAN.md` §6.3.
* Behavioral diff entry: [BD-116](../BEHAVIORAL_DIFFERENCES.md)
* Implementation: contracts PR #132 (option 2), stacked on #130
