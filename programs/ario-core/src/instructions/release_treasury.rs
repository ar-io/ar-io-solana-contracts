//! Release tokens from the protocol treasury to a recipient, signed by
//! the ArioConfig PDA.
//!
//! Background: `ArioConfig.treasury` is the SPL token account that holds
//! the protocol's ARIO balance. Its SPL `Owner` authority is the
//! ArioConfig PDA — only `ario-core` can sign transfers from it. This ix
//! is the single canonical path through which other programs request
//! that ario-core release treasury funds on their behalf.
//!
//! Today the only caller is `ario-gar::distribute_epoch`, which CPIs in
//! once per batch to move the per-epoch reward pool from treasury into
//! GAR's stake token account. The constraints on this ix:
//!
//!   * Caller authentication via `seeds::program = ario_gar::ID` on the
//!     `gar_settings` signer — only the canonical GAR program can produce
//!     a `gar_settings` PDA signature.
//!   * Destination locked to `gar_settings.stake_token_account` — even if
//!     GAR is compromised or buggy, the funds can only land in GAR's
//!     own stake-pool token account, not an arbitrary destination.
//!   * Source pinned to `ArioConfig.treasury`.
//!
//! The ix is NOT gated on `migration_active`. Reward distribution is a
//! permanent protocol function that runs every epoch on mainnet.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::state::{ArioConfig, CONFIG_SEED};

pub fn release_treasury_to_recipient(
    ctx: Context<ReleaseTreasuryToRecipient>,
    amount: u64,
) -> Result<()> {
    let config_bump = ctx.accounts.config.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[CONFIG_SEED, &[config_bump]]];

    let cpi_accounts = SplTransfer {
        from: ctx.accounts.protocol_token_account.to_account_info(),
        to: ctx.accounts.recipient_token_account.to_account_info(),
        authority: ctx.accounts.config.to_account_info(),
    };
    let cpi_ctx = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        cpi_accounts,
        signer_seeds,
    );
    token::transfer(cpi_ctx, amount)
}

#[derive(Accounts)]
pub struct ReleaseTreasuryToRecipient<'info> {
    /// ArioConfig PDA. Signs the SPL transfer as treasury authority.
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArioConfig>,

    /// Source: the protocol treasury. Address pinned via
    /// `ArioConfig.treasury` so callers can't substitute a different
    /// token account.
    #[account(
        mut,
        address = config.treasury,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// Destination: locked to GAR's stake_token_account. Even if a
    /// compromised or buggy GAR build calls this ix with wild values,
    /// the funds can only flow into GAR's own staking pool.
    #[account(
        mut,
        address = gar_settings.stake_token_account,
    )]
    pub recipient_token_account: Account<'info, TokenAccount>,

    /// Cross-program signer proof. Three things must hold for this
    /// account to be accepted:
    ///   * `signer` — the call carries a signature for this account.
    ///     Only `ario-gar` can produce one (via `invoke_signed` with
    ///     `SETTINGS_SEED`); Solana's runtime enforces that PDA
    ///     signatures can only originate from the program owning them.
    ///   * `seeds = [b"gar_settings"]` + `seeds::program = ario_gar::ID`
    ///     — the account is the canonical gar_settings PDA derived
    ///     from the trusted ario-gar program ID.
    ///   * `Account<'info, GatewaySettings>` — Anchor type-deserializes,
    ///     verifying the account is owned by ario-gar.
    ///
    /// Together these prove: the call originated from ario-gar's code,
    /// not from a third party impersonating gar_settings as a regular
    /// account.
    #[account(
        signer,
        seeds = [b"gar_settings"],
        bump = gar_settings.bump,
        seeds::program = ario_gar::ID,
    )]
    pub gar_settings: Account<'info, ario_gar::state::GatewaySettings>,

    pub token_program: Program<'info, Token>,
}
