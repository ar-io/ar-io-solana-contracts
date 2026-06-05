use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    bpf_loader_upgradeable,
    hash::hash,
    program::{invoke, invoke_signed},
    system_instruction,
};

use crate::error::AntError;
use crate::state::*;

// --- Heap-free borsh structural validation (import path) --------------------
//
// Full `AccountDeserialize::try_deserialize` for AntConfig/AntRecord allocates large
// `String`/`Vec` on the BPF heap and can OOM on validators (e.g. Surfpool) even with
// RequestHeapFrame. We walk the Anchor borsh layout with a byte cursor only.

fn read_u8(data: &[u8], i: &mut usize) -> Result<u8> {
    require!(*i < data.len(), AntError::InvalidAccountData);
    let b = data[*i];
    *i += 1;
    Ok(b)
}

fn read_u32_le(data: &[u8], i: &mut usize) -> Result<u32> {
    require!(data.len() >= *i + 4, AntError::InvalidAccountData);
    let v = u32::from_le_bytes(data[*i..*i + 4].try_into().unwrap());
    *i += 4;
    Ok(v)
}

fn skip_pubkey(data: &[u8], i: &mut usize) -> Result<()> {
    require!(data.len() >= *i + 32, AntError::InvalidAccountData);
    *i += 32;
    Ok(())
}

fn skip_string(data: &[u8], i: &mut usize) -> Result<()> {
    let len = read_u32_le(data, i)? as usize;
    require!(data.len() >= *i + len, AntError::InvalidAccountData);
    *i += len;
    Ok(())
}

fn skip_vec_string(data: &[u8], i: &mut usize) -> Result<()> {
    let n = read_u32_le(data, i)? as usize;
    require!(n <= MAX_KEYWORDS, AntError::InvalidAccountData);
    for _ in 0..n {
        skip_string(data, i)?;
    }
    Ok(())
}

fn skip_vec_pubkey(data: &[u8], i: &mut usize) -> Result<()> {
    let n = read_u32_le(data, i)? as usize;
    require!(n <= MAX_CONTROLLERS, AntError::InvalidAccountData);
    require!(data.len() >= *i + n * 32, AntError::InvalidAccountData);
    *i += n * 32;
    Ok(())
}

fn skip_option_u32(data: &[u8], i: &mut usize) -> Result<()> {
    let tag = read_u8(data, i)?;
    if tag == 1 {
        read_u32_le(data, i)?;
    } else {
        require!(tag == 0, AntError::InvalidAccountData);
    }
    Ok(())
}

fn skip_option_pubkey(data: &[u8], i: &mut usize) -> Result<()> {
    let tag = read_u8(data, i)?;
    if tag == 1 {
        skip_pubkey(data, i)?;
    } else {
        require!(tag == 0, AntError::InvalidAccountData);
    }
    Ok(())
}

fn skip_option_string(data: &[u8], i: &mut usize) -> Result<()> {
    let tag = read_u8(data, i)?;
    if tag == 1 {
        skip_string(data, i)?;
    } else {
        require!(tag == 0, AntError::InvalidAccountData);
    }
    Ok(())
}

fn skip_option_vec_string(data: &[u8], i: &mut usize) -> Result<()> {
    let tag = read_u8(data, i)?;
    if tag == 1 {
        skip_vec_string(data, i)?;
    } else {
        require!(tag == 0, AntError::InvalidAccountData);
    }
    Ok(())
}

/// After the last borsh field, import payloads may be padded with zeros up to `SIZE`.
fn require_trailing_zero_padding(data: &[u8], i: usize) -> Result<()> {
    require!(
        data[i..].iter().all(|&b| b == 0),
        AntError::InvalidAccountData
    );
    Ok(())
}

/// Skip the three-byte `SchemaVersion { major, minor, patch }` field.
fn skip_schema_version(data: &[u8], i: &mut usize) -> Result<()> {
    require!(
        data.len() >= *i + SCHEMA_VERSION_SIZE,
        AntError::InvalidAccountData
    );
    *i += SCHEMA_VERSION_SIZE;
    Ok(())
}

fn validate_ant_config_borsh_payload(data: &[u8]) -> Result<()> {
    let mut i = 8usize;
    skip_pubkey(data, &mut i)?;
    skip_string(data, &mut i)?;
    skip_string(data, &mut i)?;
    skip_string(data, &mut i)?;
    skip_string(data, &mut i)?;
    skip_vec_string(data, &mut i)?;
    skip_pubkey(data, &mut i)?;
    let _bump = read_u8(data, &mut i)?;
    skip_schema_version(data, &mut i)?;
    require_trailing_zero_padding(data, i)
}

