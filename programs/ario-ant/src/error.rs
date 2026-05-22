use anchor_lang::prelude::*;

#[error_code]
pub enum AntError {
    #[msg("Unauthorized: caller is not the ANT owner or a controller")]
    Unauthorized,

    #[msg("Unauthorized: caller does not have permission for this record")]
    UnauthorizedRecordAccess,

    #[msg("Invalid undername format")]
    InvalidUndername,

    #[msg("Undername too long (max 61 characters)")]
    UndernameTooLong,

    #[msg("Invalid target for the specified protocol")]
    InvalidTarget,

    #[msg("Unsupported target protocol")]
    UnsupportedProtocol,

    #[msg("Target too long (max 128 characters)")]
    TargetTooLong,

    #[msg("TTL out of range (must be 60-86400 seconds)")]
    InvalidTtl,

    #[msg("Record not found")]
    RecordNotFound,

    #[msg("Record has no owner, cannot transfer")]
    RecordHasNoOwner,

    #[msg("Cannot transfer record to current owner")]
    RecordTransferToSelf,

    #[msg("Controller already exists")]
    ControllerAlreadyExists,

    #[msg("Controller not found")]
    ControllerNotFound,

    #[msg("Maximum controllers reached (4)")]
    MaxControllersReached,

    #[msg("Name too long (max 61 characters)")]
    NameTooLong,

    #[msg("Name cannot be empty")]
    NameEmpty,

    #[msg("Description too long (max 128 characters)")]
    DescriptionTooLong,

    #[msg("Too many keywords (max 3)")]
    TooManyKeywords,

    #[msg("Keyword too long (max 32 characters)")]
    KeywordTooLong,

    #[msg("Invalid keyword format (alphanumeric, dash, underscore, #, @ only, no spaces)")]
    InvalidKeyword,

    #[msg("Duplicate keyword")]
    DuplicateKeyword,

    #[msg("Logo must be a valid 43-character Arweave address")]
    InvalidLogo,

    #[msg("Cannot change priority of @ record (must be 0)")]
    CannotChangePriorityOfRoot,

    #[msg("Priority can only be set by owner or controllers")]
    PriorityRequiresOwnerOrController,

    #[msg("Only owner or controllers can create new records")]
    OnlyOwnerOrControllerCanCreate,

    #[msg("Invalid Metaplex Core asset")]
    InvalidAsset,

    #[msg("Caller is not the NFT holder")]
    NotNftHolder,

    #[msg("Cannot remove the @ (root) record")]
    CannotRemoveRootRecord,

    #[msg("Ticker too long (max 16 characters)")]
    TickerTooLong,

    #[msg("Ticker cannot be empty")]
    TickerEmpty,

    // Migration
    #[msg("Migration is not active")]
    MigrationInactive,

    #[msg("Migration has already been finalized")]
    MigrationAlreadyFinalized,

    #[msg("Invalid account data for migration import")]
    InvalidAccountData,

    #[msg("PDA derivation does not match target account")]
    InvalidPda,

    // ANT migration
    #[msg("ANT is already at the latest version")]
    AlreadyLatestVersion,

    #[msg("Unknown schema version — no migration path exists from this version")]
    UnknownSchemaVersion,

    #[msg("Migration deadline has passed")]
    MigrationExpired,

    #[msg("Cannot close metadata: corresponding record still exists")]
    RecordStillExists,

    // ACL Registry (ADR-012 paginated per-user ACL)
    #[msg("ACL entry already exists for this (asset, role) pair")]
    AclEntryAlreadyExists,

    #[msg("ACL entry not found for this (asset, role) pair")]
    AclEntryNotFound,

    #[msg("ACL page has reached MAX_ACL_PAGE_ENTRIES — allocate a new page")]
    AclPageFull,

    #[msg("Cannot close ACL config: user still has allocated pages")]
    AclConfigNotEmpty,

    #[msg("Cannot close ACL page: page still has live entries")]
    AclPageNotEmpty,

    #[msg("Only the most recently allocated page can be closed")]
    AclPageNotLast,

    #[msg("Supplied AclPage does not belong to the target user")]
    AclPageUserMismatch,

    #[msg("Supplied AclPage index does not match the seeds used to derive it")]
    AclPageIndexMismatch,

    #[msg("Page index out of bounds for AclConfig.page_count")]
    AclPageOutOfBounds,

    #[msg("AclConfig.user does not match the target user address")]
    AclConfigUserMismatch,

    #[msg("Unknown AclRole codepoint — see AclRole::try_from_u8")]
    AclUnknownRole,

    #[msg("Target address is not the current owner of this asset")]
    NotCurrentOwner,

    #[msg("Target address is not currently a controller of this asset")]
    NotCurrentController,

    #[msg("Cannot remove ACL entry: address is still the current owner")]
    StillCurrentOwner,

    #[msg("Cannot remove ACL entry: address is still a current controller")]
    StillCurrentController,

    #[msg("Cannot transfer asset to its current owner")]
    TransferToSelf,

    /// `sync_attributes`: the supplied ArnsRecord PDA failed validation —
    /// owner is not ario-arns, PDA seeds do not derive from the supplied
    /// `name`, the discriminator is wrong, or the record's `ant` field
    /// does not point at the asset being synced.
    #[msg("Invalid ArnsRecord for sync_attributes")]
    InvalidArnsRecord,

    /// `admin_close_orphaned_ant_state`: the asset must be in post-burn
    /// state (System-owned, empty data) before per-ANT orphans can be
    /// cleaned up.
    #[msg("Asset still exists — cannot clean up orphaned state for a live asset")]
    AssetStillExists,

    /// Arithmetic overflow during manual account-close lamport math.
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,
}
