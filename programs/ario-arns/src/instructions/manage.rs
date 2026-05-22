use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::ArnsError;
use crate::pricing::*;
use crate::state::*;
use crate::{
    LeaseExtendedEvent, NameReassignedEvent, NameReleasedEvent, NameUpgradedEvent,
    UndernameIncreasedEvent, FUNDING_SOURCE_BALANCE,
};

pub mod upgrade_name {
    use super::*;

    pub fn handler(ctx: Context<UpgradeName>) -> Result<()> {
        // Permissionless: any ARIO holder can pay to upgrade any active lease
        // to permabuy. Matches Lua `arns.upgradeRecord` (arns.lua:853) — no
        // caller-side authorization. The Metaplex Attributes plugin still
        // requires the ANT NFT holder to sign UpdatePluginV1, so trait sync
        // is best-effort: if the caller IS the ANT owner, traits update in
        // the same tx; otherwise the ANT owner can call `sync_attributes`
        // later to reconcile.
        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        // Must be a lease (not already permabuy)
        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::AlreadyPermanent
        );

        // Must be active or in grace period
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        // Lazy demand factor rollover (matches Lua tick() behavior)
        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        // Calculate upgrade cost (full permabuy price)
        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_permabuy_fee(base_fee, demand.current_demand_factor)?;

        // Apply gateway operator discount if caller passes a Gateway account
        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        // Transfer tokens
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.caller_token_account.to_account_info(),
            to: ctx.accounts.protocol_token_account.to_account_info(),
            authority: ctx.accounts.caller.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, token_cost)?;

        // Update record
        let record = &mut ctx.accounts.arns_record;
        record.purchase_type = PurchaseType::Permabuy;
        record.end_timestamp = None;
        record.purchase_price = token_cost;

        // Tally demand
        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        // ADR-016 reshape: ario-arns no longer touches MPL Core. SDK calls
        // ario-ant::sync_attributes in the same tx to update traits.

        emit!(NameUpgradedEvent {
            owner: ctx.accounts.arns_record.owner,
            name: ctx.accounts.arns_record.name.clone(),
            cost: token_cost,
            funding_source: FUNDING_SOURCE_BALANCE,
            timestamp,
        });

        msg!(
            "Name '{}' upgraded to permabuy for {} mARIO",
            ctx.accounts.arns_record.name,
            token_cost
        );
        Ok(())
    }
}

pub mod extend_lease {
    use super::*;

    pub fn handler(ctx: Context<ExtendLease>, years: u8) -> Result<()> {
        // Permissionless: any ARIO holder can pay to extend any active lease.
        // Matches Lua `arns.extendLease` (arns.lua:220) — no caller-side
        // authorization, the `from` parameter is only used for funding/balance.
        // Extension changes only `end_timestamp`, which is NOT mirrored in
        // Metaplex Attributes plugin traits (the on-chain record is the
        // source of truth), so no MPL Core CPI is needed.
        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        // Must be a lease
        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::CannotExtendPermanent
        );

        // Must be active or in grace period
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        // Validate extension years
        require!(years >= 1, ArnsError::InvalidLeaseDuration);
        let end_ts = record.end_timestamp.ok_or(ArnsError::InvalidParameter)?;
        let remaining_seconds = end_ts.saturating_sub(timestamp).max(0);
        let remaining_years =
            (remaining_seconds as u64 + ONE_YEAR_SECONDS as u64 - 1) / ONE_YEAR_SECONDS as u64;
        let max_extension = (MAX_LEASE_LENGTH_YEARS as u64).saturating_sub(remaining_years);
        require!(
            years as u64 <= max_extension,
            ArnsError::ExtensionExceedsMax
        );

        // Lazy demand factor rollover (matches Lua tick() behavior)
        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        // Calculate extension cost
        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_extension_fee(base_fee, years, demand.current_demand_factor)?;

        // Apply gateway operator discount if caller passes a Gateway account
        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        // Transfer tokens
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.caller_token_account.to_account_info(),
            to: ctx.accounts.protocol_token_account.to_account_info(),
            authority: ctx.accounts.caller.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, token_cost)?;

        // Update record
        let record = &mut ctx.accounts.arns_record;
        let extension = (years as i64)
            .checked_mul(ONE_YEAR_SECONDS)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        let current_end = record.end_timestamp.ok_or(ArnsError::InvalidParameter)?;
        record.end_timestamp = Some(
            current_end
                .checked_add(extension)
                .ok_or(ArnsError::ArithmeticOverflow)?,
        );

        // Update prune schedule
        let config = &mut ctx.accounts.config;
        if let Some(new_end) = record.end_timestamp {
            let prune_ts = new_end
                .checked_add(config.grace_period_seconds)
                .ok_or(ArnsError::ArithmeticOverflow)?;
            if prune_ts < config.next_records_prune_timestamp {
                config.next_records_prune_timestamp = prune_ts;
            }
        }

        // Tally demand
        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(LeaseExtendedEvent {
            owner: ctx.accounts.arns_record.owner,
            name: ctx.accounts.arns_record.name.clone(),
            years,
            cost: token_cost,
            new_end_timestamp: ctx.accounts.arns_record.end_timestamp.unwrap_or(0),
            funding_source: FUNDING_SOURCE_BALANCE,
            timestamp,
        });

        msg!("Lease extended by {} years for {} mARIO", years, token_cost);
        Ok(())
    }
}

