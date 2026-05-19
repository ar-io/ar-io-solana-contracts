use anchor_lang::prelude::*;

use crate::error::AntError;
use crate::state::{
    AntConfig, AntControllers, AntRecord, AntRecordMetadata, SchemaVersion, ANT_CONFIG_VERSION,
    ANT_CONTROLLERS_VERSION, ANT_RECORD_METADATA_VERSION, ANT_RECORD_VERSION,
};

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
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}
