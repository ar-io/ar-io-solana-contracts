use anchor_lang::prelude::*;
use anchor_lang::solana_program::{program::invoke, system_instruction};

use crate::error::ArioError;
use crate::state::{
    ArioConfig, Balance, PrimaryName, PrimaryNameRequest, PrimaryNameReverse, SchemaVersion, Vault,
    VaultCounter, ARIO_CONFIG_VERSION, BALANCE_VERSION, PRIMARY_NAME_REQUEST_VERSION,
    PRIMARY_NAME_REVERSE_VERSION, PRIMARY_NAME_VERSION, VAULT_COUNTER_VERSION, VAULT_VERSION,
};

/// Grow a program-owned PDA to `new_size` for an in-place schema migration,
/// topping up rent from `payer`, then zero-fill the newly-appended tail.
///
/// **Why this exists (the schema-migration grow-then-deserialize pattern):**
/// Anchor builds `Account<'info, T>` by borsh-deserializing the *new* layout
/// BEFORE any `realloc` constraint runs. An account created under an older
/// (shorter) layout therefore hits Borsh EOF and the instruction fails before
/// it can grow the account — so `migrate_*` must NOT load the target as a
/// typed `Account<T>`. Instead it loads it as an `UncheckedAccount`, calls
/// this helper to grow first, and only THEN deserializes. Because versioned
/// fields are appended at the byte-end and the appended tail is zeroed here,
/// the `version` field reads as {0,0,0} and the bootstrap arm in
/// `migrate_*_version` stamps the baseline.
///
/// Idempotent: a no-op when the account is already at/above `new_size`
/// (callers then hit the `version < LATEST` AlreadyLatestVersion guard).
/// Mirrors the proven `ario-gar::migration` UncheckedAccount pattern.
pub fn grow_account<'info>(
    info: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    new_size: usize,
) -> Result<()> {
    let current = info.data_len();
    if current >= new_size {
        return Ok(());
    }
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
    // Deterministically zero the appended region (don't rely on runtime
    // grow-zeroing) so the new trailing `version` field is {0,0,0}.
    let mut data = info.try_borrow_mut_data()?;
    for byte in data[current..new_size].iter_mut() {
        *byte = 0;
    }
    Ok(())
}

/// Serialize `account` (8-byte discriminator + borsh body) back into `info`'s
/// data buffer **without advancing the account's own data slice**.
///
/// The obvious `account.try_serialize(&mut *info.try_borrow_mut_data()?)`
/// is a trap: `Write for &mut [u8]` advances the slice reference as it
/// writes, and because that reference IS the account's data ref (shared via
/// the `RefCell`), it leaves the AccountInfo pointing at the empty tail —
/// truncating the account to 0 bytes. Anchor's own `exit` dodges this with a
/// non-advancing `BpfWriter`; here we serialize into a temp buffer and
/// copy by index. Account length is unchanged.
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
