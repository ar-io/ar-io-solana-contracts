# Funding Modes — Integrator Guide

How to fund fee-paying AR.IO operations on Solana.

This is the integrator-facing guide. For the full implementation plan see
`docs/FUND_FROM_PLAN.md`. For the on-chain primitive ix see
`docs/INSTRUCTION_REFERENCE.md`. For Lua-vs-Solana behavioral differences
see `docs/BEHAVIORAL_DIFFERENCES.md` BD-031 / BD-076 / BD-081.

---

## TL;DR

Every fee-paying AR.IO ix accepts `fundFrom`. Pick the mode that matches
where you want to spend from:

| `fundFrom` | What spends | When to use |
|------------|-------------|-------------|
| `'balance'` (default) | User's SPL ATA | Simple buy with cash on hand. |
| `'stakes'` + `gatewayAddress` | Active delegation on that gateway | Pay from staked tokens; tokens go directly stake-pool→treasury (no withdraw cycle). Add `fundAsOperator: true` to spend from operator stake instead. |
| `'withdrawal'` + `withdrawalId` | A locked Withdrawal vault | Spend from tokens that are unwinding. Vault stays open at zero until permissionless cleanup. |
| `'plan'` + `sources` | Caller-supplied multi-source plan | You computed the plan and want bypass discovery. |
| `'any'` | Auto-picked multi-source plan | You want the SDK to compose the cheapest path. Drawdown matches Lua: balance → vaults (oldest first) → excess delegated stake → minimum delegated stake (auto-vault residue). |

The SDK's CLI exposes the same modes:

```bash
# Plain balance buy (default)
ar.io buy-record --name foo --type lease --years 1

# Spend from delegation on a specific gateway
ar.io buy-record --name foo --type lease --years 1 \
  --fund-from stakes --gateway-address <GATEWAY_PUBKEY>

# Spend from operator stake on a specific gateway
ar.io buy-record --name foo --type lease --years 1 \
  --fund-from stakes --gateway-address <GATEWAY_PUBKEY> --fund-as-operator

# Spend from a locked Withdrawal vault (vault id 7)
ar.io buy-record --name foo --type lease --years 1 \
  --fund-from withdrawal --withdrawal-id 7

# Multi-source: explicit plan
ar.io buy-record --name foo --type lease --years 1 \
  --fund-from plan \
  --funding-plan-json '[{"kind":"balance","amount":"50000000"},{"kind":"withdrawal","amount":"450000000"}]' \
  --withdrawal-id 7

# Multi-source: auto-pick (Lua-faithful planner)
ar.io buy-record --name foo --type lease --years 1 --fund-from any \
  --gateway-address <GATEWAY_PUBKEY>
```

---

## Mode-by-mode

### `'balance'` — direct SPL transfer

The user's ARIO ATA → protocol treasury. One SPL transfer. No
gateway/withdrawal/plan accounts involved.

```ts
ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'balance', // or omit — this is the default
});
```

This is the path the SDK took before the multi-source feature shipped.
Existing integrations work without changes.

### `'stakes'` — vaults + delegation or operator stake

Tokens go directly from the stake pool to the protocol treasury — no
withdrawal vault for delegation/operator paths, no lock period, no
penalty. The gateway state isn't mutated apart from the bookkeeping
decrement.

When the SDK's auto-picker runs (the multi-source path), `'stakes'`
draws **withdrawal vaults first, then delegation/operator stake**.
This matches Lua's `getFundingPlan` (gar.lua:1437): only `'balance'`
short-circuits before the vault pass, so `'stakes'` walks vaults too.

```ts
// Delegation (default when fundAsOperator is false/unset)
ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'stakes',
  gatewayAddress: 'GatewayPDA…',
});

// Operator stake (preserves gateway viability — rejects if
// post-deduction stake would fall below `min_operator_stake`).
ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'stakes',
  gatewayAddress: 'GatewayPDA…',
  fundAsOperator: true,
});
```

Constraints:
- **Skips wallet balance.** A user with a positive ATA balance who picks
  `'stakes'` won't have it touched.
