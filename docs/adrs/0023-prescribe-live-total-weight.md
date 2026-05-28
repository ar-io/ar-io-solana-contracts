# ADR-023: `prescribe_epoch` Roulette Modulus Is the Live Registry Sum

- Status: accepted
- Date: 2026-05-28
- Deciders: protocol engineering
- Related: BD-102 (in-place leave/prune preserves registry indices —
  this ADR fixes the bias that the in-place model accidentally opened),
  BD-108 (this ADR's behavioral note), audit reference NEW-2 (which
  added the now-removed fallback)

## Context

`ario-gar` runs the epoch lifecycle as a sequence of permissionless
cranks:

1. `create_epoch` — snapshots `active_gateway_count = registry.count`
   and derives `epoch.hashchain` from the slot/timestamp/epoch_index.
2. `tally_weights` — walks every Joined gateway, caches each slot's
   `composite_weight` in the registry, and accumulates
   `epoch.total_composite_weight` (Joined slots only — Leaving slots
   and late-joiners cache zero and contribute nothing).
3. `prescribe_epoch` — runs a weighted-roulette observer selection:
   `random_value = hash % epoch.total_composite_weight`, then walks
   `registry.gateways[..active_count]` until `cumulative > random_value`.

Between (2) and (3), a Joined gateway may call `leave_network` (its own
operator, signature-gated) or be subject to permissionless
`prune_gateway` (any caller, once the target's `failed_consecutive`
crosses `max_consecutive_failures`). Both paths flip the gateway's
slot to `STATUS_LEAVING` and zero its `composite_weight` **in place**
to preserve registry indices mid-epoch (BD-102 / audit H2 / H3) — this
keeps `failure_counts[i]` and observer bitmaps stable for the rest of
the epoch.

