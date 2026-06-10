# Compute Budgets, Account Sizes, and Protocol Limits

Precise reference for all compute budgets, account sizes, transaction constraints,
batching requirements, constants, PDA seeds, and fixed-point arithmetic used across
the four AR.IO Solana programs.

All values are taken directly from the source code. Token amounts are in **mARIO**
(1 ARIO = 1,000,000 mARIO). All durations are in **seconds** (not milliseconds).

---

## 1. Compute Unit Budgets

| Program    | CU Budget   | Source |
|------------|-------------|--------|
| ario-core  | 400,000     | `programs/ario-core/tests/integration.rs:74` |
| ario-gar   | 1,000,000   | `programs/ario-gar/tests/integration.rs:78` |
| ario-arns  | 1,000,000   | `programs/ario-arns/tests/integration.rs:69` |
| ario-ant   | 200,000     | Default Solana CU (no explicit override) |

**Why ario-gar needs 1M CU:** Epoch operations (`tally_weights`, `prescribe_epoch`,
`distribute_epoch`) process batches of gateway accounts via `remaining_accounts`,
performing PDA validation, deserialization, weight computation, and reward math per
gateway in a single instruction.

**Why ario-arns needs 1M CU:** Trait-mutating handlers (`buy_name`, `buy_returned_name`,
`upgrade_name`, `increase_undername_limit`, `reassign_name`, `release_name`,
`sync_attributes`) CPI into MPL Core's `UpdatePluginV1` to keep the on-chain
Attributes plugin coherent with the source-of-truth `ArnsRecord` PDA. The Core CPI
(asset deserialization + plugin re-serialization + potential `realloc` + system-program
rent transfer) consumes ~30K–80K CU depending on attribute payload size; budgeting
1M CU leaves comfortable headroom for the surrounding pricing/demand/registry work
and the optional gateway-discount lookup. Empirically: a lease `buy_name` end-to-end
runs ~80K CU on surfpool, well below the budget. See `programs/ario-arns/src/mpl_core_cpi.rs`.

---

## 2. Account Size Limits

### ario-core accounts

| Account             | SIZE (bytes) | PDA Seeds | Notes |
|---------------------|-------------|-----------|-------|
| `ArioConfig`        | 226         | `["ario_config"]` | Singleton protocol config |
| `Balance`           | 49          | `["balance", user_pubkey]` | Per-user protocol balance |
| `VaultCounter`      | 49          | `["vault_counter", owner_pubkey]` | Per-user vault ID counter |
| `Vault`             | 107         | `["vault", owner_pubkey, vault_id.to_le_bytes()]` | Individual locked vault |
| `PrimaryNameRequest`| 124         | `["primary_name_request", initiator_pubkey]` | Pending name request; max name 63 chars |
| `PrimaryName`       | 116         | `["primary_name", owner_pubkey]` | Active primary name assignment |
| `PrimaryNameReverse`| 108         | `["primary_name_reverse", hash(name.to_lowercase())]` | Reverse lookup: name to owner |

### ario-gar accounts

| Account             | SIZE (bytes) | PDA Seeds | Notes |
|---------------------|-------------|-----------|-------|
| `GatewaySettings`   | 254         | `["gar_settings"]` | Singleton GAR config; includes pinned token accounts (C-1/C-2 audit) and `arns_program_id` (H-4 audit) |
| `GatewayRegistry`   | 120,040     | `["gateway_registry"]` | Zero-copy; 3,000 gateway slots |
| `Gateway`           | 942         | `["gateway", operator_pubkey]` | Max-length strings; SIZE includes 64-char label, 128-char fqdn, 256-char properties/note |
| `Delegation`        | 105         | `["delegation", gateway_pubkey, delegator_pubkey]` | Per-delegate stake record |
| `WithdrawalCounter` | 49          | `["withdrawal_counter", owner_pubkey]` | Per-user withdrawal ID counter |
| `Withdrawal`        | 107         | `["withdrawal", owner_pubkey, withdrawal_id.to_le_bytes()]` | Pending withdrawal |
| `AllowlistEntry`    | 81          | `["allowlist", gateway_pubkey, delegate_pubkey]` | Delegate allowlist entry |
| `ObserverLookup`    | 41          | `["observer_lookup", observer_address]` | Observer uniqueness index |
| `RedelegationRecord`| 61          | `["redelegation", delegator_pubkey]` | Redelegation fee tracking |
| `EpochSettings`     | 159         | `["epoch_settings"]` | Singleton epoch config; includes `disable_at` timelock (GAR-007) and `failed_gateway_slash_rate` |
| `Epoch`             | 9,400       | `["epoch", epoch_index.to_le_bytes()]` | Zero-copy; embeds prescriptions, failure tallies |
| `Observation`       | 466         | `["observation", epoch_index.to_le_bytes(), observer_pubkey]` | Per-observer observation report |

**Gateway SIZE breakdown (942 bytes):**
```
  8   discriminator
 32   operator
 68   label (4 + 64)
132   fqdn (4 + 128)
  2   port
  1   protocol
260   properties (4 + 256)
260   note (4 + 256)
  8   operator_stake
  8   total_delegated_stake
  1   status
  8   start_timestamp
  9   leave_timestamp (Option<i64>)
 22   stats (GatewayStats: 5×u32 + 2×u8)
 56   weights (GatewayWeights: 7×u64)
 13   settings (GatewaySettings2: bool + u16 + bool + u64 + bool)
  5   registry_index (u32 + bool)
 32   observer_address
 16   cumulative_reward_per_token (u128)
  1   bump
```

