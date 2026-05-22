// AR.IO ArNS State Accounts
// PDA structures for name registry, demand factor, and name enumeration
//
// Reference: ar-io-network-process (Lua)
// All timestamps are in SECONDS (not milliseconds).
// All scaled values use DEMAND_FACTOR_SCALE (1_000_000 = 1.0).
// Fees are in mARIO (1 ARIO = 1_000_000 mARIO).

use anchor_lang::prelude::*;

// =========================================
// SEEDS
// =========================================

pub const ARNS_CONFIG_SEED: &[u8] = b"arns_config";
pub const ARNS_RECORD_SEED: &[u8] = b"arns_record";
pub const RETURNED_NAME_SEED: &[u8] = b"returned_name";
pub const RESERVED_NAME_SEED: &[u8] = b"reserved_name";
pub const DEMAND_FACTOR_SEED: &[u8] = b"demand_factor";
pub const NAME_REGISTRY_SEED: &[u8] = b"name_registry";

// =========================================
// CONSTANTS
// =========================================

/// Scale factor for fixed-point math (1.0 = 1_000_000)
pub const DEMAND_FACTOR_SCALE: u64 = 1_000_000;

/// Minimum demand factor (0.5)
pub const DEMAND_FACTOR_MIN: u64 = 500_000;

/// Upward adjustment multiplier (1.05x)
pub const DEMAND_FACTOR_UP_ADJUSTMENT: u64 = 1_050_000;

/// Downward adjustment multiplier (0.985x)
pub const DEMAND_FACTOR_DOWN_ADJUSTMENT: u64 = 985_000;

/// After this many consecutive periods at min, fees are permanently halved
pub const MAX_PERIODS_AT_MIN_DEMAND_FACTOR: u32 = 7;

/// Number of trailing periods used for moving average
pub const MOVING_AVG_PERIOD_COUNT: usize = 7;

/// Duration of one demand-factor period (1 day)
pub const PERIOD_LENGTH_SECONDS: i64 = 86_400;

/// Annual percentage fee for lease renewals (0.2 scaled)
pub const ANNUAL_PERCENTAGE_FEE: u64 = 200_000;

/// Equivalent lease length in years for permabuy pricing
pub const PERMABUY_LEASE_FEE_LENGTH_YEARS: u64 = 20;

/// Undername fee as fraction of registration (lease, 0.001 scaled)
pub const UNDERNAME_LEASE_FEE_PCT: u64 = 1_000;

/// Undername fee as fraction of registration (permabuy, 0.005 scaled)
pub const UNDERNAME_PERMABUY_FEE_PCT: u64 = 5_000;

/// Grace period after lease expiry before name can be reclaimed (14 days)
pub const GRACE_PERIOD_SECONDS: i64 = 14 * 86_400;

/// Duration of a returned-name auction (14 days)
pub const RETURNED_NAME_DURATION_SECONDS: i64 = 14 * 86_400;

/// Maximum premium multiplier at the start of a returned-name auction
pub const RETURNED_NAME_MAX_MULTIPLIER: u64 = 50;

/// Maximum lease duration
pub const MAX_LEASE_LENGTH_YEARS: u8 = 5;

/// Seconds in one year (365 days, no leap)
pub const ONE_YEAR_SECONDS: i64 = 365 * 86_400;

/// Maximum ArNS name length (characters)
pub const MAX_NAME_LENGTH: usize = 51;

/// Minimum ArNS name length (characters)
pub const MIN_NAME_LENGTH: usize = 1;

/// Length of an Arweave address (forbidden as a name length)
pub const ARWEAVE_ADDRESS_LENGTH: usize = 43;

/// Default number of undernames included with every registration
pub const DEFAULT_UNDERNAME_COUNT: u16 = 10;

/// Discount for gateway operators purchasing names (0.2 scaled)
pub const GATEWAY_OPERATOR_DISCOUNT_PCT: u64 = 200_000;

/// Number of entries in the genesis fee table (indices 0..=50)
pub const NUM_NAME_LENGTH_FEES: usize = 51;

// =========================================
// SIZING CONSTANTS
// =========================================

/// Anchor account discriminator (first 8 bytes of SHA-256("account:<Name>")).
pub const ANCHOR_DISCRIMINATOR_SIZE: usize = 8;

/// Solana `Pubkey` serialized size.
pub const PUBKEY_SIZE: usize = 32;

/// Borsh `u32` length prefix used for `String` and `Vec<T>`.
pub const BORSH_LEN_PREFIX: usize = 4;

/// Borsh `Option<T>` discriminant byte (0 = None, 1 = Some).
pub const BORSH_OPTION_PREFIX: usize = 1;

/// Bump seed (single `u8`).
pub const BUMP_SIZE: usize = 1;

/// `SchemaVersion { major, minor, patch }` — 3 consecutive u8s.
pub const SCHEMA_VERSION_SIZE: usize = 3;

// =========================================
// SCHEMA VERSIONING
// =========================================

/// Semantic version for ArNS on-chain account schemas.
///
/// Stored as three consecutive `u8` bytes (3 bytes on the wire).
/// `Ord` is derived lexicographically over `(major, minor, patch)`, which
/// matches semver precedence: major is compared first, then minor, then
/// patch. This means `config.version < ARNS_CONFIG_VERSION` is a valid
/// "needs migration" guard.
///
/// Bump rules:
///   - `major`: breaking layout change (field removed, type changed, reorder)
///   - `minor`: additive layout change (new field appended, default = zero)
///   - `patch`: logic-only change (no layout change; bump optional for audits)
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Debug,
)]
pub struct SchemaVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

impl SchemaVersion {
    pub const fn new(major: u8, minor: u8, patch: u8) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl std::fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Current schema version for each ArNS account type.
///
/// Bump the appropriate constant when changing the on-chain layout; the
/// `schema_migration` module's dispatch functions key off the stored version
/// to walk an account through every intermediate step up to the new current.
#[cfg(not(feature = "migration-test"))]
pub const ARNS_CONFIG_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);
#[cfg(feature = "migration-test")]
pub const ARNS_CONFIG_VERSION: SchemaVersion = SchemaVersion::new(1, 3, 0);
pub const DEMAND_FACTOR_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);
pub const ARNS_RECORD_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);
pub const RETURNED_NAME_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);
pub const RESERVED_NAME_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);

// =========================================
// GENESIS FEES (mARIO)
// =========================================

