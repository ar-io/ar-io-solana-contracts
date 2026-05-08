//! Release the escrowed ANT to `claimant` after verifying an Ed25519
//! attestation from the AR.IO attestor service.
//!
//! ## Why this exists alongside `claim_arweave`
//!
//! `claim_arweave` does on-chain RSA-PSS-4096 verification via the
//! `sol_big_mod_exp` syscall. The syscall is feature-gated and
//! currently blocked on every public Solana cluster (devnet, testnet,
//! mainnet), so that path will revert at runtime. The on-chain RSA
//! path stays in the codebase as a reference and as a fallback for
//! the day the syscall is enabled.
//!
//! For practical use, AR.IO operates an off-chain attestor service
//! (`migration/attestor/`) that:
//!   1. Receives the user's RSA-PSS-signed canonical claim message
//!   2. Verifies the RSA-PSS sig via `node:crypto` (~5ms)
//!   3. Re-signs the SAME canonical message bytes with Ed25519
//!   4. Returns the Ed25519 sig + attestor pubkey
//!
//! The user constructs a Solana transaction with two instructions:
//!   1. `Ed25519Program` native sigverify ix (~720 CU)
//!      — verifies the Ed25519 signature
//!   2. `claim_ant_arweave_attested` (this ix)
//!      — uses sysvar::instructions to confirm the previous ix is
//!        Ed25519Program with the right pubkey + canonical message
//!
//! Crucially, the canonical message bytes are reconstructed entirely
//! on-chain from escrow state. The user supplies only `(message_nonce)`
//! to detect stale-nonce errors with a clean error code; the canonical
//! message body is never client-supplied.
//!
//! See `migration/attestor/README.md` for the off-chain side and
//! `src/verify/attested.rs` for the introspection helper.

use anchor_lang::prelude::*;

use crate::{
    canonical::build_ant_escrow_claim_message,
    error::EscrowError,
    mpl_core_cpi::{set_update_authority_signed_by_pda, transfer_asset_signed_by_pda},
    state::{
        assert_mpl_core_asset_v1, EscrowAnt, ESCROW_ANT_SEED, MPL_CORE_PROGRAM_ID, PROTOCOL_ARWEAVE,
    },
    verify::attested::verify_attested_signature,
};

pub fn handler(ctx: Context<ClaimAntArweaveAttested>, message_nonce: [u8; 32]) -> Result<()> {
    let escrow = &ctx.accounts.escrow;

    // 0. AssetV1 discriminator check (parity with deposit / claim_arweave).
    assert_mpl_core_asset_v1(&ctx.accounts.ant_asset)?;

    // 1. Protocol guard. Stops a misrouted Ethereum-recipient escrow
    //    from being claimed via the Arweave path.
    require!(
        escrow.recipient_protocol == PROTOCOL_ARWEAVE,
        EscrowError::ProtocolMismatch
    );

    // 2. Replay protection: signed message must reference the CURRENT
    //    escrow nonce. After this instruction runs the escrow PDA
    //    closes, so even an exact-replay tx with the same attestation
    //    would fail to find the account on the second submission.
    require!(message_nonce == escrow.nonce, EscrowError::NonceMismatch);

    // 3. Reconstruct the canonical message from on-chain state. Same
    //    bytes the user signed with their Arweave wallet, same bytes
    //    the off-chain attestor re-signed with Ed25519. NEVER trust
    //    client-supplied message bytes.
    //
    //    The `recipient_pubkey_active()` is the trusted modulus stored
    //    at deposit time. The attestor's canonical builder hashes the
    //    *client-supplied* modulus into the same `recipient` field.
    //    If they differ, the canonical bytes diverge and the Ed25519
    //    introspection in step 4 fails — closing F-1 (see
    //    `docs/ATTESTOR_SECURITY_REVIEW.md`).
    let message = build_ant_escrow_claim_message(
        &escrow.ant_mint,
        &ctx.accounts.claimant.key(),
        &escrow.nonce,
        escrow.recipient_pubkey_active(),
    );

    // 4. Verify the Ed25519 attestation via instruction introspection.
    //    Confirms a preceding Ed25519Program sigverify ix verified the
    //    attestor's signature over `message`.
    verify_attested_signature(&ctx.accounts.instructions_sysvar, &message)?;

    // 5. Attestation is good — release the ANT.
    let ant_mint_key = escrow.ant_mint;
    let bump = escrow.bump;
    let signer_seeds: &[&[u8]] = &[ESCROW_ANT_SEED, ant_mint_key.as_ref(), &[bump]];

    transfer_asset_signed_by_pda(
        &ctx.accounts.ant_asset,
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.claimant.to_account_info(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        signer_seeds,
    )?;

    // Audit L23: rotate UpdateAuthority to claimant atomically. See
    // claim_arweave.rs for full rationale.
    set_update_authority_signed_by_pda(
        &ctx.accounts.ant_asset,
        &ctx.accounts.payer.to_account_info(),
        &ctx.accounts.escrow.to_account_info(),
        &ctx.accounts.claimant.key(),
        &ctx.accounts.system_program.to_account_info(),
        &ctx.accounts.mpl_core_program,
        signer_seeds,
    )?;

    msg!(
        "ant-escrow: claimed (arweave-attested) mint={} claimant={}",
        ant_mint_key,
        ctx.accounts.claimant.key()
    );

    Ok(())
}

#[derive(Accounts)]
pub struct ClaimAntArweaveAttested<'info> {
    /// Escrow PDA. `has_one = depositor` ties the close-rent recipient
    /// to the original depositor account passed below; `close = depositor`
    /// returns rent on successful claim.
    #[account(
        mut,
        seeds = [ESCROW_ANT_SEED, escrow.ant_mint.as_ref()],
        bump = escrow.bump,
        has_one = depositor,
        close = depositor,
    )]
    pub escrow: Account<'info, EscrowAnt>,

    /// The Metaplex Core asset being released. Must match
    /// `escrow.ant_mint` so a misrouted account can't redirect the
    /// transfer.
    /// CHECK: pinned to escrow.ant_mint and to the mpl-core program.
    #[account(
        mut,
        address = escrow.ant_mint @ EscrowError::AntMintMismatch,
        constraint = ant_asset.owner == &MPL_CORE_PROGRAM_ID @ EscrowError::InvalidAsset,
    )]
    pub ant_asset: AccountInfo<'info>,

    /// Recipient of the ANT — its pubkey is bound into the canonical
    /// message that the attestor signed over. Front-runners can resubmit
    /// the tx but cannot redirect the asset.
    /// CHECK: validated by the canonical message ↔ Ed25519 sig binding.
    pub claimant: AccountInfo<'info>,

    /// Original depositor — receives rent on escrow close. Pinned via
    /// `has_one = depositor` on the escrow above.
    /// CHECK: identity validated by the `has_one` constraint.
    #[account(mut)]
    pub depositor: AccountInfo<'info>,

    /// Tx fee payer. Doesn't have to be the claimant — anyone can submit
    /// a valid attestation on the recipient's behalf.
    #[account(mut)]
    pub payer: Signer<'info>,

    /// CHECK: pinned by address constraint.
    #[account(address = MPL_CORE_PROGRAM_ID)]
    pub mpl_core_program: AccountInfo<'info>,

    /// Solana `sysvar::instructions` — required for introspecting the
    /// preceding Ed25519Program sigverify instruction.
    /// CHECK: pinned by address constraint to the sysvar id.
    #[account(address = anchor_lang::solana_program::sysvar::instructions::ID)]
    pub instructions_sysvar: AccountInfo<'info>,

    pub system_program: Program<'info, System>,
}
