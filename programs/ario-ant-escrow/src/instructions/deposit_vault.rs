use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::{
    error::EscrowError,
    events::encode_recipient_pubkey_for_event,
    state::{
        derive_initial_nonce, validated_protocol_and_len, EscrowToken, ASSET_TYPE_VAULT,
        ESCROW_TOKEN_VERSION, ESCROW_VAULT_SEED, RECIPIENT_PUBKEY_MAX_LEN,
    },
    EscrowDepositedEvent,
};

/// Minimum vault lock duration: 14 days in seconds.
pub const MIN_VAULT_LOCK_DURATION: i64 = 14 * 86_400; // 1_209_600

/// Minimum vault deposit amount: 100 ARIO = 100_000_000 mARIO.
pub const MIN_VAULT_AMOUNT: u64 = 100_000_000;

/// Deposit ARIO tokens into escrow as a vaulted (time-locked) position for
/// a designated Arweave or Ethereum recipient.
///
/// Same shape as `deposit_tokens` but additionally records the vault lock
/// duration and revocability flag. Uses `ESCROW_VAULT_SEED` for PDA
/// derivation instead of `ESCROW_TOKEN_SEED`.
///
/// When the recipient claims:
/// - If the vault is still active (clock < vault_end_timestamp), the claim
///   instruction CPIs into `ario_core::vaulted_transfer` to create a
///   time-locked vault with the remaining duration.
/// - If the vault has expired, the claim instruction does a liquid SPL
///   transfer (same as token claim).
pub fn handler(
    ctx: Context<DepositVault>,
    asset_id: [u8; 32],
    amount: u64,
    lock_duration_seconds: i64,
    revocable: bool,
    recipient_protocol: u8,
    recipient_pubkey: Vec<u8>,
) -> Result<()> {
    require!(amount > 0, EscrowError::AmountZero);
    require!(
        amount >= MIN_VAULT_AMOUNT,
        EscrowError::VaultAmountBelowMinimum
    );
    require!(
        lock_duration_seconds >= MIN_VAULT_LOCK_DURATION,
        EscrowError::VaultDurationTooShort
    );
    // Revocable vaults are not supported by the escrow: it has no field for
    // the legitimate revoker, so a revocable re-lock on claim could only
    // assign control to the unbound claim-tx payer (theft). Reject the flag
    // at the source so the field is never set to an unhonorable value.
    // `escrow.vault_revocable` therefore stays `false`. See ADR-021.
    require!(!revocable, EscrowError::RevocableVaultUnsupported);

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

    // Transfer tokens from depositor's ATA to escrow's ATA.
    let cpi_accounts = SplTransfer {
        from: ctx.accounts.depositor_token_account.to_account_info(),
        to: ctx.accounts.escrow_token_account.to_account_info(),
        authority: ctx.accounts.depositor.to_account_info(),
    };
    let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
    token::transfer(cpi_ctx, amount)?;

    let clock = Clock::get()?;
    let deposit_slot = clock.slot;
    let vault_end_timestamp = clock
        .unix_timestamp
        .checked_add(lock_duration_seconds)
        .ok_or(EscrowError::ArithmeticOverflow)?;

    let escrow = &mut ctx.accounts.escrow;
    escrow.version = ESCROW_TOKEN_VERSION;
    escrow.bump = ctx.bumps.escrow;
    escrow.depositor = ctx.accounts.depositor.key();
    escrow.asset_type = ASSET_TYPE_VAULT;
    escrow.amount = amount;
    escrow.ario_mint = ctx.accounts.ario_mint.key();
    escrow.asset_id = asset_id;
    escrow.recipient_protocol = protocol;
    escrow.recipient_pubkey_len = expected_len as u16;
    escrow.recipient_pubkey = [0u8; RECIPIENT_PUBKEY_MAX_LEN];
    escrow.recipient_pubkey[..expected_len].copy_from_slice(&recipient_pubkey);
    escrow.nonce = derive_initial_nonce(
        deposit_slot,
        &Pubkey::new_from_array(asset_id),
        &ctx.accounts.depositor.key(),
    );
    escrow.deposit_slot = deposit_slot;
    escrow.vault_end_timestamp = vault_end_timestamp;
    escrow.vault_revocable = revocable;
    escrow._reserved = [0u8; 30];

    let (recipient_pubkey_buf, recipient_pubkey_len) = encode_recipient_pubkey_for_event(
        escrow.recipient_protocol,
        escrow.recipient_pubkey_active(),
    );

    emit!(EscrowDepositedEvent {
        escrow: escrow.key(),
        depositor: escrow.depositor,
        // Vault escrows: use the client-supplied 32-byte `asset_id`
        // (interpreted as a Pubkey) since it uniquely identifies the
        // vault escrow PDA — see events.rs comment for rationale.
        asset_id: Pubkey::new_from_array(escrow.asset_id),
        asset_type: ASSET_TYPE_VAULT,
        amount: escrow.amount,
        recipient_protocol: escrow.recipient_protocol,
        recipient_pubkey: recipient_pubkey_buf,
        recipient_pubkey_len,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: deposited vault {} mARIO for {} recipient, lock_duration={}s, revocable={}, asset_id={}",
        amount,
        if protocol == 0 { "arweave" } else { "ethereum" },
        lock_duration_seconds,
        revocable,
        Pubkey::new_from_array(asset_id),
    );

    Ok(())
}

#[derive(Accounts)]
#[instruction(asset_id: [u8; 32])]
pub struct DepositVault<'info> {
    /// New escrow PDA for this vault deposit.
    #[account(
        init,
        payer = depositor,
        space = EscrowToken::SIZE,
        seeds = [ESCROW_VAULT_SEED, depositor.key().as_ref(), &asset_id],
        bump,
    )]
    pub escrow: Account<'info, EscrowToken>,

    /// Depositor's ARIO token account (source of tokens).
    #[account(
        mut,
        constraint = depositor_token_account.owner == depositor.key(),
        constraint = depositor_token_account.mint == ario_mint.key(),
    )]
    pub depositor_token_account: Account<'info, TokenAccount>,

    /// Escrow PDA's ARIO token account (destination). Must be pre-created.
    #[account(
        mut,
        constraint = escrow_token_account.owner == escrow.key(),
        constraint = escrow_token_account.mint == ario_mint.key(),
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    /// The ARIO SPL token mint.
    /// CHECK: validated by token account mint constraints.
    pub ario_mint: AccountInfo<'info>,

    /// Depositor -- pays rent + tx fee + signs SPL transfer.
    #[account(mut)]
    pub depositor: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