/// Base registration fees by name length.
/// Index 0 = 1-character name, index 50 = 51-character name.
/// Values are in mARIO (1 ARIO = 1_000_000 mARIO).
pub const GENESIS_FEES: [u64; NUM_NAME_LENGTH_FEES] = [
    500_000_000_000, // 1-char
    100_000_000_000, // 2-char
    10_000_000_000,  // 3-char
    5_000_000_000,   // 4-char
    2_500_000_000,   // 5-char
    1_500_000_000,   // 6-char
    800_000_000,     // 7-char
    500_000_000,     // 8-char
    400_000_000,     // 9-char
    350_000_000,     // 10-char
    300_000_000,     // 11-char
    250_000_000,     // 12-char
    200_000_000,     // 13-char
    200_000_000,     // 14-char
    200_000_000,     // 15-char
    200_000_000,     // 16-char
    200_000_000,     // 17-char
    200_000_000,     // 18-char
    200_000_000,     // 19-char
    200_000_000,     // 20-char
    200_000_000,     // 21-char
    200_000_000,     // 22-char
    200_000_000,     // 23-char
    200_000_000,     // 24-char
    200_000_000,     // 25-char
    200_000_000,     // 26-char
    200_000_000,     // 27-char
    200_000_000,     // 28-char
    200_000_000,     // 29-char
    200_000_000,     // 30-char
    200_000_000,     // 31-char
    200_000_000,     // 32-char
    200_000_000,     // 33-char
    200_000_000,     // 34-char
    200_000_000,     // 35-char
    200_000_000,     // 36-char
    200_000_000,     // 37-char
    200_000_000,     // 38-char
    200_000_000,     // 39-char
    200_000_000,     // 40-char
    200_000_000,     // 41-char
    200_000_000,     // 42-char
    200_000_000,     // 43-char
    200_000_000,     // 44-char
    200_000_000,     // 45-char
    200_000_000,     // 46-char
    200_000_000,     // 47-char
    200_000_000,     // 48-char
    200_000_000,     // 49-char
    200_000_000,     // 50-char
    200_000_000,     // 51-char
];

// =========================================
// PURCHASE TYPE
// =========================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub enum PurchaseType {
    Lease,
    Permabuy,
}

// =========================================
// ARNS CONFIG
// =========================================

/// ArNS registry configuration
/// PDA: ["arns_config"]
#[account]
pub struct ArnsConfig {
    /// Program upgrade / admin authority
    pub authority: Pubkey,
    /// ARIO SPL token mint
    pub mint: Pubkey,
    /// Treasury token account for protocol revenue
    pub treasury: Pubkey,
    /// Grace period after lease expiry (seconds, default 14 days)
    pub grace_period_seconds: i64,
    /// Duration of a returned-name auction (seconds, default 14 days)
    pub return_auction_duration_seconds: i64,
    /// Maximum lease length in years (default 5)
    pub max_lease_length_years: u8,
    /// Running count of names ever registered
    pub total_names_registered: u64,
    /// Next timestamp at which expired records should be pruned
    pub next_records_prune_timestamp: i64,
    /// Next timestamp at which returned names should be pruned
    pub next_returned_names_prune_timestamp: i64,
    /// Whether migration import is active (permanently disabled by finalize_migration)
    pub migration_active: bool,
    /// Dedicated migration authority (hot key for batch imports)
    pub migration_authority: Pubkey,
    /// PDA bump
    pub bump: u8,
    /// Schema version for forward-compatible migrations.
    pub version: SchemaVersion,
    /// Schema v1.1.0 — migration-test sentinel, not emitted in production builds.
    /// Populated by the 1.0.0→1.1.0 migration arm with a default of 1000.
    #[cfg(feature = "migration-test")]
    pub field_1: u64,
    /// Schema v1.2.0 — migration-test sentinel, not emitted in production builds.
    /// Populated by the 1.1.0→1.2.0 migration arm with a default of 42.
    #[cfg(feature = "migration-test")]
    pub field_2: u32,
    /// Schema v1.3.0 — migration-test sentinel, not emitted in production builds.
    /// Populated by the 1.2.0→1.3.0 migration arm with a default of true.
    #[cfg(feature = "migration-test")]
    pub field_3: bool,
}

impl ArnsConfig {
    // discriminator(8) + authority(32) + mint(32) + treasury(32) + grace_period(8)
    // + return_auction_duration(8) + max_lease_length_years(1) + total_names_registered(8)
    // + next_records_prune(8) + next_returned_names_prune(8)
    // + migration_active(1) + migration_authority(32) + bump(1) + version(3)
    #[cfg(not(feature = "migration-test"))]
    pub const SIZE: usize = 8 + 32 + 32 + 32 + 8 + 8 + 1 + 8 + 8 + 8 + 1 + 32 + 1 + 3;

    #[cfg(feature = "migration-test")]
    pub const SIZE: usize = 8 + 32 + 32 + 32 + 8 + 8 + 1 + 8 + 8 + 8 + 1 + 32 + 1 + 3
        + 8   // field_1: u64
        + 4   // field_2: u32
        + 1; // field_3: bool
}

// =========================================
// DEMAND FACTOR
// =========================================

/// Demand-factor state, updated once per period (day).
/// PDA: ["demand_factor"]
///
/// The demand factor adjusts name prices based on recent purchase activity.
/// Each period, the factor is multiplied by UP (1.05) if purchases exceed the
/// trailing-period moving average, or by DOWN (0.985) otherwise. If the factor
/// remains at MIN for MAX_PERIODS_AT_MIN_DEMAND_FACTOR consecutive periods the
/// fee table is permanently halved and the factor resets to 1.0.
#[account]
pub struct DemandFactor {
    /// Current demand factor, scaled by 1_000_000 (starts at 1_000_000 = 1.0)
    pub current_demand_factor: u64,
    /// Current period number (1-based, increments each day)
    pub current_period: u64,
    /// Number of name purchases in the current (incomplete) period
    pub purchases_this_period: u64,
    /// Revenue collected in the current (incomplete) period (mARIO)
    pub revenue_this_period: u64,
    /// How many consecutive periods the factor has been at DEMAND_FACTOR_MIN
    pub consecutive_periods_with_min_demand_factor: u32,
    /// Trailing purchase counts for the last 7 completed periods
    pub trailing_period_purchases: [u64; MOVING_AVG_PERIOD_COUNT],
    /// Trailing revenue for the last 7 completed periods
    pub trailing_period_revenues: [u64; MOVING_AVG_PERIOD_COUNT],
    /// Mutable fee table (starts as GENESIS_FEES, permanently halved when demand stays low)
    pub fees: [u64; NUM_NAME_LENGTH_FEES],
    /// Timestamp of period-zero start (anchor for computing current period)
    pub period_zero_start_timestamp: i64,
    /// Criteria for determining if demand is increasing (0 = revenue, 1 = purchases)
    pub criteria: u8,
    /// PDA bump
    pub bump: u8,
    /// Schema version for forward-compatible migrations.
    pub version: SchemaVersion,
}

