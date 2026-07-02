//! Shared post-verification settlement for the two vault-claim handlers
//! (`claim_vault_arweave_attested`, `claim_vault_ethereum`).
//!
//! Once a claim's signature scheme has verified, the settlement logic is
//! protocol-independent and branches on the remaining lock time
//! (ADR-0027, restoring the active path ADR-022 disabled):
//!
//! * `remaining >= ArioConfig.min_vault_duration` — **re-lock**: an atomic
//!   payer pass-through (escrow ATA → payer ATA for exactly `amount`,
//!   escrow PDA signed) followed by a same-instruction CPI into ario-core
//!   (`vaulted_transfer`, or `create_vault` when the payer IS the
//!   claimant, since `vaulted_transfer` rejects sender == recipient).
//!   The new vault is owned by the claimant, non-revocable (ADR-021),
//!   and unlocks at the escrow's original `vault_end_timestamp`.
//!   Unlike the pre-ADR-022 introspection design, the credit and debit
//!   of the pass-through happen inside one instruction — 1:1 by
//!   construction, and any CPI failure reverts the pass-through.
//! * `remaining < min_vault_duration` (including expired) — **liquid**
//!   transfer straight to the claimant's ATA. The sub-minimum window
//!   exists because ario-core would reject the re-lock CPI
//!   (`LockDurationTooShort`); delivering liquid up to
//!   `min_vault_duration` early is a deliberate, bounded consequence
//!   accepted in ADR-0027.

use anchor_lang::prelude::*;
use anchor_spl::token::{self, CloseAccount, Transfer as SplTransfer};

use crate::{error::EscrowError, state::ASSET_TYPE_VAULT, EscrowClaimedEvent};

/// Account handles both vault-claim handlers hand to [`settle_vault_claim`].
///
/// The six `Option` fields mirror the trailing optional accounts on the
/// claim instructions: `None` on expired/liquid claims, all `Some` when
/// the claim may re-lock (still-locked escrow).
pub(crate) struct VaultClaimCtx<'a, 'info> {
    pub escrow: AccountInfo<'info>,
    pub escrow_token_account: AccountInfo<'info>,
    pub claimant_token_account: AccountInfo<'info>,
    pub claimant: AccountInfo<'info>,
    pub depositor: AccountInfo<'info>,
    pub payer: AccountInfo<'info>,
    pub token_program: AccountInfo<'info>,
    pub system_program: AccountInfo<'info>,
    // --- optional re-lock account set (ADR-0027) ---
    pub payer_token_account: Option<AccountInfo<'info>>,
    pub ario_core_config: Option<&'a Account<'info, ario_core::state::ArioConfig>>,
    pub recipient_vault_counter: Option<AccountInfo<'info>>,
    pub vault: Option<AccountInfo<'info>>,
    pub vault_token_account: Option<AccountInfo<'info>>,
    pub ario_core_program: Option<AccountInfo<'info>>,
}

