//! Release escrowed vault tokens after Ethereum ECDSA verification.
//!
//! Ethereum auth is secp256k1 `verify_personal_sign` (no sysvar — the
//! signature is passed as an instruction argument). Mirrors
//! `claim_vault_arweave_attested` aside from the verification scheme.
//!
//! Settlement branches on the remaining lock (ADR-027; see
//! `claim_vault_common`): still-locked vaults re-lock into a native
//! ario-core vault preserving the original unlock time via direct CPI;
//! expired (or sub-`min_vault_duration`) vaults deliver liquid tokens to
//! the claimant. The trailing optional accounts carry the re-lock set:
//! omitted entirely on EXPIRED claims, required whenever the escrow is
//! still locked — even when the sub-minimum fallback delivers liquid,
//! the handler must read `ario_core_config.min_vault_duration` to decide.

use anchor_lang::prelude::*;
use anchor_spl::token::{Token, TokenAccount};

use crate::{
    canonical::build_escrow_claim_message,
    error::EscrowError,
    instructions::claim_vault_common::{settle_vault_claim, VaultClaimCtx},
    state::{
        EscrowToken, ASSET_TYPE_VAULT, ESCROW_VAULT_SEED, ETHEREUM_PUBKEY_LEN, PROTOCOL_ETHEREUM,
    },
    verify::ethereum::verify_personal_sign,
};

pub fn handler(
    ctx: Context<ClaimVaultEthereum>,
    message_nonce: [u8; 32],
    signature: [u8; 65],
) -> Result<()> {
    let escrow = &ctx.accounts.escrow;

    // 1. Asset type guard.
    require!(
        escrow.asset_type == ASSET_TYPE_VAULT,
        EscrowError::AssetTypeMismatch
    );

    // 2. Protocol guard.
    require!(
        escrow.recipient_protocol == PROTOCOL_ETHEREUM,
        EscrowError::ProtocolMismatch
    );

    // 3. Replay protection.
    require!(message_nonce == escrow.nonce, EscrowError::NonceMismatch);

    // 4. Canonical message from on-chain state.
    let message = build_escrow_claim_message(
        "vault",
        &escrow.asset_id,
        escrow.amount,
        &ctx.accounts.claimant.key(),
        &escrow.nonce,
        escrow.recipient_pubkey_active(),
    );

    // 5. Verify ECDSA + EIP-191 + low-S.
    let expected_address = escrow.recipient_pubkey_active();
    require!(
        expected_address.len() == ETHEREUM_PUBKEY_LEN,
        EscrowError::SignatureVerificationFailed
    );
    verify_personal_sign(&message, &signature, expected_address)?;

    // 6. Sig is good — settle (re-lock or liquid).
    let depositor_key = escrow.depositor;
    let asset_id = escrow.asset_id;
    let bump = escrow.bump;
    let amount = escrow.amount;
    let vault_end_timestamp = escrow.vault_end_timestamp;
    let escrow_pda = escrow.key();

    let bump_bytes = [bump];
    let signer_seeds: &[&[&[u8]]] = &[&[
        ESCROW_VAULT_SEED,
        depositor_key.as_ref(),
        asset_id.as_ref(),
        &bump_bytes,
    ]];

    settle_vault_claim(
        VaultClaimCtx {
            escrow: ctx.accounts.escrow.to_account_info(),
            escrow_token_account: ctx.accounts.escrow_token_account.to_account_info(),
            claimant_token_account: ctx.accounts.claimant_token_account.to_account_info(),
            claimant: ctx.accounts.claimant.to_account_info(),
            depositor: ctx.accounts.depositor.to_account_info(),
            payer: ctx.accounts.payer.to_account_info(),
            token_program: ctx.accounts.token_program.to_account_info(),
            system_program: ctx.accounts.system_program.to_account_info(),
            payer_token_account: ctx
                .accounts
                .payer_token_account
                .as_ref()
                .map(|a| a.to_account_info()),
            ario_core_config: ctx.accounts.ario_core_config.as_deref(),
            recipient_vault_counter: ctx
                .accounts
                .recipient_vault_counter
                .as_ref()
                .map(|a| a.to_account_info()),
            vault: ctx.accounts.vault.as_ref().map(|a| a.to_account_info()),
            vault_token_account: ctx
                .accounts
                .vault_token_account
                .as_ref()
                .map(|a| a.to_account_info()),
            ario_core_program: ctx
                .accounts
                .ario_core_program
                .as_ref()
                .map(|a| a.to_account_info()),
        },
        amount,
        vault_end_timestamp,
        escrow_pda,
        asset_id,
        PROTOCOL_ETHEREUM,
        signer_seeds,
    )
}

