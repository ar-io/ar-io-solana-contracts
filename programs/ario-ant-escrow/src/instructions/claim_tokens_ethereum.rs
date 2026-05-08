use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer as SplTransfer};

use crate::{
    canonical::build_escrow_claim_message,
    error::EscrowError,
    state::{
        EscrowToken, ASSET_TYPE_TOKEN, ESCROW_TOKEN_SEED, ETHEREUM_PUBKEY_LEN, PROTOCOL_ETHEREUM,
    },
    verify::ethereum::verify_personal_sign,
    EscrowClaimedEvent,
};

/// Release escrowed ARIO tokens to `claimant` after verifying an Ethereum
/// `personal_sign` ECDSA signature over the escrow canonical message.
///
/// Mirrors `claim_tokens_arweave` exactly -- same canonical-message
/// reconstruction, same nonce check, same close-and-transfer flow -- only
/// the signature verifier differs.
pub fn handler(
    ctx: Context<ClaimTokensEthereum>,
    message_nonce: [u8; 32],
    signature: [u8; 65],
) -> Result<()> {
    let escrow = &ctx.accounts.escrow;

    // 1. Asset type guard.
    require!(
        escrow.asset_type == ASSET_TYPE_TOKEN,
        EscrowError::AssetTypeMismatch
    );

    // 2. Protocol guard.
    require!(
        escrow.recipient_protocol == PROTOCOL_ETHEREUM,
        EscrowError::ProtocolMismatch
    );

    // 3. Replay protection.
    require!(message_nonce == escrow.nonce, EscrowError::NonceMismatch);

    // 4. Canonical message from on-chain state.
    let message = build_escrow_claim_message(
        "token",
        &escrow.asset_id,
        escrow.amount,
        &ctx.accounts.claimant.key(),
        &escrow.nonce,
        escrow.recipient_pubkey_active(),
    );

    // 5. Verify ECDSA + EIP-191 + low-S.
    let expected_address = escrow.recipient_pubkey_active();
    require!(
        expected_address.len() == ETHEREUM_PUBKEY_LEN,
        EscrowError::SignatureVerificationFailed
    );
    verify_personal_sign(&message, &signature, expected_address)?;

    // 6. SPL transfer from escrow ATA to claimant ATA.
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

    // 7. Close escrow token account.
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
        claim_protocol: PROTOCOL_ETHEREUM,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: claimed tokens (ethereum) amount={} claimant={}",
        amount,
        ctx.accounts.claimant.key()
    );

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimTokensEthereum<'info> {
    /// Escrow PDA holding token deposit metadata.
    #[account(
        mut,
        seeds = [ESCROW_TOKEN_SEED, escrow.depositor.as_ref(), &escrow.asset_id],
        bump = escrow.bump,
        has_one = depositor,
        close = depositor,
    )]
    pub escrow: Account<'info, EscrowToken>,

    /// Escrow PDA's ARIO token account (source of tokens).
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

    /// CHECK: bound into the canonical message; no on-chain ownership check.
    pub claimant: AccountInfo<'info>,

    /// CHECK: identity validated by the `has_one` constraint on escrow.
    #[account(mut)]
    pub depositor: AccountInfo<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