**Epoch SIZE breakdown (9,400 bytes):**
```
 72   9 x u64 fields (epoch_index, timestamps, rewards, rates, weight pair)
 32   hashchain
 12   3 x u32 (active_gateway_count, distribution_index, tally_index)
  8   8 x u8 (observer_count, name_count, flags, bump, padding)
6000  failure_counts: [u16; 3000]
1600  prescribed_observers: [Pubkey; 50]
1600  prescribed_observer_gateways: [Pubkey; 50]
 64   prescribed_names: [[u8; 32]; 2]
  7   has_observed bitmap (50 bits)
  5   padding
```

### ario-arns accounts

| Account         | SIZE (bytes) | PDA Seeds | Notes |
|-----------------|-------------|-----------|-------|
| `ArnsConfig`    | 179         | `["arns_config"]` | Singleton ArNS config |
| `DemandFactor`  | 574         | `["demand_factor"]` | Singleton; embeds 51-entry fee table + 7-period trailing arrays |
| `ArnsRecord`    | 188         | `["arns_record", name_hash]` | Per-name record; name_hash = SHA256(name.to_lowercase()) |
| `ReturnedName`  | 136         | `["returned_name", name_hash]` | Returned name auction state |
| `ReservedName`  | 150         | `["reserved_name", name_hash]` | Reserved name entry |
| `NameRegistry`  | **2,000,040 (initial)** | `["name_registry"]` | **Dynamic-capacity (ADR-020):** 40-byte header + byte-offset slot array. Initial deploy 50,000 slots (~2 MB); expandable via `admin_expand_name_registry` up to any target. Slot count derived from `data.len()`, no header field. |

**NameEntry size:** 40 bytes (32 name_hash + 4 registry_index + 4 padding)

### ario-ant accounts

| Account             | SIZE (bytes) | PDA Seeds | Notes |
|---------------------|-------------|-----------|-------|
| `AntMigrationConfig`| 74          | `["ant_migration_config"]` | Singleton migration config |
| `AntConfig`         | 758         | `["ant_config", asset_pubkey]` | Per-ANT metadata; includes `version: u8` for schema migrations |
| `AntControllers`    | 365         | `["ant_controllers", asset_pubkey]` | Per-ANT controller list; max 10 controllers |
| `AntRecord`         | 313         | `["ant_record", asset_pubkey, hash(undername.to_lowercase())]` | Per-undername core record (target, ttl, protocol) |
| `AntRecordMetadata` | 741         | `["ant_record_meta", asset_pubkey, hash(undername.to_lowercase())]` | Optional per-record metadata (display_name, logo, description, keywords); only created when needed |

**Metaplex Core Attributes plugin:** Each ANT's Core asset also carries an Attributes plugin with DAS-queryable traits (ArNS Name, Type, Undername Limit). This adds ~200 bytes to the Metaplex Core asset account (header + 3 key-value pairs). Additional rent: ~0.00125 SOL per ANT. The `AntConfig` PDA remains the source of truth for extended metadata; the Attributes plugin is a marketplace projection. See ADR-012.

---

## 3. Batching Requirements

### Epoch lifecycle (ario-gar)

All epoch operations process gateways via `remaining_accounts` and track progress
via on-chain indices. Call repeatedly until the index reaches `active_gateway_count`.

| Operation           | Batch Mechanism | Progress Field | Done Flag |
|---------------------|-----------------|----------------|-----------|
| `tally_weights`     | remaining_accounts (gateway PDAs) | `epoch.tally_index` | `epoch.weights_tallied` |
| `prescribe_epoch`   | Single call (observer PDAs + NameRegistry in remaining_accounts) | N/A | `epoch.prescriptions_done` |
| `distribute_epoch`  | remaining_accounts (gateway PDAs) | `epoch.distribution_index` | `epoch.rewards_distributed` |

**`distribute_epoch` batching:** Process up to ~15 gateways per transaction
(`DISTRIBUTION_BATCH_SIZE = 15`). Each gateway requires PDA validation,
deserialization, weight verification, pass/fail determination, reward calculation,
delegate reward accumulator update, and re-serialization.

**`tally_weights` batching:** Similar per-gateway processing. Each gateway is
deserialized, weights computed (u128 intermediates), composite weight cached in
registry slot, epoch accumulator updated, and weights written back to gateway account.

### ArNS pruning (ario-arns)

| Operation               | Parameter    | Notes |
|-------------------------|-------------|-------|
| `prune_expired_names`   | `max_names: u8` | Batch size passed as argument |
| `prune_returned_names`  | `max_names: u8` | Batch size passed as argument |
| `prune_name_to_returned`| 1 per call  | Single name per transaction |
| `prune_expired_reservation` | 1 per call | Single reservation per transaction |

---

## 4. Key Constants

### Scaling Factors (`ario-core/src/constants.rs`)

| Constant | Value | Meaning |
|----------|-------|---------|
| `RATE_SCALE` | 1,000,000 | 1.0 in fixed-point (used for all percentage math) |
| `REWARD_PRECISION` | 1,000,000,000,000,000,000 (1e18) | Reward-per-share accumulator precision |
| `TOKEN_DECIMALS` | 6 | SPL token decimal places |
| `ONE_TOKEN` | 1,000,000 | 1 ARIO in mARIO base units |

