use anchor_lang::prelude::*;

use crate::error::GarError;
use crate::state::{
    AllowlistEntry, Delegation, EpochSettings, Gateway, GatewaySettings, Observation,
    ObserverLookup, RedelegationRecord, SchemaVersion, Withdrawal, WithdrawalCounter,
    ALLOWLIST_ENTRY_VERSION, DELEGATION_VERSION, EPOCH_SETTINGS_VERSION, GATEWAY_SETTINGS_VERSION,
    GATEWAY_VERSION, OBSERVATION_VERSION, OBSERVER_LOOKUP_VERSION, REDELEGATION_RECORD_VERSION,
    WITHDRAWAL_COUNTER_VERSION, WITHDRAWAL_VERSION,
};

/// Walk a `GatewaySettings` account from its current version to
/// `GATEWAY_SETTINGS_VERSION`. Each match arm MUST advance
/// `account.version` before returning so the loop terminates.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_gateway_settings_version(account: &mut GatewaySettings) -> Result<()> {
    while account.version < GATEWAY_SETTINGS_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 0,
                patch: 0,
            } => {
                account.field_1 = 1000;
                account.version = SchemaVersion::new(1, 1, 0);
            }
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 1,
                patch: 0,
            } => {
                account.field_2 = 42;
                account.version = SchemaVersion::new(1, 2, 0);
            }
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 2,
                patch: 0,
            } => {
                account.field_3 = true;
                account.version = SchemaVersion::new(1, 3, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk a `Gateway` account from its current version to `GATEWAY_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_gateway_version(account: &mut Gateway) -> Result<()> {
    while account.version < GATEWAY_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk a `Delegation` account from its current version to `DELEGATION_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_delegation_version(account: &mut Delegation) -> Result<()> {
    while account.version < DELEGATION_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk a `WithdrawalCounter` account from its current version to
/// `WITHDRAWAL_COUNTER_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_withdrawal_counter_version(account: &mut WithdrawalCounter) -> Result<()> {
    while account.version < WITHDRAWAL_COUNTER_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk a `Withdrawal` account from its current version to `WITHDRAWAL_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_withdrawal_version(account: &mut Withdrawal) -> Result<()> {
    while account.version < WITHDRAWAL_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `AllowlistEntry` account from its current version to
/// `ALLOWLIST_ENTRY_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_allowlist_entry_version(account: &mut AllowlistEntry) -> Result<()> {
    while account.version < ALLOWLIST_ENTRY_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `ObserverLookup` account from its current version to
/// `OBSERVER_LOOKUP_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_observer_lookup_version(account: &mut ObserverLookup) -> Result<()> {
    while account.version < OBSERVER_LOOKUP_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk a `RedelegationRecord` account from its current version to
/// `REDELEGATION_RECORD_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_redelegation_record_version(account: &mut RedelegationRecord) -> Result<()> {
    while account.version < REDELEGATION_RECORD_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `EpochSettings` account from its current version to
/// `EPOCH_SETTINGS_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_epoch_settings_version(account: &mut EpochSettings) -> Result<()> {
    while account.version < EPOCH_SETTINGS_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

/// Walk an `Observation` account from its current version to
/// `OBSERVATION_VERSION`.
#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_observation_version(account: &mut Observation) -> Result<()> {
    while account.version < OBSERVATION_VERSION {
        match account.version {
            SchemaVersion {
                major: 0,
                minor: 0,
                patch: 0,
            } => {
                account.version = SchemaVersion::new(1, 0, 0);
            }
            _ => return err!(GarError::UnknownSchemaVersion),
        }
    }
    Ok(())
}
