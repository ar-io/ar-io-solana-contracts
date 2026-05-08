//! Release escrowed ARIO tokens after Ed25519 attestation verification
//! by the AR.IO attestor service. The off-chain attestor verifies the
//! Arweave RSA-PSS signature over the canonical claim message, then
//! re-signs the same bytes with Ed25519. This instruction reads the
//! preceding Ed25519Program sigverify ix via instruction introspection
//! and confirms the signing pubkey matches `ATTESTOR_PUBKEY` and the
//! signed message matches the canonical message reconstructed from
//! escrow state.
//!
//! Mirrors `claim_tokens_arweave` exactly aside from the verification
//! step. See `instructions/claim_arweave_attested.rs` for design
//! rationale and `migration/attestor/README.md` for the service.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer as SplTransfer};

use crate::{
    canonical::build_escrow_claim_message,
    error::EscrowError,
    state::{EscrowToken, ASSET_TYPE_TOKEN, ESCROW_TOKEN_SEED, PROTOCOL_ARWEAVE},
    verify::attested::verify_attested_signature,
    EscrowClaimedEvent,
};

pub fn handler(ctx: Context<ClaimTokensArweaveAttested>, message_nonce: [u8; 32]) -> Result<()> {
    let escrow = &ctx.accounts.escrow;

    // 1. Asset type guard.
    require!(
        escrow.asset_type == ASSET_TYPE_TOKEN,
        EscrowError::AssetTypeMismatch
    );

    // 2. Protocol guard. Stops a misrouted Ethereum-recipient escrow
    //    from being claimed via the Arweave path.
    require!(
        escrow.recipient_protocol == PROTOCOL_ARWEAVE,
        EscrowError::ProtocolMismatch
    );

    // 3. Replay protection.
    require!(message_nonce == escrow.nonce, EscrowError::NonceMismatch);

    // 4. Reconstruct the canonical message from on-chain state. NEVER
    //    trust client-supplied message bytes. The `recipient_pubkey_active()`
    //    is the trusted modulus stored at deposit time; binding it into
    //    the canonical closes F-1 (see `claim_arweave_attested.rs`).
    let message = build_escrow_claim_message(
        "token",
        &escrow.asset_id,
        escrow.amount,
        &ctx.accounts.claimant.key(),
        &escrow.nonce,
        escrow.recipient_pubkey_active(),
    );

    // 5. Verify the Ed25519 attestation via instruction introspection.
    verify_attested_signature(&ctx.accounts.instructions_sysvar, &message)?;

    // 6. SPL transfer escrow ATA → claimant ATA, signed by escrow PDA.
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

    // 7. Close escrow token account (lamports back to depositor).
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

    let clock = Clock::get()?;
    emit!(EscrowClaimedEvent {
        escrow: escrow_pda,
        claimer: ctx.accounts.claimant.key(),
        asset_id: ario_mint,
        asset_type: ASSET_TYPE_TOKEN,
        amount,
        claim_protocol: PROTOCOL_ARWEAVE,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: claimed tokens (arweave-attested) amount={} claimant={}",
        amount,
        ctx.accounts.claimant.key()
    );

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimTokensArweaveAttested<'info> {
    /// Escrow PDA holding token deposit metadata.
    #[account(
        mut,
        seeds = [ESCROW_TOKEN_SEED, escrow.depositor.as_ref(), &escrow.asset_id],
        bump = escrow.bump,
        has_one = depositor,
        close = depositor,
    )]
    pub escrow: Account<'info, EscrowToken>,

    /// Escrow PDA's ARIO token account (source).
    #[account(
        mut,
        constraint = escrow_token_account.owner == escrow.key(),
        constraint = escrow_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    /// Claimant's ARIO token account (destination).
    #[account(
        mut,
        constraint = claimant_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
        constraint = claimant_token_account.owner == claimant.key() @ EscrowError::TokenAccountOwnerMismatch,
    )]
    pub claimant_token_account: Account<'info, TokenAccount>,

    /// Recipient — pubkey bound into the canonical message.
    /// CHECK: validated by canonical message ↔ Ed25519 sig binding.
    pub claimant: AccountInfo<'info>,

    /// Original depositor — receives rent on escrow close.
    /// CHECK: identity validated by `has_one` constraint on escrow.
    #[account(mut)]
    pub depositor: AccountInfo<'info>,

    /// Tx fee payer. Anyone can submit a valid attestation.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// Solana `sysvar::instructions` — required for introspecting the
    /// preceding Ed25519Program sigverify instruction.
    /// CHECK: pinned by address constraint to the sysvar id.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
