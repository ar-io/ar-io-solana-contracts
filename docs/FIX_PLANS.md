# Fix Plans for Whitepaper Discrepancies

> Plans prepared 2026-05-29. Based on analysis in `docs/WHITEPAPER_COMPARISON.md`.

---

## Fix #2 — Returned Name Premium Formula

**Status:** Plan ready. Code change not yet applied.

**Problem:** The code formula `50 × (duration − elapsed) / duration` decayed the
premium from 50x to **0x**, while the whitepaper specifies `RNP = 50 − (49/14) × t`
which decays from 50x to **1x**. The code had a different slope (50/14 vs 49/14)
and a boundary discontinuity: at `elapsed = duration − 1 second`, integer
truncation produced `cost = 0`, and the `require!(token_cost > 0)` guard in all
10 `buy_returned_name` handler variants **rejected the purchase**. One second later
the guard returned `registration_fee` (1x).

**Root cause:** Code slope was `50/14` (intercept at 0) instead of WP's `49/14`
(intercept at 1).

**Fix (`programs/ario-arns/src/pricing.rs` — `calculate_returned_name_premium`,
lines 173-212):**

The function signature and guard are unchanged. Replace the formula body (the
lines between the `if elapsed >= duration` guard and the final `Ok(result)`).

The actual variable names in the function are:
- `registration_fee: u64` (parameter)
- `elapsed: i64` (computed from `current_timestamp - returned_at`)
- `duration: i64` (`RETURNED_NAME_DURATION_SECONDS` = 1,209,600)
- `scale: u128` (`DEMAND_FACTOR_SCALE as u128` = 1,000,000)
- `remaining: u128` (`(duration - elapsed) as u128`)
- `dur: u128` (`duration as u128`)
- Constants: `RETURNED_NAME_MAX_MULTIPLIER: u64 = 50` (line 30)

Replace the old formula (approximately lines 186-209):
```rust
// OLD: decays 50→0
let pct_remaining = remaining
    .checked_mul(scale)
    .ok_or(...)?
    .checked_div(dur)
    .ok_or(...)?;
let multiplier = (RETURNED_NAME_MAX_MULTIPLIER as u128)
    .checked_mul(pct_remaining)
    .ok_or(...)?;
let result = (registration_fee as u128)
    .checked_mul(multiplier)
    .ok_or(...)?
    .checked_div(scale)
    .ok_or(...)?;
```

With the whitepaper formula:
```rust
// NEW: decays 50→1 (matches WP: RNP = 50 − 49/14 × t)
// cost = registration_fee × (MAX × duration − (MAX−1) × elapsed) / duration
let max_mult = RETURNED_NAME_MAX_MULTIPLIER as u128; // 50
let el = elapsed as u128;
let numerator = max_mult
    .checked_mul(dur)
    .ok_or(ArnsError::ArithmeticOverflow)?
    .checked_sub(
        (max_mult - 1)
            .checked_mul(el)
            .ok_or(ArnsError::ArithmeticOverflow)?,
    )
    .ok_or(ArnsError::ArithmeticOverflow)?;
let multiplier = numerator
    .checked_mul(scale)
    .ok_or(ArnsError::ArithmeticOverflow)?
    .checked_div(dur)
    .ok_or(ArnsError::ArithmeticOverflow)?;
let result = (registration_fee as u128)
    .checked_mul(multiplier)
    .ok_or(ArnsError::ArithmeticOverflow)?
    .checked_div(scale)
    .ok_or(ArnsError::ArithmeticOverflow)?;
```

Note: match the existing error variant used in the function (`ArnsError::ArithmeticOverflow`
or similar). Read the current code to confirm the exact error variant name.

In plain math: `cost = registration_fee × (50 × duration − 49 × elapsed) / duration`

**Overflow safety:** All intermediates use u128. Worst case:
`registration_fee × multiplier` = `500_000_000_000 × 50_000_000 = 2.5e19` — exceeds
u64 but fits u128 (max 3.4e38). Same as the old code path.

**Guard `if elapsed >= duration { return Ok(registration_fee) }` unchanged** — at
elapsed=duration the WP formula yields exactly 1x, so the guard is a correct
short-circuit. The dead zone is eliminated because the minimum in-window multiplier
is now 1x (never 0).

**Verification at key points:**