- **Delegation**: residue must be `0` or `≥ min_delegation_amount`
  (the gateway's per-gateway floor). Drawing from a delegation that
  would leave a sub-min residue rejects with `DelegationBelowMinimum`.
  If you want auto-vault behavior, use `'any'` mode instead.
- **Operator stake**: residue must be `≥ min_operator_stake`. Hard reject
  on sub-min — rationale is gateway viability (Lua doesn't fund-from
  operator stake at all; this is a Solana extension).

### `'withdrawal'` — spend from a locked vault

```ts
ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'withdrawal',
  withdrawalId: 7n,
});
```

Properties:
- **Gateway-status-independent**: vaults from leaving / Gone gateways
  drain just fine. Only `cancel_withdrawal` requires `gateway.status ==
  Joined` (because it re-stakes).
- **Partial drains supported**: residue stays in the vault with the
  original `available_at`. No minimum-residue constraint.
- **Permissionless cleanup**: once `withdrawal.amount == 0`, anyone can
  call `close_drained_withdrawal` to refund rent to the original owner.
  The SDK plan executor automatically batches the cleanup in the same
  tx as a fully-draining payment.
- **Works on exit vaults** (`is_exit_vault: true`) and delegate-side
  vaults (`is_delegate: true`) transparently — the ix doesn't branch.

### `'plan'` — explicit multi-source

You compute the plan and pass it verbatim. SDK skips source discovery
and just builds the on-chain ix.

`'plan'` **requires** `params.sources` to be set. Calling with
`fundFrom: 'plan'` and no sources throws — use `fundFrom: 'any'` if you
want the SDK to discover and pick for you. (Pre-2026-05 the SDK
silently fell through to discovery, making `'plan'` a synonym for
`'any'`; this was a bug.)

```ts
ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'plan',
  gatewayAddress: 'GatewayPDA…', // required when sources include delegation/operatorStake
  sources: [
    { kind: 'balance', amount: 50_000_000n },
    { kind: 'withdrawal', amount: 450_000_000n },
  ],
  withdrawalId: 7n, // required when sources include exactly 1 withdrawal
});
```

Constraints:
- **Multi-gateway**: `delegation` sources may span up to
  `MAX_DELEGATION_SOURCES = 3` distinct gateways per plan. Each
  Delegation/OperatorStake entry carries an optional `gateway` field
  (per-source); when omitted, the executor falls back to
  `params.gatewayAddress`. `balance` and `withdrawal` are
  gateway-independent.
- **Hard cap of 5 sources** per call (`MAX_FUNDING_SOURCES`).
- **Sum of `amount` values must equal the operation cost.** The
  on-chain handler verifies this and rejects with
  `FundingPlanAmountMismatch` on a mismatch — the SDK's
  `_estimateBuyNameCost` / `_estimateManageStakeCost` reads the live
  `DemandFactor` so caller can compute cost ahead of time.

### `'any'` — Lua-faithful multi-source picker

This is the auto-picker. The SDK enumerates the user's available
sources, sorts them in a fixed order, and composes a multi-source plan
that satisfies the operation cost. Faithful port of
`ar-io-network-process/src/gar.lua::getFundingPlan` (lines 1421–1623).

```ts
ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'any',
  // optional — pins the gateway to the front of Stage 3 iteration
  gatewayAddress: 'GatewayPDA…',
});
```

The picker runs **four stages** in order. Each stage is a standalone
draw against the running shortfall; the next stage runs only if the
prior didn't fully cover.

#### Stage 1 — Wallet balance

The user's ARIO Associated Token Account is the cheapest source: one
SPL transfer, no on-chain bookkeeping. If balance >= shortfall, the
planner stops here and returns a single `Balance` source.

#### Stage 2 — Withdrawal vaults (oldest-maturing first)

Any `Withdrawal` PDA owned by the user is a fund-eligible source:
vaults from `decrease_operator_stake`, `decrease_delegate_stake`,
`leave_network` exit, etc. They're sorted **ascending by
`available_at`** (closest to maturity first — `gar.lua:1531`) and
drained sequentially. Partial drains leave the remaining balance in
the vault with the original `available_at` intact; once a vault hits
zero anyone can call `close_drained_withdrawal` to refund the rent.

The maturity time isn't enforced at draw — pay-from-vault is allowed
even before unlock. The lock period is enforced only on
`claim_withdrawal` (which sends tokens to the user's ATA) and
`instant_withdrawal` (which charges a time-decaying penalty).

#### Stage 3 — Excess delegated stake (across gateways)

The user's `Delegation` PDAs are sorted by Lua's `getStakingProfile`
order — primary key first:

1. **Excess descending** — `available - min_delegation_amount`.
   Gateways where the user has the most "above-floor" stake go first;
   intuition is "drain the excess that's least costly to lose."
2. **Performance ratio ascending** —
   `(1 + passed_epochs) / (1 + total_epochs)`. Tie-breaker: prefer the
   worst-performing gateway, since you'd rather not concentrate stake
   there anyway.
3. **Total delegated stake descending** — pick the higher-cap gateway
   as a third tie-break.
4. **Start timestamp descending** — newest gateway last.

The picker iterates this list and draws each gateway's excess until
shortfall hits zero or the source caps are reached. Up to **3 distinct
delegation gateways** can be touched in one plan
(`MAX_DELEGATION_SOURCES`); the overall source array maxes at **5**
(`MAX_FUNDING_SOURCES`). These caps exist for transaction-size reasons.

If the caller supplied `gatewayAddress`, that gateway is moved to the
front of the iteration order (Stage 3 only — it doesn't re-bias Stage 4).

#### Stage 4 — Minimum delegated stake (Lua-faithful re-sort)

If the shortfall still isn't covered after all excess is drained, the
picker dips into the per-gateway floor. Crucially, the iteration order
is **re-sorted** before this pass (`planMinimumStakesDrawdown` at
`gar.lua:1587`):

1. **Performance ratio ascending** — drain the worst-performing
   gateway's floor first.
2. **Total delegated stake descending** — tie-break.
3. **Start timestamp descending** — final tie-break.

The intuition is "concentrate the auto-vault on bad gateways" — the
user is dipping below the per-gateway floor anyway, so prefer the
gateway you'd want to exit. This is the one place where Stage 4
deliberately disagrees with Stage 3's ordering.

When a gateway's drain leaves the post-decrease delegation in
`(0, min_delegation_amount)` — i.e. positive but sub-floor — the
on-chain handler **automatically creates a fresh `Withdrawal` vault
for the residue** and zeros the delegation. The residue vault inherits
the standard 30-day lock. The SDK pre-computes which gateways will
trigger this (the `residueDelegationIndexes` array) and supplies the
residue vault PDAs in the trailing slots of `remaining_accounts`,
sequenced from `WithdrawalCounter.next_id`.

#### Caps and rejection

| Cap | Value | Rejection |
|---|---|---|
| Total sources per plan | 5 | `TooManyFundingSources` |
| Distinct delegation gateways per plan | 3 | `TooManyDelegationSources` / `DuplicateGatewayInSources` |
| Sum of source amounts | Must equal operation cost exactly | `FundingPlanAmountMismatch` |
| Residue vault count | Must match `residueDelegationIndexes.length` | `MismatchedResidueVaultCount` / `MissingResidueVault` |

If the planner can't cover the cost given the discovered sources and
these caps, it returns a structured `InsufficientFunding` error with a
per-source breakdown — the caller can surface that to the user, top up
balance, or pick a different gateway.

#### Worked example

User has:
- ARIO balance: 50 mARIO
- Withdrawal vault `id=7`: 100 mARIO, available in 5 days
- Delegation on Gateway A: 200 mARIO, min 100 → 100 excess, perfRatio 0.95
- Delegation on Gateway B: 250 mARIO, min 100 → 150 excess, perfRatio 0.60
- Delegation on Gateway C: 130 mARIO, min 100 → 30 excess, perfRatio 0.99

Cost: 800 mARIO. Funding plan composes as:

| Step | Source | Amount | Running shortfall |
|---|---|---|---|
| Stage 1 | Balance | 50 | 750 |
| Stage 2 | Withdrawal #7 | 100 | 650 |
| Stage 3 | Delegation B (excess) — picked first by excess-desc | 150 | 500 |
| Stage 3 | Delegation A (excess) — second by excess-desc | 100 | 400 |
| Stage 3 | Delegation C (excess) | 30 | 370 |
| Stage 4 | **Re-sort: perf asc → B (0.60) first.** Drain floor of B | 100 | 270 |
| Stage 4 | A (0.95) — drain floor | 100 | 170 |
| Stage 4 | C (0.99) — drain remaining 130 (full drain → no auto-vault) | 130 | 40 |
| **Insufficient** | | | 40 short |

Result: an `InsufficientFunding` error with shortfall=40. The user adds
40 mARIO to their balance and retries.

#### `OperatorStake` opt-in

**OperatorStake is NOT included by default.** Lua's `getFundingPlan`
only iterates gateways where the address is a delegate; operator stake
is untouched. The Solana picker mirrors that. Opt in with
`fundAsOperator: true` to include operator stake as an additional
draw source — drawn after Stage 3 excess, before Stage 4 floor.
Hard-rejects sub-min residue (preserves gateway viability).

#### Where to read the code

- **Off-chain planner**: `sdk/src/solana/funding-plan.ts::buildFundingPlan`
  — pure function, fully unit-tested.
- **On-chain executor**:
  `programs/ario-gar/src/instructions/payment.rs::pay_from_funding_plan`
  — composite ix, walks `sources`, makes ≤2 SPL transfers per call,
  auto-creates residue vaults.
- **Per-fee-path wrappers**: `_from_funding_plan` and `_from_withdrawal`
  variants exist for every ArNS fee-paying instruction (`buy_name`,
  `extend_lease`, `upgrade_name`, `increase_undername_limit`,
  `request_primary_name`, `request_and_set_primary_name`).

#### Solana-only extensions to the Lua model

Three places where the Solana planner does more than Lua. Documented so
multi-protocol integrators can reason about parity vs feature.

| Extension | What it does | Lua has? | Why |
|---|---|---|---|
| **`'withdrawal'` mode** | Vault-only path; spends one or more `Withdrawal` PDAs without touching balance or delegations | No | Useful for surfacing a specific locked vault to a fee path. The Lua model has no equivalent — vault drawdown is entry-or-skip-everything inside `'any'`/`'stakes'`. |
| **`OperatorStake` as a fund source** | Opt-in via `fundAsOperator: true`; operator stake is treated as a 1-source plan, hard-rejecting sub-min residue | No | Lua's `getFundingPlan` never iterates operator stake (only `gateway.delegates[address]`). Adds parity for operators who want to spend their own stake without leaving the network. |
| **Operator-side `Withdrawal` vaults included in pool** | `discoverFundingSources` returns every `Withdrawal` PDA owned by the user except `is_protected: true` exit vaults — operator-side decrease vaults (`is_delegate: false, is_protected: false`) and delegate-side vaults (`is_delegate: true`) are sorted together by `available_at` | No (Lua's `planVaultsDrawdown` only flattens `gatewayInfo.delegate.vaults`) | Treats vaults uniformly except for the operator's protected min-stake exit vault, which is filtered at discovery (and rejected on-chain by `deduct_withdrawal_for_payment` if a caller bypasses the planner). The 90-day lock on the operator min stake is enforced — see [BD-102](./BEHAVIORAL_DIFFERENCES.md#bd-102). |

#### `getProgramAccounts` is restricted on default RPCs

Source discovery requires `getProgramAccounts` with memcmp filters,
which most public Solana RPCs disable for cost reasons. Switch to a
DAS-equivalent endpoint (Helius, Triton, etc.) **or** pass the
`sources` array explicitly via `'plan'` mode to skip discovery.

#### What residue auto-vault means

If `'any'` drains a Delegation source to a sub-min residue (e.g., 5 ARIO
residue when `min_delegation_amount = 10 ARIO`), the on-chain
`pay_from_funding_plan` ix automatically creates a fresh `Withdrawal`
vault for the residue and zeros the delegation. Matches Lua's
`applyFundingPlan` behavior (`gar.lua:1679`). The SDK exposes this as
`plan.residueDelegationIndexes: number[]` — one entry per Delegation
source slated to drain sub-min.

When a plan touches multiple gateways, each gateway's sub-min residue
gets its own Withdrawal vault. The vaults are sequenced from
`WithdrawalCounter.next_id` (next_id, next_id+1, …). The SDK
pre-derives them via `predictResidueVaults` and passes them as the
trailing slots in `remaining_accounts`; pass exactly
`plan.residueDelegationIndexes.length` PDAs.

---

## Choosing a mode

```
                       Need Lua-equivalent compose-anything?
                                       │
                                  yes  │  no — single source is fine
                                       ▼
                              ┌── 'any' (auto)
                              │
                              └── 'plan' + sources= (explicit)

Single source:
  Balance only?          → 'balance'   (default)
  One delegation/op?     → 'stakes'    (+ gatewayAddress [+ fundAsOperator])
  One withdrawal vault?  → 'withdrawal' (+ withdrawalId)
```

## Errors

| Error | Mode | Cause | Fix |
|-------|------|-------|-----|
| `InsufficientFunding` (SDK throw) | any/plan | No discovered sources cover cost | Top up balance, pick a different gateway, or pass explicit sources |
| `InsufficientDelegationForPayment` | stakes (delegation) | Delegation amount < cost | Increase delegation or use `'any'` to compose |
| `InsufficientOperatorStakeForPayment` | stakes (operator) | Operator stake < cost | Increase operator stake or pick a different mode |
| `InsufficientWithdrawalForPayment` | withdrawal | Vault amount < cost | Use a different vault or compose via `'any'` |
| `DelegationBelowMinimum` | stakes (delegation) | Post-deduction would leave sub-min residue | Drain entire delegation, leave ≥ min, or use `'any'` for auto-vault |
| `StakeBelowMinimum` | stakes (operator) | Post-deduction would leave sub-`min_operator_stake` | Use a smaller draw, or pick a non-stake mode |
| `FundingPlanAmountMismatch` | plan | `sum(sources.amount) ≠ cost` | Re-estimate cost via `_estimateBuyNameCost` and rebuild plan |
| `TooManyFundingSources` | plan/any | Plan has more than 5 sources | Consolidate balance/withdrawals, or chain multiple txs |
| `TooManyDelegationSources` | plan/any | Plan touches more than 3 distinct delegation gateways | Pick the top 3 gateways or chain multiple txs |
| `DuplicateGatewayInSources` | plan | Same gateway appears in multiple Delegation/OperatorStake sources | Aggregate into one source per gateway client-side |
| `MismatchedResidueVaultCount` / `MissingResidueVault` | plan | Trailing residue PDAs don't match Delegation sub-min count | Use `predictResidueVaults` + `buildFundingPlanRemainingAccounts` |
| `OnlyOneOperatorStakeSource` | plan | More than 1 OperatorStake source | Single OperatorStake per call (Solana extension constraint) |

## Behavioral differences vs Lua

See `docs/BEHAVIORAL_DIFFERENCES.md` for the full list. Highlights for
funding modes:

- **Multi-gateway delegation composition** — Closed (2026-05-04). Plans
  may span up to `MAX_DELEGATION_SOURCES = 3` distinct gateways. The
  SDK picker iterates Lua-sorted delegations and draws excess across
  gateways before falling back to floor-draining (which auto-vaults
  the residue per gateway).
- **Operator stake fund-from is a Solana extension** — Lua doesn't
  fund-from operator stake at all (`getStakingProfile` only iterates
  gateways where the address is a delegate). Solana adds it for
  symmetry with delegation; SDK picker excludes it by default to
  preserve Lua parity.
- **Operator stake sub-min hard reject vs Lua-style auto-vault** —
  Delegation sub-min residue auto-vaults per gateway (matches Lua);
  operator stake sub-min residue rejects with `StakeBelowMinimum`
  (preserves gateway viability — Solana-specific safety).

## Quick examples

### Spend from a withdrawal vault that's still locked

```ts
const ario = ARIO.init({ rpc, signer });
const result = await ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'withdrawal',
  withdrawalId: 0n, // first vault from your decrease_*_stake call
});
console.log(`tx: ${result.id}`);
```

### Compose balance + delegation in one tx

```ts
const ario = ARIO.init({ rpc, signer });
// Suppose name costs 600M mARIO. We have 100M in balance, 800M staked.
const result = await ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'plan',
  gatewayAddress: myGateway,
  sources: [
    { kind: 'balance', amount: 100_000_000n },
    { kind: 'delegation', amount: 500_000_000n },
  ],
});
```

### Let the SDK pick everything

```ts
const ario = ARIO.init({ rpc, signer }); // requires DAS-equivalent RPC for discovery
const result = await ario.buyRecord({
  name: 'foo',
  type: 'lease',
  years: 1,
  fundFrom: 'any',
});
// SDK runs discoverFundingSources + buildFundingPlan internally; throws
// InsufficientFunding (with structured cause) if no plan covers the cost.
```

### Same flows for the other fee-paying ix

Every fee-paying ix exposes the same `fundFrom` surface:
`buyRecord`, `buyReturnedName`, `upgradeRecord`, `extendLease`,
`increaseUndernameLimit`, `requestPrimaryName`, `setPrimaryName`. The
parameter shapes are identical — pass `fundFrom` + the matching
companion field (`gatewayAddress` / `withdrawalId` / `sources`).

## See also

- `docs/FUND_FROM_PLAN.md` — implementation plan + phase tracking.
- `docs/BEHAVIORAL_DIFFERENCES.md` BD-031 / BD-076 / BD-081.
- `docs/INSTRUCTION_REFERENCE.md` — on-chain ix reference.
- `sdk/src/solana/funding-plan.ts` — Lua-faithful planner source.
- `programs/ario-gar/src/instructions/payment.rs` — `pay_from_funding_plan`
  on-chain handler.
- Lua reference: `ar-io-network-process/src/gar.lua` (functions
  `getFundingPlan` line 1421 and `applyFundingPlan` line 1629).
