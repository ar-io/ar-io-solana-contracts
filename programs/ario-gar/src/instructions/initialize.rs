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
use anchor_spl::token::spl_token::instruction::AuthorityType;
use anchor_spl::token::{self, Mint, SetAuthority, Token, TokenAccount};

use crate::error::GarError;
use crate::state::*;
use crate::{
    InitializeEpochParams, InitializeParams, GATEWAY_OPERATOR_REWARD_RATE,
    MAX_EXPEDITED_WITHDRAWAL_PENALTY, MAX_REWARD_RATE, MIN_EXPEDITED_WITHDRAWAL_AMOUNT,
    MIN_EXPEDITED_WITHDRAWAL_PENALTY, MIN_REWARD_RATE, MISSED_OBSERVATION_PENALTY,
    OBSERVER_REWARD_RATE, REWARD_DECAY_LAST_EPOCH, REWARD_DECAY_START_EPOCH,
    WITHDRAWAL_LOCK_PERIOD,
};

pub fn initialize(ctx: Context<InitializeGar>, params: InitializeParams) -> Result<()> {
    let settings = &mut ctx.accounts.settings;
    settings.authority = params.authority;
    settings.mint = ctx.accounts.mint.key();
    settings.min_operator_stake = Gateway::MIN_OPERATOR_STAKE;
    settings.min_delegate_stake = 10_000_000; // 10 ARIO (matches Lua delegates.minStake)
    settings.withdrawal_period = WITHDRAWAL_LOCK_PERIOD;
    settings.max_expedited_withdrawal_penalty = MAX_EXPEDITED_WITHDRAWAL_PENALTY;
    settings.min_expedited_withdrawal_penalty = MIN_EXPEDITED_WITHDRAWAL_PENALTY;
    settings.min_expedited_withdrawal_amount = MIN_EXPEDITED_WITHDRAWAL_AMOUNT;
    settings.max_delegates_per_gateway = 10_000;
    settings.migration_active = true;
    settings.migration_authority = params.migration_authority;
    settings.stake_token_account = params.stake_token_account;
    settings.protocol_token_account = params.protocol_token_account;
    settings.arns_program_id = params.arns_program_id;
    settings.total_staked = 0;
    settings.total_delegated = 0;
    settings.total_withdrawn = 0;
    settings.bump = ctx.bumps.settings;

    msg!("GAR program initialized");
    Ok(())
}

/// Grow the GatewayRegistry PDA by at most [`MAX_PERMITTED_DATA_INCREASE`] bytes per transaction.
/// Surfpool (and the SVM realloc rules) cap growth **per top-level tx**; callers must invoke this
/// repeatedly until the account reaches full size (see `migration/import` devnet-setup loop).
pub fn create_gateway_registry(ctx: Context<CreateGatewayRegistry>) -> Result<()> {
    const TARGET: usize = 8 + GatewayRegistry::SIZE;

    let rent = Rent::get()?;
    let registry = ctx.accounts.registry.to_account_info();
    let authority = ctx.accounts.authority.to_account_info();
    let system = ctx.accounts.system_program.to_account_info();
    let auth_pk = ctx.accounts.settings.authority;

    let current_len = registry.data_len();

    if current_len == TARGET {
        let data = registry.try_borrow_data()?;
        let expected = hash(b"account:GatewayRegistry");
        if data.len() >= 8 && data[..8] == expected.to_bytes()[..8] {
            return err!(GarError::GatewayRegistryAlreadyExists);
        }
    }

    if registry.data_is_empty() {
        let initial = TARGET.min(MAX_PERMITTED_DATA_INCREASE);
        let required = rent.minimum_balance(initial);
        let bump = ctx.bumps.registry;
        let signer_seeds: &[&[&[u8]]] = &[&[REGISTRY_SEED, &[bump]]];

        // Lamport-griefing defense: an attacker can pre-fund the registry PDA
        // (deterministic from program ID + REGISTRY_SEED) before this admin
        // call lands. A naive `system_program::create_account` rejects pre-
        // funded accounts. Mirror Anchor's `init` constraint: top up the
        // deficit (if any) via Transfer, then Allocate + Assign.
        let existing = registry.lamports();
        if existing < required {
            let deficit = required - existing;
            invoke(
                &system_instruction::transfer(authority.key, registry.key, deficit),
                &[authority.clone(), registry.clone(), system.clone()],
            )?;
        }
        invoke_signed(
            &system_instruction::allocate(registry.key, initial as u64),
            &[registry.clone(), system.clone()],
            signer_seeds,
        )?;
        invoke_signed(
            &system_instruction::assign(registry.key, &crate::ID),
            &[registry.clone(), system.clone()],
            signer_seeds,
        )?;
        if initial == TARGET {
            write_gateway_registry_header(&registry, auth_pk)?;
            msg!(
                "GatewayRegistry created ({} max gateways)",
                GatewayRegistry::MAX_GATEWAYS
            );
        }
        return Ok(());
    }

    require!(current_len < TARGET, GarError::GatewayRegistryAlreadyExists);

    let next_len = (current_len + MAX_PERMITTED_DATA_INCREASE).min(TARGET);
    let min_balance = rent.minimum_balance(next_len);
    let needed = min_balance.saturating_sub(registry.lamports());
    if needed > 0 {
        invoke(
            &system_instruction::transfer(authority.key, registry.key, needed),
            &[authority.clone(), registry.clone(), system.clone()],
        )?;
    }
    registry.realloc(next_len, true)?;

    if next_len == TARGET {
        write_gateway_registry_header(&registry, auth_pk)?;
        msg!(
            "GatewayRegistry created ({} max gateways)",
            GatewayRegistry::MAX_GATEWAYS
        );
    }
    Ok(())
}

