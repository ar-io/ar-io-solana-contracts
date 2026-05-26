use anchor_lang::prelude::*;

declare_id!("ARioAntProgXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX");

pub mod acl;
pub mod error;
pub mod migration;
pub mod mpl_core_cpi;
pub mod schema_migration;
pub mod state;

pub use acl::*;
use error::AntError;
pub use migration::*;
use state::*;

/// AR.IO ANT (Arweave Name Token) Program
///
/// Each ANT is a Metaplex Core NFT. Ownership = NFT holder.
/// Extended state (records, controllers, metadata) stored in PDAs.
///
/// Handles:
/// - ANT initialization (create config + controllers + @ record)
/// - Record management (set, remove, transfer records)
/// - Controller management (add, remove)
/// - Metadata management (name, ticker, description, keywords, logo)
///
/// Transfer is handled natively by Metaplex Core (marketplace compatible).
/// Lazy ownership reconciliation clears controllers on detected ownership change.
#[program]
pub mod ario_ant {
    use super::*;

    // =========================================
    // INITIALIZATION
    // =========================================

    /// Initialize an ANT's on-chain state (config, controllers, @ record).
    /// Called after minting a Metaplex Core asset.
    pub fn initialize(ctx: Context<InitializeAnt>, params: InitializeAntParams) -> Result<()> {
        // Verify caller is the actual NFT holder
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data); // Release borrow before mutating other accounts
        require!(
            ctx.accounts.owner.key() == nft_owner,
            AntError::NotNftHolder
        );

        // Validate inputs
        require!(!params.name.is_empty(), AntError::NameEmpty);
        require!(params.name.len() <= MAX_NAME_LENGTH, AntError::NameTooLong);
        if let Some(ref ticker) = params.ticker {
            require!(
                !ticker.is_empty() && ticker.len() <= MAX_TICKER_LENGTH,
                AntError::TickerTooLong
            );
        }
        let target_protocol = params.target_protocol.unwrap_or(PROTOCOL_ARWEAVE);
        require!(
            validate_target(&params.target, target_protocol),
            AntError::InvalidTarget
        );
        if !params.logo.is_empty() {
            require!(is_valid_arweave_id(&params.logo), AntError::InvalidLogo);
        }
        if !params.description.is_empty() {
            require!(
                params.description.len() <= MAX_DESCRIPTION_LENGTH,
                AntError::DescriptionTooLong
            );
        }
        if !params.keywords.is_empty() {
            require!(
                validate_keywords(&params.keywords),
                AntError::InvalidKeyword
            );
        }

        let owner = ctx.accounts.owner.key();

        // Init config
        let config = &mut ctx.accounts.ant_config;
        config.mint = ctx.accounts.asset.key();
        config.name = params.name;
        config.ticker = params.ticker.unwrap_or_else(|| "ANT".to_string());
        config.logo = if params.logo.is_empty() {
            // Default AR.IO logo
            "AnYvLJTWcG9lr2Ll5MwYWZR2o5uTE39WbpYB0zCxwKM".to_string()
        } else {
            params.logo
        };
        config.description = params.description;
        config.keywords = params.keywords;
        config.last_known_owner = owner;
        config.bump = ctx.bumps.ant_config;
        config.version = ANT_CONFIG_VERSION;

        // Init controllers with owner as first controller (matches Lua default)
        let controllers = &mut ctx.accounts.ant_controllers;
        controllers.mint = ctx.accounts.asset.key();
        controllers.controllers = vec![owner];
        controllers.bump = ctx.bumps.ant_controllers;
        controllers.version = ANT_CONTROLLERS_VERSION;

        // Init @ record
        let root_record = &mut ctx.accounts.root_record;
        root_record.mint = ctx.accounts.asset.key();
        root_record.undername = "@".to_string();
        root_record.target = params.target;
        root_record.target_protocol = target_protocol;
        root_record.ttl_seconds = DEFAULT_TTL_SECONDS;
        root_record.priority = Some(0);
        root_record.owner = None;
        root_record.last_reconciled_owner = owner;
        root_record.bump = ctx.bumps.root_record;
        root_record.version = ANT_RECORD_VERSION;

