use anchor_lang::prelude::*;

declare_id!("ARioArnsProgXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX");

pub mod error;
pub mod instructions;
pub mod migration;
pub mod pricing;
pub mod schema_migration;
pub mod state;

use instructions::*;
pub use migration::*;
use state::PurchaseType;
use state::{
    SchemaVersion, ARNS_CONFIG_VERSION, ARNS_RECORD_VERSION, DEMAND_FACTOR_VERSION,
    RESERVED_NAME_VERSION, RETURNED_NAME_VERSION,
};

/// AR.IO ArNS Registry Program
///
/// Handles:
/// - Name purchase - lease and permabuy (F30)
/// - Name upgrade - lease to permabuy (F31)
/// - Lease extension (F32)
/// - Undername limit increases (F33)
/// - Name reassignment (F34)
/// - Name release (F35)
/// - Returned name purchase with premium (F36)
/// - Name expiration/pruning (F37)
/// - Reserved names (F38)
/// - Token cost calculation (F39)
/// - Demand factor pricing (F40)
#[program]
pub mod ario_arns {
    use super::*;

    // =========================================
    // INITIALIZATION
    // =========================================

    /// Initialize the ArNS registry and demand factor
    pub fn initialize(ctx: Context<InitializeArns>, params: InitializeArnsParams) -> Result<()> {
        instructions::initialize::handler(ctx, params)
    }

    /// Create the NameRegistry zero-copy account.
    /// Separated from initialize because the 2MB account needs a dedicated tx
    /// with sufficient compute and rent. Only callable by authority.
    pub fn create_name_registry(ctx: Context<CreateNameRegistry>) -> Result<()> {
        instructions::initialize::create_name_registry(ctx)
    }

    /// Recovery-only: shrink an over-sized NameRegistry back to the
    /// current binary's expected size and refund rent to authority.
    /// Used when switching build modes (e.g. production → devnet-shrunk)
    /// Shrink the NameRegistry to `target_capacity` slots (ADR-020).
    /// Authority + `migration_active` gated. Refunds rent to authority.
    pub fn admin_shrink_name_registry(
        ctx: Context<AdminShrinkNameRegistry>,
        target_capacity: u32,
    ) -> Result<()> {
        instructions::initialize::admin_shrink_name_registry(ctx, target_capacity)
    }

    /// Expand the NameRegistry to `target_capacity` slots (ADR-020).
    /// Pays the rent diff from authority. Reallocs ≤10KB per call — caller
    /// invokes multiple times until the on-chain account reaches
    /// `bytes_for_capacity(target_capacity)`. Idempotent (no-op if already
    /// at/above target). Authority-gated; NOT migration_active-gated —
    /// expansion is a permanent protocol lifecycle op.
    pub fn admin_expand_name_registry(
        ctx: Context<AdminExpandNameRegistry>,
        target_capacity: u32,
    ) -> Result<()> {
        instructions::initialize::admin_expand_name_registry(ctx, target_capacity)
    }

    // =========================================
    // NAME PURCHASE (F30, F36)
    // =========================================

    /// Buy a new ArNS name (F30)
    pub fn buy_name(ctx: Context<BuyName>, params: BuyNameParams) -> Result<()> {
        instructions::purchase::buy_name::handler(ctx, params)
    }

    /// Buy a returned name with premium multiplier (F36)
    pub fn buy_returned_name(
        ctx: Context<BuyReturnedName>,
        params: BuyReturnedNameParams,
    ) -> Result<()> {
        instructions::purchase::buy_returned_name::handler(ctx, params)
    }

    // =========================================
    // NAME MANAGEMENT (F31-F35)
    // =========================================

    /// Upgrade a lease to permanent ownership (F31)
    pub fn upgrade_name(ctx: Context<UpgradeName>) -> Result<()> {
        instructions::manage::upgrade_name::handler(ctx)
    }

    /// Extend the lease period (F32)
    pub fn extend_lease(ctx: Context<ExtendLease>, years: u8) -> Result<()> {
        instructions::manage::extend_lease::handler(ctx, years)
    }

