use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    bpf_loader_upgradeable,
    entrypoint::MAX_PERMITTED_DATA_INCREASE,
    hash::hash,
    program::{invoke, invoke_signed},
    rent::Rent,
    system_instruction,
    sysvar::Sysvar,
};

use crate::error::ArnsError;
use crate::state::*;
use crate::InitializeArnsParams;

pub fn handler(ctx: Context<InitializeArns>, params: InitializeArnsParams) -> Result<()> {
    // Initialize ArNS config
    let config = &mut ctx.accounts.config;
    config.authority = params.authority;
    config.mint = params.mint;
    config.treasury = params.treasury;
    config.grace_period_seconds = GRACE_PERIOD_SECONDS;
    config.return_auction_duration_seconds = RETURNED_NAME_DURATION_SECONDS;
    config.max_lease_length_years = MAX_LEASE_LENGTH_YEARS as u8;
    config.total_names_registered = 0;
    config.next_records_prune_timestamp = i64::MAX;
    config.next_returned_names_prune_timestamp = i64::MAX;
    config.migration_active = true;
    config.migration_authority = params.migration_authority;
    config.bump = ctx.bumps.config;

    // Initialize demand factor with genesis fees
    let demand = &mut ctx.accounts.demand_factor;
    demand.current_demand_factor = DEMAND_FACTOR_SCALE;
    demand.current_period = 1;
    demand.purchases_this_period = 0;
    demand.revenue_this_period = 0;
    demand.consecutive_periods_with_min_demand_factor = 0;
    demand.trailing_period_purchases = [0u64; MOVING_AVG_PERIOD_COUNT];
    demand.trailing_period_revenues = [0u64; MOVING_AVG_PERIOD_COUNT];
    demand.fees = GENESIS_FEES;
    // Validate period_zero_start_timestamp is reasonable: must not be in the
    // future and must be after 2020-01-01 (1577836800). This prevents accidental
    // misconfiguration that would freeze demand factor period progression.
    let clock = Clock::get()?;
    require!(
        params.period_zero_start_timestamp <= clock.unix_timestamp,
        ArnsError::InvalidParameter
    );
    require!(
        params.period_zero_start_timestamp >= 1_577_836_800, // 2020-01-01
        ArnsError::InvalidParameter
    );
    demand.period_zero_start_timestamp = params.period_zero_start_timestamp;
    demand.criteria = DEMAND_CRITERIA_REVENUE; // Explicit: revenue-based measurement
    demand.bump = ctx.bumps.demand_factor;

    msg!("ArNS registry initialized");
    Ok(())
}

/// Grow the NameRegistry PDA in ≤10KB steps per transaction (see ario-gar `create_gateway_registry`).
pub fn create_name_registry(ctx: Context<CreateNameRegistry>) -> Result<()> {
    const TARGET: usize = 8 + NameRegistry::SIZE;

    let rent = Rent::get()?;
    let reg = ctx.accounts.name_registry.to_account_info();
    let authority = ctx.accounts.authority.to_account_info();
    let system = ctx.accounts.system_program.to_account_info();
    let auth_pk = ctx.accounts.config.authority;

    let current_len = reg.data_len() as usize;

    if current_len == TARGET {
        let data = reg.try_borrow_data()?;
        let expected = hash(b"account:NameRegistry");
        if data.len() >= 8 && data[..8] == expected.to_bytes()[..8] {
            return err!(ArnsError::NameRegistryAlreadyExists);
        }
        drop(data);
        write_name_registry_header(&reg, auth_pk)?;
        msg!(
            "NameRegistry created ({} max names)",
            NameRegistry::MAX_NAMES
        );
        return Ok(());
    }

    if reg.data_is_empty() {
        let initial = TARGET.min(MAX_PERMITTED_DATA_INCREASE);
        let required = rent.minimum_balance(initial);
        let bump = ctx.bumps.name_registry;
        let signer_seeds: &[&[&[u8]]] = &[&[NAME_REGISTRY_SEED, &[bump]]];

        // Lamport-griefing defense: see ario-gar/instructions/initialize.rs.
        let existing = reg.lamports();
        if existing < required {
            let deficit = required - existing;
            invoke(
                &system_instruction::transfer(authority.key, reg.key, deficit),
                &[authority.clone(), reg.clone(), system.clone()],
            )?;
        }
        invoke_signed(
            &system_instruction::allocate(reg.key, initial as u64),
            &[reg.clone(), system.clone()],
            signer_seeds,
        )?;
        invoke_signed(
            &system_instruction::assign(reg.key, &crate::ID),
            &[reg.clone(), system.clone()],
            signer_seeds,
        )?;
        if initial == TARGET {
            write_name_registry_header(&reg, auth_pk)?;
            msg!(
                "NameRegistry created ({} max names)",
                NameRegistry::MAX_NAMES
            );
        }
        return Ok(());
    }

    require!(current_len < TARGET, ArnsError::NameRegistryAlreadyExists);

    let next_len = (current_len + MAX_PERMITTED_DATA_INCREASE).min(TARGET);
    let min_balance = rent.minimum_balance(next_len);
    let needed = min_balance.saturating_sub(reg.lamports());
    if needed > 0 {
        invoke(
            &system_instruction::transfer(authority.key, reg.key, needed),
            &[authority.clone(), reg.clone(), system.clone()],
        )?;
    }
    reg.realloc(next_len, true)?;

    if next_len == TARGET {
        write_name_registry_header(&reg, auth_pk)?;
        msg!(
            "NameRegistry created ({} max names)",
            NameRegistry::MAX_NAMES
        );
    }
    Ok(())
}

fn write_name_registry_header(reg: &AccountInfo, authority: Pubkey) -> Result<()> {
    let mut data = reg.try_borrow_mut_data()?;
    let disc = hash(b"account:NameRegistry");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);
    data[8..40].copy_from_slice(authority.as_ref());
    Ok(())
}

#[derive(Accounts)]
pub struct InitializeArns<'info> {
    #[account(
        init,
        payer = authority,
        space = ArnsConfig::SIZE,
        seeds = [ARNS_CONFIG_SEED],
        bump,
    )]
    pub config: Account<'info, ArnsConfig>,

    #[account(
        init,
        payer = authority,
        space = DemandFactor::SIZE,
        seeds = [DEMAND_FACTOR_SEED],
        bump,
    )]
    pub demand_factor: Account<'info, DemandFactor>,

    /// Bind initialize to the program's upgrade authority — closes the
    /// front-run window between `solana program deploy` and `initialize`
    /// (audit H1 / Theme C). Explicit `Some(authority.key())` equality
    /// rejects revoked upgrade authority (`None`).
    #[account(
        mut,
        constraint = program_data.upgrade_authority_address == Some(authority.key()) @ ArnsError::Unauthorized,
    )]
    pub authority: Signer<'info>,

    #[account(
        seeds = [crate::ID.as_ref()],
        bump,
        seeds::program = bpf_loader_upgradeable::id(),
    )]
    pub program_data: Account<'info, ProgramData>,

    pub system_program: Program<'info, System>,
}

/// Create the NameRegistry zero-copy account (2MB).
/// Must be called after initialize since it reads config.authority.
#[derive(Accounts)]
pub struct CreateNameRegistry<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
        has_one = authority @ crate::error::ArnsError::Unauthorized,
    )]
    pub config: Account<'info, ArnsConfig>,

    /// CHECK: PDA allocated via [`create_name_registry`] (manual CPI, not Anchor `init`).
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    #[account(mut)]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}