### Time Constants

| Constant | Value | Human | Source |
|----------|-------|-------|--------|
| `SECONDS_PER_DAY` | 86,400 | 1 day | constants.rs |
| `SECONDS_PER_YEAR` | 31,536,000 | 365 days | constants.rs |
| `DEFAULT_EPOCH_DURATION` | 86,400 | 24 hours | constants.rs |
| `WITHDRAWAL_LOCK_PERIOD` | 2,592,000 | 30 days | constants.rs / gar lib.rs |
| `GATEWAY_LEAVE_PERIOD` | 7,776,000 | 90 days | constants.rs / gar lib.rs |
| `LEASE_GRACE_PERIOD` | 1,209,600 | 14 days | constants.rs |
| `RETURN_AUCTION_DURATION` | 1,209,600 | 14 days | constants.rs |
| `PRIMARY_NAME_REQUEST_EXPIRY` | 604,800 | 7 days | constants.rs |
| `REDELEGATION_FEE_RESET_INTERVAL` | 604,800 | 7 days | constants.rs |
| `TENURE_WEIGHT_DURATION` | 15,552,000 | 180 days | constants.rs |
| `OBSERVATION_WINDOW_SECONDS` | 3,600 | 1 hour | constants.rs |

### Staking Constants

| Constant | Value | Human | Source |
|----------|-------|-------|--------|
| `MIN_OPERATOR_STAKE` | 20,000,000,000 | 20,000 ARIO | constants.rs |
| `MIN_DELEGATION_AMOUNT` | 10,000,000 | 10 ARIO | constants.rs |
| `MAX_DELEGATE_REWARD_SHARE` | 9,500 | 95% (basis points) | gar lib.rs |
| `MAX_EXPEDITED_WITHDRAWAL_PENALTY` | 500,000 | 50% (RATE_SCALE) | constants.rs |
| `MIN_EXPEDITED_WITHDRAWAL_PENALTY` | 100,000 | 10% (RATE_SCALE) | constants.rs |
| `MIN_EXPEDITED_WITHDRAWAL_AMOUNT` | 1,000,000 | 1 ARIO | constants.rs |
| `MIN_REDELEGATION_PENALTY` | 100,000 | 10% (RATE_SCALE) | constants.rs |
| `MAX_REDELEGATION_PENALTY` | 600,000 | 60% (RATE_SCALE) | constants.rs |
| `MAX_TENURE_WEIGHT` | 4 | Dimensionless cap | constants.rs |
| `max_delegates_per_gateway` | 10,000 | Configured at init | gar lib.rs |

### Epoch / Distribution Constants

| Constant | Value | Human | Source |
|----------|-------|-------|--------|
| `DISTRIBUTION_BATCH_SIZE` | 15 | Gateways per tx | constants.rs |
| `MAX_OBSERVERS_PER_EPOCH` | 50 | Observer cap | constants.rs |
| `DEFAULT_PRESCRIBED_NAME_COUNT` | 2 | Names per epoch | constants.rs |
| `MAX_FAILED_GATEWAYS_PER_OBSERVATION` | 100 | Per report cap | constants.rs |
| `MAX_CONSECUTIVE_FAILURES` | 30 | Before prune | constants.rs |
| `GATEWAY_OPERATOR_REWARD_RATE` | 900,000 | 90% (RATE_SCALE) | gar lib.rs |
| `OBSERVER_REWARD_RATE` | 100,000 | 10% (RATE_SCALE) | gar lib.rs |
| `MAX_REWARD_RATE` | 1,000 | 0.1% per epoch (RATE_SCALE) | gar lib.rs |
| `MIN_REWARD_RATE` | 500 | 0.05% per epoch (RATE_SCALE) | gar lib.rs |
| `REWARD_DECAY_START_EPOCH` | 365 | Epoch index | gar lib.rs |
| `REWARD_DECAY_LAST_EPOCH` | 547 | Epoch index | gar lib.rs |
| `MISSED_OBSERVATION_PENALTY` | 250,000 | 25% (RATE_SCALE) | gar lib.rs |
| `FAILED_GATEWAY_SLASH_RATE` | 1,000,000 | 100% (RATE_SCALE) | constants.rs |

### ArNS Constants

