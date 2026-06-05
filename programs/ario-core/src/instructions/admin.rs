use anchor_lang::prelude::*;

use crate::error::ArioError;
use crate::migration::MIGRATION_DEADLINE;
use crate::state::*;
use crate::{
    UpdateConfigParams, CORE_CONFIG_FIELD_MAX_VAULT_DURATION, CORE_CONFIG_FIELD_MINT,
    CORE_CONFIG_FIELD_MIN_VAULT_DURATION, CORE_CONFIG_FIELD_NEW_AUTHORITY,
    CORE_CONFIG_FIELD_PRIMARY_NAME_REQUEST_EXPIRY, CORE_CONFIG_FIELD_TREASURY,
};

pub mod update_config {
    use super::*;

    pub fn handler(ctx: Context<UpdateConfig>, params: UpdateConfigParams) -> Result<()> {
        let config = &mut ctx.accounts.config;
        let admin = ctx.accounts.authority.key();
        let clock = Clock::get()?;

        // PR-5: emit one ConfigUpdatedEvent per mutated field. Consumers
        // branch on `field` (CORE_CONFIG_FIELD_*) rather than diffing the
        // params payload off-chain. Order matches `UpdateConfigParams`
        // declaration so the emit sequence is deterministic per field set.
        if let Some(min_duration) = params.min_vault_duration {
            config.min_vault_duration = min_duration;
            let mut new_value = [0u8; 32];
            // Underlying field is i64, but durations are non-negative; encode
            // the bit pattern as u64 little-endian for ABI consistency.
            new_value[..8].copy_from_slice(&(min_duration as u64).to_le_bytes());
            emit!(ConfigUpdatedEvent {
                admin,
                field: CORE_CONFIG_FIELD_MIN_VAULT_DURATION,
                new_value,
                timestamp: clock.unix_timestamp,
            });
        }
        if let Some(max_duration) = params.max_vault_duration {
            config.max_vault_duration = max_duration;
            let mut new_value = [0u8; 32];
            new_value[..8].copy_from_slice(&(max_duration as u64).to_le_bytes());
            emit!(ConfigUpdatedEvent {
                admin,
                field: CORE_CONFIG_FIELD_MAX_VAULT_DURATION,
                new_value,
                timestamp: clock.unix_timestamp,
            });
        }
        if let Some(expiry) = params.primary_name_request_expiry {
            config.primary_name_request_expiry = expiry;
            let mut new_value = [0u8; 32];
            new_value[..8].copy_from_slice(&(expiry as u64).to_le_bytes());
            emit!(ConfigUpdatedEvent {
                admin,
                field: CORE_CONFIG_FIELD_PRIMARY_NAME_REQUEST_EXPIRY,
                new_value,
                timestamp: clock.unix_timestamp,
            });
        }
        if let Some(new_authority) = params.new_authority {
            // ADR-026 null guard: reject the all-zero pubkey so a fat-fingered
            // renounce cannot brick admin into an unspendable address. Mirrors
            // the dedicated `transfer_authority`.
            require!(
                new_authority != Pubkey::default(),
                ArioError::InvalidParameter
            );
            config.authority = new_authority;
            let mut new_value = [0u8; 32];
            new_value.copy_from_slice(&new_authority.to_bytes());
            emit!(ConfigUpdatedEvent {
                admin,
                field: CORE_CONFIG_FIELD_NEW_AUTHORITY,
                new_value,
                timestamp: clock.unix_timestamp,
            });
        }

        // Cross-field validation after applying updates
        require!(
            config.min_vault_duration <= config.max_vault_duration,
            ArioError::InvalidParameter
        );
        require!(config.min_vault_duration >= 0, ArioError::InvalidParameter);
        require!(
            config.primary_name_request_expiry > 0,
            ArioError::InvalidParameter
        );
        require!(config.max_vault_duration > 0, ArioError::InvalidParameter);

        msg!("Config updated");
        Ok(())
    }
}

#[derive(Accounts)]
pub struct UpdateConfig<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.authority == authority.key() @ ArioError::Unauthorized,
    )]
    pub config: Account<'info, ArioConfig>,

    pub authority: Signer<'info>,
}