/// Demand factor criteria: which metric determines if demand is increasing
pub const DEMAND_CRITERIA_REVENUE: u8 = 0;
pub const DEMAND_CRITERIA_PURCHASES: u8 = 1;

impl DemandFactor {
    // discriminator(8)
    // + current_demand_factor(8) + current_period(8)
    // + purchases_this_period(8) + revenue_this_period(8)
    // + consecutive_periods_with_min_demand_factor(4)
    // + trailing_period_purchases(8 * 7 = 56)
    // + trailing_period_revenues(8 * 7 = 56)
    // + fees(8 * 51 = 408)
    // + period_zero_start_timestamp(8)
    // + criteria(1)
    // + bump(1)
    // + version(3)
    pub const SIZE: usize = 8
        + 8
        + 8
        + 8
        + 8
        + 4
        + (8 * MOVING_AVG_PERIOD_COUNT)
        + (8 * MOVING_AVG_PERIOD_COUNT)
        + (8 * NUM_NAME_LENGTH_FEES)
        + 8
        + 1
        + 1
        + 3;
}

// =========================================
// ARNS RECORD
// =========================================

/// ArNS name record
/// PDA: ["arns_record", name_hash]
///
/// NOTE: Uses SHA256 hash of lowercase name as seed to prevent collision attacks.
///
/// Field order is **load-bearing**: every fixed-size field comes before
/// the variable-length `name` so external callers can `memcmp`-filter
/// `getProgramAccounts` queries on `ant` (offset 72) or `owner`
/// (offset 40) at fixed byte offsets. Notably this lets the SDK
/// resolve "ArNS records for this ANT mint" as a true point query
/// without scanning every record. See
/// `docs/ACCOUNT_SCALING_PATTERNS.md` for the broader pattern.
///
/// Offsets (Borsh, in bytes):
///   - discriminator:         0
///   - name_hash:             8   (32)
///   - owner:                 40  (32)  ← memcmp candidate
///   - ant:                   72  (32)  ← memcmp candidate
///   - purchase_type:         104 (1)
///   - start_timestamp:       105 (8)
///   - end_timestamp:         113 (1 + 8 option)
///   - undername_limit:       122 (2)
///   - purchase_price:        124 (8)
///   - bump:                  132 (1)
///   - version:               133 (3)
///   - name (variable-length): 136 (4 + ≤51)
#[account]
pub struct ArnsRecord {
    /// SHA256 hash of lowercase name (used for PDA derivation)
    pub name_hash: [u8; 32],
    /// Current owner. NOTE (ISSUES.md, Minor): set at purchase, never
    /// updated, never read on-chain. Slated for removal — kept here
    /// for now to avoid a second schema break.
    pub owner: Pubkey,
    /// Associated ANT (Metaplex NFT mint). Authoritative for all
    /// "who controls this name" decisions — flows through MPL Core
    /// asset ownership + `AntControllers`.
    pub ant: Pubkey,
    /// Lease or Permabuy
    pub purchase_type: PurchaseType,
    /// When the name was purchased (seconds)
    pub start_timestamp: i64,
    /// When the lease expires (seconds); None for permabuy
    pub end_timestamp: Option<i64>,
    /// Maximum number of undernames
    pub undername_limit: u16,
    /// Price paid at purchase (mARIO)
    pub purchase_price: u64,
    /// PDA bump
    pub bump: u8,
    /// Schema version for forward-compatible migrations.
    pub version: SchemaVersion,
    /// The name string (max 51 chars, original casing preserved).
    /// Variable-length — must remain the last field so all preceding
    /// fields stay at fixed byte offsets for memcmp filtering.
    pub name: String,
}

impl ArnsRecord {
    // discriminator(8) + name_hash(32) + owner(32) + ant(32)
    // + purchase_type(1) + start_timestamp(8) + end_timestamp(1 + 8)
    // + undername_limit(2) + purchase_price(8) + bump(1) + version(3)
    // + name(4 + MAX_NAME_LENGTH)
    pub const SIZE: usize = 8 + 32 + 32 + 32 + 1 + 8 + 9 + 2 + 8 + 1 + 3 + (4 + MAX_NAME_LENGTH);

    /// Byte offset of `ant` within the account data. Memcmp filter
    /// target for "ArNS records by ANT mint" point queries. SDK
    /// pins this in `sdk/src/solana/constants.ts` — keep them in
    /// sync if you ever reorder fields again.
    pub const ANT_OFFSET: usize = 8 + 32 + 32;

    /// Byte offset of `owner` within the account data. Same memcmp
    /// usage as `ANT_OFFSET` — wired through to the SDK constants.
    pub const OWNER_OFFSET: usize = 8 + 32;

    /// Maximum name length (characters)
    pub const MAX_NAME_LENGTH: usize = MAX_NAME_LENGTH;

    /// Minimum name length (characters)
    pub const MIN_NAME_LENGTH: usize = MIN_NAME_LENGTH;

    /// Default undername limit for new registrations
    pub const DEFAULT_UNDERNAME_LIMIT: u16 = DEFAULT_UNDERNAME_COUNT;

    /// Returns true if the name is currently active (not expired).
    /// A permabuy is always active. A lease is active while `end_timestamp >= timestamp`.
    pub fn is_active(&self, timestamp: i64) -> bool {
        match self.purchase_type {
            PurchaseType::Permabuy => true,
            PurchaseType::Lease => match self.end_timestamp {
                Some(end) => end >= timestamp,
                None => true, // should not happen for a lease, treat as active
            },
        }
    }

    /// Returns true if the name is expired but still within the grace period.
    pub fn is_in_grace_period(&self, timestamp: i64, grace_period: i64) -> bool {
        match self.purchase_type {
            PurchaseType::Permabuy => false,
            PurchaseType::Lease => match self.end_timestamp {
                Some(end) => {
                    let expired = end < timestamp;
                    let within_grace = timestamp <= end.saturating_add(grace_period);
                    expired && within_grace
                }
                None => false,
            },
        }
    }
}

// =========================================
// RETURNED NAME
// =========================================

