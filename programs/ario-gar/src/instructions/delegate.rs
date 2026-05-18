use anchor_lang::prelude::*;
use anchor_lang::AccountDeserialize;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::GarError;
use crate::state::*;
use crate::{
    DelegationClosedEvent, DelegationDecreasedEvent, DelegationEvent, RedelegationEvent,
    RewardsCompoundedEvent, RATE_SCALE,
};

pub fn delegate_stake(ctx: Context<DelegateStake>, amount: u64) -> Result<()> {
    let clock = Clock::get()?;
    let gateway = &mut ctx.accounts.gateway;

    require!(amount > 0, GarError::InvalidAmount);
    require!(
        gateway.settings.allow_delegated_staking,
        GarError::DelegationNotAllowed
    );
    require!(
        amount >= gateway.settings.min_delegation_amount,
        GarError::DelegationBelowMinimum
    );
    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayNotJoined
    );

    // M-1: Prevent operator from delegating to their own gateway
    require!(
        ctx.accounts.delegator.key() != gateway.operator,
        GarError::CannotDelegateToSelf
    );

    // Allowlist enforcement (matches Lua: gar.delegateAllowedToStake)
    // If allowlist is enabled, delegate must be on the allowlist OR already have stake > 0
    if gateway.settings.allowlist_enabled {
        let delegation = &ctx.accounts.delegation;
        let already_staked = delegation.amount > 0;
        if !already_staked {
            // Must have an allowlist entry — check via remaining_accounts[0]
            let remaining = ctx.remaining_accounts;
            let delegator_key = ctx.accounts.delegator.key();
            let (expected_pda, _) = Pubkey::find_program_address(
                &[
                    ALLOWLIST_SEED,
                    gateway.operator.as_ref(),
                    delegator_key.as_ref(),
                ],
                ctx.program_id,
            );
            let has_allowlist_entry = !remaining.is_empty()
                && remaining[0].key() == expected_pda
                && remaining[0].owner == ctx.program_id;
            require!(has_allowlist_entry, GarError::DelegateNotAllowed);
        }
    }

    // Settle pending rewards for existing delegation
    let delegation = &mut ctx.accounts.delegation;
    if delegation.amount > 0 {
        settle_delegate_rewards(gateway, delegation);
    }

    let is_new = delegation.amount == 0;

    // Transfer tokens
    let cpi_accounts = SplTransfer {
        from: ctx.accounts.delegator_token_account.to_account_info(),
        to: ctx.accounts.stake_token_account.to_account_info(),
        authority: ctx.accounts.delegator.to_account_info(),
    };
    let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
    token::transfer(cpi_ctx, amount)?;

    // Update delegation
    delegation.gateway = gateway.operator;
    delegation.delegator = ctx.accounts.delegator.key();
    delegation.amount = delegation
        .amount
        .checked_add(amount)
        .ok_or(GarError::ArithmeticOverflow)?;

    if is_new {
        delegation.start_timestamp = clock.unix_timestamp;
        delegation.reward_debt = gateway.cumulative_reward_per_token;
        delegation.bump = ctx.bumps.delegation;
    }

    // Update gateway totals
    gateway.total_delegated_stake = gateway
        .total_delegated_stake
        .checked_add(amount)
        .ok_or(GarError::ArithmeticOverflow)?;

    emit!(DelegationEvent {
        delegator: delegation.delegator,
        gateway: gateway.operator,
        amount,
        total: delegation.amount,
        timestamp: clock.unix_timestamp,
    });

    // Supply counter: delegated stake increased
    let settings = &mut ctx.accounts.settings;
    settings.total_delegated = settings
        .total_delegated
        .checked_add(amount)
        .ok_or(GarError::ArithmeticOverflow)?;

    Ok(())
}