        msg!("ANT initialized for asset {}", ctx.accounts.asset.key());
        Ok(())
    }

    // =========================================
    // RECORD MANAGEMENT
    // =========================================

    /// Set (create or update) a record for an undername.
    /// New records: owner/controller only.
    /// Existing records: owner/controller or record owner (without priority change).
    pub fn set_record(ctx: Context<SetRecord>, params: SetRecordParams) -> Result<()> {
        let undername = params.undername.to_lowercase();
        require!(is_valid_undername(&undername), AntError::InvalidUndername);
        require!(
            validate_target(&params.target, params.target_protocol),
            AntError::InvalidTarget
        );
        require!(is_valid_ttl(params.ttl_seconds), AntError::InvalidTtl);

        // Get current NFT owner from Metaplex Core asset
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        let is_owner_or_controller =
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller);

        let record = &mut ctx.accounts.record;
        let is_new = record.undername.is_empty();

        // H-7: Clear stale record owners on NFT transfer
        if !is_new && record.last_reconciled_owner != config.last_known_owner {
            record.owner = None;
            record.last_reconciled_owner = config.last_known_owner;
        }

        if is_new {
            // Only owner/controllers can create new records
            require!(
                is_owner_or_controller,
                AntError::OnlyOwnerOrControllerCanCreate
            );

            record.mint = ctx.accounts.asset.key();
            record.undername = undername.clone();
            record.last_reconciled_owner = config.last_known_owner;
            record.bump = ctx.bumps.record;
            record.version = ANT_RECORD_VERSION;
        } else {
            // Existing record - check permission
            let has_perm = is_owner_or_controller || record.owner.map_or(false, |ro| ro == caller);
            require!(has_perm, AntError::UnauthorizedRecordAccess);
        }

        // Priority rules
        if undername == "@" {
            // @ record must always have priority 0
            require!(
                is_valid_priority_for_undername("@", params.priority),
                AntError::CannotChangePriorityOfRoot
            );
            record.priority = Some(0);
        } else if params.priority.is_some() {
            // Only owner/controllers can set priority
            require!(
                is_owner_or_controller,
                AntError::PriorityRequiresOwnerOrController
            );
            record.priority = params.priority;
        }

        // Record owner assignment - only owner/controllers can set or clear
        if is_owner_or_controller {
            record.owner = params.record_owner;
        }

        // Core fields: modifiable by owner, controllers, OR record owner.
        // Record owners can update content pointers and TTL for their assigned records.
        // Only priority (above) and record.owner assignment are restricted to owner/controllers.
        record.target = params.target;
        record.target_protocol = params.target_protocol;
        record.ttl_seconds = params.ttl_seconds;

        emit!(RecordSetEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            undername: undername.clone(),
            target: record.target.clone(),
            target_protocol: record.target_protocol,
            ttl_seconds: record.ttl_seconds,
            // 0 sentinel covers both the @ root (priority always Some(0))
            // and the "no priority assigned" case for non-root records.
            // Indexers reading `record.priority == None` can treat 0 as
            // unset; ANT-internal logic uses `Option<u32>` directly.
            priority: record.priority.unwrap_or(0),
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("Record '{}' set", undername);
        Ok(())
    }

    /// Remove a record (owner/controllers only). Cannot remove the @ record.
    pub fn remove_record(ctx: Context<RemoveRecord>) -> Result<()> {
        // Cannot remove the root @ record
        require!(
            ctx.accounts.record.undername != "@",
            AntError::CannotRemoveRootRecord
        );

        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        require!(
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller),
            AntError::Unauthorized
        );

        emit!(RecordRemovedEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            undername: ctx.accounts.record.undername.clone(),
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("Record '{}' removed", ctx.accounts.record.undername);
        // Account closed by close constraint
        Ok(())
    }

    /// Transfer record ownership to another address.
    pub fn transfer_record(ctx: Context<TransferRecord>, new_owner: Pubkey) -> Result<()> {
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;
        let record = &mut ctx.accounts.record;

        // Reconcile config/controllers first (updates config.last_known_owner)
        let is_owner_or_controller =
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller);

        // H-7: Clear stale record owners on NFT transfer
        if record.last_reconciled_owner != config.last_known_owner {
            record.owner = None;
            record.last_reconciled_owner = config.last_known_owner;
        }

        if is_owner_or_controller {
            // Owner/controller can always (re)assign record ownership,
            // even when record.owner is None after reconciliation (ANT-017).
            require!(
                record.owner != Some(new_owner),
                AntError::RecordTransferToSelf
            );
        } else {
            // Non-owner/non-controller: record must have an owner and caller must be that owner
            require!(record.owner.is_some(), AntError::RecordHasNoOwner);
            require!(
                record.owner.unwrap() == caller,
                AntError::UnauthorizedRecordAccess
            );
            require!(
                record.owner.unwrap() != new_owner,
                AntError::RecordTransferToSelf
            );
        }

        let previous_owner = record.owner;
        record.owner = Some(new_owner);

        emit!(RecordTransferredEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            undername: record.undername.clone(),
            previous_owner,
            new_owner,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!(
            "Record '{}' transferred from {:?} to {}",
            record.undername,
            previous_owner,
            new_owner
        );
        Ok(())
    }

    // =========================================
    // CONTROLLER MANAGEMENT
    // =========================================

    /// Add a controller address.
    ///
    /// The controller's per-user paginated ACL (ADR-012) is updated
    /// inline — `controller_acl_config` and `controller_acl_page` are
    /// required accounts, so it is impossible to add a controller
    /// without also writing the matching `record_controller` ACL entry.
    /// The SDK preflights `register_acl_config` + `add_acl_page` ixs in
    /// the same tx if the controller has no head/page yet (or every
    /// page is full). The contract is the source of truth for "ANT
    /// controller add ⇒ ACL entry."
    pub fn add_controller(ctx: Context<AddController>, controller: Pubkey) -> Result<()> {
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data);

        let caller = ctx.accounts.caller.key();

        require!(
            reconcile_and_check_permission(
                &mut ctx.accounts.ant_config,
                &mut ctx.accounts.ant_controllers,
                &nft_owner,
                &caller,
            ),
            AntError::Unauthorized
        );

        let controllers = &mut ctx.accounts.ant_controllers;
        require!(
            !controllers.controllers.contains(&controller),
            AntError::ControllerAlreadyExists
        );
        require!(
            controllers.controllers.len() < MAX_CONTROLLERS,
            AntError::MaxControllersReached
        );

        controllers.controllers.push(controller);

        // ACL: record this controller's relationship to this asset.
        // Same checks as `record_acl_controller_handler`, run inline so
        // there is no window where AntControllers ≠ ACL state.
        require!(
            ctx.accounts.controller_acl_config.user == controller,
            AntError::AclConfigUserMismatch
        );
        acl::assert_page_belongs(
            &ctx.accounts.controller_acl_config,
            &ctx.accounts.controller_acl_page,
        )?;

        let asset_key = ctx.accounts.asset.key();
        acl::page_push_unique(
            &mut ctx.accounts.controller_acl_page,
            asset_key,
            AclRole::Controller as u8,
        )?;
        acl::resync_page_size(
            &ctx.accounts.controller_acl_page,
            &ctx.accounts.caller,
            &ctx.accounts.system_program,
        )?;

        let acl_cfg = &mut ctx.accounts.controller_acl_config;
        acl_cfg.total_entries = acl_cfg.total_entries.saturating_add(1);

        emit!(ControllerAddedEvent {
            mint: ctx.accounts.asset.key(),
            // `nft_owner` reflects the post-reconcile asset holder, which
            // matches `AntConfig.last_known_owner` after the permission
            // check ran. Indexers can use it to scope a "controller added
            // by owner X" view without re-fetching the asset.
            owner: nft_owner,
            controller,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!(
            "Controller {} added (ACL: page={}, total_entries={})",
            controller,
            ctx.accounts.controller_acl_page.page_idx,
            acl_cfg.total_entries
        );
        Ok(())
    }

    /// Remove a controller address.
    ///
    /// Symmetric to `add_controller` — `controller_acl_config` and
    /// `controller_acl_page` (containing the live entry) are required,
    /// so the ACL entry is removed in the same transaction as the
    /// `AntControllers` mutation. Pages are not realloc-shrunk on
    /// remove (see ADR-012 / `remove_acl_*_handler`); rent-recovery
    /// happens lazily via `close_acl_page` / `close_acl_config`.
    pub fn remove_controller(ctx: Context<RemoveController>, controller: Pubkey) -> Result<()> {
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data);

        let caller = ctx.accounts.caller.key();

        require!(
            reconcile_and_check_permission(
                &mut ctx.accounts.ant_config,
                &mut ctx.accounts.ant_controllers,
                &nft_owner,
                &caller,
            ),
            AntError::Unauthorized
        );

        let controllers = &mut ctx.accounts.ant_controllers;
        let pos = controllers
            .controllers
            .iter()
            .position(|c| *c == controller);
        require!(pos.is_some(), AntError::ControllerNotFound);
        controllers.controllers.remove(pos.unwrap());

        // ACL: remove the matching entry. Validates that the supplied
        // page actually carries the (asset, controller) row — caller
        // must have used the registry's preflight to find it.
        require!(
            ctx.accounts.controller_acl_config.user == controller,
            AntError::AclConfigUserMismatch
        );
        acl::assert_page_belongs(
            &ctx.accounts.controller_acl_config,
            &ctx.accounts.controller_acl_page,
        )?;

        let asset_key = ctx.accounts.asset.key();
        acl::page_swap_remove(
            &mut ctx.accounts.controller_acl_page,
            &asset_key,
            AclRole::Controller as u8,
        )?;

        let acl_cfg = &mut ctx.accounts.controller_acl_config;
        acl_cfg.total_entries = acl_cfg.total_entries.saturating_sub(1);

        emit!(ControllerRemovedEvent {
            mint: ctx.accounts.asset.key(),
            owner: nft_owner,
            controller,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!(
            "Controller {} removed (ACL: page={}, total_entries={})",
            controller,
            ctx.accounts.controller_acl_page.page_idx,
            acl_cfg.total_entries
        );
        Ok(())
    }

    // =========================================
    // METADATA MANAGEMENT
    // =========================================

    /// Set ANT display name.
    pub fn set_name(ctx: Context<ManageMetadata>, name: String) -> Result<()> {
        require!(!name.is_empty(), AntError::NameEmpty);
        require!(name.len() <= MAX_NAME_LENGTH, AntError::NameTooLong);

        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        require!(
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller),
            AntError::Unauthorized
        );

        config.name = name.clone();

        emit!(AntMetadataUpdatedEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            field: ANT_METADATA_FIELD_NAME,
            new_value: name.clone(),
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("ANT name set to '{}'", name);
        Ok(())
    }

    /// Set ANT ticker.
    pub fn set_ticker(ctx: Context<ManageMetadata>, ticker: String) -> Result<()> {
        require!(!ticker.is_empty(), AntError::TickerEmpty);
        require!(ticker.len() <= MAX_TICKER_LENGTH, AntError::TickerTooLong);

        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        require!(
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller),
            AntError::Unauthorized
        );

        config.ticker = ticker.clone();

        emit!(AntMetadataUpdatedEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            field: ANT_METADATA_FIELD_TICKER,
            new_value: ticker.clone(),
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("ANT ticker set to '{}'", ticker);
        Ok(())
    }

    /// Set ANT description.
    pub fn set_description(ctx: Context<ManageMetadata>, description: String) -> Result<()> {
        require!(
            description.len() <= MAX_DESCRIPTION_LENGTH,
            AntError::DescriptionTooLong
        );

        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        require!(
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller),
            AntError::Unauthorized
        );

        let new_value = description.clone();
        config.description = description;

        emit!(AntMetadataUpdatedEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            field: ANT_METADATA_FIELD_DESCRIPTION,
            new_value,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("ANT description updated");
        Ok(())
    }

    /// Set ANT keywords.
    pub fn set_keywords(ctx: Context<ManageMetadata>, keywords: Vec<String>) -> Result<()> {
        require!(validate_keywords(&keywords), AntError::InvalidKeyword);

        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        require!(
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller),
            AntError::Unauthorized
        );

        // Encode keywords as a comma-separated string for the event payload —
        // the canonical typed value lives on the AntConfig PDA.
        let new_value = keywords.join(",");
        config.keywords = keywords;

        emit!(AntMetadataUpdatedEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            field: ANT_METADATA_FIELD_KEYWORDS,
            new_value,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("ANT keywords updated");
        Ok(())
    }

    /// Set ANT logo.
    pub fn set_logo(ctx: Context<ManageMetadata>, logo: String) -> Result<()> {
        require!(is_valid_arweave_id(&logo), AntError::InvalidLogo);

        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        require!(
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller),
            AntError::Unauthorized
        );

        let new_value = logo.clone();
        config.logo = logo;

        emit!(AntMetadataUpdatedEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            field: ANT_METADATA_FIELD_LOGO,
            new_value,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("ANT logo updated");
        Ok(())
    }

    // =========================================
    // RECONCILIATION
    // =========================================

    // =========================================
    // MIGRATION OPERATIONS
    // =========================================

    /// Initialize migration config singleton for the ANT program
    pub fn initialize_migration(
        ctx: Context<InitializeAntMigration>,
        params: InitializeAntMigrationParams,
    ) -> Result<()> {
        initialize_migration_handler(ctx, params)
    }

    /// Import a pre-serialized account during migration
    pub fn import_account(
        ctx: Context<ImportAccount>,
        seeds: Vec<Vec<u8>>,
        data: Vec<u8>,
    ) -> Result<()> {
        import_account_handler(ctx, seeds, data)
    }

    /// Permanently disable migration imports (main authority only)
    pub fn finalize_migration(ctx: Context<FinalizeMigration>) -> Result<()> {
        finalize_migration_handler(ctx)
    }

    // =========================================
    // ANT MIGRATION (per-ANT schema upgrade)
    // =========================================

    /// Migrate an ANT's `AntConfig` and `AntControllers` to the latest schema
    /// versions. Permissionless — anyone can pay the realloc rent. The
    /// `schema_migration` dispatch functions step through every intermediate
    /// version in order, so a single call handles any version gap.
    ///
    /// Per-undername `AntRecord` and `AntRecordMetadata` accounts have their
    /// own instructions (`migrate_ant_record`, `migrate_ant_record_metadata`)
    /// because their cardinality is unbounded and cannot fit in a single tx.
    pub fn migrate_ant(ctx: Context<AntMigration>) -> Result<()> {
        let asset_key = ctx.accounts.asset.key();

        let config = &mut ctx.accounts.ant_config;
        require!(
            config.version < ANT_CONFIG_VERSION,
            AntError::AlreadyLatestVersion
        );
        schema_migration::migrate_config_version(config)?;
        msg!(
            "ANT {} config migrated to {}.{}.{}",
            asset_key,
            config.version.major,
            config.version.minor,
            config.version.patch,
        );

        let controllers = &mut ctx.accounts.ant_controllers;
        if controllers.version < ANT_CONTROLLERS_VERSION {
            schema_migration::migrate_controllers_version(controllers)?;
            msg!(
                "ANT {} controllers migrated to {}.{}.{}",
                asset_key,
                controllers.version.major,
                controllers.version.minor,
                controllers.version.patch,
            );
        }

        Ok(())
    }

    /// Migrate a single `AntRecord` PDA to the latest schema version.
    /// Permissionless — anyone can pay. Call once per undername that needs
    /// migrating; callers can derive which records exist via `getProgramAccounts`
    /// filtered on `ANT_RECORD_SEED`.
    pub fn migrate_ant_record(ctx: Context<AntMigrationRecord>, undername: String) -> Result<()> {
        let record = &mut ctx.accounts.record;
        require!(
            record.version < ANT_RECORD_VERSION,
            AntError::AlreadyLatestVersion
        );
        schema_migration::migrate_record_version(record)?;
        msg!(
            "ANT {} record '{}' migrated to {}.{}.{}",
            ctx.accounts.asset.key(),
            undername,
            record.version.major,
            record.version.minor,
            record.version.patch,
        );
        Ok(())
    }

    /// Migrate a single `AntRecordMetadata` PDA to the latest schema version.
    /// Permissionless — anyone can pay. Only needed for records that have an
    /// existing metadata PDA; the `undername` is the same one passed to
    /// `migrate_ant_record`.
    pub fn migrate_ant_record_metadata(
        ctx: Context<AntMigrationRecordMetadata>,
        undername: String,
    ) -> Result<()> {
        let metadata = &mut ctx.accounts.record_metadata;
        require!(
            metadata.version < ANT_RECORD_METADATA_VERSION,
            AntError::AlreadyLatestVersion
        );
        schema_migration::migrate_record_metadata_version(metadata)?;
        msg!(
            "ANT {} record metadata '{}' migrated to {}.{}.{}",
            ctx.accounts.asset.key(),
            undername,
            metadata.version.major,
            metadata.version.minor,
            metadata.version.patch,
        );
        Ok(())
    }

    // =========================================
    // SCHEMA MIGRATION — AntMigrationConfig, AclConfig, AclPage
    // =========================================

    /// Migrate the singleton `AntMigrationConfig` account to the latest schema.
    pub fn migrate_ant_migration_config(ctx: Context<AntMigrationConfigMigration>) -> Result<()> {
        let config = &mut ctx.accounts.migration_config;
        require!(
            config.version < ANT_MIGRATION_CONFIG_VERSION,
            AntError::AlreadyLatestVersion
        );
        schema_migration::migrate_migration_config_version(config)?;
        msg!(
            "AntMigrationConfig migrated to {}.{}.{}",
            config.version.major,
            config.version.minor,
            config.version.patch,
        );
        Ok(())
    }

    /// Migrate a user's `AclConfig` account to the latest schema.
    pub fn migrate_acl_config(ctx: Context<AclConfigMigration>) -> Result<()> {
        let config = &mut ctx.accounts.acl_config;
        require!(
            config.version < ACL_CONFIG_VERSION,
            AntError::AlreadyLatestVersion
        );
        schema_migration::migrate_acl_config_version(config)?;
        msg!(
            "AclConfig for {} migrated to {}.{}.{}",
            config.user,
            config.version.major,
            config.version.minor,
            config.version.patch,
        );
        Ok(())
    }

    /// Migrate a user's `AclPage` account to the latest schema.
    pub fn migrate_acl_page(ctx: Context<AclPageMigration>, page_idx: u64) -> Result<()> {
        let page = &mut ctx.accounts.acl_page;
        require!(
            page.version < ACL_PAGE_VERSION,
            AntError::AlreadyLatestVersion
        );
        schema_migration::migrate_acl_page_version(page)?;
        msg!(
            "AclPage {} for {} migrated to {}.{}.{}",
            page_idx,
            page.user,
            page.version.major,
            page.version.minor,
            page.version.patch,
        );
        Ok(())
    }

    // =========================================
    // TRANSFER (wrapped MPL Core transferV1 + reconcile + owner ACL swap)
    // =========================================

    /// Transfer the ANT to a new owner.
    ///
    /// This is a wrapped MPL Core `transferV1` CPI that bundles in the
    /// post-conditions our protocol needs:
    ///   1. CPI to MPL Core to actually move the asset.
    ///   2. Inline reconcile — clears `AntControllers` and updates
    ///      `AntConfig.last_known_owner = new_owner`.
    ///   3. Inline ACL swap — `record_owner(new_owner)` +
    ///      `remove_owner(old_owner)`.
    ///
    /// Both ACL configs/pages are required accounts so it is not
    /// possible to transfer through this path without keeping the
    /// per-owner ACL view in sync. Ex-controllers' ACL entries are NOT
    /// touched here (variable cardinality is incompatible with strong
    /// Codama account typing); the SDK opportunistically bundles
    /// `remove_acl_controller` for each, and the standalone healing ix
    /// can be called permissionlessly to clean up later.
    ///
    /// Direct MPL Core `transferV1` calls (e.g., from marketplaces that
    /// do not know about ARIO) leave both `AntControllers` and the ACL
    /// stale — the permissionless `reconcile` + `remove_acl_owner`
    /// instructions exist exactly for that fallback path.
    pub fn transfer(ctx: Context<Transfer>) -> Result<()> {
        let asset_key = ctx.accounts.asset.key();
        let caller_key = ctx.accounts.caller.key();
        let new_owner_key = ctx.accounts.new_owner.key();

        // Validate the caller is the current MPL Core owner. We do this
        // before the CPI both to short-circuit and to avoid trusting MPL
        // Core's error path for protocol-level invariants.
        let old_owner = {
            let asset_data = ctx.accounts.asset.try_borrow_data()?;
            read_mpl_core_owner(&asset_data)?
        };
        require!(caller_key == old_owner, AntError::NotNftHolder);
        require!(new_owner_key != old_owner, AntError::TransferToSelf);

        // ACL configs must belong to the right parties.
        require!(
            ctx.accounts.new_owner_acl_config.user == new_owner_key,
            AntError::AclConfigUserMismatch
        );
        require!(
            ctx.accounts.old_owner_acl_config.user == caller_key,
            AntError::AclConfigUserMismatch
        );
        acl::assert_page_belongs(
            &ctx.accounts.new_owner_acl_config,
            &ctx.accounts.new_owner_acl_page,
        )?;
        acl::assert_page_belongs(
            &ctx.accounts.old_owner_acl_config,
            &ctx.accounts.old_owner_acl_page,
        )?;

        // CPI into MPL Core transferV1.
        //
        // Account order (per mpl-core v1.x transferV1):
        //   0: asset (writable)
        //   1: collection (optional — MPL Core program ID = "None")
        //   2: payer (writable signer)
        //   3: authority (signer; MPL Core program ID = "use payer")
        //   4: newOwner (read-only)
        //   5: systemProgram (optional)
        //   6: logWrapper (optional — MPL Core program ID = "None")
        //
        // Data: u8 discriminator (14) + Option<CompressionProof> = None (0).
        //
        // We do not depend on the mpl-core crate to keep the build matrix
        // small and to avoid coupling our toolchain to Metaplex's Solana
        // version pins. The layout is exercised by integration tests
        // and pinned in `read_mpl_core_owner` — any layout change would
        // be caught there first.
        let mpl_ix = anchor_lang::solana_program::instruction::Instruction {
            program_id: MPL_CORE_PROGRAM_ID,
            accounts: vec![
                anchor_lang::solana_program::instruction::AccountMeta::new(asset_key, false),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    MPL_CORE_PROGRAM_ID,
                    false,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new(caller_key, true),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    caller_key, true,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    new_owner_key,
                    false,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    anchor_lang::solana_program::system_program::ID,
                    false,
                ),
                anchor_lang::solana_program::instruction::AccountMeta::new_readonly(
                    MPL_CORE_PROGRAM_ID,
                    false,
                ),
            ],
            data: vec![14u8, 0u8],
        };
        anchor_lang::solana_program::program::invoke(
            &mpl_ix,
            &[
                ctx.accounts.asset.to_account_info(),
                ctx.accounts.mpl_core_program.to_account_info(),
                ctx.accounts.caller.to_account_info(),
                ctx.accounts.caller.to_account_info(),
                ctx.accounts.new_owner.to_account_info(),
                ctx.accounts.system_program.to_account_info(),
                ctx.accounts.mpl_core_program.to_account_info(),
            ],
        )?;

        // Inline reconcile — controllers are now invalid.
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;
        controllers.controllers.clear();
        config.last_known_owner = new_owner_key;

        // ACL: record_owner(new_owner)
        acl::page_push_unique(
            &mut ctx.accounts.new_owner_acl_page,
            asset_key,
            AclRole::Owner as u8,
        )?;
        acl::resync_page_size(
            &ctx.accounts.new_owner_acl_page,
            &ctx.accounts.caller,
            &ctx.accounts.system_program,
        )?;
        let new_acl_cfg = &mut ctx.accounts.new_owner_acl_config;
        new_acl_cfg.total_entries = new_acl_cfg.total_entries.saturating_add(1);

        // ACL: remove_owner(old_owner). Pages stay sparse on remove —
        // density is restored on next append (see ADR-012).
        acl::page_swap_remove(
            &mut ctx.accounts.old_owner_acl_page,
            &asset_key,
            AclRole::Owner as u8,
        )?;
        let old_acl_cfg = &mut ctx.accounts.old_owner_acl_config;
        old_acl_cfg.total_entries = old_acl_cfg.total_entries.saturating_sub(1);

        emit!(AntTransferredEvent {
            mint: asset_key,
            from: old_owner,
            to: new_owner_key,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!(
            "ANT {} transferred {} -> {} (controllers cleared, owner ACL synced)",
            asset_key,
            old_owner,
            new_owner_key,
        );
        Ok(())
    }

    // =========================================
    // RECONCILIATION (fallback for out-of-band MPL Core transfers)
    // =========================================

    /// Force ownership reconciliation — clears controllers if NFT ownership changed.
    /// Call this after a marketplace transfer to ensure clean state.
    /// Permissionless: anyone can trigger reconciliation (it only helps the new owner).
    pub fn reconcile(ctx: Context<Reconcile>) -> Result<()> {
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;

        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        if config.last_known_owner != nft_owner {
            let previous_owner = config.last_known_owner;
            controllers.controllers.clear();
            config.last_known_owner = nft_owner;

            // Emit ONLY when state actually changed — no-op reconciles
            // do not spam the indexer activity feed.
            emit!(AntReconciledEvent {
                mint: ctx.accounts.asset.key(),
                previous_owner,
                new_owner: nft_owner,
                controllers_cleared: true,
                timestamp: Clock::get()?.unix_timestamp,
            });

            msg!(
                "Ownership reconciled — controllers cleared for new owner {}",
                nft_owner
            );
        } else {
            msg!("No ownership change detected");
        }

        Ok(())
    }

    // =========================================
    // RECORD METADATA (separate PDA for optional per-record fields)
    // =========================================

    /// Set (create or update) metadata for a record's undername.
    /// Metadata is stored in a separate PDA to save rent on records that
    /// don't use display_name/logo/description/keywords.
    pub fn set_record_metadata(
        ctx: Context<SetRecordMetadata>,
        params: SetRecordMetadataParams,
    ) -> Result<()> {
        let undername = params.undername.to_lowercase();
        require!(is_valid_undername(&undername), AntError::InvalidUndername);

        // Validate metadata fields
        if let Some(ref dn) = params.display_name {
            require!(dn.len() <= MAX_NAME_LENGTH, AntError::NameTooLong);
        }
        if let Some(ref logo) = params.record_logo {
            require!(is_valid_arweave_id(logo), AntError::InvalidLogo);
        }
        if let Some(ref desc) = params.record_description {
            require!(
                desc.len() <= MAX_DESCRIPTION_LENGTH,
                AntError::DescriptionTooLong
            );
        }
        if let Some(ref kws) = params.record_keywords {
            require!(validate_keywords(kws), AntError::InvalidKeyword);
        }

        // Get current NFT owner
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data);

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        let is_owner_or_controller =
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller);

        // The core record must exist
        let record = &ctx.accounts.record;
        require!(!record.undername.is_empty(), AntError::RecordNotFound);

        // H-7: Check stale record owner
        let record_owner_valid = record.last_reconciled_owner == config.last_known_owner;
        let has_perm = is_owner_or_controller
            || (record_owner_valid && record.owner.map_or(false, |ro| ro == caller));
        require!(has_perm, AntError::UnauthorizedRecordAccess);

        let meta = &mut ctx.accounts.record_metadata;
        meta.mint = ctx.accounts.asset.key();
        meta.undername_hash = hash_undername(&undername);
        meta.display_name = params.display_name;
        meta.record_logo = params.record_logo;
        meta.record_description = params.record_description;
        meta.record_keywords = params.record_keywords;
        meta.bump = ctx.bumps.record_metadata;
        meta.version = ANT_RECORD_METADATA_VERSION;

        emit!(RecordMetadataUpdatedEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            undername: undername.clone(),
            // `set_record_metadata` writes every field in one ix; the
            // discriminator is reserved so future granular setters can
            // emit specific values without mutating the event shape.
            field: RECORD_METADATA_FIELD_ALL,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("Record metadata for '{}' set", undername);
        Ok(())
    }

    /// Remove metadata for a record. Owner/controllers only.
    /// Rent is returned to the caller.
    pub fn remove_record_metadata(
        ctx: Context<RemoveRecordMetadata>,
        undername: String,
    ) -> Result<()> {
        let undername = undername.to_lowercase();
        require!(is_valid_undername(&undername), AntError::InvalidUndername);

        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data);

        let caller = ctx.accounts.caller.key();
        let config = &mut ctx.accounts.ant_config;
        let controllers = &mut ctx.accounts.ant_controllers;

        require!(
            reconcile_and_check_permission(config, controllers, &nft_owner, &caller),
            AntError::Unauthorized
        );

        emit!(RecordMetadataRemovedEvent {
            mint: ctx.accounts.asset.key(),
            caller,
            undername: undername.clone(),
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("Record metadata for '{}' removed", undername);
        // Account closed by close constraint
        Ok(())
    }

    /// Permissionlessly close an orphaned `AntRecordMetadata` PDA whose
    /// corresponding `AntRecord` no longer exists. Rent flows to the caller as
    /// a cleanup incentive.
    ///
    /// `RemoveRecord` accepts metadata as `Option`, so a forgetful caller can
    /// close the record without closing its sibling metadata, leaving rent
    /// trapped at the orphan PDA. This instruction recovers that rent without
    /// requiring owner/controller authorization — the only precondition is
    /// that the record is gone (closed by Anchor → owner reassigned to the
    /// System Program and data drained).
    pub fn close_orphaned_record_metadata(
        ctx: Context<CloseOrphanedRecordMetadata>,
        undername: String,
    ) -> Result<()> {
        let undername = undername.to_lowercase();
        require!(is_valid_undername(&undername), AntError::InvalidUndername);

        // The record PDA must be in the post-close state: System-Program-owned
        // AND zero data. Either condition being false (data present, or owner
        // still ario-ant) means the record is still live.
        let record = &ctx.accounts.record;
        require!(
            record.data_is_empty() && record.owner == &anchor_lang::system_program::ID,
            AntError::RecordStillExists
        );

        emit!(RecordMetadataPrunedEvent {
            mint: ctx.accounts.asset.key(),
            undername: undername.clone(),
            pruner: ctx.accounts.caller.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!("Orphaned record metadata for '{}' closed", undername);
        Ok(())
    }

    // =========================================
    // ACL REGISTRY (ADR-012 — paginated per-user ACL)
    // =========================================
    //
    // Per-user paginated reverse index. `AclConfig(user)` is the head and
    // tracks `page_count` + `total_entries`. `AclPage(user, page_idx)` is a
    // CDPDA whose seeds include the 8-byte LE `u64` page index, so any
    // single page is fetchable in O(1). Frontends read the head once and
    // fan out to `[0..page_count)` page PDAs via `getMultipleAccountsInfo`
    // — no `getProgramAccounts`, no DAS provider, no foundation indexer.
    //
    // All instructions are permissionless and verify the on-chain
    // relationship before mutating the ACL. The SDK bundles
    // `record_acl_*` / `remove_acl_*` alongside the operations that drift
    // the relationship (`add_controller`, `remove_controller`, transfer,
    // spawn) so callers that opt in get atomic ACL maintenance per-tx.
    // Callers that don't bundle still write correct canonical state — only
    // the reverse index drifts, and the cleanup instructions can heal it
    // permissionlessly.
    //
    // See `docs/DECISIONS.md#adr-012-paginated-per-user-ant-acl` for the
    // full design and `docs/ACCOUNT_SCALING_PATTERNS.md` Pattern C for the
    // taxonomy this implements.

    /// Initialize an `AclConfig` head for `user`. Permissionless. Fails at
    /// the `init` constraint if the head already exists.
    pub fn register_acl_config(ctx: Context<RegisterAclConfig>, user: Pubkey) -> Result<()> {
        register_acl_config_handler(ctx, user)
    }

    /// Allocate the next `AclPage` (`page_idx == acl_config.page_count`).
    /// SDK calls this lazily when the most recent page fills up.
    pub fn add_acl_page(ctx: Context<AddAclPage>) -> Result<()> {
        add_acl_page_handler(ctx)
    }

    /// Record that `acl_config.user` owns `asset`. Verifies via the MPL
    /// Core asset account that the user really is the current NFT holder.
    /// Caller picks the target page; SDK convention is "first non-full".
    pub fn record_acl_owner(ctx: Context<RecordAclOwner>) -> Result<()> {
        record_acl_owner_handler(ctx)
    }

    /// Record that `acl_config.user` is a controller of `asset`. Verifies
    /// via `AntControllers` that the user is currently listed.
    pub fn record_acl_controller(ctx: Context<RecordAclController>) -> Result<()> {
        record_acl_controller_handler(ctx)
    }

    /// Remove a stale owner entry. Verifies via the MPL Core asset that
    /// `acl_config.user` is *no longer* the owner — never removes a live
    /// entry. Uses `swap_remove`; pages stay sparse until next append.
    pub fn remove_acl_owner(ctx: Context<RecordAclOwner>) -> Result<()> {
        remove_acl_owner_handler(ctx)
    }

    /// Remove a stale controller entry. Verifies via `AntControllers` that
    /// `acl_config.user` is *no longer* listed.
    pub fn remove_acl_controller(ctx: Context<RecordAclController>) -> Result<()> {
        remove_acl_controller_handler(ctx)
    }

    /// Close the most recently allocated `AclPage` when it is empty.
    /// Pages must be closed in LIFO order so indices stay dense.
    pub fn close_acl_page(ctx: Context<CloseAclPage>) -> Result<()> {
        close_acl_page_handler(ctx)
    }

    /// Close an empty `AclConfig` head (`page_count == 0`). Refunds rent
    /// to `acl_config.user`.
    pub fn close_acl_config(ctx: Context<CloseAclConfig>) -> Result<()> {
        close_acl_config_handler(ctx)
    }

    // =========================================
    // RENT RECLAIM (user-callable + admin orphan cleanup)
    // =========================================
    //
    // Four user-callable closes that let the current NFT holder destroy
    // their ANT's on-chain state and recover rent. Order-independent —
    // close any PDA in any order. The mpl-core asset itself is burned via
    // a separate mpl-core `BurnV1` ix (not gated through ario-ant); this
    // module only closes the per-ANT state we own.
    //
    // Authorization: every user-callable close re-reads the asset's MPL
    // Core owner field on-chain and verifies the signer matches. Stale
    // owner (e.g., after a transfer) is rejected.
    //
    // Plus one admin-callable cleanup for the post-purge case where the
    // mpl-core asset has been burned (by `ario-ant-escrow::admin_purge`)
    // and the per-ANT state remains orphaned. Authority-only, refunds to
    // the migration_authority.

    /// Close a single `AntRecord` PDA. Caller must be the current MPL
    /// Core asset owner. Rent flows to the caller. The corresponding
    /// `AntRecordMetadata` PDA (if any) must be closed separately via
    /// `close_ant_record_metadata`.
    pub fn close_ant_record(ctx: Context<CloseAntRecord>, _undername: String) -> Result<()> {
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data);
        require!(
            ctx.accounts.caller.key() == nft_owner,
            AntError::NotNftHolder
        );

        emit!(AntRecordClosedEvent {
            mint: ctx.accounts.asset.key(),
            undername_hash: ctx.accounts.record.undername.as_bytes().to_vec(),
            closer: ctx.accounts.caller.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    /// Close a single `AntRecordMetadata` PDA. Caller must be the current
    /// MPL Core asset owner. Rent flows to the caller.
    pub fn close_ant_record_metadata_for_owner(
        ctx: Context<CloseAntRecordMetadataForOwner>,
        _undername: String,
    ) -> Result<()> {
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data);
        require!(
            ctx.accounts.caller.key() == nft_owner,
            AntError::NotNftHolder
        );

        emit!(AntRecordMetadataClosedEvent {
            mint: ctx.accounts.asset.key(),
            undername_hash: ctx.accounts.record_metadata.undername_hash.to_vec(),
            closer: ctx.accounts.caller.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    /// Close the ANT's `AntControllers` PDA. Caller must be the current
    /// MPL Core asset owner. Rent flows to the caller.
    pub fn close_ant_controllers(ctx: Context<CloseAntControllers>) -> Result<()> {
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data);
        require!(
            ctx.accounts.caller.key() == nft_owner,
            AntError::NotNftHolder
        );

        emit!(AntControllersClosedEvent {
            mint: ctx.accounts.asset.key(),
            closer: ctx.accounts.caller.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    /// Close the ANT's `AntConfig` PDA. Caller must be the current MPL
    /// Core asset owner. Rent flows to the caller. Does NOT require
    /// other per-ANT PDAs to be closed first — leaves them recoverable
    /// via separate ixs, or as orphaned-but-permissionless-closeable via
    /// `close_orphaned_record_metadata` after the parent record's gone.
    pub fn close_ant_config(ctx: Context<CloseAntConfig>) -> Result<()> {
        let asset_data = ctx.accounts.asset.try_borrow_data()?;
        let nft_owner = read_mpl_core_owner(&asset_data)?;
        drop(asset_data);
        require!(
            ctx.accounts.caller.key() == nft_owner,
            AntError::NotNftHolder
        );

        emit!(AntConfigClosedEvent {
            mint: ctx.accounts.asset.key(),
            closer: ctx.accounts.caller.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    /// Admin-only cleanup of per-ANT state for an asset that has been
    /// burned by `ario_ant_escrow::admin_purge_unclaimed_ant`. Closes
    /// `AntConfig` + `AntControllers` if present, and any AntRecord +
    /// AntRecordMetadata PDAs passed via `remaining_accounts`. Refunds
    /// all rent to the configured migration_authority.
    ///
    /// Pre-condition: the mpl-core asset for `mint` must already be
    /// closed (System-owned, empty data). This ensures no user can
    /// reconcile / claim the orphaned state after cleanup.
    pub fn admin_close_orphaned_ant_state<'info>(
        ctx: Context<'_, '_, '_, 'info, AdminCloseOrphanedAntState<'info>>,
    ) -> Result<()> {
        // The asset account must be in the post-burn state: System-owned
        // and zero data. If it's still mpl-core-owned, the asset is
        // alive — refuse, this is for orphaned-state recovery only.
        let asset = &ctx.accounts.asset;
        require!(
            asset.data_is_empty() && asset.owner == &anchor_lang::system_program::ID,
            AntError::AssetStillExists
        );

        let mut closed_records: u32 = 0;
        let mut closed_metadata: u32 = 0;
        let authority_info = ctx.accounts.authority.to_account_info();
        let asset_key = asset.key();
        for acct in ctx.remaining_accounts.iter() {
            // Per-account discriminator dispatch — close whichever PDA
            // type this happens to be. Each record is bound to the asset
            // being cleaned (see the mint check below); within that, the
            // Anchor close pattern refunds to the authority.
            if acct.data_is_empty() {
                continue;
            }
            // Read the 8-byte discriminator AND the record's `mint` field
            // (bytes 8..40 — the first field on both AntRecord and
            // AntRecordMetadata) in one borrow, then drop it before
            // `close_account_to_authority` mutates the account's data.
            let (disc, record_mint): ([u8; 8], Pubkey) = {
                let data = acct.try_borrow_data()?;
                if data.len() < 40 {
                    continue;
                }
                let mut disc = [0u8; 8];
                disc.copy_from_slice(&data[0..8]);
                let mut mint = [0u8; 32];
                mint.copy_from_slice(&data[8..40]);
                (disc, Pubkey::new_from_array(mint))
            };
            if disc == AntRecord::DISCRIMINATOR || disc == AntRecordMetadata::DISCRIMINATOR {
                // Defense-in-depth: bind every closed record to the asset
                // being cleaned up. The auth gate already restricts this ix
                // to the migration authority; this stops that authority from
                // accidentally passing (and thereby closing + draining the
                // rent of) a live record belonging to a DIFFERENT asset —
                // e.g. via a tooling bug that mixes up account lists.
                require_keys_eq!(record_mint, asset_key, AntError::OrphanRecordAssetMismatch);
            }
            if disc == AntRecord::DISCRIMINATOR {
                close_account_to_authority(acct, &authority_info)?;
                closed_records += 1;
            } else if disc == AntRecordMetadata::DISCRIMINATOR {
                close_account_to_authority(acct, &authority_info)?;
                closed_metadata += 1;
            }
        }

        emit!(OrphanedAntStateClosedEvent {
            mint: ctx.accounts.asset.key(),
            closed_records,
            closed_metadata,
            closer: ctx.accounts.authority.key(),
            timestamp: Clock::get()?.unix_timestamp,
        });
        Ok(())
    }

    // =========================================
    // ATTRIBUTES SYNC (Sprint 3 / ADR-016 reshape)
    // =========================================

    /// Sync the Metaplex Core Attributes plugin to reflect a single
    /// ArnsRecord (`name`). Reads the canonical ario-arns record at
    /// `arns_record` via the typed `ario_arns::state::ArnsRecord`,
    /// validates `record.ant == asset.key()`, preserves any existing
    /// `ANT Program` trait (so ADR-016 / BD-100 routing isn't silently
    /// dropped), and CPIs `mpl_core::UpdatePluginV1` to overwrite the
    /// plugin payload with `[ArNS Name, Type, Undername Limit (+ ANT
    /// Program)]`.
    ///
    /// Authority: ANTs mint with the Attributes plugin authority set to
    /// `Owner` — only the current MPL Core asset holder can sign the
    /// inner CPI. `authority` MUST equal the on-chain owner (Anchor
    /// handler check) and is forwarded as both `payer` and `authority`
    /// to the CPI.
    ///
    /// Reshape rationale: pre-Sprint-3, ario-arns `buy_name` /
    /// `upgrade_name` / etc. CPI'd into MPL Core themselves (and skipped
    /// the CPI when the buyer wasn't the ANT holder, requiring a
    /// permissionless re-converge ix). The reshape inverts that —
    /// ario-arns no longer touches MPL, the SDK composes `arns.buy_name`
    /// + `ant.sync_attributes` in one tx, and this is the single point
    /// where the Attributes plugin is written.
    pub fn sync_attributes(ctx: Context<SyncAttributes>, name: String) -> Result<()> {
        sync_attributes_handler(ctx, name)
    }

    /// Clear ArNS-related traits from the asset's Attributes plugin while
    /// preserving the `ANT Program` routing trait.
    ///
    /// The complement of `sync_attributes`: where sync writes
    /// `ArNS Name`/`Type`/`Undername Limit` from a live ArnsRecord, clear
    /// removes them — useful when the canonical record is gone (released
    /// name) or no longer points at this asset (after `reassign_name`).
    /// Both scenarios produce stale traits in the plugin that the asset
    /// owner otherwise has no AR.IO-program path to remove (sync would
    /// fail the post-state `record.ant == asset.key()` check).
    ///
    /// Authority must currently hold the asset (matching sync's authority
    /// rule — Owner-authority Attributes plugin). Strictly additive: no
    /// existing flow breaks. See ADR-016 / BD-100 for the architectural
    /// context this closes.
    pub fn clear_attributes(ctx: Context<ClearAttributes>) -> Result<()> {
        clear_attributes_handler(ctx)
    }
}

fn sync_attributes_handler(ctx: Context<SyncAttributes>, name: String) -> Result<()> {
    use anchor_lang::AccountDeserialize;

    // Authority must currently hold the asset (Owner-authority plugin).
    let asset_data = ctx.accounts.asset.try_borrow_data()?;
    let nft_owner = read_mpl_core_owner(&asset_data)?;
    require!(
        ctx.accounts.authority.key() == nft_owner,
        AntError::NotNftHolder
    );
    // Preserve `ANT Program` across the whole-list replace UpdatePluginV1
    // performs. Read it before we drop the asset borrow.
    let existing_program =
        mpl_core_cpi::read_existing_attribute(&asset_data, mpl_core_cpi::TRAIT_KEY_ANT_PROGRAM);
    drop(asset_data);

    // Validate the ArnsRecord PDA is the canonical ario-arns record for
    // this `name`. Owner check pins the program; seeds check pins the
    // record-to-name binding.
    require!(
        *ctx.accounts.arns_record.owner == ario_arns::ID,
        AntError::InvalidArnsRecord
    );
    let name_lower = name.to_lowercase();
    let name_hash = anchor_lang::solana_program::hash::hash(name_lower.as_bytes());
    let (expected_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &ario_arns::ID);
    require!(
        ctx.accounts.arns_record.key() == expected_pda,
        AntError::InvalidArnsRecord
    );

    // Deserialize the typed ArnsRecord (Anchor checks the discriminator).
    // Pulling the record type from the source crate keeps the borsh
    // layout in lock-step automatically — no parallel parser to drift.
    let arns_data = ctx.accounts.arns_record.try_borrow_data()?;
    let record = ario_arns::state::ArnsRecord::try_deserialize(&mut &arns_data[..])
        .map_err(|_| error!(AntError::InvalidArnsRecord))?;
    drop(arns_data);

    // The record must point at THIS asset — without this check, anyone
    // who owns SOMEONE ELSE's ArnsRecord (forwarded via `name`) could
    // overwrite a victim ANT's traits with values from a different name.
    require!(
        record.ant == ctx.accounts.asset.key(),
        AntError::InvalidArnsRecord
    );

    let attributes = mpl_core_cpi::build_attribute_list(&record, existing_program);
    mpl_core_cpi::update_attributes_plugin(
        &ctx.accounts.asset,
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.authority.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &attributes,
    )?;

    emit!(AttributesSyncedEvent {
        mint: ctx.accounts.asset.key(),
        name: record.name.clone(),
        timestamp: Clock::get()?.unix_timestamp,
    });

    msg!(
        "ANT {} attributes synced for ArNS name '{}' ({} traits)",
        ctx.accounts.asset.key(),
        record.name,
        attributes.len()
    );
    Ok(())
}

fn clear_attributes_handler(ctx: Context<ClearAttributes>) -> Result<()> {
    // Authority must currently hold the asset (Owner-authority plugin).
    // Same rule sync_attributes enforces — only the NFT holder can sign
    // UpdatePluginV1.
    let asset_data = ctx.accounts.asset.try_borrow_data()?;
    let nft_owner = read_mpl_core_owner(&asset_data)?;
    require!(
        ctx.accounts.authority.key() == nft_owner,
        AntError::NotNftHolder
    );

    // Preserve `ANT Program` across the whole-list replace. This trait is
    // asset-bound (ADR-016 / BD-100) — clearing the ArNS-related traits
    // must not blow away the routing key. Match `sync_attributes`'s
    // preservation pattern byte-for-byte.
    let existing_program =
        mpl_core_cpi::read_existing_attribute(&asset_data, mpl_core_cpi::TRAIT_KEY_ANT_PROGRAM);
    drop(asset_data);

    // Build the post-clear attribute list: just `ANT Program` if it was
    // set, otherwise an empty list. UpdatePluginV1 with an empty
    // attribute list is valid; mpl-core treats it as "remove all
    // attributes."
    let mut attributes: Vec<mpl_core_cpi::AttributeKv> = Vec::new();
    if let Some(program) = existing_program {
        attributes.push(mpl_core_cpi::AttributeKv {
            key: mpl_core_cpi::TRAIT_KEY_ANT_PROGRAM.into(),
            value: program,
        });
    }

    let kept_program = !attributes.is_empty();

    mpl_core_cpi::update_attributes_plugin(
        &ctx.accounts.asset,
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.authority.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        &attributes,
    )?;

    let clock = Clock::get()?;
    emit!(AttributesClearedEvent {
        mint: ctx.accounts.asset.key(),
        authority: ctx.accounts.authority.key(),
        kept_program,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "ANT {} attributes cleared ({} traits remain)",
        ctx.accounts.asset.key(),
        attributes.len()
    );
    Ok(())
}

// =========================================
// METAPLEX CORE ASSET DESERIALIZATION
// =========================================

/// Read the owner field from a Metaplex Core asset account's raw data.
///
/// Verified against mpl-core v1.8.0 AssetV1 layout:
/// - byte 0: Key (u8) = 1 for AssetV1
/// - bytes 1..33: Owner (Pubkey, 32 bytes)
/// - byte 33+: UpdateAuthority (varies)
///
/// We only need the owner for permission checks.
///
/// WARNING: This function hardcodes the Metaplex Core AssetV1 byte layout.
/// mpl-core is NOT a Cargo dependency — there is no compile-time layout verification.
/// If Metaplex Core upgrades the AssetV1 layout, this function must be updated manually.
/// Monitor Metaplex Core program upgrades and test against the deployed on-chain version.
pub(crate) fn read_mpl_core_owner(data: &[u8]) -> Result<Pubkey> {
    // Metaplex Core AssetV1 key byte is 1
    require!(data.len() >= 33, AntError::InvalidAsset);
    require!(data[0] == 1, AntError::InvalidAsset);

    let owner_bytes: [u8; 32] = data[1..33].try_into().map_err(|_| AntError::InvalidAsset)?;
    Ok(Pubkey::from(owner_bytes))
}

// =========================================
// PARAMETER TYPES
// =========================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeAntParams {
    /// ANT display name
    pub name: String,
    /// Optional ticker (defaults to "ANT")
    pub ticker: Option<String>,
    /// Content target for the @ record (Arweave TX ID, IPFS CID, etc.)
    pub target: String,
    /// Target protocol (0 = Arweave, 1 = IPFS). Defaults to Arweave if not specified.
    pub target_protocol: Option<u8>,
    /// Logo (Arweave TX ID, 43 chars) — empty for default
    pub logo: String,
    /// Description (max 256 chars)
    pub description: String,
    /// Keywords (max 8)
    pub keywords: Vec<String>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetRecordParams {
    /// Undername (e.g. "@", "blog", "docs")
    pub undername: String,
    /// Content target (Arweave TX ID, IPFS CID, etc., max 128 chars)
    pub target: String,
    /// Target protocol (0 = Arweave, 1 = IPFS)
    pub target_protocol: u8,
    /// TTL in seconds (60-86400)
    pub ttl_seconds: u32,
    /// Priority (only owner/controllers can set)
    pub priority: Option<u32>,
    /// Record owner address (only owner/controllers can assign)
    pub record_owner: Option<Pubkey>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct SetRecordMetadataParams {
    /// Undername (needed for PDA seed derivation)
    pub undername: String,
    /// Optional display name (max 61 chars)
    pub display_name: Option<String>,
    /// Optional logo (Arweave TX ID, 43 chars)
    pub record_logo: Option<String>,
    /// Optional description (max 256 chars)
    pub record_description: Option<String>,
    /// Optional keywords (max 8)
    pub record_keywords: Option<Vec<String>>,
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

/// Metaplex Core program ID
pub const MPL_CORE_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

#[derive(Accounts)]
#[instruction(params: InitializeAntParams)]
pub struct InitializeAnt<'info> {
    /// The Metaplex Core asset (NFT). Verified as owned by mpl-core program.
    /// CHECK: We read raw data to extract owner. Verified by owner constraint.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        init,
        payer = owner,
        space = AntConfig::SIZE,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        init,
        payer = owner,
        space = AntControllers::SIZE,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    #[account(
        init,
        payer = owner,
        space = AntRecord::SIZE,
        seeds = [ANT_RECORD_SEED, asset.key().as_ref(), &hash_undername("@")],
        bump,
    )]
    pub root_record: Account<'info, AntRecord>,

    /// The NFT owner (must be signer and actual NFT holder)
    #[account(mut)]
    pub owner: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(params: SetRecordParams)]
pub struct SetRecord<'info> {
    /// CHECK: Metaplex Core asset — read raw data for owner
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    #[account(
        init_if_needed,
        payer = caller,
        space = AntRecord::SIZE,
        seeds = [ANT_RECORD_SEED, asset.key().as_ref(), &hash_undername(&params.undername)],
        bump,
    )]
    pub record: Account<'info, AntRecord>,

    #[account(mut)]
    pub caller: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RemoveRecord<'info> {
    /// CHECK: Metaplex Core asset
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    /// Record account to close (rent returned to caller, who is an authorized owner/controller)
    #[account(
        mut,
        seeds = [ANT_RECORD_SEED, asset.key().as_ref(), &hash_undername(&record.undername)],
        bump = record.bump,
        close = caller,
    )]
    pub record: Account<'info, AntRecord>,

    /// Optional sibling AntRecordMetadata PDA. When the record being removed
    /// has a metadata PDA, callers SHOULD pass it here so both close in the
    /// same tx. If a caller forgets, the orphan is recoverable via the
    /// permissionless `close_orphaned_record_metadata` instruction (rent
    /// flows to whoever invokes the cleanup).
    /// Pass the ario-ant program ID as the account key to signal `None`.
    #[account(
        mut,
        seeds = [ANT_RECORD_META_SEED, asset.key().as_ref(), &hash_undername(&record.undername)],
        bump,
        close = caller,
    )]
    pub record_metadata: Option<Account<'info, AntRecordMetadata>>,

    #[account(mut)]
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
pub struct TransferRecord<'info> {
    /// CHECK: Metaplex Core asset
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    #[account(
        mut,
        seeds = [ANT_RECORD_SEED, asset.key().as_ref(), &hash_undername(&record.undername)],
        bump = record.bump,
    )]
    pub record: Account<'info, AntRecord>,

    #[account(mut)]
    pub caller: Signer<'info>,
}

/// Accounts for `add_controller`.
///
/// In addition to the usual ANT state accounts, this requires the
/// **controller's** `AclConfig` + `AclPage` so the ACL entry is written
/// inline (ADR-012). The caller pays for any page realloc growth.
#[derive(Accounts)]
#[instruction(controller: Pubkey)]
pub struct AddController<'info> {
    /// CHECK: Metaplex Core asset
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    /// Pre-existing `AclConfig` for the controller being added.
    /// SDK preflight bundles `register_acl_config` first if missing.
    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, controller.as_ref()],
        bump = controller_acl_config.bump,
    )]
    pub controller_acl_config: Account<'info, AclConfig>,

    /// Pre-existing `AclPage` (`page_idx == controller_acl_page.page_idx`)
    /// chosen by the SDK as the destination for the new ACL entry.
    /// SDK preflight bundles `add_acl_page` first if every existing
    /// page is at `MAX_ACL_PAGE_ENTRIES`.
    #[account(
        mut,
        seeds = [
            ACL_PAGE_SEED,
            controller_acl_config.user.as_ref(),
            &controller_acl_page.page_idx.to_le_bytes(),
        ],
        bump = controller_acl_page.bump,
    )]
    pub controller_acl_page: Account<'info, AclPage>,

    /// Pays for any `AclPage` realloc growth caused by the new entry.
    #[account(mut)]
    pub caller: Signer<'info>,

    pub system_program: Program<'info, System>,
}

