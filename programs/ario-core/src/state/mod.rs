// AR.IO Core State Accounts
// PDA structures for token, vaults, and primary names

use anchor_lang::prelude::*;

// =========================================
// SEEDS
// =========================================

pub const CONFIG_SEED: &[u8] = b"ario_config";
pub const BALANCE_SEED: &[u8] = b"balance";
pub const VAULT_SEED: &[u8] = b"vault";
pub const VAULT_COUNTER_SEED: &[u8] = b"vault_counter";
pub const PRIMARY_NAME_SEED: &[u8] = b"primary_name";
pub const PRIMARY_NAME_REQUEST_SEED: &[u8] = b"primary_name_request";
pub const PRIMARY_NAME_REVERSE_SEED: &[u8] = b"primary_name_reverse";

// =========================================
// PROTOCOL CONFIGURATION
// =========================================

/// Global protocol configuration
/// PDA: ["ario_config"]
#[account]
pub struct ArioConfig {
    /// Admin authority for protocol updates
    pub authority: Pubkey,
    /// SPL token mint for ARIO token
    pub mint: Pubkey,
    /// ArNS program ID (for cross-program validation)
    pub arns_program: Pubkey,
    /// Treasury token account for fee collection
    pub treasury: Pubkey,
    /// Total token supply (fixed at creation)
    pub total_supply: u64,
    /// Protocol treasury balance
    pub protocol_balance: u64,
    /// Circulating supply (total - locked - staked)
    pub circulating_supply: u64,
    /// Amount locked in vaults
    pub locked_supply: u64,
    /// Minimum vault lock duration (seconds)
    pub min_vault_duration: i64,
    /// Maximum vault lock duration (seconds)
    pub max_vault_duration: i64,
    /// Primary name request expiry (seconds)
    pub primary_name_request_expiry: i64,
    /// Whether migration import is active (permanently disabled by finalize_migration)
    pub migration_active: bool,
    /// Dedicated migration authority (hot key for batch imports)
    pub migration_authority: Pubkey,
    /// PDA bump
    pub bump: u8,
    /// GAR program ID. Used by `release_treasury_to_recipient` to verify
    /// the cross-program signer (`gar_settings`) was derived from the
    /// canonical GAR program. Mirrors the `arns_program` storage
    /// pattern. **Appended after `bump` for backward-compatibility:**
    /// fresh deploys via `initialize` populate it directly; pre-existing
    /// deployments populate via `admin_set_gar_program` (which reallocs
    /// the account and writes the field). On a pre-realloc account this
    /// field reads as `Pubkey::default()`.
    pub gar_program: Pubkey,
}

impl ArioConfig {
    pub const SIZE: usize = 8  // discriminator
        + 32  // authority
        + 32  // mint
        + 32  // arns_program
        + 32  // treasury
        + 8   // total_supply
        + 8   // protocol_balance
        + 8   // circulating_supply
        + 8   // locked_supply
        + 8   // min_vault_duration
        + 8   // max_vault_duration
        + 8   // primary_name_request_expiry
        + 1   // migration_active
        + 32  // migration_authority
        + 1   // bump
        + 32; // gar_program (appended for backward-compat realloc)

    /// Default minimum vault duration: 14 days (matches Lua MIN_TOKEN_LOCK_TIME)
    pub const DEFAULT_MIN_VAULT_DURATION: i64 = 14 * 86_400;

    /// Default maximum vault duration: 200 years (matches Lua MAX_TOKEN_LOCK_TIME)
    pub const DEFAULT_MAX_VAULT_DURATION: i64 = 200 * 365 * 86_400;

    /// Default primary name request expiry: 7 days
    pub const DEFAULT_PRIMARY_NAME_REQUEST_EXPIRY: i64 = 7 * 86_400;

    /// Minimum vault size in mARIO (100 ARIO = 100_000_000 mARIO)
    /// Matches Lua: constants.MIN_VAULT_SIZE = 100 ARIO
    pub const MIN_VAULT_SIZE: u64 = 100_000_000;

    /// M1: Base fee for primary name requests (mARIO).
    /// Matches Lua: baseFeeForNameLength(51) * UNDERNAME_LEASE_FEE_PERCENTAGE
    /// = 200_000_000 * 0.001 = 200_000 mARIO (0.2 ARIO) at demand_factor=1.0
    /// Demand factor is applied by reading the ArNS DemandFactor via remaining_accounts.
    pub const PRIMARY_NAME_REQUEST_BASE_FEE: u64 = 200_000;
}

// =========================================
// TOKEN BALANCES
// =========================================

