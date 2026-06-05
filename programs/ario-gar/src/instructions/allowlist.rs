use anchor_lang::prelude::*;

use crate::error::GarError;
use crate::state::*;
use crate::{AllowlistToggledEvent, DelegateAllowlistedEvent};

/// Add a delegate to the gateway's allowlist (F20)
/// Matches Lua: gar.allowDelegates
pub fn allow_delegate(ctx: Context<AllowDelegate>) -> Result<()> {
    let entry = &mut ctx.accounts.allowlist_entry;
    entry.gateway = ctx.accounts.gateway.operator;
    entry.delegate = ctx.accounts.delegate.key();
    // L-9: Set added_at timestamp
    let now = Clock::get()?.unix_timestamp;
    entry.added_at = now;
    entry.bump = ctx.bumps.allowlist_entry;
    entry.version = ALLOWLIST_ENTRY_VERSION;

    emit!(DelegateAllowlistedEvent {
        operator: entry.gateway,
        delegate: entry.delegate,
        allowed: true,
        timestamp: now,
    });

    msg!(
        "Delegate {} allowed on gateway {}",
        entry.delegate,
        entry.gateway
    );
    Ok(())
}

/// Remove a delegate from the gateway's allowlist (F20)
/// If delegate still has stake, they keep it but cannot add more after removal.
/// Matches Lua: gar.disallowDelegates
pub fn disallow_delegate(ctx: Context<DisallowDelegate>) -> Result<()> {
    // Read pubkeys before the close constraint takes effect at exit.
    let operator = ctx.accounts.operator.key();
    let delegate = ctx.accounts.delegate.key();
    let timestamp = Clock::get()?.unix_timestamp;

    emit!(DelegateAllowlistedEvent {
        operator,
        delegate,
        allowed: false,
        timestamp,
    });

    // Account is closed via close constraint
    msg!("Delegate removed from allowlist");
    Ok(())
}

/// Enable or disable the allowlist for a gateway
pub fn set_allowlist_enabled(
    ctx: Context<super::gateway::UpdateGatewaySettings>,
    enabled: bool,
) -> Result<()> {
    ctx.accounts.gateway.settings.allowlist_enabled = enabled;
    let timestamp = Clock::get()?.unix_timestamp;

    emit!(AllowlistToggledEvent {
        operator: ctx.accounts.gateway.operator,
        enabled,
        timestamp,
    });

    msg!("Allowlist enabled: {}", enabled);
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct AllowDelegate<'info> {
    #[account(
        seeds = [GATEWAY_SEED, operator.key().as_ref()],
        bump = gateway.bump,
        constraint = gateway.operator == operator.key() @ GarError::NotOperator,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        init,
        payer = operator,
        space = AllowlistEntry::SIZE,
        seeds = [ALLOWLIST_SEED, operator.key().as_ref(), delegate.key().as_ref()],
        bump,
    )]
    pub allowlist_entry: Account<'info, AllowlistEntry>,

    /// CHECK: Delegate pubkey, does not need to sign
    pub delegate: UncheckedAccount<'info>,

    #[account(mut)]
    pub operator: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct DisallowDelegate<'info> {
    #[account(
        seeds = [GATEWAY_SEED, operator.key().as_ref()],
        bump = gateway.bump,
        constraint = gateway.operator == operator.key() @ GarError::NotOperator,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        mut,
        seeds = [ALLOWLIST_SEED, operator.key().as_ref(), delegate.key().as_ref()],
        bump = allowlist_entry.bump,
        close = operator,
    )]
    pub allowlist_entry: Account<'info, AllowlistEntry>,

    /// CHECK: Delegate pubkey, does not need to sign
    pub delegate: UncheckedAccount<'info>,

    #[account(mut)]
    pub operator: Signer<'info>,
}
