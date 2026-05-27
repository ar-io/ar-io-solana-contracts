use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, system_instruction};

use crate::error::ArnsError;
use crate::state::*;

/// Resize a program-owned PDA to the canonical SIZE for an in-place schema
/// migration (grow pre-versioning accounts; shrink ones over-allocated by a
/// past init bug), then deserialize. Grow-then-deserialize: Anchor builds
/// `Account<T>` from the NEW layout before `realloc` runs, so `migrate_*`
/// loads the target as `UncheckedAccount`, calls this first, then
/// deserializes. See ario-core's `schema_migration::grow_account`.
pub fn grow_account<'info>(
    info: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    new_size: usize,
) -> Result<()> {
    let current = info.data_len();
    if current == new_size {
        return Ok(());
    }
    if current < new_size {
        let rent = Rent::get()?;
        let needed = rent
            .minimum_balance(new_size)
            .saturating_sub(info.lamports());
        if needed > 0 {
            invoke(
                &system_instruction::transfer(payer.key, info.key, needed),
                &[payer.clone(), info.clone(), system_program.clone()],
            )?;
        }
        info.realloc(new_size, false)?;
        let mut data = info.try_borrow_mut_data()?;
        for byte in data[current..new_size].iter_mut() {
            *byte = 0;
        }
    } else {
        info.realloc(new_size, false)?;
    }
    Ok(())
}

/// Serialize `account` back into `info` without advancing the account's own
/// data slice (temp buffer + index copy). See ario-core's `write_account`.
pub fn write_account<'info, T: anchor_lang::AccountSerialize>(
    info: &AccountInfo<'info>,
    account: &T,
) -> Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    account.try_serialize(&mut buf)?;
    let mut data = info.try_borrow_mut_data()?;
    if buf.len() > data.len() {
        return Err(anchor_lang::error::ErrorCode::AccountDidNotSerialize.into());
    }
    data[..buf.len()].copy_from_slice(&buf);
    Ok(())
}

/// Walk an `ArnsConfig` account from its current version to `ARNS_CONFIG_VERSION`.
///
/// The `while` loop applies one migration step per iteration so a single call
/// handles any gap — an account at 1.0.0 in a codebase that has advanced to
/// 1.2.0 runs through the 1.0.0→1.1.0 arm then the 1.1.0→1.2.0 arm
/// automatically.
///
/// Each match arm MUST advance `version` before returning so the loop
/// terminates. Add a new arm at the bottom when bumping `ARNS_CONFIG_VERSION`.
#[allow(clippy::never_loop, clippy::while_immutable_condition)]
pub fn migrate_arns_config_version(config: &mut ArnsConfig) -> Result<()> {
    while config.version < ARNS_CONFIG_VERSION {
        match config.version {
            // Bootstrap arm: accounts created before PR #51/#53 introduced
            // versioning have version={0,0,0} after the realloc-zero. Stamp
            // them at the post-#53 baseline (1.0.0) — no data transformation,
            // just the version field becomes correct.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                config.version = SchemaVersion::new(1, 0, 0);
            }
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 0,
                patch: 0,
            } => {
                config.field_1 = 1000;
                config.version = SchemaVersion::new(1, 1, 0);
            }
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 1,
                patch: 0,
            } => {
                config.field_2 = 42;
                config.version = SchemaVersion::new(1, 2, 0);
            }
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 2,
                patch: 0,
            } => {
                config.field_3 = true;
                config.version = SchemaVersion::new(1, 3, 0);
            }
            _ => return err!(ArnsError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk a `DemandFactor` account from its current version to `DEMAND_FACTOR_VERSION`.
/// Same sequential-dispatch pattern as `migrate_arns_config_version`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_demand_factor_version(demand_factor: &mut DemandFactor) -> Result<()> {
    while demand_factor.version < DEMAND_FACTOR_VERSION {
        match demand_factor.version {
            // Bootstrap arm — see `migrate_arns_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                demand_factor.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArnsError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `ArnsRecord` account from its current version to `ARNS_RECORD_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_arns_record_version(record: &mut ArnsRecord) -> Result<()> {
    while record.version < ARNS_RECORD_VERSION {
        match record.version {
            // Bootstrap arm — see `migrate_arns_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                record.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArnsError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk a `ReturnedName` account from its current version to `RETURNED_NAME_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_returned_name_version(returned_name: &mut ReturnedName) -> Result<()> {
    while returned_name.version < RETURNED_NAME_VERSION {
        match returned_name.version {
            // Bootstrap arm — see `migrate_arns_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                returned_name.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArnsError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk a `ReservedName` account from its current version to `RESERVED_NAME_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_reserved_name_version(reserved_name: &mut ReservedName) -> Result<()> {
    while reserved_name.version < RESERVED_NAME_VERSION {
        match reserved_name.version {
            // Bootstrap arm — see `migrate_arns_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                reserved_name.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArnsError::UnknownSchemaVersion),
        }
    }
    Ok(())
}