/// Dedicated single-step admin-authority rotation (ADR-026). Equivalent to
/// `update_config` with only `new_authority` set, but a named, single-purpose
/// instruction so all five programs share one `transfer_authority`. Also
/// rotates the admin gate that `ario-ant-escrow::admin_purge_unclaimed_ant`
/// reads cross-program from `ArioConfig.authority`.
pub mod transfer_authority {
    use super::*;

    pub fn handler(ctx: Context<TransferAuthority>, new_authority: Pubkey) -> Result<()> {
        require!(
            new_authority != Pubkey::default(),
            ArioError::InvalidParameter
        );

        let config = &mut ctx.accounts.config;
        let old_authority = config.authority;
        config.authority = new_authority;

        emit!(AuthorityTransferredEvent {
            old_authority,
            new_authority,
            timestamp: Clock::get()?.unix_timestamp,
        });

        msg!(
            "ArioConfig.authority {} → {} (admin rotation)",
            old_authority,
            new_authority
        );
        Ok(())
    }
}

/// Context for `transfer_authority`. `constraint = config.authority ==
/// authority.key()` binds the signer to the CURRENT admin authority — the
/// only load-bearing check. Mirrors `UpdateConfig`'s gate.
#[derive(Accounts)]
pub struct TransferAuthority<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        constraint = config.authority == authority.key() @ ArioError::Unauthorized,
    )]
    pub config: Account<'info, ArioConfig>,

    pub authority: Signer<'info>,
}

pub mod admin_repair_config {
    use super::*;

    /// Recovery instruction for ArioConfig.mint / .treasury when devnet
    /// state corruption requires re-pointing without a full re-init.
    ///
    /// Background: if `devnet-setup.ts` is interrupted between
    /// `initializeCore` and `initializeArns` and re-run with a fresh mint,
    /// `ArioConfig` and `ArnsConfig` end up pointing at different mints —
    /// and there is no path to fix that via `update_config` (which can
    /// only mutate vault durations / primary-name expiry / authority).
    /// The repair flow is:
    ///   1. Devnet-setup.ts pre-flight refuses to overwrite an existing
    ///      ArioConfig (added in fix/ario-config-set-treasury-mint).
    ///   2. If a mismatch already exists on chain, this instruction lets
    ///      the protocol authority point ArioConfig at the canonical
    ///      mint+treasury without redeploying.
    ///
    /// Both fields are mutated atomically. The constraint that
    /// migration_active must still be true means this is a pre-cutover
    /// repair tool only — once `finalize_migration` runs, the configuration
    /// is locked.
    ///
    /// Emits two `ConfigUpdatedEvent`s (one per field) for indexer
    /// observability.
    pub fn handler(
        ctx: Context<AdminRepairConfig>,
        new_mint: Pubkey,
        new_treasury: Pubkey,
    ) -> Result<()> {
        let clock = Clock::get()?;
        // Repairs are only valid pre-cutover. Pin the deadline AND the
        // migration_active flag — the flag is the watershed (`finalize_migration`
        // disables it permanently); the deadline is a defense-in-depth
        // backstop in case `finalize_migration` is delayed.
        require!(
            clock.unix_timestamp < MIGRATION_DEADLINE,
            ArioError::MigrationExpired
        );

        let config = &mut ctx.accounts.config;
        let admin = ctx.accounts.authority.key();

        config.mint = new_mint;
        config.treasury = new_treasury;

        let mut mint_value = [0u8; 32];
        mint_value.copy_from_slice(&new_mint.to_bytes());
        emit!(ConfigUpdatedEvent {
            admin,
            field: CORE_CONFIG_FIELD_MINT,
            new_value: mint_value,
            timestamp: clock.unix_timestamp,
        });

        let mut treasury_value = [0u8; 32];
        treasury_value.copy_from_slice(&new_treasury.to_bytes());
        emit!(ConfigUpdatedEvent {
            admin,
            field: CORE_CONFIG_FIELD_TREASURY,
            new_value: treasury_value,
            timestamp: clock.unix_timestamp,
        });

        msg!(
            "ArioConfig repaired: mint={}, treasury={}",
            new_mint,
            new_treasury
        );
        Ok(())
    }
}

#[derive(Accounts)]
pub struct AdminRepairConfig<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
        has_one = authority @ ArioError::Unauthorized,
        constraint = config.migration_active @ ArioError::MigrationInactive,
    )]
    pub config: Account<'info, ArioConfig>,

    pub authority: Signer<'info>,
}