/// Accounts for `remove_controller`.
///
/// Mirrors `AddController` but does not need `system_program` —
/// removes do not realloc-shrink pages (see ADR-012). The supplied
/// page must be the one currently holding `(asset, controller)`.
#[derive(Accounts)]
#[instruction(controller: Pubkey)]
pub struct RemoveController<'info> {
    /// CHECK: Metaplex Core asset
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, controller.as_ref()],
        bump = controller_acl_config.bump,
    )]
    pub controller_acl_config: Account<'info, AclConfig>,

    #[account(
        mut,
        seeds = [
            ACL_PAGE_SEED,
            controller_acl_config.user.as_ref(),
            &controller_acl_page.page_idx.to_le_bytes(),
        ],
        bump = controller_acl_page.bump,
    )]
    pub controller_acl_page: Account<'info, AclPage>,

    pub caller: Signer<'info>,
}

#[derive(Accounts)]
pub struct ManageMetadata<'info> {
    /// CHECK: Metaplex Core asset
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    pub caller: Signer<'info>,
}

#[derive(Accounts)]
pub struct AntMigration<'info> {
    /// CHECK: Metaplex Core asset
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
        realloc = AntConfig::SIZE,
        realloc::payer = payer,
        realloc::zero = false,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
        realloc = AntControllers::SIZE,
        realloc::payer = payer,
        realloc::zero = false,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    /// Anyone can pay to migrate any ANT (permissionless)
    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AntMigrationRecord<'info> {
    /// CHECK: Metaplex Core asset — validates the record belongs to a real ANT.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_RECORD_SEED, asset.key().as_ref(), &hash_undername(&record.undername)],
        bump = record.bump,
        realloc = AntRecord::SIZE,
        realloc::payer = payer,
        realloc::zero = false,
    )]
    pub record: Account<'info, AntRecord>,

    /// Anyone can pay to migrate any ANT record (permissionless)
    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(undername: String)]
