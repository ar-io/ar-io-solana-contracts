# ADR-0036: Registry Positions Are Frozen While an Epoch Is Unfinished

* **Status:** accepted (2026-09-21; implemented in contracts #149 — ships with ADR-0034, never before it)
* **Date:** 2026-09-16
* **Deciders:** @vilenarios
* **Consulted:** observer-operator report and on-chain audit of mainnet epoch 542;
  independent audit of epochs 542 and 543
* **Informed:** gateway operators running crankers/observers

> **TL;DR:** `finalize_gone` swap-removes a registry slot, which silently changes
> which gateway every later position refers to. Observations and distribution
> both address gateways **by position**, so a removal between an epoch's
> snapshot and its distribution mis-scores gateways — it already paid one
> gateway that all 11 observers failed. `finalize_gone` is refused while the
> latest epoch is unfinished, using the same predicate as
> [ADR-0034](0034-an-unfinished-epoch-must-not-be-superseded.md).

## Context and problem statement

`create_epoch` freezes `Epoch.active_gateway_count = registry.count`. Every
later step of that epoch then addresses gateways **by registry position**,
bounded by that frozen count:

| step | what it does with position `i < active_gateway_count` |
|---|---|
| `tally_weights` | writes `registry.gateways[i].composite_weight`; requires `registry.gateways[i].address == gateway.operator` |
| `prescribe_epoch` | builds prefix sums over `registry.gateways[0..active].composite_weight`, picks observers by index |
| `save_observations` | bit `i` of the 375-byte bitmap increments `epoch.failure_counts[i]` |
| `distribute_epoch` | pays `registry.gateways[i]`, judging it by `epoch.failure_counts[i]` |

`failure_counts` lives in the Epoch and never moves. Registry slots do.

### What moves a slot

Only three instructions change which gateway occupies a position:

| instruction | effect |
|---|---|
| `join_network` | appends at index `count`, then `count += 1` |
| `import_registry_entry_handler` | appends at index `count` (migration-only: `migration_active` + `migration_authority`) |
| `finalize_gone` | **swap-remove**: moves the last slot into the freed index, zeroes the old last slot, `count -= 1` |

`leave_network` and `prune_gateway` mark a slot Leaving **in place** and leave
`count` unchanged. That was a deliberate fix — `leave_network` carries the
rationale:

> keeping registry indices stable mid-epoch means `failure_counts[i]` and
> observer bitmaps continue to refer to the same gateway throughout an epoch.
> The previous swap-remove pattern silently re-attributed failure tallies to
> whichever gateway took over the freed slot (audit H2 / H3, 2026-04).

`finalize_gone` — the permissionless garbage collector for those Leaving slots —
reintroduced exactly that swap-remove. Its only preconditions are that the
gateway is Leaving, its leave window has expired, and it has no delegated stake.
**Nothing checks epoch state.** It is also the only instruction anywhere that
decreases `registry.count`.

The only alignment check in `save_observations` is
`gateway_count == epoch.active_gateway_count` — a count, not an ordering — so a
reorder is invisible to the program.

### What it has cost (mainnet, read from chain)

Epoch 542 (`active_gateway_count` 647, window 2026-09-12 00:04 → 09-13 00:04):

| time (UTC) | event |
|---|---|
| 09-12 13:11 – 15:57 | all 11 observations that landed |
| **09-12 16:19 – 16:40** | **14 × `finalize_gone`**, from 6 distinct fee payers |
| 09-13 00:04 – 11:39 | 542's distribution, against the reordered 633-slot registry |

**Lost observations.** SDK ≥ 4.2.0 detects the reorder and refuses to submit
rather than record verdicts against the wrong gateways. An observer operator
reported their prescribed observer blocked for the rest of the window. Only 11
reports landed for 542, against 19 for 541 and 26–30 for 544–545.

**Mis-scored payouts.** All 11 submitted bitmaps were built against the full
647-slot snapshot (every one reports `gateway_count = 647` and sets bits in
positions 633–646), so the observation data itself was sound. Distribution was
not: it ran after the swaps and read `failure_counts` by live position.
Reconstructing the layouts — replaying all 27 removals forward reproduces
today's registry exactly — 14 gateways were judged at a different position
than the one their observations were recorded against. 13 happened to land on a
count with the same pass/fail outcome. One did not:

* `lasaucisse` (`ar.lasaucisse.be`, Joined, 20,327 ARIO staked): **failed by all
  11 observers** (`failure_counts` = 11 at its snapshot position 643), but
  distributed at position 149, where the departed gateway's count was 2 — so it
  was scored as **passed**. The epoch-542 distribute transaction confirms it
  was processed immediately after positions 147 and 148. Its consecutive-failure
  counter was reset rather than incremented, which also delays pruning it.

Epoch 543 repeated the pattern: 13 more removals on 2026-09-13 14:26–14:32
(during its window), 13 gateways moved, and none changed outcome — 543 received
only a single observation.

**This is not rare.** 27 removals across two consecutive epochs, and 276 of 620
mainnet registry entries are Leaving today. Of those 27 removals, only one came
from the reference cranker; the other 26 came from eight other fee payers.
Client sequencing cannot fix this — the program has to.

## Decision drivers

* **Correctness of payouts**, not only of observation submission. A client-side
  guard can refuse to *submit* a misaligned bitmap; nothing a client does can
  stop `distribute_epoch` reading a moved slot.
* **`finalize_gone` stays permissionless.** It is how Leaving slots and their
  rent get reclaimed.
* **Minimal new surface.** Prefer a precondition over new instructions or new
  state.

## Considered options

1. **Do nothing** — rely on the SDK's refusal to submit.
2. **Refuse `finalize_gone` while the latest epoch is unfinished** (the
   ADR-0034 predicate).
3. **Tombstone, then compact at a safe point** — `finalize_gone` zeroes the slot
   in place without moving anything; a separate permissionless instruction
   compacts the registry only when no epoch is unfinished.
4. **An alignment commitment** — hash the slot → address order at
   `create_epoch`; `save_observations` verifies it.
5. **Address-keyed observations** — submit failed gateway addresses instead of
   positional bits.
6. **Snapshot the slot table into the Epoch** at `create_epoch`.

## Decision

**Option 2.**

`finalize_gone` requires that **no unfinished epoch exists**: either no epoch has
been created yet, or the latest one (`current_epoch_index − 1`) has
`rewards_distributed == 1` or no longer exists. This is exactly ADR-0034's
`create_epoch` precondition, implemented once and shared.

### Why it is sufficient

While the latest epoch E is unfinished, `finalize_gone` — the only instruction
that decreases `registry.count` or moves a slot — cannot run. So
`registry.count >= E.active_gateway_count` throughout, and both appending
instructions write at index `count`, i.e. **at or above** the frozen count,
outside every range E iterates. Positions `0 .. active_gateway_count` therefore
keep the same gateway from E's creation until E is distributed, and every row of
the table above reads the snapshot.

### Why it depends on ADR-0034

The predicate inspects only the **latest** epoch. That is enough only if an
older epoch can never still be unfinished — which is what ADR-0034 guarantees.
Without it, a stuck epoch N could be followed by an N+1 that finishes, reopening
`finalize_gone` while N still depends on positions. **This ADR ships with, or
after, ADR-0034 — never before.**

### When garbage collection can happen

Under ADR-0034, the interval between *N distributed* and *N+1 created* is the
only time no snapshot is in use. On 2026-09-16 that interval was 23 minutes
(545's last distribute batch at 00:32:16 UTC, 546 created at 00:55:17).

A cranker can make its garbage collection race-free by putting `finalize_gone`
instructions in the **same transaction** as the final `distribute_epoch` batch:
instructions execute in order, so they see `rewards_distributed == 1`, and no
`create_epoch` can land between them. Otherwise, running the sweep before
`create_epoch` in the same tick is enough; a sweep that misses the window simply
succeeds in the next one.

### Rejected options

* **Option 1** protects submission only. Epoch 542 shows distribution
  mis-scores gateways even when every submitted bitmap is correct.
* **Option 3** is the fallback if the window proves too narrow in practice (see
  risks). It lets rent be reclaimed at any time and defers only slot reuse, but
  needs a new instruction and a compaction pass that must also rewrite every
  moved gateway's `registry_index`. Not worth it until the simpler rule fails.
* **Option 4** detects a misaligned *submission* but does nothing for
  `distribute_epoch`, which would still read `failure_counts[i]` against the
  live registry.
* **Option 5** has the same gap unless `failure_counts` also become
  address-keyed, and it enlarges `save_observations` transactions.
* **Option 6**: 3,000 slots × 32 bytes = 96,000 bytes, against a 9,400-byte Epoch
  and the 10 KB per-instruction growth limit; the Epoch would need pre-creation
  like the registries. Rejected on cost.

## Consequences

### Positive

* Observation bitmaps and distribution always refer to the gateways that were
  snapshotted. 542-style lost reports and mis-scored payouts both stop.
* The SDK's reorder refusal (`resolveObservationGatewayCount`) stops tripping.
* Invariant is statable and testable: *while an epoch is unfinished, positions
  below its `active_gateway_count` do not change.*

### Negative / risks

* **Garbage collection only runs between epochs.** If a caller creates the next
  epoch before anyone sweeps, reclamation waits a full epoch. Two costs follow:
  the `finalize_gone` rent bounty (`close = caller`) is delayed, and a departed
  operator cannot re-join until then, because `join_network` `init`s the Gateway
  PDA that `finalize_gone` closes. Neither affects payouts. Registry capacity is
  3,000 slots against 620 in use. If starvation is observed in practice, move to
  option 3.
* **`finalize_gone` outside the window now fails** with the shared error.
  Callers — mostly third parties today — must upgrade or will waste fees.
* **No retroactive correction.** Epoch 542's mis-scored payout stands; whether
  to remediate it is a separate decision.

### Neutral

* The SDK's handling of zeroed slots inside the epoch range (the #726 filler
  path) becomes unreachable for epochs created after the upgrade, because the
  count can no longer drop below the snapshot. It stays for mixed-version
  safety.

## Implementation notes

* **One shared check** for `create_epoch` (ADR-0034) and `finalize_gone`, with
  one error variant appended to `GarError` (ADR-035) — e.g.
  `LatestEpochUnfinished` — and blessed into `error-code-snapshots.json`.
* `finalize_gone` already receives `epoch_settings`, so it can derive the latest
  Epoch PDA from `current_epoch_index` without an account-list change.
* **`remaining_accounts` is already in use.** When the freed index is not the
  last slot, `finalize_gone` requires the swapped Gateway PDA at
  `remaining_accounts[0]`; when it is the last slot, it reads nothing. For
  compatibility with the **current** program, clients keep the swapped gateway
  first and append the latest Epoch PDA after it. In the **new** program, find
  both by key rather than by position.
* Absent means "not owned by this program", exactly as in ADR-0034.

### Rollout (client-first, together with ADR-0034)

1. SDK: append the latest Epoch PDA on `finalize_gone` (after the swapped
   gateway), and only attempt `finalize_gone` when the latest epoch is finished.
   The cranker sweeps in the window, ideally bundled with the final distribute
   batch.
2. Release; operators upgrade; announce that `finalize_gone` will become
   window-only.
3. Upgrade the program together with ADR-0034.

### Tests

* `finalize_gone` refused while the latest epoch is undistributed — both the
  swap case and the last-slot case; accepted when it is distributed, when it has
  been written off, and when no epoch exists.
* **Regression for epoch 542:** create an epoch; tally; submit a bitmap that
  fails the gateway in the last slot; attempt to `finalize_gone` a Leaving
  gateway in a lower slot → refused; distribute → the failing gateway is scored
  failed; `finalize_gone` → now accepted. **Negative control:** the same
  sequence without the gate reproduces the mis-score.
* A `join_network` during an unfinished epoch lands at an index at or above
  `active_gateway_count` and is ignored by that epoch's tally, prescription and
  distribution.
* The bundled final-distribute + `finalize_gone` transaction succeeds.

## Related

* [ADR-0034](0034-an-unfinished-epoch-must-not-be-superseded.md) — the shared
  predicate, and the guarantee this rule depends on
* [ADR-035](0035-anchor-error-codes-are-append-only.md) — the new error variant
  must be appended
* Audit H2 / H3 (2026-04) — the original swap-remove finding, fixed for
  `leave_network` but not for `finalize_gone`
* ar-io-sdk ≥ 4.2.0 — client-side refusal to submit a misaligned bitmap