| Constant | Value | Human | Source |
|----------|-------|-------|--------|
| `MAX_NAME_LENGTH` | 51 | Characters | constants.rs |
| `MIN_NAME_LENGTH` | 1 | Characters | constants.rs |
| `ARWEAVE_ADDRESS_LENGTH` | 43 | Forbidden name length | arns state |
| `DEFAULT_UNDERNAME_LIMIT` | 10 | Per name | constants.rs |
| `MAX_LEASE_YEARS` | 5 | Years | constants.rs |
| `PERMABUY_MULTIPLIER` | 2,000 | 20x annual (scaled /100) | constants.rs |
| `ANNUAL_PERCENTAGE_FEE` | 200,000 | 0.2 (SCALE) | arns pricing.rs |
| `PERMABUY_LEASE_FEE_LENGTH_YEARS` | 20 | Equivalent years | arns pricing.rs |
| `UNDERNAME_LEASE_FEE_PCT` | 1,000 | 0.001 (SCALE) | arns pricing.rs |
| `UNDERNAME_PERMABUY_FEE_PCT` | 5,000 | 0.005 (SCALE) | arns pricing.rs |
| `GATEWAY_OPERATOR_DISCOUNT_PCT` | 200,000 | 20% (SCALE) | arns pricing.rs |
| `RETURNED_NAME_MAX_MULTIPLIER` | 50 | 50x premium at start | arns pricing.rs |
| `RETURNED_NAME_DURATION_SECONDS` | 1,209,600 | 14 days | arns pricing.rs |
| `DEMAND_FACTOR_SCALE` | 1,000,000 | 1.0 in fixed-point | arns state |
| `DEMAND_FACTOR_MIN` | 500,000 | 0.5 floor | arns state |
| `DEMAND_FACTOR_UP_ADJUSTMENT` | 1,050,000 | 1.05x | arns state |
| `DEMAND_FACTOR_DOWN_ADJUSTMENT` | 985,000 | 0.985x | arns state |
| `MAX_PERIODS_AT_MIN_DEMAND_FACTOR` | 7 | Triggers fee halving | arns state |
| `MOVING_AVG_PERIOD_COUNT` | 7 | Trailing periods | arns state |
| `PERIOD_LENGTH_SECONDS` | 86,400 | 1 day | arns state |

### ArNS Genesis Fees (mARIO by name length)

| Length | Fee (mARIO) | Fee (ARIO) |
|--------|------------|------------|
| 1 char | 500,000,000,000 | 500,000 |
| 2 char | 100,000,000,000 | 100,000 |
| 3 char | 10,000,000,000 | 10,000 |
| 4 char | 5,000,000,000 | 5,000 |
| 5 char | 2,500,000,000 | 2,500 |
| 6 char | 1,500,000,000 | 1,500 |
| 7 char | 800,000,000 | 800 |
| 8 char | 500,000,000 | 500 |
| 9 char | 400,000,000 | 400 |
| 10 char | 350,000,000 | 350 |
| 11 char | 300,000,000 | 300 |
| 12 char | 250,000,000 | 250 |
| 13-51 chars | 200,000,000 | 200 |

Fees are stored in the mutable `DemandFactor.fees[51]` array. When the demand factor
stays at minimum for 7 consecutive periods, the entire fee table is permanently halved.

### ANT Constants (`ario-ant/src/state.rs`)

| Constant | Value | Source |
|----------|-------|--------|
| `MAX_UNDERNAME_LENGTH` | 61 | state.rs |
| `MIN_TTL_SECONDS` | 60 | state.rs |
| `DEFAULT_TTL_SECONDS` | 900 | state.rs (15 minutes) |
| `MAX_TTL_SECONDS` | 86,400 | state.rs (1 day) |
| `MAX_NAME_LENGTH` (ANT) | 61 | state.rs |
| `MAX_DESCRIPTION_LENGTH` | 256 | state.rs |
| `MAX_KEYWORDS` | 8 | state.rs |
| `MAX_KEYWORD_LENGTH` | 32 | state.rs |
| `MAX_CONTROLLERS` | 10 | state.rs |
| `MAX_TICKER_LENGTH` | 16 | state.rs |
| `ARWEAVE_TX_ID_LENGTH` | 43 | state.rs |

### Vault Constants (`ario-core/src/state/mod.rs`)

| Constant | Value | Human |
|----------|-------|-------|
| `ArioConfig::DEFAULT_MIN_VAULT_DURATION` | 1,209,600 | 14 days |
| `ArioConfig::DEFAULT_MAX_VAULT_DURATION` | 6,307,200,000 | 200 years |
| `ArioConfig::MIN_VAULT_SIZE` | 100,000,000 | 100 ARIO |
| `ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE_LEASE` | 200,000 | 0.2 ARIO (lease names) |
| `ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE_PERMABUY` | 1,000,000 | 1.0 ARIO (permabuy names) |
| `PrimaryNameRequest::MAX_NAME_LENGTH` | 63 | Characters |

---

## 5. Transaction Constraints

### Solana protocol limits

| Constraint | Limit |
|-----------|-------|
| Max transaction size | 1,232 bytes |
| Max accounts per transaction | ~64 (including program accounts, sysvars) |
| Max account data increase per instruction | 10,240 bytes (`MAX_PERMITTED_DATA_INCREASE`) |
| Default compute units | 200,000 |
| Max compute units (with budget instruction) | 1,400,000 |
| Account data max size | 10 MB |

### Zero-copy pre-allocation requirement

`GatewayRegistry` (168,040 bytes) and `NameRegistry` (initial 2,000,040 bytes,
dynamic per ADR-020) exceed the 10,240-byte `MAX_PERMITTED_DATA_INCREASE` limit.
These accounts **cannot** be created via Anchor's `init` constraint in a normal
instruction. They must be:

1. **Pre-allocated** by a separate transaction (`create_account` with sufficient space)
2. **Discriminator pre-written** before the `initialize` instruction runs

Test pattern for pre-allocation:
```rust
let disc = hash(b"account:GatewayRegistry");
data[..8].copy_from_slice(&disc.to_bytes()[..8]);
pt.add_account(registry_key, Account { data, .. });
```

### Account count per instruction (practical limits)

