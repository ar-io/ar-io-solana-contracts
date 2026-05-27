//! Release escrowed vault tokens after Ed25519 attestation verification
//! by the AR.IO attestor service.
//!
//! Two introspections happen in this instruction:
//!
//! 1. **Ed25519Program sigverify ix** — must be the instruction
//!    immediately preceding this one (idx-1). Confirms the attestor
//!    signed the canonical claim message.
//! 2. **`ario_core::vaulted_transfer` ix** — for the active-vault
//!    path only. May be anywhere in the transaction (the helper
//!    scans). Confirms tokens released to the payer's ATA will
//!    be re-locked into a new vault for the claimant atomically.
//!
//! Both introspections use the same `instructions_sysvar` account.
//! Mirrors `claim_vault_arweave` aside from swapping the RSA-PSS
//! verification for Ed25519 introspection.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer as SplTransfer};

use crate::{
    canonical::build_escrow_claim_message,
    error::EscrowError,
    state::{EscrowToken, ASSET_TYPE_VAULT, ESCROW_VAULT_SEED, PROTOCOL_ARWEAVE},
    vault_introspect::verify_vaulted_transfer_in_tx,
    verify::attested::verify_attested_signature,
    EscrowClaimedEvent,
};

pub fn handler(ctx: Context<ClaimVaultArweaveAttested>, message_nonce: [u8; 32]) -> Result<()> {
    let escrow = &ctx.accounts.escrow;

    require!(
        escrow.asset_type == ASSET_TYPE_VAULT,
        EscrowError::AssetTypeMismatch
    );
    require!(
        escrow.recipient_protocol == PROTOCOL_ARWEAVE,
        EscrowError::ProtocolMismatch
    );
    require!(message_nonce == escrow.nonce, EscrowError::NonceMismatch);

    // F-1: bind escrow.recipient_pubkey into the canonical so the
    //      attestor's canonical (built from client-supplied modulus)
    //      diverges if the modulus is wrong.
    let message = build_escrow_claim_message(
        "vault",
        &escrow.asset_id,
        escrow.amount,
        &ctx.accounts.claimant.key(),
        &escrow.nonce,
        escrow.recipient_pubkey_active(),
    );

    // Verify the Ed25519 attestation. This consults the same
    // `instructions_sysvar` we use for `vaulted_transfer` introspection
    // below. Order: Ed25519Program ix at idx-1 of the claim ix;
    // `vaulted_transfer` may live anywhere.
    verify_attested_signature(&ctx.accounts.instructions_sysvar, &message)?;

    let depositor_key = escrow.depositor;
    let asset_id = escrow.asset_id;
    let bump = escrow.bump;
    let amount = escrow.amount;
    let vault_end_timestamp = escrow.vault_end_timestamp;
    let escrow_pda = escrow.key();

    let bump_bytes = [bump];
    let signer_seeds: &[&[&[u8]]] = &[&[
        ESCROW_VAULT_SEED,
        depositor_key.as_ref(),
        asset_id.as_ref(),
        &bump_bytes,
    ]];

    let clock = Clock::get()?;

    if clock.unix_timestamp < vault_end_timestamp {
        // Active vault — verify sibling `vaulted_transfer` first.
        // Defense-in-depth: tx atomicity protects either order, but
        // checking before transferring leaves no token state if the
        // verification fails.
        let remaining = vault_end_timestamp
            .checked_sub(clock.unix_timestamp)
            .ok_or(EscrowError::ArithmeticOverflow)?;

        verify_vaulted_transfer_in_tx(
            &ctx.accounts.instructions_sysvar,
            &ario_core::ID,
            amount,
            remaining,
            &ctx.accounts.claimant.key(),
            60,
        )?;

        // Active path: transfer to payer's ATA so the sibling
        // `vaulted_transfer` can re-lock it for the claimant.
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            SplTransfer {
                from: ctx.accounts.escrow_token_account.to_account_info(),
                to: ctx.accounts.payer_token_account.to_account_info(),
                authority: ctx.accounts.escrow.to_account_info(),
            },
            signer_seeds,
        );
        token::transfer(cpi_ctx, amount)?;
    } else {
        // Expired vault — direct liquid transfer to claimant's ATA.
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            SplTransfer {
                from: ctx.accounts.escrow_token_account.to_account_info(),
                to: ctx.accounts.claimant_token_account.to_account_info(),
                authority: ctx.accounts.escrow.to_account_info(),
            },
            signer_seeds,
        );
        token::transfer(cpi_ctx, amount)?;
    }

    // Close escrow token account.
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

    emit!(EscrowClaimedEvent {
        escrow: escrow_pda,
        claimer: ctx.accounts.claimant.key(),
        asset_id: Pubkey::new_from_array(asset_id),
        asset_type: ASSET_TYPE_VAULT,
        amount,
        claim_protocol: PROTOCOL_ARWEAVE,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: claimed vault (arweave-attested) amount={} claimant={} expired={}",
        amount,
        ctx.accounts.claimant.key(),
        clock.unix_timestamp >= vault_end_timestamp
    );

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimVaultArweaveAttested<'info> {
    #[account(
        mut,
        seeds = [ESCROW_VAULT_SEED, escrow.depositor.as_ref(), &escrow.asset_id],
        bump = escrow.bump,
        has_one = depositor,
        close = depositor,
    )]
    pub escrow: Account<'info, EscrowToken>,

    #[account(
        mut,
        constraint = escrow_token_account.owner == escrow.key(),
        constraint = escrow_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    /// Claimant's ARIO token account (destination for expired-vault path).
    #[account(
        mut,
        constraint = claimant_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
        constraint = claimant_token_account.owner == claimant.key() @ EscrowError::TokenAccountOwnerMismatch,
    )]
    pub claimant_token_account: Account<'info, TokenAccount>,

    /// Payer's ARIO token account (intermediate for active-vault path).
    #[account(
        mut,
        constraint = payer_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
        constraint = payer_token_account.owner == payer.key() @ EscrowError::TokenAccountOwnerMismatch,
    )]
    pub payer_token_account: Account<'info, TokenAccount>,

    /// CHECK: validated by canonical message ↔ Ed25519 sig binding.
    pub claimant: AccountInfo<'info>,

    /// CHECK: identity validated by `has_one` constraint on escrow.
    #[account(mut)]
    pub depositor: AccountInfo<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    /// Solana `sysvar::instructions` — used for BOTH the Ed25519 sig
    /// introspection (preceding ix) and the `vaulted_transfer`
    /// introspection (anywhere in tx).
    /// CHECK: pinned by address constraint to the sysvar id.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