pub struct AntMigrationRecordMetadata<'info> {
    /// CHECK: Metaplex Core asset — validates the metadata belongs to a real ANT.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_RECORD_META_SEED, asset.key().as_ref(), &hash_undername(&undername)],
        bump = record_metadata.bump,
        realloc = AntRecordMetadata::SIZE,
        realloc::payer = payer,
        realloc::zero = false,
    )]
    pub record_metadata: Account<'info, AntRecordMetadata>,

    /// Anyone can pay to migrate any ANT record metadata (permissionless)
    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AntMigrationConfigMigration<'info> {
    #[account(
        mut,
        seeds = [ANT_MIGRATION_CONFIG_SEED],
        bump = migration_config.bump,
        realloc = AntMigrationConfig::SIZE,
        realloc::payer = payer,
        realloc::zero = false,
    )]
    pub migration_config: Account<'info, AntMigrationConfig>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct AclConfigMigration<'info> {
    /// CHECK: the user whose ACL this is
    pub user: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, user.key().as_ref()],
        bump = acl_config.bump,
        realloc = AclConfig::SIZE,
        realloc::payer = payer,
        realloc::zero = false,
    )]
    pub acl_config: Account<'info, AclConfig>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(page_idx: u64)]
pub struct AclPageMigration<'info> {
    /// CHECK: the user whose ACL page this is
    pub user: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ACL_PAGE_SEED, user.key().as_ref(), &page_idx.to_le_bytes()],
        bump = acl_page.bump,
        realloc = AclPage::size_for(acl_page.entries.len()),
        realloc::payer = payer,
        realloc::zero = false,
    )]
    pub acl_page: Account<'info, AclPage>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