| Instruction | Fixed Accounts | Remaining Accounts | Total |
|------------|---------------|-------------------|-------|
| `tally_weights` | ~5 (epoch, registry, settings, epoch_settings, signer) | N gateways | ~5 + N |
| `prescribe_epoch` | ~4 | N observer gateways + 1 NameRegistry | ~4 + N + 1 |
| `distribute_epoch` | ~6 (+ protocol_token_account, stake_token_account) | N gateways | ~6 + N |
| `save_observations` | ~4 (epoch, observation, observer, system) | 0 | ~4 |

Given the ~64 account limit, **practical batch sizes** are:
- `tally_weights`: ~55 gateways per tx
- `distribute_epoch`: ~55 gateways per tx (though CU limits to ~15 practically)
- `prescribe_epoch`: limited by 50 observer gateway PDAs + 1 NameRegistry = ~51 remaining

---

## 6. PDA Derivation Reference

### ario-core

| PDA | Seeds | Program |
|-----|-------|---------|
| ArioConfig | `["ario_config"]` | ario-core |
| Balance | `["balance", user_pubkey]` | ario-core |
| VaultCounter | `["vault_counter", owner_pubkey]` | ario-core |
| Vault | `["vault", owner_pubkey, vault_id.to_le_bytes()]` | ario-core |
| PrimaryName | `["primary_name", owner_pubkey]` | ario-core |
| PrimaryNameRequest | `["primary_name_request", initiator_pubkey]` | ario-core |
| PrimaryNameReverse | `["primary_name_reverse", SHA256(name.to_lowercase())]` | ario-core |

### ario-gar

| PDA | Seeds | Program |
|-----|-------|---------|
| GatewaySettings | `["gar_settings"]` | ario-gar |
| GatewayRegistry | `["gateway_registry"]` | ario-gar |
| Gateway | `["gateway", operator_pubkey]` | ario-gar |
| Delegation | `["delegation", gateway_pubkey, delegator_pubkey]` | ario-gar |
| WithdrawalCounter | `["withdrawal_counter", owner_pubkey]` | ario-gar |
| Withdrawal | `["withdrawal", owner_pubkey, withdrawal_id.to_le_bytes()]` | ario-gar |
| AllowlistEntry | `["allowlist", gateway_pubkey, delegate_pubkey]` | ario-gar |
| ObserverLookup | `["observer_lookup", observer_address]` | ario-gar |
| RedelegationRecord | `["redelegation", delegator_pubkey]` | ario-gar |
| EpochSettings | `["epoch_settings"]` | ario-gar |
| Epoch | `["epoch", epoch_index.to_le_bytes()]` | ario-gar |
| Observation | `["observation", epoch_index.to_le_bytes(), observer_pubkey]` | ario-gar |

### ario-arns

| PDA | Seeds | Program |
|-----|-------|---------|
| ArnsConfig | `["arns_config"]` | ario-arns |
| DemandFactor | `["demand_factor"]` | ario-arns |
| ArnsRecord | `["arns_record", SHA256(name.to_lowercase())]` | ario-arns |
| ReturnedName | `["returned_name", SHA256(name.to_lowercase())]` | ario-arns |
| ReservedName | `["reserved_name", SHA256(name.to_lowercase())]` | ario-arns |
| NameRegistry | `["name_registry"]` | ario-arns |

### ario-ant

| PDA | Seeds | Program |
|-----|-------|---------|
| AntMigrationConfig | `["ant_migration_config"]` | ario-ant |
| AntConfig | `["ant_config", asset_pubkey]` | ario-ant |
| AntControllers | `["ant_controllers", asset_pubkey]` | ario-ant |
| AntRecord | `["ant_record", asset_pubkey, SHA256(undername.to_lowercase())]` | ario-ant |

### Name hashing convention

Variable-length names are hashed for PDA derivation to avoid the 32-byte seed limit:

```
seed_component = SHA256(name.to_lowercase().as_bytes())
```

This applies to: ArNS record names, returned names, reserved names, primary name
reverse lookups, and ANT undername records. The hash function is
`anchor_lang::solana_program::hash::hash()` (SHA256).

---

## 7. Fixed-Point Arithmetic

### Scale factors

| Scale | Value | Usage |
|-------|-------|-------|
| `RATE_SCALE` | 1,000,000 (1e6) | All percentage rates, weight ratios, penalty rates |
| `DEMAND_FACTOR_SCALE` | 1,000,000 (1e6) | ArNS demand factor and pricing |
| `REWARD_PRECISION` | 1,000,000,000,000,000,000 (1e18) | Delegate reward-per-share accumulator |
| Basis points (delegate_reward_share_ratio) | 10,000 = 100% | `MAX_DELEGATE_REWARD_SHARE = 9500` (95%) |

### Rate encoding examples

| Rate | RATE_SCALE value | Meaning |
|------|-----------------|---------|
| 100% | 1,000,000 | `FAILED_GATEWAY_SLASH_RATE` |
| 90%  | 900,000 | `GATEWAY_OPERATOR_REWARD_RATE` |
| 60%  | 600,000 | `MAX_REDELEGATION_PENALTY` |
| 50%  | 500,000 | `MAX_EXPEDITED_WITHDRAWAL_PENALTY` |
| 25%  | 250,000 | `MISSED_OBSERVATION_PENALTY` |
| 20%  | 200,000 | `GATEWAY_OPERATOR_DISCOUNT_PCT` |
| 10%  | 100,000 | `MIN_EXPEDITED_WITHDRAWAL_PENALTY` |
| 0.2% | 200,000 | `ANNUAL_PERCENTAGE_FEE` (same encoding as 20%) |
| 0.1% | 1,000 | `MAX_REWARD_RATE` |
| 0.05%| 500 | `MIN_REWARD_RATE` |

