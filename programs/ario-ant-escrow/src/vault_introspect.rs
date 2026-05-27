//! Transaction instruction introspection for vault claims.
//!
//! When claiming an active vault escrow, the escrow program releases tokens
//! to the payer's ATA and requires a matching `ario_core::vaulted_transfer`
//! instruction to exist in the SAME transaction. This module verifies that
//! the sibling instruction has the correct parameters (amount, lock duration,
//! recipient, revocability).
//!
//! Pattern: same as Ed25519 / Secp256k1 precompile verification — read
//! `sysvar::instructions` to inspect sibling instructions.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions as ix_sysvar;

use crate::error::EscrowError;

/// Anchor discriminator for `ario_core::vaulted_transfer`.
/// = sha256("global:vaulted_transfer")[..8]
const VAULTED_TRANSFER_DISC: [u8; 8] = [0x09, 0xc0, 0x19, 0x26, 0x6d, 0xea, 0xbf, 0x93];

/// Verify that the current transaction contains a `vaulted_transfer`
/// instruction addressed to `ario_core_program_id` with matching params.
///
/// Checks:
/// - Instruction program_id == ario_core_program_id
/// - Discriminator matches vaulted_transfer
/// - amount (u64 LE at offset 8) == expected_amount
/// - lock_duration_seconds (i64 LE at offset 16) >= min_lock_duration
/// - revocable (bool at offset 24) == FALSE — the escrow only ever re-locks
///   non-revocable vaults (a revocable re-lock's controller would be the
///   unbound claim-tx payer; see ADR-021). A revocable-but-otherwise-matching
///   transfer returns `RevocableVaultUnsupported`.
/// - The recipient account (index 5 in the instruction's accounts)
///   matches expected_recipient
///
/// `tolerance_seconds`: allowed slack between the escrow's remaining
/// duration and the vaulted_transfer's lock_duration. Accounts for clock
/// drift between when the SDK built the tx and when it executes. 60s is
/// generous.
pub fn verify_vaulted_transfer_in_tx(
    instructions_sysvar: &AccountInfo,
    ario_core_program_id: &Pubkey,
    expected_amount: u64,
    min_lock_duration: i64,
    expected_recipient: &Pubkey,
    tolerance_seconds: i64,
) -> Result<()> {
    // Iterate through all instructions looking for a matching vaulted_transfer.
    //
    // The escrow ONLY re-locks NON-revocable vaults: a revocable re-lock makes
    // the `vaulted_transfer` sender the vault controller (`ario_core` sets
    // `controller = sender` when revocable), and the sender is the
    // attacker-choosable claim-tx payer — who could then `revoke_vault` and
    // steal the re-locked funds before expiry. The claim attestation binds the
    // claimant/amount/nonce but NOT the controller, so revocable re-locks have
    // no safe binding. See ADR-021. (`deposit_vault` also rejects revocable, so
    // `expected_revocable` is implicitly false everywhere.)
    let mut found = false;
    // A transfer that matches everything EXCEPT it's revocable — tracked so we
    // can return a clear error instead of the opaque "missing transfer" one.
    let mut saw_revocable_match = false;
    let mut idx: u16 = 0;

    loop {
        let ix = match ix_sysvar::load_instruction_at_checked(idx as usize, instructions_sysvar) {
            Ok(ix) => ix,
            Err(_) => break, // No more instructions
        };

        // Check if this instruction is addressed to ario-core
        if ix.program_id == *ario_core_program_id {
            // Check discriminator (first 8 bytes of data)
            if ix.data.len() >= 25 && ix.data[..8] == VAULTED_TRANSFER_DISC {
                // Parse params from instruction data:
                // offset 8:  amount (u64 LE)
                // offset 16: lock_duration_seconds (i64 LE)
                // offset 24: revocable (bool, 1 byte)
                let amount = u64::from_le_bytes(ix.data[8..16].try_into().unwrap_or([0u8; 8]));
                let lock_duration =
                    i64::from_le_bytes(ix.data[16..24].try_into().unwrap_or([0u8; 8]));
                let revocable = ix.data[24] != 0;

                // Check the recipient account (index 5 in VaultedTransfer accounts)
                // VaultedTransfer account order:
                //   0: config, 1: recipient_vault_counter, 2: vault,
                //   3: sender_token_account, 4: vault_token_account,
                //   5: recipient, 6: sender, 7: token_program, 8: system_program
                let recipient_matches =
                    ix.accounts.len() > 5 && ix.accounts[5].pubkey == *expected_recipient;

                if amount == expected_amount
                    && lock_duration >= min_lock_duration - tolerance_seconds
                    && recipient_matches
                {
                    if revocable {
                        // Matches in every other respect but is revocable —
                        // not an acceptable re-lock. Keep scanning (a valid
                        // non-revocable sibling could appear later), but
                        // remember it for a precise error.
                        saw_revocable_match = true;
                    } else {
                        found = true;
                        break;
                    }
                }
            }
        }

        idx += 1;
        if idx > 20 {
            // Safety: don't iterate forever
            break;
        }
    }

    // A revocable re-lock that otherwise matched is rejected with a precise
    // error rather than the generic "missing transfer".
    if !found && saw_revocable_match {
        return err!(EscrowError::RevocableVaultUnsupported);
    }
    require!(found, EscrowError::MissingVaultedTransferInstruction);
    Ok(())
}
