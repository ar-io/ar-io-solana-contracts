use anchor_lang::prelude::*;

use crate::{
    canonical::build_ant_escrow_claim_message,
    error::EscrowError,
    mpl_core_cpi::{set_update_authority_signed_by_pda, transfer_asset_signed_by_pda},
    state::{
        assert_mpl_core_asset_v1, EscrowAnt, ASSET_TYPE_ANT, ESCROW_ANT_SEED, ETHEREUM_PUBKEY_LEN,
        MPL_CORE_PROGRAM_ID, PROTOCOL_ETHEREUM,
    },
    verify::ethereum::verify_personal_sign,
    EscrowClaimedEvent,
};

/// Release the escrowed ANT to `claimant` after verifying an Ethereum
/// `personal_sign` ECDSA signature over the canonical message.
///
/// Mirrors `claim_ant_arweave` exactly — same canonical-message
/// reconstruction, same nonce check, same close-and-transfer flow — only
/// the signature verifier differs.
pub fn handler(
    ctx: Context<ClaimAntEthereum>,
    message_nonce: [u8; 32],
    signature: [u8; 65],
) -> Result<()> {
    let escrow = &ctx.accounts.escrow;

    // 0. Audit L22: AssetV1 discriminator check (parity with deposit path).
    assert_mpl_core_asset_v1(&ctx.accounts.ant_asset)?;

    // 1. Protocol guard.
    require!(
        escrow.recipient_protocol == PROTOCOL_ETHEREUM,
        EscrowError::ProtocolMismatch
    );

    // 2. Replay protection.
    require!(message_nonce == escrow.nonce, EscrowError::NonceMismatch);

    // 3. Canonical message from on-chain state.
    let message = build_ant_escrow_claim_message(
        &escrow.ant_mint,
        &ctx.accounts.claimant.key(),
        &escrow.nonce,
        escrow.recipient_pubkey_active(),
    );

    // 4. Verify ECDSA + EIP-191 + low-S against the 20-byte address
    //    stored at deposit time. The active prefix is exactly 20 bytes.
    let expected_address = escrow.recipient_pubkey_active();
    require!(
        expected_address.len() == ETHEREUM_PUBKEY_LEN,
        EscrowError::SignatureVerificationFailed
    );
    verify_personal_sign(&message, &signature, expected_address)?;

    // 5. Release.
    let ant_mint_key = escrow.ant_mint;
    let bump = escrow.bump;
    let signer_seeds: &[&[u8]] = &[ESCROW_ANT_SEED, ant_mint_key.as_ref(), &[bump]];

    transfer_asset_signed_by_pda(
        &ctx.accounts.ant_asset,
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.claimant.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        signer_seeds,
    )?;

    // Audit L23: rotate UpdateAuthority to claimant atomically. See
    // claim_arweave.rs for full rationale.
    set_update_authority_signed_by_pda(
        &ctx.accounts.ant_asset,
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.claimant.key(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        signer_seeds,
    )?;

    // Note: ario_ant::reconcile CPI intentionally omitted — see
    // claim_arweave.rs comment. Lazy reconciliation by ario-ant is safe.

    let clock = Clock::get()?;
    emit!(EscrowClaimedEvent {
        escrow: ctx.accounts.escrow.key(),
        claimer: ctx.accounts.claimant.key(),
        asset_id: ant_mint_key,
        asset_type: ASSET_TYPE_ANT,
        amount: 0,
        claim_protocol: PROTOCOL_ETHEREUM,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "ant-escrow: claimed (ethereum) mint={} claimant={}",
        ant_mint_key,
        ctx.accounts.claimant.key()
    );

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimAntEthereum<'info> {
    /// Escrow PDA. Same constraints as `ClaimAntArweave`.
    #[account(
        mut,
        seeds = [ESCROW_ANT_SEED, escrow.ant_mint.as_ref()],
        bump = escrow.bump,
        has_one = depositor,
        close = depositor,
    )]
    pub escrow: Account<'info, EscrowAnt>,

    /// CHECK: pinned to escrow.ant_mint and to the mpl-core program.
    #[account(
        mut,
        address = escrow.ant_mint @ EscrowError::AntMintMismatch,
        constraint = ant_asset.owner == &MPL_CORE_PROGRAM_ID @ EscrowError::InvalidAsset,
    )]
    pub ant_asset: AccountInfo<'info>,

    /// CHECK: bound into the canonical message; no on-chain ownership check.
    pub claimant: AccountInfo<'info>,

    /// CHECK: identity validated by the `has_one` constraint on escrow.
    #[account(mut)]
    pub depositor: AccountInfo<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: pinned by address constraint.
    #[account(address = MPL_CORE_PROGRAM_ID)]
    pub mpl_core_program: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}