fn validate_ant_controllers_borsh_payload(data: &[u8]) -> Result<()> {
    let mut i = 8usize;
    skip_pubkey(data, &mut i)?;
    skip_vec_pubkey(data, &mut i)?;
    let _bump = read_u8(data, &mut i)?;
    skip_schema_version(data, &mut i)?;
    require_trailing_zero_padding(data, i)
}

fn validate_ant_record_borsh_payload(data: &[u8]) -> Result<()> {
    let mut i = 8usize;
    skip_pubkey(data, &mut i)?; // mint
    skip_string(data, &mut i)?; // undername
    skip_string(data, &mut i)?; // target
    let _protocol = read_u8(data, &mut i)?; // target_protocol
    read_u32_le(data, &mut i)?; // ttl_seconds
    skip_option_u32(data, &mut i)?; // priority
    skip_option_pubkey(data, &mut i)?; // owner
    skip_pubkey(data, &mut i)?; // last_reconciled_owner
    let _bump = read_u8(data, &mut i)?;
    skip_schema_version(data, &mut i)?;
    require_trailing_zero_padding(data, i)
}

fn validate_ant_record_metadata_borsh_payload(data: &[u8]) -> Result<()> {
    let mut i = 8usize;
    skip_pubkey(data, &mut i)?; // mint
                                // undername_hash is a fixed [u8; 32], read directly
    require!(data.len() >= i + 32, AntError::InvalidAccountData);
    i += 32;
    skip_option_string(data, &mut i)?; // display_name
    skip_option_string(data, &mut i)?; // record_logo
    skip_option_string(data, &mut i)?; // record_description
    skip_option_vec_string(data, &mut i)?; // record_keywords
    let _bump = read_u8(data, &mut i)?;
    skip_schema_version(data, &mut i)?;
    require_trailing_zero_padding(data, i)
}

/// Structural check so truncated or garbage blobs fail (replaces full heap deserialize).
fn validate_ant_account_borsh(disc: &[u8; 8], data: &[u8]) -> Result<()> {
    let h_cfg = hash(b"account:AntConfig").to_bytes();
    let h_ctrl = hash(b"account:AntControllers").to_bytes();
    let h_rec = hash(b"account:AntRecord").to_bytes();
    let h_meta = hash(b"account:AntRecordMetadata").to_bytes();
    if disc.as_slice() == &h_cfg[..8] {
        validate_ant_config_borsh_payload(data)?;
    } else if disc.as_slice() == &h_ctrl[..8] {
        validate_ant_controllers_borsh_payload(data)?;
    } else if disc.as_slice() == &h_rec[..8] {
        validate_ant_record_borsh_payload(data)?;
    } else if disc.as_slice() == &h_meta[..8] {
        validate_ant_record_metadata_borsh_payload(data)?;
    } else {
        return err!(AntError::InvalidAccountData);
    }
    Ok(())
}

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

