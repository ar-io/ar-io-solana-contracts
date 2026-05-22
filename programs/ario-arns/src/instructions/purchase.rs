use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::ArnsError;
use crate::pricing::*;
use crate::state::*;
use crate::{
    BuyNameParams, BuyReturnedNameParams, NamePurchasedEvent, ReturnedNamePurchasedEvent,
    FUNDING_SOURCE_BALANCE, PURCHASE_TYPE_LEASE, PURCHASE_TYPE_PERMABUY,
};

pub mod buy_name {
    use super::*;

    pub fn handler(ctx: Context<BuyName>, params: BuyNameParams) -> Result<()> {
        // Validate name format
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );

        let clock = Clock::get()?;
        let timestamp = clock.unix_timestamp;

        // Compute name hash for PDA
        let name_hash = hash_name(&params.name);

        // C1: Check for reserved name (matches Lua: arns.buyRecord checks state.Reserved[name])
        // The reserved_name_check account must be the correct PDA; if it has data, a reservation exists.
        let (expected_reserved_pda, _) =
            Pubkey::find_program_address(&[RESERVED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.reserved_name_check.key() == expected_reserved_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.reserved_name_check.data_is_empty() {
            // Reservation exists — buyer must be the reserved_for target
            let reserved_data = ctx.accounts.reserved_name_check.try_borrow_data()?;
            if reserved_data.len() > 8 {
                let reserved = ReservedName::try_deserialize(&mut &reserved_data[..])?;
                drop(reserved_data);
                // Check if reservation has expired
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

        // C2: Check for active returned name auction (matches Lua: arns.buyRecord checks ReturnedNames[name])
        // If a returned name exists, buyer must use buy_returned_name instead to pay the premium.
        let (expected_returned_pda, _) =
            Pubkey::find_program_address(&[RETURNED_NAME_SEED, &name_hash], ctx.program_id);
        require!(
            ctx.accounts.returned_name_check.key() == expected_returned_pda,
            ArnsError::InvalidParameter
        );
        if !ctx.accounts.returned_name_check.data_is_empty() {
            // Returned name auction is active — must use buy_returned_name instruction
            return Err(ArnsError::AuctionActive.into());
        }

        // Validate years for lease
        if params.purchase_type == PurchaseType::Lease {
            require!(
                params.years >= 1 && params.years <= MAX_LEASE_LENGTH_YEARS as u8,
                ArnsError::InvalidLeaseDuration
            );
        }

        // Lazy demand factor rollover (matches Lua tick() behavior)
        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        // Calculate cost
        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let token_cost = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;

        require!(token_cost > 0, ArnsError::InvalidParameter);

        // Apply gateway operator discount if buyer passes a Gateway account
        let token_cost = try_apply_gateway_discount(
            token_cost,
            ctx.remaining_accounts,
            &ctx.accounts.buyer.key(),
        )?;

        // Transfer tokens from buyer to protocol
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.buyer_token_account.to_account_info(),
            to: ctx.accounts.protocol_token_account.to_account_info(),
            authority: ctx.accounts.buyer.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, token_cost)?;

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

        // Update config
        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        // Schedule prune if lease
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // ADR-016 reshape: ario-arns no longer touches MPL Core. SDK calls
        // ario-ant::sync_attributes in the same tx to populate the Attributes
        // plugin from this newly-created ArnsRecord.

        // BUG-7: Auto-remove reserved name entry after successful purchase.
        // Manual closure is required because reserved_name_check is an UncheckedAccount
        // (it may or may not exist), so Anchor's `close` constraint cannot be used.
        // Safety: PDA is validated above (line 24-30), and data_is_empty() guards the branch.
        if !ctx.accounts.reserved_name_check.data_is_empty() {
            let reserved_info = ctx.accounts.reserved_name_check.to_account_info();
            // Transfer lamports to buyer
            let lamports = reserved_info.lamports();
            **reserved_info.try_borrow_mut_lamports()? = 0;
            **ctx
                .accounts
                .buyer
                .to_account_info()
                .try_borrow_mut_lamports()? += lamports;
            // Zero out data
            let mut data = reserved_info.try_borrow_mut_data()?;
            for byte in data.iter_mut() {
                *byte = 0;
            }
            // Assign to system program
            reserved_info.assign(&anchor_lang::solana_program::system_program::ID);
        }

        // Plugin sync happens earlier in the handler (line 170) when the buyer
        // owns the ANT; for non-owner buyers `sync_attributes` recovers it.
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
            funding_source: FUNDING_SOURCE_BALANCE,
            timestamp,
        });

        msg!(
            "ArNS name '{}' purchased for {} mARIO",
            params.name,
            token_cost
        );
        Ok(())
    }
}

