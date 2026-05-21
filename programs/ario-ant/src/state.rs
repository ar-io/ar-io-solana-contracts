use anchor_lang::prelude::*;

// =========================================
// SEEDS
// =========================================

pub const ANT_CONFIG_SEED: &[u8] = b"ant_config";
pub const ANT_CONTROLLERS_SEED: &[u8] = b"ant_controllers";
pub const ANT_RECORD_SEED: &[u8] = b"ant_record";
pub const ANT_RECORD_META_SEED: &[u8] = b"ant_record_meta";
pub const ANT_MIGRATION_CONFIG_SEED: &[u8] = b"ant_migration_config";
/// Per-user ACL config head (ADR-012). PDA: ["acl_config", user.as_ref()]
pub const ACL_CONFIG_SEED: &[u8] = b"acl_config";
/// Per-user ACL page (ADR-012).
/// PDA: ["acl_page", user.as_ref(), page_idx_le_bytes]
/// `page_idx_le_bytes` is always the 8-byte little-endian encoding of `u64`.
pub const ACL_PAGE_SEED: &[u8] = b"acl_page";

// =========================================
// MIGRATION CONFIG (singleton for ario-ant)
// =========================================

/// Migration configuration for the ANT program.
/// Separate from AntConfig because AntConfig is per-NFT.
/// PDA: ["ant_migration_config"]
#[account]
pub struct AntMigrationConfig {
    /// Admin authority (multi-sig)
    pub authority: Pubkey,
    /// Dedicated migration authority (hot key for batch imports)
    pub migration_authority: Pubkey,
    /// Whether migration import is active (permanently disabled by finalize_migration)
    pub migration_active: bool,
    /// PDA bump
    pub bump: u8,
}

impl AntMigrationConfig {
    pub const SIZE: usize = ANCHOR_DISCRIMINATOR_SIZE
        + PUBKEY_SIZE       // authority
        + PUBKEY_SIZE       // migration_authority
        + 1                 // migration_active: bool
        + BUMP_SIZE;
}

// =========================================
// CONSTANTS (matching Lua ANT process)
// =========================================

/// Maximum undername length (matches Lua MAX_UNDERNAME_LENGTH = 61)
pub const MAX_UNDERNAME_LENGTH: usize = 61;

/// Minimum TTL in seconds (matches Lua MIN_TTL_SECONDS = 60)
pub const MIN_TTL_SECONDS: u32 = 60;

/// Default TTL in seconds (matches Lua DEFAULT_TTL_SECONDS = 900)
pub const DEFAULT_TTL_SECONDS: u32 = 900;

/// Maximum TTL in seconds (matches Lua MAX_TTL_SECONDS = 86400)
pub const MAX_TTL_SECONDS: u32 = 86400;

/// Maximum ANT name / display name length (matches Lua MAX_NAME_LENGTH = 61)
pub const MAX_NAME_LENGTH: usize = 61;

/// Maximum description length. Tightened from 256 → 128 (2026-05-21) after
/// snapshot audit showed p99=107 chars across 1,040 source descriptions —
/// 128 covers 99% without truncation. Save ~3.2 SOL of rent across a
/// mainnet-scale migration. Lua's was 512; protocol has been progressively
/// shrinking this as real usage patterns clarify.
pub const MAX_DESCRIPTION_LENGTH: usize = 128;

/// Maximum number of keywords. Tightened from 8 → 3 (2026-05-21) after
/// snapshot audit showed p99=4 keywords (1% of ANTs have more). 3 covers
/// the typical-use case while saving 180 bytes per AntConfig × 3,551 ANTs
/// ≈ 4.5 SOL of mainnet rent. Lua's was 16.
pub const MAX_KEYWORDS: usize = 3;

/// Maximum keyword length (matches Lua MAX_KEYWORD_LENGTH = 32)
pub const MAX_KEYWORD_LENGTH: usize = 32;

/// Maximum number of controllers. Tightened from 10 → 4 (2026-05-21) after
/// snapshot audit showed p99=2 controllers (exactly 1 ANT in 3,551 has 7;
/// loses 3 on migration — acceptable, manually surfaced via the
/// migration's pre-flight check). Save 128 bytes per AntControllers ×
/// 3,551 ≈ 3.2 SOL of mainnet rent.
pub const MAX_CONTROLLERS: usize = 4;

/// Maximum number of ACL entries per `AclPage` (ADR-012).
///
/// Sizing rationale (per `docs/ACCOUNT_SCALING_PATTERNS.md` §8):
/// - Each entry is `Pubkey + u8 = 33 bytes`. `256 * 33 = 8_448 bytes` raw
///   page data — fits the default 32 KiB BPF heap with margin (no
///   `requestHeapFrame` needed) and stays well under Solana's 10 KiB
///   per-tx realloc limit at full size.
/// - Linear scan to find a `(mint, role)` entry is trivial CU at 256.
/// - Per-user `page_count` and `total_entries` are `u64` — there is **no
///   protocol-level cap** on the number of pages a user can have. Page
///   count grows with the user's ANT footprint (owner + controller
///   relationships across all of ArNS).
pub const MAX_ACL_PAGE_ENTRIES: usize = 256;

/// Maximum ticker length
pub const MAX_TICKER_LENGTH: usize = 16;

/// Arweave transaction ID length (base64url, 43 chars)
pub const ARWEAVE_TX_ID_LENGTH: usize = 43;

/// Maximum length for a content target (Arweave TX = 43, IPFS CIDv0 = 46,
/// IPFS CIDv1 ~59, generous headroom for future protocols).
pub const MAX_TARGET_LENGTH: usize = 128;

// =========================================
// BORSH / ACCOUNT LAYOUT SIZING CONSTANTS
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

/// Borsh-serialized max size of a single keyword: length prefix + MAX_KEYWORD_LENGTH.
pub const KEYWORD_BORSH_SIZE: usize = BORSH_LEN_PREFIX + MAX_KEYWORD_LENGTH;

/// IPFS CIDv0: always exactly 46 base58btc characters starting with "Qm".
pub const CIDV0_LENGTH: usize = 46;

/// Storage protocol: Arweave (default)
pub const PROTOCOL_ARWEAVE: u8 = 0;
/// Storage protocol: IPFS (CIDv0 and CIDv1)
pub const PROTOCOL_IPFS: u8 = 1;

// =========================================
// SCHEMA VERSION
// =========================================

/// Semantic version for ANT on-chain account schemas.
///
/// Stored as three consecutive `u8` bytes (3 bytes on the wire).
/// `Ord` is derived lexicographically over `(major, minor, patch)`, which
/// matches semver precedence: major is compared first, then minor, then
/// patch. This means `config.version < ANT_CONFIG_VERSION` is a valid
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

// =========================================
// ACCOUNTS
// =========================================

/// Current schema version for each ANT-side account type.
///
/// Bump the appropriate constant when changing the on-chain layout; the
/// `schema_migration` module's dispatch functions key off the stored version
/// to walk an account through every intermediate step up to the new current.
///
/// When the `migration-test` feature is active the constant is bumped to
/// 1.3.0 so that the four-schema migration E2E suite exercises a real
/// multi-step upgrade path.  Production builds always compile at 1.0.0.
#[cfg(not(feature = "migration-test"))]
pub const ANT_CONFIG_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);
#[cfg(feature = "migration-test")]
pub const ANT_CONFIG_VERSION: SchemaVersion = SchemaVersion::new(1, 3, 0);
pub const ANT_CONTROLLERS_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);
pub const ANT_RECORD_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);
pub const ANT_RECORD_METADATA_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);

