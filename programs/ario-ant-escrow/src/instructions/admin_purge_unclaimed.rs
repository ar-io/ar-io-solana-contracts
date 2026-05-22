//! `admin_purge_unclaimed_ant` — burn an abandoned escrow ANT after a
//! grace period, returning the asset's rent + the escrow PDA's rent to
//! the protocol authority.
//!
//! Implements `ANT_ESCROW_DESIGN.md` §11 "Permissionless cleanup of
//! abandoned escrows" (originally deferred for v1; landed here for the
//! mainnet rent-reclaim PR series).
//!
//! Time gate: `Clock::slot - escrow.deposit_slot >= UNCLAIMED_PURGE_GRACE_SLOTS`
//! (≈ 5 years). Auth gate: signer must equal `ArioConfig.authority`.

use anchor_lang::prelude::*;
use ario_core::state::ArioConfig;

use crate::{
    error::EscrowError,
    events::EscrowAntPurgedEvent,
    mpl_core_cpi::burn_asset_signed_by_pda,
    state::{
        assert_mpl_core_asset_v1, EscrowAnt, ESCROW_ANT_SEED, MPL_CORE_PROGRAM_ID,
        UNCLAIMED_PURGE_GRACE_SLOTS,
    },
};

pub fn handler(ctx: Context<AdminPurgeUnclaimedAnt>) -> Result<()> {
    // Defense-in-depth: ensure the asset is still in escrow custody and
    // hasn't been claimed/canceled out from under us.
    require_keys_eq!(
        ctx.accounts.escrow.ant_mint,
        ctx.accounts.ant_asset.key(),
        EscrowError::AntMintMismatch
    );
    assert_mpl_core_asset_v1(&ctx.accounts.ant_asset)?;

    // Time gate: grace period must have elapsed since deposit.
    let current_slot = Clock::get()?.slot;
    let elapsed = current_slot.saturating_sub(ctx.accounts.escrow.deposit_slot);
    require!(
        elapsed >= UNCLAIMED_PURGE_GRACE_SLOTS,
        EscrowError::PurgeGraceNotElapsed
    );

    // Burn the asset, escrow PDA signs as current owner. Rent flows to
    // `authority` (configured as `payer` in the BurnV1 CPI).
    let ant_mint_key = ctx.accounts.escrow.ant_mint;
    let bump = ctx.accounts.escrow.bump;
    let signer_seeds: &[&[u8]] = &[ESCROW_ANT_SEED, ant_mint_key.as_ref(), &[bump]];

    burn_asset_signed_by_pda(
        &ctx.accounts.ant_asset,
        &ctx.accounts.authority.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        signer_seeds,
    )?;

    // The escrow PDA itself is closed via Anchor's `close = authority`
    // constraint after this handler returns.

    emit!(EscrowAntPurgedEvent {
        ant_mint: ant_mint_key,
        depositor: ctx.accounts.escrow.depositor,
        deposit_slot: ctx.accounts.escrow.deposit_slot,
        purged_at_slot: current_slot,
        elapsed_slots: elapsed,
        authority: ctx.accounts.authority.key(),
    });

    msg!(
        "admin_purge_unclaimed_ant: burned mint={} after {} slots in escrow",
        ant_mint_key,
        elapsed
    );

    Ok(())
}

#[derive(Accounts)]
pub struct AdminPurgeUnclaimedAnt<'info> {
    /// Escrow being purged. `close = authority` returns its rent to the
    /// admin on successful handler completion.
    #[account(
        mut,
        seeds = [ESCROW_ANT_SEED, escrow.ant_mint.as_ref()],
        bump = escrow.bump,
        close = authority,
    )]
    pub escrow: Account<'info, EscrowAnt>,

    /// The Metaplex Core asset to burn.
    /// CHECK: pinned by address constraint to `escrow.ant_mint` and to
    /// the mpl-core program; the BurnV1 CPI further validates layout.
    #[account(
        mut,
        address = escrow.ant_mint @ EscrowError::AntMintMismatch,
        constraint = ant_asset.owner == &MPL_CORE_PROGRAM_ID @ EscrowError::InvalidAsset,
    )]
    pub ant_asset: AccountInfo<'info>,

    /// `ArioConfig` PDA from ario-core. Read-only — used to gate the
    /// signer against the protocol authority. Cross-program account
    /// reference via the `cpi` feature dependency in Cargo.toml.
    #[account(
        seeds = [b"ario_config"],
        bump,
        seeds::program = ario_core::ID,
    )]
    pub ario_config: Account<'info, ArioConfig>,

    /// Protocol admin — must equal `ario_config.authority`. The address
    /// constraint enforces it; this is what makes the ix admin-only.
    #[account(
        mut,
        address = ario_config.authority @ EscrowError::Unauthorized,
    )]
    pub authority: Signer<'info>,

    /// CHECK: pinned by address constraint.
    #[account(address = MPL_CORE_PROGRAM_ID)]
    pub mpl_core_program: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}
