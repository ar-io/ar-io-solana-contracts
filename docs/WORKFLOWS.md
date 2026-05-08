# AR.IO Network: Protocol Workflows

Complete reference for every major workflow in the AR.IO Solana protocol.
Organized by actor type, with cross-program interactions, state transitions,
and exact preconditions from the smart contract code.

**Programs:** ario-core, ario-gar, ario-arns, ario-ant

---

## Table of Contents

1. [Overview](#1-overview)
2. [End User Flows](#2-end-user-flows)
   - 2.1 Buy an ArNS Name
   - 2.2 Manage ANT Records & Controllers
   - 2.3 Request a Primary Name
   - 2.4 Vault Operations
3. [Gateway Operator Flows](#3-gateway-operator-flows)
   - 3.1 Join Network & Configure
   - 3.2 Leave Network & Withdraw
   - 3.3 Manage Delegate Allowlist
4. [Delegator Flows](#4-delegator-flows)
   - 4.1 Delegate Stake
   - 4.2 Redelegate Stake
   - 4.3 Withdraw & Claim
   - 4.4 Compound Delegation Rewards
5. [Epoch Lifecycle](#5-epoch-lifecycle)
   - 5.1 Full Pipeline
   - 5.2 Observation Submission
   - 5.3 Reward Computation & Distribution
   - 5.4 Gateway Pruning
6. [ArNS Name Lifecycle](#6-arns-name-lifecycle)
   - 6.1 Lease Expiry → Grace → Returned Auction
   - 6.2 Reserved Names
   - 6.3 Demand Factor Updates
7. [Migration](#7-migration)
8. [State Transition Summary](#8-state-transition-summary)

---

## 1. Overview

### Actors

| Actor | What they do | Programs touched |
|-------|-------------|-----------------|
| **End User** | Buy names, manage ANTs, create vaults, set primary names | ario-arns, ario-ant, ario-core |
| **Gateway Operator** | Run a gateway node, stake ARIO, configure settings | ario-gar |
| **Delegator** | Delegate ARIO to gateways for yield | ario-gar |
| **Observer** | Submit observation reports (prescribed gateway operators) | ario-gar |
| **Cranker** | Drive epoch pipeline, update demand factor, prune state | ario-gar, ario-arns |
| **Admin** | Reserve names, update config, manage migration | all programs |

### Cross-Program Map

```
┌──────────────────────────────────────────────────────────────────┐
│                                                                  │
│  ario-core                       ario-gar                        │
│  ┌────────────────────┐          ┌──────────────────────────┐    │
│  │ Token transfers     │◄──CPI───┤ Stake/reward transfers    │    │
│  │ Vaults              │◄──CPI───┤                           │    │
│  │ Primary names       │         │ Gateway registry          │    │
│  └──────┬─────────────┘          │ Epochs & distribution     │    │
│         │                        └────┬──────────────┬──────┘    │
│         │ read (ArnsRecord,           │              │           │
│         │  DemandFactor, ANT)         │read           │read      │
│         ▼                             │(NameRegistry) │(Gateway) │
│  ┌────────────────────┐          ┌────▼──────────────▼──────┐    │
│  │ ario-ant            │◄──ref───┤ ario-arns                 │    │
│  │ ANT NFT records     │         │ Name registry & pricing   │    │
│  │ Controllers         │         │ Demand factor              │    │
│  └────────────────────┘          └───────────────────────────┘    │
│                                                                  │
└──────────────────────────────────────────────────────────────────┘

CPI  = Cross-Program Invocation (token transfers to/from ario-core)
read = Cross-program account read via remaining_accounts (no CPI)
ref  = Client passes account reference (ANT asset ownership check)

Key cross-program reads:
  ario-core reads ario-arns: ArnsRecord + DemandFactor (primary name validation/fee)
  ario-core reads ario-ant:  ANT NFT asset (primary name ownership check)
  ario-gar  reads ario-arns: NameRegistry (epoch name prescription)
  ario-arns reads ario-gar:  Gateway account (operator discount verification)
  ario-arns reads ario-ant:  ANT NFT asset (buy_name validation)
```

---

## 2. End User Flows

### 2.1 Buy an ArNS Name

A user purchases a human-readable ArNS name (lease or permabuy), creating an
on-chain name record linked to a Metaplex Core NFT (the ANT).

```
User                    ario-arns              ario-ant           Metaplex
 │                         │                      │                  │
 │  1. Create NFT ─────────┼──────────────────────┼─────────────────►│
 │◄────────────────────────┼──────────────────────┼──────────────────│
 │                         │                      │                  │
 │  2. buy_name ──────────►│                      │                  │
 │     (tokens flow to     │                      │                  │
 │      protocol treasury) │                      │                  │
 │◄────── ArnsRecord ──────│                      │                  │
 │                         │                      │                  │
 │  3. initialize ─────────┼─────────────────────►│                  │
 │     (AntConfig +        │                      │                  │
 │      AntControllers +   │                      │                  │
 │      @ record created)  │                      │                  │
 │                         │                      │                  │
 │  4. set_record ─────────┼─────────────────────►│                  │
 │     (undername records) │                      │                  │
```

**Step 1 — Create Metaplex Core NFT** (off-chain / Metaplex SDK)
- User creates the ANT asset before purchasing a name
- The asset's initial Attributes plugin includes an `ANT Program` entry that names the program owning this asset's per-mint state PDAs (canonical `ARIO_ANT_PROGRAM_ID` for default flows; an alternative program ID for BYO-ANT). Both `migration/import` and the SDK's `spawnSolanaANT` write this trait at `CreateV1` time. See ADR-016 / BD-100.

**Step 2 — `buy_name`** (ario-arns)
- **Params:** name, purchase_type (Lease/Permabuy), years (1-5 for leases), ant (NFT pubkey)
- **Pricing:** `base_fee × demand_factor × year_multiplier`
  - Lease: `base_fee × demand_factor × (1 + 0.2 × years) / SCALE`
  - Permabuy: `base_fee × demand_factor × 5 / SCALE`
  - Base fees range from 200 ARIO (13+ chars) to 1,000,000 ARIO (1 char)
- **Gateway operator discount:** 20% off if buyer is an eligible gateway operator
  - Requirements: `Joined` status, 180+ day tenure, 90%+ observation pass rate
  - Verified via `remaining_accounts` (reads Gateway PDA from ario-gar, no CPI)
- **Preconditions:**
  - Name must not be reserved — `reserved_name_check` PDA is validated; if it has data, the reservation is checked: expired reservations are ignored, unexpired reservations require the buyer to be the `reserved_for` target (or fail if no target is set)
  - Name must not be in returned auction (checked via `returned_name_check` account)
  - ANT asset must be owned by `MPL_CORE_PROGRAM_ID`
  - Name validation: 1-51 chars, lowercase alphanumeric + hyphens, length 43 prohibited
- **Token flow:** buyer_token_account → protocol_token_account
- **State created:** ArnsRecord PDA, name added to NameRegistry

**Step 3 — `initialize`** (ario-ant)
- **Who:** ANT NFT holder (verified by reading Metaplex Core asset bytes)
- **Creates:** AntConfig + AntControllers (owner added as first controller) + @ record
- **Defaults:** ticker = "ANT", TTL = 900s (15 min)

**Step 4 — `set_record`** (ario-ant)
- **Who:** Owner, controller, or record owner (for existing records)
- **Creates/updates** undername records (e.g., `blog_myname`, `app_myname`)

**Optional follow-up instructions:**

| Action | Instruction | Program | Who |
|--------|------------|---------|-----|
| Extend lease | `extend_lease` | ario-arns | **Any ARIO holder** (matches Lua) |
| Upgrade to permabuy | `upgrade_name` | ario-arns | **Any ARIO holder** (matches Lua) |
| Add undernames | `increase_undername_limit` | ario-arns | **Any ARIO holder** (matches Lua) |
| Point to new ANT | `reassign_name` | ario-arns | ANT holder |
| Release name | `release_name` | ario-arns | ANT holder (permabuy only) |
| Sync ANT marketplace traits | `sync_attributes` | ario-arns | ANT holder (permissionless reconciliation) |

**Note on permissionless lease management:** `extend_lease`, `upgrade_name`, and
`increase_undername_limit` (plus their `_from_delegation` and
`_from_operator_stake` variants) are callable by any ARIO holder — the caller
just pays from their own balance or stake pool. This matches Lua's
`arns.extendLease`, `arns.upgradeRecord`, `arns.increaseUndernameLimit`. Only
`reassign_name` and `release_name` remain holder-gated because they are
stewardship transitions (handing control to a new ANT, returning the name to
the auction pool). See BD-095.

**Note on ANT traits:** When `caller == ant_owner`, each trait-mutating ARNS
instruction (`buy_name`, `upgrade_name`, `increase_undername_limit`) syncs the
ANT NFT's on-chain Attributes plugin via `UpdatePluginV1` CPI in the same tx.
When `caller != ant_owner` (e.g. a permissionless upgrade by a third party, or
a buy-on-behalf flow), the CPI is skipped silently and the on-chain
`ArnsRecord` becomes the authoritative state — the ANT holder runs
`sync_attributes` later to reconcile traits. The `_from_*` stake variants
never CPI into MPL Core, so they always defer trait sync. See ADR-012, BD-095,
and BD-096.

---

### 2.2 Manage ANT Records & Controllers

An ANT (Arweave Name Token) is a Metaplex Core NFT with on-chain config,
controller list, and undername records managed by the ario-ant program.

**Authorization model:**

| Role | Can do |
|------|--------|
| **Owner** (NFT holder) | Everything: records, controllers, metadata |
| **Controller** | Add/remove controllers, create/remove records, assign record owners |
| **Record owner** | Modify content of assigned record (target, target_protocol, TTL, metadata) |

**Key flows:**

**Add undername records:**
1. `set_record` — creates record PDA at `["ant_record", asset, hash(undername)]`
2. Optionally assign a `record_owner` who can update content without being a controller

**Transfer after marketplace sale:**
1. NFT changes hands via Metaplex transfer (e.g., Tensor, Magic Eden)
2. `reconcile` (permissionless) — if NFT owner has changed since `last_known_owner`, clears all controllers and updates `last_known_owner`. No-op if ownership unchanged.
3. New owner calls `add_controller` to set up their controllers

**Schema migration:**
- `migrate_ant` (permissionless) — bumps `version` field, uses `realloc` for account size changes

**Constraints:**
- Max 10 controllers per ANT (`MAX_CONTROLLERS = 10`)
- @ record cannot be removed (root record)
- @ record priority always 0
- Undername max length: 61 chars, TTL range: 60s–86,400s

---

### 2.3 Request a Primary Name

A primary name maps a Solana address to an ArNS name (reverse lookup).

**Flow A — Owner sets their own name (single tx):**

```
User (owns ArNS name "alice")
 │
 │  request_and_set_primary_name("alice")
 │────────────────────────────────────────►  ario-core
 │                                           │
 │  Validates:                               │
 │  - ArnsRecord exists & active (read from ario-arns)
 │  - Caller holds ANT NFT for "alice"       │
 │  - DemandFactor read for fee calculation  │
 │                                           │
 │  Fee: 0.2 ARIO × demand_factor           │
 │  Creates: PrimaryName + PrimaryNameReverse│
 │◄──────────────────────────────────────────│
```

**Flow B — Someone requests, name owner approves:**

```
Requester                     ario-core              Name Owner
 │                               │                       │
 │  1. request_primary_name ────►│                       │
 │     (fee: 0.2 ARIO × df)     │                       │
 │◄── PrimaryNameRequest ───────│                       │
 │                               │                       │
 │                               │  2. approve ◄─────────│
 │                               │     (validates ANT    │
 │                               │      ownership)       │
 │◄── PrimaryName set ──────────│                       │
```

- Request expires after 7 days (`PRIMARY_NAME_REQUEST_EXPIRY = 604,800s`)
- `close_expired_request` (permissionless) cleans up expired requests
- Base name owner can revoke via `remove_primary_name_for_base_name`
- User removes their own via `remove_primary_name`
- Cross-program reads: ArnsRecord (ario-arns) and DemandFactor (ario-arns) via `remaining_accounts`

**Flow C — Undername record owner sets their own primary name (BD-097):**

When a name has the form `<undername>_<base>`, both `request_and_set_primary_name`
and `approve_primary_name` accept either the ANT NFT holder *or* the
`AntRecord.owner` for that undername. Base names always require ANT-holder auth.

```
Record Owner (AntRecord.owner = alice; alice does NOT hold the ANT NFT)
 │
 │  request_and_set_primary_name("alice_company")
 │     remaining_accounts:
 │       [0] ArnsRecord(company)
 │       [1] DemandFactor
 │       [2] ANT Metaplex Core asset (held by Bob)
 │       [3] AntRecord("alice", company-ant)  ← new
 │────────────────────────────────────────►  ario-core
 │                                           │
 │  ario-core sees nft_owner != caller,      │
 │  splits "alice_company" → undername "alice"│
 │  reads remaining[3] as AntRecord PDA,     │
 │  checks owner program / PDA / discriminator,│
 │  parses owner field → matches caller ✓    │
 │                                           │
 │  PrimaryName set                          │
 │◄──────────────────────────────────────────│
```

- The on-chain code only consults the AntRecord slot when the caller is *not*
  the NFT holder. The SDK's always-pass policy (`requestPrimaryName` always
  includes the AntRecord PDA for undernames) keeps the call shape uniform; the
  contract simply ignores the slot for ANT-holder callers.
- The AntRecord PDA is derived against the program named in the asset's
  `ANT Program` Attributes-plugin entry (canonical fallback when absent —
  ADR-016 / BD-100). Both the SDK and ario-core read this from the asset
  before deriving the PDA, so BYO-ANT undernames work transparently.
- Lazy reconciliation in ario-ant clears `AntRecord.owner` whenever the NFT
  has been transferred since the last record write, so a stale record owner
  cannot override a new ANT holder.
- The ANT holder retains the safety valve: `remove_primary_name_for_base_name`
  is unchanged and lets the holder nuke any primary name set on their domain.

---

### 2.4 Vault Operations

Time-locked token vaults. Users can lock tokens for themselves or send locked
tokens to others (optionally revocable).

**Self-vault (lock tokens):**
1. `create_vault` — lock `amount` for `duration` seconds
2. Wait for lock to expire
3. `release_vault` (vault owner only) — tokens return to owner

**Vaulted transfer (send locked tokens):**
1. `vaulted_transfer` — sender creates vault for recipient, optionally `revocable`
2. If revocable: sender can call `revoke_vault` before expiry to reclaim
3. After expiry: recipient calls `release_vault` to claim

**Other operations:**
- `extend_vault` — extend lock duration (owner only, before expiry)
- `increase_vault` — add more tokens (owner only, before expiry)

**Constraints:**
- Duration: 14 days – 200 years (`DEFAULT_MIN_VAULT_DURATION` / `DEFAULT_MAX_VAULT_DURATION`)
- Minimum size: 100 ARIO (`MIN_VAULT_SIZE = 100,000,000 mARIO`)
- `revoke_vault` fails if vault has expired (tokens belong to recipient)
- `release_vault` fails if vault has NOT expired

---

## 3. Gateway Operator Flows

### 3.1 Join Network & Configure

A gateway operator stakes ARIO and registers their node on the network.

```
Operator                          ario-gar
 │                                   │
 │  join_network ───────────────────►│
 │  (stake ≥ 20,000 ARIO)           │
 │                                   │──► Gateway PDA created (status: Joined)
 │                                   │──► ObserverLookup PDA created
 │                                   │──► Added to GatewayRegistry
 │                                   │──► Tokens: operator → stake_account
 │◄──────────────────────────────────│
 │                                   │
 │  update_gateway_settings ────────►│  (optional, any time while Joined)
 │  update_observer_address ────────►│  (optional, changes observer key)
 │  increase_operator_stake ────────►│  (optional, adds more stake)
```

**`join_network` params:**
- `operator_stake` — must be ≥ `settings.min_operator_stake` (20,000 ARIO)
- `label` (1-64 chars), `fqdn` (1-128 chars), `port`, `protocol` (Http/Https)
- `observer_address` — separate key for observation submission
- `allow_delegated_staking`, `delegate_reward_share_ratio` (0-95%), `min_delegate_stake`

**Observer address uniqueness:** The `ObserverLookup` PDA at `["observer_lookup", observer_address]` enforces that no two gateways share the same observer address. `update_observer_address` closes the old lookup and creates a new one.

**`update_gateway_settings` constraints:**
- Gateway must be `Joined` (not `Leaving`)
- `delegate_reward_share_ratio` ≤ 95% (`MAX_DELEGATE_REWARD_SHARE = 9,500` basis points)
- `min_delegate_stake` ≥ global `settings.min_delegate_stake`

---

### 3.2 Leave Network & Withdraw

```
Operator                          ario-gar
 │                                   │
 │  leave_network ──────────────────►│
 │                                   │──► status: Joined → Leaving
 │                                   │──► Removed from GatewayRegistry
 │                                   │──► Withdrawal created (90-day lock)
 │                                   │──► operator_stake zeroed
 │                                   │──► ObserverLookup closed
 │◄──────────────────────────────────│
 │                                   │
 │  ... 90 days pass ...             │
 │                                   │
 │  claim_withdrawal ───────────────►│
 │                                   │──► Tokens: stake_account → operator
 │◄──────────────────────────────────│
```

**90-day exit period:** `GATEWAY_LEAVE_PERIOD = 7,776,000 seconds`

**What happens to delegates:** They are NOT automatically withdrawn. Each
delegator must individually call `claim_delegate_from_leaving_gateway` to create
their own 30-day withdrawal.

**Early withdrawal option:** `instant_withdrawal` with a time-decaying penalty:
- At day 0: 50% penalty (`MAX_EXPEDITED_WITHDRAWAL_PENALTY = 500,000`)
- At day 45: 30% penalty
- At day 90: 10% penalty (`MIN_EXPEDITED_WITHDRAWAL_PENALTY = 100,000`)
- Formula: `penalty = max_penalty - (max_penalty - min_penalty) × elapsed / total_period`
- Penalty tokens go to protocol treasury

**Cancel withdrawal:** `cancel_withdrawal` returns stake to gateway — only if gateway is NOT `Leaving`.

---

### 3.3 Manage Delegate Allowlist

Gateway operators can restrict who may delegate to their gateway.

| Instruction | Effect |
|-------------|--------|
| `set_allowlist_enabled(true)` | New delegators must be on allowlist (existing delegators keep stake) |
| `set_allowlist_enabled(false)` | Anyone can delegate |
| `allow_delegate(address)` | Add address to allowlist |
| `disallow_delegate(address)` | Remove from allowlist (existing stake unaffected) |

When allowlist is enabled: new delegations (amount = 0) require an AllowlistEntry PDA.
Existing delegators (amount > 0) can always add more stake regardless of allowlist status.

---

## 4. Delegator Flows

### 4.1 Delegate Stake

```
Delegator                         ario-gar
 │                                   │
 │  delegate_stake ─────────────────►│
 │  (amount ≥ gateway.min_delegate)  │
 │                                   │──► Delegation PDA created/updated
 │                                   │──► reward_debt = gateway.cumulative_reward_per_token
 │                                   │──► gateway.total_delegated_stake += amount
 │                                   │──► Tokens: delegator → stake_account
 │◄──────────────────────────────────│
```

**Preconditions:**
- Gateway must be `Joined` and `allow_delegated_staking = true`
- Amount ≥ `gateway.settings.min_delegation_amount`
- If allowlist enabled and new delegation: must have AllowlistEntry PDA
- Delegator cannot delegate to their own gateway

**If delegation already exists:** Pending rewards are settled first (via reward-per-share
accumulator), then amount is added.

---

### 4.2 Redelegate Stake

Move delegation from one gateway to another, with an escalating fee that resets
every 7 days.

```
Delegator                         ario-gar
 │                                   │
 │  redelegate_stake ───────────────►│
 │  (source_gw → target_gw)         │
 │                                   │──► Settle rewards on both delegations
 │                                   │──► Fee deducted, sent to protocol
 │                                   │──► Net amount moved to target delegation
 │                                   │──► RedelegationRecord.count incremented
 │◄──────────────────────────────────│
```

**Fee schedule** (resets after 7 days of no redelegations):

| Redelegation # | Fee | RATE_SCALE value |
|---------------|-----|------------------|
| 1st (free) | 0% | 0 |
| 2nd | 10% | 100,000 |
| 3rd | 20% | 200,000 |
| 4th | 30% | 300,000 |
| 5th | 40% | 400,000 |
| 6th | 50% | 500,000 |
| 7th+ | 60% (cap) | 600,000 |

`FEE_RESET_INTERVAL = 604,800 seconds` (7 days)

**Preconditions:**
- Source gateway must be `Joined`
- Target gateway must be `Joined`, allow delegations, meet min amount after fee
- Source remaining must be 0 or ≥ min_delegation_amount

---

### 4.3 Withdraw & Claim

**Standard withdrawal:**
1. `decrease_delegate_stake(amount)` — creates Withdrawal PDA with 30-day lock
2. Wait 30 days
3. `claim_withdrawal` — tokens returned to delegator

**Expedited withdrawal:**
1. `decrease_delegate_stake(amount)` — creates Withdrawal PDA
2. `instant_withdrawal` — immediate payout minus penalty (50% → 10% linear decay)

**Cancel:**
- `cancel_withdrawal` — returns stake to delegation (only if gateway is NOT `Leaving`)
- Settles pending rewards and updates reward_debt

**Forced exit (gateway leaving):**
1. Gateway operator calls `leave_network`
2. Delegator calls `claim_delegate_from_leaving_gateway` — settles pending rewards first via `settle_delegate_rewards()`, then creates 30-day withdrawal for the full delegation amount (rewards included)
3. `claim_withdrawal` after maturity

**Minimum check:** When decreasing, remaining delegation must be 0 or ≥ `min_delegation_amount`.
After withdrawal, `close_empty_delegation` (permissionless) cleans up zero-balance delegations.

---

### 4.4 Compound Delegation Rewards

Delegate rewards accumulate via a reward-per-share accumulator (Sushi MasterChef
pattern). Rewards are "virtual" until settled.

```
Delegator                         ario-gar
 │                                   │
 │  compound_delegation_rewards ────►│
 │                                   │
 │  pending = delegation.amount      │
 │    × (gateway.cumulative_reward_per_token - delegation.reward_debt)
 │    / REWARD_PRECISION (1e18)      │
 │                                   │
 │  delegation.amount += pending     │
 │  delegation.reward_debt = gateway.cumulative_reward_per_token
 │◄──────────────────────────────────│
```

This materializes pending rewards into the delegation amount. Should be called
periodically (or is called automatically before any delegation modification).

---

## 5. Epoch Lifecycle

### 5.1 Full Pipeline

Each epoch lasts 24 hours. A cranker (or anyone) drives the pipeline through
6 permissionless steps with on-chain idempotency guards.

```
                              24-hour epoch
 ◄──────────────────────────────────────────────────────────────────►

 ┌────────────┐ ┌─────────────┐ ┌───────────┐        ┌─────────────┐ ┌───────┐
 │  create    │►│   tally     │►│ prescribe │► ... ►  │ distribute  │►│ close │
 │  epoch     │ │  weights    │ │   epoch   │        │   epoch     │ │ epoch │
 └────────────┘ └─────────────┘ └───────────┘        └─────────────┘ └───────┘
   1 tx          batched         1 tx        observers  batched       7+ epochs
                 (~15/tx)                    submit     (~15/tx)      later
```

| Step | Instruction | Batched? | Guard | Done flag |
|------|------------|----------|-------|-----------|
| 1 | `create_epoch` | No | `clock ≥ genesis + index × duration` | Anchor `init` (PDA uniqueness) |
| 2 | `tally_weights` | Yes (remaining_accounts) | `weights_tallied == 0` | `tally_index ≥ active_count → weights_tallied = 1` |
| 3 | `prescribe_epoch` | No | `weights_tallied ≠ 0`, `prescriptions_done == 0` | `prescriptions_done = 1` |
| 4 | `save_observations` | Per-observer | `prescriptions_done ≠ 0`, within epoch window | Observation PDA uniqueness |
| 5 | `distribute_epoch` | Yes (remaining_accounts) | `prescriptions_done ≠ 0`, `clock ≥ end`, `rewards_distributed == 0` | `distribution_index ≥ active_count → rewards_distributed = 1` |
| 6 | `close_epoch` | No | `rewards_distributed ≠ 0`, `current_index ≥ epoch_index + 7` | Anchor `close` |

**Idempotency:** All guards make it safe for multiple crankers to run simultaneously.
The worst case is a wasted transaction fee (~0.000005 SOL).

**Step 1 — `create_epoch`:**
- Computes hashchain entropy: `SHA256(slot ∥ epoch_index ∥ timestamp)` (24 bytes)
- Snapshots `active_gateway_count` from GatewayRegistry
- Computes reward rate (linear decay: 0.1% → 0.05% over epochs 365–547)
- Calculates `total_eligible_rewards = protocol_balance × reward_rate / RATE_SCALE`

**Step 2 — `tally_weights`** (batched, ~15 gateways per tx):
- For each gateway, computes 4-factor composite weight:
  ```
  stake_weight     = total_stake / min_operator_stake
  tenure_weight    = min(time_running / 180 days, 4)
  gw_performance   = (1 + passed) / (1 + total)
  obs_performance  = (1 + observed) / (1 + prescribed)
  composite        = stake × tenure × gw_perf × obs_perf / SCALE³
  ```
- Gateways that joined after epoch start get composite = 0
- Caches composite in registry slot, accumulates into `epoch.total_composite_weight`

**Step 3 — `prescribe_epoch`:**
- **Observer selection** — weighted roulette using hashchain entropy:
  - Up to 50 observers selected (`MAX_OBSERVERS_PER_EPOCH = 50`)
  - Uses 10× multiplier loop (`max_observers × 10` iterations, up to 500) to handle equal-weight collisions
  - Each iteration: `hash = SHA256(hash)`, `random = u128(hash[..16]) % total_weight`
  - Walk registry accumulating weights until cumulative > random → select that gateway
- **Name selection** — 2 names from NameRegistry via same roulette
  - Reads NameRegistry from ario-arns via `remaining_accounts` (cross-program read)
- **Reward computation:**
  ```
  per_gateway  = (total_eligible × 90% / SCALE) / active_count
  per_observer = (total_eligible × 10% / SCALE) / observer_count
  ```

---

### 5.2 Observation Submission

Prescribed observers submit pass/fail reports for gateways they monitored.

```
Observer                          ario-gar
 │                                   │
 │  save_observations ──────────────►│
 │  (gateway_results bitmap,         │
 │   report_tx_id)                   │
 │                                   │──► Observation PDA created
 │                                   │──► epoch.failure_counts[i] updated
 │                                   │──► epoch.has_observed bitmap set
 │                                   │──► epoch.observations_submitted++
 │◄──────────────────────────────────│
```

**Timing window:** Only during `[epoch.start_timestamp, epoch.end_timestamp)`

**Bitmap format:** `gateway_results` is a byte array covering up to 3,000 gateways.
- Bit 1 = passed, Bit 0 = failed
- `byte_idx = i / 8`, `bit_idx = i % 8`, `passed = (results[byte_idx] >> bit_idx) & 1`
- `gateway_count` param must exactly match `epoch.active_gateway_count`

**One submission per observer:** Observation PDA at `["observation", epoch_index, observer]`
prevents double-submission via Anchor `init` constraint.

**Cleanup:** `close_observation` (permissionless) after epoch is fully distributed.

---

### 5.3 Reward Computation & Distribution

`distribute_epoch` processes gateways in batches (~15/tx). For each gateway:

**1. Determine pass/fail:**
```
failed = (observations_submitted > 0) AND (failure_counts[i] > observations_submitted / 2)
```

**2. Calculate reward by scenario:**

| Passed | Prescribed | Observed | Leaving | Reward |
|--------|-----------|----------|---------|--------|
| Yes | Yes | Yes | No | per_gateway + per_observer |
| Yes | Yes | No | No | per_gateway × (1 − 25%) |
| Yes | No | — | No | per_gateway |
| No | Yes | Yes | No | per_observer only |
| No | Yes | No | No | 0 |
| No | No | — | No | 0 |
| — | — | — | Yes | 0 (leaving gateways excluded) |

Missed observation penalty: `MISSED_OBSERVATION_PENALTY = 250,000` (25% of RATE_SCALE)

**3. Split between operator and delegates:**
```
delegate_pool   = reward × delegate_reward_share_ratio / 10,000
operator_reward = reward − delegate_pool
```

- Operator reward always compounded into `gateway.operator_stake` (no `auto_stake` check — differs from Lua where non-auto-stake operators receive wallet transfers). Operators use `decrease_operator_stake` to withdraw.
- Delegate pool distributed via accumulator:
  ```
  increment = delegate_pool × REWARD_PRECISION / total_delegated_stake
  gateway.cumulative_reward_per_token += increment
  ```
  (`REWARD_PRECISION = 1e18`)

**4. Update stats:**
- Pass: `passed_epochs++`, `passed_consecutive++`, `failed_consecutive = 0`
- Fail: `failed_epochs++`, `failed_consecutive++`, `passed_consecutive = 0`

**5. Failed gateway slashing:**
- Failed gateways receive no reward (reward = 0)
- No additional stake slashing during distribution (slashing only occurs via `prune_gateway`)

**6. SPL transfer:**
- Batch total reward calculated, capped at available protocol balance
- Single transfer per batch: protocol_token_account → stake_token_account

---

### 5.4 Gateway Pruning

Gateways with 30+ consecutive failures are eligible for permissionless pruning.

```
Anyone                            ario-gar
 │                                   │
 │  prune_gateway ──────────────────►│
 │                                   │
 │  Check: failed_consecutive ≥ 30   │
 │  Check: status == Joined          │
 │                                   │
 │  Slash: min(min_operator_stake, operator_stake) → protocol
 │  Remaining stake → Withdrawal (90-day lock)
 │  Status → Leaving                 │
 │  Removed from GatewayRegistry     │
 │◄──────────────────────────────────│
```

`MAX_CONSECUTIVE_FAILURES = 30` (configurable in EpochSettings)
Slash rate: 100% of min_operator_stake (`FAILED_GATEWAY_SLASH_RATE = 1,000,000`)

---

## 6. ArNS Name Lifecycle

### 6.1 Lease Expiry → Grace → Returned Auction

Names go through a multi-phase lifecycle after a lease expires.

```
Timeline:
│◄── Active Lease ──►│◄── Grace (14d) ──►│◄── Dutch Auction (14d) ──►│◄── Available ──►│

States:
  ArnsRecord active    ArnsRecord active    ReturnedName exists         Name available
  (end_timestamp set)  (can still extend)   (premium decays 50x→1x)    for buy_name
```

**Phase 1 — Active lease:**
- Owner can `extend_lease`, `upgrade_name`, `reassign_name`

**Phase 2 — Grace period** (14 days after `end_timestamp`):
- `LEASE_GRACE_PERIOD = 1,209,600 seconds`
- Owner can still `extend_lease` or `upgrade_name` to save the name
- Name remains in NameRegistry

**Phase 3 — Returned name (Dutch auction):**
- Triggered by `prune_name_to_returned` (permissionless)
- Creates ReturnedName PDA, closes ArnsRecord, removes from NameRegistry
- Premium decays linearly: 50× at start → 1× at end over 14 days
  - `RETURNED_NAME_MAX_MULTIPLIER = 50`
  - `RETURNED_NAME_DURATION_SECONDS = 1,209,600`
  - Formula: `multiplier = 50 × (duration − elapsed) / duration`
  - Cost: `registration_fee × multiplier / SCALE`
- Anyone can buy via `buy_returned_name`
- Revenue split: 50% protocol / 50% to initiator (or 100% protocol if protocol-initiated)

**Phase 4 — Available:**
- After auction expires: `prune_returned_names` (permissionless) cleans up ReturnedName PDA
- Name becomes available for regular `buy_name`

**Direct prune shortcut:** `prune_expired_names` can skip the returned auction phase entirely for names that are past **both** the grace period and the auction window (`end + grace + auction_duration`). This handles names where no one called `prune_name_to_returned` during the auction window — the ArnsRecord is closed directly without creating a ReturnedName.

**Permabuy release:**
- Owner calls `release_name` → creates ReturnedName (enters Dutch auction)
- Only permabuy names can be released (leases expire naturally)

---

### 6.2 Reserved Names

Admin can reserve names for specific addresses or as general holds.

| Instruction | Who | Effect |
|-------------|-----|--------|
| `reserve_name` | Admin (`config.authority`) | Creates ReservedName PDA with optional `reserved_for` and `expires_at` |
| `claim_reserved_name` | Admin | Claims the reserved name (uses standard pricing) |
| `unreserve_name` | Admin | Removes reservation |
| `prune_expired_reservation` | Anyone (permissionless) | Cleans up expired reservations |

Reserved names block `buy_name` — the reserved_name_check account validation fails
if a ReservedName PDA exists for the name.

---

### 6.3 Demand Factor Updates

Dynamic pricing adjustment that runs once per period (24 hours).

```
Anyone                            ario-arns
 │                                   │
 │  update_demand_factor ───────────►│
 │                                   │
 │  For each missed period:          │
 │    if metric > moving_avg:        │
 │      factor *= 1.05               │  (metric = purchases or revenue,
 │    else:                          │   based on config.criteria)
 │      factor *= 0.985              │
 │      floor at 0.5                 │
 │                                   │
 │    if at floor for 7 periods:     │
 │      HALVE all genesis fees       │
 │      reset factor to 1.0          │
 │◄──────────────────────────────────│
```

**Constants:**
- `DEMAND_FACTOR_UP_ADJUSTMENT = 1,050,000` (1.05×)
- `DEMAND_FACTOR_DOWN_ADJUSTMENT = 985,000` (0.985×)
- `DEMAND_FACTOR_MIN = 500,000` (0.5× floor)
- `MAX_PERIODS_AT_MIN_DEMAND_FACTOR = 7`
- `PERIOD_LENGTH_SECONDS = 86,400` (1 day)
- `MOVING_AVG_PERIOD_COUNT = 7` (trailing window)

**Fee halving:** When the demand factor stays at minimum for 7 consecutive periods,
ALL genesis fees are permanently halved and the factor resets to 1.0. This is
irreversible and ensures long-term price discovery.

---

## 7. Migration

One-time import of all AO state to Solana. All migration instructions are gated by
`migration_active == true` and restricted to `migration_authority`.

```
Migration Authority               All Programs
 │                                   │
 │  import_account (batched) ───────►│  Pre-serialized account data
 │  import_registry_entry ──────────►│  GatewayRegistry / NameRegistry entries
 │                                   │
 │  ... ~17,500 transactions ...     │
 │                                   │
 │  finalize_supply ────────────────►│  ario-core: set supply totals
 │  finalize_migration ─────────────►│  Each program: permanently disable imports
 │                                   │  (migration_active = false, irreversible)
```

**Per-program migration instructions:**

| Program | Instructions |
|---------|-------------|
| ario-core | `import_account`, `finalize_supply`, `finalize_migration` |
| ario-gar | `import_account`, `import_registry_entry`, `finalize_migration` |
| ario-arns | `import_account`, `import_registry_entry`, `finalize_migration` |
| ario-ant | `initialize_migration`, `import_account`, `finalize_migration` |

See `docs/MIGRATION_ARCHITECTURE.md` for full specification.

---

## 8. State Transition Summary

### Gateway

```
                 join_network          leave_network / prune_gateway
  (not exist) ─────────────► Joined ──────────────────────────────► Leaving
                               │                                      │
                               │ increase/decrease stake              │ claim_withdrawal
                               │ update settings                      │ instant_withdrawal
                               │ participate in epochs                │
                               │                                      ▼
                               │                                   (withdrawn)
```

### Delegation

```
                delegate_stake         decrease_delegate_stake
  (not exist) ─────────────► Active ──────────────────────────► Withdrawal Pending
                               │                                      │
                               │ compound rewards                     │ claim (90d)
                               │ redelegate                           │ instant (penalty)
                               │                                      │ cancel → Active
                               │                                      ▼
                               │ close_empty_delegation            (withdrawn)
                               ▼
                            (closed)
```

### ArNS Name (Lease)

```
               buy_name                        grace expires        auction expires
  Available ──────────► Active (lease) ──────► Grace Period ──────► Returned ──────► Available
                          │                      │                    │
                          │ extend_lease         │ extend/upgrade     │ buy_returned_name
                          │ upgrade → Permabuy   │ saves name         │ (Dutch auction)
                          │ reassign_name        │                    │
                          ▼                      │                    │
                       Permabuy ─── release ─────┼────────────────────┘
                          (permanent)            │
                                               (prune_name_to_returned)
```

### Epoch

```
  create_epoch    tally_weights     prescribe     [observations]   distribute     close
  ───────────► Created ──────────► Tallied ──────► Prescribed ──────► Distributed ──► Closed
                 │                   │                │                  │
                 │ weights_tallied=0 │ =1             │ prescriptions    │ rewards
                 │ tally_index=0     │                │ _done=1          │ _distributed=1
```

### Withdrawal

```
               decrease_stake          30 days pass
  (created) ─────────────► Pending ──────────────────► Claimable ──► (claimed)
                             │                                         ▲
                             │ instant_withdrawal ─────────────────────┘
                             │   (10-50% penalty)
                             │
                             │ cancel_withdrawal ──► (returned to stake)
```

### Vault

```
              create_vault              time passes
  (created) ───────────► Locked ──────────────────► Expired ──► release_vault ──► (closed)
                           │                                        ▲
                           │ revoke_vault (if revocable) ──► (reclaimed by controller)
                           │ extend_vault (extend lock)
                           │ increase_vault (add tokens)
```