/// Accounts for the wrapped `transfer` instruction.
///
/// Carries the standard ANT state, the MPL Core asset + program for the
/// CPI, and the ACL state for both old and new owners. Codama renders
/// every field as a required account, so SDK callers cannot accidentally
/// transfer through this path without bundling the ACL maintenance.
#[derive(Accounts)]
pub struct Transfer<'info> {
    /// Metaplex Core asset being transferred (writable for the CPI).
    /// CHECK: validated via owner constraint + handler reads owner field.
    #[account(
        mut,
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    /// Current asset owner — also the MPL Core authority and rent payer
    /// for any new owner page realloc growth.
    #[account(mut)]
    pub caller: Signer<'info>,

    /// New owner of the asset. Pubkey-only; the actual ownership move
    /// happens via the MPL Core CPI.
    /// CHECK: pubkey input; validated indirectly via the
    /// `new_owner_acl_config` seed binding (`AclConfig.user == this`)
    /// and the post-CPI MPL Core state.
    pub new_owner: AccountInfo<'info>,

    /// New owner's `AclConfig`. Required so we can write
    /// `record_owner(new_owner)` inline. SDK preflight bundles
    /// `register_acl_config` first if it does not exist yet.
    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, new_owner.key().as_ref()],
        bump = new_owner_acl_config.bump,
    )]
    pub new_owner_acl_config: Account<'info, AclConfig>,

    /// New owner's destination `AclPage` (first non-full page chosen by
    /// SDK). SDK preflight bundles `add_acl_page` if every existing
    /// page is at `MAX_ACL_PAGE_ENTRIES`.
    #[account(
        mut,
        seeds = [
            ACL_PAGE_SEED,
            new_owner_acl_config.user.as_ref(),
            &new_owner_acl_page.page_idx.to_le_bytes(),
        ],
        bump = new_owner_acl_page.bump,
    )]
    pub new_owner_acl_page: Account<'info, AclPage>,

    /// Old owner's `AclConfig` (= caller). Required so we can
    /// `swap_remove` the (asset, Owner) entry inline.
    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, caller.key().as_ref()],
        bump = old_owner_acl_config.bump,
    )]
    pub old_owner_acl_config: Account<'info, AclConfig>,

    /// Old owner's `AclPage` containing the live (asset, Owner) entry.
    /// SDK looks this up via `getMultipleAccountsInfo` over
    /// `[0..page_count)` before calling.
    #[account(
        mut,
        seeds = [
            ACL_PAGE_SEED,
            old_owner_acl_config.user.as_ref(),
            &old_owner_acl_page.page_idx.to_le_bytes(),
        ],
        bump = old_owner_acl_page.bump,
    )]
    pub old_owner_acl_page: Account<'info, AclPage>,

    /// MPL Core program — invoked via CPI to do the actual asset transfer.
    /// CHECK: validated via key constraint.
    #[account(
        constraint = mpl_core_program.key() == MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub mpl_core_program: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct Reconcile<'info> {
    /// CHECK: Metaplex Core asset
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    /// Anyone can call reconcile (permissionless)
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(params: SetRecordMetadataParams)]
pub struct SetRecordMetadata<'info> {
    /// CHECK: Metaplex Core asset — read raw data for owner
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    /// Core record — must already exist. Used for permission checks.
    #[account(
        seeds = [ANT_RECORD_SEED, asset.key().as_ref(), &hash_undername(&params.undername)],
        bump = record.bump,
    )]
    pub record: Account<'info, AntRecord>,

    /// Metadata PDA — created on first use, updated on subsequent calls.
    #[account(
        init_if_needed,
        payer = caller,
        space = AntRecordMetadata::SIZE,
        seeds = [ANT_RECORD_META_SEED, asset.key().as_ref(), &hash_undername(&params.undername)],
        bump,
    )]
    pub record_metadata: Account<'info, AntRecordMetadata>,

    #[account(mut)]
    pub caller: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(undername: String)]