    /// Increase undername limit (F33)
    pub fn increase_undername_limit(
        ctx: Context<IncreaseUndernameLimit>,
        quantity: u16,
    ) -> Result<()> {
        instructions::manage::increase_undername_limit::handler(ctx, quantity)
    }

    /// Reassign name to a different ANT/process (F34)
    pub fn reassign_name(ctx: Context<ReassignName>, new_ant: Pubkey) -> Result<()> {
        instructions::manage::reassign_name::handler(ctx, new_ant)
    }

    /// Release a permanent name back to the registry (F35)
    pub fn release_name(ctx: Context<ReleaseName>) -> Result<()> {
        instructions::manage::release_name::handler(ctx)
    }

    // ARNS no longer touches MPL Core (ADR-016 reshape per Atticus's review).
    // Trait sync moved to ario-ant::sync_attributes — that program is the only
    // consumer of MPL Core CPIs. SDK composes [arns:buy_name | reassign | …]
    // with [ant:sync_attributes] in a single tx for atomic UX.

    // =========================================
    // RESERVED NAMES (F38)
    // =========================================

    /// Reserve a name (admin only)
    pub fn reserve_name(ctx: Context<ReserveName>, params: ReserveNameParams) -> Result<()> {
        instructions::reserved::reserve::handler(ctx, params)
    }

    /// Claim a reserved name
    pub fn claim_reserved_name(ctx: Context<ClaimReservedName>) -> Result<()> {
        instructions::reserved::claim::handler(ctx)
    }

    /// Remove a reservation (admin only)
    pub fn unreserve_name(ctx: Context<UnreserveName>) -> Result<()> {
        instructions::reserved::unreserve::handler(ctx)
    }

    /// GAP-9: Prune expired reserved names (permissionless)
    pub fn prune_expired_reservation(ctx: Context<PruneExpiredReservation>) -> Result<()> {
        instructions::reserved::prune_expired_reservation::handler(ctx)
    }

    // =========================================
    // PRUNING (F37)
    // =========================================

    /// Prune expired name leases
    pub fn prune_expired_names(ctx: Context<PruneExpiredNames>, max_names: u8) -> Result<()> {
        instructions::prune::prune_expired::handler(ctx, max_names)
    }

    /// Prune returned names past their auction window
    pub fn prune_returned_names(ctx: Context<PruneReturnedNames>, max_names: u8) -> Result<()> {
        instructions::prune::prune_returned::handler(ctx, max_names)
    }

    /// Prune a single expired name to a returned name (Dutch auction)
    pub fn prune_name_to_returned(ctx: Context<PruneToReturned>) -> Result<()> {
        instructions::prune::prune_to_returned::handler(ctx)
    }

    // =========================================
    // DEMAND FACTOR (F40)
    // =========================================

    /// Update demand factor for new period
    pub fn update_demand_factor(ctx: Context<UpdateDemandFactor>) -> Result<()> {
        instructions::demand::update::handler(ctx)
    }

    // =========================================
    // COST QUERIES (F39)
    // =========================================

    /// Calculate token cost for an operation (F39)
    pub fn get_token_cost(ctx: Context<GetTokenCost>, params: TokenCostParams) -> Result<()> {
        instructions::cost::get_token_cost::handler(ctx, params)
    }

    // =========================================
    // FUND FROM STAKE VARIANTS
    // =========================================

    /// Buy a new ArNS name funded from delegated stake
    pub fn buy_name_from_delegation(
        ctx: Context<BuyNameFromDelegation>,
        params: BuyNameParams,
    ) -> Result<()> {
        instructions::purchase_from_stake::buy_name_from_delegation::handler(ctx, params)
    }

    /// Buy a new ArNS name funded from operator stake
    pub fn buy_name_from_operator_stake(
        ctx: Context<BuyNameFromOperatorStake>,
        params: BuyNameParams,
    ) -> Result<()> {
        instructions::purchase_from_stake::buy_name_from_operator_stake::handler(ctx, params)
    }

