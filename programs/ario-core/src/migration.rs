use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    hash::hash,
    program::{invoke, invoke_signed},
    system_instruction,
};
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token::{self, Mint, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::ArioError;
use crate::state::*;

/// Migration deadline: imports are rejected after this timestamp.
// Migration deadline: 2026-06-18 00:00:00 UTC. Update before mainnet if migration date changes.
pub const MIGRATION_DEADLINE: i64 = 1781884800;

// =========================================
// DISCRIMINATOR VALIDATION
// =========================================

/// Check if discriminator matches a known ario-core account type and return expected SIZE.
fn known_discriminator(disc: &[u8; 8]) -> Option<usize> {
    let checks: &[(&str, usize)] = &[
        ("account:Balance", Balance::SIZE),
        ("account:Vault", Vault::SIZE),
        ("account:VaultCounter", VaultCounter::SIZE),
        ("account:PrimaryName", PrimaryName::SIZE),
        ("account:PrimaryNameRequest", PrimaryNameRequest::SIZE),
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
        ArioError::MigrationExpired
    );

    // Derive PDA from seeds
    let seed_slices: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let (derived_pda, bump) = Pubkey::find_program_address(&seed_slices, ctx.program_id);

    require!(
        derived_pda == ctx.accounts.account.key(),
        ArioError::InvalidPda
    );

    // Validate discriminator
    require!(data.len() >= 8, ArioError::InvalidAccountData);
    let disc: [u8; 8] = data[..8].try_into().unwrap();
    let expected_size = known_discriminator(&disc).ok_or(error!(ArioError::InvalidAccountData))?;

    // Variable-length Anchor borsh (snapshot) must fit in max account size; the
    // encoded payload is always <= the precomputed Anchor `SIZE` constant because
    // strings/options use compact borsh encoding. Allocate the full max so future
    // mutating instructions don't need realloc.
    require!(data.len() <= expected_size, ArioError::InvalidAccountData);

    // Create or overwrite account
    let account_info = ctx.accounts.account.to_account_info();

    if account_info.data_is_empty() {
        let bump_bytes = [bump];
        let mut seeds_with_bump: Vec<&[u8]> = seed_slices;
        seeds_with_bump.push(&bump_bytes);

        let rent = Rent::get()?;
        let required = rent.minimum_balance(expected_size);

        // Lamport-griefing defense: an attacker can pre-fund the predicted PDA
        // with 1 lamport before the migration authority's tx lands. A naive
        // `system_program::create_account` rejects pre-funded accounts with
        // AccountAlreadyInUse. Mirror Anchor's `init` constraint: top up the
        // deficit (if any) via Transfer, then Allocate + Assign — both
        // tolerate pre-existing lamports.
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
            ArioError::InvalidAccountData
        );
        require!(
            account_info.data_len() == expected_size,
            ArioError::InvalidAccountData
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

/// Typed migration import for `Balance` accounts.
///
/// Genesis-distribution path: writes the off-chain `Balance` audit record AND
/// (when the owner is pre-registered, i.e. resolves to a real Solana
/// destination rather than the `migration_authority` placeholder) transfers
/// `amount` mARIO from `protocol_token_account` to the owner's ATA. The
/// ArioConfig PDA signs the SPL transfer as the treasury authority.
///
/// Two branches:
///
/// * `owner == config.migration_authority` (unregistered AO holder)
///   → write Balance PDA only; tokens stay in treasury for the future
///     escrow flow (per ADR-014 follow-up). The PDA's `owner` field still
///     records the placeholder; the AR address mapping lives off-chain in
///     `address-map.json`.
///
/// * `owner != config.migration_authority` (pre-registered)
///   → write Balance PDA AND CPI `token::transfer` treasury → recipient ATA,
///     signed by ArioConfig PDA. The recipient ATA is `init_if_needed`, so
///     callers don't need to pre-create.
///
/// `Balance` is also surfaced in the IDL via this typed entry point — Anchor
/// would otherwise omit it (no non-migration instruction touches `Balance`
/// directly), forcing off-chain tools to hand-roll Borsh + discriminator
/// bytes. Codama-generated clients consume the IDL type.
///
/// Bounded by `MIGRATION_DEADLINE`, gated by `migration_authority`, and
/// non-idempotent by design: the Balance PDA uses `init` (not
/// `init_if_needed`), so a re-import for the same owner is rejected with
/// `account already in use`. Solana txs are atomic — if a tx succeeded,
/// the orchestrator should never retry it. The hard failure on retry
/// catches progress-tracker bugs before they cause a silent double SPL
/// transfer.
pub fn import_balance_handler(
    ctx: Context<ImportBalance>,
    owner: Pubkey,
    amount: u64,
) -> Result<()> {
    let clock = Clock::get()?;
    require!(
        clock.unix_timestamp < MIGRATION_DEADLINE,
        ArioError::MigrationExpired
    );

    let balance = &mut ctx.accounts.balance;
    balance.owner = owner;
    balance.amount = amount;
    balance.bump = ctx.bumps.balance;

    // Pre-registered branch: distribute tokens to the recipient ATA.
    // Unregistered (owner == migration_authority) skips — those tokens stay
    // in treasury until the holder registers and an out-of-band flow (escrow
    // deposit per ADR-014) moves them.
    if owner != ctx.accounts.config.migration_authority {
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.protocol_token_account.to_account_info(),
            to: ctx.accounts.recipient_token_account.to_account_info(),
            authority: ctx.accounts.config.to_account_info(),
        };
        let bump = ctx.accounts.config.bump;
        let signer_seeds: &[&[&[u8]]] = &[&[CONFIG_SEED, &[bump]]];
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer_seeds,
        );
        token::transfer(cpi_ctx, amount)?;
    }

    Ok(())
}

pub fn finalize_migration_handler(ctx: Context<FinalizeMigration>) -> Result<()> {
    // Watershed event — fires exactly once. The
    // `config.migration_active` constraint on `FinalizeMigration` (in the
    // accounts struct below) gates re-entry: once flipped to false, the
    // constraint rejects subsequent calls with `MigrationAlreadyFinalized`.
    let admin = ctx.accounts.authority.key();
    let total_supply = ctx.accounts.config.total_supply;
    let clock = Clock::get()?;
    let slot = clock.slot;

    let config = &mut ctx.accounts.config;
    config.migration_active = false;

    emit!(CoreMigrationFinalizedEvent {
        admin,
        total_supply,
        slot,
        timestamp: clock.unix_timestamp,
    });

    msg!("Migration finalized — import instructions permanently disabled");
    Ok(())
}

pub fn finalize_supply_handler(
    ctx: Context<FinalizeSupply>,
    total_supply: u64,
    protocol_balance: u64,
    circulating_supply: u64,
    locked_supply: u64,
) -> Result<()> {
    // Validate supply invariant: circulating + locked + protocol == total
    let computed_total = circulating_supply
        .checked_add(locked_supply)
        .and_then(|v| v.checked_add(protocol_balance))
        .ok_or(ArioError::ArithmeticOverflow)?;
    require!(computed_total == total_supply, ArioError::InvalidParameter);

    let admin = ctx.accounts.authority.key();
    let clock = Clock::get()?;
    let config = &mut ctx.accounts.config;
    config.total_supply = total_supply;
    config.protocol_balance = protocol_balance;
    config.circulating_supply = circulating_supply;
    config.locked_supply = locked_supply;

    // Watershed event — supply genesis is permanent. `decimals` is the
    // canonical TOKEN_DECIMALS constant (the SPL mint's actual decimals
    // are not surfaced in the `ArioConfig` PDA, but the protocol pins
    // the value via `crate::constants::TOKEN_DECIMALS`).
    emit!(SupplyFinalizedEvent {
        admin,
        total_supply,
        decimals: crate::constants::TOKEN_DECIMALS,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "Supply totals set: total={}, protocol={}, circulating={}, locked={}",
        total_supply,
        protocol_balance,
        circulating_supply,
        locked_supply
    );
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct ImportAccount<'info> {
    // Read-only: the handler only validates `migration_active`,
    // `migration_authority`, and the bump — it never mutates `config`. Marking
    // this `mut` would require the import client to pass `config` as a
    // writable account; for parity with `ario-ant`, `ario-arns`, and
    // `ario-gar`, keep it read-only.
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.migration_active @ ArioError::MigrationInactive,
        constraint = config.migration_authority == authority.key() @ ArioError::Unauthorized,
    )]
    pub config: Account<'info, ArioConfig>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Validated via PDA derivation in handler
    #[account(mut)]
    pub account: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(owner: Pubkey)]
