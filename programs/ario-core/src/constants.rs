// AR.IO Protocol Constants
// Shared constants across all programs

use anchor_lang::prelude::*;

// =========================================
// SCALING FACTORS
// =========================================

/// Base scaling for all rate calculations (1e6 = 1.0)
pub const RATE_SCALE: u64 = 1_000_000;

/// Precision for reward-per-share accumulator (1e18)
/// Used in delegate reward distribution to avoid rounding to zero
pub const REWARD_PRECISION: u128 = 1_000_000_000_000_000_000;

/// Token decimals (matches original AO token)
pub const TOKEN_DECIMALS: u8 = 6;

/// One token in base units
pub const ONE_TOKEN: u64 = 1_000_000;

// =========================================
// TIME CONSTANTS
// =========================================

/// Seconds per day
pub const SECONDS_PER_DAY: i64 = 86_400;

/// Seconds per year (365 days)
pub const SECONDS_PER_YEAR: i64 = 31_536_000;

/// Epoch duration in seconds (matches original: ~24 hours)
pub const DEFAULT_EPOCH_DURATION: u64 = 86_400;

/// Withdrawal lock period in seconds (30 days)
pub const WITHDRAWAL_LOCK_PERIOD: i64 = 30 * SECONDS_PER_DAY;

/// Grace period for expired leases (14 days)
pub const LEASE_GRACE_PERIOD: i64 = 14 * SECONDS_PER_DAY;

/// Returned name auction duration (14 days)
pub const RETURN_AUCTION_DURATION: i64 = 14 * SECONDS_PER_DAY;

/// Primary name request expiry (7 days)
pub const PRIMARY_NAME_REQUEST_EXPIRY: i64 = 7 * SECONDS_PER_DAY;

// =========================================
// STAKING CONSTANTS
// =========================================

/// Minimum operator stake (20,000 ARIO)
pub const MIN_OPERATOR_STAKE: u64 = 20_000 * ONE_TOKEN;

/// Minimum delegation amount (10 ARIO — matches Lua delegates.minStake)
pub const MIN_DELEGATION_AMOUNT: u64 = 10 * ONE_TOKEN;

/// Maximum delegate reward share ratio (95%)
pub const MAX_DELEGATE_REWARD_SHARE: u16 = 9500;

/// Maximum expedited withdrawal penalty rate — Lua: 0.50 (50%)
pub const MAX_EXPEDITED_WITHDRAWAL_PENALTY: u64 = 500_000; // 50%

/// Minimum expedited withdrawal penalty rate — Lua: 0.10 (10%)
pub const MIN_EXPEDITED_WITHDRAWAL_PENALTY: u64 = 100_000; // 10%

/// Minimum expedited withdrawal amount — Lua: 1 ARIO
pub const MIN_EXPEDITED_WITHDRAWAL_AMOUNT: u64 = ONE_TOKEN;

/// Gateway leave period in seconds (90 days — matches Lua leaveLengthMs)
pub const GATEWAY_LEAVE_PERIOD: i64 = 90 * SECONDS_PER_DAY;

/// Tenure weight duration (180 days — matches Lua tenureWeightDurationMs)
pub const TENURE_WEIGHT_DURATION: i64 = 180 * SECONDS_PER_DAY;

/// Maximum tenure weight — Lua: maxTenureWeight = 4
pub const MAX_TENURE_WEIGHT: u64 = 4;

/// Redelegation fee reset interval (7 days — matches Lua)
pub const REDELEGATION_FEE_RESET_INTERVAL: i64 = 7 * SECONDS_PER_DAY;

/// Minimum redelegation penalty rate — Lua: 0.10 (10%)
pub const MIN_REDELEGATION_PENALTY: u64 = 100_000;

/// Maximum redelegation penalty rate — Lua: 0.60 (60%)
pub const MAX_REDELEGATION_PENALTY: u64 = 600_000;

// =========================================
// EPOCH CONSTANTS
// =========================================

/// Maximum prescribed observers per epoch
pub const MAX_OBSERVERS_PER_EPOCH: u8 = 50;