    /// Buy a returned name funded from delegated stake
    pub fn buy_returned_name_from_delegation(
        ctx: Context<BuyReturnedNameFromDelegation>,
        params: BuyReturnedNameParams,
    ) -> Result<()> {
        instructions::purchase_from_stake::buy_returned_name_from_delegation::handler(ctx, params)
    }

    /// Buy a returned name funded from operator stake
    pub fn buy_returned_name_from_operator_stake(
        ctx: Context<BuyReturnedNameFromOperatorStake>,
        params: BuyReturnedNameParams,
    ) -> Result<()> {
        instructions::purchase_from_stake::buy_returned_name_from_operator_stake::handler(
            ctx, params,
        )
    }

    /// Upgrade lease to permabuy funded from delegated stake
    pub fn upgrade_name_from_delegation(ctx: Context<UpgradeNameFromDelegation>) -> Result<()> {
        instructions::manage_from_stake::upgrade_name_from_delegation::handler(ctx)
    }

    /// Upgrade lease to permabuy funded from operator stake
    pub fn upgrade_name_from_operator_stake(
        ctx: Context<UpgradeNameFromOperatorStake>,
    ) -> Result<()> {
        instructions::manage_from_stake::upgrade_name_from_operator_stake::handler(ctx)
    }

    /// Extend lease funded from delegated stake
    pub fn extend_lease_from_delegation(
        ctx: Context<ExtendLeaseFromDelegation>,
        years: u8,
    ) -> Result<()> {
        instructions::manage_from_stake::extend_lease_from_delegation::handler(ctx, years)
    }

    /// Extend lease funded from operator stake
    pub fn extend_lease_from_operator_stake(
        ctx: Context<ExtendLeaseFromOperatorStake>,
        years: u8,
    ) -> Result<()> {
        instructions::manage_from_stake::extend_lease_from_operator_stake::handler(ctx, years)
    }

    /// Increase undername limit funded from delegated stake
    pub fn increase_undername_limit_from_delegation(
        ctx: Context<IncreaseUndernameFromDelegation>,
        quantity: u16,
    ) -> Result<()> {
        instructions::manage_from_stake::increase_undername_limit_from_delegation::handler(
            ctx, quantity,
        )
    }

    /// Increase undername limit funded from operator stake
    pub fn increase_undername_limit_from_operator_stake(
        ctx: Context<IncreaseUndernameFromOperatorStake>,
        quantity: u16,
    ) -> Result<()> {
        instructions::manage_from_stake::increase_undername_limit_from_operator_stake::handler(
            ctx, quantity,
        )
    }

    // =========================================
    // FUND-FROM-WITHDRAWAL — Phase 2 (single-source parity with _from_delegation)
    // =========================================

    /// Buy name funded from a Withdrawal vault (single source).
    pub fn buy_name_from_withdrawal(
        ctx: Context<BuyNameFromWithdrawal>,
        params: BuyNameParams,
    ) -> Result<()> {
        instructions::purchase_from_stake::buy_name_from_withdrawal::handler(ctx, params)
    }

    /// Buy returned name funded from a Withdrawal vault (protocol portion only).
    pub fn buy_returned_name_from_withdrawal(
        ctx: Context<BuyReturnedNameFromWithdrawal>,
        params: BuyReturnedNameParams,
    ) -> Result<()> {
        instructions::purchase_from_stake::buy_returned_name_from_withdrawal::handler(ctx, params)
    }

    /// Upgrade lease to permabuy funded from a Withdrawal vault.
    pub fn upgrade_name_from_withdrawal(ctx: Context<UpgradeNameFromWithdrawal>) -> Result<()> {
        instructions::manage_from_stake::upgrade_name_from_withdrawal::handler(ctx)
    }

    /// Extend lease funded from a Withdrawal vault.
    pub fn extend_lease_from_withdrawal(
        ctx: Context<ExtendLeaseFromWithdrawal>,
        years: u8,
    ) -> Result<()> {
        instructions::manage_from_stake::extend_lease_from_withdrawal::handler(ctx, years)
    }