| t | Old code | New code (WP) |
|---|---------|---------------|
| 0 days | 50.0x | 50.0x |
| 3.5 days | 37.5x | 37.75x |
| 7 days | 25.0x | 25.5x |
| 10.5 days | 12.5x | 13.25x |
| 13.5 days | 1.785x | 2.75x |
| 14d − 1s | ~0x (rejected!) | ~1.0x |
| 14 days | 1x (guard) | 1x (guard) |

**Tests updated:**

| Test | Old expected | New expected |
|------|-------------|-------------|
| `test_returned_name_premium_halfway` (7d) | 25000 | 25500 |
| `premium_at_25pct` (3.5d) | 37500 | 37750 |
| `premium_at_75pct` (10.5d) | 12500 | 13250 |
| `premium_near_end` (13.5d) | 1785 | 2750 |

**New tests added:**
- `premium_at_last_second_never_zero` — asserts cost ≥ registration_fee at elapsed=duration−1
- `premium_at_exact_duration_equals_base` — asserts cost == registration_fee at elapsed=duration

Existing `test_returned_name_premium_at_start` (50x at t=0) and
`test_returned_name_premium_expired` (1x at/past t=14) are unchanged. All proptests
(`returned_name_premium_decays`, `returned_name_at_expiry_equals_base`) still hold.

---

## Fix #3 — Primary Name Fee: Vary by Purchase Type

**Problem:** All 4 primary-name-request handlers charge a fixed 0.2 ARIO × DF
regardless of purchase type. Whitepaper says permabuy names should cost
1.0 ARIO × DF (5x more).

**Root cause:** Single constant `PRIMARY_NAME_REQUEST_BASE_FEE = 200_000` derived
from the lease formula only. The handler already has the ArnsRecord in
`remaining_accounts[0]` and `purchase_type` at byte offset 104 is already parsed
by `verify_arns_record_active` — but the value is discarded after the expiry check.

**Fix (no schema change, no migration):**

1. **`programs/ario-core/src/state/mod.rs` (line 208):** Replace one constant with two:
   ```rust
   pub const PRIMARY_NAME_REQUEST_BASE_FEE_LEASE: u64 = 200_000;    // 0.2 ARIO
   pub const PRIMARY_NAME_REQUEST_BASE_FEE_PERMABUY: u64 = 1_000_000; // 1.0 ARIO
   ```
   Derivation: `BRF(51) × UNDERNAME_LEASE_FEE_PCT = 200_000_000 × 0.001 = 200_000`
   and `BRF(51) × UNDERNAME_PERMABUY_FEE_PCT = 200_000_000 × 0.005 = 1_000_000`.

2. **`programs/ario-core/src/instructions/primary_name.rs`:** Add a small helper
   to read `purchase_type` from a validated ArnsRecord account:
   ```rust
   fn read_arns_purchase_type(arns_record_info: &AccountInfo) -> Result<u8> {
       let data = arns_record_info.try_borrow_data()?;
       let purchase_type_offset: usize = 8 + 32 + 32 + 32; // 104
       require!(data.len() > purchase_type_offset, ArioError::InvalidAccountState);
       Ok(data[purchase_type_offset])
   }
   ```

3. **4 handler sites** (lines 418, 544, 1336, 1466): Replace
   `ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE` with:
   ```rust
   let base_fee = if read_arns_purchase_type(arns_record_info)? == 1 {
       ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE_PERMABUY
   } else {
       ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE_LEASE
   };
   ```
   The `arns_record_info` reference is:
   - `request_primary_name`: `remaining_accounts[0]` (line 406)
   - `request_and_set_primary_name`: `remaining_accounts[0]` (line 489)
   - `request_primary_name_from_funding_plan`: `validation_accounts[0]` (line 1329)
   - `request_and_set_primary_name_from_funding_plan`: `validation_accounts[0]` (line 1433)

4. **Tests to update** (currently assert `fee == 200_000` using Permabuy records):
   - `test_request_and_set_primary_name` (line 3844) → expect `1_000_000`
   - `test_request_primary_name_emits_event_with_balance_funding_source` (line 10441) → expect `1_000_000`
   - `test_request_primary_name_from_funding_plan_multi_gateway` (line 9557) → expect `1_000_000`
   - `test_request_and_set_primary_name_from_funding_plan_multi_gateway` (line 9726) → expect `1_000_000`
   - `test_request_primary_name_from_funding_plan_emits_event` (line 9942) → expect `1_000_000`

