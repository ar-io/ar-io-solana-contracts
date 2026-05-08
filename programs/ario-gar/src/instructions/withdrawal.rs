use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::GarError;
use crate::state::*;
use crate::{InstantWithdrawalEvent, WithdrawalCancelledEvent, WithdrawalClaimedEvent, RATE_SCALE};

/// Claim matured withdrawal
pub fn claim_withdrawal(ctx: Context<ClaimWithdrawal>) -> Result<()> {
    let clock = Clock::get()?;
    let withdrawal = &ctx.accounts.withdrawal;

    require!(
        clock.unix_timestamp >= withdrawal.available_at,
        GarError::WithdrawalNotReady
    );

    let amount = withdrawal.amount;
    let owner_key = withdrawal.owner;
    let withdrawal_id = withdrawal.withdrawal_id;

    // Transfer tokens using Settings PDA as authority over stake_token_account
    let settings_bump = ctx.accounts.settings.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[SETTINGS_SEED, &[settings_bump]]];

    let cpi_accounts = SplTransfer {
        from: ctx.accounts.stake_token_account.to_account_info(),
        to: ctx.accounts.owner_token_account.to_account_info(),
        authority: ctx.accounts.settings.to_account_info(),
    };
    let cpi_ctx = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        cpi_accounts,
        signer_seeds,
    );
    token::transfer(cpi_ctx, amount)?;

    emit!(WithdrawalClaimedEvent {
        owner: owner_key,
        withdrawal_id,
        amount,
        timestamp: clock.unix_timestamp,
    });

    // Supply counter: withdrawal claimed (tokens leave the system)
    let settings = &mut ctx.accounts.settings;
    settings.total_withdrawn = settings.total_withdrawn.saturating_sub(amount);

    Ok(())
}

/// Expedited (instant) withdrawal with time-decaying penalty (F19)
/// Penalty decays linearly from max to min as time elapses in the withdrawal period.
/// Matches Lua processInstantWithdrawal: penaltyRate = max - (max - min) * elapsed / total
///
/// **Lua-parity gate**: `is_protected: true` rejects with `ProtectedVault`.
/// The protected vault holds the operator's minimum stake at leave/prune
/// time and matches Lua's "Gateway operator minimum stake vault cannot be
/// instantly withdrawn" assertion (`gar.lua::instantGatewayWithdrawal`).
/// The non-protected excess vault from a leave is still expedite-able.
pub fn instant_withdrawal(ctx: Context<InstantWithdrawal>) -> Result<()> {
    let clock = Clock::get()?;
    let settings = &ctx.accounts.settings;
    let withdrawal = &ctx.accounts.withdrawal;

    require!(!withdrawal.is_protected, GarError::ProtectedVault);

    let amount = withdrawal.amount;
    require!(
        amount >= settings.min_expedited_withdrawal_amount,
        GarError::InvalidWithdrawalAmount
    );

    // Calculate time-decaying penalty rate
    let max_penalty = settings.max_expedited_withdrawal_penalty;
    let min_penalty = settings.min_expedited_withdrawal_penalty;
    let total_period = settings.withdrawal_period;

    let elapsed = clock
        .unix_timestamp
        .saturating_sub(withdrawal.created_at)
        .max(0);

    let penalty_rate = if total_period == 0 || elapsed >= total_period {
        // Full period elapsed: use minimum penalty
        min_penalty
    } else {
        // Linear decay: max - (max - min) * elapsed / total. Both checked
        // ops are unreachable at current bounds — (max-min) ≤ RATE_SCALE
        // (1e6), elapsed < total_period ≤ i64::MAX, and the outer branch
        // gates total_period > 0. The `?` is defense-in-depth: a future
        // bound regression fails the tx with ArithmeticOverflow instead
        // of silently zeroing the decay (which would lock penalty at the
        // maximum rate).
        let decay = (max_penalty.saturating_sub(min_penalty) as u128)
            .checked_mul(elapsed as u128)
            .ok_or(GarError::ArithmeticOverflow)?
            .checked_div(total_period as u128)
            .ok_or(GarError::ArithmeticOverflow)? as u64;
        let rate_after_decay = max_penalty.saturating_sub(decay);
        // Clamp to [min, max]
        rate_after_decay.max(min_penalty).min(max_penalty)
    };

    let fee = (amount as u128)
        .checked_mul(penalty_rate as u128)
        .ok_or(GarError::ArithmeticOverflow)?
        .checked_div(RATE_SCALE as u128)
        .ok_or(GarError::ArithmeticOverflow)? as u64;
    let payout = amount.saturating_sub(fee);

    let owner_key = withdrawal.owner;
    let withdrawal_id = withdrawal.withdrawal_id;

    // Transfer using Settings PDA as authority over stake_token_account
    let settings_bump = ctx.accounts.settings.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[SETTINGS_SEED, &[settings_bump]]];

    // Transfer payout to owner
    let cpi_accounts = SplTransfer {
        from: ctx.accounts.stake_token_account.to_account_info(),
        to: ctx.accounts.owner_token_account.to_account_info(),
        authority: ctx.accounts.settings.to_account_info(),
    };
    let cpi_ctx = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        cpi_accounts,
        signer_seeds,
    );
    token::transfer(cpi_ctx, payout)?;

    // Transfer fee to protocol
    let cpi_accounts_fee = SplTransfer {
        from: ctx.accounts.stake_token_account.to_account_info(),
        to: ctx.accounts.protocol_token_account.to_account_info(),
        authority: ctx.accounts.settings.to_account_info(),
    };
    let cpi_ctx_fee = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        cpi_accounts_fee,
        signer_seeds,
    );
    token::transfer(cpi_ctx_fee, fee)?;

    emit!(InstantWithdrawalEvent {
        owner: owner_key,
        withdrawal_id,
        amount,
        fee,
        payout,
        timestamp: clock.unix_timestamp,
    });

    // Supply counter: withdrawal claimed (tokens leave the system)
    let settings = &mut ctx.accounts.settings;
    settings.total_withdrawn = settings.total_withdrawn.saturating_sub(amount);

    Ok(())
}

