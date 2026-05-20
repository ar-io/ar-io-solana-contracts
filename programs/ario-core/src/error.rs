use anchor_lang::prelude::*;

#[error_code]
pub enum ArioError {
    // =========================================
    // INITIALIZATION ERRORS
    // =========================================
    #[msg("Protocol already initialized")]
    AlreadyInitialized,

    // =========================================
    // TOKEN ERRORS (F1-F3)
    // =========================================
    #[msg("Insufficient balance for transfer")]
    InsufficientBalance,

    #[msg("Invalid transfer amount - must be greater than zero")]
    InvalidAmount,

    #[msg("Cannot transfer to self")]
    SelfTransfer,

    // =========================================
    // VAULT ERRORS (F4-F9)
    // =========================================
    #[msg("Vault is still locked")]
    VaultLocked,

    #[msg("Vault has already been released")]
    VaultAlreadyReleased,

    #[msg("Invalid lock duration - below minimum")]
    LockDurationTooShort,

    #[msg("Invalid lock duration - exceeds maximum")]
    LockDurationTooLong,

    #[msg("Vault not found")]
    VaultNotFound,

    #[msg("Cannot extend vault - new end time must be after current")]
    InvalidVaultExtension,

    #[msg("Vault is not revocable")]
    VaultNotRevocable,

    #[msg("Not authorized to revoke this vault")]
    NotVaultController,

    #[msg("Cannot increase vault amount - vault already expired")]
    VaultExpired,

    #[msg("Maximum vaults per user exceeded")]
    MaxVaultsExceeded,

    #[msg("Vault amount below minimum (100 ARIO)")]
    VaultBelowMinimum,

    // =========================================
    // PRIMARY NAME ERRORS (F42-F46)
    // =========================================
    #[msg("Primary name request already exists")]
    PrimaryNameRequestExists,

    #[msg("Primary name request not found")]
    PrimaryNameRequestNotFound,

    #[msg("Primary name request has expired")]
    PrimaryNameRequestExpired,

    #[msg("Primary name request has not expired yet")]
    PrimaryNameRequestNotExpired,

    #[msg("Not authorized to approve this request - must be name owner")]
    NotNameOwner,

    #[msg("Primary name already set")]
    PrimaryNameAlreadySet,

    #[msg("No primary name set for this address")]
    NoPrimaryName,

    #[msg("Must remove existing primary name before setting a new one")]
    MustRemoveExistingPrimaryName,

    #[msg("Name too long - maximum 63 characters")]
    NameTooLong,

    #[msg("Name cannot be empty")]
    NameEmpty,

    // =========================================
    // AUTHORIZATION ERRORS
    // =========================================
    #[msg("Unauthorized - not the authority")]
    Unauthorized,

    #[msg("Invalid authority provided")]
    InvalidAuthority,

    #[msg("Invalid owner")]
    InvalidOwner,

    // =========================================
    // GENERAL ERRORS
    // =========================================
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,

    #[msg("Arithmetic underflow")]
    ArithmeticUnderflow,

    #[msg("Invalid parameter")]
    InvalidParameter,

    #[msg("Account already exists")]
    AccountAlreadyExists,

    #[msg("Invalid account state")]
    InvalidAccountState,

    #[msg("ArNS record not found for the base name")]
    ArnsRecordNotFound,

    #[msg("Invalid primary name format")]
    InvalidNameFormat,

    #[msg("Invalid treasury account")]
    InvalidTreasury,

    #[msg("Invalid ANT Metaplex Core asset")]
    InvalidAntAsset,

    #[msg("Caller is not the ANT NFT holder")]
    NotAntHolder,

    #[msg("Undername record not found or caller is not the record owner")]
    UndernameRecordOwnerRequired,

    // =========================================
    // MIGRATION ERRORS
    // =========================================
    #[msg("Migration is not active")]
    MigrationInactive,

    #[msg("Migration has already been finalized")]
    MigrationAlreadyFinalized,

    #[msg("Invalid account data for migration import")]
    InvalidAccountData,

    #[msg("PDA derivation does not match target account")]
    InvalidPda,

    #[msg("Migration deadline has passed")]
    MigrationExpired,

    // =========================================
    // SCHEMA MIGRATION ERRORS
    // =========================================
    #[msg("Account is already at the latest schema version")]
    AlreadyLatestVersion,

    #[msg("Unknown schema version — no migration path exists from this version")]
    UnknownSchemaVersion,
}