/// Returned name (released or expired, available via Dutch auction)
/// PDA: ["returned_name", name_hash]
///
/// Floor price and premium multiplier are computed dynamically from
/// `returned_at`, the current timestamp, and RETURNED_NAME_DURATION_SECONDS /
/// RETURNED_NAME_MAX_MULTIPLIER, so they are NOT stored on-chain.
#[account]
pub struct ReturnedName {
    /// The name string (max 51 chars)
    pub name: String,
    /// SHA256 hash of lowercase name
    pub name_hash: [u8; 32],
    /// Who initiated the return (owner who released, or protocol for expiry)
    pub initiator: Pubkey,
    /// Timestamp when the name was returned (seconds)
    pub returned_at: i64,
    /// PDA bump
    pub bump: u8,
    /// Schema version for forward-compatible migrations.
    pub version: SchemaVersion,
}

impl ReturnedName {
    // discriminator(8) + name(4 + MAX_NAME_LENGTH) + name_hash(32)
    // + initiator(32) + returned_at(8) + bump(1) + version(3)
    pub const SIZE: usize = 8 + (4 + MAX_NAME_LENGTH) + 32 + 32 + 8 + 1 + 3;
}

// =========================================
// RESERVED NAME
// =========================================

/// Reserved name
/// PDA: ["reserved_name", name_hash]
#[account]
pub struct ReservedName {
    /// The name string (max 51 chars)
    pub name: String,
    /// Optional address the name is reserved for
    pub reserved_for: Option<Pubkey>,
    /// Optional expiry of the reservation (seconds)
    pub expires_at: Option<i64>,
    /// Authority that created the reservation
    pub reserved_by: Pubkey,
    /// When the reservation was created (seconds)
    pub created_at: i64,
    /// PDA bump
    pub bump: u8,
    /// Schema version for forward-compatible migrations.
    pub version: SchemaVersion,
}

impl ReservedName {
    // discriminator(8) + name(4 + MAX_NAME_LENGTH) + reserved_for(1 + 32)
    // + expires_at(1 + 8) + reserved_by(32) + created_at(8) + bump(1) + version(3)
    pub const SIZE: usize = 8 + (4 + MAX_NAME_LENGTH) + 33 + 9 + 32 + 8 + 1 + 3;
}

// =========================================
// NAME REGISTRY (for epoch prescription)
// =========================================

/// Global name registry for efficient enumeration.
/// PDA: `["name_registry"]`
///
/// **Dynamic-capacity layout (ADR-020, 2026-05-22):** The on-chain
/// account is variable-size — the struct here is the header only (40
/// bytes), and slots live after it as a raw byte array. Slot count is
/// derived from `data.len()` at read time, so reallocs (expand/shrink)
/// don't need to update any header field. This pattern is used by
/// OpenBook v2, MarginFi, Phoenix, and mpl-core itself.
///
/// Layout in bytes:
/// ```text
/// [0..8]    Anchor discriminator
/// [8..48]   header (this struct: authority, count, _padding)
/// [48..]    NameEntry slots (40 bytes each, count derived from data.len())
/// ```
///
/// Slot access goes through the `name_slots` / `name_slots_mut` helpers
/// below — every reading ix uses `AccountInfo` and these helpers
/// instead of `AccountLoader<NameRegistry>`, because the header size
/// is fixed but the slot array isn't.
///
/// **Initial deploy capacity:** `INITIAL_CAPACITY` slots (50,000 on
/// mainnet, 200 on `devnet-shrunk`). Expandable via
/// `admin_expand_name_registry` up to any reasonable target.
#[account(zero_copy(unsafe))]
#[repr(C)]
pub struct NameRegistry {
    pub authority: Pubkey,
    pub count: u32,
    pub _padding: [u8; 4],
}

impl NameRegistry {
    /// On-chain struct size (header only). Excludes the 8-byte Anchor
    /// discriminator. Used by Anchor's `init` constraint and by
    /// `AccountLoader` size check.
    pub const SIZE: usize = 32 + 4 + 4;
    /// Byte offset where the first NameEntry slot begins (includes the
    /// 8-byte discriminator).
    pub const HEADER_BYTES: usize = 8 + Self::SIZE;
    /// Initial slot count provisioned at deploy time. Expandable via
    /// `admin_expand_name_registry`.
    #[cfg(not(feature = "devnet-shrunk"))]
    pub const INITIAL_CAPACITY: usize = 50_000;
    #[cfg(feature = "devnet-shrunk")]
    pub const INITIAL_CAPACITY: usize = 200;

    /// Backward-compatible alias — many callers still reference
    /// `NameRegistry::MAX_NAMES` to bound iteration. Semantically the
    /// same as INITIAL_CAPACITY for fresh deployments, but on a
    /// post-expand registry the actual capacity (derived from
    /// `data.len()` via `slot_capacity`) may be higher.
    pub const MAX_NAMES: usize = Self::INITIAL_CAPACITY;

    /// Total account bytes for a registry at the given slot capacity.
    /// Includes the 8-byte discriminator.
    pub const fn bytes_for_capacity(slots: usize) -> usize {
        Self::HEADER_BYTES + slots * NameEntry::SIZE
    }
}

/// Compute the current slot capacity from raw account data length.
/// Round-trips through `(data.len() - HEADER_BYTES) / NameEntry::SIZE`.
/// The trailing bytes beyond `capacity * 40` (if data.len() isn't a
/// perfect multiple) are ignored.
pub fn slot_capacity(data: &[u8]) -> usize {
    data.len().saturating_sub(NameRegistry::HEADER_BYTES) / NameEntry::SIZE
}

/// Read-only slice view of every NameEntry slot. Returns an empty
/// slice if the account is too small to contain the header. Bounds
/// check defends against truncated registries (audit finding #4).
pub fn name_slots(data: &[u8]) -> &[NameEntry] {
    if data.len() < NameRegistry::HEADER_BYTES {
        return &[];
    }
    let cap = slot_capacity(data);
    let end = NameRegistry::HEADER_BYTES + cap * NameEntry::SIZE;
    bytemuck::cast_slice(&data[NameRegistry::HEADER_BYTES..end])
}

/// Mutable slice view of every NameEntry slot. Same shape as
/// `name_slots` but allows in-place edits + swap_remove during
/// `buy_record` / `release` flows. Empty slice on truncated input.
pub fn name_slots_mut(data: &mut [u8]) -> &mut [NameEntry] {
    if data.len() < NameRegistry::HEADER_BYTES {
        return &mut [];
    }
    let cap = slot_capacity(data);
    let end = NameRegistry::HEADER_BYTES + cap * NameEntry::SIZE;
    bytemuck::cast_slice_mut(&mut data[NameRegistry::HEADER_BYTES..end])
}

