use anchor_lang::prelude::*;
use anchor_spl::token::{Token, TokenAccount};

use crate::error::ArnsError;
use crate::pricing::*;
use crate::state::*;
use crate::{
    LeaseExtendedEvent, NameUpgradedEvent, UndernameIncreasedEvent, FUNDING_SOURCE_DELEGATION,
    FUNDING_SOURCE_FUNDING_PLAN, FUNDING_SOURCE_OPERATOR_STAKE, FUNDING_SOURCE_WITHDRAWAL,
};

use super::purchase_from_stake::{cpi_deduct_delegation, cpi_deduct_operator_stake};

// =========================================
// UPGRADE NAME FROM DELEGATION
// =========================================

pub mod upgrade_name_from_delegation {
    use super::*;

    pub fn handler(ctx: Context<UpgradeNameFromDelegation>) -> Result<()> {
        // Permissionless: matches Lua. Trait sync is not performed in
        // stake variants (stake-funded calls don't carry MPL Core CPI
        // accounts); ANT owner reconciles via `sync_attributes`.

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::AlreadyPermanent
        );
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_permabuy_fee(base_fee, demand.current_demand_factor)?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_delegation(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.delegation,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.purchase_type = PurchaseType::Permabuy;
        record.end_timestamp = None;
        record.purchase_price = token_cost;

        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(NameUpgradedEvent {
            owner: record.owner,
            name: record.name.clone(),
            cost: token_cost,
            funding_source: FUNDING_SOURCE_DELEGATION,
            timestamp,
        });
        msg!(
            "Name '{}' upgraded to permabuy from delegation for {} mARIO",
            record.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// UPGRADE NAME FROM OPERATOR STAKE
// =========================================

pub mod upgrade_name_from_operator_stake {
    use super::*;

    pub fn handler(ctx: Context<UpgradeNameFromOperatorStake>) -> Result<()> {
        // Permissionless: matches Lua. Trait sync is not performed in
        // stake variants (stake-funded calls don't carry MPL Core CPI
        // accounts); ANT owner reconciles via `sync_attributes`.

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::AlreadyPermanent
        );
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_permabuy_fee(base_fee, demand.current_demand_factor)?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_operator_stake(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.purchase_type = PurchaseType::Permabuy;
        record.end_timestamp = None;
        record.purchase_price = token_cost;

        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(NameUpgradedEvent {
            owner: record.owner,
            name: record.name.clone(),
            cost: token_cost,
            funding_source: FUNDING_SOURCE_OPERATOR_STAKE,
            timestamp,
        });
        msg!(
            "Name '{}' upgraded to permabuy from operator stake for {} mARIO",
            record.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// EXTEND LEASE FROM DELEGATION
// =========================================

pub mod extend_lease_from_delegation {
    use super::*;

    pub fn handler(ctx: Context<ExtendLeaseFromDelegation>, years: u8) -> Result<()> {
        // Permissionless: matches Lua. Trait sync is not performed in
        // stake variants (stake-funded calls don't carry MPL Core CPI
        // accounts); ANT owner reconciles via `sync_attributes`.

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::CannotExtendPermanent
        );
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

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

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_extension_fee(base_fee, years, demand.current_demand_factor)?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_delegation(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.delegation,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

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

        let config = &mut ctx.accounts.config;
        if let Some(new_end) = record.end_timestamp {
            let prune_ts = new_end
                .checked_add(config.grace_period_seconds)
                .ok_or(ArnsError::ArithmeticOverflow)?;
            if prune_ts < config.next_records_prune_timestamp {
                config.next_records_prune_timestamp = prune_ts;
            }
        }

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
            owner: record.owner,
            name: record.name.clone(),
            years,
            cost: token_cost,
            new_end_timestamp: record.end_timestamp.unwrap_or(0),
            funding_source: FUNDING_SOURCE_DELEGATION,
            timestamp,
        });
        msg!(
            "Lease extended by {} years from delegation for {} mARIO",
            years,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// EXTEND LEASE FROM OPERATOR STAKE
// =========================================

pub mod extend_lease_from_operator_stake {
    use super::*;

    pub fn handler(ctx: Context<ExtendLeaseFromOperatorStake>, years: u8) -> Result<()> {
        // Permissionless: matches Lua. Trait sync is not performed in
        // stake variants (stake-funded calls don't carry MPL Core CPI
        // accounts); ANT owner reconciles via `sync_attributes`.

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::CannotExtendPermanent
        );
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

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

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_extension_fee(base_fee, years, demand.current_demand_factor)?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_operator_stake(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

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

        let config = &mut ctx.accounts.config;
        if let Some(new_end) = record.end_timestamp {
            let prune_ts = new_end
                .checked_add(config.grace_period_seconds)
                .ok_or(ArnsError::ArithmeticOverflow)?;
            if prune_ts < config.next_records_prune_timestamp {
                config.next_records_prune_timestamp = prune_ts;
            }
        }

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
            owner: record.owner,
            name: record.name.clone(),
            years,
            cost: token_cost,
            new_end_timestamp: record.end_timestamp.unwrap_or(0),
            funding_source: FUNDING_SOURCE_OPERATOR_STAKE,
            timestamp,
        });
        msg!(
            "Lease extended by {} years from operator stake for {} mARIO",
            years,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// INCREASE UNDERNAME LIMIT FROM DELEGATION
// =========================================

pub mod increase_undername_limit_from_delegation {
    use super::*;

    pub fn handler(ctx: Context<IncreaseUndernameFromDelegation>, quantity: u16) -> Result<()> {
        // Permissionless: matches Lua. Trait sync is not performed in
        // stake variants (stake-funded calls don't carry MPL Core CPI
        // accounts); ANT owner reconciles via `sync_attributes`.

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(quantity > 0, ArnsError::InvalidUndernameQuantity);
        require!(record.is_active(timestamp), ArnsError::RecordExpired);

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_undername_cost(
            base_fee,
            quantity,
            record.purchase_type,
            demand.current_demand_factor,
        )?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_delegation(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.delegation,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.undername_limit = record
            .undername_limit
            .checked_add(quantity)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(UndernameIncreasedEvent {
            owner: record.owner,
            name: record.name.clone(),
            quantity: quantity as u32,
            new_limit: record.undername_limit as u32,
            cost: token_cost,
            funding_source: FUNDING_SOURCE_DELEGATION,
            timestamp,
        });
        msg!(
            "Undername limit increased by {} from delegation for {} mARIO",
            quantity,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// INCREASE UNDERNAME LIMIT FROM OPERATOR STAKE
// =========================================

pub mod increase_undername_limit_from_operator_stake {
    use super::*;

    pub fn handler(ctx: Context<IncreaseUndernameFromOperatorStake>, quantity: u16) -> Result<()> {
        // Permissionless: matches Lua. Trait sync is not performed in
        // stake variants (stake-funded calls don't carry MPL Core CPI
        // accounts); ANT owner reconciles via `sync_attributes`.

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(quantity > 0, ArnsError::InvalidUndernameQuantity);
        require!(record.is_active(timestamp), ArnsError::RecordExpired);

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_undername_cost(
            base_fee,
            quantity,
            record.purchase_type,
            demand.current_demand_factor,
        )?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_operator_stake(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.undername_limit = record
            .undername_limit
            .checked_add(quantity)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(UndernameIncreasedEvent {
            owner: record.owner,
            name: record.name.clone(),
            quantity: quantity as u32,
            new_limit: record.undername_limit as u32,
            cost: token_cost,
            funding_source: FUNDING_SOURCE_OPERATOR_STAKE,
            timestamp,
        });
        msg!(
            "Undername limit increased by {} from operator stake for {} mARIO",
            quantity,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS — Delegation variants
// =========================================

/// Shared account layout for manage operations funded from delegation.
/// Used by upgrade_name, extend_lease, increase_undername_limit.
macro_rules! manage_from_delegation_accounts {
    ($name:ident) => {
        #[derive(Accounts)]
        pub struct $name<'info> {
            #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
            pub config: Box<Account<'info, ArnsConfig>>,

            #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
            pub demand_factor: Box<Account<'info, DemandFactor>>,

            #[account(
                mut,
                seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
                bump = arns_record.bump,
            )]
            pub arns_record: Box<Account<'info, ArnsRecord>>,

            // ant_asset removed: stake-variant handlers no longer authorize
            // by NFT ownership (matches Lua), and none of them currently CPI
            // into MPL Core. Trait sync for stake-funded upgrade/increase
            // stays a known gap — recoverable via permissionless
            // `sync_attributes` after the call.

            // --- ario-gar CPI accounts ---
            /// CHECK: GarSettings PDA — validated by ario-gar CPI
            #[account(mut)]
            pub gar_settings: AccountInfo<'info>,

            /// CHECK: Gateway PDA — validated by ario-gar CPI
            #[account(mut)]
            pub gateway: AccountInfo<'info>,

            /// CHECK: Delegation PDA — validated by ario-gar CPI
            #[account(mut)]
            pub delegation: AccountInfo<'info>,

            /// CHECK: Stake token account — validated by ario-gar CPI
            #[account(mut)]
            pub stake_token_account: AccountInfo<'info>,

            #[account(
                mut,
                constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
                constraint = protocol_token_account.mint == config.mint,
            )]
            pub protocol_token_account: Box<Account<'info, TokenAccount>>,

            #[account(mut)]
            pub caller: Signer<'info>,

            /// CHECK: ario-gar program for CPI
            #[account(address = ario_gar::ID)]
            pub gar_program: AccountInfo<'info>,
            pub token_program: Program<'info, Token>,
        }
    };
}

manage_from_delegation_accounts!(UpgradeNameFromDelegation);
manage_from_delegation_accounts!(ExtendLeaseFromDelegation);
manage_from_delegation_accounts!(IncreaseUndernameFromDelegation);

// =========================================
// ACCOUNT CONTEXTS — Operator stake variants
// =========================================

macro_rules! manage_from_operator_stake_accounts {
    ($name:ident) => {
        #[derive(Accounts)]
        pub struct $name<'info> {
            #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
            pub config: Box<Account<'info, ArnsConfig>>,

            #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
            pub demand_factor: Box<Account<'info, DemandFactor>>,

            #[account(
                mut,
                seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
                bump = arns_record.bump,
            )]
            pub arns_record: Box<Account<'info, ArnsRecord>>,

            // ant_asset removed: see manage_from_delegation_accounts macro
            // for rationale. Same gap, same recovery path.

            // --- ario-gar CPI accounts ---
            /// CHECK: GarSettings PDA — validated by ario-gar CPI
            #[account(mut)]
            pub gar_settings: AccountInfo<'info>,

            /// CHECK: Gateway PDA — validated by ario-gar CPI
            #[account(mut)]
            pub gateway: AccountInfo<'info>,

            /// CHECK: Stake token account — validated by ario-gar CPI
            #[account(mut)]
            pub stake_token_account: AccountInfo<'info>,

            #[account(
                mut,
                constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
                constraint = protocol_token_account.mint == config.mint,
            )]
            pub protocol_token_account: Box<Account<'info, TokenAccount>>,

            #[account(mut)]
            pub caller: Signer<'info>,

            /// CHECK: ario-gar program for CPI
            #[account(address = ario_gar::ID)]
            pub gar_program: AccountInfo<'info>,
            pub token_program: Program<'info, Token>,
        }
    };
}

manage_from_operator_stake_accounts!(UpgradeNameFromOperatorStake);
manage_from_operator_stake_accounts!(ExtendLeaseFromOperatorStake);
manage_from_operator_stake_accounts!(IncreaseUndernameFromOperatorStake);

// =========================================================================
// Phase 2: _from_withdrawal + _from_funding_plan variants
// =========================================================================

use super::purchase_from_stake::{cpi_deduct_withdrawal, cpi_pay_from_funding_plan};

// =========================================
// UPGRADE NAME FROM WITHDRAWAL
// =========================================

pub mod upgrade_name_from_withdrawal {
    use super::*;

    pub fn handler(ctx: Context<UpgradeNameFromWithdrawal>) -> Result<()> {
        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::AlreadyPermanent
        );
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_permabuy_fee(base_fee, demand.current_demand_factor)?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_withdrawal(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.withdrawal,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.purchase_type = PurchaseType::Permabuy;
        record.end_timestamp = None;
        record.purchase_price = token_cost;

        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(NameUpgradedEvent {
            owner: record.owner,
            name: record.name.clone(),
            cost: token_cost,
            funding_source: FUNDING_SOURCE_WITHDRAWAL,
            timestamp,
        });
        msg!(
            "Name '{}' upgraded to permabuy from withdrawal vault for {} mARIO",
            record.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// EXTEND LEASE FROM WITHDRAWAL
// =========================================

pub mod extend_lease_from_withdrawal {
    use super::*;

    pub fn handler(ctx: Context<ExtendLeaseFromWithdrawal>, years: u8) -> Result<()> {
        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::CannotExtendPermanent
        );
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        // Cap aligned with `extend_lease`, `extend_lease_from_delegation`, and
        // `extend_lease_from_operator_stake` (audit M-3, 2026-05-29). Pre-fix
        // this path used a total-duration-since-start cap that under-permitted
        // legitimate extensions for old leases purchased for less than the
        // maximum — inconsistent with the other 3 paths and with the Lua
        // reference (`arns.lua` extendLease). Aligning to the remaining-years-
        // from-now policy: an extension is allowed iff (remaining + extension)
        // fits within MAX_LEASE_LENGTH_YEARS, regardless of how long ago the
        // original lease started. See BD-103 / docs/BEHAVIORAL_DIFFERENCES.md.
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

        let new_end = end_ts
            .checked_add(
                (years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?,
            )
            .ok_or(ArnsError::ArithmeticOverflow)?;

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_extension_fee(base_fee, years, demand.current_demand_factor)?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_withdrawal(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.withdrawal,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.end_timestamp = Some(new_end);
        record.purchase_price = record
            .purchase_price
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        let config = &mut ctx.accounts.config;
        let prune_ts = new_end
            .checked_add(config.grace_period_seconds)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if prune_ts < config.next_records_prune_timestamp {
            config.next_records_prune_timestamp = prune_ts;
        }

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
            owner: record.owner,
            name: record.name.clone(),
            years,
            cost: token_cost,
            new_end_timestamp: record.end_timestamp.unwrap_or(0),
            funding_source: FUNDING_SOURCE_WITHDRAWAL,
            timestamp,
        });
        msg!(
            "Lease extended by {} years from withdrawal vault for {} mARIO",
            years,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// INCREASE UNDERNAME LIMIT FROM WITHDRAWAL
// =========================================

pub mod increase_undername_limit_from_withdrawal {
    use super::*;

    pub fn handler(ctx: Context<IncreaseUndernameFromWithdrawal>, quantity: u16) -> Result<()> {
        require!(
            quantity >= 1 && quantity <= 9990,
            ArnsError::InvalidParameter
        );

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        let new_limit = (record.undername_limit as u32)
            .checked_add(quantity as u32)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        require!(new_limit <= 10_000, ArnsError::InvalidParameter);

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_undername_cost(
            base_fee,
            quantity,
            record.purchase_type,
            demand.current_demand_factor,
        )?;

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.caller.key(),
        )?;

        cpi_deduct_withdrawal(
            &ctx.accounts.gar_program,
            &ctx.accounts.gar_settings,
            &ctx.accounts.withdrawal,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.caller.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.undername_limit = new_limit as u16;
        record.purchase_price = record
            .purchase_price
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(UndernameIncreasedEvent {
            owner: record.owner,
            name: record.name.clone(),
            quantity: quantity as u32,
            new_limit: record.undername_limit as u32,
            cost: token_cost,
            funding_source: FUNDING_SOURCE_WITHDRAWAL,
            timestamp,
        });
        msg!(
            "Undername limit increased by {} from withdrawal vault for {} mARIO",
            quantity,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// UPGRADE NAME FROM FUNDING PLAN
// =========================================

pub mod upgrade_name_from_funding_plan {
    use super::*;

    pub fn handler<'info>(
        ctx: Context<'_, '_, 'info, 'info, UpgradeNameFromFundingPlan<'info>>,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        require!(discount_account_count <= 1, ArnsError::InvalidParameter);

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::AlreadyPermanent
        );
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_permabuy_fee(base_fee, demand.current_demand_factor)?;

        let split = discount_account_count as usize;
        require!(
            split <= ctx.remaining_accounts.len(),
            ArnsError::InvalidParameter
        );
        let (discount_accounts, funding_source_accounts) = ctx.remaining_accounts.split_at(split);
        let token_cost =
            try_apply_gateway_discount(token_cost, discount_accounts, &ctx.accounts.caller.key())?;

        let gar_accounts = ario_gar::cpi::accounts::PayFromFundingPlan {
            settings: ctx.accounts.gar_settings.clone(),
            stake_token_account: ctx.accounts.stake_token_account.clone(),
            protocol_token_account: ctx.accounts.protocol_token_account.to_account_info(),
            payer_token_account: ctx.accounts.payer_token_account.clone(),
            payer: ctx.accounts.caller.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
            withdrawal_counter: ctx.accounts.withdrawal_counter.clone(),
            system_program: ctx.accounts.system_program.to_account_info(),
        };
        cpi_pay_from_funding_plan(
            &ctx.accounts.gar_program,
            gar_accounts,
            funding_source_accounts,
            sources,
            token_cost,
            residue_vault_count,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.purchase_type = PurchaseType::Permabuy;
        record.end_timestamp = None;
        record.purchase_price = token_cost;

        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(NameUpgradedEvent {
            owner: record.owner,
            name: record.name.clone(),
            cost: token_cost,
            funding_source: FUNDING_SOURCE_FUNDING_PLAN,
            timestamp,
        });
        msg!(
            "Name '{}' upgraded to permabuy from funding plan for {} mARIO",
            record.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// EXTEND LEASE FROM FUNDING PLAN
// =========================================

pub mod extend_lease_from_funding_plan {
    use super::*;

    pub fn handler<'info>(
        ctx: Context<'_, '_, 'info, 'info, ExtendLeaseFromFundingPlan<'info>>,
        years: u8,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        require!(discount_account_count <= 1, ArnsError::InvalidParameter);

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;

        require!(
            record.purchase_type == PurchaseType::Lease,
            ArnsError::CannotExtendPermanent
        );
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        // Cap aligned with `extend_lease`, `extend_lease_from_delegation`,
        // `extend_lease_from_operator_stake`, and (post-this-PR)
        // `extend_lease_from_withdrawal` (audit M-3, 2026-05-29). See the
        // sibling comment in `extend_lease_from_withdrawal` above for the
        // rationale; this is the same change applied to the funding-plan
        // path so all 5 extend-lease entry points share one policy.
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

        let new_end = end_ts
            .checked_add(
                (years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?,
            )
            .ok_or(ArnsError::ArithmeticOverflow)?;

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_extension_fee(base_fee, years, demand.current_demand_factor)?;

        let split = discount_account_count as usize;
        require!(
            split <= ctx.remaining_accounts.len(),
            ArnsError::InvalidParameter
        );
        let (discount_accounts, funding_source_accounts) = ctx.remaining_accounts.split_at(split);
        let token_cost =
            try_apply_gateway_discount(token_cost, discount_accounts, &ctx.accounts.caller.key())?;

        let gar_accounts = ario_gar::cpi::accounts::PayFromFundingPlan {
            settings: ctx.accounts.gar_settings.clone(),
            stake_token_account: ctx.accounts.stake_token_account.clone(),
            protocol_token_account: ctx.accounts.protocol_token_account.to_account_info(),
            payer_token_account: ctx.accounts.payer_token_account.clone(),
            payer: ctx.accounts.caller.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
            withdrawal_counter: ctx.accounts.withdrawal_counter.clone(),
            system_program: ctx.accounts.system_program.to_account_info(),
        };
        cpi_pay_from_funding_plan(
            &ctx.accounts.gar_program,
            gar_accounts,
            funding_source_accounts,
            sources,
            token_cost,
            residue_vault_count,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.end_timestamp = Some(new_end);
        record.purchase_price = record
            .purchase_price
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        let config = &mut ctx.accounts.config;
        let prune_ts = new_end
            .checked_add(config.grace_period_seconds)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if prune_ts < config.next_records_prune_timestamp {
            config.next_records_prune_timestamp = prune_ts;
        }

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
            owner: record.owner,
            name: record.name.clone(),
            years,
            cost: token_cost,
            new_end_timestamp: record.end_timestamp.unwrap_or(0),
            funding_source: FUNDING_SOURCE_FUNDING_PLAN,
            timestamp,
        });
        msg!(
            "Lease extended by {} years from funding plan for {} mARIO",
            years,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// INCREASE UNDERNAME LIMIT FROM FUNDING PLAN
// =========================================

pub mod increase_undername_limit_from_funding_plan {
    use super::*;

    pub fn handler<'info>(
        ctx: Context<'_, '_, 'info, 'info, IncreaseUndernameFromFundingPlan<'info>>,
        quantity: u16,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        require!(
            quantity >= 1 && quantity <= 9990,
            ArnsError::InvalidParameter
        );
        require!(discount_account_count <= 1, ArnsError::InvalidParameter);

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let record = &ctx.accounts.arns_record;
        let grace = ctx.accounts.config.grace_period_seconds;
        require!(
            record.is_active(timestamp) || record.is_in_grace_period(timestamp, grace),
            ArnsError::RecordExpired
        );

        let new_limit = (record.undername_limit as u32)
            .checked_add(quantity as u32)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        require!(new_limit <= 10_000, ArnsError::InvalidParameter);

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, record.name.len())?;
        let token_cost = calculate_undername_cost(
            base_fee,
            quantity,
            record.purchase_type,
            demand.current_demand_factor,
        )?;

        let split = discount_account_count as usize;
        require!(
            split <= ctx.remaining_accounts.len(),
            ArnsError::InvalidParameter
        );
        let (discount_accounts, funding_source_accounts) = ctx.remaining_accounts.split_at(split);
        let token_cost =
            try_apply_gateway_discount(token_cost, discount_accounts, &ctx.accounts.caller.key())?;

        let gar_accounts = ario_gar::cpi::accounts::PayFromFundingPlan {
            settings: ctx.accounts.gar_settings.clone(),
            stake_token_account: ctx.accounts.stake_token_account.clone(),
            protocol_token_account: ctx.accounts.protocol_token_account.to_account_info(),
            payer_token_account: ctx.accounts.payer_token_account.clone(),
            payer: ctx.accounts.caller.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
            withdrawal_counter: ctx.accounts.withdrawal_counter.clone(),
            system_program: ctx.accounts.system_program.to_account_info(),
        };
        cpi_pay_from_funding_plan(
            &ctx.accounts.gar_program,
            gar_accounts,
            funding_source_accounts,
            sources,
            token_cost,
            residue_vault_count,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.undername_limit = new_limit as u16;
        record.purchase_price = record
            .purchase_price
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        let demand = &mut ctx.accounts.demand_factor;
        demand.purchases_this_period = demand
            .purchases_this_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        demand.revenue_this_period = demand
            .revenue_this_period
            .checked_add(token_cost)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        emit!(UndernameIncreasedEvent {
            owner: record.owner,
            name: record.name.clone(),
            quantity: quantity as u32,
            new_limit: record.undername_limit as u32,
            cost: token_cost,
            funding_source: FUNDING_SOURCE_FUNDING_PLAN,
            timestamp,
        });
        msg!(
            "Undername limit increased by {} from funding plan for {} mARIO",
            quantity,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS — _from_withdrawal
// =========================================

/// Shared account layout for manage operations funded from a single Withdrawal vault.
macro_rules! manage_from_withdrawal_accounts {
    ($name:ident) => {
        #[derive(Accounts)]
        pub struct $name<'info> {
            #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
            pub config: Box<Account<'info, ArnsConfig>>,

            #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
            pub demand_factor: Box<Account<'info, DemandFactor>>,

            #[account(
                mut,
                seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
                bump = arns_record.bump,
            )]
            pub arns_record: Box<Account<'info, ArnsRecord>>,

            /// CHECK: GarSettings PDA — validated by ario-gar CPI
            #[account(mut)]
            pub gar_settings: AccountInfo<'info>,

            /// CHECK: Withdrawal PDA — validated by ario-gar CPI
            #[account(mut)]
            pub withdrawal: AccountInfo<'info>,

            /// CHECK: Stake token account — validated by ario-gar CPI
            #[account(mut)]
            pub stake_token_account: AccountInfo<'info>,

            #[account(
                mut,
                constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
                constraint = protocol_token_account.mint == config.mint,
            )]
            pub protocol_token_account: Box<Account<'info, TokenAccount>>,

            #[account(mut)]
            pub caller: Signer<'info>,

            /// CHECK: ario-gar program for CPI
            #[account(address = ario_gar::ID)]
            pub gar_program: AccountInfo<'info>,
            pub token_program: Program<'info, Token>,
        }
    };
}

manage_from_withdrawal_accounts!(UpgradeNameFromWithdrawal);
manage_from_withdrawal_accounts!(ExtendLeaseFromWithdrawal);
manage_from_withdrawal_accounts!(IncreaseUndernameFromWithdrawal);

// =========================================
// ACCOUNT CONTEXTS — _from_funding_plan
// =========================================

/// Shared account layout for manage operations funded by a multi-source plan.
macro_rules! manage_from_funding_plan_accounts {
    ($name:ident) => {
        #[derive(Accounts)]
        pub struct $name<'info> {
            #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
            pub config: Box<Account<'info, ArnsConfig>>,

            #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
            pub demand_factor: Box<Account<'info, DemandFactor>>,

            #[account(
                mut,
                seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
                bump = arns_record.bump,
            )]
            pub arns_record: Box<Account<'info, ArnsRecord>>,

            /// CHECK: GarSettings PDA
            #[account(mut)]
            pub gar_settings: AccountInfo<'info>,

            // Per-source gateway PDAs live in remaining_accounts (multi-gateway).

            /// CHECK: Stake token account
            #[account(mut)]
            pub stake_token_account: AccountInfo<'info>,

            #[account(
                mut,
                constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
                constraint = protocol_token_account.mint == config.mint,
            )]
            pub protocol_token_account: Box<Account<'info, TokenAccount>>,

            /// CHECK: Optional payer SPL ATA — required when sources include Balance
            #[account(mut)]
            pub payer_token_account: Option<AccountInfo<'info>>,

            #[account(mut)]
            pub caller: Signer<'info>,

            /// CHECK: WithdrawalCounter PDA
            #[account(mut)]
            pub withdrawal_counter: AccountInfo<'info>,

            // Residue vault slots live in remaining_accounts after per-source PDAs.

            /// CHECK: ario-gar program for CPI
            #[account(address = ario_gar::ID)]
            pub gar_program: AccountInfo<'info>,
            pub token_program: Program<'info, Token>,
            pub system_program: Program<'info, System>,
        }
    };
}

manage_from_funding_plan_accounts!(UpgradeNameFromFundingPlan);
manage_from_funding_plan_accounts!(ExtendLeaseFromFundingPlan);
manage_from_funding_plan_accounts!(IncreaseUndernameFromFundingPlan);
