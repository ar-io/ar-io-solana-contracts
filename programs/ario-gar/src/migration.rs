use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    hash::hash,
    program::{invoke, invoke_signed},
    system_instruction,
};

use crate::error::GarError;
use crate::state::*;
use crate::GarMigrationFinalizedEvent;

/// Migration deadline: imports are rejected after this timestamp.
// Migration deadline: 2112-04-20 00:01:09 UTC -- INTENTIONALLY far-future: this time-based backstop
// is effectively disabled; finalize_migration is the sole active control on the migration-authority
// write window. MUST be tightened to the real migration cutoff before mainnet (until then this
// re-opens audit MIGRATION-001: an unbounded migration window if the authority key is compromised).
pub const MIGRATION_DEADLINE: i64 = 4490553669;

// =========================================
// DISCRIMINATOR VALIDATION
// =========================================

/// Check if discriminator matches a known ario-gar account type and return expected SIZE.
fn known_discriminator(disc: &[u8; 8]) -> Option<usize> {
    // EpochSettings intentionally excluded (audit M-4, 2026-05-29) —
    // it's a singleton with its own rotatable `authority: Pubkey` field
    // (state/mod.rs:758). Allowing import_account to overwrite it would
    // let a compromised `migration_authority` rotate `EpochSettings.authority`
    // to a key it controls, and that rotation SURVIVES
    // `finalize_migration` (the migration deadline only gates this ix,
    // not subsequent `update_epoch_settings` calls authorized by the
    // rotated key). Mirrors the ARNS pattern: `ArnsConfig` /
    // `DemandFactor` are similarly excluded from ario-arns's import
    // allowlist with the same rationale (see ario-arns/migration.rs:22-25).
    //
    // GatewaySettings (also has an `authority: Pubkey` field, state/mod.rs:133)
    // is already excluded by being absent from this list entirely.
    let checks: &[(&str, usize)] = &[
        ("account:Gateway", Gateway::SIZE),
        ("account:Delegation", Delegation::SIZE),
        ("account:Withdrawal", Withdrawal::SIZE),
        ("account:WithdrawalCounter", WithdrawalCounter::SIZE),
        ("account:AllowlistEntry", AllowlistEntry::SIZE),
        ("account:RedelegationRecord", RedelegationRecord::SIZE),
        ("account:Observation", Observation::SIZE),
        ("account:ObserverLookup", ObserverLookup::SIZE),
    ];
    for (name, size) in checks {
        let h = hash(name.as_bytes());
        if &h.to_bytes()[..8] == disc {
            return Some(*size);
        }
    }
    // Zero-copy accounts use a different discriminator format
    let zc_checks: &[(&str, usize)] = &[("account:Epoch", Epoch::SIZE)];
    for (name, size) in zc_checks {
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
        GarError::MigrationExpired
    );

    // Derive PDA from seeds
    let seed_slices: Vec<&[u8]> = seeds.iter().map(|s| s.as_slice()).collect();
    let (derived_pda, bump) = Pubkey::find_program_address(&seed_slices, ctx.program_id);

    require!(
        derived_pda == ctx.accounts.account.key(),
        GarError::InvalidPda
    );

    // Validate discriminator
    require!(data.len() >= 8, GarError::InvalidAccountData);
    let disc: [u8; 8] = data[..8].try_into().unwrap();
    let expected_size = known_discriminator(&disc).ok_or(error!(GarError::InvalidAccountData))?;

    // Variable-length Anchor borsh (snapshot) must fit in max account size; the
    // encoded payload is always <= the precomputed Anchor `SIZE` constant because
    // strings/options use compact borsh encoding. Allocate the full max so future
    // mutating instructions don't need realloc.
    require!(data.len() <= expected_size, GarError::InvalidAccountData);

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
            GarError::InvalidAccountData
        );
        require!(
            account_info.data_len() == expected_size,
            GarError::InvalidAccountData
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
    entry_pubkey: Pubkey,
    weight: u64,
    start_timestamp: i64,
) -> Result<()> {
    // Check migration deadline
    let clock = Clock::get()?;
    require!(
        clock.unix_timestamp < MIGRATION_DEADLINE,
        GarError::MigrationExpired
    );

    let mut registry = ctx.accounts.registry.load_mut()?;

    require!(
        (registry.count as usize) < GatewayRegistry::MAX_GATEWAYS,
        GarError::InvalidAccountData
    );

    let idx = registry.count as usize;
    // Imported gateways always start as Joined — AO has no "Leaving" concept
    // in snapshots; if a gateway is leaving on AO, it's simply not exported.
    registry.gateways[idx] = GatewaySlot {
        address: entry_pubkey,
        composite_weight: weight,
        start_timestamp,
        status: GatewaySlot::STATUS_JOINED,
        // Set correctly at the first live tally_weights; imported gateways
        // aren't mid-epoch at migration time.
        delegated_at_tally: 0,
        _padding: [0; 6],
    };
    registry.count += 1;

    Ok(())
}

