use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::{
    error::EscrowError,
    events::encode_recipient_pubkey_for_event,
    state::{
        derive_initial_nonce, validated_protocol_and_len, EscrowToken, ASSET_TYPE_TOKEN,
        ESCROW_TOKEN_SEED, RECIPIENT_PUBKEY_MAX_LEN,
    },
    EscrowDepositedEvent,
};

/// Deposit ARIO tokens into escrow for a designated Arweave or Ethereum
/// recipient. The tokens are transferred from the depositor's ATA to the
/// escrow PDA's ATA. The escrow records the amount and recipient identity.
///
/// `asset_id` is a client-supplied 32-byte identifier that becomes part of
/// the PDA seeds. For the migration batch, this is deterministic:
/// `sha256("token-escrow:" + arweave_addr)`. For ad-hoc deposits, pass a
/// random 32 bytes.
pub fn handler(
    ctx: Context<DepositTokens>,
    asset_id: [u8; 32],
    amount: u64,
    recipient_protocol: u8,
    recipient_pubkey: Vec<u8>,
) -> Result<()> {
    require!(amount > 0, EscrowError::AmountZero);

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

    let escrow = &mut ctx.accounts.escrow;
    escrow.version = 1;
    escrow.bump = ctx.bumps.escrow;
    escrow.depositor = ctx.accounts.depositor.key();
    escrow.asset_type = ASSET_TYPE_TOKEN;
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
    escrow.vault_end_timestamp = 0;
    escrow.vault_revocable = false;
    escrow._reserved = [0u8; 32];

    let (recipient_pubkey_buf, recipient_pubkey_len) = encode_recipient_pubkey_for_event(
        escrow.recipient_protocol,
        escrow.recipient_pubkey_active(),
    );

    emit!(EscrowDepositedEvent {
        escrow: escrow.key(),
        depositor: escrow.depositor,
        // Token escrows: use the SPL token mint as `asset_id` so indexers
        // can group escrow events by the asset being escrowed (per
        // events.rs comment).
        asset_id: escrow.ario_mint,
        asset_type: ASSET_TYPE_TOKEN,
        amount: escrow.amount,
        recipient_protocol: escrow.recipient_protocol,
        recipient_pubkey: recipient_pubkey_buf,
        recipient_pubkey_len,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "escrow: deposited {} mARIO for {} recipient, asset_id={}",
        amount,
        if protocol == 0 { "arweave" } else { "ethereum" },
        Pubkey::new_from_array(asset_id),
    );

    Ok(())
}

#[derive(Accounts)]
#[instruction(asset_id: [u8; 32])]
pub struct DepositTokens<'info> {
    /// New escrow PDA for this token deposit.
    #[account(
        init,
        payer = depositor,
        space = EscrowToken::SIZE,
        seeds = [ESCROW_TOKEN_SEED, depositor.key().as_ref(), &asset_id],
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

    /// Escrow PDA's ARIO token account (destination). Must be pre-created
    /// by the client (via createAssociatedTokenAccountIdempotent in the
    /// same tx). Owner must be the escrow PDA.
    #[account(
        mut,
        constraint = escrow_token_account.owner == escrow.key(),
        constraint = escrow_token_account.mint == ario_mint.key(),
    )]
    pub escrow_token_account: Account<'info, TokenAccount>,

    /// The ARIO SPL token mint.
    /// CHECK: validated by token account mint constraints.
    pub ario_mint: AccountInfo<'info>,

    /// Depositor — pays rent + tx fee + signs SPL transfer.
    #[account(mut)]
    pub depositor: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
