//! Metaplex Core `TransferV1` and `UpdateV1` CPI helpers (hand-rolled,
//! no `mpl-core` crate).
//!
//! Why hand-rolled: same reason as `programs/ario-arns/src/mpl_core_cpi.rs`
//! — Solana 2.1.0's bundled Cargo 1.79 can't parse newer mpl-core crate
//! manifests (edition2024). Encoding the wire format ourselves keeps the
//! dep graph clean and the call site obvious.
//!
//! Wire format reference (from kinobi-generated `@metaplex-foundation/mpl-core`,
//! cross-checked against the official rust client at
//! `clients/rust/src/generated/instructions/transfer_v1.rs`):
//!
//! TransferV1 instruction data:
//!   - discriminator: u8 = 14
//!   - compression_proof: Option<CompressionProof>
//!     For uncompressed assets (everything we mint), this is always `None`,
//!     which Borsh encodes as a single 0x00 byte.
//!
//! TransferV1 account order (from mpl-core's `TransferV1` accounts struct):
//!   0. asset           writable
//!   1. collection      optional, writable (None → mpl_core program id)
//!   2. payer           writable, signer (rent payer)
//!   3. authority       optional, signer (current owner; None → payer is authority)
//!   4. new_owner       readonly (no signature; just the destination)
//!   5. system_program  optional readonly (None → mpl_core program id)
//!   6. log_wrapper     optional readonly (None → mpl_core program id)
//!
//! The "use mpl_core program id as None placeholder" convention matches the
//! existing `update_attributes_plugin` helper in `ario-arns`.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::{invoke, invoke_signed},
};

use crate::state::MPL_CORE_PROGRAM_ID;

/// MPL Core TransferV1 instruction discriminator (single byte, kinobi-generated).
pub const TRANSFER_V1_DISCRIMINATOR: u8 = 14;

/// Build the raw instruction data for TransferV1.
///
/// `compression_proof` is always `None` for the uncompressed assets ANTs
/// use; this constant-folds the encoding to two bytes (`0x0E 0x00`).
pub fn encode_transfer_v1_ix_data() -> Vec<u8> {
    vec![
        TRANSFER_V1_DISCRIMINATOR,
        0u8, // Option<CompressionProof> = None
    ]
}

/// CPI into MPL Core `TransferV1` for an asset whose current owner is a
/// PDA of the *caller* program — i.e. the escrow PDA signs to release the
/// ANT to a claimant or back to the depositor.
///
/// `signer_seeds` must be the full seed slice including the bump, exactly
/// as you'd pass to `invoke_signed`. The caller is responsible for:
/// - validating the asset's current owner is the escrow PDA before calling
/// - ensuring the asset/collection/system_program/log_wrapper account
///   handles map to the correct underlying accounts
pub fn transfer_asset_signed_by_pda<'info>(
    asset: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    authority_pda: &AccountInfo<'info>,
    new_owner: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
    signer_seeds: &[&[u8]],
) -> Result<()> {
    let metas = transfer_v1_metas(
        asset.key(),
        payer.key(),
        authority_pda.key(),
        new_owner.key(),
        system_program.key(),
    );

    let ix = Instruction {
        program_id: MPL_CORE_PROGRAM_ID,
        accounts: metas,
        data: encode_transfer_v1_ix_data(),
    };

    let collection_placeholder = mpl_core_program.clone();
    let log_wrapper_placeholder = mpl_core_program.clone();

    invoke_signed(
        &ix,
        &[
            asset.clone(),
            collection_placeholder,
            payer.clone(),
            authority_pda.clone(),
            new_owner.clone(),
            system_program.clone(),
            log_wrapper_placeholder,
        ],
        &[signer_seeds],
    )?;
    Ok(())
}

/// CPI into MPL Core `TransferV1` for an asset whose current owner is a
/// regular wallet — i.e. the depositor signs `deposit_ant` and the asset
/// moves from depositor → escrow PDA.
///
/// `authority` here is just an `AccountInfo` that will appear with
/// `is_signer = true` in the TransferV1 account metas. The actual
/// signature lives on the outer Anchor instruction; `invoke` propagates it.
pub fn transfer_asset_signed_by_wallet<'info>(
    asset: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    new_owner: &AccountInfo<'info>,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
) -> Result<()> {
    let metas = transfer_v1_metas(
        asset.key(),
        payer.key(),
        authority.key(),
        new_owner.key(),
        system_program.key(),
    );

    let ix = Instruction {
        program_id: MPL_CORE_PROGRAM_ID,
        accounts: metas,
        data: encode_transfer_v1_ix_data(),
    };

    let collection_placeholder = mpl_core_program.clone();
    let log_wrapper_placeholder = mpl_core_program.clone();

    invoke(
        &ix,
        &[
            asset.clone(),
            collection_placeholder,
            payer.clone(),
            authority.clone(),
            new_owner.clone(),
            system_program.clone(),
            log_wrapper_placeholder,
        ],
    )?;
    Ok(())
}