pub fn finalize_migration_handler(ctx: Context<FinalizeMigration>) -> Result<()> {
    let admin = ctx.accounts.authority.key();
    let settings = &mut ctx.accounts.settings;
    settings.migration_active = false;

    // Optional registry as remaining_accounts[0]: lets indexers stamp the
    // event with the count at finalize time without breaking the existing
    // 2-account wire layout (the migration import tool already builds with
    // [settings, authority] only). When omitted, `gateway_count` falls back
    // to 0; PDA-validated when present.
    let mut gateway_count: u32 = 0;
    if let Some(reg_info) = ctx.remaining_accounts.first() {
        let (expected_pda, _) = Pubkey::find_program_address(&[REGISTRY_SEED], ctx.program_id);
        if reg_info.key() == expected_pda && reg_info.owner == ctx.program_id {
            // GatewayRegistry zero-copy layout: 8 (disc) + 32 (authority) + 4 (count)
            let data = reg_info.try_borrow_data()?;
            if data.len() >= 8 + 32 + 4 {
                gateway_count = u32::from_le_bytes(data[8 + 32..8 + 32 + 4].try_into().unwrap());
            }
        }
    }

    let clock = Clock::get()?;
    let slot = clock.slot;

    emit!(GarMigrationFinalizedEvent {
        admin,
        gateway_count,
        slot,
        timestamp: clock.unix_timestamp,
    });

    msg!("Migration finalized — import instructions permanently disabled");
    Ok(())
}