The in-place design was correct for stability but had a hidden
coupling: `prescribe_epoch`'s modulus came from
`epoch.total_composite_weight`, the tally snapshot, which `leave_network`
and `prune_gateway` did **not** update (no `epoch` account on those
ix's; no `sub_composite_weight` on `Epoch`). When the leaver's weight
was meaningful, the stale modulus opened a "dead range" in the
random_value space the inner walk could not cover — the cumulative
sum across `[..active_count]` topped out at `live_total < stale_total`.

A previous audit (NEW-2) added a fallback for the "no slot matched"
case: walk backward from `active_count` and select the last non-zero
slot. The intent was defensive (don't select a leaver or
`Pubkey::default()` placeholder). The unintended effect was to
collapse the entire dead range onto whichever single gateway sat at
the highest non-zero index.

A Codex security review on 2026-05-28 demonstrated the bias with a
[1, 100, 1] PoC: after the middle gateway leaves, sampling
`random_value % 102` against `[1, 0, 1]` makes the random pointer fall
into the dead `[2, 101]` range 100/102 of the time, the fallback
attributes every such hit to the tail slot, and the tail gateway wins
~101/102 of observer selections instead of the expected ~1/2.

The bias matters because prescribed observers submit `gateway_results`
that drive `failure_counts`, observer/missed-observer rewards, and
`prune_gateway` eligibility — the bug is self-reinforcing across
epochs and is exploitable by any operator who can engineer a
high-weight leave or prune in the tally→prescribe window (including
the cranker themselves, who is permissionless).

## Decision

**Sample observer selection modulo the live sum of current
`registry.gateways[..active_count].composite_weight`, not the
`epoch.total_composite_weight` snapshot. Remove the NEW-2 "last
non-zero slot" fallback.**

In `programs/ario-gar/src/instructions/epoch.rs::prescribe_epoch`:

1. Before the inner hash loop, compute `total_weight` as a
   walk-and-sum over `registry.gateways[..active_count]` using
   `saturating_add` on `u128` (matches the `Epoch` accessor's
   precision; cannot overflow because three `u64`s of weight per slot
   over ≤3000 slots fits comfortably below `u128::MAX`).
2. Use that `total_weight` as the modulus for `random_value`.
3. Delete the `if !found && active_count > 0 { … fallback … }`
   branch — with a live modulus, every `random_value` in
   `[0, total_weight)` lands inside an actual non-zero slot and the
   inner walk always selects.
4. Keep the inner walk's `slot.composite_weight > 0` guard as
   defense-in-depth (rejects any zero-weight slot a future refactor
   might surface; the existing audit NEW-2 invariant).
5. Update the comment in `gateway.rs::leave_network` (mirrored from
   `prune_gateway` by reference) that previously characterized the
   bias as "≪ 1 slot." The new comment frames the stale snapshot as
   intentional (still used by `EpochWeightsTalliedEvent`) and points
   at the prescribe-site recompute as the safety property.

`Epoch.total_composite_weight_lo`/`_hi` and the
`total_composite_weight`/`set_total_composite_weight`/`add_composite_weight`
accessors are retained — the value is still meaningful as a tally
snapshot (it's what `EpochWeightsTalliedEvent` reports), it's just no
longer load-bearing for selection. `set_total_composite_weight(0)` in
`create_epoch` is unchanged.

## Rejected alternatives

- **Decrement on leave/prune** (`leave_network`/`prune_gateway` take
  the active `epoch` account as mut, call a new `sub_composite_weight`
  helper). Possible but more invasive: adds an account to two
  instruction contexts (and to every cranker/SDK call site), introduces
  a "which epoch is the current one?" race between operators and the
  cranker, and requires idempotency guarding against double-subtraction
  on retried txs. Doesn't help in-flight `tally_weights` (gated by
  `weights_tallied == 0`, but in-batch state mid-tally is still a
  consideration if the leave races a partial tally — the live recompute
  is simply correct regardless). **No advantage over option 1 and more
  surface area to get wrong.**
- **Reject leave/prune during the tally→prescribe window.** Adds a
  liveness hazard: a cranker that crashes between tally and prescribe
  would temporarily lock out every operator from leaving. Operators
  shouldn't pay a UX cost for cranker reliability.
- **Recompute the snapshot inside `prescribe_epoch` and write it back
  to `epoch.total_composite_weight`.** Tempting because it keeps the
  field meaningful as "the modulus that was used." Rejected because
  it would change the semantics that `EpochWeightsTalliedEvent` reports
  (tally-time vs prescribe-time), forcing indexer consumers to handle
  two distinct meanings of "the total" depending on which event fired
  most recently. Keeping `total_composite_weight` as a strict
  tally-time snapshot and recomputing the modulus inline is cleaner.

## Consequences

- **One intended behavior change**: observer selection probability per
  gateway is now `slot.composite_weight / sum(live composite_weights)`
  — strictly correct given the in-place leave model. Pre-fix it was
  `slot.composite_weight / epoch.total_composite_weight` with the
  fallback collapsing the dead range onto the tail slot. Documented
  as BD-108.
- **No IDL or event-shape changes.** `Epoch.total_composite_weight_lo`
  and `_hi` keep their byte offsets; `EpochWeightsTalliedEvent` still
  reports the tally snapshot; no new instructions; no new errors;
  `LeaveNetwork`/`PruneGateway` account lists are unchanged. The TS
  client at `clients/ts/` does not need regeneration. Indexer impact
  is documented in BD-108.
- **CU cost**: O(active_count) `u128::saturating_add` calls before the
  roulette loop. At the production cap of 3000 active gateways this is
  ~3000 additions — well under the 1M CU budget given a single CU per
  add. The benchmarked `cu-baseline.sh` for `prescribe_epoch` will
  shift up by a small constant; bless on the next baseline pass.
- **Test coverage**:
  - 7 unit tests in `epoch.rs::prescribe_roulette_math` that
    exhaustively cover the random_value space against synthetic
    registries (including a "pre_fix_dead_range_matches_codex_poc"
    test that locks down the PoC arithmetic so future readers can see
    *why* the fallback was load-bearing on the bug).
  - 1 integration test `test_prescribe_unbiased_after_mid_epoch_leave`
    in `tests/integration.rs` that exercises the full path: three
    gateways with stake ratio [1:100:1], tally, leave the middle one,
    prescribe, assert both surviving gateways are selected
    (`observer_count == 2`) and the leaver is excluded.
- **Reversibility**: the change is fully internal to `prescribe_epoch`
  and uses no new on-chain state. A revert is one file's worth of
  diff; no migration required.
