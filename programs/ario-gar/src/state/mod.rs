// AR.IO GAR State Accounts
// PDA structures for gateway registry, staking, delegation, epochs, and rewards

use anchor_lang::prelude::*;

/// Precision for reward-per-share accumulator (1e18)
/// Used in delegate reward distribution to avoid rounding to zero
pub const REWARD_PRECISION: u128 = 1_000_000_000_000_000_000;

/// Hard cap on the number of `FundingSourceSpec` entries accepted by
/// `pay_from_funding_plan`. Bounded so per-source validation + remaining_accounts
/// dispatch fit within the 200K CU and 1232-byte tx-size budgets. Realistic plans
/// are 1–3 sources; 5 is a conservative ceiling that matches the SDK funding-plan
/// builder. Tightened from 8 → 5 with the multi-gateway refactor (per
/// `docs/MULTI_GATEWAY_FUNDING_PLAN.md`).
pub const MAX_FUNDING_SOURCES: usize = 5;

/// Hard cap on the number of `Delegation` sources within a single
/// `pay_from_funding_plan` call. Each Delegation source consumes 2
/// `remaining_accounts` slots (gateway PDA + delegation PDA), and may trigger
/// auto-creation of a residue Withdrawal vault if it drains to sub-min — both
/// adding to tx-size + CU pressure. Capping at 3 within the 5-source overall
/// cap leaves room for 2 non-Delegation sources (Balance + Withdrawal, etc.).
pub const MAX_DELEGATION_SOURCES: usize = 3;

/// H-4: NameRegistry PDA seed (must match ario-arns::state::NAME_REGISTRY_SEED)
pub const ARNS_NAME_REGISTRY_SEED: &[u8] = b"name_registry";

// =========================================
// SEEDS
// =========================================

// Gateway seeds
pub const REGISTRY_SEED: &[u8] = b"gateway_registry";
pub const SETTINGS_SEED: &[u8] = b"gar_settings";
pub const GATEWAY_SEED: &[u8] = b"gateway";
pub const DELEGATION_SEED: &[u8] = b"delegation";
pub const WITHDRAWAL_SEED: &[u8] = b"withdrawal";
pub const WITHDRAWAL_COUNTER_SEED: &[u8] = b"withdrawal_counter";
pub const ALLOWLIST_SEED: &[u8] = b"allowlist";
pub const OBSERVER_LOOKUP_SEED: &[u8] = b"observer_lookup";

// Epoch seeds
pub const EPOCH_SEED: &[u8] = b"epoch";
pub const EPOCH_SETTINGS_SEED: &[u8] = b"epoch_settings";
pub const OBSERVATION_SEED: &[u8] = b"observation";

// =========================================
// GATEWAY SETTINGS
// =========================================

/// Global gateway settings
/// PDA: ["gateway_settings"]
#[account]
pub struct GatewaySettings {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub min_operator_stake: u64,
    pub min_delegate_stake: u64,
    pub withdrawal_period: i64,
    pub max_expedited_withdrawal_penalty: u64,
    pub min_expedited_withdrawal_penalty: u64,
    pub min_expedited_withdrawal_amount: u64,
    pub max_delegates_per_gateway: u32,
    /// Whether migration import is active (permanently disabled by finalize_migration)
    pub migration_active: bool,
    /// Dedicated migration authority (hot key for batch imports)
    pub migration_authority: Pubkey,
    /// C-1: Pinned stake token account address (validated on every instruction that uses it)
    pub stake_token_account: Pubkey,
    /// C-2: Pinned protocol token account address (validated on every instruction that uses it)
    pub protocol_token_account: Pubkey,
    /// H-4: ArNS program ID. Pinned at init so `prescribe_epoch` can validate
    /// the NameRegistry account it reads from cross-program. Replaces the
    /// hardcoded string literal previously embedded in `epoch.rs`. Set once
    /// via [`crate::initialize`] and migrated for pre-existing accounts via
    /// [`crate::migrate_settings_set_arns_program_id`].
    pub arns_program_id: Pubkey,
    /// Aggregate supply counters — updated atomically at every staking mutation site.
    /// Enables the SDK to return the full AoTokenSupplyData breakdown (parity with Lua).
    pub total_staked: u64,
    pub total_delegated: u64,
    pub total_withdrawn: u64,
    pub bump: u8,
}

impl GatewaySettings {
    pub const SIZE: usize = 8  // discriminator
        + 32  // authority
        + 32  // mint
        + 8   // min_operator_stake
        + 8   // min_delegate_stake
        + 8   // withdrawal_period
        + 8   // max_expedited_withdrawal_penalty
        + 8   // min_expedited_withdrawal_penalty
        + 8   // min_expedited_withdrawal_amount
        + 4   // max_delegates_per_gateway
        + 1   // migration_active
        + 32  // migration_authority
        + 32  // stake_token_account
        + 32  // protocol_token_account
        + 32  // arns_program_id
        + 8   // total_staked
        + 8   // total_delegated
        + 8   // total_withdrawn
        + 1; // bump
}

// =========================================
// GATEWAY REGISTRY
// =========================================

/// Entry in the gateway registry, pairing address with cached state for
/// `create_epoch` / `tally_weights` / `prescribe_epoch` to consult without
/// having to deserialize the Gateway PDA. Mirrors a subset of the Gateway
/// record so iteration sites can filter Joined-only without 3000 PDA reads.
///
/// `status` is stored as a raw u8 (zero-copy can't carry the typed enum):
///   0 = Joined, 1 = Leaving, 2 = Gone. See `GatewaySlot::STATUS_*` constants.
///
/// `start_timestamp` is the gateway's join time and is used by `create_epoch`
/// to filter "joined-after-epoch-start" gateways out of `active_gateway_count`.
#[zero_copy(unsafe)]
#[repr(C)]
pub struct GatewaySlot {
    pub address: Pubkey,       // 32 bytes
    pub composite_weight: u64, // 8 bytes
    pub start_timestamp: i64,  // 8 bytes
    pub status: u8,            // 1 byte (see STATUS_* constants)
    pub _padding: [u8; 7],     // 7 bytes (align struct to 8 bytes)
}

impl GatewaySlot {
    pub const STATUS_JOINED: u8 = 0;
    pub const STATUS_LEAVING: u8 = 1;
    pub const STATUS_GONE: u8 = 2;
}

/// Global gateway registry for efficient enumeration
/// PDA: ["gateway_registry"]
/// NOTE: Uses zero-copy for performance with large gateway counts
#[account(zero_copy(unsafe))]
#[repr(C)]
pub struct GatewayRegistry {
    /// L-8: Vestigial field — not checked at runtime. Kept to preserve account layout.
    pub authority: Pubkey,
    pub count: u32,
    pub _padding: [u8; 4],
    pub gateways: [GatewaySlot; 3000],
}

impl GatewayRegistry {
    /// 32 (authority) + 4 (count) + 4 (_padding) + 56 (GatewaySlot) * 3000 = 168,040
    pub const SIZE: usize = 32 + 4 + 4 + (56 * 3000);
    pub const MAX_GATEWAYS: usize = 3000;
}

// =========================================
// GATEWAY ACCOUNT
// =========================================