5. **New tests to add:**
   - `test_request_primary_name_lease_fee` — Lease ArnsRecord, assert fee = 200,000 mARIO at DF=1.0
   - `test_request_primary_name_permabuy_fee` — Permabuy ArnsRecord, assert fee = 1,000,000 mARIO at DF=1.0
   - `test_request_and_set_primary_name_lease_fee` — combined handler, Lease record
   - `test_request_and_set_primary_name_permabuy_fee` — combined handler, Permabuy record
   - Optional: DF != 1.0 test (e.g. DF=2.0: lease=400,000, permabuy=2,000,000)

**Note:** Both Lua and current Solana use the lease rate unconditionally. This fix
aligns with the whitepaper but diverges from Lua. The whitepaper is the canonical
spec for v3.0.0.

---

## Fix #7 — Defer `delegate_reward_share_ratio` to Next Epoch

**Problem:** `update_gateway_settings` applies `delegate_reward_share_ratio`
immediately. An operator can front-run `distribute_epoch` to change their
reward split. Whitepaper says delegation settings take effect next epoch.

**Scope:** Only `delegate_reward_share_ratio` is deferred. The other two delegation
settings (`allow_delegated_staking`, `min_delegation_amount`) are per-transaction
gates that should remain immediate — deferring them would hurt operator UX
(e.g., a delegate could keep joining after the operator decided to close).

**Fix (schema change + migration required):**

### Step 1: Add field to Gateway struct

**`programs/ario-gar/src/state/mod.rs`:**

Add `pending_delegate_reward_share_ratio: Option<u16>` to Gateway, between
`bump` and `version` (per ADR-020: new fields append before version):

```rust
pub struct Gateway {
    // ... existing fields ...
    pub bump: u8,
    pub pending_delegate_reward_share_ratio: Option<u16>,  // NEW: 3 bytes
    pub version: SchemaVersion,                            // MUST remain last
}
```

Update `Gateway::SIZE`: 942 → 945 bytes (+3).

### Step 2: Bump version

**`programs/ario-gar/src/state/mod.rs`:**

```rust
pub const GATEWAY_VERSION: SchemaVersion = SchemaVersion::new(1, 1, 0);
```

### Step 3: Add migration arm

**`programs/ario-gar/src/schema_migration.rs`** in `migrate_gateway_version`:

```rust
SchemaVersion { major: 1, minor: 0, patch: 0 } => {
    account.pending_delegate_reward_share_ratio = None;
    account.version = SchemaVersion::new(1, 1, 0);
}
```

Uses existing grow-then-deserialize infrastructure (`grow_account` + `write_account`).

### Step 3b: `join_network` — initial ratio is immediate, not pending

