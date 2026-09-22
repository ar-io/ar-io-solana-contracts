# Wave 1 + Wave 2 GAR rollout — plan, ABI delta, and who must act

Companion to **#149** (ADR-0034 / ADR-0036 / ADR-0037), **#150** (test gaps) and
**#151** (the pre-flight gate), plus the Wave 1 work already live on staging
(ADR-0030 gateway operations address, ADR-0031 transferable epoch-settings
authority).

Two waves, deliberately **not** deployed together on mainnet. See
[Sequencing](#sequencing-and-why-it-is-not-one-upgrade).

---

## ABI delta — three instructions gain a required account

All three are **clients-first**: the new client works against the OLD program
(the extra entry is ignored as a `remaining_account`), but an OLD client FAILS
against the new one. That asymmetry is the whole reason for the sequencing
below.

| instruction | new account | ordering rule |
|---|---|---|
| `create_epoch` | previous `Epoch` PDA (ADR-0034) | **rent receipt stays FIRST.** The pre-Wave-2 program reads `remaining_accounts[0]` as the receipt; put the Epoch there and it is handed to `init_epoch_rent_receipt`, which rejects it. A receipt must also always be PRESENT. |
| `finalize_gone` | latest `Epoch` PDA (ADR-0036) | **swapped gateway stays FIRST.** The pre-Wave-2 program reads position 0 as the swapped Gateway. |
| `compound_delegation_rewards` | `GatewaySettings` (ADR-0037) | **must be LAST.** Declared in the IDL since `@ar.io/solana-contracts@1.4.0-staging.33`; wire-identical to the trailing form it replaced. |

Post-upgrade the program finds these **by key**, so order is irrelevant to it.
The ordering rules exist entirely for the pre-upgrade program, which is what the
new client runs against during the window.

### Failure modes for an un-updated client

| caller | failure after the upgrade |
|---|---|
| `create_epoch` | `MissingLatestEpochAccount` (6103) |
| `finalize_gone` | `MissingLatestEpochAccount` (6103) |
| `compound_delegation_rewards` | `AccountNotEnoughKeys` |

`save_observations` is **unchanged** — gateway operators submitting observations
need to do nothing.

### New error codes (appended, ADR-035)

`6102` `LatestEpochUnfinished` · `6103` `MissingLatestEpochAccount` ·
`6104` `StaleDelegatedStakeCounter` · `6105` `DelegationReconcileMismatch` ·
`6106` `DuplicateDelegationAccount` · `6107` `InvalidDelegationAccount` ·
`6108` `StaleSupplyCounters`

**6102 means two opposite things depending on the instruction that raised it**,
and consumers must distinguish them:

* from `finalize_gone` — **routine**. Registry positions are frozen while an
  epoch is unfinished, so the GC sweep is refused for the whole window between
  an epoch's creation and its distribution. A cleanup pass that runs every cycle
  will see this most cycles. Treat as wait-and-retry.
* from `create_epoch` — **the network is halted**. The previous epoch cannot be
  distributed, so the whole lifecycle has stopped until an operator writes it
  off with `admin_close_stale_epoch`. Treat as a page.

Anchor logs `Program log: Instruction: <Name>` before the error; the LAST such
line is the instruction that reverted. `ar-io-cranker#24` and
`ar-io-observer#140` implement exactly this.

### New event

`EpochSkippedNoObservationsEvent` (ADR-0034 addendum). An epoch with
`observations_submitted == 0` now pays nothing and credits nothing, rather than
recording a PASS for every gateway.

**Zero totals are not a discriminator.** A normal distribution with no eligible
gateway leaves *identical on-chain state* to a skip — no payment, no treasury
movement, no `total_epochs`, no `passed_epochs`. The new event's presence is the
only reliable signal. Indexers keying on `EpochDistributedEvent` totals will
misclassify.

---

## Who must act

Determine the live set from chain rather than from this document — the
population turns over. Fetch recent GAR signatures and group fee payers by
instruction:

```bash
# see scripts/ for the pattern; the short version is:
#   getSignaturesForAddress(GAR) -> getTransaction -> group
#   accountKeys[0] by "Program log: Instruction: <Name>"
```

At the time of writing (mainnet, 20-hour window) that yielded:

| role | count | impact if not updated |
|---|---|---|
| epoch crankers (`create_epoch`, `tally`, `distribute`, `compound`) | 2 | **highest** — but see below |
| `finalize_gone` sweepers only | 2 | lose rent-harvesting income |
| observers (`save_observations`) | 13 | none — instruction unchanged |

**Epoch creation does not halt if one cranker breaks.** `create_epoch` is
permissionless and the crankers alternate, whoever wins the race; epoch-rent
receipts (ADR-0029) record the creator per epoch and show both participating.
Any single updated cranker keeps the lifecycle running.

**This is not a gate on the upgrade.** A third-party cranker that does not
update simply stops participating until it does, and the AR.IO cranker carries
the load alone in the meantime. That is an accepted outcome: the protocol's
correctness does not depend on third-party crankers, only its redundancy does,
and they return when they update. Confirm the AR.IO cranker can carry 100% of
tally / distribute / compound before upgrading, and proceed.

### Release-note line for operators

> **Action required for cranker operators and anyone calling `finalize_gone` or
> `compound_delegation_rewards`:** update to `@ar.io/sdk` >= 4.4.0 before
> *[date]*. These instructions gain a required account; older clients will fail
> after the upgrade. Observers submitting `save_observations` are unaffected.

A version and a date are what make this actionable — without them the only way
an unknown operator learns is by breaking. No broader announcement is planned;
this line in the release notes is the notification.

---

## What this rollout actually buys: steady-state cranking

Wave 2 exists because the epoch lifecycle has repeatedly needed manual repair.
Each incident below is now closed by a shipped gate:

| incident | cause | closed by |
|---|---|---|
| mainnet epoch 540 | a mid-epoch join left a gateway untallied; `distribute_epoch` reverted the whole batch, halting rewards network-wide until that operator left | **ADR-0032** — staleness became a per-gateway reward-eligibility term instead of a batch-wide revert |
| staging epochs 790/791 | racing crankers tallied an old partially-tallied epoch, re-stamping weights belonging to the live one | **ADR-0033** — only the LIVE epoch may be tallied |
| mainnet epochs 542/543 | a `finalize_gone` sweep mid-epoch swap-removed a registry slot; gateways were then scored against another gateway's failure count, paying one that all 11 observers had failed | **ADR-0036** — registry positions frozen while an epoch is unfinished |
| mainnet epoch 550 | an epoch with zero observations distributed normally, crediting a PASS to all 633 gateways and laundering the record of failing ones | **ADR-0034 addendum** — no observations, no distribution |
| a stuck epoch of any kind | nothing stopped the next epoch superseding it, so the damage compounded silently | **ADR-0034** — an unfinished epoch blocks its successor, forcing the problem to be dealt with |

The last row is the trade this rollout makes deliberately: a stuck epoch now
**stops the line** instead of being quietly papered over. That is the point —
it converts silent corruption into a loud, recoverable halt with a documented
unstick path (`admin_close_stale_epoch`). It is also why the pre-flight gate and
the no-`--final` rule below are not optional.

---

## Sequencing, and why it is not one upgrade

**1. Wave 2 to staging first.** Wave 2 must not debut on mainnet. Staging
already carries Wave 1 with migrated gateways, which is the state mainnet
reaches after its own Wave 1 step — the right rehearsal surface.

**2. Mainnet sequential: Wave 1, soak, then Wave 2.** The specific reason, not a
general preference:

> Do not introduce the ADR-0034 halt gate at the same moment as a schema
> migration that touches every gateway.

ADR-0034 makes a stuck epoch stop the whole lifecycle. Wave 1 rewrites the
schema every gateway instruction reads, with the ADR-0030 stale-tail hazard
live. If the schema work destabilised distribution and both landed together,
you would be debugging it *under* a program where a stuck epoch also blocks
epoch creation — a schema bug compounding into a halt. Sequenced, the old
forgiving lifecycle is still there to recover under.

Supporting: mainnet's upgrade authority is a hot wallet, so a second upgrade is
one transaction rather than a second multisig ceremony; and each wave keeps its
own rollback artifact.

### Order of operations

1. Wave 2 → staging. Soak: observe a full `create → tally → prescribe →
   distribute → close` cycle, and `finalize_gone` behaviour (6102 during the
   epoch, success in the gap).
2. Mainnet **Wave 1** program upgrade.
3. Verify un-migrated gateways still decode (they must — the version gate).
4. Migrate every gateway (964 → 996 bytes).
5. Audit `operations_address`: expect all == operator, zero == `observer_address`,
   zero null.
6. Soak ≥ 1 full mainnet epoch distributing cleanly on migrated gateways.
7. Publish the release note above.
8. Mainnet **Wave 2** program upgrade — pre-flight immediately before.
9. Reconcile **every** gateway, **then** resync
   (`admin_reconcile_delegated_stake` → `admin_resync_supply_counters`).
10. `finalize_migration`.
11. Authority transfers **last**.

### Ordering rules that must not be broken

* **Clients ship before the program.** Always.
* **Reconcile only after ADR-0036 is live** — it makes gateways prunable, and
  each prune moves a registry slot.
* **Reconcile every gateway before the resync.** Resyncing first makes each
  later reconcile subtract from an already-corrected counter and underflow.
* **`expected_removed` must come from the genesis snapshot**, never from the
  same `getProgramAccounts` read that produced the delegation list — otherwise
  the completeness check is vacuous and a missed delegation strands stake.
* **Supply-counter drift is carried through the upgrade, not fixed first.** The
  reconcile/resync instructions are themselves Wave 2; they do not exist on the
  pre-upgrade program. The drift is an accounting counter, not a fund, and no
  instruction's correctness depends on it.

---

## The pre-flight gate

Run **before and after** every program upgrade and every migration batch:

```bash
node scripts/preflight-wave2.mjs --cluster staging|mainnet [--json out.json]
# exit 0 = clear to proceed
# exit 1 = findings  (it checked, and found problems)
# exit 2 = it could NOT check — nothing was verified, do not proceed
```

Read-only; issues no transaction and holds no key. Runs without a built
checkout — it falls back to the published `@ar.io/solana-contracts` package and
accepts `--program-ids <path>` — so it works on a gateway box. Each run prints
which client decoded and its exact version.

It reproduces `distribute_epoch`'s own `EpochWeightsClobbered` predicate against
live accounts, so an epoch that can never be distributed is caught **before** an
upgrade converts it into a halt.

**Re-run it immediately before each step, every time.** A clear result is a
point-in-time fact, not a standing one.

---

## Risks and notes

**The halt mode is real but its cause is structurally closed.** After ADR-0034 a
permanently undistributable epoch stops the lifecycle network-wide. The only
realistic cause is `EpochWeightsClobbered`, and ADR-0033 already gates tally to
the live epoch while ADR-0034 gates epoch creation on distribution — chained,
an epoch stops being tallyable only after it has been distributed. What remains
is legacy state (an epoch clobbered *before* the upgrade) and unforeseen bugs.
That is what the pre-flight and the backstop are for.

**`ario-gar` must never be deployed `--final`.** Recovery from a halted epoch
has two layers: `admin_close_stale_epoch` (gated on
`EpochSettings.authority`, minutes) and a program upgrade (hours). If both were
revoked, an undistributable epoch would brick the protocol permanently. For the
same reason, keep `EpochSettings.authority` on a key that can sign quickly until
Wave 2 is bedded in — `transfer_epoch_settings_authority` rejects the null
pubkey, but a lost key or a quorum-less multisig is indistinguishable from
burning it.

**Gateway counts move continuously.** `finalize_gone` sweeps run constantly, so
any count-derived figure — batch counts, rent top-ups, fee budgets — must be
computed fresh at execution time, never pre-baked.

**A staging soak against a Wave 1 program does not exercise 6102.** The code
cannot raise it. A clean staging run before the staging Wave 2 upgrade proves
backward compatibility only — not the new error handling.