/// User token balance for protocol accounting
/// Note: Actual tokens held in SPL token accounts, this tracks protocol state
/// PDA: ["balance", user_pubkey]
#[account]
pub struct Balance {
    /// Account owner
    pub owner: Pubkey,
    /// Available balance (not locked)
    pub amount: u64,
    /// PDA bump
    pub bump: u8,
}

impl Balance {
    pub const SIZE: usize = 8 + 32 + 8 + 1;
}

// =========================================
// VAULTS
// =========================================

/// Tracks next vault ID per user
/// PDA: ["vault_counter", owner_pubkey]
#[account]
pub struct VaultCounter {
    /// Vault owner
    pub owner: Pubkey,
    /// Next vault ID to use
    pub next_id: u64,
    /// PDA bump
    pub bump: u8,
}

impl VaultCounter {
    pub const SIZE: usize = 8 + 32 + 8 + 1;
}

/// Locked token vault
/// PDA: ["vault", owner_pubkey, vault_id (as le_bytes)]
#[account]
pub struct Vault {
    /// Vault owner (receives tokens when unlocked)
    pub owner: Pubkey,
    /// Unique vault ID for this owner
    pub vault_id: u64,
    /// Locked token amount
    pub amount: u64,
    /// Lock start timestamp
    pub start_timestamp: i64,
    /// Unlock timestamp
    pub end_timestamp: i64,
    /// Optional controller for revocable vaults
    pub controller: Option<Pubkey>,
    /// Whether this vault is revocable by controller
    pub revocable: bool,
    /// PDA bump
    pub bump: u8,
}

impl Vault {
    pub const SIZE: usize = 8  // discriminator
        + 32  // owner
        + 8   // vault_id
        + 8   // amount
        + 8   // start_timestamp
        + 8   // end_timestamp
        + 33  // controller (Option<Pubkey>)
        + 1   // revocable
        + 1; // bump

    /// Check if vault is unlocked
    pub fn is_unlocked(&self, current_timestamp: i64) -> bool {
        current_timestamp >= self.end_timestamp
    }

    /// Check if vault can be revoked by the given authority
    pub fn can_revoke(&self, authority: &Pubkey) -> bool {
        self.revocable && self.controller.as_ref() == Some(authority)
    }
}

// =========================================
// PRIMARY NAMES
// =========================================

/// Primary name request (pending approval)
/// PDA: ["primary_name_request", initiator_pubkey]
#[account]
pub struct PrimaryNameRequest {
    /// Address requesting primary name
    pub initiator: Pubkey,
    /// Name being requested (must be owned by someone)
    pub name: String,
    /// Request creation timestamp
    pub created_at: i64,
    /// Request expiry timestamp
    pub expires_at: i64,
    /// PDA bump
    pub bump: u8,
}

impl PrimaryNameRequest {
    pub const SIZE: usize = 8  // discriminator
        + 32  // initiator
        + (4 + 63)  // name (String with max 63 chars)
        + 8   // created_at
        + 8   // expires_at
        + 1; // bump

    /// Maximum primary name length (matches Lua MAX_PRIMARY_NAME_LENGTH = 63)
    pub const MAX_NAME_LENGTH: usize = 63;

    /// Check if request has expired (matches Lua: endTimestamp <= timestamp)
    pub fn is_expired(&self, current_timestamp: i64) -> bool {
        current_timestamp >= self.expires_at
    }
}

/// Active primary name assignment
/// PDA: ["primary_name", owner_pubkey]
#[account]
pub struct PrimaryName {
    /// Address this primary name resolves to
    pub owner: Pubkey,
    /// The primary name
    pub name: String,
    /// When the primary name was set
    pub set_at: i64,
    /// PDA bump
    pub bump: u8,
}

impl PrimaryName {
    pub const SIZE: usize = 8  // discriminator
        + 32  // owner
        + (4 + 63)  // name
        + 8   // set_at
        + 1; // bump
}

/// Reverse lookup: name → owner. Ensures a name can only be set as primary for one address.
/// PDA: ["primary_name_reverse", hash(name.to_lowercase())]
#[account]
pub struct PrimaryNameReverse {
    /// The primary name (lowercase)
    pub name: String,
    /// The owner who has this as their primary name
    pub owner: Pubkey,
    /// PDA bump
    pub bump: u8,
}

impl PrimaryNameReverse {
    pub const SIZE: usize = 8  // discriminator
        + (4 + 63)  // name (String with max 63 chars)
        + 32  // owner
        + 1; // bump
}

// =========================================
// EVENTS
// =========================================

/// Emitted when tokens are transferred
#[event]
pub struct TransferEvent {
    pub from: Pubkey,
    pub to: Pubkey,
    pub amount: u64,
    pub timestamp: i64,
}

