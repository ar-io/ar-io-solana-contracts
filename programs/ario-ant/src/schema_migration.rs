use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, system_instruction};

use crate::error::AntError;
use crate::state::{
    AclConfig, AclPage, AntConfig, AntControllers, AntMigrationConfig, AntRecord,
    AntRecordMetadata, SchemaVersion, ACL_CONFIG_VERSION, ACL_PAGE_VERSION, ANT_CONFIG_VERSION,
    ANT_CONTROLLERS_VERSION, ANT_MIGRATION_CONFIG_VERSION, ANT_RECORD_METADATA_VERSION,
    ANT_RECORD_VERSION,
};

/// Resize a program-owned PDA to the canonical SIZE for an in-place schema
/// migration (grow pre-versioning accounts; shrink over-allocated ones), then
/// deserialize. Grow-then-deserialize — `migrate_*` loads the target as
/// `UncheckedAccount` and calls this BEFORE deserializing, because Anchor
/// builds `Account<T>` from the NEW layout and a shorter old account would
/// EOF. See ario-core's `schema_migration::grow_account`.
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

/// Serialize `account` back into `info` without advancing the account's data
/// slice (temp buffer + index copy). See ario-core's `write_account`.
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

/// Walk an `AntConfig` account from its current version to `ANT_CONFIG_VERSION`.
///
/// The `while` loop applies one migration step per iteration so a single call
/// handles any gap — an account at 1.0.0 in a codebase that has advanced to
/// 1.2.0 runs through the 1.0.0→1.1.0 arm then the 1.1.0→1.2.0 arm
/// automatically.
///
/// Each match arm MUST advance `config.version` before returning so the loop
/// terminates. Add a new arm at the bottom when bumping `ANT_CONFIG_VERSION`.
#[allow(clippy::never_loop, clippy::while_immutable_condition)]
pub fn migrate_config_version(config: &mut AntConfig) -> Result<()> {
    while config.version < ANT_CONFIG_VERSION {
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
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `AntControllers` account from its current version to
/// `ANT_CONTROLLERS_VERSION`. Same sequential-dispatch pattern as
/// `migrate_config_version`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_controllers_version(controllers: &mut AntControllers) -> Result<()> {
    while controllers.version < ANT_CONTROLLERS_VERSION {
        match controllers.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                controllers.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `AntRecord` account from its current version to `ANT_RECORD_VERSION`.
/// Called once per undername record by `migrate_ant_record`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_record_version(record: &mut AntRecord) -> Result<()> {
    while record.version < ANT_RECORD_VERSION {
        match record.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                record.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `AntRecordMetadata` account from its current version to
/// `ANT_RECORD_METADATA_VERSION`. Called once per metadata PDA by
/// `migrate_ant_record_metadata`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_record_metadata_version(metadata: &mut AntRecordMetadata) -> Result<()> {
    while metadata.version < ANT_RECORD_METADATA_VERSION {
        match metadata.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                metadata.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `AntMigrationConfig` account from its current version to
/// `ANT_MIGRATION_CONFIG_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_migration_config_version(config: &mut AntMigrationConfig) -> Result<()> {
    while config.version < ANT_MIGRATION_CONFIG_VERSION {
        match config.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                config.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `AclConfig` account from its current version to `ACL_CONFIG_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_acl_config_version(config: &mut AclConfig) -> Result<()> {
    while config.version < ACL_CONFIG_VERSION {
        match config.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                config.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `AclPage` account from its current version to `ACL_PAGE_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_acl_page_version(page: &mut AclPage) -> Result<()> {
    while page.version < ACL_PAGE_VERSION {
        match page.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                page.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}