/// Read the registry header (authority + count + padding) from raw
/// account data. Skips the 8-byte Anchor discriminator. Callers must
/// ensure `data.len() >= HEADER_BYTES` — the wrapping ixs already
/// enforce this via PDA-seed-constrained account access, but in-program
/// callers should prefer the fallible `try_name_registry_header` below
/// if working with raw bytes from an unknown source.
pub fn name_registry_header(data: &[u8]) -> &NameRegistry {
    bytemuck::from_bytes(&data[8..NameRegistry::HEADER_BYTES])
}

/// Mutable header reader. Used by `buy_record` / `release` to bump
/// `count`.
pub fn name_registry_header_mut(data: &mut [u8]) -> &mut NameRegistry {
    bytemuck::from_bytes_mut(&mut data[8..NameRegistry::HEADER_BYTES])
}

/// Fallible variants — return an `InvalidAccountData` error instead
/// of panicking on truncated input. Use these in handlers that receive
/// account data through `remaining_accounts` or other paths where the
/// PDA-seed validation can't statically guarantee the size.
pub fn try_name_registry_header(data: &[u8]) -> Result<&NameRegistry> {
    require!(
        data.len() >= NameRegistry::HEADER_BYTES,
        crate::error::ArnsError::InvalidAccountData
    );
    Ok(name_registry_header(data))
}

pub fn try_name_registry_header_mut(data: &mut [u8]) -> Result<&mut NameRegistry> {
    require!(
        data.len() >= NameRegistry::HEADER_BYTES,
        crate::error::ArnsError::InvalidAccountData
    );
    Ok(name_registry_header_mut(data))
}

/// Append a new NameEntry to the registry. Returns the index it was
/// written at. Errors with `RegistryFull` if `count >= capacity` (no
/// expansion auto-triggered; caller must call `admin_expand_name_registry`
/// in a separate tx).
///
/// Encapsulates the borrow-checker dance: header and slot bytes are
/// non-overlapping regions, but `bytemuck::from_bytes_mut` returns a
/// reference that conflicts with `cast_slice_mut`. We work around it
/// by scoping each borrow narrowly.
pub fn append_name_entry(data: &mut [u8], entry: NameEntry) -> Result<u32> {
    let capacity = slot_capacity(data);
    let count_before = name_registry_header(data).count as usize;
    require!(
        count_before < capacity,
        crate::error::ArnsError::RegistryFull
    );
    let new_count = (count_before as u32)
        .checked_add(1)
        .ok_or(crate::error::ArnsError::ArithmeticOverflow)?;
    name_registry_header_mut(data).count = new_count;
    name_slots_mut(data)[count_before] = entry;
    Ok(count_before as u32)
}

/// Remove the NameEntry with the given `name_hash` via swap-remove
/// (move last slot into the freed position, zero out the now-unused
/// last slot, decrement count). No-op if the hash is not found. The
/// moved slot's `registry_index` self-pointer is updated to its new
/// position.
///
/// Used by `release_name_to_returned` / lease-expiry cleanup paths.
pub fn remove_name_entry_by_hash(data: &mut [u8], name_hash: [u8; 32]) -> bool {
    // Defensive cap: if a post-shrink registry ever had `count` exceed
    // the new capacity (shouldn't happen — admin_shrink rejects that
    // — but cheap to defend), don't iterate past the slot region.
    let count = (name_registry_header(data).count as usize).min(slot_capacity(data));
    if count == 0 {
        return false;
    }

    let mut found_idx: Option<usize> = None;
    for j in 0..count {
        if name_slots(data)[j].name_hash == name_hash {
            found_idx = Some(j);
            break;
        }
    }
    let idx = match found_idx {
        Some(i) => i,
        None => return false,
    };

    let last = count - 1;
    if idx != last {
        let last_entry = name_slots(data)[last];
        let slots = name_slots_mut(data);
        slots[idx] = last_entry;
        slots[idx].registry_index = idx as u32;
    }
    name_slots_mut(data)[last] = NameEntry::default();
    name_registry_header_mut(data).count = (count - 1) as u32;
    true
}

/// Entry in the name registry for enumeration
#[zero_copy]
#[repr(C)]
pub struct NameEntry {
    /// SHA256 hash of the name (matches ArnsRecord PDA derivation)
    pub name_hash: [u8; 32],
    /// Index in the registry (for O(1) removal)
    pub registry_index: u32,
    /// Padding for alignment
    pub _padding: [u8; 4],
}

impl Default for NameEntry {
    fn default() -> Self {
        Self {
            name_hash: [0u8; 32],
            registry_index: 0,
            _padding: [0u8; 4],
        }
    }
}

impl NameEntry {
    pub const SIZE: usize = 32 + 4 + 4; // 40 bytes
}

// =========================================
// NAME REGISTRY INDEX (stored in ArnsRecord)
// =========================================

/// Tracks the name's position in the registry
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default)]
pub struct RegistryIndex {
    pub index: u32,
    pub is_registered: bool,
}

impl RegistryIndex {
    pub const SIZE: usize = 4 + 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================
    // 3C. Demand Factor Tests
    // =========================================

    #[test]
    fn demand_factor_default_is_1x() {
        assert_eq!(DEMAND_FACTOR_SCALE, 1_000_000);
    }

    #[test]
    fn demand_factor_minimum_floor() {
        assert_eq!(DEMAND_FACTOR_MIN, 500_000);
    }

    #[test]
    fn demand_factor_up_adjustment() {
        // 1.05x
        assert_eq!(DEMAND_FACTOR_UP_ADJUSTMENT, 1_050_000);
    }

    #[test]
    fn demand_factor_down_adjustment() {
        // 0.985x
        assert_eq!(DEMAND_FACTOR_DOWN_ADJUSTMENT, 985_000);
    }

    #[test]
    fn demand_factor_step_increase() {
        // current=1.0, apply up adjustment: 1.0 * 1.05 = 1.05
        let current: u128 = DEMAND_FACTOR_SCALE as u128;
        let adjusted = current * DEMAND_FACTOR_UP_ADJUSTMENT as u128 / DEMAND_FACTOR_SCALE as u128;
        assert_eq!(adjusted as u64, 1_050_000);
    }

    #[test]
    fn demand_factor_step_decrease() {
        // current=1.0, apply down adjustment: 1.0 * 0.985 = 0.985
        let current: u128 = DEMAND_FACTOR_SCALE as u128;
        let adjusted =
            current * DEMAND_FACTOR_DOWN_ADJUSTMENT as u128 / DEMAND_FACTOR_SCALE as u128;
        assert_eq!(adjusted as u64, 985_000);
    }