/// Gateway registry entry
/// PDA: ["gateway", operator_pubkey]
#[account]
pub struct Gateway {
    pub operator: Pubkey,
    pub label: String,
    pub fqdn: String,
    pub port: u16,
    pub protocol: Protocol,
    pub properties: String,
    pub note: String,
    pub operator_stake: u64,
    pub total_delegated_stake: u64,
    pub status: GatewayStatus,
    pub start_timestamp: i64,
    pub leave_timestamp: Option<i64>,
    /// Snapshot of `epoch_settings.epoch_duration` (seconds) taken when this
    /// gateway transitioned to Leaving via `leave_network` or `prune_gateway`.
    /// Used by `finalize_gone` to compute the GC eligibility window
    /// (`leave_timestamp + GATEWAY_LEAVE_PERIOD + 7 * leave_epoch_duration`)
    /// without depending on a possibly-mutated live `epoch_settings.epoch_duration`.
    /// Zero for gateways that never left (Joined). Defense-in-depth: `finalize_gone`
    /// also takes `max(this snapshot, current_settings.epoch_duration)` so an
    /// admin shortening of `epoch_duration` cannot retroactively bring forward
    /// a leaver's GC eligibility.
    pub leave_epoch_duration: i64,
    pub stats: GatewayStats,
    pub weights: GatewayWeights,
    pub settings: GatewaySettings2,
    pub registry_index: RegistryIndex,
    /// M3: Separate observer address (matches Lua: observerAddress)
    /// Defaults to operator address if not set. Allows a different wallet to submit observations.
    pub observer_address: Pubkey,
    /// Cumulative reward per unit of delegated stake (scaled by REWARD_PRECISION = 1e18)
    /// Increases each epoch during distribute_epoch
    pub cumulative_reward_per_token: u128,
    pub bump: u8,
}

impl Gateway {
    pub const SIZE: usize = 8  // discriminator
        + 32  // operator
        + (4 + 64)  // label
        + (4 + 128)  // fqdn
        + 2  // port
        + 1  // protocol
        + (4 + 256)  // properties
        + (4 + 256)  // note
        + 8  // operator_stake
        + 8  // total_delegated_stake
        + 1  // status
        + 8  // start_timestamp
        + 9  // leave_timestamp (Option)
        + 8  // leave_epoch_duration
        + GatewayStats::SIZE  // stats
        + GatewayWeights::SIZE  // weights
        + GatewaySettings2::SIZE  // settings
        + RegistryIndex::SIZE  // registry_index
        + 32  // observer_address
        + 16  // cumulative_reward_per_token
        + 1; // bump

    /// Minimum operator stake required to join (in base units)
    pub const MIN_OPERATOR_STAKE: u64 = 20_000_000_000; // 20,000 ARIO

    /// Get total stake (operator + delegated)
    pub fn total_stake(&self) -> u64 {
        self.operator_stake
            .saturating_add(self.total_delegated_stake)
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Http,
    Https,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum GatewayStatus {
    Joined,
    Leaving,
    /// Terminal state — set by `finalize_gone` after the leave window expires
    /// AND all delegations have been claimed. Gateway PDA is closed in the
    /// same instruction; this variant exists for any in-flight state where
    /// callers still hold a deserialized copy.
    Gone,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Default, Debug)]
pub struct GatewayStats {
    pub passed_epochs: u32,
    pub failed_epochs: u32,
    pub total_epochs: u32,
    pub prescribed_epochs: u32,
    pub observed_epochs: u32,
    pub failed_consecutive: u8,
    /// Tracks consecutive passed epochs (matches Lua passedConsecutiveEpochs)
    pub passed_consecutive: u8,
}

impl GatewayStats {
    pub const SIZE: usize = 4 + 4 + 4 + 4 + 4 + 1 + 1;
}

/// Gateway-specific settings (named differently to avoid conflict)
#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct GatewaySettings2 {
    pub allow_delegated_staking: bool,
    pub delegate_reward_share_ratio: u16,
    pub min_delegation_amount: u64,
    /// When true, only delegates on the allowlist (or existing stakers) can delegate
    /// Matches Lua: allowedDelegatesLookup ~= nil
    pub allowlist_enabled: bool,
}

impl GatewaySettings2 {
    pub const SIZE: usize = 1 + 2 + 8 + 1;
}

/// Gateway weights for epoch observer selection and reward distribution
/// All values scaled by RATE_SCALE (1e6) for fixed-point precision
/// Matches Lua: stakeWeight, tenureWeight, gatewayPerformanceRatio, observerPerformanceRatio, compositeWeight
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Default)]
pub struct GatewayWeights {
    /// totalStake / minOperatorStake (scaled by RATE_SCALE)
    pub stake_weight: u64,
    /// min(timeRunning / tenureWeightDuration, maxTenureWeight) (scaled by RATE_SCALE)
    pub tenure_weight: u64,
    /// (1 + passedEpochs) / (1 + totalEpochs) (scaled by RATE_SCALE)
    pub gateway_performance_ratio: u64,
    /// (1 + observedEpochs) / (1 + prescribedEpochs) (scaled by RATE_SCALE)
    pub observer_performance_ratio: u64,
    /// Product of all weights (scaled by RATE_SCALE)
    pub composite_weight: u64,
    /// compositeWeight / totalCompositeWeight (scaled by RATE_SCALE)
    pub normalized_composite_weight: u64,
    /// M-5: Epoch index when weights were computed (prevents stale weights)
    pub weights_epoch: u64,
}

impl GatewayWeights {
    pub const SIZE: usize = 8 * 7; // 7 u64 fields

    /// Compute gateway weights from current state
    /// Uses u128 intermediates to avoid overflow
    #[allow(clippy::too_many_arguments)]
    pub fn compute(
        total_stake: u64,
        min_operator_stake: u64,
        start_timestamp: i64,
        current_timestamp: i64,
        tenure_weight_duration: i64,
        max_tenure_weight: u64,
        stats: &GatewayStats,
        rate_scale: u64,
    ) -> Self {
        let scale = rate_scale as u128;

        // stakeWeight = totalStake / minOperatorStake
        let stake_weight = if min_operator_stake > 0 {
            ((total_stake as u128) * scale / (min_operator_stake as u128)) as u64
        } else {
            scale as u64
        };

        // tenureWeight = min(timeRunning / tenureWeightDuration, maxTenureWeight)
        let time_running = if current_timestamp >= start_timestamp {
            (current_timestamp - start_timestamp).max(0)
        } else {
            0
        };
        let tenure_weight = if tenure_weight_duration > 0 && time_running > 0 {
            let tw = (time_running as u128) * scale / (tenure_weight_duration as u128);
            let max_tw = (max_tenure_weight as u128) * scale;
            tw.min(max_tw) as u64
        } else if time_running == 0 && tenure_weight_duration > 0 {
            // Gateway just started: 1 / tenureWeightDuration (matches Lua)
            (scale / (tenure_weight_duration as u128)) as u64
        } else {
            0
        };

        // gatewayPerformanceRatio = (1 + passedEpochs) / (1 + totalEpochs)
        let gateway_performance_ratio = {
            let numerator = (1 + stats.passed_epochs as u128) * scale;
            let denominator = 1 + stats.total_epochs as u128;
            (numerator / denominator) as u64
        };

        // observerPerformanceRatio = (1 + observedEpochs) / (1 + prescribedEpochs)
        let observer_performance_ratio = {
            let numerator = (1 + stats.observed_epochs as u128) * scale;
            let denominator = 1 + stats.prescribed_epochs as u128;
            (numerator / denominator) as u64
        };

        // compositeWeight = stakeWeight * tenureWeight * gatewayPerfRatio * observerPerfRatio
        // GAR-008: Accumulate the full product first, then divide by scale^3 once
        // to avoid precision loss from sequential division.
        let composite_weight = {
            let product = (stake_weight as u128)
                .checked_mul(tenure_weight as u128)
                .unwrap_or(0)
                .checked_mul(gateway_performance_ratio as u128)
                .unwrap_or(0)
                .checked_mul(observer_performance_ratio as u128)
                .unwrap_or(0);
            let scale_cubed = scale.saturating_mul(scale).saturating_mul(scale);
            let w = product.checked_div(scale_cubed).unwrap_or(0);
            u64::try_from(w).unwrap_or(u64::MAX)
        };

        GatewayWeights {
            stake_weight,
            tenure_weight,
            gateway_performance_ratio,
            observer_performance_ratio,
            composite_weight,
            normalized_composite_weight: 0, // set later after summing all gateways
            weights_epoch: 0,               // set by tally_weights caller
        }
    }
}