    /// Increase undername limit funded from a Withdrawal vault.
    pub fn increase_undername_limit_from_withdrawal(
        ctx: Context<IncreaseUndernameFromWithdrawal>,
        quantity: u16,
    ) -> Result<()> {
        instructions::manage_from_stake::increase_undername_limit_from_withdrawal::handler(
            ctx, quantity,
        )
    }

    // =========================================
    // FUND-FROM-FUNDING-PLAN — Phase 2 (Lua-faithful multi-source via ario-gar CPI)
    // =========================================

    /// Buy name via a multi-source funding plan (CPIs into ario-gar's pay_from_funding_plan).
    /// `discount_account_count` (0 or 1) tells the wrapper how many discount-gateway PDAs are
    /// at the front of remaining_accounts; the rest are funding-source PDAs.
    pub fn buy_name_from_funding_plan<'info>(
        ctx: Context<'_, '_, 'info, 'info, BuyNameFromFundingPlan<'info>>,
        params: BuyNameParams,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        instructions::purchase_from_stake::buy_name_from_funding_plan::handler(
            ctx,
            params,
            sources,
            discount_account_count,
            residue_vault_count,
        )
    }

    /// Buy returned name via a multi-source funding plan (protocol share only;
    /// initiator share stays a direct buyer→initiator SPL transfer).
    pub fn buy_returned_name_from_funding_plan<'info>(
        ctx: Context<'_, '_, 'info, 'info, BuyReturnedNameFromFundingPlan<'info>>,
        params: BuyReturnedNameParams,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        instructions::purchase_from_stake::buy_returned_name_from_funding_plan::handler(
            ctx,
            params,
            sources,
            discount_account_count,
            residue_vault_count,
        )
    }

    /// Upgrade lease to permabuy via a multi-source funding plan.
    pub fn upgrade_name_from_funding_plan<'info>(
        ctx: Context<'_, '_, 'info, 'info, UpgradeNameFromFundingPlan<'info>>,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        instructions::manage_from_stake::upgrade_name_from_funding_plan::handler(
            ctx,
            sources,
            discount_account_count,
            residue_vault_count,
        )
    }

    /// Extend lease via a multi-source funding plan.
    pub fn extend_lease_from_funding_plan<'info>(
        ctx: Context<'_, '_, 'info, 'info, ExtendLeaseFromFundingPlan<'info>>,
        years: u8,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        instructions::manage_from_stake::extend_lease_from_funding_plan::handler(
            ctx,
            years,
            sources,
            discount_account_count,
            residue_vault_count,
        )
    }

    /// Increase undername limit via a multi-source funding plan.
    pub fn increase_undername_limit_from_funding_plan<'info>(
        ctx: Context<'_, '_, 'info, 'info, IncreaseUndernameFromFundingPlan<'info>>,
        quantity: u16,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        instructions::manage_from_stake::increase_undername_limit_from_funding_plan::handler(
            ctx,
            quantity,
            sources,
            discount_account_count,
            residue_vault_count,
        )
    }

    // =========================================
    // SCHEMA MIGRATION (per-account version upgrade)
    // =========================================

    /// Migrate an `ArnsConfig` PDA to the latest schema version.
    /// Permissionless — anyone can pay the realloc rent.
    pub fn migrate_arns_config(ctx: Context<MigrateArnsConfig>) -> Result<()> {
        let info = ctx.accounts.config.to_account_info();
        schema_migration::grow_account(
            &info,
            &ctx.accounts.payer.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            state::ArnsConfig::SIZE,
        )?;
        let mut config: state::ArnsConfig = {
            let data = info.try_borrow_data()?;
            state::ArnsConfig::try_deserialize(&mut &data[..])?
        };
        require!(
            config.version < ARNS_CONFIG_VERSION,
            error::ArnsError::AlreadyLatestVersion
        );
        schema_migration::migrate_arns_config_version(&mut config)?;
        schema_migration::write_account(&info, &config)?;
        msg!(
            "ArnsConfig migrated to {}.{}.{}",
            config.version.major,
            config.version.minor,
            config.version.patch,
        );
        Ok(())
    }

    /// Migrate a `DemandFactor` PDA to the latest schema version.
    /// Permissionless — anyone can pay the realloc rent.
    pub fn migrate_demand_factor(ctx: Context<MigrateDemandFactor>) -> Result<()> {
        let info = ctx.accounts.demand_factor.to_account_info();
        schema_migration::grow_account(
            &info,
            &ctx.accounts.payer.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            state::DemandFactor::SIZE,
        )?;
        let mut demand_factor: state::DemandFactor = {
            let data = info.try_borrow_data()?;
            state::DemandFactor::try_deserialize(&mut &data[..])?
        };
        require!(
            demand_factor.version < DEMAND_FACTOR_VERSION,
            error::ArnsError::AlreadyLatestVersion
        );
        schema_migration::migrate_demand_factor_version(&mut demand_factor)?;
        schema_migration::write_account(&info, &demand_factor)?;
        msg!(
            "DemandFactor migrated to {}.{}.{}",
            demand_factor.version.major,
            demand_factor.version.minor,
            demand_factor.version.patch,
        );
        Ok(())
    }

    /// Migrate a single `ArnsRecord` PDA to the latest schema version.
    /// Permissionless — anyone can pay the realloc rent. Call once per
    /// name that needs migrating.
    pub fn migrate_arns_record(ctx: Context<MigrateArnsRecord>) -> Result<()> {
        let info = ctx.accounts.record.to_account_info();
        schema_migration::grow_account(
            &info,
            &ctx.accounts.payer.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            state::ArnsRecord::SIZE,
        )?;
        let mut record: state::ArnsRecord = {
            let data = info.try_borrow_data()?;
            state::ArnsRecord::try_deserialize(&mut &data[..])?
        };
        // Seed derives from stored `name_hash`, readable only after
        // deserialize — validate the PDA here (realloc already required
        // program ownership; try_deserialize checked the discriminator).
        let expected = Pubkey::create_program_address(
            &[
                state::ARNS_RECORD_SEED,
                record.name_hash.as_ref(),
                &[record.bump],
            ],
            &crate::ID,
        )
        .map_err(|_| error!(anchor_lang::error::ErrorCode::ConstraintSeeds))?;
        require_keys_eq!(
            info.key(),
            expected,
            anchor_lang::error::ErrorCode::ConstraintSeeds
        );
        require!(
            record.version < ARNS_RECORD_VERSION,
            error::ArnsError::AlreadyLatestVersion
        );
        schema_migration::migrate_arns_record_version(&mut record)?;
        schema_migration::write_account(&info, &record)?;
        msg!(
            "ArnsRecord '{}' migrated to {}.{}.{}",
            record.name,
            record.version.major,
            record.version.minor,
            record.version.patch,
        );
        Ok(())
    }

    /// Migrate a single `ReturnedName` PDA to the latest schema version.
    /// Permissionless — anyone can pay the realloc rent.
    pub fn migrate_returned_name(ctx: Context<MigrateReturnedName>) -> Result<()> {
        let info = ctx.accounts.returned_name.to_account_info();
        schema_migration::grow_account(
            &info,
            &ctx.accounts.payer.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            state::ReturnedName::SIZE,
        )?;
        let mut returned_name: state::ReturnedName = {
            let data = info.try_borrow_data()?;
            state::ReturnedName::try_deserialize(&mut &data[..])?
        };
        // Seed derives from stored `name_hash`, readable only after
        // deserialize — validate the PDA here (realloc already required
        // program ownership; try_deserialize checked the discriminator).
        let expected = Pubkey::create_program_address(
            &[
                state::RETURNED_NAME_SEED,
                returned_name.name_hash.as_ref(),
                &[returned_name.bump],
            ],
            &crate::ID,
        )
        .map_err(|_| error!(anchor_lang::error::ErrorCode::ConstraintSeeds))?;
        require_keys_eq!(
            info.key(),
            expected,
            anchor_lang::error::ErrorCode::ConstraintSeeds
        );
        require!(
            returned_name.version < RETURNED_NAME_VERSION,
            error::ArnsError::AlreadyLatestVersion
        );
        schema_migration::migrate_returned_name_version(&mut returned_name)?;
        schema_migration::write_account(&info, &returned_name)?;
        msg!(
            "ReturnedName '{}' migrated to {}.{}.{}",
            returned_name.name,
            returned_name.version.major,
            returned_name.version.minor,
            returned_name.version.patch,
        );
        Ok(())
    }

    /// Migrate a single `ReservedName` PDA to the latest schema version.
    /// Permissionless — anyone can pay the realloc rent.
    pub fn migrate_reserved_name(ctx: Context<MigrateReservedName>, name: String) -> Result<()> {
        let info = ctx.accounts.reserved_name.to_account_info();
        schema_migration::grow_account(
            &info,
            &ctx.accounts.payer.to_account_info(),
            &ctx.accounts.system_program.to_account_info(),
            state::ReservedName::SIZE,
        )?;
        let mut reserved_name: state::ReservedName = {
            let data = info.try_borrow_data()?;
            state::ReservedName::try_deserialize(&mut &data[..])?
        };
        require!(
            reserved_name.version < RESERVED_NAME_VERSION,
            error::ArnsError::AlreadyLatestVersion
        );
        schema_migration::migrate_reserved_name_version(&mut reserved_name)?;
        schema_migration::write_account(&info, &reserved_name)?;
        msg!(
            "ReservedName '{}' migrated to {}.{}.{}",
            name,
            reserved_name.version.major,
            reserved_name.version.minor,
            reserved_name.version.patch,
        );
        Ok(())
    }

    // =========================================
    // MIGRATION OPERATIONS
    // =========================================

    /// Import a pre-serialized account during migration
    pub fn import_account(
        ctx: Context<ImportAccount>,
        seeds: Vec<Vec<u8>>,
        data: Vec<u8>,
    ) -> Result<()> {
        import_account_handler(ctx, seeds, data)
    }

    /// Import a name entry into the NameRegistry
    pub fn import_registry_entry(
        ctx: Context<ImportRegistryEntry>,
        name_hash: [u8; 32],
        registry_index: u32,
    ) -> Result<()> {
        import_registry_entry_handler(ctx, name_hash, registry_index)
    }

    /// Permanently disable migration imports (main authority only)
    pub fn finalize_migration(ctx: Context<FinalizeMigration>) -> Result<()> {
        finalize_migration_handler(ctx)
    }
}

