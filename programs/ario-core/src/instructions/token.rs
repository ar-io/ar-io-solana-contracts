use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::ArioError;
use crate::state::TransferEvent;

pub fn handler(ctx: Context<TransferTokens>, amount: u64) -> Result<()> {
    require!(amount > 0, ArioError::InvalidAmount);
    require!(
        ctx.accounts.from_token_account.key() != ctx.accounts.to_token_account.key(),
        ArioError::SelfTransfer
    );

    let cpi_accounts = SplTransfer {
        from: ctx.accounts.from_token_account.to_account_info(),
        to: ctx.accounts.to_token_account.to_account_info(),
        authority: ctx.accounts.authority.to_account_info(),
    };
    let cpi_program = ctx.accounts.token_program.to_account_info();
    let cpi_ctx = CpiContext::new(cpi_program, cpi_accounts);

    token::transfer(cpi_ctx, amount)?;

    // Emit event for indexers
    let clock = Clock::get()?;
    emit!(TransferEvent {
        from: ctx.accounts.from_token_account.key(),
        to: ctx.accounts.to_token_account.key(),
        amount,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct TransferTokens<'info> {
    #[account(
        mut,
        constraint = from_token_account.owner == authority.key() @ ArioError::InvalidOwner,
        constraint = from_token_account.mint == to_token_account.mint @ ArioError::InvalidAccountState,
    )]
    pub from_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub to_token_account: Account<'info, TokenAccount>,

    pub authority: Signer<'info>,

    pub token_program: Program<'info, Token>,
}