pub mod increase_undername_limit {
    use super::*;

    pub fn handler(ctx: Context<IncreaseUndernameLimit>, quantity: u16) -> Result<()> {
        // Permissionless: any ARIO holder can pay to bump the undername
        // limit on any active record. Matches Lua `arns.increaseUndernameLimit`
        // (arns.lua:278) — no caller-side authorization. Trait sync is
        // best-effort against the ANT NFT owner; if caller != owner, the
        // ANT owner reconciles via permissionless `sync_attributes`.
        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(quantity > 0, ArnsError::InvalidUndernameQuantity);
        require!(record.is_active(timestamp), ArnsError::RecordExpired);

        // Lazy demand factor rollover (matches Lua tick() behavior)
        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        // Calculate cost
        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_undername_cost(
            base_fee,
            quantity,
            record.purchase_type,
            demand.current_demand_factor,
        )?;

        // Apply gateway operator discount if caller passes a Gateway account
        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        // Transfer tokens
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.caller_token_account.to_account_info(),
            to: ctx.accounts.protocol_token_account.to_account_info(),
            authority: ctx.accounts.caller.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, token_cost)?;

        // Update record
        let record = &mut ctx.accounts.arns_record;
        record.undername_limit = record
            .undername_limit
            .checked_add(quantity)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        // Tally demand
        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        // ADR-016 reshape: ario-arns no longer touches MPL Core. SDK calls
        // ario-ant::sync_attributes in the same tx to update traits.

        emit!(UndernameIncreasedEvent {
            owner: ctx.accounts.arns_record.owner,
            name: ctx.accounts.arns_record.name.clone(),
            quantity: quantity as u32,
            new_limit: ctx.accounts.arns_record.undername_limit as u32,
            cost: token_cost,
            funding_source: FUNDING_SOURCE_BALANCE,
            timestamp,
        });

        msg!(
            "Undername limit increased by {} for {} mARIO",
            quantity,
            token_cost
        );
        Ok(())
    }
}

pub mod reassign_name {
    use super::*;

    pub fn handler(ctx: Context<ReassignName>, new_ant: Pubkey) -> Result<()> {
        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;
        let config = &ctx.accounts.config;

        // ADR-016 reshape: ario-arns is MPL-agnostic and can no longer read
        // the ANT's NFT holder. Authorization simplifies to caller ==
        // record.owner (the buyer who registered the name; set at buy_name
        // time). NFT-holder-changes-confer-reassign-authority semantics live
        // in the SDK-composed flow (ant.transfer → claim-name → reassign),
        // not on-chain in ario-arns. See BD-100.
        require!(
            ctx.accounts.caller.key() == record.owner,
            ArnsError::NotAntHolder
        );

        // Must be active (not in grace period or expired)
        require!(record.is_active(timestamp), ArnsError::RecordExpired);

        // Must not be in grace period
        require!(
            !record.is_in_grace_period(timestamp, config.grace_period_seconds),
            ArnsError::InGracePeriod
        );

        // Reject no-op reassign — repeated invocations on the same ANT would
        // be visible on DAS / marketplace UIs as flickering traits when the
        // SDK-composed sync_attributes runs (audit M10, 2026-04).
        require!(new_ant != record.ant, ArnsError::InvalidParameter);

        // ADR-016 reshape: trait sync moves to ario-ant. The SDK composes
        // [arns:reassign_name, ant:sync_attributes(name, asset=NEW)] in a
        // single tx — the new asset's traits get populated against the
        // post-reassign record. The OLD asset keeps its (now-stale) name
        // traits; off-chain resolvers MUST treat the on-chain ArnsRecord
        // as source of truth (BD-100 amendment). The OLD asset's
        // asset-scoped `ANT Program` override is unaffected by the
        // reassign — it's a per-asset trait, not name-scoped.

        let old_ant = record.ant;

        // Update ANT/process reference
        let record = &mut ctx.accounts.arns_record;
        record.ant = new_ant;

        emit!(NameReassignedEvent {
            caller: ctx.accounts.caller.key(),
            name: record.name.clone(),
            old_ant,
            new_ant,
            timestamp,
        });

        msg!("Name '{}' reassigned", record.name);
        Ok(())
    }
}

pub mod release_name {
    use super::*;

