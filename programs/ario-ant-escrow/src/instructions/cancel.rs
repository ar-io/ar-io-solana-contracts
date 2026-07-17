use anchor_lang::prelude::*;

use crate::{
    error::EscrowError,
    events::ASSET_TYPE_ANT,
    mpl_core_cpi::{set_update_authority_signed_by_pda, transfer_asset_signed_by_pda},
    state::{assert_mpl_core_asset_v1, EscrowAnt, ESCROW_ANT_SEED, MPL_CORE_PROGRAM_ID},
    EscrowCancelledEvent,
};

/// Return the escrowed ANT to the depositor and close the escrow PDA.
///
/// Only callable by `escrow.depositor`. Rent on the escrow account is
/// returned to the depositor via Anchor's `close = depositor` constraint.
///
/// Note on race with claim: if a recipient submits a valid claim signature
/// in the same slot, slot-ordering decides who wins. See design doc
/// § Race conditions for the recommended pattern when the depositor needs
/// to definitively block an in-flight claim.
pub fn handler(ctx: Context<CancelDeposit>) -> Result<()> {
    // The depositor signature is enforced by the `has_one = depositor`
    // constraint plus `Signer<'info>` on `depositor`. Authority check
    // implicit in account constraints is the canonical Anchor pattern;
    // no explicit `require_keys_eq!` needed here.

    // Audit L22: AssetV1 discriminator check (parity with deposit path).
    assert_mpl_core_asset_v1(&ctx.accounts.ant_asset)?;

    // Reconstruct PDA signer seeds for the TransferV1 CPI. The escrow
    // PDA is the current asset owner, so it must sign to release.
    let ant_mint_key = ctx.accounts.escrow.ant_mint;
    let bump = ctx.accounts.escrow.bump;
    let signer_seeds: &[&[u8]] = &[ESCROW_ANT_SEED, ant_mint_key.as_ref(), &[bump]];

    transfer_asset_signed_by_pda(
        &ctx.accounts.ant_asset,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        signer_seeds,
    )?;

    // Audit L23 mirror: deposit moves UA to the escrow PDA, so cancel
    // must move UA back to the depositor — otherwise UA stays at the now-
    // closed escrow PDA forever and the depositor loses the ability to
    // update their own asset's metadata URI.
    set_update_authority_signed_by_pda(
        &ctx.accounts.ant_asset,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.depositor.key(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        signer_seeds,
    )?;

    // Note: ario_ant::reconcile CPI intentionally omitted — see
    // claim_arweave.rs comment. Lazy reconciliation by ario-ant is safe.

    let clock = Clock::get()?;
    emit!(EscrowCancelledEvent {
        escrow: ctx.accounts.escrow.key(),
        depositor: ctx.accounts.depositor.key(),
        asset_id: ant_mint_key,
        asset_type: ASSET_TYPE_ANT,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "ant-escrow: cancelled mint={} depositor={}",
        ant_mint_key,
        ctx.accounts.depositor.key()
    );

    Ok(())
}

#[derive(Accounts)]
pub struct CancelDeposit<'info> {
    /// Escrow account being closed. `has_one = depositor` rejects calls
    /// from anyone other than the original depositor; `close = depositor`
    /// returns rent and zeroes the data on success.
    #[account(
        mut,
        seeds = [ESCROW_ANT_SEED, escrow.ant_mint.as_ref()],
        bump = escrow.bump,
        has_one = depositor @ EscrowError::NotDepositor,
        close = depositor,
    )]
    pub escrow: Account<'info, EscrowAnt>,

    /// The Metaplex Core asset returning to the depositor.
    /// `has_one`-style validation: `escrow.ant_mint` must equal this
    /// account's key. Defends against wallet UIs that mis-route accounts.
    /// CHECK: pinned by address constraint to escrow.ant_mint and to the mpl-core program.
    #[account(
        mut,
        address = escrow.ant_mint @ EscrowError::AntMintMismatch,
        constraint = ant_asset.owner == &MPL_CORE_PROGRAM_ID @ EscrowError::InvalidAsset,
    )]
    pub ant_asset: AccountInfo<'info>,

    /// Original depositor — sole authority for cancel. Must sign so the
    /// system program will credit them with the closed-account rent.
    #[account(mut)]
    pub depositor: Signer<'info>,

    /// CHECK: pinned by address constraint.
    #[account(address = MPL_CORE_PROGRAM_ID)]
    pub mpl_core_program: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}
