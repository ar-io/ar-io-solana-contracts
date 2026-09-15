# ADR-0034: An Unfinished Epoch Must Not Be Superseded

* **Status:** proposed
* **Date:** 2026-09-15
* **Deciders:** @vilenarios
* **Consulted:** —
* **Informed:** gateway operators running crankers/observers

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

Both instructions are permissionless, and neither is gated on N's state:

| instruction | its only preconditions today |
|---|---|
| `create_epoch` | `epoch_settings.enabled`, `clock >= epoch_start` |
| `tally_weights` | `weights_tallied == 0`, epoch is the live one (ADR-0033) |

**This is not hypothetical.** Mainnet epoch **540** stalled, epoch 541 was created
and tallied while it sat, and 540's remaining rewards became unrecoverable — two
gateways in its reward set, written off on 2026-09-15 via
`admin_close_stale_epoch`. Mainnet **543** later stalled for 3h20m in the same
shape and survived only because every client in play happened to wait for
distribution before creating 544.

### What currently prevents it is client convention, not the program

`crankEpochStep` creates the next epoch only after the live one reports
`rewards_distributed == 1`, and since ar-io-sdk#726 a stalled cursor throws and
stops the tick *before* `createEpoch`. That is the correct behaviour — and it is
a convention held in client code, in a system whose whole premise is that these
instructions are permissionless. At least one operator already runs a patched
fork of the SDK; a fork that advances differently is enough to destroy an epoch.

### ADR-0032 narrows the exposure but does not close it

ADR-0032 removed the *mid-epoch-joiner* stall, and SDK #726 removes the
*zeroed-slot* stall. Fewer stalls means fewer windows. But any future stall — a
cranker outage, an RPC failure mid-distribution, an unforeseen revert — reopens
exactly the same window, and the loss is permanent and silent.

## Decision drivers

* **A written-off epoch must not freeze the network.** A naive "previous epoch
  must be distributed" gate would have frozen mainnet epoch progression from
  2026-09-11 until 540 was closed on 2026-09-15. Any gate must treat
  *written-off* as finished.
* **The instructions must stay permissionless.** No admin in the hot path.
* **Bounded compute**, and no per-gateway work added to `create_epoch`.
* **Client compatibility is already forfeit** — see below — so the cost to
  weigh is the size of the coordinated rollout, not whether one is needed.

### There is no backward-compatible option

Neither `create_epoch` nor `tally_weights` receives the previous epoch's
account, and `distribute_epoch` holds `epoch_settings` **read-only**. So every
gate below requires either a new account in the instruction's list, or flipping
an existing account to writable. Both break un-upgraded crankers, which supply
the old account list. Any fix here therefore ships with a client release and an
operator announcement.

## Considered options

1. **Do nothing.** Keep relying on client ordering.
2. **Gate `create_epoch` on the previous Epoch account**, passed via
   `remaining_accounts`; the program derives the expected PDA and requires it to
   be either finished (`rewards_distributed == 1`) or **absent** (written off).
3. **Gate `create_epoch` on a marker in `EpochSettings`** — e.g.
   `last_finished_epoch_index`, advanced by `distribute_epoch` on completion and
   by `admin_close_stale_epoch`. Needs `epoch_settings` writable in
   `distribute_epoch` and a schema migration (ADR-020 grow-then-deserialize;
   `migrate_epoch_settings` already exists).
4. **Gate `tally_weights(N)` instead**, on N-1 being finished. Same mechanics,
   later in the sequence: it permits a useless epoch to be created but stops the
   destructive step.
5. **Make weights per-epoch** — snapshot composite weights into the Epoch
   account at tally instead of onto the Gateway/registry slot. Removes the class
   entirely rather than ordering around it. Epoch grows by roughly
   `active_gateway_count × 8` bytes (~5 KB at today's 620), plus a migration and
   changes to tally and distribute.

## Decision

**Proposed: option 2 now, option 5 as the structural end-state.**

Option 2 keeps the check adjacent to the thing being protected — you cannot
supersede an epoch without presenting it — and "absent counts as finished" falls
out naturally, because `admin_close_stale_epoch` closes the account. Option 3
needs a schema migration and a second writer of `EpochSettings` on the hot
distribution path; its marker can also drift from reality, whereas the account
either exists or does not.

Option 5 is the only one that makes the hazard unrepresentable, and it should be
scheduled deliberately rather than bolted onto an incident fix.

**Open questions for the decision:**

* Should `create_epoch` reject, or should `tally_weights` reject (option 4)?
  Rejecting at creation is earlier and clearer; rejecting at tally leaves the
  network able to create epochs during an outage and catch up afterwards.
* Does "finished" include an epoch that is distributed but whose observations
  are still open? (`close_epoch` needs those closed; distribution does not.)

## Consequences

### Positive

* The 540 loss mode becomes unrepresentable rather than merely unlikely.
* The protection stops depending on every client — including forks — behaving.

### Negative / risks

* **Breaks un-upgraded crankers** on the release that ships it. Needs the same
  coordinated rollout just completed for SDK 4.3.1, plus an operator notice.
* A genuinely unrecoverable epoch now **blocks progression until it is written
  off**, converting a silent loss into a visible, admin-gated stop. That is the
  intended trade, but it puts `admin_close_stale_epoch` — currently gated on
  `migration_active`, which `finalize_migration` will switch off — on the
  critical path. That interaction must be resolved before this ships.

### Neutral

* `create_epoch` gains one account; the crank's call site changes.

## Implementation notes

* `admin_close_stale_epoch` has **no precondition of its own** — it will close
  any epoch named, including the live one. If it becomes the release valve for a
  blocked network, it should gain its own guard.
* Staging holds ~112 never-tallied epochs (496–745) from the August cranker
  restarts. Under a creation-time gate those would each have blocked
  progression; they are a useful test corpus for whichever predicate is chosen.

## Related

* [ADR-0032](0032-distribution-skips-untallied-gateways.md) — removed the
  mid-epoch-joiner stall trigger
* [ADR-0033](0033-epoch-weights-are-destroyed-by-the-next-tally.md) — closed the
  belated-tally direction of the same shared-state hazard
* [ADR-029](0029-epoch-rent-refunds-creator.md) — epoch rent receipts, which
  `admin_close_stale_epoch` orphans
* ar-io-sdk#726 — the client-side stall fix that removes the most common window
