//! Post-finalize recovery instructions.
//!
//! **THIS MODULE IS BUILD-GATED BEHIND `--features recovery` AND MUST NOT
//! SHIP TO MAINNET.** A default build (no `--features recovery`) compiles
//! this module out entirely — neither the dispatch arms in `lib.rs` nor
//! the discriminators below appear in the resulting `.so`.
//!
//! ## Why this exists
//!
//! On 2026-05-24 the staging-v1 import surfaced ~1,000 silent batched-tx
//! failures in phases 4 / 5 (gateways + user state). Verify confirmed
//! real on-chain gaps:
//!   - 261 missing Vault PDAs
//!   - 553 missing PrimaryName PDAs
//!   - 214 missing Balance PDAs
//!   - 159 missing Gateway PDAs (in ario-gar's recovery module)
//!   - 1 missing ANT (in ario-ant's recovery module)
//!
//! Phase 6 had already fired `finalize_migration`, flipping
//! `config.migration_active` to `false` across all 4 programs. That
//! permanently disables `import_*` ixs by design — the existing
//! `admin_repair_config` is also `migration_active`-gated. Without a
//! one-time recovery surface, ~90 SOL of PDA rents on staging-v1
//! would be permanently stranded and the cluster would forever sit
//! at ~95% data.
//!
//! ## What this module is
//!
//! Two ixs (in ario-core; ario-gar / ario-arns / ario-ant carry only the
//! generic one) that mirror the existing `import_account_handler` and
//! `import_balance_handler` bodies, but:
//!
//! - **Authority:** `config.authority` (multi-sig) — NOT
//!   `config.migration_authority` (the hot key, which is now considered
//!   compromised by exposure during the long import run).
//! - **No `migration_active` constraint** — that's the whole point.
//! - **Fail-if-exists guard:** explicit `require!(data_is_empty, …)` so
//!   recovery refuses to overwrite any account that survived the
//!   original import. Defense-in-depth against a stale gap list.
//! - **No MIGRATION_DEADLINE check** — recovery may need to run after
//!   the original deadline lapses.
//!
//! ## Lifecycle
//!
//! 1. Build `staging-recovery` branch with `--features recovery`.
//! 2. Upgrade the 4 programs at the staging-v1 program IDs (multi-sig
//!    signs the BPFLoaderUpgradeable upgrade).
//! 3. Run the verify-derived gap-list import via
//!    `migration/import/src/repair.ts` in the operator monorepo.
//! 4. Run verify; confirm 100% clean.
//! 5. **Re-lock**: rebuild from `develop` without `--features recovery`
//!    and deploy again at the same program IDs. The deployed `.so` no
//!    longer contains the repair ixs.
//!
//! Mainnet build = `develop` branch, default features. The CI guard at
//! `.github/workflows/recovery-feature-guard.yml` greps any release-build
//! `.so` for the repair discriminators and fails the build if any are
//! found — belt-and-braces against an accidental merge.

#![cfg(feature = "recovery")]

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