pub mod admin_set_gar_program {
    use super::*;
    use anchor_lang::system_program;
    use anchor_lang::Discriminator;

    /// Migration ix for pre-`gar_program` ArioConfig deployments.
    /// Grows the ArioConfig PDA by 32 bytes (the size of the new
    /// `gar_program` field, appended after `bump`) and writes the
    /// supplied GAR program ID.
    ///
    /// `config` is an `UncheckedAccount` (not `Account<ArioConfig>`)
    /// because Anchor's account-load runs *before* `realloc` and
    /// would fail with `AccountDidNotDeserialize` on the pre-realloc
    /// 226-byte account (the new 258-byte struct hits EOF reading the
    /// appended `gar_program` field). Validation is therefore done
    /// manually: owner, discriminator, PDA derivation, stored
    /// authority, and `migration_active` flag.
    ///
    /// Idempotent: if the account is already at the target size,
    /// realloc is skipped and the handler just rewrites `gar_program`.
    pub fn handler(ctx: Context<AdminSetGarProgram>, new_gar_program: Pubkey) -> Result<()> {
        let clock = Clock::get()?;
        require!(
            clock.unix_timestamp < MIGRATION_DEADLINE,
            ArioError::MigrationExpired
        );

        let config_ai = &ctx.accounts.config;
        let authority = &ctx.accounts.authority;
        let system_program_ai = &ctx.accounts.system_program;

        require_keys_eq!(*config_ai.owner, crate::ID, ArioError::Unauthorized);

        let current_size = config_ai.data_len();
        require!(current_size >= 226, ArioError::Unauthorized);

        {
            let data = config_ai.try_borrow_data()?;
            require!(
                &data[..8] == ArioConfig::DISCRIMINATOR,
                ArioError::Unauthorized
            );

            // Stored authority is the first field after the discriminator.
            let stored_authority =
                Pubkey::try_from(&data[8..40]).map_err(|_| error!(ArioError::Unauthorized))?;
            require_keys_eq!(stored_authority, authority.key(), ArioError::Unauthorized);

            // `migration_active` lives at payload offset 184 (= data offset 192).
            // Layout: authority(32)+mint(32)+arns_program(32)+treasury(32)
            //   + 4*u64(32) + 3*i64(24) = 184 bytes before this field.
            require!(data[192] == 1, ArioError::MigrationInactive);
        }

        let target_size = ArioConfig::SIZE;
        if current_size < target_size {
            let rent = Rent::get()?;
            let required = rent.minimum_balance(target_size);
            let cur_lamports = config_ai.lamports();
            if required > cur_lamports {
                let diff = required - cur_lamports;
                let cpi_accounts = system_program::Transfer {
                    from: authority.to_account_info(),
                    to: config_ai.to_account_info(),
                };
                let cpi_ctx = CpiContext::new(system_program_ai.to_account_info(), cpi_accounts);
                system_program::transfer(cpi_ctx, diff)?;
            }
            config_ai.realloc(target_size, false)?;
            let mut data = config_ai.try_borrow_mut_data()?;
            for byte in &mut data[current_size..target_size] {
                *byte = 0;
            }
        }

        {
            // CRITICAL: write at the canonical offset of `gar_program`,
            // not `target_size - 32`. Post-PR #53, `SIZE - 32` overlaps
            // the trailing `version: SchemaVersion` field (3 bytes),
            // which would clobber both fields on every call — see
            // ArioConfig::GAR_PROGRAM_OFFSET docstring + the
            // static_assert that keeps the constant in sync.
            let mut data = config_ai.try_borrow_mut_data()?;
            let offset = ArioConfig::GAR_PROGRAM_OFFSET;
            data[offset..offset + 32].copy_from_slice(&new_gar_program.to_bytes());
        }

        msg!("ArioConfig.gar_program set: {}", new_gar_program);
        Ok(())
    }
}

#[derive(Accounts)]
pub struct AdminSetGarProgram<'info> {
    /// CHECK: manually validated in handler — owner, discriminator,
    /// stored authority, and migration_active are all checked before
    /// any mutation. Cannot use `Account<'info, ArioConfig>` because
    /// Anchor account-load runs before `realloc` and would reject the
    /// pre-realloc 226-byte layout.
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump,
    )]
    pub config: UncheckedAccount<'info>,

    #[account(mut)]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}