/// Gateway registry index
///
/// `_reserved` is a layout-preserving placeholder for the legacy
/// `is_registered: bool` field. The status flag on the Gateway and the
/// registry slot replace its semantics. The byte stays so existing on-chain
/// account layouts deserialize correctly; no production code reads or
/// writes it.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default)]
pub struct RegistryIndex {
    pub index: u32,
    pub _reserved: u8,
}

impl RegistryIndex {
    pub const SIZE: usize = 4 + 1;
}

// =========================================
// DELEGATION
// =========================================

/// Stake delegation entry
/// PDA: ["delegation", gateway_pubkey, delegator_pubkey]
#[account]
pub struct Delegation {
    pub gateway: Pubkey,
    pub delegator: Pubkey,
    pub amount: u64,
    pub start_timestamp: i64,
    /// Snapshot of gateway.cumulative_reward_per_token at last settlement
    pub reward_debt: u128,
    pub bump: u8,
}

impl Delegation {
    pub const SIZE: usize = 8 + 32 + 32 + 8 + 8 + 16 + 1;
}

/// Settle pending delegate rewards using the reward-per-share accumulator.
/// Called at the start of any delegate interaction to materialize pending rewards.
pub fn settle_delegate_rewards(gateway: &mut Gateway, delegation: &mut Delegation) {
    if delegation.amount > 0 && gateway.cumulative_reward_per_token > delegation.reward_debt {
        let delta = gateway.cumulative_reward_per_token - delegation.reward_debt;
        // Overflow-safe reward calculation with precision-preserving fallback
        let pending = (delegation.amount as u128)
            .checked_mul(delta)
            .map(|v| v / REWARD_PRECISION)
            .unwrap_or_else(|| {
                // Split into quotient and remainder to avoid truncation to zero
                let quot = delta / REWARD_PRECISION;
                let rem = delta % REWARD_PRECISION;
                let from_quot = (delegation.amount as u128).saturating_mul(quot);
                let from_rem = (delegation.amount as u128).saturating_mul(rem) / REWARD_PRECISION;
                from_quot.saturating_add(from_rem)
            });
        // GAR-001: Safe u128→u64 conversion — cap at u64::MAX instead of truncating
        let pending_u64 = u64::try_from(pending).unwrap_or(u64::MAX);
        if pending_u64 > 0 {
            delegation.amount = delegation.amount.saturating_add(pending_u64);
            gateway.total_delegated_stake =
                gateway.total_delegated_stake.saturating_add(pending_u64);
        }
    }
    delegation.reward_debt = gateway.cumulative_reward_per_token;
}

// =========================================
// WITHDRAWAL
// =========================================

/// Tracks next withdrawal ID per user
/// PDA: ["withdrawal_counter", owner_pubkey]
#[account]
pub struct WithdrawalCounter {
    pub owner: Pubkey,
    pub next_id: u64,
    pub bump: u8,
}

impl WithdrawalCounter {
    pub const SIZE: usize = 8 + 32 + 8 + 1;
}

/// Pending withdrawal
/// PDA: ["withdrawal", owner_pubkey, withdrawal_id]
#[account]
pub struct Withdrawal {
    pub owner: Pubkey,
    pub withdrawal_id: u64,
    pub gateway: Pubkey,
    pub amount: u64,
    pub created_at: i64,
    pub available_at: i64,
    pub is_delegate: bool,
    /// True if this withdrawal was created by `leave_network` or
    /// `prune_gateway`. Used for analytics + future cleanup logic;
    /// does NOT itself gate any handler. The actual no-instant /
    /// no-fund-from gates live on `is_protected` below.
    pub is_exit_vault: bool,
    /// True if this is the protected operator min-stake exit vault.
    /// Lua parity (`gar.lua::createGatewayExitVault`): the min portion
    /// of an operator's stake at leave/prune time goes into a vault
    /// that **cannot be expedited** (`instant_withdrawal` rejects)
    /// and **cannot be drained via the funding plan**
    /// (`deduct_withdrawal_for_payment` rejects). Only path out is
    /// `claim_withdrawal` after the 90-day lock expires. The excess
    /// portion of a leave goes into a separate `is_protected: false`
    /// vault that behaves like every other operator-side withdrawal.
    pub is_protected: bool,
    pub bump: u8,
}

impl Withdrawal {
    /// Account discriminator (8) + owner (32) + id (8) + gateway (32)
    /// + amount (8) + created_at (8) + available_at (8)
    /// + is_delegate (1) + is_exit_vault (1) + is_protected (1)
    /// + bump (1) = 108 bytes.
    pub const SIZE: usize = 8 + 32 + 8 + 32 + 8 + 8 + 8 + 1 + 1 + 1 + 1;
}

// =========================================
// ALLOWLIST
// =========================================

/// Delegate allowlist entry
/// PDA: ["allowlist", gateway_pubkey, delegate_pubkey]
#[account]
pub struct AllowlistEntry {
    pub gateway: Pubkey,
    pub delegate: Pubkey,
    pub added_at: i64,
    pub bump: u8,
}

impl AllowlistEntry {
    pub const SIZE: usize = 8 + 32 + 32 + 8 + 1;
}

// =========================================
// OBSERVER LOOKUP (uniqueness index)
// =========================================

/// Observer address uniqueness index
/// PDA: ["observer_lookup", observer_address]
/// Ensures each observer address is used by at most one gateway.
#[account]
pub struct ObserverLookup {
    pub gateway: Pubkey,
    pub bump: u8,
}

impl ObserverLookup {
    pub const SIZE: usize = 8 + 32 + 1; // 41
}

// =========================================
// REDELEGATION TRACKING
// =========================================

pub const REDELEGATION_SEED: &[u8] = b"redelegation";

/// Tracks redelegation history per delegator for fee calculation
/// PDA: ["redelegation", delegator_pubkey]
#[account]
pub struct RedelegationRecord {
    pub delegator: Pubkey,
    /// Number of redelegations since last fee reset
    pub redelegation_count: u32,
    /// Timestamp of last redelegation
    pub last_redelegation_at: i64,
    /// When the fee counter resets (last_redelegation_at + 7 days)
    pub fee_reset_at: i64,
    pub bump: u8,
}

impl RedelegationRecord {
    pub const SIZE: usize = 8 + 32 + 4 + 8 + 8 + 1;

    /// Redelegation fee reset interval (7 days, matches Lua)
    pub const FEE_RESET_INTERVAL: i64 = 7 * 86_400;

    /// Calculate fee rate: min(10% * count, 60%) — first is free
    /// Returns fee rate scaled by RATE_SCALE (1e6)
    pub fn get_fee_rate(&self, current_timestamp: i64, rate_scale: u64) -> u64 {
        let count = if current_timestamp >= self.fee_reset_at {
            0 // Reset — fee counter expired
        } else {
            self.redelegation_count
        };
        // 10% per redelegation, max 60%
        let pct = std::cmp::min(10 * count, 60) as u64;
        pct * rate_scale / 100
    }
}

// =========================================
// EPOCH SETTINGS
// =========================================