### Overflow protection

All pricing and reward calculations use `u128` intermediates to prevent overflow:

```
// ArNS pricing pattern (pricing.rs)
let result = (base_fee as u128)
    .checked_mul(demand_factor as u128)?
    .checked_mul(year_factor as u128)?
    .checked_div(SCALE as u128)?
    .checked_div(SCALE as u128)?;
u64::try_from(result)?
```

```
// Delegate reward accumulator pattern (gar state)
let pending = (delegation.amount as u128)
    .checked_mul(delta)?             // delta = cumulative - debt
    .map(|v| v / REWARD_PRECISION)   // 1e18
    .unwrap_or_else(|| ...);         // fallback for extreme overflow
```

```
// Gateway weight computation (gar state)
let composite = (stake_weight as u128)
    * (tenure_weight as u128) / scale
    * (gw_perf_ratio as u128) / scale
    * (obs_perf_ratio as u128) / scale;
```

### Reward rate decay formula

Linear interpolation between `MAX_REWARD_RATE` and `MIN_REWARD_RATE`:

```
if epoch_index < REWARD_DECAY_START_EPOCH (365):
    rate = MAX_REWARD_RATE (1,000 = 0.1%)
elif epoch_index > REWARD_DECAY_LAST_EPOCH (547):
    rate = MIN_REWARD_RATE (500 = 0.05%)
else:
    total_decay_period = 547 - 365 = 182
    epochs_decayed = epoch_index - 365
    rate_range = 1,000 - 500 = 500
    decay = rate_range * epochs_decayed / total_decay_period
    rate = MAX_REWARD_RATE - decay
```

### ArNS pricing formulas

**Lease registration:** `floor(base_fee * demand_factor * (1 + 0.2 * years) / SCALE / SCALE)`

**Permabuy:** `floor(base_fee * demand_factor * 5 / SCALE)` (equivalent to 1 + 0.2 * 20 = 5)

**Lease extension:** `floor(base_fee * demand_factor * 0.2 * years / SCALE / SCALE)`

**Undername cost (lease):** `floor(base_fee * demand_factor * 0.001 * qty / SCALE / SCALE)`

**Undername cost (permabuy):** `floor(base_fee * demand_factor * 0.005 * qty / SCALE / SCALE)`

**Returned name premium:** Decays linearly from 50x to 1x over 14 days:
```
pct_remaining = (duration - elapsed) / duration  (in SCALE)
multiplier = 50 * pct_remaining                  (in SCALE)
cost = registration_fee * multiplier / SCALE
```

**Gateway operator discount:** 20% off: `cost - cost * 200,000 / 1,000,000`

### Demand factor adjustment

Each period (1 day), the demand factor is adjusted based on the configured criteria
(revenue or purchases):

```
if demand_increasing:
    factor = factor * 1,050,000 / 1,000,000  (1.05x up)
else:
    factor = max(factor * 985,000 / 1,000,000, 500,000)  (0.985x down, floor 0.5)

if consecutive_periods_at_min >= 7:
    fees[*] /= 2  (permanent halving)
    factor = 1,000,000  (reset to 1.0)
```

### Redelegation fee schedule

Resets after 7 days of no redelegations. Scales linearly from 0% to 60%:

```
fee_rate = min(10% * redelegation_count, 60%)
```

| Redelegation # | Fee Rate (RATE_SCALE) | Percentage |
|---------------|----------------------|------------|
| 1st (count=0) | 0 | 0% (free) |
| 2nd (count=1) | 100,000 | 10% |
| 3rd (count=2) | 200,000 | 20% |
| 4th (count=3) | 300,000 | 30% |
| 5th (count=4) | 400,000 | 40% |
| 6th (count=5) | 500,000 | 50% |
| 7th+ (count>=6) | 600,000 | 60% (capped) |

---

## 8. Epoch Lifecycle

### 6-step ordering

Each epoch progresses through these steps in strict order. Each step has a guard
requiring the previous step to be complete.

```
Step 1: create_epoch
        Guard: epoch_settings.enabled == true
               clock >= genesis + epoch_index * duration
        Sets:  epoch_index, timestamps, reward_rate, active_gateway_count
               Reads protocol_token_account.amount for total_eligible_rewards

Step 2: tally_weights (batched, permissionless crank)
        Guard: epoch.weights_tallied == 0
        Input: remaining_accounts = gateway PDAs (ordered by registry index)
        Sets:  gateway.weights, registry[i].composite_weight, epoch.total_composite_weight
        Done:  epoch.tally_index >= active_gateway_count -> weights_tallied = 1

Step 3: prescribe_epoch (single call)
        Guard: epoch.weights_tallied != 0
               epoch.prescriptions_done == 0
        Input: remaining_accounts = observer gateway PDAs + NameRegistry PDA
        Sets:  prescribed_observers[50], prescribed_observer_gateways[50],
               prescribed_names[[u8;32]; 2]
               Computes per_gateway_reward, per_observer_reward
        Done:  prescriptions_done = 1

Step 4: save_observations (per-observer, during epoch window)
        Guard: prescriptions_done != 0
               clock >= epoch.start_timestamp
               clock < epoch.end_timestamp
        Input: gateway_results bitmap [u8; 256], report_tx_id [u8; 32]
        Sets:  observation PDA, epoch.failure_counts[], epoch.has_observed bitmap
        Constraint: One per prescribed observer (PDA uniqueness)

Step 5: distribute_epoch (batched, permissionless crank, after epoch ends)
        Guard: prescriptions_done != 0
               clock >= epoch.end_timestamp
               epoch.rewards_distributed == 0
        Input: remaining_accounts = gateway PDAs (ordered by registry index)
        Sets:  gateway.stats (passed/failed/prescribed/observed), gateway.weights.normalized,
               gateway.cumulative_reward_per_token, epoch.distribution_index
        Done:  distribution_index >= active_gateway_count -> rewards_distributed = 1

Step 6: close_epoch (permissionless, after distribution complete)
        Guard: epoch.rewards_distributed != 0
               epoch is at least 7 epochs old (retention window)
        Effect: Closes epoch PDA, returns rent to payer
        Note: Epoch data is retained for 7 epochs before becoming closeable
```

