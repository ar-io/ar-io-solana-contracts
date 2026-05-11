use anchor_lang::prelude::*;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_spl::token::Mint;

use crate::error::ArioError;
use crate::state::*;
use crate::InitializeParams;

pub fn handler(ctx: Context<Initialize>, params: InitializeParams) -> Result<()> {
    let config = &mut ctx.accounts.config;
    let _clock = Clock::get()?;

    config.authority = params.authority;
    config.mint = ctx.accounts.mint.key();
    config.arns_program = params.arns_program;
    config.treasury = params.treasury;
    config.total_supply = params.total_supply;
    config.protocol_balance = 0;
    config.circulating_supply = params.total_supply;
    config.locked_supply = 0;
    config.min_vault_duration = ArioConfig::DEFAULT_MIN_VAULT_DURATION;
    config.max_vault_duration = ArioConfig::DEFAULT_MAX_VAULT_DURATION;
    config.primary_name_request_expiry = ArioConfig::DEFAULT_PRIMARY_NAME_REQUEST_EXPIRY;
    config.migration_active = true;
    config.migration_authority = params.migration_authority;
    config.bump = ctx.bumps.config;
    config.gar_program = params.gar_program;

    msg!(
        "AR.IO protocol initialized with supply: {}",
        params.total_supply
    );
    Ok(())
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(
        init,
        payer = payer,
        space = ArioConfig::SIZE,
        seeds = [CONFIG_SEED],
        bump,
    )]
    pub config: Account<'info, ArioConfig>,

    /// The ARIO token mint
    pub mint: Account<'info, Mint>,

    /// Bind initialize to the program's upgrade authority. Closes the
    /// front-run window between `solana program deploy` and `initialize`
    /// (audit H1 / Theme C). The constraint uses `Some(payer.key())`
    /// equality so a revoked upgrade authority (`None`) cannot satisfy it
    /// — explicit, unlike `unwrap_or_default()` which would coerce a
    /// revoked authority to System Program.
    #[account(
        mut,
        constraint = program_data.upgrade_authority_address == Some(payer.key()) @ ArioError::Unauthorized,
    )]
    pub payer: Signer<'info>,

    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = bpf_loader_upgradeable::id(),
    )]
    pub program_data: Account<'info, ProgramData>,

    pub system_program: Program<'info, System>,
}