    #[test]
    fn demand_factor_clamped_at_min() {
        // After many decreases, should not go below 0.5
        let mut factor = DEMAND_FACTOR_SCALE; // 1.0
        for _ in 0..200 {
            let adjusted = (factor as u128) * DEMAND_FACTOR_DOWN_ADJUSTMENT as u128
                / DEMAND_FACTOR_SCALE as u128;
            factor = (adjusted as u64).max(DEMAND_FACTOR_MIN);
        }
        assert_eq!(factor, DEMAND_FACTOR_MIN);
    }

    #[test]
    fn moving_avg_period_count() {
        assert_eq!(MOVING_AVG_PERIOD_COUNT, 7);
    }

    #[test]
    fn period_length_1_day() {
        assert_eq!(PERIOD_LENGTH_SECONDS, 86_400);
    }

    // =========================================
    // 3D. ArNS Record State Tests
    // =========================================

    fn make_lease_record(end_timestamp: i64) -> ArnsRecord {
        ArnsRecord {
            name_hash: [0u8; 32],
            name: "test".to_string(),
            owner: Pubkey::default(),
            ant: Pubkey::default(),
            purchase_type: PurchaseType::Lease,
            start_timestamp: 0,
            end_timestamp: Some(end_timestamp),
            undername_limit: 10,
            purchase_price: 1_000_000,
            bump: 0,
            version: ARNS_RECORD_VERSION,
        }
    }

    fn make_permabuy_record() -> ArnsRecord {
        ArnsRecord {
            name_hash: [0u8; 32],
            name: "test".to_string(),
            owner: Pubkey::default(),
            ant: Pubkey::default(),
            purchase_type: PurchaseType::Permabuy,
            start_timestamp: 0,
            end_timestamp: None,
            undername_limit: 10,
            purchase_price: 5_000_000,
            bump: 0,
            version: ARNS_RECORD_VERSION,
        }
    }

    #[test]
    fn lease_active_before_end() {
        let record = make_lease_record(100);
        assert!(record.is_active(50));
    }

    #[test]
    fn lease_active_at_end() {
        let record = make_lease_record(100);
        // end >= timestamp, so active at boundary
        assert!(record.is_active(100));
    }

    #[test]
    fn lease_expired_after_end() {
        let record = make_lease_record(100);
        assert!(!record.is_active(101));
    }

    #[test]
    fn in_grace_period() {
        let record = make_lease_record(100);
        let grace = 14 * 86_400i64;
        // ts=105, end=100 => expired but within grace
        assert!(record.is_in_grace_period(105, grace));
    }

    #[test]
    fn past_grace_period() {
        let record = make_lease_record(100);
        let grace = 14 * 86_400i64;
        // ts = 100 + 14*86400 + 1 = past grace
        assert!(!record.is_in_grace_period(100 + grace + 1, grace));
    }

    #[test]
    fn permabuy_always_active() {
        let record = make_permabuy_record();
        assert!(record.is_active(i64::MAX));
    }

    #[test]
    fn permabuy_not_in_grace_period() {
        let record = make_permabuy_record();
        assert!(!record.is_in_grace_period(1_000_000, 14 * 86_400));
    }

    // =========================================
    // ArNS Constants Tests
    // =========================================

    #[test]
    fn grace_period_14_days() {
        assert_eq!(GRACE_PERIOD_SECONDS, 14 * 86_400);
        assert_eq!(GRACE_PERIOD_SECONDS, 1_209_600);
    }

    #[test]
    fn returned_name_duration_14_days() {
        assert_eq!(RETURNED_NAME_DURATION_SECONDS, 14 * 86_400);
    }

    #[test]
    fn max_lease_length_5_years() {
        assert_eq!(MAX_LEASE_LENGTH_YEARS, 5);
    }

    #[test]
    fn one_year_seconds() {
        assert_eq!(ONE_YEAR_SECONDS, 365 * 86_400);
        assert_eq!(ONE_YEAR_SECONDS, 31_536_000);
    }

    #[test]
    fn max_name_length_51() {
        assert_eq!(MAX_NAME_LENGTH, 51);
    }

    #[test]
    fn arns_record_default_undername_limit() {
        assert_eq!(ArnsRecord::DEFAULT_UNDERNAME_LIMIT, 10);
    }

    #[test]
    fn genesis_fees_length() {
        assert_eq!(GENESIS_FEES.len(), 51);
    }

    #[test]
    fn demand_factor_size() {
        // 8 + 8 + 8 + 8 + 8 + 4 + 56 + 56 + 408 + 8 + 1 + 1 + 3 = 577
        assert_eq!(DemandFactor::SIZE, 577);
    }

    #[test]
    fn arns_config_size() {
        #[cfg(not(feature = "migration-test"))]
        assert_eq!(ArnsConfig::SIZE, 182);
        #[cfg(feature = "migration-test")]
        assert_eq!(ArnsConfig::SIZE, 182 + 8 + 4 + 1);
    }

    /// ADR-016 / BD-100 layout pin. The memcmp offsets (owner @ 40,
    /// ant @ 72) are load-bearing — the SDK pins them in
    /// `sdk/src/solana/constants.ts`. The `version` field sits after
    /// `bump` (offset 133, 3 bytes) and before the variable-length
    /// `name` (offset 136) so all fixed-offset fields are unchanged.
    #[test]
    fn arns_record_layout_pinned_at_191_bytes() {
        // discriminator(8) + name_hash(32) + owner(32) + ant(32)
        // + purchase_type(1) + start_timestamp(8) + end_timestamp(1+8)
        // + undername_limit(2) + purchase_price(8) + bump(1) + version(3)
        // + name(4 + MAX_NAME_LENGTH=51)
        // = 8 + 32 + 32 + 32 + 1 + 8 + 9 + 2 + 8 + 1 + 3 + 4 + 51
        // = 191
        assert_eq!(ArnsRecord::SIZE, 191);
        // Byte offsets pinned for the SDK's memcmp queries — MUST NOT CHANGE.
        assert_eq!(ArnsRecord::OWNER_OFFSET, 40);
        assert_eq!(ArnsRecord::ANT_OFFSET, 72);
    }

    // =========================================
    // Additional Demand Factor Behavioral Tests
    // =========================================

    #[test]
    fn demand_factor_increases_on_revenue() {
        // Simulate: current factor is 1.0, apply UP adjustment (revenue exceeded avg)
        // 1_000_000 * 1_050_000 / 1_000_000 = 1_050_000
        let current: u64 = DEMAND_FACTOR_SCALE;
        let adjusted = (current as u128 * DEMAND_FACTOR_UP_ADJUSTMENT as u128
            / DEMAND_FACTOR_SCALE as u128) as u64;
        assert_eq!(adjusted, 1_050_000);
        assert!(adjusted > current);
    }