#[derive(Accounts)]
pub struct ClaimVaultEthereum<'info> {
    /// Escrow PDA holding vault deposit metadata.
    #[account(
        mut,
        seeds = [ESCROW_VAULT_SEED, escrow.depositor.as_ref(), &escrow.asset_id],
        bump = escrow.bump,
        has_one = depositor,
        close = depositor,
    )]
    pub escrow: Account<'info, EscrowToken>,

    /// Escrow PDA's ARIO token account (source of tokens).
    #[account(
        mut,
        constraint = escrow_token_account.owner == escrow.key(),
        constraint = escrow_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    /// Claimant's ARIO token account (destination for the liquid path).
    #[account(
        mut,
        constraint = claimant_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
        constraint = claimant_token_account.owner == claimant.key() @ EscrowError::TokenAccountOwnerMismatch,
    )]
    pub claimant_token_account: Account<'info, TokenAccount>,

    /// Recipient of the vault/tokens — pubkey bound into canonical message.
    /// CHECK: validated by canonical message ↔ signature binding.
    pub claimant: AccountInfo<'info>,

    /// Original depositor — receives rent on escrow close.
    /// CHECK: identity validated by `has_one` constraint on escrow.
    #[account(mut)]
    pub depositor: AccountInfo<'info>,

    /// Tx fee payer.
    #[account(mut)]
    pub payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,

    // --- Optional re-lock account set (ADR-027) ---
    // Trailing optionals: omitted entirely on EXPIRED claims, so the
    // pre-ADR-027 claim ABI keeps working. Pass ALL six whenever the escrow
    // is still locked (`vault_end_timestamp > now`) — including claims that
    // will settle liquid via the sub-`min_vault_duration` fallback.
    //
    /// Payer's ARIO ATA — atomic pass-through leg of the active re-lock:
    /// receives `amount` from escrow and is drained by the
    /// same-instruction CPI into ario-core. Net-zero for the payer.
    #[account(
        mut,
        constraint = payer_token_account.mint == escrow.ario_mint @ EscrowError::MintMismatch,
        constraint = payer_token_account.owner == payer.key() @ EscrowError::TokenAccountOwnerMismatch,
    )]
    pub payer_token_account: Option<Box<Account<'info, TokenAccount>>>,

    /// ario-core `ArioConfig` PDA — read for the live `min_vault_duration`
    /// at claim time; `mut` because the re-lock CPI updates its supply
    /// tracking (Anchor never writes back foreign-owned accounts).
    #[account(
        mut,
        seeds = [b"ario_config"],
        bump,
        seeds::program = ario_core::ID,
    )]
    pub ario_core_config: Option<Box<Account<'info, ario_core::state::ArioConfig>>>,

    /// CHECK: seeds `[VAULT_COUNTER_SEED, claimant]` validated (and
    /// init-if-needed) by ario-core during the re-lock CPI.
    #[account(mut)]
    pub recipient_vault_counter: Option<UncheckedAccount<'info>>,

    /// CHECK: seeds `[VAULT_SEED, claimant, counter.next_id]` validated and
    /// initialized by ario-core during the re-lock CPI. The caller derives
    /// it from the counter's current `next_id` (see ANT_ESCROW_PROTOCOL_SPEC).
    #[account(mut)]
    pub vault: Option<UncheckedAccount<'info>>,

    /// CHECK: pre-created token account owned by the new vault PDA;
    /// owner/mint validated by ario-core during the re-lock CPI.
    #[account(mut)]
    pub vault_token_account: Option<UncheckedAccount<'info>>,

    /// CHECK: pinned by address constraint to the canonical ario-core id.
    #[account(address = ario_core::ID)]
    pub ario_core_program: Option<AccountInfo<'info>>,
}