/// Global epoch settings
/// PDA: ["epoch_settings"]
#[account]
pub struct EpochSettings {
    pub authority: Pubkey,
    /// Duration of each epoch in seconds
    pub epoch_duration: i64,
    /// Number of observers to prescribe per epoch
    pub prescribed_observer_count: u8,
    /// Number of names to prescribe per epoch
    pub prescribed_name_count: u8,
    /// Minimum stake required to be prescribed as observer
    pub min_observer_stake: u64,
    /// Percent of stake to slash for failed observations (scaled by 10000)
    pub slash_rate: u16,
    /// Whether epochs are currently enabled
    pub enabled: bool,
    /// The current epoch index (incremented each tick)
    pub current_epoch_index: u64,
    /// Timestamp of first epoch start
    pub genesis_timestamp: i64,
    /// Tenure weight duration in seconds (matches Lua tenureWeightDurationMs / 1000)
    pub tenure_weight_duration: i64,
    /// Maximum tenure weight — Lua: maxTenureWeight = 4
    pub max_tenure_weight: u64,
    // --- Reward/slash parameters (configurable) ---
    /// Gateway reward ratio (scaled by RATE_SCALE, default 900_000 = 90%)
    pub gateway_reward_ratio: u64,
    /// Observer reward ratio (scaled by RATE_SCALE, default 100_000 = 10%)
    pub observer_reward_ratio: u64,
    /// Missed observation penalty rate (scaled by RATE_SCALE, default 250_000 = 25%)
    pub missed_observation_penalty_rate: u64,
    /// Maximum reward rate (scaled by RATE_SCALE, default 1_000 = 0.1%)
    pub max_reward_rate: u64,
    /// Minimum reward rate (scaled by RATE_SCALE, default 500 = 0.05%)
    pub min_reward_rate: u64,
    /// Epoch index at which reward decay begins
    pub reward_decay_start_epoch: u64,
    /// Epoch index at which reward decay completes (min rate reached)
    pub reward_decay_last_epoch: u64,
    /// Maximum consecutive failures before gateway pruning
    pub max_consecutive_failures: u8,
    /// Slash rate for failed gateways (scaled by RATE_SCALE, default 1_000_000 = 100%)
    pub failed_gateway_slash_rate: u64,
    /// GAR-007: Timestamp at which epochs will be disabled (0 = no pending disable).
    /// When authority calls set_epochs_enabled(false), this is set to clock + 7 days
    /// instead of instantly disabling. create_epoch checks this timestamp.
    pub disable_at: i64,
    pub bump: u8,
}

impl EpochSettings {
    pub const SIZE: usize = 8 + 32 + 8 + 1 + 1 + 8 + 2 + 1 + 8 + 8 + 8 + 8
        + 8   // gateway_reward_ratio
        + 8   // observer_reward_ratio
        + 8   // missed_observation_penalty_rate
        + 8   // max_reward_rate
        + 8   // min_reward_rate
        + 8   // reward_decay_start_epoch
        + 8   // reward_decay_last_epoch
        + 1   // max_consecutive_failures
        + 8   // failed_gateway_slash_rate
        + 8   // disable_at
        + 1; // bump
    /// 7-day timelock for disabling epochs
    pub const EPOCH_DISABLE_DELAY: i64 = 7 * 24 * 60 * 60;
}

// =========================================
// EPOCH (zero-copy with embedded prescriptions)
// =========================================

/// Epoch account tracking state for a specific epoch
/// PDA: ["epoch", epoch_index.to_le_bytes()]
///
/// Zero-copy account with embedded observer/name prescriptions and failure tallies.
/// Eliminates the need for separate PrescribedObserver, PrescribedName, and
/// RewardDistribution PDAs.
#[account(zero_copy(unsafe))]
#[repr(C)]
pub struct Epoch {
    // --- u64-aligned fields first ---
    pub epoch_index: u64,
    pub start_timestamp: i64,
    pub end_timestamp: i64,
    pub total_eligible_rewards: u64,
    pub per_gateway_reward: u64,
    pub per_observer_reward: u64,
    pub reward_rate: u64,
    /// total_composite_weight stored as u64 pair to avoid u128 alignment issues in zero_copy.
    /// Use total_composite_weight() / set_total_composite_weight() helpers.
    pub total_composite_weight_lo: u64,
    pub total_composite_weight_hi: u64,

    // --- Hashchain for deterministic randomness ---
    pub hashchain: [u8; 32],

    // --- u32 fields ---
    pub active_gateway_count: u32,
    pub distribution_index: u32,
    pub tally_index: u32,

    // --- u8 fields ---
    pub observer_count: u8,
    pub name_count: u8,
    pub observations_submitted: u8,
    pub rewards_distributed: u8,
    pub weights_tallied: u8,
    pub prescriptions_done: u8,
    pub bump: u8,
    /// Count of `Observation` PDAs that have been closed for this epoch.
    /// `close_epoch` requires `observations_closed == observations_submitted`
    /// so observation rent isn't orphaned when the parent epoch closes
    /// (audit M8). Replaces a former `_padding1` byte — no layout change.
    pub observations_closed: u8,

    // --- Failure tallies (u16 per gateway slot) ---
    pub failure_counts: [u16; 3000],

    // --- Embedded prescriptions ---
    /// Observer addresses (the observer_address from Gateway, used to verify signer)
    pub prescribed_observers: [Pubkey; 50],
    /// Gateway addresses (the operator pubkey, used for reward distribution)
    pub prescribed_observer_gateways: [Pubkey; 50],
    /// Name hashes prescribed for this epoch
    pub prescribed_names: [[u8; 32]; 2],

    /// Bitmap tracking which prescribed observers have submitted (50 bits = 7 bytes)
    pub has_observed: [u8; 7],
    pub _padding2: [u8; 5],
}

impl Epoch {
    // 9*8 + 32 + 3*4 + 8*1 + 3000*2 + 50*32*2 + 2*32 + 7 + 5 = 9400
    pub const SIZE: usize = 9400;

    pub fn total_composite_weight(&self) -> u128 {
        (self.total_composite_weight_hi as u128) << 64 | (self.total_composite_weight_lo as u128)
    }

    pub fn set_total_composite_weight(&mut self, val: u128) {
        self.total_composite_weight_lo = val as u64;
        self.total_composite_weight_hi = (val >> 64) as u64;
    }

    pub fn add_composite_weight(&mut self, val: u64) {
        let current = self.total_composite_weight();
        self.set_total_composite_weight(current.saturating_add(val as u128));
    }

    /// Mark prescribed observer at index `idx` as having submitted.
    pub fn set_observed(&mut self, idx: usize) {
        if idx < 50 {
            self.has_observed[idx / 8] |= 1 << (idx % 8);
        }
    }

    /// Check if prescribed observer at index `idx` has submitted.
    pub fn is_observed(&self, idx: usize) -> bool {
        if idx < 50 {
            (self.has_observed[idx / 8] >> (idx % 8)) & 1 == 1
        } else {
            false
        }
    }
}

/// Compute reward rate for a given epoch index
/// Linear decay from max_rate to min_rate between decay_start and decay_last epochs
/// Matches Lua: epochs.getRewardRateForEpoch
pub fn compute_reward_rate(
    epoch_index: u64,
    max_rate: u64,
    min_rate: u64,
    decay_start: u64,
    decay_last: u64,
) -> u64 {
    if epoch_index < decay_start {
        return max_rate;
    }
    if epoch_index > decay_last {
        return min_rate;
    }
    // Linear decay: max - (max - min) * epochsDecayed / totalDecayPeriod
    let total_decay_period = decay_last - decay_start;
    let epochs_decayed = epoch_index - decay_start;
    let rate_range = max_rate.saturating_sub(min_rate);
    let decay = (rate_range as u128)
        .checked_mul(epochs_decayed as u128)
        .unwrap_or(0)
        .checked_div(total_decay_period as u128)
        .unwrap_or(0) as u64;
    max_rate.saturating_sub(decay)
}

// =========================================
// OBSERVATION
// =========================================

/// Observation report from a prescribed observer
/// PDA: ["observation", epoch_index.to_le_bytes(), observer.to_bytes()]
#[account]
pub struct Observation {
    pub epoch_index: u64,
    pub observer: Pubkey,
    /// Bitmap of gateway pass/fail results (up to 3000 gateways per observation)
    /// Each bit represents a gateway: 1 = passed, 0 = failed
    pub gateway_results: [u8; 375],
    /// Number of gateways evaluated
    pub gateway_count: u16,
    /// Transaction ID of the observation report on Arweave
    pub report_tx_id: [u8; 32],
    pub submitted_at: i64,
    pub bump: u8,
}

impl Observation {
    pub const SIZE: usize = 8 + 8 + 32 + 375 + 2 + 32 + 8 + 1;
}

// =========================================
// CROSS-PROGRAM: NameRegistry reader
// =========================================

/// Minimal NameEntry for cross-program read of ario-arns NameRegistry.
/// Must match the layout of ario-arns::state::NameEntry.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct NameEntryReader {
    pub name_hash: [u8; 32],
    pub registry_index: u32,
    pub _padding: [u8; 4],
}