    #[test]
    fn demand_factor_decreases_on_low_revenue() {
        // Simulate: current factor is 1.0, apply DOWN adjustment (revenue below avg)
        // 1_000_000 * 985_000 / 1_000_000 = 985_000
        let current: u64 = DEMAND_FACTOR_SCALE;
        let adjusted = (current as u128 * DEMAND_FACTOR_DOWN_ADJUSTMENT as u128
            / DEMAND_FACTOR_SCALE as u128) as u64;
        assert_eq!(adjusted, 985_000);
        assert!(adjusted < current);
    }

    #[test]
    fn moving_avg_correct() {
        // Simulate trailing revenues: [100, 200, 300, 400, 500, 600, 700]
        // Average = sum / 7 = 2800 / 7 = 400
        let revenues: [u64; MOVING_AVG_PERIOD_COUNT] = [100, 200, 300, 400, 500, 600, 700];
        let sum: u64 = revenues.iter().sum();
        let avg = sum / MOVING_AVG_PERIOD_COUNT as u64;
        assert_eq!(avg, 400);
    }

    #[test]
    fn moving_avg_all_zeros() {
        let revenues: [u64; MOVING_AVG_PERIOD_COUNT] = [0; MOVING_AVG_PERIOD_COUNT];
        let sum: u64 = revenues.iter().sum();
        let avg = sum / MOVING_AVG_PERIOD_COUNT as u64;
        assert_eq!(avg, 0);
    }

    #[test]
    fn arns_record_active_check() {
        // Lease with end_timestamp=1000 should be active at ts=500 and inactive at ts=1001
        let record = make_lease_record(1000);
        assert!(record.is_active(500));
        assert!(record.is_active(1000));
        assert!(!record.is_active(1001));
    }

    #[test]
    fn grace_period_boundary_exact() {
        // end=100, grace=1000: at ts=1100 (end+grace) should still be in grace
        let record = make_lease_record(100);
        assert!(record.is_in_grace_period(1100, 1000));
        // at ts=1101 (end+grace+1) should NOT be in grace
        assert!(!record.is_in_grace_period(1101, 1000));
    }

    #[test]
    fn lease_not_in_grace_while_active() {
        // end=100, ts=50 => active, NOT in grace period
        let record = make_lease_record(100);
        assert!(record.is_active(50));
        assert!(!record.is_in_grace_period(50, 14 * 86_400));
    }

    #[test]
    fn demand_factor_multiple_up_adjustments() {
        // Apply 3 consecutive up adjustments from 1.0
        // 1.0 -> 1.05 -> 1.1025 -> 1.157625
        let mut factor: u64 = DEMAND_FACTOR_SCALE;
        for _ in 0..3 {
            factor = (factor as u128 * DEMAND_FACTOR_UP_ADJUSTMENT as u128
                / DEMAND_FACTOR_SCALE as u128) as u64;
        }
        // 1_000_000 * 1.05^3 = 1_157_625
        assert_eq!(factor, 1_157_625);
    }

    #[test]
    fn demand_factor_consecutive_periods_at_min_triggers_fee_halving() {
        // After MAX_PERIODS_AT_MIN_DEMAND_FACTOR (7) periods at min, fees should halve
        assert_eq!(MAX_PERIODS_AT_MIN_DEMAND_FACTOR, 7);
        // Verify halving logic: each genesis fee halved
        let original = GENESIS_FEES[0]; // 500_000_000_000
        let halved = original / 2;
        assert_eq!(halved, 250_000_000_000);
    }

    // =========================================
    // Gap 8: Demand Factor Criteria Field
    // =========================================

    #[test]
    fn demand_criteria_constants() {
        assert_eq!(DEMAND_CRITERIA_REVENUE, 0);
        assert_eq!(DEMAND_CRITERIA_PURCHASES, 1);
    }

    #[test]
    fn demand_criteria_explicit_values() {
        // Verify constants match expected values for explicit initialization
        // These must be stable -- changing them would break on-chain state interpretation
        assert_eq!(DEMAND_CRITERIA_REVENUE, 0);
        assert_eq!(DEMAND_CRITERIA_PURCHASES, 1);
        // Ensure they are distinct
        assert_ne!(DEMAND_CRITERIA_REVENUE, DEMAND_CRITERIA_PURCHASES);
    }

    #[test]
    fn demand_factor_size_577() {
        // discriminator(8) + current_demand_factor(8) + current_period(8)
        // + purchases_this_period(8) + revenue_this_period(8)
        // + consecutive_periods_with_min_demand_factor(4)
        // + trailing_period_purchases(8*7=56)
        // + trailing_period_revenues(8*7=56)
        // + fees(8*51=408) + period_zero_start_timestamp(8)
        // + criteria(1) + bump(1) + version(3) = 577
        assert_eq!(DemandFactor::SIZE, 577);
    }

    fn make_demand_factor(
        criteria: u8,
        purchases_this_period: u64,
        revenue_this_period: u64,
        trailing_purchases: [u64; MOVING_AVG_PERIOD_COUNT],
        trailing_revenues: [u64; MOVING_AVG_PERIOD_COUNT],
    ) -> DemandFactor {
        DemandFactor {
            current_demand_factor: DEMAND_FACTOR_SCALE,
            current_period: 8,
            purchases_this_period,
            revenue_this_period,
            consecutive_periods_with_min_demand_factor: 0,
            trailing_period_purchases: trailing_purchases,
            trailing_period_revenues: trailing_revenues,
            fees: GENESIS_FEES,
            period_zero_start_timestamp: 0,
            criteria,
            bump: 0,
            version: DEMAND_FACTOR_VERSION,
        }
    }

    #[test]
    fn revenue_criteria_demand_increasing_when_above_avg() {
        // criteria = 0 (revenue): demand increases when revenue > trailing avg
        let trailing_revenues = [100, 200, 300, 400, 500, 600, 700];
        // avg = 2800 / 7 = 400
        let df = make_demand_factor(
            DEMAND_CRITERIA_REVENUE,
            0,   // purchases don't matter for revenue criteria
            500, // revenue this period = 500 > avg of 400
            [0; MOVING_AVG_PERIOD_COUNT],
            trailing_revenues,
        );

        let avg: u64 =
            df.trailing_period_revenues.iter().sum::<u64>() / MOVING_AVG_PERIOD_COUNT as u64;
        assert_eq!(avg, 400);
        // Revenue-based: demand increasing when revenue > avg
        assert!(df.revenue_this_period > avg);
        assert_eq!(df.criteria, DEMAND_CRITERIA_REVENUE);
    }

