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

#[cfg(test)]
mod decay_tests {
    //! Unit coverage for the linearly-decaying expedited-withdrawal penalty
    //! rate computed in `instant_withdrawal` (this file, ~lines 84-103).
    //!
    //! `expected_penalty_rate` below is a 1:1 mirror of that inline
    //! computation — it is NOT production code (it lives in a `#[cfg(test)]`
    //! module). If the source formula changes, this mirror and these
    //! assertions must change with it. The end-to-end interpolation against
    //! the real handler is covered by the `instant_withdrawal_*` integration
    //! tests in `tests/integration.rs`.
    use crate::RATE_SCALE;

    /// Byte-for-byte mirror of the penalty branch in `instant_withdrawal`.
    fn expected_penalty_rate(
        max_penalty: u64,
        min_penalty: u64,
        total_period: i64,
        created_at: i64,
        now: i64,
    ) -> u64 {
        let elapsed = now.saturating_sub(created_at).max(0);
        if total_period == 0 || elapsed >= total_period {
            min_penalty
        } else {
            let decay = (max_penalty.saturating_sub(min_penalty) as u128)
                .checked_mul(elapsed as u128)
                .unwrap()
                .checked_div(total_period as u128)
                .unwrap() as u64;
            let rate_after_decay = max_penalty.saturating_sub(decay);
            rate_after_decay.max(min_penalty).min(max_penalty)
        }
    }

    const MAX: u64 = 500_000; // default max_expedited_withdrawal_penalty (50%)
    const MIN: u64 = 100_000; // default min_expedited_withdrawal_penalty (10%)
    const PERIOD: i64 = 30 * 86_400; // default withdrawal_period (30 days)

    #[test]
    fn decay_at_known_elapsed_points() {
        // elapsed = 0  ->  exactly max (no decay yet)
        assert_eq!(expected_penalty_rate(MAX, MIN, PERIOD, 0, 0), MAX);

        // elapsed = period/4  ->  max - (max-min)/4 = 500_000 - 100_000 = 400_000
        let q = PERIOD / 4;
        assert_eq!(expected_penalty_rate(MAX, MIN, PERIOD, 0, q), 400_000);

        // elapsed = period/2  ->  max - (max-min)/2 = 500_000 - 200_000 = 300_000
        let half = PERIOD / 2;
        assert_eq!(expected_penalty_rate(MAX, MIN, PERIOD, 0, half), 300_000);

        // elapsed = period - 1  ->  just above min, NOT yet min (boundary is `>=`)
        let almost = PERIOD - 1;
        let rate_almost = expected_penalty_rate(MAX, MIN, PERIOD, 0, almost);
        assert!(
            rate_almost > MIN && rate_almost < MIN + 100,
            "at period-1 the rate should be just above min, got {rate_almost}"
        );

        // elapsed = period  ->  exactly min (the `elapsed >= total_period` arm)
        assert_eq!(expected_penalty_rate(MAX, MIN, PERIOD, 0, PERIOD), MIN);

        // elapsed = period + 1  ->  still min (clamped via the `>=` arm)
        assert_eq!(expected_penalty_rate(MAX, MIN, PERIOD, 0, PERIOD + 1), MIN);
    }

    #[test]
    fn boundary_is_inclusive_at_period() {
        // The source gate is `elapsed >= total_period` (not `>`), so the
        // transition to the minimum rate happens AT the period boundary, not
        // one second after it. Pin that: period-1 is still decaying, period
        // is already min.
        let at_period_minus_one = expected_penalty_rate(MAX, MIN, PERIOD, 0, PERIOD - 1);
        let at_period = expected_penalty_rate(MAX, MIN, PERIOD, 0, PERIOD);
        assert!(at_period_minus_one > MIN, "period-1 still decaying");
        assert_eq!(at_period, MIN, "period boundary is inclusive -> min");
    }

    #[test]
    fn zero_total_period_yields_min() {
        // total_period == 0 short-circuits to min (avoids div-by-zero) for any
        // elapsed, including elapsed == 0.
        assert_eq!(expected_penalty_rate(MAX, MIN, 0, 0, 0), MIN);
        assert_eq!(expected_penalty_rate(MAX, MIN, 0, 0, 123_456), MIN);
    }

    #[test]
    fn result_always_within_min_max_clamp() {
        // Sweep elapsed across (and beyond) the period; the rate must never
        // leave [min, max] regardless of input.
        for num in 0..=40i64 {
            let now = (PERIOD * num) / 30; // 0 .. ~1.33 * period
            let rate = expected_penalty_rate(MAX, MIN, PERIOD, 0, now);
            assert!(
                (MIN..=MAX).contains(&rate),
                "rate {rate} out of [{MIN},{MAX}] at elapsed {now}"
            );
        }
        // A future-clock created_at (now < created_at) clamps elapsed to 0
        // via `.max(0)`, so the rate is max (no negative decay).
        assert_eq!(expected_penalty_rate(MAX, MIN, PERIOD, 1_000, 500), MAX);
    }

    #[test]
    fn fee_and_payout_track_the_rate() {
        // The fee is amount * rate / RATE_SCALE, payout = amount - fee.
        // Sanity-check the conversion the handler performs after computing the
        // rate, at mid-period (rate = 300_000 = 30%).
        let amount: u64 = 1_000_000;
        let rate = expected_penalty_rate(MAX, MIN, PERIOD, 0, PERIOD / 2);
        let fee = (amount as u128 * rate as u128 / RATE_SCALE as u128) as u64;
        let payout = amount - fee;
        assert_eq!(rate, 300_000);
        assert_eq!(fee, 300_000);
        assert_eq!(payout, 700_000);
    }
}