/// Build the canonical `AccountMeta` list for TransferV1. Pulled out so unit
/// tests can pin the layout and so both signer paths share one source of
/// truth.
fn transfer_v1_metas(
    asset: Pubkey,
    payer: Pubkey,
    authority: Pubkey,
    new_owner: Pubkey,
    system_program: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(asset, false), // 0. asset (writable)
        AccountMeta::new_readonly(MPL_CORE_PROGRAM_ID, false), // 1. collection (None)
        AccountMeta::new(payer, true),  // 2. payer (signer, writable)
        AccountMeta::new_readonly(authority, true), // 3. authority (signer)
        AccountMeta::new_readonly(new_owner, false), // 4. new_owner (passive)
        AccountMeta::new_readonly(system_program, false), // 5. system_program
        AccountMeta::new_readonly(MPL_CORE_PROGRAM_ID, false), // 6. log_wrapper (None)
    ]
}

// =========================================
// UpdateV1 — transfers Metaplex Core asset's UpdateAuthority
// =========================================
//
// PR-5 (audit L23): the escrow needs to custody UpdateAuthority alongside
// Owner so depositors cannot rewrite the metadata URI on a claimed asset.
// Per ADR-013, AR.IO ANTs are minted with `Owner == UpdateAuthority`;
// `deposit_ant` now also moves UA → escrow PDA, and `claim_ant_*` /
// `cancel_deposit` move UA → claimant / depositor atomically with the
// TransferV1 they already do.
//
// UpdateV1 instruction data:
//   - discriminator: u8 = 15
//   - new_name: Option<String>             — None (0x00) for escrow flows
//   - new_uri: Option<String>              — None (0x00)
//   - new_update_authority: Option<UpdateAuthority>
//       BaseUpdateAuthority enum: 0=None, 1=Address(Pubkey), 2=Collection(Pubkey).
//       Escrow always sets this to Some(Address(target)) — 4 bytes of
//       Option/variant tags + 32-byte pubkey.
//
// UpdateV1 account order (verified against migration claim-transfers.ts):
//   0. asset           writable
//   1. collection      optional readonly (None → mpl_core program id)
//   2. payer           writable, signer
//   3. authority       optional, signer (current UpdateAuthority)
//   4. system_program  readonly
//   5. log_wrapper     optional readonly (None → mpl_core program id)

/// MPL Core UpdateV1 instruction discriminator.
pub const UPDATE_V1_DISCRIMINATOR: u8 = 15;

/// Build the raw instruction data for an UpdateV1 that ONLY rotates
/// `new_update_authority` to `Some(Address(new_ua))`. Name and URI are
/// passed as `None` so the existing values stay put.
pub fn encode_update_v1_set_ua_ix_data(new_ua: &Pubkey) -> Vec<u8> {
    let mut data = Vec::with_capacity(1 + 1 + 1 + 1 + 1 + 32);
    data.push(UPDATE_V1_DISCRIMINATOR);
    data.push(0u8); // new_name: Option = None
    data.push(0u8); // new_uri: Option = None
    data.push(1u8); // new_update_authority: Option = Some
    data.push(1u8); // BaseUpdateAuthority::Address
    data.extend_from_slice(new_ua.as_ref());
    data
}

/// Build the canonical `AccountMeta` list for UpdateV1.
fn update_v1_metas(
    asset: Pubkey,
    payer: Pubkey,
    authority: Pubkey,
    system_program: Pubkey,
) -> Vec<AccountMeta> {
    vec![
        AccountMeta::new(asset, false), // 0. asset (writable)
        AccountMeta::new_readonly(MPL_CORE_PROGRAM_ID, false), // 1. collection (None)
        AccountMeta::new(payer, true),  // 2. payer (signer, writable)
        AccountMeta::new_readonly(authority, true), // 3. authority (current UA)
        AccountMeta::new_readonly(system_program, false), // 4. system_program
        AccountMeta::new_readonly(MPL_CORE_PROGRAM_ID, false), // 5. log_wrapper (None)
    ]
}