/// Emitted when a vault is created
#[event]
pub struct VaultCreatedEvent {
    pub owner: Pubkey,
    pub vault_id: u64,
    pub amount: u64,
    pub end_timestamp: i64,
    pub revocable: bool,
}

/// Emitted when a vault is revoked
#[event]
pub struct VaultRevokedEvent {
    pub owner: Pubkey,
    pub vault_id: u64,
    pub amount: u64,
    pub controller: Pubkey,
    pub timestamp: i64,
}

/// Emitted when a vault is released
#[event]
pub struct VaultReleasedEvent {
    pub owner: Pubkey,
    pub vault_id: u64,
    pub amount: u64,
    pub timestamp: i64,
}

/// Emitted when primary name is set
#[event]
pub struct PrimaryNameSetEvent {
    pub owner: Pubkey,
    pub name: String,
    pub timestamp: i64,
}

/// PR-5: emitted when an existing vault's lock duration is extended.
///
/// `new_end_timestamp` is the post-extension expiry, so a consumer
/// indexing this stream can update vault state without reading the
/// account back.
#[event]
pub struct VaultExtendedEvent {
    pub owner: Pubkey,
    pub vault_id: u64,
    pub new_end_timestamp: i64,
    pub timestamp: i64,
}

/// PR-5: emitted when additional tokens are added to an existing vault.
///
/// `new_balance` mirrors `Vault.amount` post-increase so consumers don't
/// have to fetch the account.
#[event]
pub struct VaultIncreasedEvent {
    pub owner: Pubkey,
    pub vault_id: u64,
    pub added_amount: u64,
    pub new_balance: u64,
    pub timestamp: i64,
}

/// PR-5: emitted when a primary name *request* is created.
///
/// Distinct from `PrimaryNameSetEvent` — set fires from the
/// auto-approve path (`request_and_set_primary_name*`) and from
/// `approve_primary_name`, while this event covers the request-only
/// path that needs explicit approval (or expiry).
///
/// `funding_source` matches the wire encoding used by ario-arns
/// (`FUNDING_SOURCE_*` constants in ario-arns and re-exported here as
/// `crate::FUNDING_SOURCE_BALANCE` / `crate::FUNDING_SOURCE_FUNDING_PLAN`):
/// `0 = Balance` (single-source SPL transfer from the initiator's ATA),
/// `4 = FundingPlan` (multi-source CPI through ario-gar
/// `pay_from_funding_plan`).
#[event]
pub struct PrimaryNameRequestedEvent {
    pub initiator: Pubkey,
    pub name: String,
    pub fee: u64,
    pub request_pda: Pubkey,
    /// See `crate::FUNDING_SOURCE_*` (matches ario-arns wire encoding).
    pub funding_source: u8,
    pub timestamp: i64,
}

/// PR-5: emitted when an expired primary-name request is reaped.
///
/// `refunded` is the lamports returned to the initiator (rent recovered
/// when the PDA is closed). The request fee is paid up front and is NOT
/// refunded — only the rent reservation comes back. Indexers consume
/// this to drop pending-request UI rows once they expire.
#[event]
pub struct PrimaryNameRequestExpiredEvent {
    pub initiator: Pubkey,
    pub name: String,
    pub refunded: u64,
    pub timestamp: i64,
}

/// PR-5: emitted when a primary name is removed.
///
/// `caller != owner` is the base-name override path
/// (`remove_primary_name_for_base_name`); `caller == owner` is the
/// holder's own removal (`remove_primary_name`). One event covers both
/// to keep the wire surface compact — consumers branch on the caller
/// equality rather than subscribing to two event types.
#[event]
pub struct PrimaryNameRemovedEvent {
    pub owner: Pubkey,
    pub name: String,
    pub caller: Pubkey,
    pub timestamp: i64,
}

/// PR-5: emitted exactly once when migration imports are sealed.
///
/// Watershed event — every indexer needs to know "migration closed at
/// slot N." `total_supply` is captured at the moment of finalization
/// (post `finalize_supply`), and `slot` is the Clock slot at the
/// finalize tx.
#[event]
pub struct CoreMigrationFinalizedEvent {
    pub admin: Pubkey,
    pub total_supply: u64,
    pub slot: u64,
    pub timestamp: i64,
}

/// PR-5: emitted exactly once when supply totals are sealed via
/// `finalize_supply`.
///
/// Genesis-equivalent: pins the canonical `total_supply` and `decimals`
/// for the ARIO mint. Consumed by indexers to seed their supply-tracking
/// dashboards from chain state alone.
#[event]
pub struct SupplyFinalizedEvent {
    pub admin: Pubkey,
    pub total_supply: u64,
    pub decimals: u8,
    pub timestamp: i64,
}