/// One-shot migration to backfill `GatewaySettings::arns_program_id` on
/// pre-existing accounts that were created before that field existed.
///
/// Old layout: …protocol_token_account(32) + bump(1)        = ends at offset N
/// New layout: …protocol_token_account(32) + arns_program_id(32) + bump(1)
///
/// Steps:
///   1. realloc the account from old SIZE to current `GatewaySettings::SIZE`
///      (idempotent — no-op if already at the new size)
///   2. preserve the old bump byte and reposition it after the new field
///   3. write `arns_program_id` into its new offset
///
/// Auth: settings.authority — same authority that ran `initialize`. The
/// migration_active gate is intentionally NOT checked here because the
/// purpose of this instruction is to repair settings deployed before the
/// field existed, which may include settings whose migration window has
/// already closed (devnet/test deployments). The realloc + write is
/// strictly additive — no existing field is mutated.
pub fn migrate_settings_set_arns_program_id_handler(
    ctx: Context<MigrateSettingsSetArnsProgramId>,
    arns_program_id: Pubkey,
) -> Result<()> {
    let info = ctx.accounts.settings.to_account_info();
    let new_size = GatewaySettings::SIZE;
    // pre-refactor size: one Pubkey shorter (no arns_program_id), no supply
    // counters (3×u64 = 24 bytes), and no schema version field (3 bytes).
    let old_size = new_size - 32 - 24 - SCHEMA_VERSION_SIZE;

    let current_size = info.data_len();
    if current_size == new_size {
        // Already migrated. Idempotent return so re-runs are safe; the field
        // value is left unchanged (callers must use a separate setter to
        // rotate the program ID, which is intentionally not provided — this
        // is a one-time backfill, not a generic admin tool).
        msg!("GatewaySettings already at new size; backfill skipped");
        return Ok(());
    }
    require!(current_size == old_size, GarError::InvalidAccountData);

    // Top up rent to cover the larger account before realloc.
    let rent = Rent::get()?;
    let needed = rent
        .minimum_balance(new_size)
        .saturating_sub(info.lamports());
    if needed > 0 {
        let payer = ctx.accounts.payer.to_account_info();
        let system = ctx.accounts.system_program.to_account_info();
        anchor_lang::solana_program::program::invoke(
            &system_instruction::transfer(payer.key, info.key, needed),
            &[payer, info.clone(), system],
        )?;
    }
    info.realloc(new_size, false)?;

    // Now reposition fields:
    //   old layout (offsets within the data buffer):
    //     [0..N-1]              ... up through protocol_token_account (last field before bump)
    //     [N-1]                 bump (1 byte)
    //   new layout (after realloc, account is N+32 bytes):
    //     [0..N-1]              ... unchanged
    //     [N-1 .. N+31]         arns_program_id (32 bytes) ← NEW
    //     [N+31]                bump (1 byte)
    let mut data = info.try_borrow_mut_data()?;
    let bump_byte = data[old_size - 1];
    // Write the new arns_program_id at the slot previously occupied by `bump`
    // and extending into the freshly-realloc'd zero-filled tail.
    data[old_size - 1..old_size - 1 + 32].copy_from_slice(arns_program_id.as_ref());
    // Repaint the bump in its new location (before the trailing version field).
    // Layout tail: ... | bump (1) | version (3) |
    data[new_size - 1 - SCHEMA_VERSION_SIZE] = bump_byte;

    msg!(
        "GatewaySettings backfilled: arns_program_id = {}",
        arns_program_id
    );
    Ok(())
}

/// One-shot backfill for the supply counter fields added to GatewaySettings.
/// Called once after migration import to set counters to the correct totals
/// computed from the imported gateways, delegations, and withdrawals.
/// Authority-gated: only the settings.authority can call this.
pub fn migrate_settings_supply_counters_handler(
    ctx: Context<MigrateSettingsSupplyCounters>,
    total_staked: u64,
    total_delegated: u64,
    total_withdrawn: u64,
) -> Result<()> {
    let settings = &mut ctx.accounts.settings;
    settings.total_staked = total_staked;
    settings.total_delegated = total_delegated;
    settings.total_withdrawn = total_withdrawn;

    msg!(
        "Supply counters backfilled: staked={}, delegated={}, withdrawn={}",
        total_staked,
        total_delegated,
        total_withdrawn,
    );
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct ImportAccount<'info> {
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        constraint = settings.migration_active @ GarError::MigrationInactive,
        constraint = settings.migration_authority == authority.key() @ GarError::Unauthorized,
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

#[derive(Accounts)]
pub struct ImportRegistryEntry<'info> {
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        constraint = settings.migration_active @ GarError::MigrationInactive,
        constraint = settings.migration_authority == authority.key() @ GarError::Unauthorized,
    )]
    pub settings: Account<'info, GatewaySettings>,
    pub authority: Signer<'info>,
    #[account(mut)]
    pub registry: AccountLoader<'info, GatewayRegistry>,
}

#[derive(Accounts)]
pub struct FinalizeMigration<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        constraint = settings.migration_active @ GarError::MigrationAlreadyFinalized,
        constraint = settings.authority == authority.key() @ GarError::Unauthorized,
    )]
    pub settings: Account<'info, GatewaySettings>,
    pub authority: Signer<'info>,
}