pub struct RemoveRecordMetadata<'info> {
    /// CHECK: Metaplex Core asset
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = ant_config.bump,
    )]
    pub ant_config: Account<'info, AntConfig>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    /// Metadata PDA to close — rent returned to caller.
    #[account(
        mut,
        seeds = [ANT_RECORD_META_SEED, asset.key().as_ref(), &hash_undername(&undername)],
        bump = record_metadata.bump,
        close = caller,
    )]
    pub record_metadata: Account<'info, AntRecordMetadata>,

    #[account(mut)]
    pub caller: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(undername: String)]
pub struct CloseOrphanedRecordMetadata<'info> {
    /// CHECK: Validated as an MPL Core asset; we only need its key for PDA derivation.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    /// CHECK: Must be in post-close state (System-owned, empty). Validated in the handler.
    /// Anchor's `seeds` constraint here verifies the address derives correctly from the
    /// canonical record seeds for `(asset, undername)` — guaranteeing this is the right
    /// PDA regardless of whether the underlying account exists.
    #[account(
        seeds = [ANT_RECORD_SEED, asset.key().as_ref(), &hash_undername(&undername.to_lowercase())],
        bump,
    )]
    pub record: UncheckedAccount<'info>,

    /// Orphaned metadata PDA to close — rent flows to the caller.
    #[account(
        mut,
        seeds = [ANT_RECORD_META_SEED, asset.key().as_ref(), &hash_undername(&undername.to_lowercase())],
        bump = record_metadata.bump,
        close = caller,
    )]
    pub record_metadata: Account<'info, AntRecordMetadata>,

    #[account(mut)]
    pub caller: Signer<'info>,
}

