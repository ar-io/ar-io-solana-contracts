use anchor_lang::prelude::*;
use anchor_lang::AccountDeserialize;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::ArnsError;
use crate::pricing::*;
use crate::state::*;
use crate::{
    BuyNameParams, BuyReturnedNameParams, NamePurchasedEvent, ReturnedNamePurchasedEvent,
    FUNDING_SOURCE_DELEGATION, FUNDING_SOURCE_FUNDING_PLAN, FUNDING_SOURCE_OPERATOR_STAKE,
    FUNDING_SOURCE_WITHDRAWAL, PURCHASE_TYPE_LEASE, PURCHASE_TYPE_PERMABUY,
};

// =========================================
// CPI helper: calls ario-gar's deduct_delegation_for_payment
// =========================================

pub(crate) fn cpi_deduct_delegation<'info>(
    gar_program: &AccountInfo<'info>,
    gar_settings: &AccountInfo<'info>,
    gateway: &AccountInfo<'info>,
    delegation: &AccountInfo<'info>,
    stake_token_account: &AccountInfo<'info>,
    protocol_token_account: &AccountInfo<'info>,
    delegator: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    let cpi_accounts = ario_gar::cpi::accounts::DeductDelegationForPayment {
        settings: gar_settings.clone(),
        gateway: gateway.clone(),
        delegation: delegation.clone(),
        stake_token_account: stake_token_account.clone(),
        protocol_token_account: protocol_token_account.clone(),
        delegator: delegator.clone(),
        token_program: token_program.clone(),
    };
    ario_gar::cpi::deduct_delegation_for_payment(
        CpiContext::new(gar_program.clone(), cpi_accounts),
        amount,
    )
}

pub(crate) fn cpi_deduct_operator_stake<'info>(
    gar_program: &AccountInfo<'info>,
    gar_settings: &AccountInfo<'info>,
    gateway: &AccountInfo<'info>,
    stake_token_account: &AccountInfo<'info>,
    protocol_token_account: &AccountInfo<'info>,
    operator: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    let cpi_accounts = ario_gar::cpi::accounts::DeductOperatorStakeForPayment {
        settings: gar_settings.clone(),
        gateway: gateway.clone(),
        stake_token_account: stake_token_account.clone(),
        protocol_token_account: protocol_token_account.clone(),
        operator: operator.clone(),
        token_program: token_program.clone(),
    };
    ario_gar::cpi::deduct_operator_stake_for_payment(
        CpiContext::new(gar_program.clone(), cpi_accounts),
        amount,
    )
}

/// CPI to ario-gar's `deduct_withdrawal_for_payment` (Phase 1 primitive).
/// Same shape as `cpi_deduct_delegation` minus the gateway account.
/// Used by ArNS `_from_withdrawal` variants for single-source funding.
pub(crate) fn cpi_deduct_withdrawal<'info>(
    gar_program: &AccountInfo<'info>,
    gar_settings: &AccountInfo<'info>,
    withdrawal: &AccountInfo<'info>,
    stake_token_account: &AccountInfo<'info>,
    protocol_token_account: &AccountInfo<'info>,
    owner: &AccountInfo<'info>,
    token_program: &AccountInfo<'info>,
    amount: u64,
) -> Result<()> {
    let cpi_accounts = ario_gar::cpi::accounts::DeductWithdrawalForPayment {
        settings: gar_settings.clone(),
        withdrawal: withdrawal.clone(),
        stake_token_account: stake_token_account.clone(),
        protocol_token_account: protocol_token_account.clone(),
        owner: owner.clone(),
        token_program: token_program.clone(),
    };
    ario_gar::cpi::deduct_withdrawal_for_payment(
        CpiContext::new(gar_program.clone(), cpi_accounts),
        amount,
    )
}

/// CPI to ario-gar's `pay_from_funding_plan` (Phase 1.5 primitive).
/// Forwards the per-source PDAs via `with_remaining_accounts`. Caller is
/// responsible for slicing out any discount-gateway accounts from the
/// outer ix's `remaining_accounts` BEFORE passing them here — only
/// per-source PDAs (Delegation / Withdrawal) belong in this slice.
pub(crate) fn cpi_pay_from_funding_plan<'info>(
    gar_program: &AccountInfo<'info>,
    accounts: ario_gar::cpi::accounts::PayFromFundingPlan<'info>,
    funding_source_accounts: &[AccountInfo<'info>],
    sources: Vec<ario_gar::FundingSourceSpec>,
    expected_total: u64,
    residue_vault_count: u8,
) -> Result<()> {
    let cpi_ctx = CpiContext::new(gar_program.clone(), accounts)
        .with_remaining_accounts(funding_source_accounts.to_vec());
    ario_gar::cpi::pay_from_funding_plan(cpi_ctx, sources, expected_total, residue_vault_count)
}

// =========================================
// BUY NAME FROM DELEGATION
// =========================================

pub mod buy_name_from_delegation {
    use super::*;

    pub fn handler(ctx: Context<BuyNameFromDelegation>, params: BuyNameParams) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let name_hash = hash_name(&params.name);