When a gateway first joins, `delegate_reward_share_ratio` should be written
directly to `gateway.settings.delegate_reward_share_ratio` (not to pending),
because the gateway isn't participating in any epoch yet. The existing
`join_network` handler already sets `settings.delegate_reward_share_ratio`
at join time — **no change needed here**. Just ensure `pending` is initialized
to `None` (which it will be by default via Anchor's zero-init + migration).

### Step 4: Modify `update_gateway_settings`

**`programs/ario-gar/src/instructions/gateway.rs` (lines 446-455):**

Change from writing to `gateway.settings.delegate_reward_share_ratio` to writing
to `gateway.pending_delegate_reward_share_ratio`:

```rust
if let Some(ratio) = params.delegate_reward_share_ratio {
    require!(
        (ratio as u16) * 100 <= MAX_DELEGATE_REWARD_SHARE,
        GarError::InvalidRewardShare
    );
    gateway.pending_delegate_reward_share_ratio = Some(ratio as u16 * 100);
    fields_changed |= GATEWAY_SETTINGS_FIELD_DELEGATE_REWARD_SHARE_RATIO;
}
```

### Step 5: Apply pending in `tally_weights`

**`programs/ario-gar/src/instructions/epoch.rs` (after line 461):**

Piggyback on the existing Gateway write-back in `tally_weights`:

```rust
gw.weights = weights;
gw.weights.weights_epoch = epoch.epoch_index;

// Apply deferred delegation settings
if let Some(pending) = gw.pending_delegate_reward_share_ratio.take() {
    gw.settings.delegate_reward_share_ratio = pending;
}
```

No additional write needed — `gw.serialize(...)` at line 464 persists the full
Gateway including the cleared pending field.

### Step 6: No changes to `distribute_epoch`

`distribute_epoch` already reads `gateway.settings.delegate_reward_share_ratio`
(the active field). After tally applies pending→active, distribute naturally
picks up the epoch-stable value.

### Timing correctness

| When operator changes ratio | Tally status | Effect |
|-----------------------------|-------------|--------|
| Before tally_weights | Not yet run | Applied during this epoch's tally |
| After tally_weights, before distribute | Already ran | Pending sits; old ratio used for this epoch; applied at next epoch's tally |
| Multiple changes before tally | N/A | Last write wins (overwrite) |

### Operational requirement

**All Gateway PDAs must be migrated before the first post-upgrade `tally_weights`.**
Both `tally_weights` and `distribute_epoch` use raw `Gateway::deserialize` on
remaining_accounts — pre-migration accounts (old SIZE, missing the 3 new bytes)
will fail deserialization with `InvalidGatewayAccount`. Similarly, all Anchor
typed access points (`update_gateway_settings`, `join_network`, `leave_network`,
`prune_gateway`, `finalize_gone`, `update_observer_address`) will fail on
pre-migration accounts.

The `migrate_gateway` instruction is permissionless and can be cranked for all
live gateways in parallel before the upgrade epoch.

### Tests to add

1. **`test_delegate_reward_share_ratio_deferred`** — Change ratio mid-epoch,
   verify distribute uses old ratio, verify next epoch's distribute uses new ratio.
2. **`test_delegate_reward_share_ratio_pending_overwrite`** — Two changes before
   tally, verify last write wins.
3. **`test_delegate_reward_share_ratio_applied_at_tally`** — Change ratio before
   tally, verify tally applies it and distribute uses the new value.
4. **`test_migrate_gateway_1_0_0_to_1_1_0`** — Genuine pre-version account (old
   SIZE, no pending field), verify migration succeeds and pending defaults to None.
5. Update existing `test_update_gateway_settings_all_fields` to verify that
   `delegate_reward_share_ratio` change is pending, not immediately active.

### Event emission

`GatewaySettingsUpdatedEvent` should continue to fire when the operator calls
`update_gateway_settings` with a new ratio. The event's `fields_changed` bitmask
already includes `GATEWAY_SETTINGS_FIELD_DELEGATE_REWARD_SHARE_RATIO`. The event
signals "operator requested a change" — downstream consumers should understand
the change is **pending** until the next tally. No new event fields are needed,
but SDK/docs should note the deferred semantics.

### Client / SDK implications

After calling `update_gateway_settings`, reading the Gateway account will show
the **old** `settings.delegate_reward_share_ratio` until the next `tally_weights`
applies the pending value. The `pending_delegate_reward_share_ratio` field is
visible on the account and clients can check it to see the queued change. SDK
helpers that display "current delegate reward share" should read `pending` if
set, falling back to `settings`, and label it appropriately.

### Rent impact

- Per gateway: ~0.0004 SOL additional
- Total across 3000 gateways: ~1.2 SOL

---

## Fix #6 — Disable Delegation Forces Delegate Withdrawal + 30-Day Cooldown

**Problem:** On AO/Lua, disabling `allow_delegated_staking` auto-withdraws all
delegates and blocks re-enabling for 30 days. On Solana, the toggle just flips a
boolean — existing delegates sit indefinitely, the operator can toggle freely, and
there's no mechanism to force delegates out.

**WP references:** §6.3: "If a gateway has delegated stakers and disables 'allow
delegated staking,' all delegates will have their tokens withdrawn." Also: "The
gateway cannot re-enable delegated staking until all previous delegates have been
withdrawn."

**Scope:** Three changes:
1. New permissionless instruction `claim_delegate_from_disabled_gateway`
2. New field `delegation_disabled_at: Option<i64>` on Gateway
3. Re-enable guard in `update_gateway_settings`

### Design

On Solana we can't iterate PDAs in a single tx, so "auto-withdraw all delegates"
becomes a **permissionless cranking pattern** — the same model used for
`claim_delegate_from_leaving_gateway` on leaving gateways. The cranker (or anyone)
calls the new instruction once per delegate to create withdrawal vaults.

**Flow:**
1. Operator calls `update_gateway_settings` with `allow_delegated_staking = false`
2. Code records `gateway.delegation_disabled_at = Some(clock.unix_timestamp)`
3. Cranker (or anyone) calls `claim_delegate_from_disabled_gateway` for each
   delegate — creates 30-day withdrawal vaults, zeroes each delegation
4. Operator cannot re-enable until:
   - `gateway.total_delegated_stake == 0` (all delegates claimed), AND
   - `clock.unix_timestamp >= delegation_disabled_at + WITHDRAWAL_LOCK_PERIOD` (30 days)
5. On re-enable, `delegation_disabled_at` is cleared to `None`

This prevents toggle churn: the operator can't dump delegates then immediately
re-recruit.

### Step 1: Add field to Gateway struct

**`programs/ario-gar/src/state/mod.rs`:**

Add `delegation_disabled_at: Option<i64>` to Gateway, alongside the other new
fields from Fix #7 (both appended before `version`):

```rust
pub struct Gateway {
    // ... existing fields ...
    pub bump: u8,
    pub pending_delegate_reward_share_ratio: Option<u16>,  // Fix #7: 3 bytes
    pub delegation_disabled_at: Option<i64>,               // Fix #6: 9 bytes
    pub version: SchemaVersion,                            // MUST remain last
}
```

Update `Gateway::SIZE`: 942 → 954 bytes (+12 total from both #7 and #6).

**Bundle with Fix #7's migration** — one schema version bump (1.0.0 → 1.1.0),
one `migrate_gateway` call per PDA. The migration arm defaults both new fields
to `None`.

### Step 2: Modify `update_gateway_settings` — disable path

**`programs/ario-gar/src/instructions/gateway.rs`:**

When `allow_delegated_staking` is set to `false`:
```rust
if let Some(allow_delegated_staking) = params.allow_delegated_staking {
    if !allow_delegated_staking && gateway.settings.allow_delegated_staking {
        // Disabling: record timestamp for 30-day cooldown
        gateway.delegation_disabled_at = Some(clock.unix_timestamp);
    }
    gateway.settings.allow_delegated_staking = allow_delegated_staking;
    fields_changed |= GATEWAY_SETTINGS_FIELD_ALLOW_DELEGATED_STAKING;
}
```

### Step 3: Modify `update_gateway_settings` — re-enable guard

When `allow_delegated_staking` is set to `true`:
```rust
if let Some(allow_delegated_staking) = params.allow_delegated_staking {
    if allow_delegated_staking && !gateway.settings.allow_delegated_staking {
        // Re-enabling: require all delegates withdrawn + 30-day cooldown
        require!(
            gateway.total_delegated_stake == 0,
            GarError::DelegatesStillActive
        );
        if let Some(disabled_at) = gateway.delegation_disabled_at {
            require!(
                clock.unix_timestamp >= disabled_at
                    .checked_add(WITHDRAWAL_LOCK_PERIOD)
                    .ok_or(GarError::ArithmeticOverflow)?,
                GarError::DelegationCooldownActive
            );
        }
        gateway.delegation_disabled_at = None;
    }
    // ... (disable path from Step 2 above)
    gateway.settings.allow_delegated_staking = allow_delegated_staking;
    fields_changed |= GATEWAY_SETTINGS_FIELD_ALLOW_DELEGATED_STAKING;
}
```

**New errors needed:** `GarError::DelegatesStillActive`, `GarError::DelegationCooldownActive`.

### Step 4: New instruction `claim_delegate_from_disabled_gateway`

**`programs/ario-gar/src/instructions/delegate.rs`:**

Mirrors `claim_delegate_from_leaving_gateway` (lines 216-297) almost exactly.
Differences:

| Aspect | `_from_leaving_gateway` | `_from_disabled_gateway` |
|--------|------------------------|-------------------------|
| Gate | `gateway.status == Leaving` | `gateway.settings.allow_delegated_staking == false` |
| Gateway status | `Leaving` | `Joined` (still active) |
| Withdrawal lock | `settings.withdrawal_period` (30 days) | Same: `settings.withdrawal_period` |
| Withdrawal flags | `is_delegate: true, is_exit_vault: false, is_protected: false` | Same |
| Reward settlement | `settle_delegate_rewards` before claim | Same |
| Event | `DelegationEvent` with amount=0, total=0 | Same |
| Supply counters | `settings.total_delegated -= amount; settings.total_withdrawn += amount` | Same |
| Delegation PDA | Left open with amount=0 (closed later via `close_empty_delegation`) | Same |

**Account context struct** (`ClaimDelegateFromDisabledGateway`):

Identical to `ClaimDelegateFromLeavingGateway` (lines 675-733) except the
gateway constraint changes from:
```rust
constraint = gateway.status == GatewayStatus::Leaving @ GarError::GatewayNotJoined,
```
to:
```rust
constraint = !gateway.settings.allow_delegated_staking @ GarError::DelegationNotDisabled,
```

**New error:** `GarError::DelegationNotDisabled` (gate: delegation must be disabled).

**Permissionless:** `delegator` is `AccountInfo` (not `Signer`). `payer` covers
rent on withdrawal PDA. Cranker pays ~0.003 SOL per delegate.

### Step 5: Add dispatch in `lib.rs`

**`programs/ario-gar/src/lib.rs`** — add alongside `claim_delegate_from_leaving_gateway`
(~line 222):

```rust
pub fn claim_delegate_from_disabled_gateway(
    ctx: Context<ClaimDelegateFromDisabledGateway>,
) -> Result<()> {
    instructions::delegate::claim_delegate_from_disabled_gateway(ctx)
}
```

### Tests to add

1. **`test_claim_delegate_from_disabled_gateway`** — Disable delegation, call
   claim for a delegate, verify withdrawal vault created with 30-day lock,
   delegation zeroed, `total_delegated_stake` decremented.

2. **`test_claim_delegate_from_disabled_gateway_settles_rewards`** — Delegate has
   unsettled rewards, claim includes them in the withdrawal amount.

3. **`test_claim_delegate_from_disabled_gateway_permissionless`** — Third party
   (not the delegate) calls claim successfully.

4. **`test_claim_delegate_from_disabled_gateway_rejects_when_enabled`** — Fails
   with `DelegationNotDisabled` when `allow_delegated_staking == true`.

5. **`test_reenable_delegation_requires_zero_stake_and_cooldown`** — Disable,
   claim all delegates, try re-enable before 30 days (fails with
   `DelegationCooldownActive`), warp 30 days, re-enable succeeds.

6. **`test_reenable_delegation_rejects_with_remaining_delegates`** — Disable,
   claim some but not all delegates, try re-enable (fails with
   `DelegatesStillActive`).

7. **`test_disable_delegation_records_timestamp`** — Verify
   `delegation_disabled_at` is set on disable and cleared on re-enable.

8. **`test_disable_delegation_cancel_withdrawal_blocked`** — After disable +
   claim, delegate's `cancel_withdrawal` fails with `DelegationNotAllowed`
   (existing behavior, verify it still holds).

9. Update existing `test_update_gateway_settings_all_fields` to verify the
   disable/re-enable guard behavior.

### Event ABI

The new instruction emits the existing `DelegationEvent` (same as
`claim_delegate_from_leaving_gateway`). **No new event type needed**, so no
`idl-event-snapshots.json` update required.

### SDK / client impact

**Auto-generated — no hand-written SDK code needed.** After `anchor build` +
`yarn codegen`, the new instruction appears at:
```typescript
import { getClaimDelegateFromDisabledGatewayInstruction } from '@ar.io/solana-contracts/gar';
```

**Cranker changes (`ar-io-cranker`):**
- Add a sweep loop: query indexer for gateways with `allow_delegated_staking == false`
  and `total_delegated_stake > 0`
- For each, query delegate PDAs and call `claim_delegate_from_disabled_gateway`
- Same pattern as the existing leaving-gateway delegate sweep

**Portal / ar.io network UI:**
- When a gateway disables delegation, show a banner to affected delegates:
  "This gateway has disabled delegation. Your stake is being moved to a
  30-day withdrawal vault."
- Show the operator a status: "Delegation disabled. N delegates remaining.
  Re-enable available after [date]."
- Read `delegation_disabled_at` + `WITHDRAWAL_LOCK_PERIOD` to compute the
  re-enable date.

### Migration note

This fix shares the Gateway schema migration with Fix #7. Both add fields before
`version`. One `migrate_gateway` instruction handles both (grow by 12 bytes total,
default both new fields to `None`, bump to 1.1.0).