    pub fn handler(ctx: Context<ReleaseName>) -> Result<()> {
        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        // ADR-016 reshape: ario-arns is MPL-agnostic. Authorization simplifies
        // to caller == record.owner; see reassign_name comment + BD-100.
        require!(
            ctx.accounts.caller.key() == record.owner,
            ArnsError::NotAntHolder
        );

        // Only permabuy names can be released
        require!(
            record.purchase_type == PurchaseType::Permabuy,
            ArnsError::CannotReleaseLease
        );
        require!(record.is_active(timestamp), ArnsError::RecordExpired);

        let name = record.name.clone();
        let name_hash = record.name_hash;

        // Create returned name entry
        let returned = &mut ctx.accounts.returned_name;
        returned.name_hash = name_hash;
        returned.name = name.clone();
        returned.returned_at = timestamp;
        returned.initiator = ctx.accounts.caller.key();
        returned.bump = ctx.bumps.returned_name;
        returned.version = RETURNED_NAME_VERSION;

        // Update prune schedule
        let config = &mut ctx.accounts.config;
        let prune_ts = timestamp
            .checked_add(config.return_auction_duration_seconds)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if prune_ts < config.next_returned_names_prune_timestamp {
            config.next_returned_names_prune_timestamp = prune_ts;
        }

        config.total_names_registered = config.total_names_registered.saturating_sub(1);

        // ADR-016 / BD-100 amendment: trait clearing on release is intentionally
        // NOT bundled with sync_attributes. release_name closes this
        // ArnsRecord PDA, so a follow-up `ant.sync_attributes(name)` would
        // fail its PDA-existence + `record.ant == asset.key()` check. The
        // released asset keeps stale `ArNS Name` / `Type` / `Undername
        // Limit` traits until the next sync_attributes call against a
        // different live record. Off-chain resolvers must treat the
        // on-chain ArnsRecord as source of truth (a trait pointing at a
        // name without a live record means the name was released, not
        // that the asset still owns it).

        // Remove from name registry (swap-remove via byte-offset helper,
        // ADR-020 dynamic-capacity layout).
        {
            let registry_info = &ctx.accounts.name_registry;
            let mut registry_data = registry_info.try_borrow_mut_data()?;
            remove_name_entry_by_hash(&mut registry_data, name_hash);
        }

        // ArNS record account closed by close constraint

        emit!(NameReleasedEvent {
            owner: ctx.accounts.caller.key(),
            name: name.clone(),
            timestamp,
        });

        msg!("Name '{}' released", name);
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct UpgradeName<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(
        mut,
        seeds = [DEMAND_FACTOR_SEED],
        bump = demand_factor.bump,
    )]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        mut,
        seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
        bump = arns_record.bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    #[account(
        mut,
        constraint = caller_token_account.owner == caller.key(),
        constraint = caller_token_account.mint == config.mint,
    )]
    pub caller_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub caller: Signer<'info>,

    pub token_program: Program<'info, Token>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ExtendLease<'info> {
    #[account(
        mut,
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(
        mut,
        seeds = [DEMAND_FACTOR_SEED],
        bump = demand_factor.bump,
    )]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        mut,
        seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
        bump = arns_record.bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    #[account(
        mut,
        constraint = caller_token_account.owner == caller.key(),
        constraint = caller_token_account.mint == config.mint,
    )]
    pub caller_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub caller: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct IncreaseUndernameLimit<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(
        mut,
        seeds = [DEMAND_FACTOR_SEED],
        bump = demand_factor.bump,
    )]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        mut,
        seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
        bump = arns_record.bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    #[account(
        mut,
        constraint = caller_token_account.owner == caller.key(),
        constraint = caller_token_account.mint == config.mint,
    )]
    pub caller_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub caller: Signer<'info>,

    pub token_program: Program<'info, Token>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ReassignName<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArnsConfig>,

    #[account(
        mut,
        seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
        bump = arns_record.bump,
    )]
    pub arns_record: Account<'info, ArnsRecord>,

    #[account(mut)]
    pub caller: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ReleaseName<'info> {
    #[account(
        mut,
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArnsConfig>,

    #[account(
        mut,
        seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
        bump = arns_record.bump,
        close = caller,
    )]
    pub arns_record: Account<'info, ArnsRecord>,

    #[account(
        init,
        payer = caller,
        space = ReturnedName::SIZE,
        seeds = [RETURNED_NAME_SEED, arns_record.name_hash.as_ref()],
        bump,
    )]
    pub returned_name: Account<'info, ReturnedName>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    #[account(mut)]
    pub caller: Signer<'info>,

    pub system_program: Program<'info, System>,
}

// ADR-016 reshape: sync_attributes ix moved to ario-ant. ario-arns no longer
// touches MPL Core. SDK composes [arns:* | ant:sync_attributes(...)] in one tx.