### Timing constraints

```
Epoch N timeline:
|---create_epoch--->|---tally/prescribe--->|---observations--->|---distribute--->|
^                   ^                       ^                   ^
epoch_start         (any time after         (start to end)      epoch_end
                     create_epoch)
```

- `create_epoch`: callable once `clock >= genesis + N * duration`
- `tally_weights` + `prescribe_epoch`: callable any time after epoch creation
- `save_observations`: only during `[epoch.start_timestamp, epoch.end_timestamp)`
- `distribute_epoch`: only after `epoch.end_timestamp`

### Reward computation

```
total_eligible = protocol_balance * reward_rate / RATE_SCALE

gateway_pool = total_eligible * gateway_reward_ratio / RATE_SCALE  (90%)
observer_pool = total_eligible * observer_reward_ratio / RATE_SCALE (10%)

per_gateway = gateway_pool / active_gateway_count
per_observer = observer_pool / observer_count
```

Per-gateway reward during distribution:
```
if gateway passed:
    reward = per_gateway + (per_observer if prescribed and observed)
    operator_share = reward * (10000 - delegate_reward_share_ratio) / 10000
    delegate_share = reward - operator_share
    gateway.cumulative_reward_per_token += delegate_share * REWARD_PRECISION / total_delegated_stake

if gateway failed:
    operator_stake slashed by: per_gateway * failed_gateway_slash_rate / RATE_SCALE
    if consecutive failures >= max_consecutive_failures: gateway pruned
```

---

## 9. Registry Capacity

### GatewayRegistry (ario-gar)

| Property | Value |
|----------|-------|
| Max slots | 3,000 |
| Slot size | 40 bytes (32 address + 8 composite_weight) |
| Header | 40 bytes (32 authority + 4 count + 4 padding) |
| Total size | 120,040 bytes |
| Zero-copy | Yes (`#[account(zero_copy(unsafe))]`) |
| Requires pre-allocation | Yes |

Operations: O(1) add (append), O(1) remove (swap-with-last), O(n) enumeration.

### NameRegistry (ario-arns)

| Property | Value |
|----------|-------|
| Max slots (initial) | 50,000 — expandable via `admin_expand_name_registry` |
| Entry size | 40 bytes (32 name_hash + 4 registry_index + 4 padding) |
| Header | 40 bytes (32 authority + 4 count + 4 padding) |
| Total size (initial) | 2,000,040 bytes |
| Zero-copy | Yes (`#[account(zero_copy(unsafe))]`) |
| Requires pre-allocation | Yes |

Used by `prescribe_epoch` to select random names for the epoch without requiring an
off-chain indexer.

### Cross-program reads

The ario-gar program reads the NameRegistry during `prescribe_epoch` by:
1. Accepting the NameRegistry as a `remaining_account`
2. Validating the account is owned by the ario-arns program
3. Validating PDA derivation: `["name_registry"]` under ario-arns
4. Reading raw bytes using `read_name_registry_header()` and `read_name_entry()`

The ario-arns program reads Gateway accounts during name purchase by:
1. Accepting the Gateway PDA as a `remaining_account`
2. Validating the account is owned by the ario-gar program
3. Validating PDA derivation: `["gateway", signer_pubkey]` under ario-gar
4. Deserializing and checking operator, status, tenure (180 days), and performance (90% pass rate)

---

## 10. SOL Cost Estimates per Operation

All costs are **rent deposits** (recoverable when accounts close) plus the
Solana base transaction fee (0.000005 SOL per signature). Priority fees are
additional and depend on network congestion. Estimates at **$83/SOL**
(2026-04-30). Rent uses the Solana formula:
`(data_len + 128) × 3480 × 2 / 1e9` SOL.

### User operations

