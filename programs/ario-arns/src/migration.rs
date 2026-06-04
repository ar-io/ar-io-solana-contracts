use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    hash::hash,
    program::{invoke, invoke_signed},
    system_instruction,
};

use crate::error::ArnsError;
use crate::state::*;

/// Migration deadline: imports are rejected after this timestamp.
// 2112-04-20 00:01:09 UTC -- INTENTIONALLY far-future BY DESIGN. The AR.IO Solana migration uses no
// time-based cutoff; finalize_migration (authority-gated, one-shot) is the sole control on the
// migration-authority write window. The constant is retained so the existing deadline checks
// compile and short-circuit harmlessly, and the far-future value (vs i64::MAX) keeps the time check
// permanently inert without overflow risk in deadline arithmetic. Accepted resolution of audit
// MIGRATION-001 (see docs/SECURITY_AUDIT_INDEPENDENT.md): the time-window risk is accepted, with
// finalize_migration bounding the window operationally.
pub const MIGRATION_DEADLINE: i64 = 4490553669;

// =========================================
// DISCRIMINATOR VALIDATION
// =========================================

/// Check if discriminator matches a known ario-arns account type and return expected SIZE.
fn known_discriminator(disc: &[u8; 8]) -> Option<usize> {
    // ArnsConfig intentionally excluded — config accounts must not be
    // overwritable via the generic import_account to prevent authority hijacking
    // by a compromised migration key.
    // DemandFactor intentionally excluded — it should only be initialized via the
    // normal `initialize` instruction, not overwritable via migration import.
    let checks: &[(&str, usize)] = &[
        ("account:ArnsRecord", ArnsRecord::SIZE),
        ("account:ReservedName", ReservedName::SIZE),
        ("account:ReturnedName", ReturnedName::SIZE),
    ];
    for (name, size) in checks {
        let h = hash(name.as_bytes());
        if &h.to_bytes()[..8] == disc {
            return Some(*size);
        }
    }
    None
}

// =========================================
// HANDLERS
// =========================================