/// Check if discriminator matches a known ario-ant account type and return expected SIZE.
fn known_discriminator(disc: &[u8; 8]) -> Option<usize> {
    let checks: &[(&str, usize)] = &[
        ("account:AntConfig", AntConfig::SIZE),
        ("account:AntControllers", AntControllers::SIZE),
        ("account:AntRecord", AntRecord::SIZE),
        ("account:AntRecordMetadata", AntRecordMetadata::SIZE),
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
// PARAMS
// =========================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeAntMigrationParams {
    pub authority: Pubkey,
    pub migration_authority: Pubkey,
}

// =========================================
// HANDLERS
// =========================================

pub fn initialize_migration_handler(
    ctx: Context<InitializeAntMigration>,
    params: InitializeAntMigrationParams,
) -> Result<()> {
    let config = &mut ctx.accounts.migration_config;
    config.authority = params.authority;
    config.migration_authority = params.migration_authority;
    config.migration_active = true;
    config.bump = ctx.bumps.migration_config;
    config.version = ANT_MIGRATION_CONFIG_VERSION;
    msg!("ANT migration config initialized");
    Ok(())
}

pub fn import_account_handler(
    ctx: Context<ImportAccount>,
    seeds: Vec<Vec<u8>>,
    data: Vec<u8>,
) -> Result<()> {
    // Check migration deadline
    let clock = Clock::get()?;
    require!(
        clock.unix_timestamp < MIGRATION_DEADLINE,
        AntError::MigrationExpired
    );

    // Derive PDA from seeds
    let seed_slices: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let (derived_pda, bump) = Pubkey::find_program_address(&seed_slices, ctx.program_id);

    require!(
        derived_pda == ctx.accounts.account.key(),
        AntError::InvalidPda
    );

    // Validate discriminator
    require!(data.len() >= 8, AntError::InvalidAccountData);
    let disc: [u8; 8] = data[..8].try_into().unwrap();
    let expected_size = known_discriminator(&disc).ok_or(error!(AntError::InvalidAccountData))?;

    // Variable-length Anchor borsh (snapshot) must fit in max account size. Exact SIZE would
    // not fit in a single Solana tx (1232-byte packet limit) when embedded in import_account.
    require!(data.len() <= expected_size, AntError::InvalidAccountData);
    validate_ant_account_borsh(&disc, &data)?;

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
            AntError::InvalidAccountData
        );
        require!(
            account_info.data_len() == expected_size,
            AntError::InvalidAccountData
        );
        require!(data.len() <= expected_size, AntError::InvalidAccountData);
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

pub fn finalize_migration_handler(ctx: Context<FinalizeMigration>) -> Result<()> {
    let config = &mut ctx.accounts.migration_config;
    config.migration_active = false;
    msg!("Migration finalized — import instructions permanently disabled");
    Ok(())
}

/// Single-step rotation of the admin `authority` on `AntMigrationConfig`
/// (ADR-026). Gated on the CURRENT admin `authority` (never
/// `migration_authority`). Rejects the null pubkey; any other pubkey is
/// allowed, including off-curve PDAs (the Squads vault). No `migration_active`
/// constraint — the admin authority (which gates
/// `admin_close_orphaned_ant_state`) must stay rotatable after migration is
/// finalized.
pub fn transfer_authority_handler(
    ctx: Context<TransferAuthority>,
    new_authority: Pubkey,
) -> Result<()> {
    require!(
        new_authority != Pubkey::default(),
        AntError::InvalidAuthority
    );

    let config = &mut ctx.accounts.migration_config;
    let old_authority = config.authority;
    config.authority = new_authority;

    emit!(crate::AuthorityTransferredEvent {
        old_authority,
        new_authority,
        timestamp: Clock::get()?.unix_timestamp,
    });

    msg!(
        "AntMigrationConfig.authority {} → {} (admin rotation)",
        old_authority,
        new_authority
    );
    Ok(())
}

/// Context for `transfer_authority`. `has_one = authority` binds the signer to
/// the CURRENT admin authority — the only load-bearing check. No
/// `migration_active` gate so rotation works post-finalize.
#[derive(Accounts)]
pub struct TransferAuthority<'info> {
    #[account(
        mut,
        seeds = [ANT_MIGRATION_CONFIG_SEED],
        bump = migration_config.bump,
        has_one = authority @ AntError::Unauthorized,
    )]
    pub migration_config: Account<'info, AntMigrationConfig>,
    pub authority: Signer<'info>,
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct InitializeAntMigration<'info> {
    #[account(
        init,
        payer = payer,
        space = AntMigrationConfig::SIZE,
        seeds = [ANT_MIGRATION_CONFIG_SEED],
        bump,
    )]
    pub migration_config: Account<'info, AntMigrationConfig>,

    /// Payer must be the program's upgrade authority. Explicit `Some(...)`
    /// equality rejects revoked upgrade authority (`None`); the previous
    /// `unwrap_or_default()` would have coerced revoked authority to the
    /// System Program pubkey, which is fragile defense-in-depth (audit
    /// Theme C, 2026-04).
    #[account(
        mut,
        constraint = program_data.upgrade_authority_address == Some(payer.key()) @ AntError::Unauthorized,
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

#[derive(Accounts)]
pub struct ImportAccount<'info> {
    #[account(
        seeds = [ANT_MIGRATION_CONFIG_SEED],
        bump = migration_config.bump,
        constraint = migration_config.migration_active @ AntError::MigrationInactive,
        constraint = migration_config.migration_authority == authority.key() @ AntError::Unauthorized,
    )]
    pub migration_config: Account<'info, AntMigrationConfig>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Validated via PDA derivation in handler
    #[account(mut)]
    pub account: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FinalizeMigration<'info> {
    #[account(
        mut,
        seeds = [ANT_MIGRATION_CONFIG_SEED],
        bump = migration_config.bump,
        constraint = migration_config.migration_active @ AntError::MigrationAlreadyFinalized,
        constraint = migration_config.authority == authority.key() @ AntError::Unauthorized,
    )]
    pub migration_config: Account<'info, AntMigrationConfig>,
    pub authority: Signer<'info>,
}

// Encoded-fixture tests (`ant-import-sample.json`) were removed alongside the
// snapshot's ANT serializer. Borsh encoding now lives in
// `migration/import/src/transform-ant.ts` and is exercised by
// `yarn test:ant-fixture` (raw → transform → contract bytes roundtrip) and the
// in-process Rust E2E `test_e2e_migration_import_full_ant_state` (synthesized
// bytes → on-chain `import_account`).
