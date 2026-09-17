# ADR-0037: A Gateway's Delegated-Stake Counter Must Equal the Delegations Behind It

* **Status:** proposed
* **Date:** 2026-09-17
* **Deciders:** @vilenarios
* **Consulted:** observer-operator report ("`finalize_gone` is blocked for 92 leaving
  gateways"); on-chain audit of mainnet and staging; replay of every mainnet
  `import_account` write; the mainnet genesis and remediation snapshots
* **Informed:** gateway operators, delegates, cranker/observer/SDK maintainers

> **TL;DR:** The AO import wrote each gateway's `total_delegated_stake` from AO's
> total, but created Delegation accounts only for delegators who had a Solana
> address. The stake of the other 2,122 delegations went to the migration
> authority's pot at genesis and never entered the stake pool. So 132 mainnet
> gateways count **2,226,210.675676 ARIO** of delegated stake that no Delegation
> account and no pool token backs. Those counters can never reach zero, so `finalize_gone` is
> blocked for good on 65 leaving gateways. They also inflate stake weight and
> dilute delegate rewards. A new authority-only instruction lowers a gateway's
> counter to the sum of the Delegation accounts it is shown, and only when the
> caller's expected before-and-after values agree. Separately, reward settlement
> never updates `GatewaySettings.total_delegated`. That is fixed, and the counter
> is re-synced once. Ships in Wave 2 with ADR-0034 and ADR-0036, after
> ADR-0036 is live.

## Context and problem statement

### How the counter is supposed to work

`Gateway.total_delegated_stake` (the counter) is the sum of the gateway's
`Delegation.amount` values. Both hold **settled principal** only:

* `distribute_epoch` credits delegates by raising
  `Gateway.cumulative_reward_per_token` (the accumulator), not by touching
  either amount (`instructions/distribution.rs`, the NOTE above the accumulator
  update).
* `settle_delegate_rewards` (`state/mod.rs`) adds the same pending amount to the
  Delegation **and** to the counter.
* Every other change — `delegate_stake`, `decrease_delegate_stake`,
  `redelegate_stake`, both `claim_delegate_from_*` cranks, `cancel_withdrawal`,
  and the fund-from-stake paths in `payment.rs` — moves the counter and one
  Delegation by the same amount.
* A Delegation account can only be closed at amount 0
  (`CloseEmptyDelegation`).

So `counter − Σ Delegation.amount` is invariant under every program instruction.
Whatever value it had when a gateway was created, it keeps forever.

The counter is used for:

| use | where |
|---|---|
| `finalize_gone` requires it to be 0 | `instructions/gateway.rs` (`finalize_gone`) |
| re-enabling delegation requires it to be 0 | `update_gateway_settings` (`DelegatesStillActive`) |
| stake weight → composite weight → observer selection | `Gateway::total_stake`, `tally_weights` |
| `delegated_at_tally` (whether a delegate share is carved out at distribution) | `tally_weights` |
| accumulator denominator (the per-token increment) | `distribute_epoch` |

### What is wrong

Measured 2026-09-17 with `scripts/delegated-stake-audit.mjs` (added with this ADR):

| | mainnet | staging |
|---|---|---|
| gateways | 620 (306 joined, 314 leaving) | 620 (202 joined, 418 leaving) |
| joined, counter > Σ Delegation | 67 (10 have no Delegation at all) | 72 (13) |
| leaving, counter > Σ Delegation | 65 (23 have no Delegation at all) | 60 (16) |
| counter < Σ Delegation | **0** | **0** |
| Σ (counter − Σ Delegation) | **2,226,210.675676** | 2,091,475.086100 |

The observer operator's report is right that these counters are unreconcilable.
Two of its hypotheses were not: residue vaults are ordinary Withdrawal accounts, and
Withdrawals cannot close the gap because moving stake into one already lowers the
counter.

### Root cause: the AO import

In `solana-ar-io/migration/snapshot/src/transform.ts`:

* line 1039 writes `totalDelegatedStake: u64BigInt(gw.totalDelegatedStake ?? 0)`,
  which is AO's total for the gateway;
* lines 1145–1150 skip the Delegation account of any delegator with no Solana
  address (`isPlaceholder`).

Under the genesis mint split (`docs/MIGRATION_VAULT_STAKE_ESCROW_PLAN.md`,
"Genesis mint split"), unmapped delegator stake is `authoritySourced`: it is held in the
migration authority's pot (the plan calls it "returned"), and the stake pool is minted
`staked + delegated − authoritySourced + boost`. So the counter includes stake that
has neither a Delegation account nor tokens in the pool.

The June remediation hit the same problem on the gateways *it* created, and patched
their counters down (`migration/import/src/remediate.ts`, the
"scoped delegation selection" note and the `totalDelegatedStake: keptDeleg` patch:
"re-adding the already-vaulted 'phantom' delegate base would double-count it").
The 622 genesis gateways were never corrected.

### Evidence (mainnet)

1. **The snapshot explains every gateway.** The genesis snapshot
   (`output-mainnet-prod-pruned`) has 622 gateways and 531 delegations. For
   **622 of 622** gateways, `counter − Σ imported delegations` equals the AO stake of
   that gateway's unmapped delegators, exactly. In total that is 2,122
   delegations and 2,226,210.675676 ARIO.
2. **The chain received exactly that snapshot.** Replaying all 1,824 `import_account`
   writes signed by the authority `45ZuEb1J…` shows every snapshot gateway's first
   on-chain import has identical operator stake, counter and status (622/622).
3. **The pool was funded for real delegations only.** The genesis mint to the stake
   pool (`FK4bsbaT…`, 2026-06-05 16:47Z) was **26,117,048.325000 ARIO**. The
   cutover-day imports wrote operator stake plus Delegation amounts of
   **26,117,048.325000**, exactly. Including the counters instead gives
   28,343,259.000676.
4. **Nothing has moved since.** Of the 611 imported gateways still on chain, all
   have the same `counter − Σ Delegation` today as at import, once the on-chain
   delegations made before the 06-11 remediation re-import are counted (Turbo
   Gateway 2,010 ARIO, Fllstck 20,000 ARIO; the re-import kept them). The 9
   gateways created on Solana since have none. No gateway is under-counted.
   Per gateway, today's overcount equals the genesis snapshot's for all 594
   genesis gateways still on chain (the 17 the remediation created carry none), and
   today's total, 2,226,210.675676, equals the snapshot's to the mARIO.
5. **The pool balances to 0.012 ARIO.** The pool holds 29,651.054790 ARIO more than
   operator stake + Delegation accounts + Withdrawals. That surplus is:

   | part | ARIO |
   |---|---|
   | unsettled rewards owed to real delegates (all 497 Delegations are settled) | 0.000000 |
   | rewards credited to phantom stake, Σ phantom × accumulator / 10¹⁸ (every imported accumulator started at 0; phantom has been constant) | 20,191.237903 |
   | remediation double-funding: delegations `FEK1Zz5Y…` (9,364.646096) and `52ykVMEP…` (95.158401) were imported at genesis, skipped as "pre-existing" by the remediation, but funded again | 9,459.804497 |
   | remainder (integer-division rounding in reward math) | 0.012390 |

Staging was imported from the same genesis data: its v2 snapshot has the same
2,226,210.675676 overcount. Three months of test activity and its own remediations
have moved the live figure, and its pool and `total_staked` show further
staging-only differences (see Implementation notes).

### Consequences today

* **Pruning.** `finalize_gone` can never succeed on the 65 over-counted leaving
  gateways. 23 of them have no Delegation accounts at all. Most leaving gateways
  left around the June cutover, so their ~97-day windows fall due from September
  to November, and every phantom one stays in the registry for good. Crankers walk
  every slot on every tally and distribution.
* **Re-enabling delegation** is impossible for any over-counted gateway that
  disables it.
* **Selection.** 67 joined gateways get stake weight, and therefore observer-selection
  odds, for stake they do not have.
* **Rewards.** Each epoch a delegate share is carved out for these gateways and
  divided by the inflated counter. The phantom part is credited to nobody:
  * about **17,221 ARIO** so far is missing from real delegates on 99 gateways;
  * about **2,970 ARIO** was carved out of rewards on the 33 gateways that have no
    real delegates today.

  Both amounts grow every epoch.
* **Supply reporting.** `GatewaySettings.total_delegated` was seeded from the
  inflated counters.

### A second, independent defect: settlement skips the supply counter

`settle_delegate_rewards` raises the gateway counter, but no caller raises
`GatewaySettings.total_delegated` by the same amount. `compound_delegation_rewards`
has no `settings` account at all, and `delegate_stake` adds only the deposit.

The integration test `test_compound_delegation_rewards`, extended to read
`settings`, shows it: after 100 ARIO is compounded, the gateway counter is
200,000,000 while `settings.total_delegated` is 100,000,000.

`INVARIANTS.md` ("Invariant 2") claims the two stay equal because both are "equally
stale". That holds only until the first settlement. The property test
(`assert_global_stake_invariants`) never settles a reward, so it cannot catch this.

On mainnet, `settings.total_delegated` is 80,442.894868 ARIO below the sum of the
counters. It was last written by `migrate_settings_supply_counters` on 2026-06-12 at
20:32Z. That was after the last gateway import write (20:04Z the same day) and
before the first `distribute_epoch` (2026-06-17 00:04Z). So the whole gap is
rewards settled since then. The seeded value also included the phantom counters,
which the reconcile below removes as well.

### Migration is still open

`GatewaySettings.migration_active` is still `true` on both clusters:
`finalize_migration` has not run, and `MIGRATION_DEADLINE` is 2112. So the migration
authority can still overwrite any GAR account with `import_account`:

* on mainnet that is `45ZuEb1J…`;
* on staging it is the deployer `FHgQn4W9…`, **not** the Squads vault.

## Decision drivers

* Fix the stored state, not the readers: every consumer (program, SDK, cranker,
  observer, indexers) should be able to trust the counter.
* A correction must never raise a liability, and must never set a counter below the
  Delegation accounts it has to cover. A too-low counter would let `finalize_gone`
  close a gateway whose delegates still need its account to claim.
* A correction must be checkable on chain, guarded against stale reads, and visible
  as an event.
* No layout change and no migration of 620 accounts.
* Mass pruning reorders the registry, so the correction must not unblock pruning
  until ADR-0036 freezes positions mid-epoch.

## Considered options

1. **A. Authority-only `admin_reconcile_delegated_stake`, proven against the Delegation
   accounts passed in** (chosen)
2. **B. Overwrite the counters with `import_account`**, which is still open
3. **C. Make `finalize_gone` ignore the counter, or derive it from Delegation accounts**
4. **D. Authority sets the counter to a supplied value, unchecked**
5. **E. Do nothing**

**B** needs no upgrade, but it writes a whole account image built from an
off-chain read. Anything that lands in between (a compound, a distribution raising
the accumulator, a delegation, stats) is silently overwritten. It has no guard and
no event. It also relies on the privilege `finalize_migration` exists to remove.
Rejected.

**C** is impossible as stated: a program cannot enumerate a gateway's Delegation
accounts or prove it has seen all of them. Ignoring the counter would let a gateway
close under live delegations. Rejected.

**D** trusts one off-chain number with nothing to check it against. Rejected in
favour of A, which costs little more.

**E** leaves 65 gateways permanently unprunable and keeps mis-paying rewards every
epoch.

## Decision

### 1. `admin_reconcile_delegated_stake(expected_counter: u64, expected_removed: u64)`

* **Signer:** `GatewaySettings.authority`. That is the mainnet authority `45ZuEb1J…`,
  and the Squads vault on staging.
* **Accounts:** `settings` (mut), `gateway` (mut), `authority` (signer).
  `remaining_accounts` must be **every** Delegation account of the gateway,
  read-only.
* **Checks, in order:**
  1. `gateway.total_delegated_stake == expected_counter`. This guards against a
     stale read: any delegation activity since the plan was computed fails the call.
  2. Each remaining account is owned by this program, carries the `Delegation`
     discriminator, deserializes, has `delegation.gateway == gateway.operator`, and
     sits at the canonical PDA `["delegation", gateway.operator, delegation.delegator]`.
  3. No account appears twice.
  4. `sum = Σ delegation.amount`. Both sides are settled principal, so no settlement
     is needed.
  5. `expected_removed > 0` and `expected_counter − sum == expected_removed`.
     This rejects any call that would raise the counter.
* **Effects:**
  * `gateway.total_delegated_stake = sum`;
  * `settings.total_delegated -= expected_removed`, checked;
  * emit a new `DelegatedStakeReconciledEvent { gateway, previous, removed, new,
    delegations_counted, timestamp }`.
* **No epoch gate.** Weights are cached at tally. Lowering the denominator before a
  distribution only sends a larger share to real delegates. If `sum == 0`, the share
  takes the existing orphaned path and stays in the treasury (ADR-025).
* **Completeness.** The program cannot prove it saw every Delegation, so the call
  needs two independent sources to agree:
  * the Delegation list comes from `getProgramAccounts`;
  * `expected_removed` comes from the genesis snapshot (the gateway's unmapped AO
    stake), which the audit reproduced exactly for all 622 gateways.

  If a Delegation were left out, check 5 fails unless both sources are wrong by the
  same amount.
* **Size.** The largest over-counted gateway has 50 Delegations today. A v0
  transaction with a lookup table fits that (about 55 accounts, under the 64
  account-lock limit). A gateway with more Delegations than fit cannot be reconciled
  this way; none exist.

### 2. Settlement keeps the supply counter in step

* `settle_delegate_rewards` returns the settled amount. Every caller adds it to
  `settings.total_delegated`.
* `CompoundDelegationRewards` gains `settings` (mut) as its **last** account. Anchor
  treats extra trailing accounts as `remaining_accounts`, so clients that append the
  settings PDA work before and after the upgrade. Clients ship first, as in
  ADR-0034.
* Every compound then write-locks `settings`, as delegation instructions already do.
  That serializes concurrent compounds but does not change results.
* A new `admin_resync_total_delegated(expected_current: u64, new_value: u64)`,
  authority-only, sets `settings.total_delegated` once after the fix is live.
  `new_value` is Σ counters from the audit, and the call fails if `expected_current`
  is stale.
* `migrate_settings_supply_counters` is **not** reused: it has no staleness guard,
  and it stops working once migration is finalized.
* `INVARIANTS.md` Invariant 2 is corrected, and the property test gains a
  settle-then-check step.

### 3. Rollout (Wave 2)

1. **Clients:** SDK and cranker append `settings` to `compound_delegation_rewards`.
   The SDK adds builders for both admin instructions. The audit script's `--json`
   output becomes the reconcile plan.
2. **One GAR upgrade** carrying ADR-0034, ADR-0036 and ADR-0037, staging first.
3. **Reconcile only after ADR-0036 is live.** The correction makes up to 65 leaving
   gateways prunable, and each prune moves a registry slot (the epoch-542 hazard).
4. **Run the plan** gateway by gateway, re-reading each one. On mainnet that is 132
   transactions; on staging they go through Squads.
5. Resync `settings.total_delegated`; re-run the audit, which must show 0
   over-counted and 0 under-counted gateways.
6. Decide separately whether to run `finalize_migration` on both clusters.

## Consequences

### Positive

* 65 leaving gateways can be pruned under the ordinary rules: the 23 with no
  Delegations right away once their window passes, and the other 42 once their real
  delegations are cranked out. Crankers stop walking them once they are pruned.
* Stake weights and delegate reward shares match real stake from the next epoch on.
* Operators of the 33 gateways with no real delegates keep their whole reward again.
* `total_delegated` becomes a trustworthy supply figure, and the audit script can
  watch for drift.

### Negative / risks

* 67 joined gateways lose stake weight, and with it observer-selection odds. This
  should be announced before it happens.
* Two new admin powers.
  * Reconcile can only lower a counter to a sum proven from Delegation accounts.
  * Resync is limited only by its staleness guard.

  Both are on the hot authority key on mainnet.
* A reconcile that missed a Delegation *and* matched a wrong `expected_removed`
  would under-count that gateway. The audit's under-count check (exit status 1)
  exists to catch that afterwards.
* This does not repay anyone. About 20,191 ARIO of past delegate share and 9,459.8
  ARIO of double funding stay in the pool, unowned (see below).

### Neutral

* The instruction has no ongoing role once the plan has run. It stays in the program
  as a correction tool.
* Adds one account to every compound transaction.

## Implementation notes

* **ABI.** New error variants are appended (ADR-035), and the new event goes through
  the event snapshot (ADR-018).
* **Tests.**
  * A fixture that is really import-shaped (counter above its Delegations, including
    a gateway with none): reconcile, then `finalize_gone` succeeds on a leaving
    gateway past its window.
  * Rejections: stale `expected_counter`; wrong `expected_removed`; a missing
    Delegation; a duplicate; a Delegation of another gateway; a non-Delegation
    account; an account owned by another program; a non-authority signer; any
    attempt to raise the counter.
  * Reconcile between tally and distribution: the delegate share goes wholly to real
    delegates, or to the orphaned path when there are none.
  * Settlement: every settling instruction keeps `settings.total_delegated` equal to
    Σ counters. Extend `assert_global_stake_invariants` scenarios with a distributed
    reward and a compound.
* **Audit tool.** `scripts/delegated-stake-audit.mjs --cluster mainnet|staging
  [--json plan.json]` is read-only. Re-run it right before building the plan.
* **Out of scope, still to be decided:**
  * Whether to repay the phantom-credited share from the per-gateway list (the
    `phantom_credited_rewards` field of the plan): about 17,221 ARIO to real
    delegates and about 2,970 ARIO to operators on mainnet. The tokens are in the
    pool, but no instruction can move unowned pool surplus, and this ADR adds none.
  * `transform.ts` should set each counter to the sum of the Delegation accounts it
    emits, in case the import tooling is ever reused.
  * Staging-only differences to investigate separately: `total_staked` is
    525,057.873339 below Σ operator stake, and a 9,857.7 ARIO pool remainder is
    not yet attributed.
  * `claimDelegateFromLeavingGateway` in the SDK should accept a delegator, as the
    disabled-gateway variant does, so crankers can clear real delegations from
    leaving gateways.