/// Sprint 3 / ADR-016 reshape: permissionless sync of the Metaplex Core
/// Attributes plugin to mirror an ArnsRecord. See the `sync_attributes`
/// instruction docs for the full design.
#[derive(Accounts)]
#[instruction(name: String)]
pub struct SyncAttributes<'info> {
    /// The Metaplex Core asset whose Attributes plugin will be updated.
    /// CHECK: validated as MPL-Core-owned via the constraint.
    #[account(
        mut,
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    /// Pays for any rent the inner CPI may charge. Often the same key as
    /// `authority` (ANT owner) but kept separate so a sponsor can pay.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// Must equal the asset's current MPL Core owner — the Attributes
    /// plugin uses `Owner` as its `BasePluginAuthority`, so only the
    /// holder can sign UpdatePluginV1.
    pub authority: Signer<'info>,

    /// CHECK: ArnsRecord PDA in the canonical ario-arns program. Owner +
    /// PDA seeds + `ant == asset.key()` are all validated in the handler.
    pub arns_record: AccountInfo<'info>,

    /// Metaplex Core program — pinned to the canonical ID; required for
    /// the inner CPI.
    /// CHECK: validated via key constraint.
    #[account(
        constraint = mpl_core_program.key() == MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub mpl_core_program: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}

/// Permissionless clear of the asset's ArNS-related Attributes plugin
/// entries. Preserves the `ANT Program` routing trait. See
/// `clear_attributes` instruction docs for the design rationale.
///
/// No `arns_record` account: clearing intentionally requires no
/// reference to a live record. That's the whole point — it's the
/// recovery path when the record is gone (released) or no longer
/// points at this asset (post-reassign).
#[derive(Accounts)]
pub struct ClearAttributes<'info> {
    /// The Metaplex Core asset whose Attributes plugin will be reset.
    /// CHECK: validated as MPL-Core-owned via the constraint.
    #[account(
        mut,
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    /// Pays for any rent the inner CPI may charge. Often the same key as
    /// `authority` but kept separate so a sponsor can pay.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// Must equal the asset's current MPL Core owner — Owner-authority
    /// plugin requires the holder to sign UpdatePluginV1.
    pub authority: Signer<'info>,

    /// Metaplex Core program — pinned to the canonical ID; required for
    /// the inner CPI.
    /// CHECK: validated via key constraint.
    #[account(
        constraint = mpl_core_program.key() == MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub mpl_core_program: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}

// =========================================
// EVENTS (PR-3 of EVENT_EMISSION_IMPLEMENTATION_PLAN)
//
// Indexer-facing events for every state-changing ario-ant instruction.
// 14 event shapes cover 19 instructions; multi-field events use a stable
// `u8` discriminator (`field` for AntConfig metadata / record metadata,
// `role` for ACL entries) instead of String enums so the IDL stays
// compact and the wire encoding is permanent.
//
// Field shapes are part of the published ABI (see ADR-017 — shipped
// events are append-only, ship a *EventV2 if a field has to change).
//
// Migration / one-time / cold-path bookkeeping instructions
// (`initialize`, `initialize_migration`, `import_account`,
// `finalize_migration`, `migrate_ant`, `register_acl_config`,
// `add_acl_page`, `close_acl_page`, `close_acl_config`) are
// intentionally not emitted from — see EVENT_EMISSION_AUDIT.md
// for rationale.
// =========================================

/// AntMetadataField discriminator carried by `AntMetadataUpdatedEvent.field`.
/// Stable wire encoding — append-only, never repurpose.
pub const ANT_METADATA_FIELD_NAME: u8 = 0;
pub const ANT_METADATA_FIELD_TICKER: u8 = 1;
pub const ANT_METADATA_FIELD_DESCRIPTION: u8 = 2;
pub const ANT_METADATA_FIELD_KEYWORDS: u8 = 3;
pub const ANT_METADATA_FIELD_LOGO: u8 = 4;

/// RecordMetadataField discriminator carried by
/// `RecordMetadataUpdatedEvent.field`. `set_record_metadata` writes
/// every field of the metadata PDA in one shot (see the handler) so
/// today only the "all" case exists; the discriminator is reserved so
/// future per-field setters can land additional values without
/// changing the event shape.
pub const RECORD_METADATA_FIELD_ALL: u8 = 0;

/// AclRole discriminator carried by `AclEntryAddedEvent.role` /
/// `AclEntryRemovedEvent.role`. Mirrors the on-chain `AclRole` enum
/// (`Owner = 0, Controller = 1`) — kept as `u8` constants here so
/// callers don't pass magic numbers.
pub const ACL_ROLE_OWNER: u8 = 0;
pub const ACL_ROLE_CONTROLLER: u8 = 1;

// =========================================
// RENT RECLAIM — Accounts structs
// =========================================

/// Close one `AntRecord` PDA. Caller signs as the NFT owner.
#[derive(Accounts)]
#[instruction(undername: String)]
pub struct CloseAntRecord<'info> {
    /// CHECK: Validated as MPL Core asset via the constraint; read in the
    /// handler to verify caller == current owner.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_RECORD_SEED, asset.key().as_ref(), &hash_undername(&undername.to_lowercase())],
        bump = record.bump,
        close = caller,
    )]
    pub record: Account<'info, AntRecord>,

    #[account(mut)]
    pub caller: Signer<'info>,
}

/// Close one `AntRecordMetadata` PDA for the current NFT owner.
#[derive(Accounts)]
#[instruction(undername: String)]
pub struct CloseAntRecordMetadataForOwner<'info> {
    /// CHECK: Validated as MPL Core asset via the constraint.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_RECORD_META_SEED, asset.key().as_ref(), &hash_undername(&undername.to_lowercase())],
        bump = record_metadata.bump,
        close = caller,
    )]
    pub record_metadata: Account<'info, AntRecordMetadata>,

    #[account(mut)]
    pub caller: Signer<'info>,
}

/// Close the `AntControllers` PDA. Caller signs as the NFT owner.
#[derive(Accounts)]
pub struct CloseAntControllers<'info> {
    /// CHECK: Validated as MPL Core asset via the constraint.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = controllers.bump,
        close = caller,
    )]
    pub controllers: Account<'info, AntControllers>,

    #[account(mut)]
    pub caller: Signer<'info>,
}

/// Close the `AntConfig` PDA. Caller signs as the NFT owner.
#[derive(Accounts)]
pub struct CloseAntConfig<'info> {
    /// CHECK: Validated as MPL Core asset via the constraint.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = config.bump,
        close = caller,
    )]
    pub config: Account<'info, AntConfig>,

    #[account(mut)]
    pub caller: Signer<'info>,
}

