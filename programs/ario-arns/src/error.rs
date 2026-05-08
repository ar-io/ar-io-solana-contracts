use anchor_lang::prelude::*;

#[error_code]
pub enum ArnsError {
    // Name validation
    #[msg("Invalid name format")]
    InvalidNameFormat,
    #[msg("Name too short")]
    NameTooShort,
    #[msg("Name too long")]
    NameTooLong,
    #[msg("Name contains invalid characters")]
    InvalidCharacters,
    #[msg("Name length matches Arweave address length")]
    ForbiddenNameLength,

    // Registration
    #[msg("Name already registered")]
    NameAlreadyRegistered,
    #[msg("Name is active and cannot be purchased")]
    NameStillActive,
    #[msg("Insufficient funds for purchase")]
    InsufficientFunds,

    // Lease management
    #[msg("Name is not a lease")]
    NotALease,
    #[msg("Name is already permanent")]
    AlreadyPermanent,
    #[msg("Lease has expired")]
    LeaseExpired,
    #[msg("Invalid lease duration")]
    InvalidLeaseDuration,
    #[msg("Cannot extend permanent name")]
    CannotExtendPermanent,
    #[msg("Lease extension exceeds maximum allowed years")]
    ExtensionExceedsMax,
    #[msg("Record is in grace period")]
    InGracePeriod,
    #[msg("Record is expired")]
    RecordExpired,

    // Undername
    #[msg("Invalid undername quantity")]
    InvalidUndernameQuantity,

    // Reassignment
    #[msg("Invalid ANT for reassignment")]
    InvalidAnt,
    #[msg("Only the ANT process owner can reassign")]
    NotProcessOwner,

    // Release
    #[msg("Cannot release leased name")]
    CannotReleaseLease,

    // Returned names
    #[msg("Name is not in returned state")]
    NameNotReturned,
    #[msg("Return auction still active")]
    AuctionActive,
    #[msg("Name not expired past auction window — use prune_name_to_returned for Dutch auction")]
    NameNotExpired,

    // Reserved names
    #[msg("Name is reserved")]
    NameReserved,
    #[msg("Not authorized to claim this name")]
    NotReservationHolder,
    #[msg("Reservation has expired")]
    ReservationExpired,
    #[msg("Reservation not expired yet")]
    ReservationNotExpired,

    // Authorization
    #[msg("Not the name owner")]
    NotOwner,
    /// Retained for ABI stability; the only remaining emitter is `reassign_name`
    /// and `release_name` (which legitimately require ANT-holder authorization
    /// per Lua). `extend_lease`, `upgrade_name`, `increase_undername_limit`
    /// and their stake variants no longer emit this — they're permissionless,
    /// matching Lua. Removing the variant would shift later error codes and
    /// break clients that decode by index, so the variant stays.
    #[msg("Caller is not the ANT NFT holder")]
    NotAntHolder,
    #[msg("Unauthorized access")]
    Unauthorized,

    // Registry
    #[msg("Name registry is full")]
    RegistryFull,

    #[msg("Name registry account already exists")]
    NameRegistryAlreadyExists,
    #[msg("Name not found in registry")]
    NameNotInRegistry,

    // Math
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,
    #[msg("Arithmetic underflow")]
    ArithmeticUnderflow,

    // Gateway operator discount
    #[msg("Gateway is not in active (Joined) status")]
    GatewayNotActive,
    #[msg("Signer is not the gateway operator")]
    NotGatewayOperator,
    #[msg("Invalid gateway program ID")]
    InvalidGatewayProgram,

    // Treasury
    #[msg("Invalid treasury account")]
    InvalidTreasury,

    // General
    #[msg("Invalid parameter")]
    InvalidParameter,
    #[msg("Demand factor period not ready for update")]
    DemandFactorNotReady,

    // ANT validation
    #[msg("Invalid ANT asset - must be a valid Metaplex Core asset")]
    InvalidAntAsset,

    // Migration
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
}
