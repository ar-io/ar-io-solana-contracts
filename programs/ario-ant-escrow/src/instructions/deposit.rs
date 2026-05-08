use anchor_lang::prelude::*;

use crate::{
    error::EscrowError,
    events::{encode_recipient_pubkey_for_event, ASSET_TYPE_ANT},
    mpl_core_cpi::{set_update_authority_signed_by_wallet, transfer_asset_signed_by_wallet},
    state::{
        derive_initial_nonce, read_mpl_core_owner, validated_protocol_and_len, EscrowAnt,
        ESCROW_ANT_SEED, ESCROW_VERSION_V1, MPL_CORE_PROGRAM_ID, RECIPIENT_PUBKEY_MAX_LEN,
    },
    EscrowDepositedEvent,
};

/// Lock an ANT into escrow targeted at a specific Arweave or Ethereum
/// identity.
///
/// Validations: see `docs/ANT_ESCROW_DESIGN.md` § Instructions → deposit_ant.
/// Protocol/pubkey-length pairs are constrained to `(0, 512)` or `(1, 20)`;
/// any other combination returns `EscrowError::InvalidRecipient*`.
///
/// Effects:
/// 1. Initializes the `EscrowAnt` PDA (rent paid by depositor).
/// 2. CPIs into `mpl-core::TransferV1` to move the ANT from depositor to PDA.
/// 3. Stamps the version, bump, denormalized fields, and the initial nonce
///    derived from `(deposit_slot, ant_mint, depositor)`.
pub fn handler(
    ctx: Context<DepositAnt>,
    recipient_protocol: u8,
    recipient_pubkey: Vec<u8>,
) -> Result<()> {
    // 1. Validate the (protocol, pubkey_len) pair up front so we never
    //    half-initialize an escrow with malformed recipient data.
    let (protocol, expected_len) =
        validated_protocol_and_len(recipient_protocol, recipient_pubkey.len()).ok_or_else(
            || {
                if recipient_protocol > 1 {
                    error!(EscrowError::InvalidRecipientProtocol)
                } else {
                    error!(EscrowError::InvalidRecipientPubkeyLength)
                }
            },
        )?;

    // 2. Verify the depositor is the current Metaplex Core asset owner.
    //    mpl-core's TransferV1 will also enforce this (the authority
    //    account must equal the asset's owner field), but checking here
    //    gives a clearer error and avoids burning CU on a CPI that's
    //    guaranteed to revert.
    let asset_data = ctx.accounts.ant_asset.try_borrow_data()?;
    let nft_owner = read_mpl_core_owner(&asset_data)?;
    drop(asset_data);
    require_keys_eq!(
        nft_owner,
        ctx.accounts.depositor.key(),
        EscrowError::NotAntOwner
    );

    // 3. Capture deposit slot now — used both for the on-chain record and
    //    as a salt in the initial nonce derivation.
    let clock = Clock::get()?;
    let deposit_slot = clock.slot;

    // 4. Move the ANT into escrow custody. From this point forward the PDA
    //    is the asset's owner; only `cancel_deposit` and `claim_ant_*`
    //    can release it.
    transfer_asset_signed_by_wallet(
        &ctx.accounts.ant_asset,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
    )?;

    // 4b. Audit L23: also move UpdateAuthority into escrow custody. AR.IO
    //     ANTs are minted with `Owner == UpdateAuthority` (ADR-013); the
    //     depositor is therefore the current UA at this point and can sign
    //     the UpdateV1 alongside the TransferV1 above. Without this, the
    //     depositor would retain UA after the recipient claimed the asset
    //     and could rewrite the metadata URI on the claimed ANT.
    set_update_authority_signed_by_wallet(
        &ctx.accounts.ant_asset,
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.depositor.to_account_info(),
        &ctx.accounts.escrow.key(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
    )?;

    // 5. Populate the EscrowAnt record. Pubkey blob is left-aligned and
    //    zero-padded to RECIPIENT_PUBKEY_MAX_LEN; verifiers slice via
    //    `recipient_pubkey_active()`.
    let escrow = &mut ctx.accounts.escrow;
    escrow.version = ESCROW_VERSION_V1;
    escrow.bump = ctx.bumps.escrow;
    escrow.depositor = ctx.accounts.depositor.key();
    escrow.ant_mint = ctx.accounts.ant_asset.key();
    escrow.recipient_protocol = protocol;
    escrow.recipient_pubkey_len = expected_len as u16;

    // Zero the full 512-byte blob first to scrub any residue, then copy
    // the active prefix in.
    escrow.recipient_pubkey = [0u8; RECIPIENT_PUBKEY_MAX_LEN];
    escrow.recipient_pubkey[..expected_len].copy_from_slice(&recipient_pubkey);

    escrow.nonce = derive_initial_nonce(
        deposit_slot,
        &ctx.accounts.ant_asset.key(),
        &ctx.accounts.depositor.key(),
    );
    escrow.deposit_slot = deposit_slot;
    escrow._reserved = [0u8; 32];

    let (recipient_pubkey_buf, recipient_pubkey_len) = encode_recipient_pubkey_for_event(
        escrow.recipient_protocol,
        escrow.recipient_pubkey_active(),
    );

    emit!(EscrowDepositedEvent {
        escrow: escrow.key(),
        depositor: escrow.depositor,
        asset_id: escrow.ant_mint,
        asset_type: ASSET_TYPE_ANT,
        amount: 0,
        recipient_protocol: escrow.recipient_protocol,
        recipient_pubkey: recipient_pubkey_buf,
        recipient_pubkey_len,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "ant-escrow: deposited mint={} depositor={} protocol={}",
        escrow.ant_mint,
        escrow.depositor,
        escrow.recipient_protocol
    );

    Ok(())
}

#[derive(Accounts)]
pub struct DepositAnt<'info> {
    /// New escrow PDA, one per ANT mint. `init` enforces "no double
    /// deposit" — a second `deposit_ant` for the same mint fails because
    /// the PDA already exists.
    #[account(
        init,
        payer = depositor,
        space = EscrowAnt::SIZE,
        seeds = [ESCROW_ANT_SEED, ant_asset.key().as_ref()],
        bump,
    )]
    pub escrow: Account<'info, EscrowAnt>,

    /// The Metaplex Core asset (ANT) being escrowed. Constrained to the
    /// mpl-core program so callers can't pass a system account or a
    /// different program's NFT. Inner-data discriminator is checked in
    /// the handler via `read_mpl_core_owner`.
    /// CHECK: validated by owner constraint + read_mpl_core_owner check.
    #[account(
        mut,
        constraint = ant_asset.owner == &MPL_CORE_PROGRAM_ID @ EscrowError::InvalidAsset,
    )]
    pub ant_asset: AccountInfo<'info>,

    /// Caller — must be the current ANT owner. Pays rent + tx fee + signs
    /// the mpl-core TransferV1 CPI.
    #[account(mut)]
    pub depositor: Signer<'info>,

    /// Metaplex Core program. Pinned to MPL_CORE_PROGRAM_ID so the CPI
    /// can't be redirected.
    /// CHECK: pinned by address constraint.
    #[account(address = MPL_CORE_PROGRAM_ID)]
    pub mpl_core_program: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}