/// Per-ANT configuration and metadata stored alongside the Metaplex Core asset.
/// Seeds: ["ant_config", mint]
#[account]
pub struct AntConfig {
    /// The Metaplex Core asset (NFT mint) this config belongs to
    pub mint: Pubkey,
    /// ANT display name (max 61 chars, matches Lua Name field)
    pub name: String,
    /// Ticker symbol (e.g. "ANT")
    pub ticker: String,
    /// Logo - Arweave transaction ID (43 chars)
    pub logo: String,
    /// Description (max 256 chars)
    pub description: String,
    /// Keywords (max 8, each max 32 chars)
    pub keywords: Vec<String>,
    /// Last known NFT owner - for lazy controller cleanup on transfer
    pub last_known_owner: Pubkey,
    /// PDA bump
    pub bump: u8,
    /// Schema version — used for per-ANT data migrations; see `ANT_CONFIG_VERSION`
    /// and `schema_migration::migrate_config_version` for the dispatch chain.
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

impl AntConfig {
    /// Conservative max size accounting for variable-length fields.
    #[cfg(not(feature = "migration-test"))]
    pub const SIZE: usize = ANCHOR_DISCRIMINATOR_SIZE
        + PUBKEY_SIZE                                       // mint
        + (BORSH_LEN_PREFIX + MAX_NAME_LENGTH)              // name
        + (BORSH_LEN_PREFIX + MAX_TICKER_LENGTH)            // ticker
        + (BORSH_LEN_PREFIX + ARWEAVE_TX_ID_LENGTH)         // logo
        + (BORSH_LEN_PREFIX + MAX_DESCRIPTION_LENGTH)       // description
        + (BORSH_LEN_PREFIX + MAX_KEYWORDS * KEYWORD_BORSH_SIZE) // keywords
        + PUBKEY_SIZE                                       // last_known_owner
        + BUMP_SIZE                                         // bump
        + SCHEMA_VERSION_SIZE; // version

    /// Extended size used by the `migration-test` feature:
    /// baseline (760) + field_1: u64 (8) + field_2: u32 (4) + field_3: bool (1) = 773.
    #[cfg(feature = "migration-test")]
    pub const SIZE: usize = ANCHOR_DISCRIMINATOR_SIZE
        + PUBKEY_SIZE                                       // mint
        + (BORSH_LEN_PREFIX + MAX_NAME_LENGTH)              // name
        + (BORSH_LEN_PREFIX + MAX_TICKER_LENGTH)            // ticker
        + (BORSH_LEN_PREFIX + ARWEAVE_TX_ID_LENGTH)         // logo
        + (BORSH_LEN_PREFIX + MAX_DESCRIPTION_LENGTH)       // description
        + (BORSH_LEN_PREFIX + MAX_KEYWORDS * KEYWORD_BORSH_SIZE) // keywords
        + PUBKEY_SIZE                                       // last_known_owner
        + BUMP_SIZE                                         // bump
        + SCHEMA_VERSION_SIZE                               // version
        + 8                                                 // field_1: u64
        + 4                                                 // field_2: u32
        + 1; // field_3: bool
}

/// Controller list for an ANT.
/// Seeds: ["ant_controllers", mint]
#[account]
pub struct AntControllers {
    /// The Metaplex Core asset this controller list belongs to
    pub mint: Pubkey,
    /// Controller addresses (max 10)
    pub controllers: Vec<Pubkey>,
    /// PDA bump
    pub bump: u8,
    /// Schema version — see `ANT_CONTROLLERS_VERSION` and
    /// `schema_migration::migrate_controllers_version`.
    pub version: SchemaVersion,
}

impl AntControllers {
    pub const SIZE: usize = ANCHOR_DISCRIMINATOR_SIZE
        + PUBKEY_SIZE                               // mint
        + (BORSH_LEN_PREFIX + MAX_CONTROLLERS * PUBKEY_SIZE) // controllers
        + BUMP_SIZE
        + SCHEMA_VERSION_SIZE;
}

// =========================================
// ACL: paginated per-user reverse index (ADR-012)
// =========================================

/// Relationship roles encoded as `u8` on each `AclEntry`.
///
/// Stored numerically rather than as an enum variant so adding new
/// relationships (e.g. `UndernameOwner`, `UndernameController`) becomes a
/// codepoint reservation rather than a schema/IDL change. The on-chain
/// program rejects unknown values via `try_from` in the instruction
/// handlers.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AclRole {
    /// Asset owner (MPL Core asset's `owner` field == user).
    Owner = 0,
    /// Asset controller (user appears in `AntControllers.controllers`).
    Controller = 1,
    // Reserved future relationships (do not reuse codepoints):
    //   2 = UndernameOwner
    //   3 = UndernameController
    //   ...
}

impl AclRole {
    pub fn try_from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(AclRole::Owner),
            1 => Some(AclRole::Controller),
            _ => None,
        }
    }
}

/// Single ACL entry: relationship between `user` (encoded in the parent
/// PDA seeds) and a Metaplex Core asset.
///
/// Layout: `Pubkey (32) + u8 (1) = 33 bytes`. See `MAX_ACL_PAGE_ENTRIES`.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub struct AclEntry {
    /// MPL Core asset (ANT mint) this relationship references.
    pub asset: Pubkey,
    /// Relationship kind — see `AclRole`. Stored as raw `u8` so future
    /// roles slot in without a struct change.
    pub role: u8,
}

impl AclEntry {
    /// Serialized size: `Pubkey + u8 = 33 bytes`.
    pub const SIZE: usize = PUBKEY_SIZE + 1;
}

/// Head record for a user's paginated ACL (ADR-012).
///
/// Stores the page count + total entry count. SDK reads this once, then
/// fans out to each `AclPage` via `getMultipleAccountsInfo`. There is no
/// protocol-level cap on how many pages a user can have — `page_count` is
/// `u64` so the design carries no implicit ceiling.
///
/// Seeds: `["acl_config", user.as_ref()]`
#[account]
pub struct AclConfig {
    /// The wallet this ACL belongs to.
    pub user: Pubkey,
    /// Number of `AclPage` PDAs allocated for this user.
    /// Pages are addressed by their index `[0, page_count)`.
    pub page_count: u64,
    /// Total live entries across all pages. Maintained by the program on
    /// every record/remove ix; lets clients sanity-check fan-out reads.
    pub total_entries: u64,
    /// PDA bump.
    pub bump: u8,
}

impl AclConfig {
    /// Discriminator + user + page_count(u64) + total_entries(u64) + bump.
    pub const SIZE: usize = ANCHOR_DISCRIMINATOR_SIZE + PUBKEY_SIZE + 8 + 8 + BUMP_SIZE;
}

/// One page of ACL entries for a user (ADR-012).
///
/// Pages are independent PDAs deterministically addressed by their index,
/// so any individual page can be fetched in O(1) without scanning sibling
/// pages. Removal uses `Vec::swap_remove` inside the page; the SDK's
/// "append to first non-full page" rule consumes any holes before
/// allocating a new page, so density is preserved at realistic churn
/// without cross-page bookkeeping.
///
/// Seeds: `["acl_page", user.as_ref(), &page_idx.to_le_bytes()]`
/// (`page_idx.to_le_bytes()` is always 8 bytes — `u64` LE encoding).
#[account]
pub struct AclPage {
    /// Owning user (matches `AclConfig.user`).
    pub user: Pubkey,
    /// Index of this page within the user's ACL.
    pub page_idx: u64,
    /// Live entries on this page. Length ≤ `MAX_ACL_PAGE_ENTRIES`.
    pub entries: Vec<AclEntry>,
    /// PDA bump.
    pub bump: u8,
}

impl AclPage {
    /// Minimum on-chain size: discriminator + user + page_idx(u64) + vec_len(0) + bump.
    pub const MIN_SIZE: usize =
        ANCHOR_DISCRIMINATOR_SIZE + PUBKEY_SIZE + 8 + BORSH_LEN_PREFIX + BUMP_SIZE;

    /// Maximum on-chain size: `MIN_SIZE + MAX_ACL_PAGE_ENTRIES * AclEntry::SIZE`.
    pub const MAX_SIZE: usize = Self::MIN_SIZE + MAX_ACL_PAGE_ENTRIES * AclEntry::SIZE;

    /// Compute on-chain size for a given live entry count.
    pub fn size_for(entry_count: usize) -> usize {
        Self::MIN_SIZE + entry_count * AclEntry::SIZE
    }

    /// Current serialized size (used for realloc decisions).
    pub fn current_size(&self) -> usize {
        Self::size_for(self.entries.len())
    }

    /// Linear scan for a `(asset, role)` pair within this page.
    pub fn position_of(&self, asset: &Pubkey, role: u8) -> Option<usize> {
        self.entries
            .iter()
            .position(|e| e.asset == *asset && e.role == role)
    }
}