        // C1: Reserved name check
        let (expected_reserved_pda, _) =
            Pubkey::find_program_address(&[RESERVED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.reserved_name_check.key() == expected_reserved_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_data = ctx.accounts.reserved_name_check.try_borrow_data()?;
            if reserved_data.len() > 8 {
                let reserved = ReservedName::try_deserialize(&mut &reserved_data[..])?;
                drop(reserved_data);
                let is_expired = reserved.expires_at.map_or(false, |exp| timestamp >= exp);
                if !is_expired {
                    match reserved.reserved_for {
                        Some(target) => {
                            require!(target == ctx.accounts.buyer.key(), ArnsError::NameReserved)
                        }
                        None => return Err(ArnsError::NameReserved.into()),
                    }
                }
            } else {
                drop(reserved_data);
            }
        }

        // C2: Returned name auction check
        let (expected_returned_pda, _) =
            Pubkey::find_program_address(&[RETURNED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.returned_name_check.key() == expected_returned_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.returned_name_check.data_is_empty() {
            return Err(ArnsError::AuctionActive.into());
        }

        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let token_cost = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;
        require!(token_cost > 0, ArnsError::InvalidParameter);

        // Gateway operator discount via remaining_accounts (same as buy_name)
        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.buyer.key(),
        )?;

        // Payment: CPI to ario-gar to deduct from delegation
        cpi_deduct_delegation(
            &ctx.accounts.gar_program.to_account_info(),
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.delegation,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.buyer.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        // Initialize the ArNS record
        let record = &mut ctx.accounts.arns_record;
        record.name_hash = name_hash;
        record.name = params.name.to_lowercase();
        record.owner = ctx.accounts.buyer.key();
        record.ant = params.ant;
        record.purchase_type = params.purchase_type;
        record.start_timestamp = timestamp;
        record.end_timestamp = match params.purchase_type {
            PurchaseType::Lease => {
                let duration = (params.years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
                Some(
                    timestamp
                        .checked_add(duration)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
            }
            PurchaseType::Permabuy => None,
        };
        record.undername_limit = DEFAULT_UNDERNAME_COUNT as u16;
        record.purchase_price = token_cost;
        record.bump = ctx.bumps.arns_record;
        record.version = ARNS_RECORD_VERSION;

        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // Append to name registry (dynamic-capacity layout, ADR-020)
        let registry_info = &ctx.accounts.name_registry;
        let mut registry_data = registry_info.try_borrow_mut_data()?;
        let count = name_registry_header(&registry_data).count as u32;
        append_name_entry(
            &mut registry_data,
            NameEntry {
                name_hash,
                registry_index: count,
                _padding: [0u8; 4],
            },
        )?;

        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_info = ctx.accounts.reserved_name_check.to_account_info();
            let lamports = reserved_info.lamports();
            **reserved_info.try_borrow_mut_lamports()? = 0;
            **ctx
                .accounts
                .buyer
                .to_account_info()
                .try_borrow_mut_lamports()? += lamports;
            let mut data = reserved_info.try_borrow_mut_data()?;
            for byte in data.iter_mut() {
                *byte = 0;
            }
            reserved_info.assign(&anchor_lang::solana_program::system_program::ID);
        }

        emit!(NamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: record.name.clone(),
            purchase_type: match record.purchase_type {
                PurchaseType::Lease => PURCHASE_TYPE_LEASE,
                PurchaseType::Permabuy => PURCHASE_TYPE_PERMABUY,
            },
            years: match record.purchase_type {
                PurchaseType::Lease => params.years,
                PurchaseType::Permabuy => 0,
            },
            cost: token_cost,
            ant: params.ant,
            funding_source: FUNDING_SOURCE_DELEGATION,
            timestamp,
        });
        msg!(
            "ArNS name '{}' purchased from delegation for {} mARIO",
            params.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// BUY NAME FROM OPERATOR STAKE
// =========================================

pub mod buy_name_from_operator_stake {
    use super::*;

    pub fn handler(ctx: Context<BuyNameFromOperatorStake>, params: BuyNameParams) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let name_hash = hash_name(&params.name);

        // C1: Reserved name check
        let (expected_reserved_pda, _) =
            Pubkey::find_program_address(&[RESERVED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.reserved_name_check.key() == expected_reserved_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_data = ctx.accounts.reserved_name_check.try_borrow_data()?;
            if reserved_data.len() > 8 {
                let reserved = ReservedName::try_deserialize(&mut &reserved_data[..])?;
                drop(reserved_data);
                let is_expired = reserved.expires_at.map_or(false, |exp| timestamp >= exp);
                if !is_expired {
                    match reserved.reserved_for {
                        Some(target) => {
                            require!(target == ctx.accounts.buyer.key(), ArnsError::NameReserved)
                        }
                        None => return Err(ArnsError::NameReserved.into()),
                    }
                }
            } else {
                drop(reserved_data);
            }
        }

        // C2: Returned name auction check
        let (expected_returned_pda, _) =
            Pubkey::find_program_address(&[RETURNED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.returned_name_check.key() == expected_returned_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.returned_name_check.data_is_empty() {
            return Err(ArnsError::AuctionActive.into());
        }

        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let token_cost = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;
        require!(token_cost > 0, ArnsError::InvalidParameter);

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.buyer.key(),
        )?;

        // Payment: CPI to ario-gar to deduct from operator stake
        cpi_deduct_operator_stake(
            &ctx.accounts.gar_program.to_account_info(),
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.buyer.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        // Initialize the ArNS record (same as buy_name)
        let record = &mut ctx.accounts.arns_record;
        record.name_hash = name_hash;
        record.name = params.name.to_lowercase();
        record.owner = ctx.accounts.buyer.key();
        record.ant = params.ant;
        record.purchase_type = params.purchase_type;
        record.start_timestamp = timestamp;
        record.end_timestamp = match params.purchase_type {
            PurchaseType::Lease => {
                let duration = (params.years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
                Some(
                    timestamp
                        .checked_add(duration)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
            }
            PurchaseType::Permabuy => None,
        };
        record.undername_limit = DEFAULT_UNDERNAME_COUNT as u16;
        record.purchase_price = token_cost;
        record.bump = ctx.bumps.arns_record;
        record.version = ARNS_RECORD_VERSION;

        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // Append to name registry (dynamic-capacity layout, ADR-020)
        let registry_info = &ctx.accounts.name_registry;
        let mut registry_data = registry_info.try_borrow_mut_data()?;
        let count = name_registry_header(&registry_data).count as u32;
        append_name_entry(
            &mut registry_data,
            NameEntry {
                name_hash,
                registry_index: count,
                _padding: [0u8; 4],
            },
        )?;

        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_info = ctx.accounts.reserved_name_check.to_account_info();
            let lamports = reserved_info.lamports();
            **reserved_info.try_borrow_mut_lamports()? = 0;
            **ctx
                .accounts
                .buyer
                .to_account_info()
                .try_borrow_mut_lamports()? += lamports;
            let mut data = reserved_info.try_borrow_mut_data()?;
            for byte in data.iter_mut() {
                *byte = 0;
            }
            reserved_info.assign(&anchor_lang::solana_program::system_program::ID);
        }

        emit!(NamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: record.name.clone(),
            purchase_type: match record.purchase_type {
                PurchaseType::Lease => PURCHASE_TYPE_LEASE,
                PurchaseType::Permabuy => PURCHASE_TYPE_PERMABUY,
            },
            years: match record.purchase_type {
                PurchaseType::Lease => params.years,
                PurchaseType::Permabuy => 0,
            },
            cost: token_cost,
            ant: params.ant,
            funding_source: FUNDING_SOURCE_OPERATOR_STAKE,
            timestamp,
        });
        msg!(
            "ArNS name '{}' purchased from operator stake for {} mARIO",
            params.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// BUY RETURNED NAME FROM DELEGATION
// =========================================

pub mod buy_returned_name_from_delegation {
    use super::*;

    pub fn handler(
        ctx: Context<BuyReturnedNameFromDelegation>,
        params: BuyReturnedNameParams,
    ) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let name_hash = hash_name(&params.name);

        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let registration_fee = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;

        let returned_name = &ctx.accounts.returned_name;
        let token_cost = calculate_returned_name_premium(
            registration_fee,
            returned_name.returned_at,
            timestamp,
        )?;
        require!(token_cost > 0, ArnsError::InvalidParameter);

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.buyer.key(),
        )?;

        // Revenue split: 50% to initiator if not protocol, 100% to protocol otherwise
        let initiator = returned_name.initiator;
        let is_protocol_initiator = initiator == ctx.accounts.config.key();
        let reward_for_protocol = if is_protocol_initiator {
            token_cost
        } else {
            token_cost / 2
        };
        let reward_for_initiator = token_cost
            .checked_sub(reward_for_protocol)
            .ok_or(ArnsError::ArithmeticUnderflow)?;

        // Pay protocol portion from delegation via CPI
        cpi_deduct_delegation(
            &ctx.accounts.gar_program.to_account_info(),
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.delegation,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.buyer.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            reward_for_protocol,
        )?;

        // Pay initiator portion from buyer's wallet (if applicable)
        if reward_for_initiator > 0 && !is_protocol_initiator {
            let cpi_accounts = SplTransfer {
                from: ctx.accounts.buyer_token_account.to_account_info(),
                to: ctx.accounts.initiator_token_account.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            };
            let cpi_ctx =
                CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
            token::transfer(cpi_ctx, reward_for_initiator)?;
        }

        // Initialize the ArNS record
        let record = &mut ctx.accounts.arns_record;
        record.name_hash = name_hash;
        record.name = params.name.to_lowercase();
        record.owner = ctx.accounts.buyer.key();
        record.ant = params.ant;
        record.purchase_type = params.purchase_type;
        record.start_timestamp = timestamp;
        record.end_timestamp = match params.purchase_type {
            PurchaseType::Lease => {
                let duration = (params.years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
                Some(
                    timestamp
                        .checked_add(duration)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
            }
            PurchaseType::Permabuy => None,
        };
        record.undername_limit = DEFAULT_UNDERNAME_COUNT as u16;
        record.purchase_price = token_cost;
        record.bump = ctx.bumps.arns_record;
        record.version = ARNS_RECORD_VERSION;

        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // Append to name registry (dynamic-capacity layout, ADR-020)
        let registry_info = &ctx.accounts.name_registry;
        let mut registry_data = registry_info.try_borrow_mut_data()?;
        let count = name_registry_header(&registry_data).count as u32;
        append_name_entry(
            &mut registry_data,
            NameEntry {
                name_hash,
                registry_index: count,
                _padding: [0u8; 4],
            },
        )?;

        emit!(ReturnedNamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: record.name.clone(),
            cost: token_cost,
            premium: token_cost.saturating_sub(registration_fee),
            ant: params.ant,
            funding_source: FUNDING_SOURCE_DELEGATION,
            timestamp,
        });
        msg!(
            "Returned name '{}' purchased from delegation for {} mARIO",
            params.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// BUY RETURNED NAME FROM OPERATOR STAKE
// =========================================

pub mod buy_returned_name_from_operator_stake {
    use super::*;

    pub fn handler(
        ctx: Context<BuyReturnedNameFromOperatorStake>,
        params: BuyReturnedNameParams,
    ) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let name_hash = hash_name(&params.name);

        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let registration_fee = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;

        let returned_name = &ctx.accounts.returned_name;
        let token_cost = calculate_returned_name_premium(
            registration_fee,
            returned_name.returned_at,
            timestamp,
        )?;
        require!(token_cost > 0, ArnsError::InvalidParameter);

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.buyer.key(),
        )?;

        let initiator = returned_name.initiator;
        let is_protocol_initiator = initiator == ctx.accounts.config.key();
        let reward_for_protocol = if is_protocol_initiator {
            token_cost
        } else {
            token_cost / 2
        };
        let reward_for_initiator = token_cost
            .checked_sub(reward_for_protocol)
            .ok_or(ArnsError::ArithmeticUnderflow)?;

        // Pay protocol portion from operator stake via CPI
        cpi_deduct_operator_stake(
            &ctx.accounts.gar_program.to_account_info(),
            &ctx.accounts.gar_settings,
            &ctx.accounts.gateway,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.buyer.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            reward_for_protocol,
        )?;

        // Pay initiator portion from buyer's wallet (if applicable)
        if reward_for_initiator > 0 && !is_protocol_initiator {
            let cpi_accounts = SplTransfer {
                from: ctx.accounts.buyer_token_account.to_account_info(),
                to: ctx.accounts.initiator_token_account.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            };
            let cpi_ctx =
                CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
            token::transfer(cpi_ctx, reward_for_initiator)?;
        }

        let record = &mut ctx.accounts.arns_record;
        record.name_hash = name_hash;
        record.name = params.name.to_lowercase();
        record.owner = ctx.accounts.buyer.key();
        record.ant = params.ant;
        record.purchase_type = params.purchase_type;
        record.start_timestamp = timestamp;
        record.end_timestamp = match params.purchase_type {
            PurchaseType::Lease => {
                let duration = (params.years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
                Some(
                    timestamp
                        .checked_add(duration)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
            }
            PurchaseType::Permabuy => None,
        };
        record.undername_limit = DEFAULT_UNDERNAME_COUNT as u16;
        record.purchase_price = token_cost;
        record.bump = ctx.bumps.arns_record;
        record.version = ARNS_RECORD_VERSION;

        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // Append to name registry (dynamic-capacity layout, ADR-020)
        let registry_info = &ctx.accounts.name_registry;
        let mut registry_data = registry_info.try_borrow_mut_data()?;
        let count = name_registry_header(&registry_data).count as u32;
        append_name_entry(
            &mut registry_data,
            NameEntry {
                name_hash,
                registry_index: count,
                _padding: [0u8; 4],
            },
        )?;

        emit!(ReturnedNamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: record.name.clone(),
            cost: token_cost,
            premium: token_cost.saturating_sub(registration_fee),
            ant: params.ant,
            funding_source: FUNDING_SOURCE_OPERATOR_STAKE,
            timestamp,
        });
        msg!(
            "Returned name '{}' purchased from operator stake for {} mARIO",
            params.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

/// buy_name funded from delegated stake (CPI to ario-gar)
#[derive(Accounts)]
#[instruction(params: BuyNameParams)]
pub struct BuyNameFromDelegation<'info> {
    #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        init, payer = buyer, space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    /// CHECK: ReservedName PDA — verified in handler
    #[account(mut)]
    pub reserved_name_check: UncheckedAccount<'info>,

    /// CHECK: ReturnedName PDA — verified in handler
    pub returned_name_check: UncheckedAccount<'info>,

    // --- ario-gar CPI accounts (validated by ario-gar during CPI) ---
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
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// buy_name funded from operator stake (CPI to ario-gar)
#[derive(Accounts)]
#[instruction(params: BuyNameParams)]
pub struct BuyNameFromOperatorStake<'info> {
    #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        init, payer = buyer, space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    /// CHECK: ReservedName PDA — verified in handler
    #[account(mut)]
    pub reserved_name_check: UncheckedAccount<'info>,

    /// CHECK: ReturnedName PDA — verified in handler
    pub returned_name_check: UncheckedAccount<'info>,

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
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// buy_returned_name funded from delegated stake.
/// Protocol share from delegation via CPI; initiator share from buyer's wallet.
#[derive(Accounts)]
#[instruction(params: BuyReturnedNameParams)]
pub struct BuyReturnedNameFromDelegation<'info> {
    #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        mut,
        seeds = [RETURNED_NAME_SEED, &crate::pricing::hash_name(&params.name)],
        bump = returned_name.bump,
        close = buyer,
    )]
    pub returned_name: Box<Account<'info, ReturnedName>>,

    #[account(
        init, payer = buyer, space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    /// Buyer's token account (needed for initiator's share of returned name split)
    #[account(
        mut,
        constraint = buyer_token_account.owner == buyer.key(),
        constraint = buyer_token_account.mint == config.mint,
    )]
    pub buyer_token_account: Box<Account<'info, TokenAccount>>,

    /// Initiator's token account (for their 50% share)
    #[account(
        mut,
        constraint = initiator_token_account.owner == returned_name.initiator @ ArnsError::InvalidParameter,
        constraint = initiator_token_account.mint == config.mint,
    )]
    pub initiator_token_account: Box<Account<'info, TokenAccount>>,

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
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// buy_returned_name funded from operator stake.
/// Protocol share from operator stake via CPI; initiator share from buyer's wallet.
#[derive(Accounts)]
#[instruction(params: BuyReturnedNameParams)]
pub struct BuyReturnedNameFromOperatorStake<'info> {
    #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        mut,
        seeds = [RETURNED_NAME_SEED, &crate::pricing::hash_name(&params.name)],
        bump = returned_name.bump,
        close = buyer,
    )]
    pub returned_name: Box<Account<'info, ReturnedName>>,

    #[account(
        init, payer = buyer, space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    /// Buyer's token account (needed for initiator's share)
    #[account(
        mut,
        constraint = buyer_token_account.owner == buyer.key(),
        constraint = buyer_token_account.mint == config.mint,
    )]
    pub buyer_token_account: Box<Account<'info, TokenAccount>>,

    /// Initiator's token account (for their 50% share)
    #[account(
        mut,
        constraint = initiator_token_account.owner == returned_name.initiator @ ArnsError::InvalidParameter,
        constraint = initiator_token_account.mint == config.mint,
    )]
    pub initiator_token_account: Box<Account<'info, TokenAccount>>,

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
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

// =========================================================================
// Phase 2: _from_withdrawal + _from_funding_plan variants
// =========================================================================
//
// _from_withdrawal mirrors _from_delegation exactly except the funding source
// is a Withdrawal PDA (gateway-independent). _from_funding_plan accepts a
// `Vec<FundingSourceSpec>` and CPIs into ario-gar's pay_from_funding_plan,
// dispatching across N sources of any kind. See docs/FUND_FROM_PLAN.md.
//
// Discount handling for `_from_funding_plan`: callers may optionally include
// a gateway-discount PDA at `remaining_accounts[0]` and set
// `discount_account_count: u8 = 1` in the ix args. Funding-source PDAs follow
// at `remaining_accounts[discount_account_count..]`. Set `discount_account_count = 0`
// to skip the discount path entirely (cheaper CU when not eligible).

// =========================================
// BUY NAME FROM WITHDRAWAL
// =========================================

pub mod buy_name_from_withdrawal {
    use super::*;

    pub fn handler(ctx: Context<BuyNameFromWithdrawal>, params: BuyNameParams) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let name_hash = hash_name(&params.name);

        let (expected_reserved_pda, _) =
            Pubkey::find_program_address(&[RESERVED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.reserved_name_check.key() == expected_reserved_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_data = ctx.accounts.reserved_name_check.try_borrow_data()?;
            if reserved_data.len() > 8 {
                let reserved = ReservedName::try_deserialize(&mut &reserved_data[..])?;
                drop(reserved_data);
                let is_expired = reserved.expires_at.map_or(false, |exp| timestamp >= exp);
                if !is_expired {
                    match reserved.reserved_for {
                        Some(target) => {
                            require!(target == ctx.accounts.buyer.key(), ArnsError::NameReserved)
                        }
                        None => return Err(ArnsError::NameReserved.into()),
                    }
                }
            } else {
                drop(reserved_data);
            }
        }

        let (expected_returned_pda, _) =
            Pubkey::find_program_address(&[RETURNED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.returned_name_check.key() == expected_returned_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.returned_name_check.data_is_empty() {
            return Err(ArnsError::AuctionActive.into());
        }

        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let token_cost = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;
        require!(token_cost > 0, ArnsError::InvalidParameter);

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.buyer.key(),
        )?;

        cpi_deduct_withdrawal(
            &ctx.accounts.gar_program.to_account_info(),
            &ctx.accounts.gar_settings,
            &ctx.accounts.withdrawal,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.buyer.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            token_cost,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.name_hash = name_hash;
        record.name = params.name.to_lowercase();
        record.owner = ctx.accounts.buyer.key();
        record.ant = params.ant;
        record.purchase_type = params.purchase_type;
        record.start_timestamp = timestamp;
        record.end_timestamp = match params.purchase_type {
            PurchaseType::Lease => {
                let duration = (params.years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
                Some(
                    timestamp
                        .checked_add(duration)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
            }
            PurchaseType::Permabuy => None,
        };
        record.undername_limit = DEFAULT_UNDERNAME_COUNT as u16;
        record.purchase_price = token_cost;
        record.bump = ctx.bumps.arns_record;
        record.version = ARNS_RECORD_VERSION;

        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // Append to name registry (dynamic-capacity layout, ADR-020)
        let registry_info = &ctx.accounts.name_registry;
        let mut registry_data = registry_info.try_borrow_mut_data()?;
        let count = name_registry_header(&registry_data).count as u32;
        append_name_entry(
            &mut registry_data,
            NameEntry {
                name_hash,
                registry_index: count,
                _padding: [0u8; 4],
            },
        )?;

        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_info = ctx.accounts.reserved_name_check.to_account_info();
            let lamports = reserved_info.lamports();
            **reserved_info.try_borrow_mut_lamports()? = 0;
            **ctx
                .accounts
                .buyer
                .to_account_info()
                .try_borrow_mut_lamports()? += lamports;
            let mut data = reserved_info.try_borrow_mut_data()?;
            for byte in data.iter_mut() {
                *byte = 0;
            }
            reserved_info.assign(&anchor_lang::solana_program::system_program::ID);
        }

        emit!(NamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: record.name.clone(),
            purchase_type: match record.purchase_type {
                PurchaseType::Lease => PURCHASE_TYPE_LEASE,
                PurchaseType::Permabuy => PURCHASE_TYPE_PERMABUY,
            },
            years: match record.purchase_type {
                PurchaseType::Lease => params.years,
                PurchaseType::Permabuy => 0,
            },
            cost: token_cost,
            ant: params.ant,
            funding_source: FUNDING_SOURCE_WITHDRAWAL,
            timestamp,
        });
        msg!(
            "ArNS name '{}' purchased from withdrawal vault for {} mARIO",
            params.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// BUY RETURNED NAME FROM WITHDRAWAL
// =========================================

pub mod buy_returned_name_from_withdrawal {
    use super::*;

    pub fn handler(
        ctx: Context<BuyReturnedNameFromWithdrawal>,
        params: BuyReturnedNameParams,
    ) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let name_hash = hash_name(&params.name);

        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let registration_fee = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;

        let returned_name = &ctx.accounts.returned_name;
        let token_cost = calculate_returned_name_premium(
            registration_fee,
            returned_name.returned_at,
            timestamp,
        )?;
        require!(token_cost > 0, ArnsError::InvalidParameter);

        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.buyer.key(),
        )?;

        let initiator = returned_name.initiator;
        let is_protocol_initiator = initiator == ctx.accounts.config.key();
        let reward_for_protocol = if is_protocol_initiator {
            token_cost
        } else {
            token_cost / 2
        };
        let reward_for_initiator = token_cost
            .checked_sub(reward_for_protocol)
            .ok_or(ArnsError::ArithmeticUnderflow)?;

        cpi_deduct_withdrawal(
            &ctx.accounts.gar_program.to_account_info(),
            &ctx.accounts.gar_settings,
            &ctx.accounts.withdrawal,
            &ctx.accounts.stake_token_account,
            &ctx.accounts.protocol_token_account.to_account_info(),
            &ctx.accounts.buyer.to_account_info(),
            &ctx.accounts.token_program.to_account_info(),
            reward_for_protocol,
        )?;

        if reward_for_initiator > 0 && !is_protocol_initiator {
            let cpi_accounts = SplTransfer {
                from: ctx.accounts.buyer_token_account.to_account_info(),
                to: ctx.accounts.initiator_token_account.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            };
            let cpi_ctx =
                CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
            token::transfer(cpi_ctx, reward_for_initiator)?;
        }

        let record = &mut ctx.accounts.arns_record;
        record.name_hash = name_hash;
        record.name = params.name.to_lowercase();
        record.owner = ctx.accounts.buyer.key();
        record.ant = params.ant;
        record.purchase_type = params.purchase_type;
        record.start_timestamp = timestamp;
        record.end_timestamp = match params.purchase_type {
            PurchaseType::Lease => {
                let duration = (params.years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
                Some(
                    timestamp
                        .checked_add(duration)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
            }
            PurchaseType::Permabuy => None,
        };
        record.undername_limit = DEFAULT_UNDERNAME_COUNT as u16;
        record.purchase_price = token_cost;
        record.bump = ctx.bumps.arns_record;
        record.version = ARNS_RECORD_VERSION;

        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // Append to name registry (dynamic-capacity layout, ADR-020)
        let registry_info = &ctx.accounts.name_registry;
        let mut registry_data = registry_info.try_borrow_mut_data()?;
        let count = name_registry_header(&registry_data).count as u32;
        append_name_entry(
            &mut registry_data,
            NameEntry {
                name_hash,
                registry_index: count,
                _padding: [0u8; 4],
            },
        )?;

        emit!(ReturnedNamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: record.name.clone(),
            cost: token_cost,
            premium: token_cost.saturating_sub(registration_fee),
            ant: params.ant,
            funding_source: FUNDING_SOURCE_WITHDRAWAL,
            timestamp,
        });
        msg!("ArNS returned name '{}' purchased from withdrawal vault for {} mARIO (protocol={}, initiator={})",
             params.name, token_cost, reward_for_protocol, reward_for_initiator);
        Ok(())
    }
}

// =========================================
// BUY NAME FROM FUNDING PLAN
// =========================================

pub mod buy_name_from_funding_plan {
    use super::*;

    pub fn handler<'info>(
        ctx: Context<'_, '_, 'info, 'info, BuyNameFromFundingPlan<'info>>,
        params: BuyNameParams,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );
        require!(discount_account_count <= 1, ArnsError::InvalidParameter);

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let name_hash = hash_name(&params.name);

        let (expected_reserved_pda, _) =
            Pubkey::find_program_address(&[RESERVED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.reserved_name_check.key() == expected_reserved_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_data = ctx.accounts.reserved_name_check.try_borrow_data()?;
            if reserved_data.len() > 8 {
                let reserved = ReservedName::try_deserialize(&mut &reserved_data[..])?;
                drop(reserved_data);
                let is_expired = reserved.expires_at.map_or(false, |exp| timestamp >= exp);
                if !is_expired {
                    match reserved.reserved_for {
                        Some(target) => {
                            require!(target == ctx.accounts.buyer.key(), ArnsError::NameReserved)
                        }
                        None => return Err(ArnsError::NameReserved.into()),
                    }
                }
            } else {
                drop(reserved_data);
            }
        }

        let (expected_returned_pda, _) =
            Pubkey::find_program_address(&[RETURNED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.returned_name_check.key() == expected_returned_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.returned_name_check.data_is_empty() {
            return Err(ArnsError::AuctionActive.into());
        }

        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let token_cost = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;
        require!(token_cost > 0, ArnsError::InvalidParameter);

        let split = discount_account_count as usize;
        require!(
            split <= ctx.remaining_accounts.len(),
            ArnsError::InvalidParameter
        );
        let (discount_accounts, funding_source_accounts) = ctx.remaining_accounts.split_at(split);
        let token_cost =
            try_apply_gateway_discount(token_cost, discount_accounts, &ctx.accounts.buyer.key())?;

        let gar_accounts = ario_gar::cpi::accounts::PayFromFundingPlan {
            settings: ctx.accounts.gar_settings.clone(),
            stake_token_account: ctx.accounts.stake_token_account.clone(),
            protocol_token_account: ctx.accounts.protocol_token_account.to_account_info(),
            payer_token_account: ctx.accounts.payer_token_account.clone(),
            payer: ctx.accounts.buyer.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
            withdrawal_counter: ctx.accounts.withdrawal_counter.clone(),
            system_program: ctx.accounts.system_program.to_account_info(),
        };
        cpi_pay_from_funding_plan(
            &ctx.accounts.gar_program.to_account_info(),
            gar_accounts,
            funding_source_accounts,
            sources,
            token_cost,
            residue_vault_count,
        )?;

        let record = &mut ctx.accounts.arns_record;
        record.name_hash = name_hash;
        record.name = params.name.to_lowercase();
        record.owner = ctx.accounts.buyer.key();
        record.ant = params.ant;
        record.purchase_type = params.purchase_type;
        record.start_timestamp = timestamp;
        record.end_timestamp = match params.purchase_type {
            PurchaseType::Lease => {
                let duration = (params.years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
                Some(
                    timestamp
                        .checked_add(duration)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
            }
            PurchaseType::Permabuy => None,
        };
        record.undername_limit = DEFAULT_UNDERNAME_COUNT as u16;
        record.purchase_price = token_cost;
        record.bump = ctx.bumps.arns_record;
        record.version = ARNS_RECORD_VERSION;

        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // Append to name registry (dynamic-capacity layout, ADR-020)
        let registry_info = &ctx.accounts.name_registry;
        let mut registry_data = registry_info.try_borrow_mut_data()?;
        let count = name_registry_header(&registry_data).count as u32;
        append_name_entry(
            &mut registry_data,
            NameEntry {
                name_hash,
                registry_index: count,
                _padding: [0u8; 4],
            },
        )?;

        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_info = ctx.accounts.reserved_name_check.to_account_info();
            let lamports = reserved_info.lamports();
            **reserved_info.try_borrow_mut_lamports()? = 0;
            **ctx
                .accounts
                .buyer
                .to_account_info()
                .try_borrow_mut_lamports()? += lamports;
            let mut data = reserved_info.try_borrow_mut_data()?;
            for byte in data.iter_mut() {
                *byte = 0;
            }
            reserved_info.assign(&anchor_lang::solana_program::system_program::ID);
        }

        emit!(NamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: record.name.clone(),
            purchase_type: match record.purchase_type {
                PurchaseType::Lease => PURCHASE_TYPE_LEASE,
                PurchaseType::Permabuy => PURCHASE_TYPE_PERMABUY,
            },
            years: match record.purchase_type {
                PurchaseType::Lease => params.years,
                PurchaseType::Permabuy => 0,
            },
            cost: token_cost,
            ant: params.ant,
            funding_source: FUNDING_SOURCE_FUNDING_PLAN,
            timestamp,
        });
        msg!(
            "ArNS name '{}' purchased from {}-source funding plan for {} mARIO",
            params.name,
            ctx.remaining_accounts.len() - split,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// BUY RETURNED NAME FROM FUNDING PLAN
// =========================================

pub mod buy_returned_name_from_funding_plan {
    use super::*;

    pub fn handler<'info>(
        ctx: Context<'_, '_, 'info, 'info, BuyReturnedNameFromFundingPlan<'info>>,
        params: BuyReturnedNameParams,
        sources: Vec<ario_gar::FundingSourceSpec>,
        discount_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );
        require!(discount_account_count <= 1, ArnsError::InvalidParameter);

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;
        let name_hash = hash_name(&params.name);

        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let registration_fee = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;

        let returned_name = &ctx.accounts.returned_name;
        let token_cost = calculate_returned_name_premium(
            registration_fee,
            returned_name.returned_at,
            timestamp,
        )?;
        require!(token_cost > 0, ArnsError::InvalidParameter);

        let split = discount_account_count as usize;
        require!(
            split <= ctx.remaining_accounts.len(),
            ArnsError::InvalidParameter
        );
        let (discount_accounts, funding_source_accounts) = ctx.remaining_accounts.split_at(split);
        let token_cost =
            try_apply_gateway_discount(token_cost, discount_accounts, &ctx.accounts.buyer.key())?;

        let initiator = returned_name.initiator;
        let is_protocol_initiator = initiator == ctx.accounts.config.key();
        let reward_for_protocol = if is_protocol_initiator {
            token_cost
        } else {
            token_cost / 2
        };
        let reward_for_initiator = token_cost
            .checked_sub(reward_for_protocol)
            .ok_or(ArnsError::ArithmeticUnderflow)?;

        let gar_accounts = ario_gar::cpi::accounts::PayFromFundingPlan {
            settings: ctx.accounts.gar_settings.clone(),
            stake_token_account: ctx.accounts.stake_token_account.clone(),
            protocol_token_account: ctx.accounts.protocol_token_account.to_account_info(),
            payer_token_account: ctx.accounts.payer_token_account.clone(),
            payer: ctx.accounts.buyer.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
            withdrawal_counter: ctx.accounts.withdrawal_counter.clone(),
            system_program: ctx.accounts.system_program.to_account_info(),
        };
        cpi_pay_from_funding_plan(
            &ctx.accounts.gar_program.to_account_info(),
            gar_accounts,
            funding_source_accounts,
            sources,
            reward_for_protocol,
            residue_vault_count,
        )?;

        if reward_for_initiator > 0 && !is_protocol_initiator {
            let cpi_accounts = SplTransfer {
                from: ctx.accounts.buyer_token_account.to_account_info(),
                to: ctx.accounts.initiator_token_account.to_account_info(),
                authority: ctx.accounts.buyer.to_account_info(),
            };
            let cpi_ctx =
                CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
            token::transfer(cpi_ctx, reward_for_initiator)?;
        }

        let record = &mut ctx.accounts.arns_record;
        record.name_hash = name_hash;
        record.name = params.name.to_lowercase();
        record.owner = ctx.accounts.buyer.key();
        record.ant = params.ant;
        record.purchase_type = params.purchase_type;
        record.start_timestamp = timestamp;
        record.end_timestamp = match params.purchase_type {
            PurchaseType::Lease => {
                let duration = (params.years as i64)
                    .checked_mul(ONE_YEAR_SECONDS)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
                Some(
                    timestamp
                        .checked_add(duration)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
            }
            PurchaseType::Permabuy => None,
        };
        record.undername_limit = DEFAULT_UNDERNAME_COUNT as u16;
        record.purchase_price = token_cost;
        record.bump = ctx.bumps.arns_record;
        record.version = ARNS_RECORD_VERSION;

        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // Append to name registry (dynamic-capacity layout, ADR-020)
        let registry_info = &ctx.accounts.name_registry;
        let mut registry_data = registry_info.try_borrow_mut_data()?;
        let count = name_registry_header(&registry_data).count as u32;
        append_name_entry(
            &mut registry_data,
            NameEntry {
                name_hash,
                registry_index: count,
                _padding: [0u8; 4],
            },
        )?;

        emit!(ReturnedNamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: record.name.clone(),
            cost: token_cost,
            premium: token_cost.saturating_sub(registration_fee),
            ant: params.ant,
            funding_source: FUNDING_SOURCE_FUNDING_PLAN,
            timestamp,
        });
        msg!("ArNS returned name '{}' purchased from funding plan for {} mARIO (protocol={}, initiator={})",
             params.name, token_cost, reward_for_protocol, reward_for_initiator);
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS — _from_withdrawal + _from_funding_plan
// =========================================

#[derive(Accounts)]
#[instruction(params: BuyNameParams)]
pub struct BuyNameFromWithdrawal<'info> {
    #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        init, payer = buyer, space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    /// CHECK: ReservedName PDA — verified in handler
    #[account(mut)]
    pub reserved_name_check: UncheckedAccount<'info>,

    /// CHECK: ReturnedName PDA — verified in handler
    pub returned_name_check: UncheckedAccount<'info>,

    /// CHECK: GarSettings PDA — validated by ario-gar CPI
    #[account(mut)]
    pub gar_settings: AccountInfo<'info>,

    /// CHECK: Withdrawal PDA — validated by ario-gar CPI (owner must == buyer)
    #[account(mut)]
    pub withdrawal: AccountInfo<'info>,

    /// CHECK: Stake token account — validated by ario-gar CPI
    #[account(mut)]
    pub stake_token_account: AccountInfo<'info>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(params: BuyReturnedNameParams)]
pub struct BuyReturnedNameFromWithdrawal<'info> {
    #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        mut,
        seeds = [RETURNED_NAME_SEED, &crate::pricing::hash_name(&params.name)],
        bump = returned_name.bump,
        close = buyer,
    )]
    pub returned_name: Box<Account<'info, ReturnedName>>,

    #[account(
        init, payer = buyer, space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    #[account(
        mut,
        constraint = buyer_token_account.owner == buyer.key(),
        constraint = buyer_token_account.mint == config.mint,
    )]
    pub buyer_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = initiator_token_account.owner == returned_name.initiator @ ArnsError::InvalidParameter,
        constraint = initiator_token_account.mint == config.mint,
    )]
    pub initiator_token_account: Box<Account<'info, TokenAccount>>,

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
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(params: BuyNameParams)]
pub struct BuyNameFromFundingPlan<'info> {
    #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        init, payer = buyer, space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    /// CHECK: ReservedName PDA — verified in handler
    #[account(mut)]
    pub reserved_name_check: UncheckedAccount<'info>,

    /// CHECK: ReturnedName PDA — verified in handler
    pub returned_name_check: UncheckedAccount<'info>,

    /// CHECK: GarSettings PDA — validated by ario-gar
    #[account(mut)]
    pub gar_settings: AccountInfo<'info>,

    // Per-source gateway PDAs live in remaining_accounts (multi-gateway).
    /// CHECK: Stake token account — validated by ario-gar
    #[account(mut)]
    pub stake_token_account: AccountInfo<'info>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: Optional payer SPL ATA — required when sources include Balance
    #[account(mut)]
    pub payer_token_account: Option<AccountInfo<'info>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: WithdrawalCounter PDA — created/validated by ario-gar's init_if_needed
    #[account(mut)]
    pub withdrawal_counter: AccountInfo<'info>,

    // Residue vault slots live in remaining_accounts after per-source PDAs.
    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(params: BuyReturnedNameParams)]
pub struct BuyReturnedNameFromFundingPlan<'info> {
    #[account(mut, seeds = [ARNS_CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArnsConfig>>,

    #[account(mut, seeds = [DEMAND_FACTOR_SEED], bump = demand_factor.bump)]
    pub demand_factor: Box<Account<'info, DemandFactor>>,

    #[account(
        mut,
        seeds = [RETURNED_NAME_SEED, &crate::pricing::hash_name(&params.name)],
        bump = returned_name.bump,
        close = buyer,
    )]
    pub returned_name: Box<Account<'info, ReturnedName>>,

    #[account(
        init, payer = buyer, space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    #[account(
        mut,
        constraint = buyer_token_account.owner == buyer.key(),
        constraint = buyer_token_account.mint == config.mint,
    )]
    pub buyer_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = initiator_token_account.owner == returned_name.initiator @ ArnsError::InvalidParameter,
        constraint = initiator_token_account.mint == config.mint,
    )]
    pub initiator_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: GarSettings PDA — validated by ario-gar
    #[account(mut)]
    pub gar_settings: AccountInfo<'info>,

    /// CHECK: Stake token account
    #[account(mut)]
    pub stake_token_account: AccountInfo<'info>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: Optional payer SPL ATA
    #[account(mut)]
    pub payer_token_account: Option<AccountInfo<'info>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    /// CHECK: WithdrawalCounter PDA
    #[account(mut)]
    pub withdrawal_counter: AccountInfo<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    // Remaining accounts layout: per-source PDAs from ario-gar followed by
    // residue_vault PDAs (count = residue_vault_count). See lib.rs docs.
}