/// Decrease delegated stake - creates withdrawal (F16)
pub fn decrease_delegate_stake(ctx: Context<DecreaseDelegateStake>, amount: u64) -> Result<()> {
    let clock = Clock::get()?;
    let settings = &ctx.accounts.settings;
    let gateway = &mut ctx.accounts.gateway;
    let delegation = &mut ctx.accounts.delegation;

    require!(amount > 0, GarError::InvalidAmount);

    // Settle pending rewards before modifying amounts
    settle_delegate_rewards(gateway, delegation);

    require!(delegation.amount >= amount, GarError::InsufficientStake);

    let remaining = delegation.amount.saturating_sub(amount);
    // Remaining balance must be zero (full withdrawal) or >= min delegation amount
    require!(
        remaining == 0 || remaining >= gateway.settings.min_delegation_amount,
        GarError::DelegationBelowMinimum
    );

    delegation.amount = remaining;
    gateway.total_delegated_stake = gateway.total_delegated_stake.saturating_sub(amount);

    emit!(DelegationDecreasedEvent {
        delegator: ctx.accounts.delegator.key(),
        gateway: gateway.operator,
        decrease: amount,
        new_total: remaining,
        timestamp: clock.unix_timestamp,
    });

    // Create withdrawal
    let withdrawal = &mut ctx.accounts.withdrawal;
    let counter = &mut ctx.accounts.withdrawal_counter;

    withdrawal.owner = ctx.accounts.delegator.key();
    withdrawal.withdrawal_id = counter.next_id;
    withdrawal.gateway = gateway.operator;
    withdrawal.amount = amount;
    withdrawal.created_at = clock.unix_timestamp;
    withdrawal.available_at = clock
        .unix_timestamp
        .checked_add(settings.withdrawal_period)
        .ok_or(GarError::ArithmeticOverflow)?;
    withdrawal.is_delegate = true;
    withdrawal.is_exit_vault = false;
    withdrawal.is_protected = false;
    withdrawal.bump = ctx.bumps.withdrawal;

    counter.next_id = counter
        .next_id
        .checked_add(1)
        .ok_or(GarError::ArithmeticOverflow)?;

    // Supply counter: delegated stake moved to withdrawal
    let settings = &mut ctx.accounts.settings;
    settings.total_delegated = settings.total_delegated.saturating_sub(amount);
    settings.total_withdrawn = settings
        .total_withdrawn
        .checked_add(amount)
        .ok_or(GarError::ArithmeticOverflow)?;

    Ok(())
}

/// Close a delegation account with zero balance (permissionless cleanup)
/// Matches Lua: delegation pruning when delegatedStake == 0
pub fn close_empty_delegation(ctx: Context<CloseEmptyDelegation>) -> Result<()> {
    // Settle any remaining rewards before closing
    // (amount should be 0 per constraint, but settle to be safe with reward_debt)
    let gateway = &mut ctx.accounts.gateway;
    let delegation = &mut ctx.accounts.delegation;
    settle_delegate_rewards(gateway, delegation);

    let delegator_pk = delegation.delegator;
    let gateway_pk = delegation.gateway;
    let timestamp = Clock::get()?.unix_timestamp;

    emit!(DelegationClosedEvent {
        delegator: delegator_pk,
        gateway: gateway_pk,
        timestamp,
    });

    // Account closed by close constraint; validation is in the account context
    msg!("Empty delegation account closed");
    Ok(())
}

