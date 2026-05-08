//! Ed25519 attestation verification via instruction introspection.
//!
//! When a `claim_*_attested` instruction runs, the same transaction
//! must contain a Solana `Ed25519Program` sigverify instruction that
//! has already cryptographically verified an Ed25519 signature over
//! the canonical claim message. This module reads that sibling
//! instruction via the `sysvar::instructions` introspection pattern
//! and confirms it verified the signature we expect:
//!
//! - The signing pubkey matches `ATTESTOR_PUBKEY` (compiled-in)
//! - The signed message matches the canonical message reconstructed
//!   from on-chain escrow state
//!
//! The signature itself is not re-verified here — Solana's runtime
//! already enforced it when executing the Ed25519Program ix, before
//! our claim_*_attested ix runs. We're confirming WHAT was verified.
//!
//! This is the same architectural pattern used by:
//! - `claim_*_ethereum` (via secp256k1_recover syscall)
//! - `claim_*_arweave` (via custom verify_rsa_pss_sha256 — too expensive for prod)
//! - `vault_introspect` (verifies sibling ario_core::vaulted_transfer ix)
//!
//! ## Why not just `ed25519_verify` directly?
//!
//! Solana's Ed25519 verification is exposed as a native sigverify
//! program rather than a syscall. The native program is hardware-
//! accelerated (~720 CU per signature) and runs in a dedicated
//! ix slot. There is no syscall equivalent we could call from inside
//! our program; the introspection pattern is the only on-chain path.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::sysvar::instructions as ix_sysvar;

use crate::error::EscrowError;
use crate::state::{ATTESTOR_PUBKEY, ED25519_PROGRAM_ID};

/// Layout constants from the Ed25519Program native ix data format.
/// Reference: agave/programs/ed25519-program/src/lib.rs.
///
/// Header is 16 bytes. The first signature's offsets follow.
const ED25519_NUM_SIGS_OFFSET: usize = 0;
const ED25519_PADDING_OFFSET: usize = 1;
const ED25519_SIG_OFFSET_OFFSET: usize = 2;
const ED25519_SIG_IX_INDEX_OFFSET: usize = 4;
const ED25519_PUBKEY_OFFSET_OFFSET: usize = 6;
const ED25519_PUBKEY_IX_INDEX_OFFSET: usize = 8;
const ED25519_MSG_OFFSET_OFFSET: usize = 10;
const ED25519_MSG_SIZE_OFFSET: usize = 12;
const ED25519_MSG_IX_INDEX_OFFSET: usize = 14;
const ED25519_HEADER_LEN: usize = 16;

/// `0xFFFF` means "data lives in the current instruction's data buffer."
/// Any other index means it lives in a different instruction's data,
/// which the on-chain code below intentionally REJECTS — accepting
/// data-from-other-ix would let an attacker stage attestation bytes in
/// an unrelated tx slot to bypass the canonical-message binding.
const ED25519_DATA_IN_SAME_IX: u16 = 0xFFFF;

const ED25519_PUBKEY_LEN: usize = 32;
const ED25519_SIG_LEN: usize = 64;