pub mod buy_returned_name {
    use super::*;

    pub fn handler(ctx: Context<BuyReturnedName>, params: BuyReturnedNameParams) -> Result<()> {
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

        // Lazy demand factor rollover (matches Lua tick() behavior)
        crate::instructions::demand::maybe_roll_demand_period(
            &mut ctx.accounts.demand_factor,
            timestamp,
        )?;

        // Calculate base registration fee
        let demand = &ctx.accounts.demand_factor;
        let base_fee = get_base_fee_for_name_length(&demand.fees, params.name.len())?;
        let registration_fee = calculate_registration_fee(
            base_fee,
            params.purchase_type,
            params.years,
            demand.current_demand_factor,
        )?;

        // Apply returned name premium (Dutch auction)
        let returned_name = &ctx.accounts.returned_name;
        let token_cost = calculate_returned_name_premium(
            registration_fee,
            returned_name.returned_at,
            timestamp,
        )?;

        require!(token_cost > 0, ArnsError::InvalidParameter);

        // Apply gateway operator discount if buyer passes a Gateway account
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

        // Transfer to protocol
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.buyer_token_account.to_account_info(),
            to: ctx.accounts.protocol_token_account.to_account_info(),
            authority: ctx.accounts.buyer.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, reward_for_protocol)?;

        // Transfer to initiator if applicable
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

        // Update config
        let config = &mut ctx.accounts.config;
        config.total_names_registered = config
            .total_names_registered
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;

        // Schedule prune if lease (M-8: match buy_name behavior)
        if let Some(end_ts) = record.end_timestamp {
            let prune_ts = end_ts
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

        // ReturnedName account closed by close constraint

        // ADR-016 reshape: ario-arns no longer touches MPL Core. SDK calls
        // ario-ant::sync_attributes in the same tx to populate traits.

        // `premium` = auction-uplift component of the final paid cost. Computed
        // as token_cost - registration_fee (saturating); if a gateway discount
        // pushed cost below the pre-discount registration fee this will be 0.
        // Indexers needing exact pre-discount premium should subscribe to
        // ario-gar's StakePaymentEvent / inner SPL transfers.
        emit!(ReturnedNamePurchasedEvent {
            buyer: ctx.accounts.buyer.key(),
            name: params.name.to_lowercase(),
            cost: token_cost,
            premium: token_cost.saturating_sub(registration_fee),
            ant: params.ant,
            funding_source: FUNDING_SOURCE_BALANCE,
            timestamp,
        });

        msg!(
            "Returned name '{}' purchased for {} mARIO",
            params.name,
            token_cost
        );
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
#[instruction(params: BuyNameParams)]
pub struct BuyName<'info> {
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
        init,
        payer = buyer,
        space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers (`append_name_entry`, etc.).
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
        constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: ReservedName PDA - verified in handler by deriving expected address.
    /// Must be the correct PDA even if no reservation exists (will have empty data).
    #[account(mut)]
    pub reserved_name_check: UncheckedAccount<'info>,

    /// CHECK: ReturnedName PDA - verified in handler by deriving expected address.
    /// Must be the correct PDA even if no returned name exists (will have empty data).
    pub returned_name_check: UncheckedAccount<'info>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    // ADR-016 reshape: ant_asset + mpl_core_program removed. ario-arns is now
    // MPL-agnostic — ArnsRecord stores `ant: Pubkey` (the asset key, treated
    // opaquely). SDK calls ario-ant::sync_attributes in the same tx to write
    // traits to the asset's Attributes plugin.
}

#[derive(Accounts)]
#[instruction(params: BuyReturnedNameParams)]
pub struct BuyReturnedName<'info> {
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
        seeds = [RETURNED_NAME_SEED, &crate::pricing::hash_name(&params.name)],
        bump = returned_name.bump,
        close = buyer,
    )]
    pub returned_name: Box<Account<'info, ReturnedName>>,

    #[account(
        init,
        payer = buyer,
        space = ArnsRecord::SIZE,
        seeds = [ARNS_RECORD_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub arns_record: Box<Account<'info, ArnsRecord>>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers (`append_name_entry`, etc.).
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
        constraint = protocol_token_account.key() == config.treasury @ ArnsError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = initiator_token_account.owner == returned_name.initiator @ ArnsError::InvalidParameter,
        constraint = initiator_token_account.mint == config.mint,
    )]
    pub initiator_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub buyer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    // ADR-016 reshape: ant_asset + mpl_core_program removed. SDK calls
    // ario-ant::sync_attributes in the same tx.
}