/// Discriminator-to-size map. Mirrors `migration::known_discriminator`
/// — duplicated here (not re-used) so the recovery module is fully
/// self-contained behind `#[cfg(feature = "recovery")]`. If a new account
/// type is added to ario-core, update BOTH lists.
fn recovery_known_discriminator(disc: &[u8; 8]) -> Option<usize> {
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
// HANDLER: generic per-PDA repair
// =========================================

pub fn admin_post_finalize_repair_account_handler(
    ctx: Context<AdminPostFinalizeRepairAccount>,
    seeds: Vec<Vec<u8>>,
    data: Vec<u8>,
) -> Result<()> {
    // Derive PDA from seeds and confirm it matches the passed account.
    let seed_slices: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let (derived_pda, bump) = Pubkey::find_program_address(&seed_slices, ctx.program_id);
    require!(
        derived_pda == ctx.accounts.account.key(),
        ArioError::InvalidPda
    );

    // Validate discriminator and look up expected allocation size.
    require!(data.len() >= 8, ArioError::InvalidAccountData);
    let disc: [u8; 8] = data[..8].try_into().unwrap();
    let expected_size =
        recovery_known_discriminator(&disc).ok_or(error!(ArioError::InvalidAccountData))?;
    require!(data.len() <= expected_size, ArioError::InvalidAccountData);

    // FAIL-IF-EXISTS guard. Repair is intentionally never an overwrite —
    // if the gap list is stale, we want to know loudly, not silently
    // corrupt working state.
    let account_info = ctx.accounts.account.to_account_info();
    require!(
        account_info.data_is_empty(),
        ArioError::AccountAlreadyExists
    );

    let bump_bytes = [bump];
    let mut seeds_with_bump: Vec<&[u8]> = seed_slices;
    seeds_with_bump.push(&bump_bytes);

    let rent = Rent::get()?;
    let required = rent.minimum_balance(expected_size);

    // Lamport-griefing defense, matching `import_account_handler`. Top up
    // any pre-funded lamports before Allocate/Assign so a 1-lamport
    // pre-fund attack can't block recovery.
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

    // Write the snapshot payload, leaving the tail zero-padded (same as
    // the original import — verify/checks/field-comparison.ts is aware
    // of this and treats zero-tail as a non-mismatch).
    let mut account_data = account_info.try_borrow_mut_data()?;
    account_data[..data.len()].copy_from_slice(&data);

    msg!(
        "[recovery] repaired account {} ({} bytes payload, {} bytes allocated)",
        derived_pda,
        data.len(),
        expected_size
    );

    Ok(())
}

// =========================================
// HANDLER: Balance repair (with treasury → ATA SPL transfer)
// =========================================

pub fn admin_post_finalize_repair_balance_handler(
    ctx: Context<AdminPostFinalizeRepairBalance>,
    owner: Pubkey,
    amount: u64,
) -> Result<()> {
    // `init` on the Balance account in the struct below means a re-call
    // for the same owner is rejected by Anchor's preamble BEFORE this
    // handler runs ("account already in use"). That's our fail-if-exists
    // guarantee — no risk of double-transfer if the gap list is stale.

    let balance = &mut ctx.accounts.balance;
    balance.owner = owner;
    balance.amount = amount;
    balance.bump = ctx.bumps.balance;
    balance.version = BALANCE_VERSION;

    // Pre-registered branch: distribute tokens to the recipient ATA.
    // Unregistered (owner == migration_authority) skips — those tokens
    // stay in treasury until the holder registers and an out-of-band
    // flow (escrow deposit per ADR-014) moves them.
    //
    // Mirrors `import_balance_handler` exactly; the only practical
    // difference is which authority signs this ix.
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

    msg!("[recovery] repaired balance for owner {} ({} mARIO)", owner, amount);

    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct AdminPostFinalizeRepairAccount<'info> {
    // Read-only: handler doesn't mutate config. CRUCIALLY uses
    // `config.authority` (multi-sig), NOT `config.migration_authority` —
    // the migration hot key is considered exposed by the long import run
    // and we don't want to expand its post-finalize blast radius.
    //
    // No `migration_active` constraint — by design, since the whole
    // reason this ix exists is to operate post-finalize.
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.authority == authority.key() @ ArioError::Unauthorized,
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
pub struct AdminPostFinalizeRepairBalance<'info> {
    // Read-only; uses multi-sig authority. Signs the SPL transfer below
    // as the treasury authority via signer_seeds.
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.authority == authority.key() @ ArioError::Unauthorized,
    )]
    pub config: Account<'info, ArioConfig>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// `init` (not `init_if_needed`) — Anchor's preamble rejects a
    /// duplicate call for the same `owner` before the handler runs,
    /// which is our atomic guard against double-transferring tokens.
    #[account(
        init,
        payer = payer,
        space = Balance::SIZE,
        seeds = [BALANCE_SEED, owner.as_ref()],
        bump,
    )]
    pub balance: Account<'info, Balance>,

    /// Treasury source for the SPL transfer. Must be `config.treasury`.
    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArioError::InvalidTreasury,
        constraint = protocol_token_account.owner == config.key() @ ArioError::InvalidOwner,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// Recipient ATA for the owner.
    #[account(
        init_if_needed,
        payer = payer,
        associated_token::mint = ario_mint,
        associated_token::authority = recipient_owner,
    )]
    pub recipient_token_account: Account<'info, TokenAccount>,

    /// CHECK: ATA authority.
    #[account(constraint = recipient_owner.key() == owner @ ArioError::InvalidOwner)]
    pub recipient_owner: UncheckedAccount<'info>,

    #[account(constraint = ario_mint.key() == config.mint @ ArioError::InvalidAccountState)]
    pub ario_mint: Account<'info, Mint>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}