/// Settle a verified vault claim: move the escrowed tokens (re-lock CPI or
/// liquid transfer), close the escrow token account back to the depositor,
/// and emit `EscrowClaimedEvent`.
///
/// Callers must have already run the asset-type / protocol / nonce guards
/// and verified the claim signature for `claim_protocol`.
pub(crate) fn settle_vault_claim(
    ctx: VaultClaimCtx,
    amount: u64,
    vault_end_timestamp: i64,
    escrow_pda: Pubkey,
    asset_id: [u8; 32],
    claim_protocol: u8,
    signer_seeds: &[&[&[u8]]],
) -> Result<()> {
    let clock = Clock::get()?;
    let remaining = vault_end_timestamp.saturating_sub(clock.unix_timestamp);

    let mut relocked = false;
    if remaining > 0 {
        // Still locked. Deciding between re-lock and the sub-minimum liquid
        // fallback needs ario-core's live `min_vault_duration` (admin-mutable
        // config, never hardcoded), so a still-locked claim must always carry
        // the re-lock account set.
        let config = ctx
            .ario_core_config
            .ok_or(EscrowError::RelockAccountsMissing)?;

        if remaining >= config.min_vault_duration {
            let payer_token_account = ctx
                .payer_token_account
                .ok_or(EscrowError::RelockAccountsMissing)?;
            let recipient_vault_counter = ctx
                .recipient_vault_counter
                .ok_or(EscrowError::RelockAccountsMissing)?;
            let vault = ctx.vault.ok_or(EscrowError::RelockAccountsMissing)?;
            let vault_token_account = ctx
                .vault_token_account
                .ok_or(EscrowError::RelockAccountsMissing)?;
            let ario_core_program = ctx
                .ario_core_program
                .ok_or(EscrowError::RelockAccountsMissing)?;

            // (a) Pass-through leg: escrow ATA → payer ATA for exactly
            // `amount`, escrow PDA signed. Net-zero for the payer by
            // construction — the CPI below drains it in the same ix.
            token::transfer(
                CpiContext::new_with_signer(
                    ctx.token_program.clone(),
                    SplTransfer {
                        from: ctx.escrow_token_account.clone(),
                        to: payer_token_account.clone(),
                        authority: ctx.escrow.clone(),
                    },
                    signer_seeds,
                ),
                amount,
            )?;

            // (b) Re-lock CPI. The payer's top-level tx signature propagates
            // through the CPI, satisfying ario-core's `sender`/`owner` signer
            // constraint. `revocable` is hardwired false (ADR-021).
            if ctx.payer.key() == ctx.claimant.key() {
                // `vaulted_transfer` rejects sender == recipient
                // (SelfTransfer); `create_vault` is its self-owner twin —
                // same amount/duration checks, same PDA seeds (owner =
                // payer = claimant), revocable structurally false.
                ario_core::cpi::create_vault(
                    CpiContext::new(
                        ario_core_program,
                        ario_core::cpi::accounts::CreateVault {
                            config: config.to_account_info(),
                            vault_counter: recipient_vault_counter,
                            vault,
                            owner_token_account: payer_token_account,
                            vault_token_account,
                            owner: ctx.payer.clone(),
                            token_program: ctx.token_program.clone(),
                            system_program: ctx.system_program.clone(),
                        },
                    ),
                    amount,
                    remaining,
                )?;
            } else {
                ario_core::cpi::vaulted_transfer(
                    CpiContext::new(
                        ario_core_program,
                        ario_core::cpi::accounts::VaultedTransfer {
                            config: config.to_account_info(),
                            recipient_vault_counter,
                            vault,
                            sender_token_account: payer_token_account,
                            vault_token_account,
                            recipient: ctx.claimant.clone(),
                            sender: ctx.payer.clone(),
                            token_program: ctx.token_program.clone(),
                            system_program: ctx.system_program.clone(),
                        },
                    ),
                    amount,
                    remaining,
                    /* revocable = */ false,
                )?;
            }
            relocked = true;
        }
        // else: 0 < remaining < min_vault_duration — ario-core would reject
        // the re-lock (LockDurationTooShort); fall through to liquid.
    }

    if !relocked {
        // Liquid path (expired, or sub-minimum remainder): direct SPL
        // transfer to the claimant's ATA.
        token::transfer(
            CpiContext::new_with_signer(
                ctx.token_program.clone(),
                SplTransfer {
                    from: ctx.escrow_token_account.clone(),
                    to: ctx.claimant_token_account.clone(),
                    authority: ctx.escrow.clone(),
                },
                signer_seeds,
            ),
            amount,
        )?;
    }

    // Close the (now empty) escrow token account, rent to the depositor.
    token::close_account(CpiContext::new_with_signer(
        ctx.token_program.clone(),
        CloseAccount {
            account: ctx.escrow_token_account.clone(),
            destination: ctx.depositor.clone(),
            authority: ctx.escrow.clone(),
        },
        signer_seeds,
    ))?;

    emit!(EscrowClaimedEvent {
        escrow: escrow_pda,
        claimer: ctx.claimant.key(),
        asset_id: Pubkey::new_from_array(asset_id),
        asset_type: ASSET_TYPE_VAULT,
        amount,
        claim_protocol,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: claimed vault amount={} claimant={} mode={}",
        amount,
        ctx.claimant.key(),
        if relocked { "relock" } else { "liquid" },
    );

    Ok(())
}