/// Default prescribed name count per epoch (matches Lua prescribedNameCount = 2)
pub const DEFAULT_PRESCRIBED_NAME_COUNT: u32 = 2;

/// Observation submission window (relative to epoch end)
pub const OBSERVATION_WINDOW_SECONDS: i64 = 3_600; // 1 hour

/// Maximum failed gateways per observation report
pub const MAX_FAILED_GATEWAYS_PER_OBSERVATION: usize = 100;

/// Consecutive failures before gateway pruning
pub const MAX_CONSECUTIVE_FAILURES: u8 = 30;

// =========================================
// ARNS CONSTANTS
// =========================================

/// Maximum base name length (matches Lua MAX_BASE_NAME_LENGTH = 51)
pub const MAX_NAME_LENGTH: usize = 51;

/// Minimum name length
pub const MIN_NAME_LENGTH: usize = 1;

/// Default undername limit
pub const DEFAULT_UNDERNAME_LIMIT: u16 = 10;

/// Maximum lease years
pub const MAX_LEASE_YEARS: u8 = 5;

/// Permabuy multiplier (scaled by 100, e.g., 2000 = 20x annual)
pub const PERMABUY_MULTIPLIER: u16 = 2000;

/// Name length fee multipliers (1-char to 5-char)
/// Scaled by 100 (e.g., 10000 = 100x base fee)
pub const NAME_LENGTH_MULTIPLIERS: [u16; 6] = [
    10000, // 1 char: 100x
    5000,  // 2 chars: 50x
    1000,  // 3 chars: 10x
    500,   // 4 chars: 5x
    100,   // 5+ chars: 1x
    100,   // default: 1x
];

// =========================================
// ANT CONSTANTS
// =========================================

/// Arweave transaction ID length
pub const ARWEAVE_TX_ID_LENGTH: usize = 43;

/// Maximum controllers per ANT. Synced to `ario_ant::state::MAX_CONTROLLERS`
/// (dropped 10 → 4 on 2026-05-21 for mainnet rent shrink). Keep these two
/// constants identical — `ario-core` exposes this to non-ant callers that
/// need the protocol cap without depending on ario-ant.
pub const MAX_CONTROLLERS_PER_ANT: u8 = 4;

/// Default TTL for records (in seconds)
pub const DEFAULT_RECORD_TTL: u32 = 3600; // 1 hour

/// Maximum TTL for records (in seconds)
pub const MAX_RECORD_TTL: u32 = 86400 * 30; // 30 days

// =========================================
// DISTRIBUTION CONSTANTS
// =========================================

/// Batch size for reward distribution
/// Optimized for CU limits (~10-20 gateways per tx)
pub const DISTRIBUTION_BATCH_SIZE: u8 = 15;

/// Maximum reward rate (scaled by RATE_SCALE) — Lua: 0.001 (0.1%)
pub const MAX_REWARD_RATE: u64 = 1_000; // 0.1%

/// Minimum reward rate (scaled by RATE_SCALE) — Lua: 0.0005 (0.05%)
pub const MIN_REWARD_RATE: u64 = 500; // 0.05%

/// Gateway operator base reward rate (scaled by RATE_SCALE) — Lua: 0.9 (90%)
pub const GATEWAY_OPERATOR_REWARD_RATE: u64 = 900_000; // 90%

/// Observer reward rate (scaled by RATE_SCALE) — Lua: 0.1 (10%)
pub const OBSERVER_REWARD_RATE: u64 = 100_000; // 10%

/// Missed observation penalty rate (scaled by RATE_SCALE) — Lua: 0.25 (25%)
pub const MISSED_OBSERVATION_PENALTY: u64 = 250_000; // 25%

/// Reward decay start epoch — Lua: rewardDecayStartEpoch = 365
pub const REWARD_DECAY_START_EPOCH: u64 = 365;

/// Reward decay last epoch — Lua: rewardDecayLastEpoch = 547
pub const REWARD_DECAY_LAST_EPOCH: u64 = 547;

/// Failed gateway slash rate (scaled by RATE_SCALE) — Lua: 1.0 (100%)
pub const FAILED_GATEWAY_SLASH_RATE: u64 = 1_000_000; // 100%