/// H2: Claim delegate stake from a leaving gateway.
/// Matches Lua: gar.leaveNetwork -> kickDelegateFromGateway for each delegate.
/// In Solana, delegates must claim individually (can't iterate PDAs on-chain).
/// Creates a withdrawal vault for the delegate's full stake from the leaving gateway.
pub fn claim_delegate_from_leaving_gateway(
    ctx: Context<ClaimDelegateFromLeavingGateway>,
) -> Result<()> {
    let clock = Clock::get()?;
    let gateway = &mut ctx.accounts.gateway;
    let delegation = &mut ctx.accounts.delegation;

    // Gateway must be leaving
    require!(
        gateway.status == GatewayStatus::Leaving,
        GarError::GatewayNotJoined
    );

    // Settle pending rewards before claiming
    settle_delegate_rewards(gateway, delegation);

    let amount = delegation.amount;
    require!(amount > 0, GarError::InvalidAmount);

    // Create withdrawal for delegate
    let withdrawal = &mut ctx.accounts.withdrawal;
    let counter = &mut ctx.accounts.withdrawal_counter;

    withdrawal.owner = ctx.accounts.delegator.key();
    withdrawal.withdrawal_id = counter.next_id;
    withdrawal.gateway = gateway.operator;
    withdrawal.amount = amount;
    withdrawal.created_at = clock.unix_timestamp;
    // Consistency fix: read from `settings.withdrawal_period` so the
    // `admin_set_withdrawal_period` lever applies to this path too.
    // Previously hardcoded to `WITHDRAWAL_LOCK_PERIOD` (the const), which
    // had the same default value but couldn't be overridden for testing /
    // ops. Mirrors the source `withdraw_delegation` handler (line 158).
    withdrawal.available_at = clock
        .unix_timestamp
        .checked_add(ctx.accounts.settings.withdrawal_period)
        .ok_or(GarError::ArithmeticOverflow)?;
    withdrawal.is_delegate = true;
    withdrawal.is_exit_vault = false;
    withdrawal.is_protected = false;
    withdrawal.bump = ctx.bumps.withdrawal;

    counter.next_id = counter
        .next_id
        .checked_add(1)
        .ok_or(GarError::ArithmeticOverflow)?;

    // Zero out delegation and reduce gateway totals
    gateway.total_delegated_stake = gateway.total_delegated_stake.saturating_sub(amount);
    delegation.amount = 0;

    emit!(DelegationEvent {
        delegator: ctx.accounts.delegator.key(),
        gateway: gateway.operator,
        amount: 0,
        total: 0,
        timestamp: clock.unix_timestamp,
    });

    // Supply counter: delegated stake moved to withdrawal
    let settings = &mut ctx.accounts.settings;
    settings.total_delegated = settings.total_delegated.saturating_sub(amount);
    settings.total_withdrawn = settings
        .total_withdrawn
        .checked_add(amount)
        .ok_or(GarError::ArithmeticOverflow)?;

    msg!(
        "Delegate {} claimed {} from leaving gateway {}",
        ctx.accounts.delegator.key(),
        amount,
        gateway.operator
    );

    Ok(())
}

