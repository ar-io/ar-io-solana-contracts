use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::GarError;
use crate::state::*;
use crate::{OperatorStakeIncreasedEvent, WithdrawalCreatedEvent};

pub fn increase_operator_stake(ctx: Context<IncreaseOperatorStake>, amount: u64) -> Result<()> {
    require!(amount > 0, GarError::InvalidAmount);

    // Stake can only be increased on a Joined gateway (matches Lua assertion).
    // Explicitly compares against Joined so Gone is also rejected.
    let gateway = &ctx.accounts.gateway;
    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayLeaving
    );

    let cpi_accounts = SplTransfer {
        from: ctx.accounts.operator_token_account.to_account_info(),
        to: ctx.accounts.stake_token_account.to_account_info(),
        authority: ctx.accounts.operator.to_account_info(),
    };
    let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
    token::transfer(cpi_ctx, amount)?;

    let gateway = &mut ctx.accounts.gateway;
    gateway.operator_stake = gateway
        .operator_stake
        .checked_add(amount)
        .ok_or(GarError::ArithmeticOverflow)?;
    let new_total = gateway.operator_stake;
    let operator_pk = gateway.operator;

    emit!(OperatorStakeIncreasedEvent {
        operator: operator_pk,
        added: amount,
        new_total,
        timestamp: Clock::get()?.unix_timestamp,
    });

    // Supply counter: operator stake increased
    let settings = &mut ctx.accounts.settings;
    settings.total_staked = settings
        .total_staked
        .checked_add(amount)
        .ok_or(GarError::ArithmeticOverflow)?;

    msg!("Operator stake increased by {}", amount);
    Ok(())
}

/// Decrease operator stake - creates withdrawal (F14)
pub fn decrease_operator_stake(ctx: Context<DecreaseOperatorStake>, amount: u64) -> Result<()> {
    let clock = Clock::get()?;
    let settings = &ctx.accounts.settings;
    let gateway = &mut ctx.accounts.gateway;

    require!(amount > 0, GarError::InvalidAmount);

    let remaining = gateway
        .operator_stake
        .checked_sub(amount)
        .ok_or(GarError::InsufficientStake)?;

    require!(
        remaining >= settings.min_operator_stake || remaining == 0,
        GarError::StakeBelowMinimum
    );

    // If stake goes to 0, gateway must leave
    if remaining == 0 {
        require!(
            gateway.status == GatewayStatus::Leaving,
            GarError::MustLeaveFirst
        );
    }

    gateway.operator_stake = remaining;

    // Create withdrawal
    let withdrawal = &mut ctx.accounts.withdrawal;
    let counter = &mut ctx.accounts.withdrawal_counter;

    withdrawal.owner = ctx.accounts.operator.key();
    withdrawal.withdrawal_id = counter.next_id;
    withdrawal.gateway = gateway.operator;
    withdrawal.amount = amount;
    withdrawal.created_at = clock.unix_timestamp;
    withdrawal.available_at = clock
        .unix_timestamp
        .checked_add(settings.withdrawal_period)
        .ok_or(GarError::ArithmeticOverflow)?;
    withdrawal.is_delegate = false;
    withdrawal.is_exit_vault = false;
    withdrawal.is_protected = false;
    withdrawal.bump = ctx.bumps.withdrawal;

    counter.next_id = counter
        .next_id
        .checked_add(1)
        .ok_or(GarError::ArithmeticOverflow)?;

    emit!(WithdrawalCreatedEvent {
        owner: withdrawal.owner,
        withdrawal_id: withdrawal.withdrawal_id,
        amount,
        available_at: withdrawal.available_at,
        timestamp: clock.unix_timestamp,
    });

    // Supply counter: operator stake moved to withdrawal
    let settings = &mut ctx.accounts.settings;
    settings.total_staked = settings.total_staked.saturating_sub(amount);
    settings.total_withdrawn = settings
        .total_withdrawn
        .checked_add(amount)
        .ok_or(GarError::ArithmeticOverflow)?;

    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct IncreaseOperatorStake<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, operator.key().as_ref()],
        bump = gateway.bump,
        constraint = gateway.operator == operator.key() @ GarError::NotOperator,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        mut,
        constraint = operator_token_account.owner == operator.key(),
        constraint = operator_token_account.mint == settings.mint,
    )]
    pub operator_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = stake_token_account.mint == settings.mint,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub operator: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct DecreaseOperatorStake<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, operator.key().as_ref()],
        bump = gateway.bump,
        constraint = gateway.operator == operator.key() @ GarError::NotOperator,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        init_if_needed,
        payer = operator,
        space = WithdrawalCounter::SIZE,
        seeds = [WITHDRAWAL_COUNTER_SEED, operator.key().as_ref()],
        bump,
    )]
    pub withdrawal_counter: Account<'info, WithdrawalCounter>,

    #[account(
        init,
        payer = operator,
        space = Withdrawal::SIZE,
        seeds = [WITHDRAWAL_SEED, operator.key().as_ref(), &withdrawal_counter.next_id.to_le_bytes()],
        bump,
    )]
    pub withdrawal: Account<'info, Withdrawal>,

    #[account(mut)]
    pub operator: Signer<'info>,

    pub system_program: Program<'info, System>,
}
