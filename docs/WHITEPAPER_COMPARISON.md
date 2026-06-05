# Whitepaper v3.0.0 vs Code: Full Parameter Comparison

> Generated 2026-05-29. Compares the [v3.0.0 whitepaper](https://html_whitepaper.ar.io/)
> against the `develop` branch at commit `3ce44e0`.

---

## MATCHING PARAMETERS (no discrepancies)

### Token (Section 5)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Total supply | 1,000,000,000 ARIO | Set at initialize (fixed) | Match |
| Decimals | 6 | `TOKEN_DECIMALS = 6` | Match |
| Sub-unit | 1 ARIO = 1,000,000 μARIO | `ONE_TOKEN = 1_000_000` | Match |
| Mint authority | Shall be revoked | ADR-015: revoked post-migration | Match |

### Gateway Staking (Table 6.2)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Max gateways | 3,000 | `MAX_GATEWAYS = 3000` | Match |
| Min operator stake | 20,000 ARIO | `MIN_OPERATOR_STAKE = 20_000_000_000` mARIO | Match |
| Excess stake withdraw | 30 days | `WITHDRAWAL_LOCK_PERIOD = 2,592,000` sec | Match |
| Leave duration | 90 days | `GATEWAY_LEAVE_PERIOD = 7,776,000` sec | Match |

### Delegated Staking (Table 6.3)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Min delegation | 10 ARIO | `MIN_DELEGATION_AMOUNT = 10,000,000` mARIO | Match |
| Delegate reward share | 0-95% | `MAX_DELEGATE_REWARD_SHARE = 9500` (of 10,000) | Match |
| Delegate withdraw | 30 days | `WITHDRAWAL_LOCK_PERIOD = 2,592,000` sec | Match |
| Max delegates/gateway | 10,000 | `max_delegates_per_gateway = 10_000` | Match |

### Redelegation (Section 6.4)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Free window | 1 free per 7 days | `FEE_RESET_INTERVAL = 604,800` sec; count=0 → 0% | Match |
| Fee increment | +10% per extra | `MIN_REDELEGATION_PENALTY = 100,000` (10%) | Match |
| Fee cap | 60% | `MAX_REDELEGATION_PENALTY = 600,000` (60%) | Match |
| Fees go to | Protocol balance | Yes | Match |

### Expedited Withdrawal (Section 6.6)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Max penalty | 50% | `MAX_EXPEDITED_WITHDRAWAL_PENALTY = 500,000` | Match |
| Min penalty | 10% | `MIN_EXPEDITED_WITHDRAWAL_PENALTY = 100,000` | Match |
| Formula | `max - (max-min)*elapsed/total` | Same | Match |
| Min amount | — | `MIN_EXPEDITED_WITHDRAWAL_AMOUNT = 1,000,000` (1 ARIO) | Code-only (not in WP) |

### Genesis Fee Table (Table 12.3)
| Length | Whitepaper (ARIO) | Code (mARIO) | Status |
|--------|------------------|--------------|--------|
| 1-char | 500,000 | 500,000,000,000 | Match |
| 2-char | 100,000 | 100,000,000,000 | Match |
| 3-char | 10,000 | 10,000,000,000 | Match |
| 4-char | 5,000 | 5,000,000,000 | Match |
| 5-char | 2,500 | 2,500,000,000 | Match |
| 6-char | 1,500 | 1,500,000,000 | Match |
| 7-char | 800 | 800,000,000 | Match |
| 8-char | 500 | 500,000,000 | Match |
| 9-char | 400 | 400,000,000 | Match |
| 10-char | 350 | 350,000,000 | Match |
| 11-char | 300 | 300,000,000 | Match |
| 12-char | 250 | 250,000,000 | Match |
| 13-51 | 200 | 200,000,000 | Match |

### ArNS Pricing Formulas (Section 12.3)
| Formula | Whitepaper | Code | Status |
|---------|-----------|------|--------|
| Lease | `BRF × DF × (1 + 0.2 × years)` | `base * df * (SCALE + 200_000 * years) / SCALE²` | Match |
| Lease extension | `BRF × DF × 0.2 × years` | `base * df * 200_000 * years / SCALE²` | Match |
| Permabuy | `BRF × DF × (1 + 0.2 × 20) = BRF × DF × 5` | `base * df * (SCALE + 200_000 * 20) / SCALE²` | Match |
| Undername (lease) | `BRF × DF × 0.1%` | `UNDERNAME_LEASE_FEE_PCT = 1,000` (0.1%) | Match |
| Undername (permabuy) | `BRF × DF × 0.5%` | `UNDERNAME_PERMABUY_FEE_PCT = 5,000` (0.5%) | Match |
| Annual fee | `ARF × 20%` | `ANNUAL_PERCENTAGE_FEE = 200,000` (20%) | Match |
| Permabuy equivalent years | 20 | `PERMABUY_LEASE_FEE_LENGTH_YEARS = 20` | Match |

### ArNS Name Rules (Section 9.2)
| Rule | Whitepaper | Code | Status |
|------|-----------|------|--------|
| Valid chars | 0-9, a-z, dashes | Validated in pricing.rs | Match |
| No leading/trailing dash | Yes | Yes | Match |
| Length range | 1-51 chars | `MIN=1, MAX=51` | Match |
| 43-char prohibited | Yes (Arweave TXID collision) | `ARWEAVE_ADDRESS_LENGTH = 43` | Match |
| Max lease | 5 years | `MAX_LEASE_YEARS = 5` | Match |
| Grace period | 2 weeks (14 days) | `LEASE_GRACE_PERIOD = 1,209,600` sec | Match |
| Default undernames | 10 | `DEFAULT_UNDERNAME_LIMIT = 10` | Match |
| Max undernames | 10,000 | Yes | Match |

### Demand Factor (Section 9.6 / 12.3)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Increase rate | +5.0% | `UP_ADJUSTMENT = 1,050,000` | Match |
| Decrease rate | -1.5% | `DOWN_ADJUSTMENT = 985,000` | Match |
| Minimum | 0.5 | `DEMAND_FACTOR_MIN = 500,000` | Match |
| Maximum | Unbounded | No upper cap | Match |
| Starting value | 1.0 | Initialized from params | Match |
| Period | 1 day | `PERIOD_LENGTH_SECONDS = 86,400` | Match |
| Moving avg window | 7 periods | `MOVING_AVG_PERIOD_COUNT = 7` | Match |
| Step pricing trigger | 7 consecutive periods at min | `MAX_PERIODS_AT_MIN = 7` | Match |
| Step: halve fees, reset DF | Yes | Yes | Match |

### Gateway Operator Discount (Section 9.8 / 12.3)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Discount | 20% | `GATEWAY_OPERATOR_DISCOUNT_PCT = 200,000` | Match |
| GPRW requirement | ≥ 0.9 | `ratio >= 900,000` | Match |
| TW requirement | ≥ 1.0 (6 months) | `DISCOUNT_MIN_TENURE = 15,552,000` sec (180 days) | Match |
| Leaving ineligible | Yes | Yes | Match |

### Epoch & Rewards (Section 10)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Epoch duration | Daily (1 day) | `DEFAULT_EPOCH_DURATION = 86,400` sec | Match |
| Max observers | 50 | `MAX_OBSERVERS_PER_EPOCH = 50` | Match |
| Prescribed names | 2 | `DEFAULT_PRESCRIBED_NAME_COUNT = 2` | Match |
| Reward rate (year 1) | 0.1% per epoch | `MAX_REWARD_RATE = 1,000` | Match |
| Reward rate (min) | 0.05% per epoch | `MIN_REWARD_RATE = 500` | Match |
| Decay start | After 1 year (365 epochs) | `REWARD_DECAY_START_EPOCH = 365` | Match |
| Decay end | 6 months later | `REWARD_DECAY_LAST_EPOCH = 547` (365+182) | Match |
| Gateway share | 90% | `GATEWAY_OPERATOR_REWARD_RATE = 900,000` | Match |
| Observer share | 10% | `OBSERVER_REWARD_RATE = 100,000` | Match |
| Missed observation penalty | 25% of gateway reward | `MISSED_OBSERVATION_PENALTY = 250,000` | Match |
| Batch size | 15 gateways/tx | `DISTRIBUTION_BATCH_SIZE = 15` | Match |

### Weight Formulas (Section 10.4)
| Weight | Whitepaper | Code | Status |
|--------|-----------|------|--------|
| SW | `(stake + delegated) / min_stake` | Same | Match |
| TW | `tenure / 6-months`, cap 4 | `tenure / 180d`, cap 4 | Match |
| GPRW | `(1+passed) / (1+total)` | Same | Match |
| OPRW | `(1+submitted) / (1+selected)` | Same | Match |
| CW | `SW × TW × GPRW × OPRW` | Same (with SCALE^3 normalization) | Match |

### Gateway Pruning (Section 10.8)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Consecutive failures | 30 epochs | `MAX_CONSECUTIVE_FAILURES = 30` | Match |
| Slash rate | 100% of min stake | `FAILED_GATEWAY_SLASH_RATE = 1,000,000` | Match |
| Permissionless | Yes | Yes | Match |
| Slashed to | Protocol balance | Yes | Match |

### Pass/Fail Determination (Section 10.5)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Pass threshold | ≥50% observers say PASS | `failed if failure_count > submitted/2` | Match |

### Vaults (Glossary)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| Min vault size | 100 ARIO | `MIN_VAULT_SIZE = 100,000,000` mARIO | Match |

### Returned Name Revenue Split (Section 9.2)
| Parameter | Whitepaper | Code | Status |
|-----------|-----------|------|--------|
| User return split | 50% owner / 50% protocol | Yes | Match |
| RNP window | 14 days | `RETURNED_NAME_DURATION_SECONDS = 1,209,600` | Match |
| Max premium | 50x | `RETURNED_NAME_MAX_MULTIPLIER = 50` | Match |

---

## DISCREPANCIES — Numeric Parameters

### 1. ANT Max Controllers — Whitepaper: 10, Code: 4

**Severity: HIGH** — externally visible constraint, affects users

| | Whitepaper | Code |
|--|-----------|------|
| Value | "Each ANT supports up to ten (10) controllers" (§9.4) | `MAX_CONTROLLERS = 4` |
| Code comment | — | "reduced from 10 → 4 (mainnet rent shrink, 2026-05-21); p99=2 controllers" |

The code intentionally reduced this from 10 to 4 for rent optimization. The whitepaper was not updated to reflect this change.

---

### 2. Returned Name Premium Formula — RESOLVED

**Status: FIXED** — code now matches the whitepaper formula

| | Whitepaper (§12.3) | Code (pricing.rs) |
|--|-----------|------|
| Formula | `RNP = 50 − (49/14) × t` | `(MAX × dur − (MAX−1) × elapsed) × SCALE / dur` |
| At t=0 | 50x | 50x |
| At t=7d | 25.5x | 25.5x |
| At t=14d | 1x | 1x (guard returns registration_fee) |
| Minimum during window | 1x | 1x |

The code previously used `50 × (duration − elapsed) / duration` which decayed to 0x
and had a boundary discontinuity / dead zone. It was updated to match the whitepaper's
`50 − (49/14) × t` formula, which decays smoothly from 50x to 1x. The dead zone
(where `require!(token_cost > 0)` rejected purchases near the end of the window) is
eliminated because the minimum in-window multiplier is now 1x.

---

### 3. Primary Name Fee — RESOLVED

**Status: FIXED** — the fee now varies by purchase type, matching the whitepaper

| | Whitepaper (§9.3 / §12.3) | Code (primary_name.rs) |
|--|-----------|------|
| Spec | "Equal to the fee for a single undername on a 51-char name **of equivalent purchase type**" | `PRIMARY_NAME_REQUEST_BASE_FEE_{LEASE,PERMABUY}` × DF, selected from the record's `purchase_type` |
| Lease primary name | 200 ARIO × 0.1% × DF = **0.2 ARIO × DF** | **0.2 ARIO × DF** (200,000 mARIO) |
| Permabuy primary name | 200 ARIO × 0.5% × DF = **1.0 ARIO × DF** | **1.0 ARIO × DF** (1,000,000 mARIO) |

The single `PRIMARY_NAME_REQUEST_BASE_FEE = 200,000` constant was split into
`PRIMARY_NAME_REQUEST_BASE_FEE_LEASE = 200,000` and
`PRIMARY_NAME_REQUEST_BASE_FEE_PERMABUY = 1,000,000`. All 4 primary-name request
handlers now read the `purchase_type` byte (offset 104) from the ArnsRecord and
charge the permabuy rate (5x) for permabuy names, matching the whitepaper's
"equivalent purchase type" requirement.

**Note on Lua divergence:** the Lua source uses `UNDERNAME_LEASE_FEE_PERCENTAGE`
unconditionally for primary name requests (per BD-031). This fix intentionally
diverges from Lua to follow the v3.0.0 whitepaper, which is canonical here.

---

### 4. Undername Max Length — Whitepaper: 51, Code: 61

**Severity: MEDIUM** — allows longer undernames than whitepaper specifies

| | Whitepaper (§9.4) | Code |
|--|-----------|------|
| Statement | "must not be longer than the total MAX_NAME_LENGTH of an ArNS name" (51) | `MAX_UNDERNAME_LENGTH = 61` |
| Code comment | — | "matches Lua MAX_UNDERNAME_LENGTH = 61" |

The code preserved the Lua-era 61-char limit for undernames. The whitepaper says undernames should have the same max as ArNS root names (51). The code follows the Lua source rather than the whitepaper.

---

## DISCREPANCIES — Behavioral Claims

### 5. Exit Vault Expedited Withdrawal — WP says eligible, code says NOT eligible

**Severity: HIGH** — directly contradicts whitepaper on user-facing economic mechanism

| | Whitepaper (§6.6) | Code |
|--|-----------|------|
| Claim | "When a gateway voluntarily leaves the network, the minimum stake becomes an exit vault and **is eligible for expedited withdrawal** on the same terms as other withdrawals." | Min-stake vault created with `is_protected: true`; `instant_withdrawal` rejects with `GarError::ProtectedVault` |

`leave_network` creates **two** vaults (per BD-102):
- **Protected exit vault** (min-stake portion): 90-day lock, `is_protected: true`, **CANNOT** be expedited.
- **Excess vault** (above-min portion): 30-day lock, `is_protected: false`, CAN be expedited.

The whitepaper's claim that the minimum stake exit vault "is eligible for expedited withdrawal" is **factually wrong** per the code. Only the excess portion can be expedited.

---

### 6. Disabling Delegation — RESOLVED

**Status: FIXED** — disabling delegation now forces delegates out and gates re-enable

| | Whitepaper (§6.3) | Code |
|--|-----------|------|
| Disable effect | "all delegates will have their tokens withdrawn, reducing total delegated stake to 0" | Permissionless `claim_delegate_from_disabled_gateway` cranks each delegate into a withdrawal vault (Solana can't iterate PDAs in one tx) |
| Re-enable guard | "cannot re-enable until all previous delegates have been withdrawn" | Re-enable requires `total_delegated_stake == 0` **and** a cooldown (`withdrawal_period`) since the disable timestamp |

Because Solana can't iterate all delegate PDAs in a single transaction, the
Lua "auto-withdraw all" becomes a permissionless cranking pattern (mirrors
`claim_delegate_from_leaving_gateway`). `update_gateway_settings` records
`delegation_disabled_at` when delegation is turned off; anyone can then crank
`claim_delegate_from_disabled_gateway` per delegate to move stake into 30-day
withdrawal vaults. Re-enabling is blocked until every delegate is cleared
(`total_delegated_stake == 0`) **and** the cooldown has elapsed, preventing
the operator from dumping delegates and immediately re-recruiting. Both new
fields live on `GatewaySettings2` (schema 1.1.0). See FIX_PLANS.md "Fix #6".

This supersedes the earlier BD-024 pull-based-model note for the disable path.

---

### 7. Gateway Setting Changes — RESOLVED (for `delegate_reward_share_ratio`)

**Status: FIXED** — the reward-split change is now deferred to the next epoch

| | Whitepaper (§6.3) | Code |
|--|-----------|------|
| `delegate_reward_share_ratio` | "changes go into effect the following epoch" | Staged in `pending_delegate_reward_share_ratio`; applied at the next `tally_weights` (epoch start), before `distribute_epoch` reads it |
| `allow_delegated_staking` | (per-tx gate) | Immediate — see #6; deferring would let delegates keep joining after the operator decided to stop |
| `min_delegation_amount` | (per-tx gate) | Immediate — a per-transaction minimum, no economic front-run vector |

The economically-sensitive setting — `delegate_reward_share_ratio` — is now
deferred so an operator can't front-run `distribute_epoch` to change their
reward split mid-epoch. `update_gateway_settings` writes the new value to
`pending_delegate_reward_share_ratio` (on `GatewaySettings2`, schema 1.1.0);
`tally_weights` applies pending→active at the start of the next epoch, and
`distribute_epoch` reads the now-epoch-stable active value. The other two
delegation settings stay immediate by design (they're per-transaction gates,
not epoch-economic levers). See FIX_PLANS.md "Fix #7".

---

### 8. Chosen Names Not Enforced On-Chain

**Severity: LOW** — design choice, not a bug

| | Whitepaper (§10.3) | Code |
|--|-----------|------|
| Claim | Observers evaluate "a set of two (2) prescribed names" plus "eight (8) chosen names" | `save_observations` accepts a pass/fail bitmap per gateway. No validation of which names were tested. Only 2 prescribed names stored on-chain. |

The "8 chosen names" are an off-chain protocol norm with no on-chain enforcement. The `Observation` PDA contains no field for tested name hashes. The protocol trusts observer reports.

---

## DISCREPANCIES — Low Severity / Informational

### 9. Number of Programs — Whitepaper: 4, Code: 5

| | Whitepaper (§4.2) | Code |
|--|-----------|------|
| Programs | "four Solana programs – ario-core, ario-gar, ario-arns, and ario-ant" | 5 programs (adds `ario-ant-escrow`) |

`ario-ant-escrow` handles trustless multi-protocol custody for the AO→Solana migration. Not part of the steady-state protocol.

---

### 10. Demand Factor Criteria — Code adds purchases option

| | Whitepaper (§12.3) | Code |
|--|-----------|------|
| Criteria | "Adjusts based on protocol revenue comparison to the RMA" | Configurable: `DEMAND_CRITERIA_REVENUE = 0` (default) or `DEMAND_CRITERIA_PURCHASES = 1` |

Default matches whitepaper. The purchases option is an undocumented enhancement.

---

### 11. Tenure Weight Period — "6 months" vs exactly 180 days

| | Whitepaper (§10.4) | Code |
|--|-----------|------|
| TW=1.0 at | "6-months" (~182.5 days) | `TENURE_WEIGHT_DURATION = 15,552,000` sec = **180 days** |
| TW=4.0 at | "2-years" (~730 days) | 4 × 180 = **720 days** |

10-day difference likely intentional for clean math (180 = 30 × 6).

---

### 12. Reward Distribution Is Flat Per-Gateway, Not Weight-Proportional

| | Whitepaper (§10.6) | Code |
|--|-----------|------|
| Implied | Rewards could be proportional to weight/stake | `per_gateway_reward = total_eligible * 0.9 / joined_count` — equal share regardless of weight |

Gateway weight only affects observer selection probability. All passing gateways receive the same base reward. This is Lua-parity and likely intentional, but the whitepaper does not explicitly state "equal share."

---

### 13. Code-Only Parameters (not in whitepaper)

| Parameter | Value | Notes |
|-----------|-------|-------|
| `MIN_EXPEDITED_WITHDRAWAL_AMOUNT` | 1 ARIO | Floor for expedited withdrawals |
| `OBSERVATION_WINDOW_SECONDS` | 3,600 sec | Defined but unused (full epoch window used per BD-082) |
| `MIN_VAULT_DURATION` | 14 days | Minimum vault lock period |
| `MAX_VAULT_DURATION` | 200 years | Maximum vault lock period |
| `PRIMARY_NAME_REQUEST_EXPIRY` | 7 days | Pending request expiry |
| `EPOCH_DISABLE_DELAY` | 7 days | Timelock for admin epoch disable |
| `MAX_FAILED_GATEWAYS_PER_OBSERVATION` | 100 | Per observation bitmap limit |
| `REDELEGATION_FEE_RESET_INTERVAL` | 7 days | Explicit constant for the WP's "7 day" rule |
| Name length fee multipliers (core) | 1-char: 100x → 5+: 1x | Exist in constants.rs, separate from genesis fees |

---

## Summary

| Category | Count |
|----------|-------|
| Parameters that match exactly | **56+** |
| HIGH severity discrepancies | **2** (#1, #5) |
| MEDIUM severity discrepancies | **1** (#4) |
| LOW / informational | **6** (#8-#13) |
| Resolved | **4** (#2 — RNP formula; #3 — primary name fee; #6 — disable delegation; #7 — deferred reward-share ratio) |

### Actionable Items — Resolve by Updating Whitepaper or Code

| # | Issue | Resolution | Plan |
|---|-------|------------|------|
| 1 | MAX_CONTROLLERS: WP=10, Code=4 | Update WP to say 4 | — |
| ~~2~~ | ~~RNP formula~~ | **RESOLVED** — code updated | `FIX_PLANS.md` #2 |
| ~~3~~ | ~~Primary name fee ignores purchase type~~ | **RESOLVED** — code updated | `FIX_PLANS.md` #3 |
| 4 | Undername max length: WP=51, Code=61 | Update WP to 61 (code follows Lua `MAX_UNDERNAME_LENGTH`) | — |
| 5 | Protected exit vault not expeditable | Update WP §6.6 — code is correct (BD-102) | — |
| ~~6~~ | ~~Disabling delegation doesn't auto-withdraw~~ | **RESOLVED** — `claim_delegate_from_disabled_gateway` + zero-stake/cooldown re-enable guard | `FIX_PLANS.md` #6 |
| ~~7~~ | ~~Gateway settings apply immediately~~ | **RESOLVED** — `delegate_reward_share_ratio` deferred to next epoch via tally | `FIX_PLANS.md` #7 |
| 8 | 8 chosen names not enforced on-chain | Informational — off-chain observer norm | — |