/// Redelegate stake from one gateway to another (F17)
/// Fee = min(10% * redelegation_count, 60%) — first is free, resets every 7 days
/// Fee goes to protocol. Net amount moves to target delegation.
pub fn redelegate_stake(ctx: Context<RedelegateStake>, amount: u64) -> Result<()> {
    let clock = Clock::get()?;
    // Validate SPL token accounts here (not in #[derive(Accounts)]) to keep try_accounts stack under the SBF 4KB frame limit.
    {
        let settings = &ctx.accounts.settings;
        let d = ctx.accounts.delegator_token_account.try_borrow_data()?;
        let delegator_ta = TokenAccount::try_deserialize(&mut &d[..])?;
        require!(
            delegator_ta.owner == ctx.accounts.delegator.key(),
            GarError::InvalidParameter
        );
        require!(
            delegator_ta.mint == settings.mint,
            GarError::InvalidParameter
        );
        drop(d);
        let s = ctx.accounts.stake_token_account.try_borrow_data()?;
        let stake_ta = TokenAccount::try_deserialize(&mut &s[..])?;
        require!(stake_ta.mint == settings.mint, GarError::InvalidParameter);
        require!(
            ctx.accounts.stake_token_account.key() == settings.stake_token_account,
            GarError::InvalidParameter
        );
        drop(s);
        let p = ctx.accounts.protocol_token_account.try_borrow_data()?;
        let protocol_ta = TokenAccount::try_deserialize(&mut &p[..])?;
        require!(
            protocol_ta.mint == settings.mint,
            GarError::InvalidParameter
        );
        require!(
            ctx.accounts.protocol_token_account.key() == settings.protocol_token_account,
            GarError::InvalidParameter
        );
    }

    let source_gateway = &mut ctx.accounts.source_gateway;
    let target_gateway = &mut ctx.accounts.target_gateway;
    let source_delegation = &mut ctx.accounts.source_delegation;
    let target_delegation = &mut ctx.accounts.target_delegation;
    let redelegation = &mut ctx.accounts.redelegation_record;

    require!(amount > 0, GarError::InvalidAmount);
    require!(
        source_gateway.operator != target_gateway.operator,
        GarError::RedelegateSameGateway
    );
    require!(
        target_gateway.status == GatewayStatus::Joined,
        GarError::GatewayNotJoined
    );
    require!(
        target_gateway.settings.allow_delegated_staking,
        GarError::DelegationNotAllowed
    );

    // Prevent operator from redelegating to their own gateway
    require!(
        ctx.accounts.delegator.key() != target_gateway.operator,
        GarError::CannotDelegateToSelf
    );

    // Allowlist enforcement on target gateway (matches delegate_stake)
    if target_gateway.settings.allowlist_enabled {
        let already_staked = target_delegation.amount > 0;
        if !already_staked {
            let remaining = ctx.remaining_accounts;
            let delegator_key = ctx.accounts.delegator.key();
            let (expected_pda, _) = Pubkey::find_program_address(
                &[
                    ALLOWLIST_SEED,
                    target_gateway.operator.as_ref(),
                    delegator_key.as_ref(),
                ],
                ctx.program_id,
            );
            let has_allowlist_entry = !remaining.is_empty()
                && remaining[0].key() == expected_pda
                && remaining[0].owner == ctx.program_id;
            require!(has_allowlist_entry, GarError::DelegateNotAllowed);
        }
    }

    // Settle pending rewards on both delegations before balance checks
    settle_delegate_rewards(source_gateway, source_delegation);
    settle_delegate_rewards(target_gateway, target_delegation);

    // Balance checks AFTER settlement (settlement may increase source_delegation.amount)
    require!(
        source_delegation.amount >= amount,
        GarError::InsufficientStake
    );

    // Source remaining must be zero or >= min delegation (matches decrease_delegate_stake)
    let source_remaining = source_delegation.amount.saturating_sub(amount);
    require!(
        source_remaining == 0 || source_remaining >= source_gateway.settings.min_delegation_amount,
        GarError::DelegationBelowMinimum
    );

    // Calculate fee (resets if past reset window)
    let fee_rate = redelegation.get_fee_rate(clock.unix_timestamp, RATE_SCALE);
    let fee = (amount as u128)
        .checked_mul(fee_rate as u128)
        .ok_or(GarError::ArithmeticOverflow)?
        .checked_div(RATE_SCALE as u128)
        .ok_or(GarError::ArithmeticOverflow)? as u64;
    let net_amount = amount.saturating_sub(fee);

    require!(net_amount > 0, GarError::InvalidAmount);

    // L-4: Verify net amount meets target gateway's minimum delegation for new delegations
    if target_delegation.amount == 0 {
        require!(
            net_amount >= target_gateway.settings.min_delegation_amount,
            GarError::DelegationBelowMinimum
        );
    }

    // Deduct from source
    source_delegation.amount = source_delegation.amount.saturating_sub(amount);
    source_gateway.total_delegated_stake =
        source_gateway.total_delegated_stake.saturating_sub(amount);

    // Add net to target
    target_delegation.gateway = target_gateway.operator;
    target_delegation.delegator = ctx.accounts.delegator.key();
    target_delegation.amount = target_delegation
        .amount
        .checked_add(net_amount)
        .ok_or(GarError::ArithmeticOverflow)?;
    if target_delegation.start_timestamp == 0 {
        target_delegation.start_timestamp = clock.unix_timestamp;
        target_delegation.reward_debt = target_gateway.cumulative_reward_per_token;
        target_delegation.bump = ctx.bumps.target_delegation;
    }
    target_gateway.total_delegated_stake = target_gateway
        .total_delegated_stake
        .checked_add(net_amount)
        .ok_or(GarError::ArithmeticOverflow)?;

    // Transfer fee from stake pool to protocol (fee comes from staked tokens, not wallet)
    if fee > 0 {
        let settings_bump = ctx.accounts.settings.bump;
        let signer_seeds: &[&[&[u8]]] = &[&[SETTINGS_SEED, &[settings_bump]]];

        let cpi_accounts = SplTransfer {
            from: ctx.accounts.stake_token_account.to_account_info(),
            to: ctx.accounts.protocol_token_account.to_account_info(),
            authority: ctx.accounts.settings.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer_seeds,
        );
        token::transfer(cpi_ctx, fee)?;
    }

    // Update redelegation tracking
    if clock.unix_timestamp >= redelegation.fee_reset_at {
        redelegation.redelegation_count = 1;
    } else {
        redelegation.redelegation_count = redelegation
            .redelegation_count
            .checked_add(1)
            .ok_or(GarError::ArithmeticOverflow)?;
    }
    redelegation.delegator = ctx.accounts.delegator.key();
    redelegation.last_redelegation_at = clock.unix_timestamp;
    redelegation.fee_reset_at = clock
        .unix_timestamp
        .checked_add(RedelegationRecord::FEE_RESET_INTERVAL)
        .ok_or(GarError::ArithmeticOverflow)?;

    // Supply counter: fee leaves the delegated pool (goes to protocol)
    if fee > 0 {
        let settings = &mut ctx.accounts.settings;
        settings.total_delegated = settings.total_delegated.saturating_sub(fee);
    }

    emit!(RedelegationEvent {
        delegator: ctx.accounts.delegator.key(),
        from_gateway: source_gateway.operator,
        to_gateway: target_gateway.operator,
        amount,
        fee,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "Redelegated {} (fee {}), net {} to target",
        amount,
        fee,
        net_amount
    );
    Ok(())
}

/// Compound delegation rewards by settling the reward-per-share accumulator.
/// Delegates call this to materialize pending rewards into their delegation amount.
pub fn compound_delegation_rewards(ctx: Context<CompoundDelegationRewards>) -> Result<()> {
    let gateway = &mut ctx.accounts.gateway;
    let delegation = &mut ctx.accounts.delegation;
    // Snapshot the pre-settle amount; settle_delegate_rewards adds the
    // pending payout into `delegation.amount`. The delta is what we
    // emit as `compounded` (zero is fine — caller may settle just to
    // refresh `reward_debt`).
    let before = delegation.amount;
    let delegator_pk = delegation.delegator;
    let gateway_pk = delegation.gateway;
    settle_delegate_rewards(gateway, delegation);
    let compounded = delegation.amount.saturating_sub(before);

    emit!(RewardsCompoundedEvent {
        delegator: delegator_pk,
        gateway: gateway_pk,
        compounded,
        timestamp: Clock::get()?.unix_timestamp,
    });

    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct DelegateStake<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Box<Account<'info, GatewaySettings>>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, gateway.operator.as_ref()],
        bump = gateway.bump,
    )]
    pub gateway: Box<Account<'info, Gateway>>,

    #[account(
        init_if_needed,
        payer = delegator,
        space = Delegation::SIZE,
        seeds = [DELEGATION_SEED, gateway.operator.as_ref(), delegator.key().as_ref()],
        bump,
    )]
    pub delegation: Box<Account<'info, Delegation>>,

    #[account(
        mut,
        constraint = delegator_token_account.owner == delegator.key(),
        constraint = delegator_token_account.mint == settings.mint,
    )]
    pub delegator_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = stake_token_account.mint == settings.mint,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub delegator: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DecreaseDelegateStake<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Box<Account<'info, GatewaySettings>>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, gateway.operator.as_ref()],
        bump = gateway.bump,
    )]
    pub gateway: Box<Account<'info, Gateway>>,

    #[account(
        mut,
        seeds = [DELEGATION_SEED, gateway.operator.as_ref(), delegator.key().as_ref()],
        bump = delegation.bump,
        constraint = delegation.delegator == delegator.key() @ GarError::NotDelegator,
    )]
    pub delegation: Box<Account<'info, Delegation>>,

    #[account(
        init_if_needed,
        payer = delegator,
        space = WithdrawalCounter::SIZE,
        seeds = [WITHDRAWAL_COUNTER_SEED, delegator.key().as_ref()],
        bump,
    )]
    pub withdrawal_counter: Box<Account<'info, WithdrawalCounter>>,

    #[account(
        init,
        payer = delegator,
        space = Withdrawal::SIZE,
        seeds = [WITHDRAWAL_SEED, delegator.key().as_ref(), &withdrawal_counter.next_id.to_le_bytes()],
        bump,
    )]
    pub withdrawal: Box<Account<'info, Withdrawal>>,

    #[account(mut)]
    pub delegator: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseEmptyDelegation<'info> {
    #[account(
        mut,
        seeds = [GATEWAY_SEED, delegation.gateway.as_ref()],
        bump = gateway.bump,
    )]
    pub gateway: Account<'info, Gateway>,

    /// GAR-016: Rent is returned to the original delegator, not the caller,
    /// to prevent griefing where a cranker harvests rent from other users' accounts.
    #[account(
        mut,
        seeds = [DELEGATION_SEED, delegation.gateway.as_ref(), delegation.delegator.as_ref()],
        bump = delegation.bump,
        close = delegator,
        constraint = delegation.amount == 0 @ GarError::InvalidAmount,
    )]
    pub delegation: Account<'info, Delegation>,

    /// The original delegator — receives rent refund on close.
    /// CHECK: Validated by constraint: delegation.delegator == delegator.key()
    #[account(
        mut,
        constraint = delegator.key() == delegation.delegator @ GarError::NotDelegator,
    )]
    pub delegator: UncheckedAccount<'info>,

    /// Anyone can close an empty delegation (permissionless)
    pub payer: Signer<'info>,
}