// =========================================
// SCHEMA MIGRATION ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct MigrateArnsConfig<'info> {
    /// CHECK: PDA pinned by seeds + canonical bump; grown then deserialized
    /// in the handler (grow-then-deserialize).
    #[account(
        mut,
        seeds = [state::ARNS_CONFIG_SEED],
        bump,
    )]
    pub config: UncheckedAccount<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateDemandFactor<'info> {
    /// CHECK: PDA pinned by seeds + canonical bump; grown then deserialized
    /// in the handler (grow-then-deserialize).
    #[account(
        mut,
        seeds = [state::DEMAND_FACTOR_SEED],
        bump,
    )]
    pub demand_factor: UncheckedAccount<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateArnsRecord<'info> {
    /// CHECK: data-derived PDA (seed = stored name_hash) validated in the
    /// handler after grow-then-deserialize.
    #[account(mut)]
    pub record: UncheckedAccount<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct MigrateReturnedName<'info> {
    /// CHECK: data-derived PDA (seed = stored name_hash) validated in the
    /// handler after grow-then-deserialize.
    #[account(mut)]
    pub returned_name: UncheckedAccount<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(name: String)]
pub struct MigrateReservedName<'info> {
    /// CHECK: PDA pinned by seeds + canonical bump; grown then deserialized
    /// in the handler (grow-then-deserialize).
    #[account(
        mut,
        seeds = [state::RESERVED_NAME_SEED, &crate::pricing::hash_name(&name)],
        bump,
    )]
    pub reserved_name: UncheckedAccount<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    pub system_program: Program<'info, System>,
}