/// Verify the previous instruction in this transaction is an
/// Ed25519Program sigverify instruction that successfully verified
/// `expected_message` was signed by `ATTESTOR_PUBKEY`.
///
/// **HARD INVARIANT:** the Ed25519Program ix MUST be at index
/// `current_ix_index - 1` (immediately before this claim ix). Do not
/// relax this — the per-claim canonical-message + nonce binding only
/// holds because each claim ix demands its own preceding sigverify.
/// A "find any Ed25519Program ix in the tx" relaxation would let one
/// honest attestation be split across many claim ixs, breaking the
/// recipient binding.
///
/// Test coverage:
/// - `test_claim_ant_arweave_attested_rejects_missing_sigverify_ix`
/// - `test_claim_ant_arweave_attested_rejects_sigverify_not_immediately_preceding`
/// - `test_claim_ant_arweave_attested_rejects_wrong_attestor`
/// - `test_claim_ant_arweave_attested_rejects_message_mismatch`
/// - `test_claim_ant_arweave_attested_rejects_wrong_modulus_in_canonical`
///   (F-1)
///
/// Errors:
/// - `MissingAttestationInstruction`: no sigverify ix found, or it's
///   the wrong program, or the introspection sysvar lookup failed.
/// - `AttestationSignerMismatch`: the pubkey in the sigverify ix is
///   not `ATTESTOR_PUBKEY`.
/// - `AttestationMessageMismatch`: the message bytes don't equal
///   `expected_message`.
/// - `MalformedAttestationInstruction`: ix data is too short, has
///   the wrong number of signatures, or uses cross-instruction data
///   references that we don't accept.
pub fn verify_attested_signature(
    instructions_sysvar: &AccountInfo,
    expected_message: &[u8],
) -> Result<()> {
    // ----------------------------------------------------------------
    // 1. Locate this instruction's index in the transaction, then
    //    fetch the one immediately before it.
    // ----------------------------------------------------------------
    let current_ix_idx = ix_sysvar::load_current_index_checked(instructions_sysvar)
        .map_err(|_| error!(EscrowError::MissingAttestationInstruction))?;
    require!(
        current_ix_idx > 0,
        EscrowError::MissingAttestationInstruction
    );
    let sigverify_ix_idx: usize = (current_ix_idx as usize) - 1;
    let sigverify_ix =
        ix_sysvar::load_instruction_at_checked(sigverify_ix_idx, instructions_sysvar)
            .map_err(|_| error!(EscrowError::MissingAttestationInstruction))?;

    // ----------------------------------------------------------------
    // 2. Confirm it's the Ed25519Program native ix.
    // ----------------------------------------------------------------
    require!(
        sigverify_ix.program_id == ED25519_PROGRAM_ID,
        EscrowError::MissingAttestationInstruction
    );

    // ----------------------------------------------------------------
    // 3. Parse the Ed25519Program instruction-data layout.
    //    Expecting exactly one signature, all data inline.
    // ----------------------------------------------------------------
    let data = sigverify_ix.data.as_slice();
    require!(
        data.len() >= ED25519_HEADER_LEN,
        EscrowError::MalformedAttestationInstruction
    );
    require!(
        data[ED25519_NUM_SIGS_OFFSET] == 1,
        EscrowError::MalformedAttestationInstruction
    );
    require!(
        data[ED25519_PADDING_OFFSET] == 0,
        EscrowError::MalformedAttestationInstruction
    );

    let sig_offset = u16_le(data, ED25519_SIG_OFFSET_OFFSET) as usize;
    let sig_ix_index = u16_le(data, ED25519_SIG_IX_INDEX_OFFSET);
    let pk_offset = u16_le(data, ED25519_PUBKEY_OFFSET_OFFSET) as usize;
    let pk_ix_index = u16_le(data, ED25519_PUBKEY_IX_INDEX_OFFSET);
    let msg_offset = u16_le(data, ED25519_MSG_OFFSET_OFFSET) as usize;
    let msg_size = u16_le(data, ED25519_MSG_SIZE_OFFSET) as usize;
    let msg_ix_index = u16_le(data, ED25519_MSG_IX_INDEX_OFFSET);

    // All three components MUST point into this same instruction's data.
    // Cross-ix references would let an attacker bind the sig over a
    // different ix's data than what's reflected in this ix's "claim
    // shape" assertions.
    require!(
        sig_ix_index == ED25519_DATA_IN_SAME_IX
            && pk_ix_index == ED25519_DATA_IN_SAME_IX
            && msg_ix_index == ED25519_DATA_IN_SAME_IX,
        EscrowError::MalformedAttestationInstruction
    );

    // ----------------------------------------------------------------
    // 4. Bounds-check the offsets and pull the pubkey + message bytes.
    // ----------------------------------------------------------------
    let pk_end = pk_offset
        .checked_add(ED25519_PUBKEY_LEN)
        .ok_or_else(|| error!(EscrowError::MalformedAttestationInstruction))?;
    let sig_end = sig_offset
        .checked_add(ED25519_SIG_LEN)
        .ok_or_else(|| error!(EscrowError::MalformedAttestationInstruction))?;
    let msg_end = msg_offset
        .checked_add(msg_size)
        .ok_or_else(|| error!(EscrowError::MalformedAttestationInstruction))?;
    require!(
        pk_end <= data.len() && sig_end <= data.len() && msg_end <= data.len(),
        EscrowError::MalformedAttestationInstruction
    );

    let pubkey_bytes = &data[pk_offset..pk_end];
    let message_bytes = &data[msg_offset..msg_end];

    // ----------------------------------------------------------------
    // 5. Match against ATTESTOR_PUBKEY and expected canonical message.
    //    The signature itself is already cryptographically verified
    //    by the Ed25519Program native ix; we're confirming WHICH
    //    (pubkey, message) it bound.
    // ----------------------------------------------------------------
    require!(
        pubkey_bytes == ATTESTOR_PUBKEY.as_ref(),
        EscrowError::AttestationSignerMismatch
    );
    require!(
        message_bytes == expected_message,
        EscrowError::AttestationMessageMismatch
    );

    Ok(())
}

#[inline]
fn u16_le(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}