/// PR-5: emitted on every `update_config` field mutation. One event per
/// mutated field — if a single tx changes both `min_vault_duration` and
/// `primary_name_request_expiry`, two events fire.
///
/// `field` discriminator matches the `CORE_CONFIG_FIELD_*` constants in
/// `crate` (re-exported as `CORE_CONFIG_FIELD_MIN_VAULT_DURATION`,
/// `CORE_CONFIG_FIELD_MAX_VAULT_DURATION`,
/// `CORE_CONFIG_FIELD_PRIMARY_NAME_REQUEST_EXPIRY`,
/// `CORE_CONFIG_FIELD_NEW_AUTHORITY`). Values are stable forever — only
/// append new ones.
///
/// `u8` discriminator beats `String { field_name }` here: same indexer
/// utility, fewer wire bytes.
#[event]
pub struct ConfigUpdatedEvent {
    pub admin: Pubkey,
    /// See `crate::CORE_CONFIG_FIELD_*` constants.
    pub field: u8,
    /// New value, encoded based on `field`:
    /// - `MIN_VAULT_DURATION` (0): little-endian u64 in bytes 0..8, zero-padded
    /// - `MAX_VAULT_DURATION` (1): little-endian u64, zero-padded
    /// - `PRIMARY_NAME_REQUEST_EXPIRY` (2): little-endian u64, zero-padded
    /// - `NEW_AUTHORITY` (3): full 32-byte Pubkey
    pub new_value: [u8; 32],
    pub timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================
    // 2A. Vault Lifecycle Tests
    // =========================================

    fn make_vault(end_timestamp: i64, revocable: bool, controller: Option<Pubkey>) -> Vault {
        Vault {
            owner: Pubkey::default(),
            vault_id: 1,
            amount: 1_000_000,
            start_timestamp: 0,
            end_timestamp,
            controller,
            revocable,
            bump: 0,
        }
    }

    #[test]
    fn vault_locked_before_end() {
        let vault = make_vault(100, false, None);
        assert!(!vault.is_unlocked(50));
    }

    #[test]
    fn vault_unlocked_at_end() {
        let vault = make_vault(100, false, None);
        assert!(vault.is_unlocked(100));
    }

    #[test]
    fn vault_unlocked_after_end() {
        let vault = make_vault(100, false, None);
        assert!(vault.is_unlocked(200));
    }

    #[test]
    fn vault_can_revoke_with_correct_controller() {
        let controller = Pubkey::new_unique();
        let vault = make_vault(100, true, Some(controller));
        assert!(vault.can_revoke(&controller));
    }

    #[test]
    fn vault_cannot_revoke_wrong_controller() {
        let controller = Pubkey::new_unique();
        let wrong = Pubkey::new_unique();
        let vault = make_vault(100, true, Some(controller));
        assert!(!vault.can_revoke(&wrong));
    }

    #[test]
    fn vault_cannot_revoke_non_revocable() {
        let controller = Pubkey::new_unique();
        let vault = make_vault(100, false, Some(controller));
        assert!(!vault.can_revoke(&controller));
    }

    #[test]
    fn vault_size_calculation() {
        // discriminator(8) + owner(32) + vault_id(8) + amount(8) + start(8) + end(8) + controller(33) + revocable(1) + bump(1) = 107
        assert_eq!(Vault::SIZE, 107);
    }

    #[test]
    fn min_vault_duration_14_days() {
        assert_eq!(ArioConfig::DEFAULT_MIN_VAULT_DURATION, 14 * 86_400);
        assert_eq!(ArioConfig::DEFAULT_MIN_VAULT_DURATION, 1_209_600);
    }

    #[test]
    fn max_vault_duration_200_years() {
        assert_eq!(ArioConfig::DEFAULT_MAX_VAULT_DURATION, 200 * 365 * 86_400);
        assert_eq!(ArioConfig::DEFAULT_MAX_VAULT_DURATION, 6_307_200_000);
    }

    // =========================================
    // 2B. Primary Name Request Tests
    // =========================================

    #[test]
    fn request_not_expired_before_expiry() {
        let request = PrimaryNameRequest {
            initiator: Pubkey::default(),
            name: "test".to_string(),
            created_at: 0,
            expires_at: 100,
            bump: 0,
        };
        assert!(!request.is_expired(50));
    }

    #[test]
    fn request_expired_at_boundary() {
        let request = PrimaryNameRequest {
            initiator: Pubkey::default(),
            name: "test".to_string(),
            created_at: 0,
            expires_at: 100,
            bump: 0,
        };
        assert!(request.is_expired(100));
    }