// =========================================
// PARAMETER TYPES
// =========================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeArnsParams {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub treasury: Pubkey,
    pub period_zero_start_timestamp: i64,
    pub migration_authority: Pubkey,
    /// Genesis demand factor (RATE_SCALE fixed-point, `DEMAND_FACTOR_SCALE`
    /// = 1.0). The migration seeds this to AO's live value (~9.8) so ArNS
    /// pricing matches the source network at cutover instead of resetting
    /// to 1.0. Must be `>= DEMAND_FACTOR_MIN`.
    pub initial_demand_factor: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BuyNameParams {
    pub name: String,
    pub purchase_type: PurchaseType,
    pub years: u8,
    /// The ANT (Arweave Name Token) address controlling this name.
    /// Named `ant` (not `process_id`) to follow Solana conventions.
    pub ant: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct BuyReturnedNameParams {
    pub name: String,
    pub purchase_type: PurchaseType,
    pub years: u8,
    /// The ANT (Arweave Name Token) address controlling this name.
    pub ant: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct ReserveNameParams {
    pub name: String,
    pub reserved_for: Option<Pubkey>,
    pub expires_at: Option<i64>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct TokenCostParams {
    pub intent: CostIntent,
    pub name: String,
    pub years: Option<u8>,
    pub quantity: Option<u16>,
    /// Required for IncreaseUndernameLimit to distinguish lease vs permabuy pricing
    pub purchase_type: Option<PurchaseType>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub enum CostIntent {
    BuyName,
    ExtendLease,
    UpgradeName,
    IncreaseUndernameLimit,
    PrimaryNameRequest,
}

// Account contexts are defined in their respective instruction modules
// and imported via `use instructions::*;` above.

// =========================================
// EVENTS (PR-1 of EVENT_EMISSION_IMPLEMENTATION_PLAN)
//
// Indexer-facing events for every fee-bearing ArNS instruction. Five
// base shapes cover all 25 funding-source variants — each event carries
// a `funding_source: u8` discriminator (see FundingSource constants
// below) so consumers subscribe to a single event type per operation
// and filter client-side.
//
// Field shapes are part of the published ABI (see ADR-017 — shipped
// events are append-only, ship a *EventV2 if a field has to change).
// =========================================

/// FundingSource discriminator carried by every ArNS purchase / manage
/// event. Values are stable forever — append new ones, never repurpose.
pub const FUNDING_SOURCE_BALANCE: u8 = 0;
pub const FUNDING_SOURCE_DELEGATION: u8 = 1;
pub const FUNDING_SOURCE_OPERATOR_STAKE: u8 = 2;
pub const FUNDING_SOURCE_WITHDRAWAL: u8 = 3;
pub const FUNDING_SOURCE_FUNDING_PLAN: u8 = 4;
// 5 = Turbo (reserved for future Turbo-credits funding path)

/// PurchaseType wire encoding inside `NamePurchasedEvent.purchase_type`.
/// Matches the on-chain `state::PurchaseType` enum tag bytes.
pub const PURCHASE_TYPE_LEASE: u8 = 0;
pub const PURCHASE_TYPE_PERMABUY: u8 = 1;

/// Emitted on every successful name purchase. Covers `buy_name` plus
/// all four `buy_name_from_*` variants (delegation / operator_stake /
/// withdrawal / funding_plan) — the funding path is encoded in
/// `funding_source`.
#[event]
pub struct NamePurchasedEvent {
    pub buyer: Pubkey,
    pub name: String,
    /// 0 = Lease, 1 = Permabuy (see PURCHASE_TYPE_* constants)
    pub purchase_type: u8,
    /// 0 for permabuy
    pub years: u8,
    /// mARIO charged
    pub cost: u64,
    /// ANT mint assigned to this name
    pub ant: Pubkey,
    /// See FUNDING_SOURCE_* constants
    pub funding_source: u8,
    pub timestamp: i64,
}

/// Emitted on returned-name (Dutch auction) purchase. Same shape as
/// `NamePurchasedEvent` plus `premium` (the auction premium charged
/// above the floor cost) and minus `purchase_type` / `years`
/// (returned names are always permabuy-equivalent for the buyer).
#[event]
pub struct ReturnedNamePurchasedEvent {
    pub buyer: Pubkey,
    pub name: String,
    pub cost: u64,
    pub premium: u64,
    pub ant: Pubkey,
    pub funding_source: u8,
    pub timestamp: i64,
}

/// Emitted when an existing lease is upgraded to a permabuy.
#[event]
pub struct NameUpgradedEvent {
    pub owner: Pubkey,
    pub name: String,
    pub cost: u64,
    pub funding_source: u8,
    pub timestamp: i64,
}

/// Emitted when a lease is extended.
#[event]
pub struct LeaseExtendedEvent {
    pub owner: Pubkey,
    pub name: String,
    pub years: u8,
    pub cost: u64,
    pub new_end_timestamp: i64,
    pub funding_source: u8,
    pub timestamp: i64,
}

/// Emitted when a record's undername limit is increased.
#[event]
pub struct UndernameIncreasedEvent {
    pub owner: Pubkey,
    pub name: String,
    pub quantity: u32,
    pub new_limit: u32,
    pub cost: u64,
    pub funding_source: u8,
    pub timestamp: i64,
}

// =========================================
// LIFECYCLE EVENTS (PR-2 of EVENT_EMISSION_IMPLEMENTATION_PLAN)
//
// Non-purchase ArNS lifecycle: record reassignment, release-to-auction,
// reserved-name flows, permissionless pruning, demand factor rolls.
// =========================================

/// PrunedKind discriminator for `NamesPrunedEvent.kind`. Stable wire
/// encoding — append-only, never repurpose.
pub const PRUNED_KIND_EXPIRED_LEASE: u8 = 0;
pub const PRUNED_KIND_RETURNED: u8 = 1;
pub const PRUNED_KIND_EXPIRED_RESERVATION: u8 = 2;

/// Emitted when an existing record's ANT pubkey is rotated.
#[event]
pub struct NameReassignedEvent {
    pub caller: Pubkey,
    pub name: String,
    pub old_ant: Pubkey,
    pub new_ant: Pubkey,
    pub timestamp: i64,
}

/// Emitted when an owner releases a name back to the auction pool.
#[event]
pub struct NameReleasedEvent {
    pub owner: Pubkey,
    pub name: String,
    pub timestamp: i64,
}

/// Emitted when the protocol authority reserves a name (optionally
/// targeted to a specific address, optionally with an expiry).
#[event]
pub struct NameReservedEvent {
    pub authority: Pubkey,
    pub name: String,
    pub target: Option<Pubkey>,
    pub expires_at: Option<i64>,
    pub timestamp: i64,
}

/// Emitted when the protocol authority claims back a reserved slot
/// (administrative cleanup, no fee). The actual name purchase by an
/// end user is covered by `NamePurchasedEvent`.
#[event]
pub struct ReservedNameClaimedEvent {
    pub claimer: Pubkey,
    pub name: String,
    pub timestamp: i64,
}

/// Emitted when the protocol authority removes a reservation.
#[event]
pub struct NameUnreservedEvent {
    pub authority: Pubkey,
    pub name: String,
    pub timestamp: i64,
}

/// Emitted once per permissionless prune transaction with a count of
/// how many records were pruned and a `kind` discriminator. NEVER
/// emitted inside the loop — see EVENT_EMISSION_PLAN for log-truncation
/// rationale. `count = 0` is a valid payload (the instruction can
/// no-op when nothing in `remaining_accounts` is actually expired).
#[event]
pub struct NamesPrunedEvent {
    pub pruner: Pubkey,
    /// 0 = expired_lease, 1 = returned, 2 = expired_reservation
    pub kind: u8,
    pub count: u16,
    pub timestamp: i64,
}

/// Emitted by `update_demand_factor`. Carries the post-roll factor and
/// period index so indexers can plot demand-factor history without
/// snapshotting every block. `fees_halved` flips true on the call that
/// triggers the permanent fee-halving event (after MAX_PERIODS_AT_MIN
/// consecutive periods at the floor).
#[event]
pub struct DemandFactorUpdatedEvent {
    pub caller: Pubkey,
    pub new_demand_factor: u64,
    pub period_index: u64,
    pub fees_halved: bool,
    pub timestamp: i64,
}