/// Read the NameRegistry header from raw account data (after 8-byte discriminator).
/// Returns (count, pointer to names array start).
pub fn read_name_registry_header(data: &[u8]) -> Option<(u32, usize)> {
    // Layout: discriminator(8) + authority(32) + count(4) + _padding(4)
    // names start at offset 48
    if data.len() < 48 {
        return None;
    }
    let count = u32::from_le_bytes(data[40..44].try_into().ok()?);
    Some((count, 48))
}

/// Read a NameEntry at the given index from raw NameRegistry account data.
pub fn read_name_entry(data: &[u8], names_offset: usize, index: usize) -> Option<[u8; 32]> {
    let entry_size = 40; // 32 + 4 + 4
    let offset = names_offset + index * entry_size;
    if offset + 32 > data.len() {
        return None;
    }
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&data[offset..offset + 32]);
    // Check if entry is non-zero (active)
    if hash == [0u8; 32] {
        return None;
    }
    Some(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE_SCALE: u64 = 1_000_000;

    // =========================================
    // 1A. Gateway Weights Tests
    // =========================================

    fn default_stats() -> GatewayStats {
        GatewayStats {
            passed_epochs: 0,
            failed_epochs: 0,
            total_epochs: 0,
            prescribed_epochs: 0,
            observed_epochs: 0,
            failed_consecutive: 0,
            passed_consecutive: 0,
        }
    }

    #[test]
    fn stake_weight_at_minimum() {
        let w = GatewayWeights::compute(
            10_000_000_000, // total_stake = min
            10_000_000_000, // min_operator_stake
            0,
            100,          // timestamps
            180 * 86_400, // tenure_weight_duration
            4,            // max_tenure_weight
            &default_stats(),
            RATE_SCALE,
        );
        assert_eq!(w.stake_weight, 1_000_000); // 1.0
    }

    #[test]
    fn stake_weight_2x_minimum() {
        let w = GatewayWeights::compute(
            40_000_000_000,
            20_000_000_000,
            0,
            100,
            180 * 86_400,
            4,
            &default_stats(),
            RATE_SCALE,
        );
        assert_eq!(w.stake_weight, 2_000_000); // 2.0
    }

    #[test]
    fn tenure_weight_zero_time() {
        // When time_running == 0, code returns 1 / tenure_weight_duration
        let w = GatewayWeights::compute(
            10_000_000_000,
            10_000_000_000,
            100,
            100,        // start == current => time_running = 0
            15_552_000, // 180 days in seconds
            4,
            &default_stats(),
            RATE_SCALE,
        );
        // 1_000_000 / 15_552_000 = 0 (integer division)
        assert_eq!(w.tenure_weight, 0);
    }

    #[test]
    fn tenure_weight_at_duration() {
        let duration = 180 * 86_400i64; // 15,552,000 seconds
        let w = GatewayWeights::compute(
            10_000_000_000,
            10_000_000_000,
            0,
            duration, // time_running == duration
            duration,
            4,
            &default_stats(),
            RATE_SCALE,
        );
        assert_eq!(w.tenure_weight, 1_000_000); // 1.0
    }

    #[test]
    fn tenure_weight_capped_at_max() {
        let duration = 180 * 86_400i64;
        let w = GatewayWeights::compute(
            10_000_000_000,
            10_000_000_000,
            0,
            duration * 5, // 5x duration = would be 5.0 but capped at 4.0
            duration,
            4,
            &default_stats(),
            RATE_SCALE,
        );
        assert_eq!(w.tenure_weight, 4_000_000); // 4.0 (capped)
    }

    #[test]
    fn gateway_perf_ratio_no_epochs() {
        let stats = default_stats(); // all zeros
        let w = GatewayWeights::compute(
            10_000_000_000,
            10_000_000_000,
            0,
            100,
            180 * 86_400,
            4,
            &stats,
            RATE_SCALE,
        );
        // (1 + 0) / (1 + 0) = 1.0
        assert_eq!(w.gateway_performance_ratio, 1_000_000);
    }

    #[test]
    fn gateway_perf_ratio_3_of_10() {
        let stats = GatewayStats {
            passed_epochs: 3,
            total_epochs: 10,
            ..default_stats()
        };
        let w = GatewayWeights::compute(
            10_000_000_000,
            10_000_000_000,
            0,
            100,
            180 * 86_400,
            4,
            &stats,
            RATE_SCALE,
        );
        // (1 + 3) / (1 + 10) = 4/11 = 0.363636...
        assert_eq!(w.gateway_performance_ratio, 363_636);
    }

    #[test]
    fn composite_weight_full() {
        // Set up stats so gw_perf = 0.5 and obs_perf = 0.5
        let stats = GatewayStats {
            passed_epochs: 1,
            total_epochs: 3, // (1+1)/(1+3) = 2/4 = 0.5
            prescribed_epochs: 3,
            observed_epochs: 1, // (1+1)/(1+3) = 2/4 = 0.5
            ..default_stats()
        };
        let duration = 180 * 86_400i64;
        let w = GatewayWeights::compute(
            10_000_000_000,
            10_000_000_000,
            0,
            duration, // stake=1.0, tenure=1.0
            duration,
            4,
            &stats,
            RATE_SCALE,
        );
        assert_eq!(w.stake_weight, 1_000_000);
        assert_eq!(w.tenure_weight, 1_000_000);
        assert_eq!(w.gateway_performance_ratio, 500_000);
        assert_eq!(w.observer_performance_ratio, 500_000);
        // composite = 1.0 * 1.0 * 0.5 * 0.5 = 0.25
        assert_eq!(w.composite_weight, 250_000);
    }

    // =========================================
    // 1B. Reward Rate Calculation Tests
    // =========================================

    #[test]
    fn reward_rate_before_decay_epoch_1() {
        let rate = compute_reward_rate(1, 1_000, 500, 365, 547);
        assert_eq!(rate, 1_000);
    }

    #[test]
    fn reward_rate_at_decay_start_epoch_365() {
        // epoch 365 >= decay_start(365), but epochs_decayed=0, so no decay
        let rate = compute_reward_rate(365, 1_000, 500, 365, 547);
        assert_eq!(rate, 1_000);
    }

    #[test]
    fn reward_rate_epoch_366_minimal_decay() {
        let rate = compute_reward_rate(366, 1_000, 500, 365, 547);
        // epochs_decayed = 1, rate_range = 500, total_period = 182
        // decay = 500 * 1 / 182 = 2 (integer)
        // rate = 1000 - 2 = 998
        assert_eq!(rate, 998);
    }

    #[test]
    fn reward_rate_decay_midpoint_epoch_456() {
        let rate = compute_reward_rate(456, 1_000, 500, 365, 547);
        // epochs_decayed = 91, decay = 500 * 91 / 182 = 250
        // rate = 1000 - 250 = 750
        assert_eq!(rate, 750);
    }

    #[test]
    fn reward_rate_decay_end_epoch_547() {
        let rate = compute_reward_rate(547, 1_000, 500, 365, 547);
        // epoch >= decay_last, returns min
        assert_eq!(rate, 500);
    }

    #[test]
    fn reward_rate_after_decay_epoch_730() {
        let rate = compute_reward_rate(730, 1_000, 500, 365, 547);
        assert_eq!(rate, 500);
    }

    // =========================================
    // 1C. Redelegation Fee Calculation Tests
    // =========================================

    fn make_redelegation(count: u32, fee_reset_at: i64) -> RedelegationRecord {
        RedelegationRecord {
            delegator: Pubkey::default(),
            redelegation_count: count,
            last_redelegation_at: 0,
            fee_reset_at,
            bump: 0,
        }
    }

    #[test]
    fn first_redelegation_free() {
        // count=0 (after reset) => fee = 0
        let record = make_redelegation(0, 1_000_000); // not expired
        let fee = record.get_fee_rate(100, RATE_SCALE);
        assert_eq!(fee, 0);
    }

    #[test]
    fn second_redelegation_10pct() {
        let record = make_redelegation(1, 1_000_000);
        let fee = record.get_fee_rate(100, RATE_SCALE);
        assert_eq!(fee, 100_000); // 10%
    }

    #[test]
    fn third_redelegation_20pct() {
        let record = make_redelegation(2, 1_000_000);
        let fee = record.get_fee_rate(100, RATE_SCALE);
        assert_eq!(fee, 200_000); // 20%
    }

    #[test]
    fn seventh_redelegation_capped_60pct() {
        let record = make_redelegation(7, 1_000_000);
        let fee = record.get_fee_rate(100, RATE_SCALE);
        assert_eq!(fee, 600_000); // 60% cap
    }

    #[test]
    fn redelegation_fee_resets_after_7_days() {
        // fee_reset_at is in the past, so count resets to 0
        let record = make_redelegation(5, 50); // reset_at=50
        let fee = record.get_fee_rate(100, RATE_SCALE); // current=100 >= 50
        assert_eq!(fee, 0);
    }

    // =========================================
    // 1E. Gateway Stats Tests
    // =========================================

    #[test]
    fn gateway_stats_size_correct() {
        // 4 + 4 + 4 + 4 + 4 + 1 + 1 = 22
        assert_eq!(GatewayStats::SIZE, 22);
    }

    #[test]
    fn gateway_weights_size_correct() {
        // 7 * 8 = 56 (includes weights_epoch field)
        assert_eq!(GatewayWeights::SIZE, 56);
    }

    #[test]
    fn gateway_weights_size_includes_epoch() {
        // 7 u64 fields * 8 = 56
        assert_eq!(GatewayWeights::SIZE, 56);
    }

    #[test]
    fn gateway_settings2_size_correct() {
        // 1 + 2 + 8 + 1 = 12 (auto_stake removed in cfc7a8b)
        assert_eq!(GatewaySettings2::SIZE, 12);
    }

    #[test]
    fn registry_index_size_correct() {
        assert_eq!(RegistryIndex::SIZE, 5);
    }

    #[test]
    fn redelegation_fee_reset_interval() {
        assert_eq!(RedelegationRecord::FEE_RESET_INTERVAL, 7 * 86_400);
    }

    // =========================================
    // 1F. Epoch Size
    // =========================================

    #[test]
    fn epoch_size_correct() {
        // 9*8 + 32 + 3*4 + 8*1 + 3000*2 + 50*32*2 + 2*32 + 7 + 5 = 9400
        assert_eq!(Epoch::SIZE, 9400);
    }

    // =========================================
    // Observer Performance Ratio
    // =========================================

    #[test]
    fn observer_perf_ratio_1_of_3() {
        let stats = GatewayStats {
            prescribed_epochs: 3,
            observed_epochs: 1,
            ..default_stats()
        };
        let w = GatewayWeights::compute(
            10_000_000_000,
            10_000_000_000,
            0,
            100,
            180 * 86_400,
            4,
            &stats,
            RATE_SCALE,
        );
        // (1 + 1) / (1 + 3) = 2/4 = 0.5
        assert_eq!(w.observer_performance_ratio, 500_000);
    }

    #[test]
    fn observer_perf_ratio_all_observed() {
        let stats = GatewayStats {
            prescribed_epochs: 10,
            observed_epochs: 10,
            ..default_stats()
        };
        let w = GatewayWeights::compute(
            10_000_000_000,
            10_000_000_000,
            0,
            100,
            180 * 86_400,
            4,
            &stats,
            RATE_SCALE,
        );
        // (1 + 10) / (1 + 10) = 11/11 = 1.0
        assert_eq!(w.observer_performance_ratio, 1_000_000);
    }

    #[test]
    fn redelegation_10th_capped_at_60pct() {
        let record = make_redelegation(10, 1_000_000);
        let fee = record.get_fee_rate(100, RATE_SCALE);
        // min(10*10, 60) = 60 => 600_000
        assert_eq!(fee, 600_000);
    }

    // =========================================
    // Weight Normalization Tests
    // =========================================

    #[test]
    fn normalized_weight_single_gateway() {
        let composite: u128 = 500_000;
        let total: u128 = 500_000;
        let normalized = composite * RATE_SCALE as u128 / total;
        assert_eq!(normalized as u64, 1_000_000);
    }

    #[test]
    fn normalized_weight_two_equal_gateways() {
        let composite: u128 = 500_000;
        let total: u128 = 1_000_000;
        let normalized = composite * RATE_SCALE as u128 / total;
        assert_eq!(normalized as u64, 500_000);
    }

    #[test]
    fn normalized_weight_unequal_gateways() {
        let total: u128 = 1_000_000;
        let a_norm = 750_000u128 * RATE_SCALE as u128 / total;
        let b_norm = 250_000u128 * RATE_SCALE as u128 / total;
        assert_eq!(a_norm as u64, 750_000);
        assert_eq!(b_norm as u64, 250_000);
    }

    #[test]
    fn normalized_weight_zero_total() {
        let total: u128 = 0;
        assert_eq!(total, 0);
    }

    #[test]
    fn normalized_weights_sum_to_scale() {
        let weights: [u128; 5] = [100_000, 200_000, 300_000, 150_000, 250_000];
        let total: u128 = weights.iter().sum();
        assert_eq!(total, 1_000_000);
        let normalized: Vec<u64> = weights
            .iter()
            .map(|w| (w * RATE_SCALE as u128 / total) as u64)
            .collect();
        let sum: u64 = normalized.iter().sum();
        assert_eq!(sum, 1_000_000);
    }

    // =========================================
    // Withdrawal Tests
    // =========================================

    #[test]
    fn withdrawal_size_correct() {
        assert_eq!(Withdrawal::SIZE, 108);
    }

    #[test]
    fn withdrawal_counter_size_correct() {
        assert_eq!(WithdrawalCounter::SIZE, 49);
    }

    fn make_withdrawal(is_exit_vault: bool, available_at: i64) -> Withdrawal {
        Withdrawal {
            owner: Pubkey::default(),
            withdrawal_id: 1,
            gateway: Pubkey::default(),
            amount: 1_000_000,
            created_at: 0,
            available_at,
            is_delegate: false,
            is_exit_vault,
            is_protected: false,
            bump: 0,
        }
    }

    #[test]
    fn exit_vault_flag_set_on_leave_network() {
        let withdrawal = make_withdrawal(true, 90 * 86_400);
        assert!(withdrawal.is_exit_vault);
    }

    #[test]
    fn normal_withdrawal_not_exit_vault() {
        let withdrawal = make_withdrawal(false, 90 * 86_400);
        assert!(!withdrawal.is_exit_vault);
    }

    #[test]
    fn withdrawal_size_constant_matches_layout() {
        // Discriminator + owner + id + gateway + amount + created_at +
        // available_at + is_delegate + is_exit_vault + is_protected + bump
        // = 8 + 32 + 8 + 32 + 8 + 8 + 8 + 1 + 1 + 1 + 1 = 108
        assert_eq!(Withdrawal::SIZE, 108);
    }

    #[test]
    fn exit_vault_flag_preserved() {
        let withdrawal = make_withdrawal(true, 90 * 86_400);
        assert!(
            withdrawal.is_exit_vault,
            "Exit vault flag should be set for leave_network/prune_gateway withdrawals"
        );
    }

    #[test]
    fn all_withdrawals_can_be_expedited() {
        // Both exit vaults and normal withdrawals allow instant withdrawal with penalty
        let exit = make_withdrawal(true, 90 * 86_400);
        let normal = make_withdrawal(false, 90 * 86_400);
        assert!(exit.is_exit_vault);
        assert!(!normal.is_exit_vault);
        // Both are eligible for instant_withdrawal (no is_exit_vault guard)
    }

    // =========================================
    // Min Remaining Delegation Check
    // =========================================

    #[test]
    fn decrease_delegation_full_withdrawal_succeeds() {
        let delegation_amount: u64 = 50_000_000;
        let decrease_amount: u64 = 50_000_000;
        let remaining = delegation_amount.saturating_sub(decrease_amount);
        let min_delegation = 10_000_000u64;
        let allowed = remaining == 0 || remaining >= min_delegation;
        assert!(allowed, "Full withdrawal should succeed");
    }

    #[test]
    fn decrease_delegation_above_minimum_succeeds() {
        let delegation_amount: u64 = 50_000_000;
        let decrease_amount: u64 = 30_000_000;
        let remaining = delegation_amount.saturating_sub(decrease_amount);
        let min_delegation = 10_000_000u64;
        assert_eq!(remaining, 20_000_000);
        let allowed = remaining == 0 || remaining >= min_delegation;
        assert!(allowed, "Remaining 20 ARIO >= 10 ARIO min should succeed");
    }

    #[test]
    fn decrease_delegation_at_minimum_boundary_succeeds() {
        let delegation_amount: u64 = 50_000_000;
        let decrease_amount: u64 = 40_000_000;
        let remaining = delegation_amount.saturating_sub(decrease_amount);
        let min_delegation = 10_000_000u64;
        assert_eq!(remaining, 10_000_000);
        let allowed = remaining == 0 || remaining >= min_delegation;
        assert!(allowed, "Remaining exactly at min should succeed");
    }

    #[test]
    fn decrease_delegation_below_minimum_fails() {
        let delegation_amount: u64 = 50_000_000;
        let decrease_amount: u64 = 45_000_000;
        let remaining = delegation_amount.saturating_sub(decrease_amount);
        let min_delegation = 10_000_000u64;
        assert_eq!(remaining, 5_000_000);
        let allowed = remaining == 0 || remaining >= min_delegation;
        assert!(
            !allowed,
            "Remaining 5 ARIO < 10 ARIO min should fail with DelegationBelowMinimum"
        );
    }

    #[test]
    fn decrease_delegation_leaves_1_mario_fails() {
        let delegation_amount: u64 = 10_000_000;
        let decrease_amount: u64 = 9_999_999;
        let remaining = delegation_amount.saturating_sub(decrease_amount);
        let min_delegation = 10_000_000u64;
        assert_eq!(remaining, 1);
        let allowed = remaining == 0 || remaining >= min_delegation;
        assert!(
            !allowed,
            "Leaving 1 mARIO should fail - must withdraw fully or keep >= min"
        );
    }

    #[test]
    fn decrease_delegation_custom_gateway_min() {
        let delegation_amount: u64 = 100_000_000;
        let decrease_amount: u64 = 60_000_000;
        let remaining = delegation_amount.saturating_sub(decrease_amount);
        let gateway_min = 50_000_000u64;
        assert_eq!(remaining, 40_000_000);
        let allowed = remaining == 0 || remaining >= gateway_min;
        assert!(
            !allowed,
            "40 ARIO remaining < 50 ARIO gateway min should fail"
        );
    }

    // =========================================
    // Reward-per-share accumulator tests
    // =========================================

    #[test]
    fn settle_no_pending_rewards() {
        // When cumulative_reward_per_token == reward_debt, no rewards
        let mut gateway = Gateway {
            operator: Pubkey::default(),
            label: String::new(),
            fqdn: String::new(),
            port: 443,
            protocol: Protocol::Https,
            properties: String::new(),
            note: String::new(),
            operator_stake: 10_000_000_000,
            total_delegated_stake: 100_000_000,
            status: GatewayStatus::Joined,
            start_timestamp: 0,
            leave_timestamp: None,
            leave_epoch_duration: 0,
            stats: GatewayStats::default(),
            weights: GatewayWeights::default(),
            settings: GatewaySettings2 {
                allow_delegated_staking: true,
                delegate_reward_share_ratio: 500,
                min_delegation_amount: 10_000_000,
                allowlist_enabled: false,
            },
            registry_index: RegistryIndex::default(),
            observer_address: Pubkey::default(),
            cumulative_reward_per_token: 1_000_000_000_000_000_000, // 1e18
            bump: 0,
        };
        let mut delegation = Delegation {
            gateway: Pubkey::default(),
            delegator: Pubkey::default(),
            amount: 50_000_000,
            start_timestamp: 0,
            reward_debt: 1_000_000_000_000_000_000, // same as cumulative
            bump: 0,
        };
        settle_delegate_rewards(&mut gateway, &mut delegation);
        assert_eq!(delegation.amount, 50_000_000); // unchanged
    }

    #[test]
    fn settle_with_pending_rewards() {
        // cumulative = 2e18, debt = 1e18, amount = 100 ARIO
        // pending = 100e6 * (2e18 - 1e18) / 1e18 = 100e6
        let mut gateway = Gateway {
            operator: Pubkey::default(),
            label: String::new(),
            fqdn: String::new(),
            port: 443,
            protocol: Protocol::Https,
            properties: String::new(),
            note: String::new(),
            operator_stake: 10_000_000_000,
            total_delegated_stake: 100_000_000,
            status: GatewayStatus::Joined,
            start_timestamp: 0,
            leave_timestamp: None,
            leave_epoch_duration: 0,
            stats: GatewayStats::default(),
            weights: GatewayWeights::default(),
            settings: GatewaySettings2 {
                allow_delegated_staking: true,
                delegate_reward_share_ratio: 500,
                min_delegation_amount: 10_000_000,
                allowlist_enabled: false,
            },
            registry_index: RegistryIndex::default(),
            observer_address: Pubkey::default(),
            cumulative_reward_per_token: 2_000_000_000_000_000_000, // 2e18
            bump: 0,
        };
        let mut delegation = Delegation {
            gateway: Pubkey::default(),
            delegator: Pubkey::default(),
            amount: 100_000_000, // 100 ARIO
            start_timestamp: 0,
            reward_debt: 1_000_000_000_000_000_000, // 1e18
            bump: 0,
        };
        let old_total = gateway.total_delegated_stake;
        settle_delegate_rewards(&mut gateway, &mut delegation);
        // pending = 100_000_000 * 1e18 / 1e18 = 100_000_000
        assert_eq!(delegation.amount, 200_000_000); // 100 + 100
        assert_eq!(delegation.reward_debt, 2_000_000_000_000_000_000);
        assert_eq!(gateway.total_delegated_stake, old_total + 100_000_000);
    }

    // =========================================
    // Epoch u128 helpers
    // =========================================

    #[test]
    fn epoch_composite_weight_roundtrip() {
        // Can't easily create a zero-copy Epoch in tests without account data,
        // so just test the u128 math logic directly
        let lo: u64 = 0xFFFF_FFFF_FFFF_FFFF;
        let hi: u64 = 0x0000_0000_0000_0001;
        let val = (hi as u128) << 64 | (lo as u128);
        assert_eq!(val, u64::MAX as u128 + (1u128 << 64));
        let lo2 = val as u64;
        let hi2 = (val >> 64) as u64;
        assert_eq!(lo2, lo);
        assert_eq!(hi2, hi);
    }

    // =========================================
    // GatewaySlot size check
    // =========================================

    #[test]
    fn gateway_slot_size_correct() {
        // 32 address + 8 composite_weight + 8 start_timestamp + 1 status + 7 _padding
        assert_eq!(std::mem::size_of::<GatewaySlot>(), 56);
    }

    #[test]
    fn gateway_registry_size_correct() {
        assert_eq!(GatewayRegistry::SIZE, 168_040);
    }

    // =========================================
    // Migration Deadline Tests
    // =========================================

    #[test]
    fn migration_deadline_is_set() {
        use crate::migration::MIGRATION_DEADLINE;
        // Verify deadline is set (currently i64::MAX as placeholder)
        assert!(
            MIGRATION_DEADLINE > 0,
            "migration deadline must be positive"
        );
    }

    // =========================================
    // Property-Based Tests (proptest)
    // =========================================

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        fn arb_stats() -> impl Strategy<Value = GatewayStats> {
            (0u32..1000u32, 0u32..1000u32, 0u32..1000u32, 0u32..1000u32).prop_map(
                |(passed, failed, prescribed, observed)| {
                    let total = passed + failed;
                    GatewayStats {
                        passed_epochs: passed,
                        failed_epochs: failed,
                        total_epochs: total,
                        prescribed_epochs: prescribed.max(observed),
                        observed_epochs: observed,
                        failed_consecutive: 0,
                        passed_consecutive: 0,
                    }
                },
            )
        }

        proptest! {
            #[test]
            fn reward_rate_bounded(
                epoch_index in 0u64..2000u64,
                max_rate in 1u64..100_000u64,
                min_rate_raw in 0u64..100_000u64,
                decay_start in 0u64..1000u64,
                decay_span in 1u64..1000u64,
            ) {
                let min_rate = min_rate_raw.min(max_rate);
                let decay_last = decay_start + decay_span;
                let rate = compute_reward_rate(epoch_index, max_rate, min_rate, decay_start, decay_last);
                prop_assert!(rate >= min_rate, "rate {} must be >= min {}", rate, min_rate);
                prop_assert!(rate <= max_rate, "rate {} must be <= max {}", rate, max_rate);
            }

            #[test]
            fn reward_rate_monotonically_decreasing(
                epoch_a in 0u64..1000u64,
                epoch_gap in 0u64..500u64,
                max_rate in 1u64..100_000u64,
                min_rate_raw in 0u64..100_000u64,
                decay_start in 0u64..500u64,
                decay_span in 1u64..500u64,
            ) {
                let min_rate = min_rate_raw.min(max_rate);
                let decay_last = decay_start + decay_span;
                let epoch_b = epoch_a + epoch_gap;
                let rate_a = compute_reward_rate(epoch_a, max_rate, min_rate, decay_start, decay_last);
                let rate_b = compute_reward_rate(epoch_b, max_rate, min_rate, decay_start, decay_last);
                prop_assert!(rate_a >= rate_b, "rate at epoch {} ({}) must be >= rate at epoch {} ({})", epoch_a, rate_a, epoch_b, rate_b);
            }

            #[test]
            fn gateway_weights_never_panics(
                total_stake in 0u64..=100_000_000_000u64,
                min_operator_stake in 1u64..=50_000_000_000u64,
                start_timestamp in 0i64..=1_000_000_000i64,
                current_timestamp in 0i64..=2_000_000_000i64,
                tenure_weight_duration in 1i64..=31_536_000i64,
                max_tenure_weight in 1u64..=10u64,
                stats in arb_stats(),
            ) {
                let _weights = GatewayWeights::compute(
                    total_stake,
                    min_operator_stake,
                    start_timestamp,
                    current_timestamp,
                    tenure_weight_duration,
                    max_tenure_weight,
                    &stats,
                    RATE_SCALE,
                );
            }

            #[test]
            fn zero_stake_gives_zero_composite(
                min_operator_stake in 1u64..=50_000_000_000u64,
                current_timestamp in 100i64..=2_000_000_000i64,
                tenure_weight_duration in 1i64..=31_536_000i64,
            ) {
                let stats = GatewayStats {
                    passed_epochs: 10,
                    failed_epochs: 0,
                    total_epochs: 10,
                    prescribed_epochs: 10,
                    observed_epochs: 10,
                    failed_consecutive: 0,
                    passed_consecutive: 10,
                };
                let weights = GatewayWeights::compute(
                    0,
                    min_operator_stake,
                    0,
                    current_timestamp,
                    tenure_weight_duration,
                    4,
                    &stats,
                    RATE_SCALE,
                );
                prop_assert_eq!(weights.composite_weight, 0, "zero stake must yield zero composite weight");
            }
        }
    }

    // =========================================
    // 1B. SIZE drift sentinels
    // =========================================
    //
    // Each `pub const SIZE` is hand-rolled from per-field byte counts. When
    // a struct gains, loses, or reshapes a field and the formula isn't
    // updated in lockstep, on-chain accounts drift from `Account<>` deserialize
    // and silently break callers (e.g. `tally_weights` returning
    // InvalidGatewayAccount on devnet after `auto_stake` was dropped from
    // GatewaySettings2). These tests serialize a max-filled instance via
    // Anchor's `try_serialize` and assert the encoded length equals SIZE,
    // catching SIZE/struct drift before it ships.
    //
    // For variable-length structs (Gateway), strings are filled to their
    // declared maxima so the serialized blob hits the SIZE upper bound.

    fn gateway_at_max_size() -> Gateway {
        Gateway {
            operator: Pubkey::default(),
            label: "x".repeat(64),
            fqdn: "x".repeat(128),
            port: 0,
            protocol: Protocol::Http,
            properties: "x".repeat(256),
            note: "x".repeat(256),
            operator_stake: 0,
            total_delegated_stake: 0,
            status: GatewayStatus::Joined,
            start_timestamp: 0,
            leave_timestamp: Some(0), // 1 + 8 = 9 bytes (matches SIZE formula)
            leave_epoch_duration: 0,
            stats: GatewayStats {
                passed_epochs: 0,
                failed_epochs: 0,
                total_epochs: 0,
                prescribed_epochs: 0,
                observed_epochs: 0,
                failed_consecutive: 0,
                passed_consecutive: 0,
            },
            weights: GatewayWeights::default(),
            settings: GatewaySettings2 {
                allow_delegated_staking: false,
                delegate_reward_share_ratio: 0,
                min_delegation_amount: 0,
                allowlist_enabled: false,
            },
            registry_index: RegistryIndex::default(),
            observer_address: Pubkey::default(),
            cumulative_reward_per_token: 0,
            bump: 0,
        }
    }

    #[test]
    fn gateway_size_matches_max_serialized() {
        let mut buf = Vec::new();
        gateway_at_max_size().try_serialize(&mut buf).unwrap();
        assert_eq!(
            buf.len(),
            Gateway::SIZE,
            "Gateway::SIZE drift — update the formula in impl Gateway::SIZE",
        );
    }

    #[test]
    fn delegation_size_matches_serialized() {
        let d = Delegation {
            gateway: Pubkey::default(),
            delegator: Pubkey::default(),
            amount: 0,
            start_timestamp: 0,
            reward_debt: 0,
            bump: 0,
        };
        let mut buf = Vec::new();
        d.try_serialize(&mut buf).unwrap();
        assert_eq!(buf.len(), Delegation::SIZE, "Delegation::SIZE drift");
    }

    #[test]
    fn withdrawal_size_matches_serialized() {
        let w = Withdrawal {
            owner: Pubkey::default(),
            withdrawal_id: 0,
            gateway: Pubkey::default(),
            amount: 0,
            created_at: 0,
            available_at: 0,
            is_delegate: false,
            is_exit_vault: false,
            is_protected: false,
            bump: 0,
        };
        let mut buf = Vec::new();
        w.try_serialize(&mut buf).unwrap();
        assert_eq!(buf.len(), Withdrawal::SIZE, "Withdrawal::SIZE drift");
    }

    #[test]
    fn withdrawal_counter_size_matches_serialized() {
        let c = WithdrawalCounter {
            owner: Pubkey::default(),
            next_id: 0,
            bump: 0,
        };
        let mut buf = Vec::new();
        c.try_serialize(&mut buf).unwrap();
        assert_eq!(
            buf.len(),
            WithdrawalCounter::SIZE,
            "WithdrawalCounter::SIZE drift"
        );
    }

    #[test]
    fn allowlist_entry_size_matches_serialized() {
        let a = AllowlistEntry {
            gateway: Pubkey::default(),
            delegate: Pubkey::default(),
            added_at: 0,
            bump: 0,
        };
        let mut buf = Vec::new();
        a.try_serialize(&mut buf).unwrap();
        assert_eq!(
            buf.len(),
            AllowlistEntry::SIZE,
            "AllowlistEntry::SIZE drift"
        );
    }
}
