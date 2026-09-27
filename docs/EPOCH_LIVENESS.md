# Epoch liveness

Under [ADR-0034](adrs/0034-an-unfinished-epoch-must-not-be-superseded.md),
`create_epoch` refuses to create an epoch until the previous one is
distributed. This file lists every way an epoch can fail to reach
distribution, what each failure does, and how the network recovers. Read it
before changing any `ario-gar` lifecycle instruction.

## How an epoch completes

An epoch moves through the following instructions. Only the first four stand
between one epoch and the next:

1. `create_epoch`: permissionless. Allowed once the epoch's scheduled start has
   passed and the previous epoch has `rewards_distributed == 1`, or its
   account is closed.
2. `tally_weights`: permissionless, in batches. Allowed only for the latest
   created epoch (`current_epoch_index - 1`). There is no time limit: the
   latest epoch stays tallyable until the next one exists, and the next one
   can't exist until this one is distributed.
3. `prescribe_epoch`: permissionless. Needs the tally complete. It has no time
   limit, and the name registry account is optional.
4. `distribute_epoch`: permissionless, in batches. Needs the prescription done
   and the epoch's end time passed.
5. `close_epoch`: refunds rent. It doesn't gate the next epoch.

`save_observations` is accepted only inside the epoch's time window. An epoch
that ends with no observations is marked distributed without paying anyone
(the ADR-0034 addendum), so it doesn't hold up the next epoch.

## Failure modes

The following table lists each way an epoch can fail to reach distribution:

| Failure | Reachable | Effect | Recovery |
|---|---|---|---|
| No observer submits during the epoch | Yes | Distribution skips: no rewards, no stats change, epoch marked distributed | None needed. The next epoch proceeds. |
| No cranker runs, for any length of time (host down, fee payer out of SOL, RPC down) | Yes, the most likely failure | The lifecycle pauses at whatever step it reached. | Run any cranker. The latest epoch is still tallyable and distributable. Epochs created after their window closed collect no observations and skip, then the network catches up one epoch at a time. |
| A cranker runs a client older than `@ar.io/sdk` 4.4.0 | Yes | Its `create_epoch`, `finalize_gone` and `compound_delegation_rewards` fail with 6103 or `AccountNotEnoughKeys`. No state changes. | Another cranker, or update the client. |
| A cranker passes wrong accounts | Client bug | The transaction fails with `InvalidGatewayAccount` or `MissingLatestEpochAccount`. No state changes. | Fix the client and resend. |
| The treasury holds less than the batch owes | Yes | The transfer is capped at the balance and every reward in the batch is scaled by the same ratio. A zero amount skips the transfer. | None needed. |
| `EpochWeightsClobbered` | No, see the following section | Would stop the epoch's distribution | `admin_close_stale_epoch` |
| A registry slot points at a closed gateway | No, see the following section | Would stop the epoch's distribution | `admin_close_stale_epoch` |
| The `ario-core` treasury release fails | Only through an `ario-core` upgrade or admin config change | Distribution batches fail | Revert the change, or `admin_close_stale_epoch` |
| Epochs are turned off (`enabled` false, or `disable_at` passed) | Admin action | No new epochs | Turn epochs back on |
| Any failure this table doesn't anticipate | Unknown | The epoch can't finish | `admin_close_stale_epoch` |

### Why two failures can't be reached

`EpochWeightsClobbered` fires when a gateway that earns in the epoch carries
weights stamped for a different epoch. Only `tally_weights` stamps weights,
and it only runs on the latest epoch, which ADR-0034 keeps from being
superseded before distribution. A gateway leaving at tally time skips the
stamp, but it stays leaving until `finalize_gone` closes it: only
`join_network` sets a gateway to joined, and `join_network` needs the account
not to exist. A gateway that joined after the epoch started is outside the
earning set and is exempt.

A registry slot can't point at a closed gateway because `finalize_gone` is the
only instruction that closes a gateway account, it clears the slot in the same
instruction, and ADR-0036 refuses it until the latest epoch is distributed.
`join_network` appends above the epoch's `active_gateway_count`, and a gateway
that leaves keeps its slot until it is finalized.

## The backstop

`admin_close_stale_epoch` writes off an ended, undistributed epoch by closing
its account. `create_epoch` treats a closed previous epoch as finished, so the
next epoch proceeds. It requires the `EpochSettings` authority, refuses a live
or a distributed epoch, and keeps working after `finalize_migration`.

Every failure in the table that the network can't clear by itself ends here,
so the following must hold:

- The `EpochSettings` authority can sign promptly. Before moving it to a
  multisig, the path to propose and execute `admin_close_stale_epoch` through
  that multisig must exist and be rehearsed.
- `ario-gar` is never deployed with `--final`. A program upgrade is the
  remaining fix for a defect this table doesn't anticipate.

## Tests

The following integration tests in `programs/ario-gar/tests/integration.rs`
cover the behaviour this file relies on:

- `test_zero_observation_skip_lets_next_epoch_be_created`: an unobserved epoch
  skips, and the next epoch is created.
- `test_distribute_epoch_skips_when_no_observations` and the other `skip`
  tests: what a skip changes and what it leaves alone.
- `test_stalled_epoch_blocks_then_writeoff_unblocks`: a stuck epoch blocks the
  next one, and a write-off clears it.
- `test_create_epoch_allowed_once_previous_written_off`,
  `test_admin_close_stale_epoch_refuses_live_epoch`,
  `test_admin_close_stale_epoch_refuses_distributed_epoch`, and
  `test_admin_close_stale_epoch_works_after_migration_finalized`: the
  write-off's guards and effect.

## Operating requirements

- Keep at least one cranker running and funded. Each `create_epoch` fronts
  the new epoch account's rent (refunded at `close_epoch`), and every step
  pays transaction fees.
- Run a second cranker on separate infrastructure and a separate RPC
  provider. Cranking is permissionless, so a second instance only adds
  redundancy.
- Alert when the latest epoch is still undistributed some hours after its end
  time. That catches every failure in the table early, whatever the cause.
- Run `scripts/preflight-wave2.mjs` before any `ario-gar` upgrade. It checks
  that the latest epoch can be distributed.
