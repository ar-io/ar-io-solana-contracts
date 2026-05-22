use anchor_lang::prelude::*;

use crate::error::ArioError;
use crate::state::{
    ArioConfig, Balance, PrimaryName, PrimaryNameRequest, PrimaryNameReverse, SchemaVersion, Vault,
    VaultCounter, ARIO_CONFIG_VERSION, BALANCE_VERSION, PRIMARY_NAME_REQUEST_VERSION,
    PRIMARY_NAME_REVERSE_VERSION, PRIMARY_NAME_VERSION, VAULT_COUNTER_VERSION, VAULT_VERSION,
};

#[allow(clippy::never_loop, clippy::while_immutable_condition)]
pub fn migrate_config_version(config: &mut ArioConfig) -> Result<()> {
    while config.version < ARIO_CONFIG_VERSION {
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
            _ => return err!(ArioError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_balance_version(balance: &mut Balance) -> Result<()> {
    while balance.version < BALANCE_VERSION {
        match balance.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                balance.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArioError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_vault_counter_version(counter: &mut VaultCounter) -> Result<()> {
    while counter.version < VAULT_COUNTER_VERSION {
        match counter.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                counter.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArioError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_vault_version(vault: &mut Vault) -> Result<()> {
    while vault.version < VAULT_VERSION {
        match vault.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                vault.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArioError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_primary_name_request_version(request: &mut PrimaryNameRequest) -> Result<()> {
    while request.version < PRIMARY_NAME_REQUEST_VERSION {
        match request.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                request.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArioError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_primary_name_version(name: &mut PrimaryName) -> Result<()> {
    while name.version < PRIMARY_NAME_VERSION {
        match name.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                name.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArioError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_primary_name_reverse_version(reverse: &mut PrimaryNameReverse) -> Result<()> {
    while reverse.version < PRIMARY_NAME_REVERSE_VERSION {
        match reverse.version {
            // Bootstrap arm — see `migrate_config_version` for rationale.
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                reverse.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(ArioError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

