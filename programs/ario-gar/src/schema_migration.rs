use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, system_instruction};

use crate::error::GarError;
use crate::state::{
    AllowlistEntry, Delegation, EpochSettings, Gateway, GatewaySettings, Observation,
    ObserverLookup, RedelegationRecord, SchemaVersion, Withdrawal, WithdrawalCounter,
    ALLOWLIST_ENTRY_VERSION, DELEGATION_VERSION, EPOCH_SETTINGS_VERSION, GATEWAY_SETTINGS_VERSION,
    GATEWAY_VERSION, OBSERVATION_VERSION, OBSERVER_LOOKUP_VERSION, REDELEGATION_RECORD_VERSION,
    WITHDRAWAL_COUNTER_VERSION, WITHDRAWAL_VERSION,
};

/// Grow a program-owned PDA to `new_size` for an in-place schema migration,
/// top up rent from `payer`, then zero the appended tail. See ario-core's
/// `schema_migration::grow_account` for the full rationale (Anchor builds
/// `Account<T>` by deserializing the NEW layout before `realloc` runs, so a
/// pre-versioning shorter account would EOF — `migrate_*` loads the target as
/// `UncheckedAccount`, grows first via this helper, then deserializes).
/// Idempotent: no-op when already at/above `new_size`. Mirrors the
/// `MigrateSettingsSetArnsProgramId` UncheckedAccount pattern in this module.
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
        // Grow: top up rent, realloc, zero the appended tail (new `version`
        // reads {0,0,0}).
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
        // Shrink: trim an account over-allocated by a past init bug to the
        // canonical SIZE (meaningful fields incl. `version` are in the first
        // SIZE bytes). Excess rent stays; still rent-exempt.
        info.realloc(new_size, false)?;
    }
    Ok(())
}

/// Serialize `account` back into `info` WITHOUT advancing the account's own
/// data slice (writing a typed value through `&mut *data` truncates the
/// account — `Write for &mut [u8]` advances the slice ref). Temp buffer +
/// index copy. See ario-core's `schema_migration::write_account`.
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
///
/// NOTE: there is intentionally **no 1.0.0 → 1.1.0 arm**. The 1.1.0 bump added
/// fields to `GatewaySettings2`, which is embedded MID-struct inside `Gateway`
/// (before `registry_index`, `observer_address`, `cumulative_reward_per_token`,
/// `bump`, `version`). The grow-then-deserialize pattern this module uses only
/// works for fields appended at the byte-end (so the zeroed tail reads as the
/// new trailing field); a mid-struct insertion shifts every subsequent field
/// and would be misread. A correct in-place migration would require reading the
/// old layout via a shadow struct. Since 1.1.0 ships pre-mainnet with a full
/// devnet/staging redeploy, pre-1.1.0 gateways are **recreated, not migrated** —
/// so `migrate_gateway` deliberately refuses 1.0.0 accounts (the `_` arm). If a
/// future field is appended at the byte-end (after `version` moves), restore the
/// normal arm pattern here.
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
