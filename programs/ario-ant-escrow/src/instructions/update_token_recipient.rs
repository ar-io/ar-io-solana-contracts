use anchor_lang::prelude::*;

use crate::{
    error::EscrowError,
    events::ASSET_TYPE_TOKEN,
    state::{
        derive_rotated_nonce, validated_protocol_and_len, EscrowToken, ESCROW_TOKEN_SEED,
        RECIPIENT_PUBKEY_MAX_LEN,
    },
    EscrowRecipientUpdatedEvent,
};

/// Re-target a token/vault escrow at a different Arweave or Ethereum identity.
///
/// Only callable by `escrow.depositor`. Rotates the nonce using the OLD
/// nonce as additional input so any in-flight claim signatures bound to
/// the previous (recipient, nonce) tuple are invalidated.
///
/// Same pattern as `update_recipient.rs` (ANT escrow) but operates on the
/// `EscrowToken` PDA.
pub fn handler(
    ctx: Context<UpdateTokenRecipient>,
    new_protocol: u8,
    new_pubkey: Vec<u8>,
) -> Result<()> {
    let (protocol, expected_len) = validated_protocol_and_len(new_protocol, new_pubkey.len())
        .ok_or_else(|| {
            if new_protocol > 1 {
                error!(EscrowError::InvalidRecipientProtocol)
            } else {
                error!(EscrowError::InvalidRecipientPubkeyLength)
            }
        })?;

    let clock = Clock::get()?;
    let slot = clock.slot;

    let escrow = &mut ctx.accounts.escrow;

    // Capture old nonce before mutating.
    let old_nonce = escrow.nonce;

    escrow.recipient_protocol = protocol;
    escrow.recipient_pubkey_len = expected_len as u16;
    escrow.recipient_pubkey = [0u8; RECIPIENT_PUBKEY_MAX_LEN];
    escrow.recipient_pubkey[..expected_len].copy_from_slice(&new_pubkey);

    // Use asset_id as the "mint" equivalent for nonce derivation.
    let asset_id_pubkey = Pubkey::new_from_array(escrow.asset_id);
    escrow.nonce = derive_rotated_nonce(slot, &asset_id_pubkey, &escrow.depositor, &old_nonce);

    emit!(EscrowRecipientUpdatedEvent {
        escrow: escrow.key(),
        depositor: escrow.depositor,
        asset_type: ASSET_TYPE_TOKEN,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: token recipient updated asset_id={} new_protocol={}",
        asset_id_pubkey,
        escrow.recipient_protocol
    );

    Ok(())
}

#[derive(Accounts)]
pub struct UpdateTokenRecipient<'info> {
    /// Escrow being re-targeted. `has_one = depositor` enforces the
    /// authority check; seeds/bump re-derive the PDA.
    #[account(
        mut,
        seeds = [ESCROW_TOKEN_SEED, escrow.depositor.as_ref(), &escrow.asset_id],
        bump = escrow.bump,
        has_one = depositor @ EscrowError::NotDepositor,
    )]
    pub escrow: Account<'info, EscrowToken>,

    /// Depositor -- sole authority for `update_token_recipient`.
    pub depositor: Signer<'info>,
}