| Operation | Accounts Created | Rent (SOL) | Rent (USD) | Notes |
|-----------|-----------------|-----------|-----------|-------|
| **Spawn ANT** | MPL Core asset + AntConfig + AntControllers + AntRecord(@) | 0.01495 | $1.24 | All rent paid by caller; 300K CU budget |
| **Buy ArNS name** (existing ANT) | ArnsRecord | 0.00208 | $0.17 | Name record only; NameRegistry entry uses existing pre-allocated slot |
| **Full name purchase** (spawn + buy) | 5 accounts | 0.01703 | $1.41 | Can compose spawn + buy in single tx |
| **Extend lease** | None | 0.00001 | $0.00 | Tx fee only (no new accounts) |
| **Upgrade to permabuy** | None | 0.00001 | $0.00 | Tx fee only |
| **Increase undernames** | None | 0.00001 | $0.00 | Tx fee only |
| **Add undername record** | AntRecord | 0.00307 | $0.25 | Per additional undername |
| **Add undername metadata** | AntRecordMetadata | 0.00605 | $0.50 | Optional; only if display_name/logo/description/keywords set |
| **Join gateway network** | Gateway + ObserverLookup | 0.00863 | $0.72 | Plus 20,000 ARIO stake required |
| **Delegate stake** | Delegation | 0.00162 | $0.13 | Per gateway-delegator pair |
| **Create vault** | Vault + VaultCounter (first time) | 0.00287 | $0.24 | VaultCounter created once; subsequent vaults cost 0.00164 SOL |
| **Initiate withdrawal** | Withdrawal + WithdrawalCounter (first) | 0.00287 | $0.24 | WithdrawalCounter created once; subsequent withdrawals cost 0.00164 SOL |
| **Set primary name** (instant) | PrimaryName + PrimaryNameReverse | 0.00334 | $0.28 | Plus ARIO fee (0.2 ARIO × demand factor) |
| **Request primary name** (async) | PrimaryNameRequest | 0.00175 | $0.15 | Rent returned when request approved/expired |
| **Add to allowlist** | AllowlistEntry | 0.00146 | $0.12 | Per gateway-delegate pair |
| **Submit observation** | Observation | 0.00414 | $0.34 | Per observer per epoch; closeable after epoch closes |
| **Redelegate stake** | RedelegationRecord (first time) | 0.00132 | $0.11 | Created once per delegator; reused on subsequent redelegations |

### Cranker / epoch operations

| Operation | Accounts Created | Rent (SOL) | Rent (USD) | Notes |
|-----------|-----------------|-----------|-----------|-------|
| **Create epoch** | Epoch PDA | 0.06632 | $5.50 | Recovered when epoch closed (after 7-epoch retention) |
| **Tally weights** | None | 0.00001 | $0.00 | Batched; ~55 gateways per tx |
| **Prescribe epoch** | None | 0.00001 | $0.00 | Single call |
| **Distribute epoch** | None | 0.00001 | $0.00 | Batched; ~15 gateways per tx (CU-limited) |
| **Close epoch** | None (closes Epoch PDA) | 0.00001 | $0.00 | Returns ~0.066 SOL rent to closer |

### Protocol singleton initialization (one-time)

| Account | Rent (SOL) | Rent (USD) | Notes |
|---------|-----------|-----------|-------|
| ArioConfig | 0.00246 | $0.20 | |
| GatewaySettings | 0.00266 | $0.22 | |
| EpochSettings | 0.00200 | $0.17 | |
| ArnsConfig | 0.00214 | $0.18 | |
| DemandFactor | 0.00489 | $0.41 | Includes 51-entry fee table |
| GatewayRegistry | 0.83637 | $69.42 | 120KB zero-copy; pre-allocated |
| NameRegistry | 55.68117 | $4,621.54 | 8MB zero-copy; pre-allocated |

### ANT spawn details

Spawning an ANT is a **2-instruction atomic transaction**:

1. **MPL Core `CreateV1`** — mints the Metaplex Core NFT asset with an
   Attributes plugin (ArNS Name, Type, Undername Limit). Even newly-spawned
   ANTs carry the empty plugin to be `purchase`-ready.
2. **ario-ant `initialize`** — creates `AntConfig`, `AntControllers`, and
   the root `AntRecord` ("@") PDAs.

| Component | Size | Rent (SOL) |
|-----------|------|-----------|
| MPL Core asset | ~200B | 0.00228 |
| AntConfig | 758B | 0.00617 |
| AntControllers | 365B | 0.00343 |
| AntRecord (@) | 313B | 0.00307 |
| **Total** | **~1,636B** | **0.01495** |

CU budget: 300,000 (configurable). Empirical: ~130K–220K CU.

### Priority fee reference

Priority fees are optional and depend on network congestion. They are
computed as: `priority_fee = requested_CU × microlamports_per_CU / 1e6`.

| CU Budget | @ 1 µL/CU | @ 10 µL/CU | @ 100 µL/CU |
|-----------|-----------|------------|-------------|
| 200K (ario-ant) | 0.0002 SOL | 0.002 SOL | 0.02 SOL |
| 400K (ario-core) | 0.0004 SOL | 0.004 SOL | 0.04 SOL |
| 1M (ario-gar/arns) | 0.001 SOL | 0.01 SOL | 0.1 SOL |

Typical mainnet priority fees range from 1–50 µL/CU depending on congestion.
At 10 µL/CU, a 1M CU transaction adds ~$0.001 in priority fees.

### Rent recovery

All account rent is recoverable when the account is closed:
- **Vaults/Withdrawals:** rent returned when vault releases or withdrawal claims
- **Delegations:** rent returned via `close_empty_delegation` (permissionless)
- **Observations:** rent returned via `close_observation` after epoch closes
- **Epochs:** rent returned via `close_epoch` after 7-epoch retention window
- **PrimaryNameRequests:** rent returned when approved, expired, or cancelled
- **AllowlistEntries:** rent returned when removed
- **ArNS records (expired leases):** rent returned when pruned
- **AntRecords/AntRecordMetadata:** rent returned when removed by ANT holder