/// A single undername record for an ANT (core fields only).
/// Optional per-record metadata lives in a separate AntRecordMetadata PDA
/// to avoid paying rent for rarely-used fields on every undername.
/// Seeds: ["ant_record", mint, undername_hash]
#[account]
pub struct AntRecord {
    /// The Metaplex Core asset this record belongs to
    pub mint: Pubkey,
    /// The undername (e.g. "@", "blog", "docs") — lowercase
    pub undername: String,
    /// Content address — Arweave TX ID (43 chars base64url), IPFS CID, or future protocol
    pub target: String,
    /// Storage protocol (0 = arweave, 1 = ipfs, 2+ = reserved for future)
    pub target_protocol: u8,
    /// TTL in seconds (60-86400)
    pub ttl_seconds: u32,
    /// Priority for ordering (None = unset, Some(0) required for "@")
    pub priority: Option<u32>,
    /// Optional record owner (delegated control)
    pub owner: Option<Pubkey>,
    /// Last reconciled owner — tracks the ANT owner at last record modification.
    /// If this differs from config.last_known_owner, the record's `owner` is cleared
    /// to prevent stale record-level ownership surviving NFT transfers.
    pub last_reconciled_owner: Pubkey,
    /// PDA bump
    pub bump: u8,
    /// Schema version — see `ANT_RECORD_VERSION` and
    /// `schema_migration::migrate_record_version`.
    pub version: SchemaVersion,
}

impl AntRecord {
    /// 8 (disc) + 32 (mint) + (4+61) undername + (4+128) target + 1 (target_protocol)
    /// + 4 (ttl) + (1+4) priority + (1+32) owner + 32 (last_reconciled_owner)
    /// + 1 (bump) + 3 (SchemaVersion)
    pub const SIZE: usize = ANCHOR_DISCRIMINATOR_SIZE
        + PUBKEY_SIZE                                       // mint
        + (BORSH_LEN_PREFIX + MAX_UNDERNAME_LENGTH)         // undername
        + (BORSH_LEN_PREFIX + MAX_TARGET_LENGTH)            // target
        + 1                                                 // target_protocol: u8
        + 4                                                 // ttl_seconds: u32
        + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX)          // priority: Option<u32>
        + (BORSH_OPTION_PREFIX + PUBKEY_SIZE)               // owner: Option<Pubkey>
        + PUBKEY_SIZE                                       // last_reconciled_owner
        + BUMP_SIZE
        + SCHEMA_VERSION_SIZE;
}

/// Optional per-record metadata, stored in a separate PDA to save rent on
/// the vast majority of records that don't use metadata fields.
/// Seeds: ["ant_record_meta", mint, undername_hash]
#[account]
pub struct AntRecordMetadata {
    /// The Metaplex Core asset this metadata belongs to (for getProgramAccounts filtering)
    pub mint: Pubkey,
    /// Hash of the undername (for PDA re-derivation in close/remove operations)
    pub undername_hash: [u8; 32],
    /// Optional display name (max 61 chars)
    pub display_name: Option<String>,
    /// Optional logo (Arweave TX ID, 43 chars — Arweave-only for permanence)
    pub record_logo: Option<String>,
    /// Optional description (max 256 chars)
    pub record_description: Option<String>,
    /// Optional keywords (max 8, each max 32 chars)
    pub record_keywords: Option<Vec<String>>,
    /// PDA bump
    pub bump: u8,
    /// Schema version — see `ANT_RECORD_METADATA_VERSION` and
    /// `schema_migration::migrate_record_metadata_version`.
    pub version: SchemaVersion,
}

impl AntRecordMetadata {
    /// 8 (disc) + 32 (mint) + 32 (undername_hash) + (1+4+61) display_name
    /// + (1+4+43) record_logo + (1+4+MAX_DESCRIPTION_LENGTH) record_description
    /// + (1+4+MAX_KEYWORDS*(4+32)) record_keywords + 1 (bump) + 3 (SchemaVersion)
    pub const SIZE: usize = ANCHOR_DISCRIMINATOR_SIZE
        + PUBKEY_SIZE                                             // mint
        + PUBKEY_SIZE                                             // undername_hash ([u8;32])
        + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX + MAX_UNDERNAME_LENGTH) // display_name: Option<String>
        + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX + ARWEAVE_TX_ID_LENGTH) // record_logo: Option<String>
        + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX + MAX_DESCRIPTION_LENGTH) // record_description
        + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX + MAX_KEYWORDS * KEYWORD_BORSH_SIZE) // record_keywords
        + BUMP_SIZE
        + SCHEMA_VERSION_SIZE;
}

// =========================================
// VALIDATION HELPERS
// =========================================

/// Hash an undername for PDA derivation
pub fn hash_undername(undername: &str) -> [u8; 32] {
    anchor_lang::solana_program::hash::hash(undername.to_lowercase().as_bytes()).to_bytes()
}

/// Validate undername format (matches Lua pattern: ^@$ or ^[a-zA-Z0-9]+[a-zA-Z0-9_-]*$)
pub fn is_valid_undername(name: &str) -> bool {
    if name.is_empty() || name.len() > MAX_UNDERNAME_LENGTH {
        return false;
    }
    if name == "@" {
        return true;
    }
    let bytes = name.as_bytes();
    // Must start with alphanumeric
    if !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    // Rest can be alphanumeric, dash, or underscore
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'-' || *b == b'_')
}

