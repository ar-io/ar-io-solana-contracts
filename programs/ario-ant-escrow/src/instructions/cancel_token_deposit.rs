use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer as SplTransfer};

use crate::{
    error::EscrowError,
    events::ASSET_TYPE_TOKEN,
    state::{EscrowToken, ESCROW_TOKEN_SEED},
    EscrowCancelledEvent,
};

/// Return escrowed ARIO tokens to the depositor and close both the escrow
/// token account and the escrow PDA.
///
/// Only callable by `escrow.depositor`. Rent on the escrow PDA is returned
/// to the depositor via Anchor's `close = depositor` constraint.
pub fn handler(ctx: Context<CancelTokenDeposit>) -> Result<()> {
    let escrow = &ctx.accounts.escrow;
    let depositor_key = escrow.depositor;
    let asset_id = escrow.asset_id;
    let bump = escrow.bump;
    let amount = escrow.amount;
    let ario_mint = escrow.ario_mint;
    let escrow_pda = escrow.key();

    let bump_bytes = [bump];
    let signer_seeds: &[&[&[u8]]] = &[&[
        ESCROW_TOKEN_SEED,
        depositor_key.as_ref(),
        asset_id.as_ref(),
        &bump_bytes,
    ]];

    // 1. Transfer tokens back to depositor's ATA.
    let cpi_ctx = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        SplTransfer {
            from: ctx.accounts.escrow_token_account.to_account_info(),
            to: ctx.accounts.depositor_token_account.to_account_info(),
            authority: ctx.accounts.escrow.to_account_info(),
        },
        signer_seeds,
    );
    token::transfer(cpi_ctx, amount)?;

    // 2. Close escrow token account (return lamports to depositor).
    let cpi_close = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        CloseAccount {
            account: ctx.accounts.escrow_token_account.to_account_info(),
            destination: ctx.accounts.depositor.to_account_info(),
            authority: ctx.accounts.escrow.to_account_info(),
        },
        signer_seeds,
    );
    token::close_account(cpi_close)?;

    // 3. Anchor's `close = depositor` returns escrow PDA rent.

    let clock = Clock::get()?;
    emit!(EscrowCancelledEvent {
        escrow: escrow_pda,
        depositor: depositor_key,
        asset_id: ario_mint,
        asset_type: ASSET_TYPE_TOKEN,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: cancelled token deposit amount={} depositor={}",
        amount,
        depositor_key
    );

    Ok(())
}

#[derive(Accounts)]
pub struct CancelTokenDeposit<'info> {
    /// Escrow PDA being closed. `has_one = depositor` rejects calls from
    /// anyone other than the original depositor; `close = depositor`
    /// returns rent and zeroes the data on success.
    #[account(
        mut,
        seeds = [ESCROW_TOKEN_SEED, escrow.depositor.as_ref(), &escrow.asset_id],
        bump = escrow.bump,
        has_one = depositor @ EscrowError::NotDepositor,
        close = depositor,
    )]
    pub escrow: Account<'info, EscrowToken>,

    /// Escrow PDA's ARIO token account (source of return).
    #[account(
        mut,
        constraint = escrow_token_account.owner == escrow.key(),
        constraint = escrow_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    /// Depositor's ARIO token account (destination for returned tokens).
    #[account(
        mut,
        constraint = depositor_token_account.owner == depositor.key(),
        constraint = depositor_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
    )]
    pub depositor_token_account: Account<'info, TokenAccount>,

    /// Original depositor -- sole authority for cancel.
    #[account(mut)]
    pub depositor: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