pub fn import_account_handler(
    ctx: Context<ImportAccount>,
    seeds: Vec<Vec<u8>>,
    data: Vec<u8>,
) -> Result<()> {
    // Check migration deadline
    let clock = Clock::get()?;
    require!(
        clock.unix_timestamp < MIGRATION_DEADLINE,
        ArnsError::MigrationExpired
    );

    // Derive PDA from seeds
    let seed_slices: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let (derived_pda, bump) = Pubkey::find_program_address(&seed_slices, ctx.program_id);

    require!(
        derived_pda == ctx.accounts.account.key(),
        ArnsError::InvalidPda
    );

    // Validate discriminator
    require!(data.len() >= 8, ArnsError::InvalidAccountData);
    let disc: [u8; 8] = data[..8].try_into().unwrap();
    let expected_size = known_discriminator(&disc).ok_or(error!(ArnsError::InvalidAccountData))?;

    // Variable-length Anchor borsh (snapshot) must fit in max account size; the
    // encoded payload is always <= the precomputed Anchor `SIZE` constant because
    // strings/options use compact borsh encoding. Allocate the full max so future
    // mutating instructions (renew, set_undername_limit, …) don't need realloc.
    require!(data.len() <= expected_size, ArnsError::InvalidAccountData);

    // Create or overwrite account
    let account_info = ctx.accounts.account.to_account_info();

    if account_info.data_is_empty() {
        let bump_bytes = [bump];
        let mut seeds_with_bump: Vec<&[u8]> = seed_slices;
        seeds_with_bump.push(&bump_bytes);

        let rent = Rent::get()?;
        let required = rent.minimum_balance(expected_size);

        // Lamport-griefing defense: see ario-core/migration.rs comment.
        let existing = account_info.lamports();
        if existing < required {
            let deficit = required - existing;
            invoke(
                &system_instruction::transfer(ctx.accounts.payer.key, &derived_pda, deficit),
                &[
                    ctx.accounts.payer.to_account_info(),
                    account_info.clone(),
                    ctx.accounts.system_program.to_account_info(),
                ],
            )?;
        }
        invoke_signed(
            &system_instruction::allocate(&derived_pda, expected_size as u64),
            &[
                account_info.clone(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[&seeds_with_bump],
        )?;
        invoke_signed(
            &system_instruction::assign(&derived_pda, ctx.program_id),
            &[
                account_info.clone(),
                ctx.accounts.system_program.to_account_info(),
            ],
            &[&seeds_with_bump],
        )?;
    } else {
        // Idempotent re-import: allows overwriting previously imported accounts for retry/recovery.
        // SECURITY: migration_authority compromise allows arbitrary state corruption
        // pre-finalization. Defenses: MIGRATION_DEADLINE constant + finalize_migration.
        require!(
            account_info.owner == ctx.program_id,
            ArnsError::InvalidAccountData
        );
        require!(
            account_info.data_len() == expected_size,
            ArnsError::InvalidAccountData
        );
    }

    // Write data; zero-fill tail so account matches fixed Anchor max layout.
    let mut account_data = account_info.try_borrow_mut_data()?;
    let n = data.len().min(account_data.len());
    account_data[..n].copy_from_slice(&data[..n]);
    if n < account_data.len() {
        account_data[n..].fill(0);
    }

    Ok(())
}

pub fn import_registry_entry_handler(
    ctx: Context<ImportRegistryEntry>,
    name_hash: [u8; 32],
    registry_index: u32,
) -> Result<()> {
    // Check migration deadline
    let clock = Clock::get()?;
    require!(
        clock.unix_timestamp < MIGRATION_DEADLINE,
        ArnsError::MigrationExpired
    );

    let registry_info = &ctx.accounts.registry;
    let mut data = registry_info.try_borrow_mut_data()?;

    let count_before = name_registry_header(&data).count as usize;
    // registry_index must match the position being written
    require!(
        registry_index == count_before as u32,
        ArnsError::InvalidAccountData
    );

    let _written_idx = append_name_entry(
        &mut data,
        NameEntry {
            name_hash,
            registry_index,
            _padding: [0u8; 4],
        },
    )?;

    Ok(())
}

pub fn finalize_migration_handler(ctx: Context<FinalizeMigration>) -> Result<()> {
    let config = &mut ctx.accounts.config;
    config.migration_active = false;
    msg!("Migration finalized — import instructions permanently disabled");
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct ImportAccount<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
        constraint = config.migration_active @ ArnsError::MigrationInactive,
        constraint = config.migration_authority == authority.key() @ ArnsError::Unauthorized,
    )]
    pub config: Account<'info, ArnsConfig>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Validated via PDA derivation in handler
    #[account(mut)]
    pub account: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ImportRegistryEntry<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
        constraint = config.migration_active @ ArnsError::MigrationInactive,
        constraint = config.migration_authority == authority.key() @ ArnsError::Unauthorized,
    )]
    pub config: Account<'info, ArnsConfig>,
    pub authority: Signer<'info>,
    /// CHECK: Variable-size NameRegistry account. Validated by PDA-seed
    /// constraint; handler uses byte-offset helpers (`append_name_entry`,
    /// `name_registry_header`) instead of `AccountLoader` because the
    /// header is fixed-size but the slot array isn't (ADR-020).
    #[account(mut, seeds = [crate::state::NAME_REGISTRY_SEED], bump)]
    pub registry: AccountInfo<'info>,
}

#[derive(Accounts)]
pub struct FinalizeMigration<'info> {
    #[account(
        mut,
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
        constraint = config.migration_active @ ArnsError::MigrationAlreadyFinalized,
        constraint = config.authority == authority.key() @ ArnsError::Unauthorized,
    )]
    pub config: Account<'info, ArnsConfig>,
    pub authority: Signer<'info>,
}