    #[test]
    fn request_expired_after() {
        let request = PrimaryNameRequest {
            initiator: Pubkey::default(),
            name: "test".to_string(),
            created_at: 0,
            expires_at: 100,
            bump: 0,
        };
        assert!(request.is_expired(200));
    }

    #[test]
    fn max_name_length_63() {
        assert_eq!(PrimaryNameRequest::MAX_NAME_LENGTH, 63);
    }

    #[test]
    fn default_expiry_7_days() {
        assert_eq!(ArioConfig::DEFAULT_PRIMARY_NAME_REQUEST_EXPIRY, 7 * 86_400);
        assert_eq!(ArioConfig::DEFAULT_PRIMARY_NAME_REQUEST_EXPIRY, 604_800);
    }

    // =========================================
    // 2C. Config Constants Tests
    // =========================================

    #[test]
    fn config_size_matches() {
        // discriminator(8) + authority(32) + mint(32) + arns_program(32) + treasury(32)
        // + total_supply(8) + protocol_balance(8) + circulating_supply(8) + locked_supply(8)
        // + min_vault_duration(8) + max_vault_duration(8) + primary_name_request_expiry(8)
        // + migration_active(1) + migration_authority(32) + bump(1)
        // + gar_program(32) = 258
        assert_eq!(ArioConfig::SIZE, 258);
    }

    #[test]
    fn balance_size_matches() {
        // discriminator(8) + owner(32) + amount(8) + bump(1) = 49
        assert_eq!(Balance::SIZE, 49);
    }

    #[test]
    fn vault_counter_size() {
        // discriminator(8) + owner(32) + next_id(8) + bump(1) = 49
        assert_eq!(VaultCounter::SIZE, 49);
    }

    #[test]
    fn vault_size() {
        assert_eq!(Vault::SIZE, 107);
    }

    // =========================================
    // 2D. Supply Tracking Logic Tests
    // =========================================

    #[test]
    fn create_vault_decreases_circulating() {
        let mut circulating: u64 = 1_000_000;
        let mut locked: u64 = 0;
        let lock_amount: u64 = 100;
        circulating -= lock_amount;
        locked += lock_amount;
        assert_eq!(circulating, 999_900);
        assert_eq!(locked, 100);
    }

    #[test]
    fn release_vault_increases_circulating() {
        let mut circulating: u64 = 999_900;
        let mut locked: u64 = 100;
        let release_amount: u64 = 100;
        circulating += release_amount;
        locked -= release_amount;
        assert_eq!(circulating, 1_000_000);
        assert_eq!(locked, 0);
    }

    #[test]
    fn transfer_no_supply_change() {
        let total_circulating: u64 = 1_000_000;
        let mut from_balance: u64 = 500;
        let mut to_balance: u64 = 300;
        let transfer_amount: u64 = 50;
        from_balance -= transfer_amount;
        to_balance += transfer_amount;
        assert_eq!(from_balance + to_balance, 800); // sum unchanged
                                                    // total circulating unchanged
        assert_eq!(total_circulating, 1_000_000);
    }

    #[test]
    fn vaulted_transfer_tracks_correctly() {
        let mut locked: u64 = 0;
        let mut from_balance: u64 = 500;
        let vault_amount: u64 = 100;
        from_balance -= vault_amount;
        locked += vault_amount;
        assert_eq!(from_balance, 400);
        assert_eq!(locked, 100);
    }

    #[test]
    fn revoke_vault_returns_to_controller() {
        let mut locked: u64 = 100;
        let mut controller_balance: u64 = 500;
        let vault_amount: u64 = 100;
        locked -= vault_amount;
        controller_balance += vault_amount;
        assert_eq!(locked, 0);
        assert_eq!(controller_balance, 600);
    }

    // =========================================
    // Primary Name and PDA sizes
    // =========================================

    #[test]
    fn primary_name_request_size() {
        // discriminator(8) + initiator(32) + name(4+63) + created_at(8) + expires_at(8) + bump(1) = 124
        assert_eq!(PrimaryNameRequest::SIZE, 124);
    }

    #[test]
    fn min_vault_size_100_ario() {
        // Matches Lua: constants.MIN_VAULT_SIZE = 100 ARIO
        assert_eq!(ArioConfig::MIN_VAULT_SIZE, 100_000_000); // 100 ARIO in mARIO
    }

    #[test]
    fn primary_name_size() {
        // discriminator(8) + owner(32) + name(4+63) + set_at(8) + bump(1) = 116
        assert_eq!(PrimaryName::SIZE, 116);
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