/// Admin-only post-burn cleanup of orphaned per-ANT PDAs. The asset must
/// already be closed (System-owned, empty data) — typically because
/// `ario_ant_escrow::admin_purge_unclaimed_ant` burned it.
///
/// Closes whichever of `AntConfig` + `AntControllers` are passed (mut),
/// plus iterates `remaining_accounts` for AntRecord + AntRecordMetadata
/// PDAs (discriminator-dispatched). All rent refunds to `authority`.
#[derive(Accounts)]
pub struct AdminCloseOrphanedAntState<'info> {
    /// CHECK: Must be the post-burn state (System-owned, empty).
    /// Validated in the handler — refuses to proceed if the asset is
    /// still mpl-core-owned (i.e. alive). PDA derivations below use
    /// `asset.key()` directly so the asset and the closed PDAs are
    /// cryptographically bound (security audit fix: previously a
    /// separate `mint_key` field allowed an attacker to pass an
    /// unrelated empty asset + a LIVE ANT's mint pubkey as `mint_key`,
    /// closing the live ANT's state to themselves).
    pub asset: AccountInfo<'info>,

    /// Admin authority gate. `has_one = authority` constrains the
    /// signer to equal `AntMigrationConfig.authority` — without this,
    /// the original ix was permissionless and would have let any
    /// caller route per-ANT rent to themselves (security audit fix).
    #[account(
        seeds = [ANT_MIGRATION_CONFIG_SEED],
        bump = migration_config.bump,
        has_one = authority @ AntError::Unauthorized,
    )]
    pub migration_config: Account<'info, AntMigrationConfig>,

    /// AntConfig PDA — closed to `authority`. Seeded on the asset's
    /// own pubkey (which survives mpl-core BurnV1 unchanged), so the
    /// post-burn check on `asset` proves the live state below was
    /// orphaned by THIS asset's burn — not some other live ANT.
    #[account(
        mut,
        seeds = [ANT_CONFIG_SEED, asset.key().as_ref()],
        bump = config.bump,
        close = authority,
    )]
    pub config: Account<'info, AntConfig>,

    /// AntControllers PDA — closed to `authority`. Same asset-bound
    /// PDA derivation as `config`.
    #[account(
        mut,
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = controllers.bump,
        close = authority,
    )]
    pub controllers: Account<'info, AntControllers>,

    /// Protocol admin authority. Verified to equal
    /// `migration_config.authority` via `has_one` above.
    #[account(mut)]
    pub authority: Signer<'info>,
}

/// Manual account close — used by `admin_close_orphaned_ant_state` to
/// close arbitrary AntRecord / AntRecordMetadata PDAs passed in
/// `remaining_accounts`. Mirrors Anchor's `close = recipient` semantics:
/// drain lamports → recipient, zero the data, reassign to system program.
fn close_account_to_authority<'info>(
    account: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
) -> Result<()> {
    let lamports = account.lamports();
    **account.try_borrow_mut_lamports()? = 0;
    **authority.try_borrow_mut_lamports()? = authority
        .lamports()
        .checked_add(lamports)
        .ok_or(AntError::ArithmeticOverflow)?;
    let mut data = account.try_borrow_mut_data()?;
    for byte in data.iter_mut() {
        *byte = 0;
    }
    drop(data);
    account.assign(&anchor_lang::solana_program::system_program::ID);
    Ok(())
}

/// Emitted when a record is created or updated via `set_record`.
#[event]
pub struct RecordSetEvent {
    pub mint: Pubkey,
    pub caller: Pubkey,
    pub undername: String,
    pub target: String,
    pub target_protocol: u8,
    pub ttl_seconds: u32,
    pub priority: u32,
    pub timestamp: i64,
}

/// Emitted when a record is deleted via `remove_record`.
#[event]
pub struct RecordRemovedEvent {
    pub mint: Pubkey,
    pub caller: Pubkey,
    pub undername: String,
    pub timestamp: i64,
}

/// Emitted when a record's `owner` is rotated via `transfer_record`.
/// `previous_owner` is `None` when the record had no prior assigned
/// owner (the typical case for first-time owner assignment).
#[event]
pub struct RecordTransferredEvent {
    pub mint: Pubkey,
    pub caller: Pubkey,
    pub undername: String,
    pub previous_owner: Option<Pubkey>,
    pub new_owner: Pubkey,
    pub timestamp: i64,
}

/// Emitted when a controller is added via `add_controller`.
#[event]
pub struct ControllerAddedEvent {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub controller: Pubkey,
    pub timestamp: i64,
}

/// Emitted when a controller is removed via `remove_controller`.
#[event]
pub struct ControllerRemovedEvent {
    pub mint: Pubkey,
    pub owner: Pubkey,
    pub controller: Pubkey,
    pub timestamp: i64,
}

/// Emitted by every `set_*` ANT-config metadata setter
/// (`set_name`, `set_ticker`, `set_description`, `set_keywords`,
/// `set_logo`). The `field` discriminator (see `ANT_METADATA_FIELD_*`)
/// tells consumers which field changed without bloating the IDL with a
/// String value field. `new_value` carries the new string.
#[event]
pub struct AntMetadataUpdatedEvent {
    pub mint: Pubkey,
    pub caller: Pubkey,
    /// 0 = name, 1 = ticker, 2 = description, 3 = keywords, 4 = logo
    pub field: u8,
    pub new_value: String,
    pub timestamp: i64,
}

/// Emitted by `reconcile` when ownership has actually changed since the
/// last reconcile. No-op reconciles (the asset hasn't moved) do NOT
/// emit — keeps the indexer activity feed clean. `controllers_cleared`
/// is `true` whenever `reconcile` ran and the controller list was
/// cleared (always true on a real ownership change in the current
/// implementation; reserved as a discriminator in case future versions
/// allow controller retention across transfers).
#[event]
pub struct AntReconciledEvent {
    pub mint: Pubkey,
    pub previous_owner: Pubkey,
    pub new_owner: Pubkey,
    pub controllers_cleared: bool,
    pub timestamp: i64,
}

/// Emitted by the wrapped `transfer` instruction (MPL Core
/// `transferV1` CPI + reconcile + ACL swap). This is the asset-side
/// transfer event marketplaces / indexers subscribe to for ANT NFT
/// movement; it is NOT emitted on direct MPL Core transfers that
/// bypass our wrapper (see `reconcile` for the fallback path).
#[event]
pub struct AntTransferredEvent {
    pub mint: Pubkey,
    pub from: Pubkey,
    pub to: Pubkey,
    pub timestamp: i64,
}

/// Emitted by `clear_attributes` after a successful UpdatePluginV1 CPI
/// that resets the asset's ArNS-related traits. The `ANT Program`
/// routing trait is preserved across the clear (BD-100); indexers use
/// this event to invalidate cached trait state when the on-chain
/// ArnsRecord no longer matches the asset (post-release / post-reassign).
/// `kept_program` is true if the asset had an `ANT Program` trait that
/// survived the clear, false if the post-clear attribute list is empty.
#[event]
pub struct AttributesClearedEvent {
    pub mint: Pubkey,
    pub authority: Pubkey,
    pub kept_program: bool,
    pub timestamp: i64,
}

/// Emitted by `sync_attributes` after a successful UpdatePluginV1 CPI.
/// Permissionless cross-program sync between ario-arns and the MPL
/// Core Attributes plugin — indexers use this to know when an ANT's
/// on-chain traits have been refreshed without polling the asset.
#[event]
pub struct AttributesSyncedEvent {
    pub mint: Pubkey,
    pub name: String,
    pub timestamp: i64,
}

/// Emitted by `set_record_metadata`. Record metadata is the optional
/// per-record sibling PDA (`AntRecordMetadata`) carrying display
/// name / logo / description / keywords. `field` is reserved for
/// future granular setters; `set_record_metadata` writes every field
/// in one ix and emits with `field = RECORD_METADATA_FIELD_ALL`.
#[event]
pub struct RecordMetadataUpdatedEvent {
    pub mint: Pubkey,
    pub caller: Pubkey,
    pub undername: String,
    pub field: u8,
    pub timestamp: i64,
}

/// Emitted by `remove_record_metadata` when the metadata PDA is
/// closed and rent flows to the caller.
#[event]
pub struct RecordMetadataRemovedEvent {
    pub mint: Pubkey,
    pub caller: Pubkey,
    pub undername: String,
    pub timestamp: i64,
}

/// Emitted by the permissionless `close_orphaned_record_metadata`
/// cleanup. `pruner` is the (possibly third-party) caller that
/// reclaimed the rent.
#[event]
pub struct RecordMetadataPrunedEvent {
    pub mint: Pubkey,
    pub undername: String,
    pub pruner: Pubkey,
    pub timestamp: i64,
}

/// Emitted when an ACL entry is added (Owner via `record_acl_owner`,
/// Controller via `record_acl_controller`). The `role` discriminator
/// (`ACL_ROLE_*`) distinguishes the two entry types.
#[event]
pub struct AclEntryAddedEvent {
    pub mint: Pubkey,
    pub address: Pubkey,
    /// 0 = Owner, 1 = Controller
    pub role: u8,
    pub timestamp: i64,
}

/// Emitted when an ACL entry is removed (Owner via `remove_acl_owner`,
/// Controller via `remove_acl_controller`).
#[event]
pub struct AclEntryRemovedEvent {
    pub mint: Pubkey,
    pub address: Pubkey,
    /// 0 = Owner, 1 = Controller
    pub role: u8,
    pub timestamp: i64,
}

// =========================================
// RENT RECLAIM events
// =========================================

/// Emitted when an `AntRecord` PDA is closed (user-callable).
#[event]
pub struct AntRecordClosedEvent {
    pub mint: Pubkey,
    pub undername_hash: Vec<u8>,
    pub closer: Pubkey,
    pub timestamp: i64,
}

/// Emitted when an `AntRecordMetadata` PDA is closed by the NFT owner.
#[event]
pub struct AntRecordMetadataClosedEvent {
    pub mint: Pubkey,
    pub undername_hash: Vec<u8>,
    pub closer: Pubkey,
    pub timestamp: i64,
}

/// Emitted when the `AntControllers` PDA is closed.
#[event]
pub struct AntControllersClosedEvent {
    pub mint: Pubkey,
    pub closer: Pubkey,
    pub timestamp: i64,
}

/// Emitted when the `AntConfig` PDA is closed.
#[event]
pub struct AntConfigClosedEvent {
    pub mint: Pubkey,
    pub closer: Pubkey,
    pub timestamp: i64,
}

/// Emitted when admin-cleanup batches close orphaned per-ANT state.
#[event]
pub struct OrphanedAntStateClosedEvent {
    pub mint: Pubkey,
    pub closed_records: u32,
    pub closed_metadata: u32,
    pub closer: Pubkey,
    pub timestamp: i64,
}
