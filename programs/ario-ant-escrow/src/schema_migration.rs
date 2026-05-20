use anchor_lang::prelude::*;

use crate::error::EscrowError;
use crate::state::{
    EscrowAnt, EscrowToken, SchemaVersion, ESCROW_ANT_VERSION, ESCROW_TOKEN_VERSION,
};

#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_escrow_ant_version(escrow: &mut EscrowAnt) -> Result<()> {
    while escrow.version < ESCROW_ANT_VERSION {
        match escrow.version {
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 0,
                patch: 0,
            } => {
                escrow.field_1 = 1000;
                escrow.version = SchemaVersion::new(1, 1, 0);
            }
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 1,
                patch: 0,
            } => {
                escrow.field_2 = 42;
                escrow.version = SchemaVersion::new(1, 2, 0);
            }
            #[cfg(feature = "migration-test")]
            SchemaVersion {
                major: 1,
                minor: 2,
                patch: 0,
            } => {
                escrow.field_3 = true;
                escrow.version = SchemaVersion::new(1, 3, 0);
            }
            _ => return err!(EscrowError::UnknownSchemaVersion),
        }
    }
    Ok(())
}

#[allow(
    clippy::never_loop,
    clippy::while_immutable_condition,
    clippy::match_single_binding
)]
pub fn migrate_escrow_token_version(escrow: &mut EscrowToken) -> Result<()> {
    while escrow.version < ESCROW_TOKEN_VERSION {
        match escrow.version {
            _ => return err!(EscrowError::UnknownSchemaVersion),
        }
    }
    Ok(())
}
