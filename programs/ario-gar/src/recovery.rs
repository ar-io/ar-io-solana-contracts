//! Post-finalize recovery instruction for ario-gar.
//!
//! Build-gated behind `--features recovery`. See
//! `programs/ario-core/src/recovery.rs` for the full design rationale
//! and lifecycle. This module mirrors the ario-core generic-repair
//! handler with ario-gar's account types (Gateway, Delegation,
//! Withdrawal, etc.) and multi-sig authority field
//! (`settings.authority`).

#![cfg(feature = "recovery")]

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    hash::hash,
    program::{invoke, invoke_signed},
    system_instruction,
};

use crate::error::GarError;
use crate::state::*;

/// Discriminator-to-size map. Mirrors `migration::known_discriminator`
/// — duplicated for module self-containment. If a new account type is
/// added to ario-gar, update BOTH lists.
fn recovery_known_discriminator(disc: &[u8; 8]) -> Option<usize> {
    let checks: &[(&str, usize)] = &[
        ("account:Gateway", Gateway::SIZE),
        ("account:Delegation", Delegation::SIZE),
        ("account:Withdrawal", Withdrawal::SIZE),
        ("account:WithdrawalCounter", WithdrawalCounter::SIZE),
        ("account:AllowlistEntry", AllowlistEntry::SIZE),
        ("account:RedelegationRecord", RedelegationRecord::SIZE),
        ("account:EpochSettings", EpochSettings::SIZE),
        ("account:Observation", Observation::SIZE),
        ("account:ObserverLookup", ObserverLookup::SIZE),
    ];
    for (name, size) in checks {
        let h = hash(name.as_bytes());
        if &h.to_bytes()[..8] == disc {
            return Some(*size);
        }
    }
    None
}

pub fn admin_post_finalize_repair_account_handler(
    ctx: Context<AdminPostFinalizeRepairAccount>,
    seeds: Vec<Vec<u8>>,
    data: Vec<u8>,
) -> Result<()> {
    let seed_slices: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let (derived_pda, bump) = Pubkey::find_program_address(&seed_slices, ctx.program_id);
    require!(derived_pda == ctx.accounts.account.key(), GarError::InvalidPda);

    require!(data.len() >= 8, GarError::InvalidAccountData);
    let disc: [u8; 8] = data[..8].try_into().unwrap();
    let expected_size =
        recovery_known_discriminator(&disc).ok_or(error!(GarError::InvalidAccountData))?;
    require!(data.len() <= expected_size, GarError::InvalidAccountData);

    let account_info = ctx.accounts.account.to_account_info();
    require!(
        account_info.data_is_empty(),
        GarError::AccountAlreadyExists
    );

    let bump_bytes = [bump];
    let mut seeds_with_bump: Vec<&[u8]> = seed_slices;
    seeds_with_bump.push(&bump_bytes);

    let rent = Rent::get()?;
    let required = rent.minimum_balance(expected_size);

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

    let mut account_data = account_info.try_borrow_mut_data()?;
    let n = data.len().min(account_data.len());
    account_data[..n].copy_from_slice(&data[..n]);
    if n < account_data.len() {
        account_data[n..].fill(0);
    }

    msg!(
        "[recovery] repaired account {} ({} bytes payload, {} bytes allocated)",
        derived_pda,
        data.len(),
        expected_size
    );

    Ok(())
}

#[derive(Accounts)]
pub struct AdminPostFinalizeRepairAccount<'info> {
    // Multi-sig only (settings.authority), NOT migration_authority. No
    // migration_active constraint — that's the whole point.
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        constraint = settings.authority == authority.key() @ GarError::Unauthorized,
    )]
    pub settings: Account<'info, GatewaySettings>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Validated via PDA derivation in handler
    #[account(mut)]
    pub account: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
}