pub struct ImportBalance<'info> {
    // Read-only: handler only validates migration_active + authority —
    // never mutates config. Allows concurrent import_balance calls without
    // write-lock contention. Also signs the SPL transfer below as the
    // treasury authority (CPI uses signer_seeds, so no `mut` needed).
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.migration_active @ ArioError::MigrationInactive,
        constraint = config.migration_authority == authority.key() @ ArioError::Unauthorized,
    )]
    pub config: Account<'info, ArioConfig>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// `init` (not `init_if_needed`) so a re-import for the same owner is
    /// rejected with `account already in use`. Solana txs are atomic, so
    /// there's no partial-success state to recover from — if this PDA
    /// exists, the import already ran and (for pre-registered owners)
    /// already moved SPL tokens. Failing loud beats silent double-transfer
    /// when the orchestrator's progress tracker is wrong.
    #[account(
        init,
        payer = payer,
        space = Balance::SIZE,
        seeds = [BALANCE_SEED, owner.as_ref()],
        bump,
    )]
    pub balance: Account<'info, Balance>,

    /// Treasury source for the SPL transfer. Must be `config.treasury`
    /// (the protocol_token_account on the ARIO mint). Owned (SPL-level)
    /// by the ArioConfig PDA — see `release_treasury_authority` for the
    /// one-shot ownership migration on networks where the treasury was
    /// originally created under a different authority.
    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArioError::InvalidTreasury,
        constraint = protocol_token_account.owner == config.key() @ ArioError::InvalidOwner,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// Recipient ATA for the owner. `init_if_needed` so callers don't
    /// have to pre-create — the migration orchestrator can fan out
    /// imports without a per-holder ATA-creation pre-pass.
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = ario_mint,
        associated_token::authority = recipient_owner,
    )]
    pub recipient_token_account: Account<'info, TokenAccount>,

    /// CHECK: ATA authority. Validated structurally by the
    /// `associated_token::authority` constraint above and by the `owner`
    /// instruction-arg constraint here (they must match — the import is
    /// for `owner`'s share, not some third party's).
    #[account(constraint = recipient_owner.key() == owner @ ArioError::InvalidOwner)]
    pub recipient_owner: UncheckedAccount<'info>,

    #[account(constraint = ario_mint.key() == config.mint @ ArioError::InvalidAccountState)]
    pub ario_mint: Account<'info, Mint>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct FinalizeMigration<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.migration_active @ ArioError::MigrationAlreadyFinalized,
        constraint = config.authority == authority.key() @ ArioError::Unauthorized,
    )]
    pub config: Account<'info, ArioConfig>,
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct FinalizeSupply<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.migration_active @ ArioError::MigrationInactive,
        // Supply finalization uses main authority (multi-sig), not migration hot key
        constraint = config.authority == authority.key() @ ArioError::Unauthorized,
    )]
    pub config: Account<'info, ArioConfig>,
    pub authority: Signer<'info>,
}