fn write_gateway_registry_header(registry: &AccountInfo, authority: Pubkey) -> Result<()> {
    let mut data = registry.try_borrow_mut_data()?;
    let disc = hash(b"account:GatewayRegistry");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);
    data[8..40].copy_from_slice(authority.as_ref());
    Ok(())
}

pub fn initialize_epochs(
    ctx: Context<InitializeEpochs>,
    params: InitializeEpochParams,
) -> Result<()> {
    let clock = Clock::get()?;
    let settings = &mut ctx.accounts.epoch_settings;

    settings.authority = params.authority;
    settings.epoch_duration = params.epoch_duration;
    settings.prescribed_observer_count = params.observer_count;
    settings.prescribed_name_count = params.name_count;
    settings.min_observer_stake = params.min_observer_stake;
    settings.slash_rate = params.slash_rate;
    settings.enabled = false;
    settings.current_epoch_index = 0;
    settings.genesis_timestamp = clock.unix_timestamp;
    // !!! DEVNET FAST-TEST VALUE — REVERT BEFORE MAINNET !!!
    // Production target is `180 * 86_400` (180 days, matches Lua
    // tenureWeightDurationMs). Shrunk to 1 hour so tenure weight ramps
    // across a handful of 5-min epochs on devnet for end-to-end
    // observer/cranker iteration. See docs/DEVNET_RUNBOOK.md → "Devnet
    // vs mainnet epoch-settings deltas". Long-term fix is to surface
    // this via `InitializeEpochParams` (separate ticket).
    settings.tenure_weight_duration = 3600; // 1 hour (DEVNET ONLY)
    settings.max_tenure_weight = 4; // matches Lua maxTenureWeight
    settings.gateway_reward_ratio = GATEWAY_OPERATOR_REWARD_RATE;
    settings.observer_reward_ratio = OBSERVER_REWARD_RATE;
    settings.missed_observation_penalty_rate = MISSED_OBSERVATION_PENALTY;
    settings.max_reward_rate = MAX_REWARD_RATE;
    settings.min_reward_rate = MIN_REWARD_RATE;
    settings.reward_decay_start_epoch = REWARD_DECAY_START_EPOCH;
    settings.reward_decay_last_epoch = REWARD_DECAY_LAST_EPOCH;
    settings.max_consecutive_failures = 30;
    settings.failed_gateway_slash_rate = 1_000_000;
    settings.disable_at = 0;
    settings.bump = ctx.bumps.epoch_settings;

    msg!("Epoch settings initialized");
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct InitializeGar<'info> {
    #[account(
        init,
        payer = payer,
        space = GatewaySettings::SIZE,
        seeds = [SETTINGS_SEED],
        bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    pub mint: Account<'info, Mint>,

    /// Bind initialize to the program's upgrade authority — closes the
    /// front-run window between `solana program deploy` and `initialize`
    /// (audit H1 / Theme C). Explicit `Some(payer.key())` equality
    /// rejects revoked upgrade authority (`None`).
    #[account(
        mut,
        constraint = program_data.upgrade_authority_address == Some(payer.key()) @ GarError::Unauthorized,
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

/// Create the GatewayRegistry zero-copy account. Must be called after [`initialize`].
#[derive(Accounts)]
pub struct CreateGatewayRegistry<'info> {
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        has_one = authority @ crate::error::GarError::Unauthorized,
    )]
    pub settings: Account<'info, GatewaySettings>,

    /// CHECK: PDA allocated via [`create_gateway_registry`] (manual CPI, not Anchor `init`).
    #[account(mut, seeds = [REGISTRY_SEED], bump)]
    pub registry: AccountInfo<'info>,

    #[account(mut)]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct InitializeEpochs<'info> {
    #[account(
        init,
        payer = payer,
        space = 8 + EpochSettings::SIZE,
        seeds = [EPOCH_SETTINGS_SEED],
        bump,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    /// Bind initialize_epochs to the program's upgrade authority — closes
    /// the front-run window between `solana program deploy` and
    /// `initialize_epochs` (audit H1 / Theme C). Explicit
    /// `Some(payer.key())` equality rejects revoked upgrade authority.
    #[account(
        mut,
        constraint = program_data.upgrade_authority_address == Some(payer.key()) @ GarError::Unauthorized,
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