/// Permissionless claim of a delegate's stake from a leaving gateway.
///
/// Anyone can crank this on a delegate's behalf — the stake still routes to
/// the delegate's own Withdrawal vault (PDA-seeded by `delegator.key()`),
/// and the delegation PDA's seed binding ensures the caller can't redirect
/// someone else's stake by passing a different `delegator` pubkey.
///
/// This unblocks the future `finalize_gone` GC: that instruction requires
/// `gateway.total_delegated_stake == 0` before closing the Gateway PDA, and
/// without permissionless cranking a forgetful delegate would strand both
/// their own stake AND the gateway slot indefinitely (Lua's `pruneGateways`
/// at gar.lua:1057 makes the same condition: no remaining vaults). Matches
/// Lua's `gar.leaveNetwork` behavior of kicking all delegates to vaults
/// atomically.
#[derive(Accounts)]
pub struct ClaimDelegateFromLeavingGateway<'info> {
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
        constraint = gateway.status == GatewayStatus::Leaving @ GarError::GatewayNotJoined,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        mut,
        seeds = [DELEGATION_SEED, gateway.operator.as_ref(), delegator.key().as_ref()],
        bump = delegation.bump,
        constraint = delegation.delegator == delegator.key() @ GarError::NotDelegator,
        constraint = delegation.amount > 0 @ GarError::InvalidAmount,
    )]
    pub delegation: Account<'info, Delegation>,

    #[account(
        init_if_needed,
        payer = payer,
        space = WithdrawalCounter::SIZE,
        seeds = [WITHDRAWAL_COUNTER_SEED, delegator.key().as_ref()],
        bump,
    )]
    pub withdrawal_counter: Account<'info, WithdrawalCounter>,

    #[account(
        init,
        payer = payer,
        space = Withdrawal::SIZE,
        seeds = [WITHDRAWAL_SEED, delegator.key().as_ref(), &withdrawal_counter.next_id.to_le_bytes()],
        bump,
    )]
    pub withdrawal: Account<'info, Withdrawal>,

    /// CHECK: the delegator's pubkey is bound by the delegation PDA seeds
    /// (the seeds constraint above re-derives `[DELEGATION_SEED, gateway,
    /// delegator]` and requires it match the passed account address). No
    /// signature required — the caller cannot redirect anyone's stake by
    /// substituting a different pubkey here.
    pub delegator: AccountInfo<'info>,

    /// Pays for the rent on the withdrawal counter (`init_if_needed`) and
    /// the withdrawal vault (`init`). Typically the cranker. Permissionless:
    /// any signer who's willing to cover ~0.003 SOL of rent can crank this.
    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RedelegateStake<'info> {
    #[account(
        mut,
        seeds = [GATEWAY_SEED, source_gateway.operator.as_ref()],
        bump = source_gateway.bump,
    )]
    pub source_gateway: Box<Account<'info, Gateway>>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, target_gateway.operator.as_ref()],
        bump = target_gateway.bump,
    )]
    pub target_gateway: Box<Account<'info, Gateway>>,

    #[account(
        mut,
        seeds = [DELEGATION_SEED, source_gateway.operator.as_ref(), delegator.key().as_ref()],
        bump = source_delegation.bump,
        constraint = source_delegation.delegator == delegator.key() @ GarError::NotDelegator,
    )]
    pub source_delegation: Box<Account<'info, Delegation>>,

    #[account(
        init_if_needed,
        payer = delegator,
        space = Delegation::SIZE,
        seeds = [DELEGATION_SEED, target_gateway.operator.as_ref(), delegator.key().as_ref()],
        bump,
    )]
    pub target_delegation: Box<Account<'info, Delegation>>,

    #[account(
        init_if_needed,
        payer = delegator,
        space = RedelegationRecord::SIZE,
        seeds = [REDELEGATION_SEED, delegator.key().as_ref()],
        bump,
    )]
    pub redelegation_record: Box<Account<'info, RedelegationRecord>>,

    /// CHECK: SPL token account; deserialized in `redelegate_stake` and asserted to satisfy
    /// `owner == delegator` and `mint == settings.mint`. Kept as `UncheckedAccount` because adding
    /// three fully-deserialized `Box<Account<'_, TokenAccount>>` to this context overflows the
    /// SBF 4 KB stack frame inside Anchor's generated `try_accounts` (BPF stack-frame error).
    #[account(mut)]
    pub delegator_token_account: UncheckedAccount<'info>,

    /// CHECK: SPL token account; deserialized in `redelegate_stake` and asserted to match
    /// `settings.stake_token_account` and `mint == settings.mint`. Same SBF stack-frame
    /// rationale as `delegator_token_account`.
    #[account(mut)]
    pub stake_token_account: UncheckedAccount<'info>,

    /// CHECK: SPL token account; deserialized in `redelegate_stake` and asserted to match
    /// `settings.protocol_token_account` and `mint == settings.mint`. Same SBF stack-frame
    /// rationale as `delegator_token_account`.
    #[account(mut)]
    pub protocol_token_account: UncheckedAccount<'info>,

    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Box<Account<'info, GatewaySettings>>,

    #[account(mut)]
    pub delegator: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CompoundDelegationRewards<'info> {
    #[account(
        mut,
        seeds = [GATEWAY_SEED, gateway.operator.as_ref()],
        bump = gateway.bump,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        mut,
        seeds = [DELEGATION_SEED, gateway.operator.as_ref(), delegator.key().as_ref()],
        bump = delegation.bump,
        constraint = delegation.delegator == delegator.key() @ GarError::NotDelegator,
    )]
    pub delegation: Account<'info, Delegation>,

    pub delegator: Signer<'info>,
}
