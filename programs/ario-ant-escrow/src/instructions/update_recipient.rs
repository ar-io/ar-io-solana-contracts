use anchor_lang::prelude::*;

use crate::{
    error::EscrowError,
    events::ASSET_TYPE_ANT,
    state::{
        derive_rotated_nonce, validated_protocol_and_len, EscrowAnt, ESCROW_ANT_SEED,
        RECIPIENT_PUBKEY_MAX_LEN,
    },
    EscrowRecipientUpdatedEvent,
};

/// Re-target an escrow at a different Arweave or Ethereum identity.
///
/// Only callable by `escrow.depositor`. Rotates the nonce using the OLD
/// nonce as additional input so any in-flight claim signatures bound to
/// the previous (recipient, nonce) tuple are invalidated.
///
/// This is the depositor's escape hatch when:
/// - the recipient's wallet was compromised before they claimed
/// - the depositor wants to switch protocols (e.g. Arweave → Ethereum)
/// - the depositor mis-typed the recipient at deposit time
pub fn handler(ctx: Context<UpdateRecipient>, new_protocol: u8, new_pubkey: Vec<u8>) -> Result<()> {
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

    // Capture old nonce before mutating; we feed it into the rotated
    // nonce derivation so the new value is unguessable.
    let old_nonce = escrow.nonce;

    escrow.recipient_protocol = protocol;
    escrow.recipient_pubkey_len = expected_len as u16;
    escrow.recipient_pubkey = [0u8; RECIPIENT_PUBKEY_MAX_LEN];
    escrow.recipient_pubkey[..expected_len].copy_from_slice(&new_pubkey);

    escrow.nonce = derive_rotated_nonce(slot, &escrow.ant_mint, &escrow.depositor, &old_nonce);

    emit!(EscrowRecipientUpdatedEvent {
        escrow: escrow.key(),
        depositor: escrow.depositor,
        asset_type: ASSET_TYPE_ANT,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "ant-escrow: recipient updated mint={} new_protocol={}",
        escrow.ant_mint,
        escrow.recipient_protocol
    );

    Ok(())
}

#[derive(Accounts)]
pub struct UpdateRecipient<'info> {
    /// Escrow being re-targeted. `has_one = depositor` enforces the
    /// authority check and the `seeds`/`bump = escrow.bump` constraint
    /// re-derives the PDA against the cached bump (cheaper than
    /// `find_program_address`).
    #[account(
        mut,
        seeds = [ESCROW_ANT_SEED, escrow.ant_mint.as_ref()],
        bump = escrow.bump,
        has_one = depositor @ EscrowError::NotDepositor,
    )]
    pub escrow: Account<'info, EscrowAnt>,

    /// Depositor — sole authority for `update_recipient`.
    pub depositor: Signer<'info>,
}