/// Validate Arweave transaction ID (43 base64url characters)
pub fn is_valid_arweave_id(id: &str) -> bool {
    id.len() == ARWEAVE_TX_ID_LENGTH
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Validate IPFS CID. Accepts both CIDv0 and CIDv1.
///
/// CIDv0: exactly 46 chars, starts with "Qm", base58btc body. Validated as
/// "Qm" + 44 alphanumeric chars (lazy — strict base58btc alphabet enforcement
/// is left to the gateway, matching the CIDv1 validation approach).
///
/// CIDv1: multibase prefix + alphanumeric body (≥ 10 chars total).
/// Accepted multibase prefixes: b (base32lower), B (base32upper),
/// z (base58btc), f (base16lower), u (base64url), k (base36lower).
pub fn is_valid_ipfs_cid(cid: &str) -> bool {
    if cid.len() < 10 || cid.len() > MAX_TARGET_LENGTH {
        return false;
    }
    // CIDv0: 'Qm' + 44 alphanumeric (no overlap with the CIDv1 multibase
    // prefixes b/B/z/f/u/k, so the dispatch is unambiguous).
    if cid.len() == CIDV0_LENGTH && cid.starts_with("Qm") {
        return cid[2..].bytes().all(|b| b.is_ascii_alphanumeric());
    }
    // CIDv1: multibase prefix + alphanumeric body.
    let first = cid.as_bytes()[0];
    if !matches!(first, b'b' | b'B' | b'z' | b'f' | b'u' | b'k') {
        return false;
    }
    cid[1..].bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Validate a content target against its declared protocol.
pub fn validate_target(target: &str, protocol: u8) -> bool {
    if target.is_empty() || target.len() > MAX_TARGET_LENGTH {
        return false;
    }
    match protocol {
        PROTOCOL_ARWEAVE => is_valid_arweave_id(target),
        PROTOCOL_IPFS => is_valid_ipfs_cid(target),
        _ => false,
    }
}

/// Validate a single keyword (matches Lua: ^[%w-_#@]+$, max 32 chars, no spaces)
pub fn is_valid_keyword(kw: &str) -> bool {
    !kw.is_empty()
        && kw.len() <= MAX_KEYWORD_LENGTH
        && kw
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'#' || b == b'@')
}

/// Validate a keywords array
pub fn validate_keywords(keywords: &[String]) -> bool {
    if keywords.len() > MAX_KEYWORDS {
        return false;
    }
    for (i, kw) in keywords.iter().enumerate() {
        if !is_valid_keyword(kw) {
            return false;
        }
        // Check for duplicates
        for other in keywords.iter().skip(i + 1) {
            if kw == other {
                return false;
            }
        }
    }
    true
}

/// Check if caller is the NFT owner or a controller.
/// Also performs lazy ownership reconciliation: if NFT owner has changed,
/// clears all controllers and updates last_known_owner.
pub fn reconcile_and_check_permission(
    config: &mut AntConfig,
    controllers: &mut AntControllers,
    current_nft_owner: &Pubkey,
    caller: &Pubkey,
) -> bool {
    // Lazy ownership reconciliation
    if config.last_known_owner != *current_nft_owner {
        controllers.controllers.clear();
        config.last_known_owner = *current_nft_owner;
    }

    // Check permission
    *caller == *current_nft_owner || controllers.controllers.contains(caller)
}

/// Validate TTL seconds against min/max bounds (Lua: utils.validateTTLSeconds)
pub fn is_valid_ttl(ttl: u32) -> bool {
    ttl >= MIN_TTL_SECONDS && ttl <= MAX_TTL_SECONDS
}

/// Validate @ record priority rules (Lua: setRecord @ priority check)
/// Returns true if the priority is valid for the given undername.
pub fn is_valid_priority_for_undername(undername: &str, priority: Option<u32>) -> bool {
    if undername == "@" {
        // @ record must have priority 0 or None (which defaults to 0)
        priority.is_none() || priority == Some(0)
    } else {
        // Other records: any non-negative priority is valid (u32 is always >= 0)
        true
    }
}

// =========================================
// TESTS
// =========================================

#[cfg(test)]
mod tests {
    use super::*;

    // Helper to create a test AntConfig
    fn make_config(owner: Pubkey) -> AntConfig {
        AntConfig {
            mint: Pubkey::new_unique(),
            name: "Test ANT".to_string(),
            ticker: "ANT".to_string(),
            logo: "a".repeat(43),
            description: String::new(),
            keywords: vec![],
            last_known_owner: owner,
            bump: 0,
            version: ANT_CONFIG_VERSION,
            #[cfg(feature = "migration-test")]
            field_1: 0,
            #[cfg(feature = "migration-test")]
            field_2: 0,
            #[cfg(feature = "migration-test")]
            field_3: false,
        }
    }

    fn make_controllers(mint: Pubkey, list: Vec<Pubkey>) -> AntControllers {
        AntControllers {
            mint,
            controllers: list,
            bump: 0,
            version: ANT_CONTROLLERS_VERSION,
        }
    }

    fn make_record(mint: Pubkey, undername: &str, owner: Option<Pubkey>) -> AntRecord {
        AntRecord {
            mint,
            undername: undername.to_string(),
            target: "a".repeat(43),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 900,
            priority: None,
            owner,
            last_reconciled_owner: Pubkey::default(),
            bump: 0,
            version: ANT_RECORD_VERSION,
        }
    }

    // =========================================
    // UNDERNAME VALIDATION (Lua: utils.validateUndername)
    // =========================================

    #[test]
    fn test_valid_undername_at() {
        assert!(is_valid_undername("@"));
    }

    #[test]
    fn test_valid_undername_alphanumeric() {
        assert!(is_valid_undername("blog"));
        assert!(is_valid_undername("my-site"));
        assert!(is_valid_undername("sub_domain"));
        assert!(is_valid_undername("a123"));
        assert!(is_valid_undername("A"));
        assert!(is_valid_undername("a"));
        assert!(is_valid_undername("abc-def_ghi"));
    }

    #[test]
    fn test_valid_undername_single_char() {
        assert!(is_valid_undername("a"));
        assert!(is_valid_undername("Z"));
        assert!(is_valid_undername("9"));
    }

    #[test]
    fn test_invalid_undername_empty() {
        assert!(!is_valid_undername(""));
    }

    #[test]
    fn test_invalid_undername_starts_with_dash() {
        assert!(!is_valid_undername("-blog"));
    }

    #[test]
    fn test_invalid_undername_starts_with_underscore() {
        assert!(!is_valid_undername("_blog"));
    }

    #[test]
    fn test_invalid_undername_special_chars() {
        assert!(!is_valid_undername("blog.site"));
        assert!(!is_valid_undername("my site"));
        assert!(!is_valid_undername("blog!"));
        assert!(!is_valid_undername("hello@world"));
        assert!(!is_valid_undername("a/b"));
        assert!(!is_valid_undername("a\\b"));
    }

    #[test]
    fn test_undername_max_length() {
        let name = "a".repeat(MAX_UNDERNAME_LENGTH);
        assert!(is_valid_undername(&name));
        let too_long = "a".repeat(MAX_UNDERNAME_LENGTH + 1);
        assert!(!is_valid_undername(&too_long));
    }

    #[test]
    fn test_undername_at_boundary_61() {
        assert!(is_valid_undername(&"a".repeat(61)));
        assert!(!is_valid_undername(&"a".repeat(62)));
    }

    #[test]
    fn test_undername_case_preserved() {
        // Validation accepts uppercase, lowering happens in handler
        assert!(is_valid_undername("Blog"));
        assert!(is_valid_undername("BLOG"));
    }

    // =========================================
    // ARWEAVE ID VALIDATION
    // =========================================

    #[test]
    fn test_valid_arweave_id() {
        assert!(is_valid_arweave_id(
            "-k7t8xMoB8hW482609Z9F4bTFMC3MnuW8bTvTyT8pFI"
        ));
        assert!(is_valid_arweave_id(&"a".repeat(43)));
        // Default AR.IO logo
        assert!(is_valid_arweave_id(
            "AnYvLJTWcG9lr2Ll5MwYWZR2o5uTE39WbpYB0zCxwKM"
        ));
    }

    #[test]
    fn test_valid_arweave_id_with_base64url_chars() {
        // All base64url chars: A-Z, a-z, 0-9, -, _
        // Must be exactly 43 chars
        assert!(is_valid_arweave_id(
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq"
        ));
        assert!(is_valid_arweave_id(
            "0123456789-abcdefghijklmnopqrstuvwxyz_01234"
        ));
    }

    #[test]
    fn test_invalid_arweave_id_wrong_length() {
        assert!(!is_valid_arweave_id(&"a".repeat(42)));
        assert!(!is_valid_arweave_id(&"a".repeat(44)));
        assert!(!is_valid_arweave_id(""));
        assert!(!is_valid_arweave_id("short"));
    }

    #[test]
    fn test_invalid_arweave_id_bad_chars() {
        let mut id = "a".repeat(42);
        id.push('!');
        assert!(!is_valid_arweave_id(&id));

        let mut id2 = "a".repeat(42);
        id2.push(' ');
        assert!(!is_valid_arweave_id(&id2));

        let mut id3 = "a".repeat(42);
        id3.push('+'); // base64 but not base64url
        assert!(!is_valid_arweave_id(&id3));
    }

    // =========================================
    // MULTI-PROTOCOL TARGET VALIDATION
    // =========================================

    #[test]
    fn test_validate_target_arweave() {
        assert!(validate_target(&"a".repeat(43), PROTOCOL_ARWEAVE));
        assert!(validate_target(
            "-k7t8xMoB8hW482609Z9F4bTFMC3MnuW8bTvTyT8pFI",
            PROTOCOL_ARWEAVE
        ));
    }

    #[test]
    fn test_validate_target_arweave_rejects_wrong_length() {
        assert!(!validate_target(&"a".repeat(42), PROTOCOL_ARWEAVE));
        assert!(!validate_target(&"a".repeat(44), PROTOCOL_ARWEAVE));
    }

    #[test]
    fn test_validate_target_rejects_empty() {
        assert!(!validate_target("", PROTOCOL_ARWEAVE));
        assert!(!validate_target("", PROTOCOL_IPFS));
    }

    #[test]
    fn test_validate_target_rejects_too_long() {
        let long = "a".repeat(MAX_TARGET_LENGTH + 1);
        assert!(!validate_target(&long, PROTOCOL_ARWEAVE));
    }

    #[test]
    fn test_validate_target_rejects_unknown_protocol() {
        assert!(!validate_target(&"a".repeat(43), 2));
        assert!(!validate_target(&"a".repeat(43), 255));
    }

    #[test]
    fn test_validate_target_ipfs_cidv1_base32() {
        assert!(validate_target(
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi",
            PROTOCOL_IPFS
        ));
    }

    #[test]
    fn test_validate_target_ipfs_cidv1_base58() {
        // z-prefix = base58btc
        assert!(validate_target(
            "zdj7WWeQ43G6JJvLWQWZrVoQuzgg1wKJpDHpBy7cnEB7bQTHD",
            PROTOCOL_IPFS
        ));
    }

    #[test]
    fn test_validate_target_ipfs_accepts_cidv0() {
        // Qm prefix = CIDv0, exactly 46 chars, base58btc body. Accepted.
        assert!(validate_target(
            "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG",
            PROTOCOL_IPFS
        ));
    }

    #[test]
    fn test_validate_target_ipfs_rejects_short_cid() {
        assert!(!validate_target("b12345678", PROTOCOL_IPFS)); // 9 chars, need >=10
    }

    #[test]
    fn test_validate_target_arweave_id_as_ipfs_fails() {
        // A valid 43-char Arweave TX ID should NOT pass IPFS validation
        // (Arweave IDs can contain - and _, IPFS CIDv1 body is alphanumeric only)
        assert!(!validate_target(
            "-k7t8xMoB8hW482609Z9F4bTFMC3MnuW8bTvTyT8pFI",
            PROTOCOL_IPFS
        ));
    }

    #[test]
    fn test_validate_target_ipfs_all_multibase_prefixes() {
        let body = "afybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        for prefix in ['b', 'B', 'z', 'f', 'u', 'k'] {
            let cid = format!("{}{}", prefix, body);
            assert!(
                validate_target(&cid, PROTOCOL_IPFS),
                "prefix '{}' should be valid",
                prefix
            );
        }
    }

    #[test]
    fn test_is_valid_ipfs_cid_v1() {
        // CIDv1 base32lower (b prefix)
        assert!(is_valid_ipfs_cid(
            "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi"
        ));
    }

    #[test]
    fn test_is_valid_ipfs_cid_multibase_prefixes() {
        let cid_body = "afybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
        assert!(is_valid_ipfs_cid(&format!("b{}", cid_body)));
        assert!(is_valid_ipfs_cid(&format!("B{}", cid_body)));
        assert!(is_valid_ipfs_cid(&format!("z{}", cid_body)));
        assert!(is_valid_ipfs_cid(&format!("f{}", cid_body)));
        assert!(is_valid_ipfs_cid(&format!("u{}", cid_body)));
        assert!(is_valid_ipfs_cid(&format!("k{}", cid_body)));
    }

    #[test]
    fn test_is_valid_ipfs_cid_accepts_cidv0() {
        // CIDv0: 46 chars, "Qm" prefix, base58btc body — accepted.
        assert!(is_valid_ipfs_cid(
            "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG"
        ));
    }

    #[test]
    fn test_is_valid_ipfs_cid_rejects_cidv0_wrong_length() {
        // 45 chars — one short of CIDv0
        assert!(!is_valid_ipfs_cid(
            "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbd"
        ));
        // 47 chars — one over
        assert!(!is_valid_ipfs_cid(
            "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdGX"
        ));
    }

    #[test]
    fn test_is_valid_ipfs_cid_rejects_cidv0_wrong_prefix() {
        // 46 chars but doesn't start with "Qm" — and "Qx" isn't a CIDv1
        // multibase prefix either, so the dispatch returns false.
        assert!(!is_valid_ipfs_cid(
            "QxYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG"
        ));
    }

    #[test]
    fn test_is_valid_ipfs_cid_rejects_short() {
        assert!(!is_valid_ipfs_cid("b12345678")); // 9 chars, needs >=10
    }

    #[test]
    fn test_is_valid_ipfs_cid_rejects_too_long() {
        let cid = format!("b{}", "a".repeat(MAX_TARGET_LENGTH));
        assert!(!is_valid_ipfs_cid(&cid)); // exceeds MAX_TARGET_LENGTH
    }

    // =========================================
    // KEYWORD VALIDATION (Lua: utils.validateKeywords)
    // =========================================

    #[test]
    fn test_valid_keywords() {
        assert!(validate_keywords(&["blog".to_string(), "tech".to_string()]));
        assert!(validate_keywords(&[
            "#defi".to_string(),
            "@user".to_string()
        ]));
        assert!(validate_keywords(&[]));
    }

    #[test]
    fn test_keywords_max_count() {
        // MAX_KEYWORDS = 3 (tightened from 8 for mainnet rent shrink).
        let kws: Vec<String> = (0..3).map(|i| format!("kw{}", i)).collect();
        assert!(validate_keywords(&kws));

        let too_many: Vec<String> = (0..4).map(|i| format!("kw{}", i)).collect();
        assert!(!validate_keywords(&too_many));
    }

    #[test]
    fn test_keywords_duplicate() {
        assert!(!validate_keywords(&[
            "blog".to_string(),
            "blog".to_string()
        ]));
    }

    #[test]
    fn test_keyword_max_length_32() {
        assert!(validate_keywords(&["a".repeat(32)]));
        assert!(!validate_keywords(&["a".repeat(33)]));
    }

    #[test]
    fn test_keyword_with_spaces() {
        assert!(!validate_keywords(&["my blog".to_string()]));
    }

    #[test]
    fn test_keyword_empty_string() {
        assert!(!validate_keywords(&["".to_string()]));
    }

    #[test]
    fn test_keyword_valid_special_chars() {
        // MAX_KEYWORDS = 3 — pick three to verify special-char acceptance.
        assert!(validate_keywords(&[
            "hello-world".to_string(),
            "foo_bar".to_string(),
            "#trending".to_string(),
        ]));
    }

    #[test]
    fn test_keyword_invalid_chars() {
        assert!(!validate_keywords(&["hello world".to_string()]));
        assert!(!validate_keywords(&["hello!".to_string()]));
        assert!(!validate_keywords(&["a.b".to_string()]));
        assert!(!validate_keywords(&["a/b".to_string()]));
    }

    #[test]
    fn test_keyword_duplicate_different_positions() {
        assert!(!validate_keywords(&[
            "a".to_string(),
            "b".to_string(),
            "a".to_string(),
        ]));
    }

    // =========================================
    // HASH UNDERNAME
    // =========================================

    #[test]
    fn test_hash_undername_case_insensitive() {
        assert_eq!(hash_undername("Blog"), hash_undername("blog"));
        assert_eq!(hash_undername("BLOG"), hash_undername("blog"));
        assert_eq!(hash_undername("@"), hash_undername("@"));
    }

    #[test]
    fn test_hash_undername_different_names() {
        assert_ne!(hash_undername("blog"), hash_undername("docs"));
        assert_ne!(hash_undername("@"), hash_undername("blog"));
    }

    // =========================================
    // CONSTANTS
    // =========================================

    #[test]
    fn test_constants() {
        assert_eq!(MAX_UNDERNAME_LENGTH, 61);
        assert_eq!(MIN_TTL_SECONDS, 60);
        assert_eq!(DEFAULT_TTL_SECONDS, 900);
        assert_eq!(MAX_TTL_SECONDS, 86400);
        assert_eq!(MAX_NAME_LENGTH, 61);
        assert_eq!(MAX_DESCRIPTION_LENGTH, 128); // reduced from 256 → 128 (mainnet rent shrink, 2026-05-21)
        assert_eq!(MAX_KEYWORDS, 3); // reduced from 8 → 3 (mainnet rent shrink, 2026-05-21)
        assert_eq!(MAX_CONTROLLERS, 4); // reduced from 10 → 4 (mainnet rent shrink, 2026-05-21)
        assert_eq!(MAX_KEYWORD_LENGTH, 32);
        assert_eq!(ARWEAVE_TX_ID_LENGTH, 43);
    }

    #[test]
    fn test_ttl_boundaries() {
        assert_eq!(MIN_TTL_SECONDS, 60); // 1 minute
        assert_eq!(MAX_TTL_SECONDS, 86400); // 1 day
        assert_eq!(DEFAULT_TTL_SECONDS, 900); // 15 minutes
    }

    #[test]
    fn test_controller_limit() {
        assert_eq!(MAX_CONTROLLERS, 4);
    }

    // =========================================
    // ACCOUNT SIZE CALCULATIONS
    // =========================================

    #[test]
    fn test_ant_config_size_sufficient() {
        let base = ANCHOR_DISCRIMINATOR_SIZE
            + PUBKEY_SIZE
            + (BORSH_LEN_PREFIX + MAX_NAME_LENGTH)
            + (BORSH_LEN_PREFIX + MAX_TICKER_LENGTH)
            + (BORSH_LEN_PREFIX + ARWEAVE_TX_ID_LENGTH)
            + (BORSH_LEN_PREFIX + MAX_DESCRIPTION_LENGTH)
            + (BORSH_LEN_PREFIX + MAX_KEYWORDS * KEYWORD_BORSH_SIZE)
            + PUBKEY_SIZE
            + BUMP_SIZE
            + SCHEMA_VERSION_SIZE;
        #[cfg(not(feature = "migration-test"))]
        let expected = base;
        #[cfg(feature = "migration-test")]
        let expected = base + 8 + 4 + 1; // field_1: u64 + field_2: u32 + field_3: bool
        assert_eq!(AntConfig::SIZE, expected);
        // Tightened from 760 → 452 (-308 bytes) on 2026-05-21:
        //   description: 256→128 (-128 b)
        //   keywords: 8×32 → 3×32 (-180 b)
        #[cfg(not(feature = "migration-test"))]
        assert_eq!(AntConfig::SIZE, 452);
        #[cfg(feature = "migration-test")]
        assert_eq!(AntConfig::SIZE, 465);
    }

    #[test]
    fn test_ant_controllers_size_sufficient() {
        let expected = ANCHOR_DISCRIMINATOR_SIZE
            + PUBKEY_SIZE
            + (BORSH_LEN_PREFIX + MAX_CONTROLLERS * PUBKEY_SIZE)
            + BUMP_SIZE
            + SCHEMA_VERSION_SIZE;
        assert_eq!(AntControllers::SIZE, expected);
    }

    // =========================================
    // ACL: paginated per-user reverse index (ADR-012) — sizing math
    // =========================================

    #[test]
    fn test_acl_role_codepoints_stable() {
        // Codepoints are part of the on-chain encoding — never reorder.
        assert_eq!(AclRole::Owner as u8, 0);
        assert_eq!(AclRole::Controller as u8, 1);
    }

    #[test]
    fn test_acl_role_try_from_round_trip() {
        assert_eq!(AclRole::try_from_u8(0), Some(AclRole::Owner));
        assert_eq!(AclRole::try_from_u8(1), Some(AclRole::Controller));
        // Reserved codepoints reject until handlers learn them.
        assert_eq!(AclRole::try_from_u8(2), None);
        assert_eq!(AclRole::try_from_u8(255), None);
    }

    #[test]
    fn test_acl_entry_size() {
        assert_eq!(AclEntry::SIZE, 32 + 1);
    }

    #[test]
    fn test_acl_config_size() {
        // disc(8) + user(32) + page_count(8) + total_entries(8) + bump(1)
        assert_eq!(AclConfig::SIZE, 8 + 32 + 8 + 8 + 1);
    }

    #[test]
    fn test_acl_page_min_and_max_size() {
        // disc(8) + user(32) + page_idx(8) + vec_len(4) + bump(1)
        assert_eq!(AclPage::MIN_SIZE, 8 + 32 + 8 + 4 + 1);
        assert_eq!(
            AclPage::MAX_SIZE,
            AclPage::MIN_SIZE + MAX_ACL_PAGE_ENTRIES * AclEntry::SIZE
        );
    }

    #[test]
    fn test_acl_page_size_for_growth_step() {
        let zero = AclPage::size_for(0);
        let one = AclPage::size_for(1);
        // Single entry append grows the account by exactly AclEntry::SIZE.
        assert_eq!(one - zero, AclEntry::SIZE);
        // The single-step delta stays well under Solana's 10 KiB
        // `MAX_PERMITTED_DATA_INCREASE` realloc cap.
        assert!(one - zero < 10 * 1024);
    }

    #[test]
    fn test_acl_page_max_size_fits_realloc_cap() {
        // Even a brand-new page allocated at MAX_SIZE fits the per-tx
        // realloc cap (≤ 10_240 bytes).
        assert!(AclPage::MAX_SIZE <= 10 * 1024);
    }

    #[test]
    fn test_acl_page_current_size_reflects_entries() {
        let user = Pubkey::new_unique();
        let entries: Vec<AclEntry> = (0..5)
            .map(|_| AclEntry {
                asset: Pubkey::new_unique(),
                role: AclRole::Owner as u8,
            })
            .collect();
        let page = AclPage {
            user,
            page_idx: 0,
            entries: entries.clone(),
            bump: 0,
        };
        assert_eq!(page.current_size(), AclPage::size_for(entries.len()));
    }

    #[test]
    fn test_acl_page_position_of_finds_entry() {
        let user = Pubkey::new_unique();
        let asset_a = Pubkey::new_unique();
        let asset_b = Pubkey::new_unique();
        let page = AclPage {
            user,
            page_idx: 0,
            entries: vec![
                AclEntry {
                    asset: asset_a,
                    role: AclRole::Owner as u8,
                },
                AclEntry {
                    asset: asset_b,
                    role: AclRole::Controller as u8,
                },
                AclEntry {
                    asset: asset_a,
                    role: AclRole::Controller as u8,
                },
            ],
            bump: 0,
        };
        assert_eq!(page.position_of(&asset_a, AclRole::Owner as u8), Some(0));
        assert_eq!(
            page.position_of(&asset_b, AclRole::Controller as u8),
            Some(1)
        );
        assert_eq!(
            page.position_of(&asset_a, AclRole::Controller as u8),
            Some(2)
        );
        // Pair must match both asset AND role.
        assert_eq!(page.position_of(&asset_b, AclRole::Owner as u8), None);
    }

    #[test]
    fn test_max_acl_page_entries_constant() {
        assert_eq!(MAX_ACL_PAGE_ENTRIES, 256);
    }

    // =========================================
    // RECONCILIATION (lazy ownership cleanup)
    // =========================================

    #[test]
    fn test_reconcile_clears_controllers_on_owner_change() {
        let old_owner = Pubkey::new_unique();
        let new_owner = Pubkey::new_unique();
        let controller = Pubkey::new_unique();

        let mut config = make_config(old_owner);
        let mut controllers = make_controllers(config.mint, vec![controller]);

        let result =
            reconcile_and_check_permission(&mut config, &mut controllers, &new_owner, &controller);
        assert!(!result);
        assert!(controllers.controllers.is_empty());
        assert_eq!(config.last_known_owner, new_owner);
    }

    #[test]
    fn test_reconcile_preserves_controllers_same_owner() {
        let owner = Pubkey::new_unique();
        let controller = Pubkey::new_unique();

        let mut config = make_config(owner);
        let mut controllers = make_controllers(config.mint, vec![controller]);

        let result =
            reconcile_and_check_permission(&mut config, &mut controllers, &owner, &controller);
        assert!(result);
        assert_eq!(controllers.controllers.len(), 1);
    }

    #[test]
    fn test_reconcile_clears_multiple_controllers() {
        let old_owner = Pubkey::new_unique();
        let new_owner = Pubkey::new_unique();
        let c1 = Pubkey::new_unique();
        let c2 = Pubkey::new_unique();
        let c3 = Pubkey::new_unique();

        let mut config = make_config(old_owner);
        let mut controllers = make_controllers(config.mint, vec![c1, c2, c3]);

        reconcile_and_check_permission(&mut config, &mut controllers, &new_owner, &c1);
        assert!(controllers.controllers.is_empty());
    }

    #[test]
    fn test_reconcile_new_owner_gets_access() {
        let old_owner = Pubkey::new_unique();
        let new_owner = Pubkey::new_unique();

        let mut config = make_config(old_owner);
        let mut controllers = make_controllers(config.mint, vec![old_owner]);

        // New owner should have access even though controllers were cleared
        let result =
            reconcile_and_check_permission(&mut config, &mut controllers, &new_owner, &new_owner);
        assert!(result);
    }

    #[test]
    fn test_reconcile_idempotent() {
        let owner = Pubkey::new_unique();
        let controller = Pubkey::new_unique();

        let mut config = make_config(owner);
        let mut controllers = make_controllers(config.mint, vec![controller]);

        // Call twice — should be the same
        reconcile_and_check_permission(&mut config, &mut controllers, &owner, &controller);
        assert_eq!(controllers.controllers.len(), 1);

        reconcile_and_check_permission(&mut config, &mut controllers, &owner, &controller);
        assert_eq!(controllers.controllers.len(), 1);
    }

    // =========================================
    // PERMISSION MODEL (Lua: 3-tier)
    // =========================================

    #[test]
    fn test_owner_always_has_permission() {
        let owner = Pubkey::new_unique();
        let mut config = make_config(owner);
        let mut controllers = make_controllers(config.mint, vec![]);

        assert!(reconcile_and_check_permission(
            &mut config,
            &mut controllers,
            &owner,
            &owner
        ));
    }

    #[test]
    fn test_controller_has_permission() {
        let owner = Pubkey::new_unique();
        let controller = Pubkey::new_unique();
        let mut config = make_config(owner);
        let mut controllers = make_controllers(config.mint, vec![controller]);

        assert!(reconcile_and_check_permission(
            &mut config,
            &mut controllers,
            &owner,
            &controller
        ));
    }

    #[test]
    fn test_random_user_no_permission() {
        let owner = Pubkey::new_unique();
        let random = Pubkey::new_unique();
        let mut config = make_config(owner);
        let mut controllers = make_controllers(config.mint, vec![]);

        assert!(!reconcile_and_check_permission(
            &mut config,
            &mut controllers,
            &owner,
            &random
        ));
    }

    // =========================================
    // VALIDATION EDGE CASES
    // =========================================

    #[test]
    fn test_undername_at_sign_only() {
        assert!(is_valid_undername("@"));
        assert!(!is_valid_undername("@@"));
        assert!(!is_valid_undername("@a"));
    }

    #[test]
    fn test_undername_numeric_start() {
        assert!(is_valid_undername("123"));
        assert!(is_valid_undername("1abc"));
    }

    #[test]
    fn test_undername_dash_underscore_middle() {
        assert!(is_valid_undername("a-b"));
        assert!(is_valid_undername("a_b"));
        assert!(is_valid_undername("a-b-c_d"));
    }

    #[test]
    fn test_undername_ends_with_dash_or_underscore() {
        // Lua pattern allows ending with dash/underscore: ^[a-zA-Z0-9]+[a-zA-Z0-9_-]*$
        assert!(is_valid_undername("blog-"));
        assert!(is_valid_undername("blog_"));
    }

    #[test]
    fn test_arweave_id_all_dashes() {
        assert!(is_valid_arweave_id(&"-".repeat(43)));
    }

    #[test]
    fn test_arweave_id_all_underscores() {
        assert!(is_valid_arweave_id(&"_".repeat(43)));
    }

    #[test]
    fn test_keyword_single_char() {
        assert!(validate_keywords(&["a".to_string()]));
    }

    #[test]
    fn test_keywords_exactly_max() {
        // MAX_KEYWORDS = 3
        let kws: Vec<String> = (0..3).map(|i| format!("kw{}", i)).collect();
        assert!(validate_keywords(&kws));
    }

    #[test]
    fn test_keyword_with_hash_and_at() {
        assert!(is_valid_keyword("#hashtag"));
        assert!(is_valid_keyword("@mention"));
        assert!(is_valid_keyword("#@combined"));
    }

    // =========================================
    // DEFAULT VALUES
    // =========================================

    #[test]
    fn test_default_logo_is_valid() {
        assert!(is_valid_arweave_id(
            "AnYvLJTWcG9lr2Ll5MwYWZR2o5uTE39WbpYB0zCxwKM"
        ));
    }

    #[test]
    fn test_default_ttl_in_range() {
        assert!(DEFAULT_TTL_SECONDS >= MIN_TTL_SECONDS);
        assert!(DEFAULT_TTL_SECONDS <= MAX_TTL_SECONDS);
    }

    // =========================================
    // RECORD STRUCT
    // =========================================

    #[test]
    fn test_record_with_all_optional_fields() {
        let record = AntRecord {
            mint: Pubkey::new_unique(),
            undername: "blog".to_string(),
            target: "a".repeat(43),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 3600,
            priority: Some(5),
            owner: Some(Pubkey::new_unique()),
            last_reconciled_owner: Pubkey::default(),
            bump: 0,
            version: ANT_RECORD_VERSION,
        };
        assert_eq!(record.undername, "blog");
        assert_eq!(record.ttl_seconds, 3600);
        assert_eq!(record.priority, Some(5));
        assert!(record.owner.is_some());
    }

    #[test]
    fn test_record_with_no_optional_fields() {
        let record = make_record(Pubkey::new_unique(), "@", None);
        assert!(record.priority.is_none());
        assert!(record.owner.is_none());
    }

    #[test]
    fn test_record_metadata_struct() {
        let meta = AntRecordMetadata {
            mint: Pubkey::new_unique(),
            undername_hash: [0u8; 32],
            display_name: Some("My Blog".to_string()),
            record_logo: Some("b".repeat(43)),
            record_description: Some("A blog about stuff".to_string()),
            record_keywords: Some(vec!["blog".to_string(), "personal".to_string()]),
            bump: 0,
            version: ANT_RECORD_METADATA_VERSION,
        };
        assert_eq!(meta.display_name, Some("My Blog".to_string()));
        assert!(meta.record_logo.is_some());
    }

    // =========================================
    // CONFIG STRUCT
    // =========================================

    #[test]
    fn test_config_stores_name() {
        let owner = Pubkey::new_unique();
        let config = AntConfig {
            mint: Pubkey::new_unique(),
            name: "My Cool ANT".to_string(),
            ticker: "COOL".to_string(),
            logo: "a".repeat(43),
            description: "Description here".to_string(),
            keywords: vec!["cool".to_string()],
            last_known_owner: owner,
            bump: 0,
            version: ANT_CONFIG_VERSION,
            #[cfg(feature = "migration-test")]
            field_1: 0,
            #[cfg(feature = "migration-test")]
            field_2: 0,
            #[cfg(feature = "migration-test")]
            field_3: false,
        };
        assert_eq!(config.name, "My Cool ANT");
        assert_eq!(config.ticker, "COOL");
        assert_eq!(config.description, "Description here");
        assert_eq!(config.keywords.len(), 1);
    }

    // =========================================
    // CONTROLLERS STRUCT
    // =========================================

    #[test]
    fn test_controllers_max_capacity() {
        let mint = Pubkey::new_unique();
        let list: Vec<Pubkey> = (0..MAX_CONTROLLERS).map(|_| Pubkey::new_unique()).collect();
        let controllers = make_controllers(mint, list.clone());
        assert_eq!(controllers.controllers.len(), MAX_CONTROLLERS);
    }

    #[test]
    fn test_controllers_empty() {
        let mint = Pubkey::new_unique();
        let controllers = make_controllers(mint, vec![]);
        assert!(controllers.controllers.is_empty());
    }

    // =========================================
    // TTL VALIDATION (Lua: utils.validateTTLSeconds)
    // =========================================

    #[test]
    fn test_ttl_at_minimum_boundary() {
        assert!(is_valid_ttl(60)); // exactly MIN_TTL_SECONDS
        assert!(!is_valid_ttl(59)); // below minimum
    }

    #[test]
    fn test_ttl_at_maximum_boundary() {
        assert!(is_valid_ttl(86400)); // exactly MAX_TTL_SECONDS
        assert!(!is_valid_ttl(86401)); // above maximum
    }

    #[test]
    fn test_ttl_valid_range() {
        assert!(is_valid_ttl(900)); // default TTL
        assert!(is_valid_ttl(3600)); // 1 hour
        assert!(is_valid_ttl(60)); // 1 minute
        assert!(is_valid_ttl(86400)); // 1 day
    }

    #[test]
    fn test_ttl_zero_invalid() {
        assert!(!is_valid_ttl(0));
    }

    #[test]
    fn test_ttl_one_second_invalid() {
        assert!(!is_valid_ttl(1));
    }

    // =========================================
    // @ RECORD PRIORITY (Lua: setRecord priority rules)
    // =========================================

    #[test]
    fn test_root_priority_none_valid() {
        // @ record: nil priority → defaults to 0 (valid)
        assert!(is_valid_priority_for_undername("@", None));
    }

    #[test]
    fn test_root_priority_zero_valid() {
        // @ record: explicit 0 is valid
        assert!(is_valid_priority_for_undername("@", Some(0)));
    }

    #[test]
    fn test_root_priority_nonzero_invalid() {
        // @ record: any non-zero priority is rejected
        assert!(!is_valid_priority_for_undername("@", Some(1)));
        assert!(!is_valid_priority_for_undername("@", Some(5)));
        assert!(!is_valid_priority_for_undername("@", Some(100)));
    }

    #[test]
    fn test_non_root_priority_any_valid() {
        // Non-@ records: any priority value is valid
        assert!(is_valid_priority_for_undername("blog", None));
        assert!(is_valid_priority_for_undername("blog", Some(0)));
        assert!(is_valid_priority_for_undername("blog", Some(1)));
        assert!(is_valid_priority_for_undername("blog", Some(999)));
    }

    // =========================================
    // NAME / DESCRIPTION / TICKER LENGTH (Lua limits)
    // =========================================

    #[test]
    fn test_name_max_length_61() {
        assert_eq!(MAX_NAME_LENGTH, 61);
        let valid = "a".repeat(61);
        assert!(valid.len() <= MAX_NAME_LENGTH);
        let too_long = "a".repeat(62);
        assert!(too_long.len() > MAX_NAME_LENGTH);
    }

    #[test]
    fn test_description_max_length() {
        // MAX_DESCRIPTION_LENGTH = 128 (tightened from 256 on 2026-05-21)
        assert_eq!(MAX_DESCRIPTION_LENGTH, 128);
        let valid = "a".repeat(128);
        assert!(valid.len() <= MAX_DESCRIPTION_LENGTH);
        let too_long = "a".repeat(129);
        assert!(too_long.len() > MAX_DESCRIPTION_LENGTH);
    }

    #[test]
    fn test_ticker_max_length_16() {
        assert_eq!(MAX_TICKER_LENGTH, 16);
    }

    // =========================================
    // ACCOUNT SIZE: AntRecord
    // =========================================

    #[test]
    fn test_ant_record_size_sufficient() {
        let expected = ANCHOR_DISCRIMINATOR_SIZE
            + PUBKEY_SIZE
            + (BORSH_LEN_PREFIX + MAX_UNDERNAME_LENGTH)
            + (BORSH_LEN_PREFIX + MAX_TARGET_LENGTH)
            + 1  // target_protocol: u8
            + 4  // ttl_seconds: u32
            + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX)   // priority: Option<u32>
            + (BORSH_OPTION_PREFIX + PUBKEY_SIZE)        // owner: Option<Pubkey>
            + PUBKEY_SIZE                                // last_reconciled_owner
            + BUMP_SIZE
            + SCHEMA_VERSION_SIZE;
        assert_eq!(AntRecord::SIZE, expected);
        assert_eq!(AntRecord::SIZE, 316);
    }

    #[test]
    fn test_ant_record_metadata_size_sufficient() {
        let expected = ANCHOR_DISCRIMINATOR_SIZE
            + PUBKEY_SIZE                                             // mint
            + PUBKEY_SIZE                                             // undername_hash
            + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX + MAX_UNDERNAME_LENGTH) // display_name
            + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX + ARWEAVE_TX_ID_LENGTH) // record_logo
            + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX + MAX_DESCRIPTION_LENGTH)
            + (BORSH_OPTION_PREFIX + BORSH_LEN_PREFIX + MAX_KEYWORDS * KEYWORD_BORSH_SIZE)
            + BUMP_SIZE
            + SCHEMA_VERSION_SIZE;
        assert_eq!(AntRecordMetadata::SIZE, expected);
        // Tightened from 744 → 436 (-308 b) on 2026-05-21 via the same
        // description (256→128) + keywords (8×32→3×32) shrinks as AntConfig.
        assert_eq!(AntRecordMetadata::SIZE, 436);
    }

    // =========================================
    // PERMISSION EDGE CASES (Lua: 3-tier extended)
    // =========================================

    #[test]
    fn test_controller_in_middle_of_list_has_permission() {
        let owner = Pubkey::new_unique();
        let c1 = Pubkey::new_unique();
        let c2 = Pubkey::new_unique();
        let c3 = Pubkey::new_unique();

        let mut config = make_config(owner);
        let mut controllers = make_controllers(config.mint, vec![c1, c2, c3]);

        // c2 is in the middle of the list
        assert!(reconcile_and_check_permission(
            &mut config,
            &mut controllers,
            &owner,
            &c2
        ));
    }

    #[test]
    fn test_removed_controller_loses_permission() {
        let owner = Pubkey::new_unique();
        let controller = Pubkey::new_unique();

        let mut config = make_config(owner);
        let mut controllers = make_controllers(config.mint, vec![controller]);

        // Remove the controller
        controllers.controllers.clear();

        assert!(!reconcile_and_check_permission(
            &mut config,
            &mut controllers,
            &owner,
            &controller
        ));
    }

    #[test]
    fn test_old_owner_loses_permission_after_transfer() {
        let old_owner = Pubkey::new_unique();
        let new_owner = Pubkey::new_unique();

        let mut config = make_config(old_owner);
        let mut controllers = make_controllers(config.mint, vec![]);

        // Old owner no longer has permission when NFT transfers
        assert!(!reconcile_and_check_permission(
            &mut config,
            &mut controllers,
            &new_owner,
            &old_owner
        ));
    }

    // =========================================
    // LOGO VALIDATION (Lua: isValidArweaveAddress)
    // =========================================

    #[test]
    fn test_logo_must_be_exactly_43_chars() {
        // Logo uses the same validation as Arweave TX IDs
        assert!(is_valid_arweave_id(&"a".repeat(43)));
        assert!(!is_valid_arweave_id(&"a".repeat(42)));
        assert!(!is_valid_arweave_id(&"a".repeat(44)));
    }

    #[test]
    fn test_default_logo_is_valid_arweave_id() {
        // Default AR.IO logo
        let default_logo = "AnYvLJTWcG9lr2Ll5MwYWZR2o5uTE39WbpYB0zCxwKM";
        assert!(is_valid_arweave_id(default_logo));
        assert_eq!(default_logo.len(), ARWEAVE_TX_ID_LENGTH);
    }

    // =========================================
    // KEYWORD EDGE CASES (additional from Lua spec)
    // =========================================

    #[test]
    fn test_keywords_empty_array_valid() {
        // Lua: validateKeywords({}) passes
        assert!(validate_keywords(&[]));
    }

    #[test]
    fn test_keywords_all_same_invalid() {
        assert!(!validate_keywords(&[
            "tag".to_string(),
            "tag".to_string(),
            "tag".to_string(),
        ]));
    }

    #[test]
    fn test_keyword_with_only_special_chars() {
        assert!(is_valid_keyword("#"));
        assert!(is_valid_keyword("@"));
        assert!(is_valid_keyword("-"));
        assert!(is_valid_keyword("_"));
        assert!(is_valid_keyword("#@-_"));
    }

    #[test]
    fn test_keyword_rejects_dot() {
        assert!(!is_valid_keyword("web3.0"));
    }

    #[test]
    fn test_keyword_rejects_slash() {
        assert!(!is_valid_keyword("a/b"));
    }

    // =========================================
    // UNDERNAME: CASE LOWERING (Lua behavior)
    // =========================================

    #[test]
    fn test_hash_undername_lowercases() {
        // Lua: name:lower() before storage
        // Our hash_undername lowercases before hashing
        assert_eq!(hash_undername("BLOG"), hash_undername("blog"));
        assert_eq!(hash_undername("Blog"), hash_undername("blog"));
        assert_eq!(hash_undername("bLoG"), hash_undername("blog"));
    }

    #[test]
    fn test_undername_rejects_dot_in_subdomain() {
        // Subdomains cannot contain dots (not part of the Lua pattern)
        assert!(!is_valid_undername("sub.domain"));
        assert!(!is_valid_undername("a.b.c"));
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
    // Metaplex Core AssetV1 Layout Tests (MPX-3)
    // =========================================

    #[test]
    fn test_read_mpl_core_owner_valid_asset() {
        // Build a minimal AssetV1 blob: Key(1) + Owner(32 bytes)
        let owner = Pubkey::new_unique();
        let mut data = vec![0u8; 33];
        data[0] = 1; // Key::AssetV1
        data[1..33].copy_from_slice(&owner.to_bytes());

        let result = crate::read_mpl_core_owner(&data).unwrap();
        assert_eq!(result, owner);
    }

    #[test]
    fn test_read_mpl_core_owner_rejects_wrong_key() {
        // Key byte 0 (Uninitialized) should be rejected
        let mut data = vec![0u8; 33];
        data[0] = 0;
        assert!(crate::read_mpl_core_owner(&data).is_err());

        // Key byte 2 (HashedAssetV1) should also be rejected
        data[0] = 2;
        assert!(crate::read_mpl_core_owner(&data).is_err());
    }

    #[test]
    fn test_read_mpl_core_owner_rejects_short_data() {
        // 32 bytes is not enough (need at least 33: 1 key + 32 owner)
        let data = vec![1u8; 32];
        assert!(crate::read_mpl_core_owner(&data).is_err());

        // Empty data
        assert!(crate::read_mpl_core_owner(&[]).is_err());
    }

    #[test]
    fn test_read_mpl_core_owner_with_trailing_data() {
        // Real assets have data beyond byte 33 (UpdateAuthority, etc.)
        // Verify we extract the correct owner regardless of trailing data
        let owner = Pubkey::new_unique();
        let mut data = vec![0xFFu8; 200]; // simulate full asset account
        data[0] = 1; // Key::AssetV1
        data[1..33].copy_from_slice(&owner.to_bytes());

        let result = crate::read_mpl_core_owner(&data).unwrap();
        assert_eq!(result, owner);
    }
}
