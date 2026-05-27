use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Token, TokenAccount, Transfer as SplTransfer};

use crate::{
    canonical::build_escrow_claim_message,
    error::EscrowError,
    state::{
        EscrowToken, ASSET_TYPE_VAULT, ESCROW_VAULT_SEED, ETHEREUM_PUBKEY_LEN, PROTOCOL_ETHEREUM,
    },
    vault_introspect::verify_vaulted_transfer_in_tx,
    verify::ethereum::verify_personal_sign,
    EscrowClaimedEvent,
};

/// Release escrowed vault tokens after Ethereum ECDSA verification.
///
/// **Active vault path:** Transfers tokens to the payer's ATA, then verifies
/// that a matching `ario_core::vaulted_transfer` instruction exists in the
/// same transaction (via sysvar::instructions introspection). This ensures
/// the tokens end up in a time-locked vault for the claimant — the lock is
/// enforced on-chain via transaction atomicity.
///
/// **Expired vault path:** Direct SPL transfer to the claimant's ATA (liquid).
pub fn handler(
    ctx: Context<ClaimVaultEthereum>,
    message_nonce: [u8; 32],
    signature: [u8; 65],
) -> Result<()> {
    let escrow = &ctx.accounts.escrow;

    // 1. Asset type guard.
    require!(
        escrow.asset_type == ASSET_TYPE_VAULT,
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
        "vault",
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

    // 6. Sig is good — transfer tokens.
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
        // ============================================================
        // Active vault — verify sibling vaulted_transfer FIRST, then
        // transfer tokens to payer's ATA. Defense-in-depth: atomicity
        // protects either way, but checking first is safer.
        // ============================================================
        let remaining = vault_end_timestamp
            .checked_sub(clock.unix_timestamp)
            .ok_or(EscrowError::ArithmeticOverflow)?;

        // Verify that the transaction includes a vaulted_transfer instruction
        // with matching parameters. If not present → revert immediately,
        // no token movement occurs.
        verify_vaulted_transfer_in_tx(
            &ctx.accounts.instructions_sysvar,
            &ario_core::ID,
            amount,
            remaining,
            &ctx.accounts.claimant.key(),
            60, // 60s tolerance for clock drift
        )?;

        // Verification passed — safe to release tokens to payer's ATA.
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
        // ============================================================
        // Expired vault — liquid SPL transfer directly to claimant.
        // ============================================================
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

    emit!(EscrowClaimedEvent {
        escrow: escrow_pda,
        claimer: ctx.accounts.claimant.key(),
        asset_id: Pubkey::new_from_array(asset_id),
        asset_type: ASSET_TYPE_VAULT,
        amount,
        claim_protocol: PROTOCOL_ETHEREUM,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: claimed vault (ethereum) amount={} claimant={} expired={}",
        amount,
        ctx.accounts.claimant.key(),
        clock.unix_timestamp >= vault_end_timestamp
    );

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimVaultEthereum<'info> {
    /// Escrow PDA holding vault deposit metadata.
    #[account(
        mut,
        seeds = [ESCROW_VAULT_SEED, escrow.depositor.as_ref(), &escrow.asset_id],
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

    /// Claimant's ARIO token account (destination for expired-vault path).
    #[account(
        mut,
        constraint = claimant_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
        constraint = claimant_token_account.owner == claimant.key() @ EscrowError::TokenAccountOwnerMismatch,
    )]
    pub claimant_token_account: Account<'info, TokenAccount>,

    /// Payer's ARIO token account (intermediate destination for active-vault
    /// path — payer then sends them to vaulted_transfer in a sibling ix).
    #[account(
        mut,
        constraint = payer_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
        constraint = payer_token_account.owner == payer.key() @ EscrowError::TokenAccountOwnerMismatch,
    )]
    pub payer_token_account: Account<'info, TokenAccount>,

    /// Recipient of the vault/tokens — pubkey bound into canonical message.
    /// CHECK: validated by canonical message ↔ signature binding.
    pub claimant: AccountInfo<'info>,

    /// Original depositor — receives rent on escrow close.
    /// CHECK: identity validated by `has_one` constraint on escrow.
    #[account(mut)]
    pub depositor: AccountInfo<'info>,

    /// Tx fee payer. For active vaults, also holds tokens temporarily
    /// between the escrow release and the vaulted_transfer sibling ix.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// Instructions sysvar — used to verify that a matching
    /// vaulted_transfer instruction exists in the same transaction.
    /// CHECK: validated by address constraint.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