/// One-shot admin: transfer SPL `Owner` authority of `protocol_token_account`
/// from the current GatewaySettings PDA to a target authority (typically
/// `ario-core`'s ArioConfig PDA).
///
/// Why this exists: at original deploy the protocol_token_account was created
/// under the GatewaySettings PDA's authority (the natural choice for a
/// fee-receipt account, since `ario-gar` is where stake/reward flows live).
/// The genesis-distribution flow added in 2026-05 (`ario-core::import_balance`
/// SPL transfer to pre-registered holders) needs ArioConfig PDA to sign
/// transfers FROM the treasury — a different program entirely. Re-architecting
/// to do cross-program-CPI'd treasury releases is heavier than just moving the
/// SPL Owner authority once. Mainnet creates the treasury under ArioConfig
/// from the start (see `migration/import/src/devnet-setup.ts`); this
/// instruction is the migration path for already-deployed clusters.
///
/// Authority required: `settings.authority` (the original protocol deployer).
/// `new_authority` is passed by the caller — typically the ArioConfig PDA
/// computed from `[ario_config, ARIO_CORE_PROGRAM_ID]`. Caller is responsible
/// for ensuring the destination is an actual signer (a PDA the new program
/// can sign for); this handler doesn't validate program ownership of the
/// target since the legitimate target is in a different program family.
///
/// Emits no event — one-shot infra change, indexers don't need it. Logs via
/// `msg!` for tx-log auditability.
pub fn release_treasury_authority(
    ctx: Context<ReleaseTreasuryAuthority>,
    new_authority: Pubkey,
) -> Result<()> {
    let cpi_accounts = SetAuthority {
        account_or_mint: ctx.accounts.protocol_token_account.to_account_info(),
        current_authority: ctx.accounts.settings.to_account_info(),
    };
    let bump = ctx.accounts.settings.bump;
    let signer_seeds: &[&[&[u8]]] = &[&[SETTINGS_SEED, &[bump]]];
    let cpi_ctx = CpiContext::new_with_signer(
        ctx.accounts.token_program.to_account_info(),
        cpi_accounts,
        signer_seeds,
    );
    token::set_authority(cpi_ctx, AuthorityType::AccountOwner, Some(new_authority))?;

    msg!(
        "protocol_token_account authority transferred: {} -> {}",
        ctx.accounts.settings.key(),
        new_authority
    );
    Ok(())
}

/// Admin: transfer SPL `Owner` authority of the protocol token account away
/// from the GatewaySettings PDA. See `release_treasury_authority` for
/// motivation.
#[derive(Accounts)]
pub struct ReleaseTreasuryAuthority<'info> {
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        has_one = authority @ crate::error::GarError::Unauthorized,
    )]
    pub settings: Account<'info, GatewaySettings>,

    pub authority: Signer<'info>,

    /// Treasury account whose SPL `Owner` authority is being transferred.
    /// Must currently be owned by the GatewaySettings PDA (i.e. its
    /// `owner` field equals `settings.key()`); the SetAuthority CPI
    /// would fail otherwise but we surface the mismatch as a typed
    /// error first.
    #[account(
        mut,
        constraint = protocol_token_account.owner == settings.key()
            @ crate::error::GarError::InvalidOwner,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
}

/// Admin recovery — repair `GatewaySettings` mint / stake_token_account /
/// protocol_token_account when devnet genesis was partially-initialized
/// (e.g. devnet-setup.ts crashed mid-script and a re-run created a fresh
/// mint, leaving GatewaySettings + ArioConfig pointed at orphaned accounts
/// while ArnsConfig was initialized with the new mint). See the matching
/// `ario_core::admin_repair_config` for the cross-program companion.
///
/// All three fields update atomically. Pre-cutover only — disabled by the
/// `migration_active` constraint (set false by `finalize_migration`).
///
/// No event emit on this path: existing GAR ConfigUpdatedEvent shape (if
/// any) is purpose-built for active-network field updates; one-shot
/// genesis-recovery isn't useful for indexers and would just add IDL
/// surface. `msg!` for tx-log auditability.
pub fn admin_repair_settings(
    ctx: Context<AdminRepairSettings>,
    new_mint: Pubkey,
    new_stake_token_account: Pubkey,
    new_protocol_token_account: Pubkey,
) -> Result<()> {
    let settings = &mut ctx.accounts.settings;
    settings.mint = new_mint;
    settings.stake_token_account = new_stake_token_account;
    settings.protocol_token_account = new_protocol_token_account;

    msg!(
        "GatewaySettings repaired: mint={}, stake={}, protocol={}",
        new_mint,
        new_stake_token_account,
        new_protocol_token_account
    );
    Ok(())
}

/// Admin recovery context — see `admin_repair_settings`.
#[derive(Accounts)]
pub struct AdminRepairSettings<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        has_one = authority @ crate::error::GarError::Unauthorized,
        constraint = settings.migration_active @ crate::error::GarError::MigrationInactive,
    )]
    pub settings: Account<'info, GatewaySettings>,

    pub authority: Signer<'info>,
}