/// Rotate the asset's UpdateAuthority to `new_ua`. Used at deposit time
/// when the depositor (current UA) signs the outer Anchor instruction.
pub fn set_update_authority_signed_by_wallet<'info>(
    asset: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    authority: &AccountInfo<'info>,
    new_ua: &Pubkey,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
) -> Result<()> {
    let metas = update_v1_metas(
        asset.key(),
        payer.key(),
        authority.key(),
        system_program.key(),
    );
    let ix = Instruction {
        program_id: MPL_CORE_PROGRAM_ID,
        accounts: metas,
        data: encode_update_v1_set_ua_ix_data(new_ua),
    };
    let collection_placeholder = mpl_core_program.clone();
    let log_wrapper_placeholder = mpl_core_program.clone();
    invoke(
        &ix,
        &[
            asset.clone(),
            collection_placeholder,
            payer.clone(),
            authority.clone(),
            system_program.clone(),
            log_wrapper_placeholder,
        ],
    )?;
    Ok(())
}

/// Rotate the asset's UpdateAuthority to `new_ua`, signed by the escrow
/// PDA (which holds UA after `deposit_ant`). Used by `claim_ant_*` and
/// `cancel_deposit` to move UA to the claimant or depositor respectively.
pub fn set_update_authority_signed_by_pda<'info>(
    asset: &AccountInfo<'info>,
    payer: &AccountInfo<'info>,
    authority_pda: &AccountInfo<'info>,
    new_ua: &Pubkey,
    system_program: &AccountInfo<'info>,
    mpl_core_program: &AccountInfo<'info>,
    signer_seeds: &[&[u8]],
) -> Result<()> {
    let metas = update_v1_metas(
        asset.key(),
        payer.key(),
        authority_pda.key(),
        system_program.key(),
    );
    let ix = Instruction {
        program_id: MPL_CORE_PROGRAM_ID,
        accounts: metas,
        data: encode_update_v1_set_ua_ix_data(new_ua),
    };
    let collection_placeholder = mpl_core_program.clone();
    let log_wrapper_placeholder = mpl_core_program.clone();
    invoke_signed(
        &ix,
        &[
            asset.clone(),
            collection_placeholder,
            payer.clone(),
            authority_pda.clone(),
            system_program.clone(),
            log_wrapper_placeholder,
        ],
        &[signer_seeds],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_v1_data_is_two_bytes() {
        // disc(0E) + compression_proof::None(00) = 0x0E 0x00
        assert_eq!(encode_transfer_v1_ix_data(), vec![0x0E, 0x00]);
    }

    #[test]
    fn update_v1_set_ua_data_layout() {
        // disc(0F) + new_name(None=00) + new_uri(None=00)
        //   + new_ua(Some=01)(Address=01)(pubkey=32 bytes)
        let target = Pubkey::new_unique();
        let data = encode_update_v1_set_ua_ix_data(&target);
        assert_eq!(data.len(), 1 + 1 + 1 + 1 + 1 + 32);
        assert_eq!(data[0], 0x0F);
        assert_eq!(data[1], 0x00);
        assert_eq!(data[2], 0x00);
        assert_eq!(data[3], 0x01);
        assert_eq!(data[4], 0x01);
        assert_eq!(&data[5..37], target.as_ref());
    }

    #[test]
    fn update_v1_metas_have_six_accounts_and_signing_authority() {
        let asset = Pubkey::new_unique();
        let payer = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let sysprog = anchor_lang::solana_program::system_program::ID;

        let metas = update_v1_metas(asset, payer, authority, sysprog);
        assert_eq!(metas.len(), 6);
        assert_eq!(metas[1].pubkey, MPL_CORE_PROGRAM_ID); // collection placeholder
        assert_eq!(metas[5].pubkey, MPL_CORE_PROGRAM_ID); // log_wrapper placeholder
        assert!(metas[0].is_writable && !metas[0].is_signer); // asset writable
        assert!(metas[2].is_writable && metas[2].is_signer); // payer writable+signer
        assert!(!metas[3].is_writable && metas[3].is_signer); // authority signer-only
    }

    #[test]
    fn transfer_v1_metas_use_mpl_core_for_optional_slots() {
        let asset = Pubkey::new_unique();
        let payer = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let new_owner = Pubkey::new_unique();
        let sysprog = anchor_lang::solana_program::system_program::ID;

        let metas = transfer_v1_metas(asset, payer, authority, new_owner, sysprog);
        assert_eq!(metas.len(), 7);

        // Slot 1 (collection) and slot 6 (log_wrapper) should be the
        // mpl-core program id, signaling None to the underlying program.
        assert_eq!(metas[1].pubkey, MPL_CORE_PROGRAM_ID);
        assert_eq!(metas[6].pubkey, MPL_CORE_PROGRAM_ID);

        // Asset writable, payer writable+signer, authority signer-only,
        // new_owner passive readonly.
        assert!(metas[0].is_writable && !metas[0].is_signer);
        assert!(metas[2].is_writable && metas[2].is_signer);
        assert!(!metas[3].is_writable && metas[3].is_signer);
        assert!(!metas[4].is_writable && !metas[4].is_signer);
        assert!(!metas[5].is_writable && !metas[5].is_signer);
    }
}