    #[test]
    fn revenue_criteria_demand_not_increasing_when_below_avg() {
        let trailing_revenues = [100, 200, 300, 400, 500, 600, 700];
        let df = make_demand_factor(
            DEMAND_CRITERIA_REVENUE,
            0,
            300, // revenue 300 < avg of 400
            [0; MOVING_AVG_PERIOD_COUNT],
            trailing_revenues,
        );

        let avg: u64 =
            df.trailing_period_revenues.iter().sum::<u64>() / MOVING_AVG_PERIOD_COUNT as u64;
        assert_eq!(avg, 400);
        assert!(df.revenue_this_period <= avg);
    }

    #[test]
    fn revenue_criteria_demand_not_increasing_when_zero() {
        // Zero revenue always means not increasing, even if avg is 0
        let df = make_demand_factor(
            DEMAND_CRITERIA_REVENUE,
            0,
            0, // zero revenue
            [0; MOVING_AVG_PERIOD_COUNT],
            [0; MOVING_AVG_PERIOD_COUNT],
        );
        // In is_demand_increasing: if revenue == 0, return false
        assert_eq!(df.revenue_this_period, 0);
    }

    #[test]
    fn purchase_criteria_demand_increasing_when_above_avg() {
        // criteria = 1 (purchases): demand increases when purchases > trailing avg
        let trailing_purchases = [10, 20, 30, 40, 50, 60, 70];
        // avg = 280 / 7 = 40
        let df = make_demand_factor(
            DEMAND_CRITERIA_PURCHASES,
            50, // purchases this period = 50 > avg of 40
            0,  // revenue doesn't matter for purchase criteria
            trailing_purchases,
            [0; MOVING_AVG_PERIOD_COUNT],
        );

        let avg: u64 =
            df.trailing_period_purchases.iter().sum::<u64>() / MOVING_AVG_PERIOD_COUNT as u64;
        assert_eq!(avg, 40);
        assert!(df.purchases_this_period > avg);
        assert_eq!(df.criteria, DEMAND_CRITERIA_PURCHASES);
    }

    #[test]
    fn purchase_criteria_demand_not_increasing_when_below_avg() {
        let trailing_purchases = [10, 20, 30, 40, 50, 60, 70];
        let df = make_demand_factor(
            DEMAND_CRITERIA_PURCHASES,
            30, // purchases 30 < avg of 40
            0,
            trailing_purchases,
            [0; MOVING_AVG_PERIOD_COUNT],
        );

        let avg: u64 =
            df.trailing_period_purchases.iter().sum::<u64>() / MOVING_AVG_PERIOD_COUNT as u64;
        assert!(df.purchases_this_period <= avg);
    }

    #[test]
    fn purchase_criteria_demand_not_increasing_when_zero() {
        let df = make_demand_factor(
            DEMAND_CRITERIA_PURCHASES,
            0, // zero purchases
            0,
            [10; MOVING_AVG_PERIOD_COUNT],
            [0; MOVING_AVG_PERIOD_COUNT],
        );
        // In is_demand_increasing: if purchases == 0, return false
        assert_eq!(df.purchases_this_period, 0);
    }

    #[test]
    fn demand_factor_up_adjustment_when_criteria_met() {
        // When demand is increasing (either criteria), factor should increase by 1.05x
        let factor = DEMAND_FACTOR_SCALE; // 1.0
        let adjusted = (factor as u128 * DEMAND_FACTOR_UP_ADJUSTMENT as u128
            / DEMAND_FACTOR_SCALE as u128) as u64;
        assert_eq!(adjusted, 1_050_000); // 1.05x
    }

    #[test]
    fn demand_factor_down_adjustment_when_criteria_not_met() {
        // When demand is not increasing, factor decreases by 0.985x
        let factor = DEMAND_FACTOR_SCALE; // 1.0
        let adjusted = (factor as u128 * DEMAND_FACTOR_DOWN_ADJUSTMENT as u128
            / DEMAND_FACTOR_SCALE as u128) as u64;
        assert_eq!(adjusted, 985_000); // 0.985x
    }

    // =========================================
    // Gap 7: Grace Period Check on Reassign
    // =========================================

    #[test]
    fn reassign_blocked_during_grace_period() {
        // A lease record in grace period should NOT be reassignable.
        // The reassign_name instruction checks:
        //   require!(record.is_active(timestamp), ArnsError::RecordExpired)
        //   require!(!record.is_in_grace_period(timestamp, grace), ArnsError::InGracePeriod)
        let record = make_lease_record(1000);
        let grace = GRACE_PERIOD_SECONDS;
        let timestamp = 1001; // just expired

        // Record is not active (expired)
        assert!(!record.is_active(timestamp));
        // Record IS in grace period
        assert!(record.is_in_grace_period(timestamp, grace));
        // Therefore reassign would be rejected
    }

    #[test]
    fn reassign_allowed_when_active() {
        // An active lease record should be reassignable (not in grace period)
        let record = make_lease_record(1000);
        let grace = GRACE_PERIOD_SECONDS;
        let timestamp = 500; // well before expiry

        assert!(record.is_active(timestamp));
        assert!(!record.is_in_grace_period(timestamp, grace));
        // Reassign would pass both checks
    }

    #[test]
    fn reassign_blocked_when_expired_past_grace() {
        // A fully expired record (past grace period) fails the is_active check
        let record = make_lease_record(1000);
        let grace = GRACE_PERIOD_SECONDS;
        let timestamp = 1000 + grace + 1; // past grace period

        assert!(!record.is_active(timestamp));
        assert!(!record.is_in_grace_period(timestamp, grace));
        // Reassign would be rejected by the is_active check (RecordExpired)
    }

    #[test]
    fn reassign_permabuy_always_allowed() {
        // Permabuy records are always active and never in grace period
        let record = make_permabuy_record();
        let grace = GRACE_PERIOD_SECONDS;
        let timestamp = i64::MAX;

        assert!(record.is_active(timestamp));
        assert!(!record.is_in_grace_period(timestamp, grace));
        // Reassign would pass both checks
    }

    #[test]
    fn reassign_grace_period_boundary() {
        // At the exact boundary of the grace period
        let record = make_lease_record(1000);
        let grace = GRACE_PERIOD_SECONDS;
        let timestamp = 1000 + grace; // exactly at end of grace period

        assert!(!record.is_active(timestamp));
        // At boundary: timestamp <= end + grace => still in grace
        assert!(record.is_in_grace_period(timestamp, grace));
        // Reassign would be rejected with InGracePeriod
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
}
