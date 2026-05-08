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