/// One-shot migration to backfill `arns_program_id` on GatewaySettings
/// accounts deployed before that field existed. Uses `UncheckedAccount`
/// because the account may be at the old (smaller) size, which Anchor
/// would reject when trying to deserialize it as `Account<GatewaySettings>`.
#[derive(Accounts)]
pub struct MigrateSettingsSetArnsProgramId<'info> {
    /// CHECK: validated manually via PDA seeds + authority byte offset.
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump,
    )]
    pub settings: UncheckedAccount<'info>,

    /// Authority signer — validated against settings.authority (offset 8, 32 bytes).
    /// The handler reads the authority pubkey directly from the account bytes
    /// so we can authenticate without deserializing via Anchor (the old layout
    /// would fail that deserialization).
    #[account(
        constraint = {
            let data = settings.try_borrow_data()?;
            require!(data.len() >= 40, GarError::InvalidAccountData);
            let stored_authority: Pubkey = Pubkey::try_from(&data[8..40])
                .map_err(|_| error!(GarError::InvalidAccountData))?;
            stored_authority == authority.key()
        } @ GarError::Unauthorized,
    )]
    pub authority: Signer<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

/// Set supply counters on GatewaySettings after migration import.
///
/// Authority-gated: only `settings.authority` can call. ALSO gated on
/// `settings.migration_active` so the handler can ONLY backfill during
/// the migration window. This differs intentionally from
/// `migrate_settings_set_arns_program_id`, which is allowed to run
/// post-finalize because it writes a one-shot immutable field. Supply
/// counters change continuously after launch — letting this handler
/// run post-finalize would let a compromised authority key overwrite
/// live counter state with arbitrary numbers (cosmetic corruption of
/// `getTokenSupply()` reads, not value loss, but a low-effort lie
/// surface). After `finalize_migration`, the only way counters change
/// is via the legitimate state-mutating ix that maintain them
/// atomically.
#[derive(Accounts)]
pub struct MigrateSettingsSupplyCounters<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        has_one = authority @ GarError::Unauthorized,
        constraint = settings.migration_active @ GarError::MigrationInactive,
    )]
    pub settings: Account<'info, GatewaySettings>,
    pub authority: Signer<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Audit M-4 (2026-05-29): EpochSettings must NOT be importable.
    /// Pins the allowlist composition so a future PR can't silently
    /// re-add it. The discriminator is computed exactly the same way
    /// `known_discriminator` itself computes it, so this also catches
    /// any accidental rename of the account type.
    #[test]
    fn epoch_settings_excluded_from_import_allowlist() {
        let h = hash(b"account:EpochSettings");
        let disc: [u8; 8] = h.to_bytes()[..8].try_into().unwrap();
        assert!(
            known_discriminator(&disc).is_none(),
            "EpochSettings was re-added to the import allowlist — audit M-4 \
             explicitly excluded it because its `authority` field can be \
             rotated post-finalize via a re-imported overwrite. Remove it \
             or document the new mitigation."
        );
    }

    /// Belt-and-braces: GatewaySettings (also has `authority: Pubkey`)
    /// must remain excluded too. It's been excluded forever but a future
    /// PR could mistakenly include it for "completeness".
    #[test]
    fn gateway_settings_excluded_from_import_allowlist() {
        let h = hash(b"account:GatewaySettings");
        let disc: [u8; 8] = h.to_bytes()[..8].try_into().unwrap();
        assert!(
            known_discriminator(&disc).is_none(),
            "GatewaySettings has an `authority` field and is intentionally \
             excluded from migration import. If you're re-adding it, document \
             the new mitigation against authority-hijack via re-import."
        );
    }

    /// Spot-check that the rest of the allowlist still resolves —
    /// ensures the M-4 exclusion edit didn't accidentally break the
    /// legitimate types' discriminator lookup.
    #[test]
    fn allowlist_smoke_check_includes_gateway() {
        let h = hash(b"account:Gateway");
        let disc: [u8; 8] = h.to_bytes()[..8].try_into().unwrap();
        assert_eq!(
            known_discriminator(&disc),
            Some(Gateway::SIZE),
            "Gateway must still resolve through known_discriminator"
        );
    }
}