/// Close a fully-drained Withdrawal vault and refund rent to the original owner.
/// Permissionless: any signer can cleanup once `withdrawal.amount == 0`.
///
/// Why split from `deduct_withdrawal_for_payment`: Anchor's `close = owner`
/// constraint fires unconditionally — there's no way to express "close iff
/// fully drained" inline. Splitting lets `deduct_withdrawal_for_payment`
/// handle partial drains (vault stays open) while this ix handles the
/// full-drain cleanup case.
///
/// Rent goes to `withdrawal.owner` (bound via the close + address constraint),
/// not the caller — so a third-party closer can't steal rent.
pub fn close_drained_withdrawal(ctx: Context<CloseDrainedWithdrawal>) -> Result<()> {
    require!(
        ctx.accounts.withdrawal.amount == 0,
        GarError::WithdrawalNotDrained
    );
    // Anchor's `close = owner` constraint on the Accounts struct does the lamport transfer.
    Ok(())
}

/// Cancel a pending withdrawal, returning stake to the gateway (F18)
/// Matches Lua: gar.cancelGatewayWithdrawal — returns vault balance to operator_stake or delegate.amount
pub fn cancel_withdrawal(ctx: Context<CancelWithdrawal>) -> Result<()> {
    let clock = Clock::get()?;
    let gateway = &mut ctx.accounts.gateway;
    let withdrawal = &ctx.accounts.withdrawal;

    // Cancellation is only valid on a Joined gateway (matches Lua assertion).
    // Explicitly compares against Joined so Gone is also rejected.
    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayLeaving
    );

    let amount = withdrawal.amount;

    if withdrawal.is_delegate {
        // Cancel delegate withdrawal: return to delegation
        let delegation = ctx
            .accounts
            .delegation
            .as_mut()
            .ok_or(GarError::DelegationNotFound)?;
        // Gateway must still allow staking (matches Lua assert)
        require!(
            gateway.settings.allow_delegated_staking,
            GarError::DelegationNotAllowed
        );
        // Settle pending rewards BEFORE adding cancelled amount back.
        // Without this, the returned tokens would retroactively earn rewards
        // from the period they were in the withdrawal vault.
        settle_delegate_rewards(gateway, delegation);
        delegation.amount = delegation
            .amount
            .checked_add(amount)
            .ok_or(GarError::ArithmeticOverflow)?;
        // Update reward_debt so returned tokens don't earn retrospective rewards
        delegation.reward_debt = gateway.cumulative_reward_per_token;
        gateway.total_delegated_stake = gateway
            .total_delegated_stake
            .checked_add(amount)
            .ok_or(GarError::ArithmeticOverflow)?;
    } else {
        // Cancel operator withdrawal: return to operator_stake
        gateway.operator_stake = gateway
            .operator_stake
            .checked_add(amount)
            .ok_or(GarError::ArithmeticOverflow)?;
    }

    emit!(WithdrawalCancelledEvent {
        owner: withdrawal.owner,
        gateway: gateway.operator,
        withdrawal_id: withdrawal.withdrawal_id,
        amount,
        is_delegate: withdrawal.is_delegate,
        timestamp: clock.unix_timestamp,
    });

    // Supply counter: withdrawal cancelled, tokens return to staking
    let settings = &mut ctx.accounts.settings;
    settings.total_withdrawn = settings.total_withdrawn.saturating_sub(amount);
    if withdrawal.is_delegate {
        settings.total_delegated = settings
            .total_delegated
            .checked_add(amount)
            .ok_or(GarError::ArithmeticOverflow)?;
    } else {
        settings.total_staked = settings
            .total_staked
            .checked_add(amount)
            .ok_or(GarError::ArithmeticOverflow)?;
    }

    // Withdrawal account is closed by the close constraint in the account context

    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct ClaimWithdrawal<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [WITHDRAWAL_SEED, owner.key().as_ref(), &withdrawal.withdrawal_id.to_le_bytes()],
        bump = withdrawal.bump,
        close = owner,
        constraint = withdrawal.owner == owner.key() @ GarError::InvalidOwner,
    )]
    pub withdrawal: Account<'info, Withdrawal>,

    #[account(
        mut,
        constraint = stake_token_account.mint == settings.mint,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == settings.mint,
    )]
    pub owner_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct InstantWithdrawal<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [WITHDRAWAL_SEED, owner.key().as_ref(), &withdrawal.withdrawal_id.to_le_bytes()],
        bump = withdrawal.bump,
        close = owner,
        constraint = withdrawal.owner == owner.key() @ GarError::InvalidOwner,
    )]
    pub withdrawal: Account<'info, Withdrawal>,

    #[account(
        mut,
        constraint = stake_token_account.mint == settings.mint,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == settings.mint,
    )]
    pub owner_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = protocol_token_account.mint == settings.mint,
        constraint = protocol_token_account.key() == settings.protocol_token_account @ GarError::InvalidParameter,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct CancelWithdrawal<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, gateway.operator.as_ref()],
        bump = gateway.bump,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        mut,
        seeds = [WITHDRAWAL_SEED, owner.key().as_ref(), &withdrawal.withdrawal_id.to_le_bytes()],
        bump = withdrawal.bump,
        close = owner,
        constraint = withdrawal.owner == owner.key() @ GarError::InvalidOwner,
        constraint = withdrawal.gateway == gateway.operator.key() @ GarError::InvalidParameter,
    )]
    pub withdrawal: Account<'info, Withdrawal>,

    /// Optional delegation account (required when cancelling delegate withdrawal).
    /// Validated in the instruction handler when withdrawal.is_delegate == true.
    #[account(
        mut,
        seeds = [DELEGATION_SEED, gateway.operator.as_ref(), owner.key().as_ref()],
        bump = delegation.bump,
    )]
    pub delegation: Option<Account<'info, Delegation>>,

    #[account(mut)]
    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct CloseDrainedWithdrawal<'info> {
    #[account(
        mut,
        seeds = [WITHDRAWAL_SEED, withdrawal.owner.as_ref(), &withdrawal.withdrawal_id.to_le_bytes()],
        bump = withdrawal.bump,
        close = owner,
    )]
    pub withdrawal: Account<'info, Withdrawal>,

    /// CHECK: rent recipient bound to `withdrawal.owner` via the address
    /// constraint. The `close = owner` constraint above redirects lamports
    /// here, so a third-party `closer` cannot redirect rent to themselves.
    #[account(mut, address = withdrawal.owner)]
    pub owner: AccountInfo<'info>,

    /// Permissionless caller — anyone willing to pay the tx fee can clean up
    /// a drained vault. Rent always goes to `owner` regardless of who signs.
    pub closer: Signer<'info>,
}
