use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::ArioError;
use crate::state::*;

pub mod create_vault {
    use super::*;

    pub fn handler(
        ctx: Context<CreateVault>,
        amount: u64,
        lock_duration_seconds: i64,
    ) -> Result<()> {
        let config = &ctx.accounts.config;
        let clock = Clock::get()?;

        // Validate lock duration
        require!(
            lock_duration_seconds >= config.min_vault_duration,
            ArioError::LockDurationTooShort
        );
        require!(
            lock_duration_seconds <= config.max_vault_duration,
            ArioError::LockDurationTooLong
        );
        require!(amount > 0, ArioError::InvalidAmount);
        require!(
            amount >= ArioConfig::MIN_VAULT_SIZE,
            ArioError::VaultBelowMinimum
        );

        // Transfer tokens to vault token account
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.owner_token_account.to_account_info(),
            to: ctx.accounts.vault_token_account.to_account_info(),
            authority: ctx.accounts.owner.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, amount)?;

        // Initialize vault
        let vault = &mut ctx.accounts.vault;
        let counter = &mut ctx.accounts.vault_counter;

        // Always set owner and bump (idempotent for init_if_needed)
        counter.owner = ctx.accounts.owner.key();
        counter.bump = ctx.bumps.vault_counter;

        vault.owner = ctx.accounts.owner.key();
        vault.vault_id = counter.next_id;
        vault.amount = amount;
        vault.start_timestamp = clock.unix_timestamp;
        vault.end_timestamp = clock
            .unix_timestamp
            .checked_add(lock_duration_seconds)
            .ok_or(ArioError::ArithmeticOverflow)?;
        vault.controller = None;
        vault.revocable = false;
        vault.bump = ctx.bumps.vault;

        // Increment counter
        counter.next_id = counter
            .next_id
            .checked_add(1)
            .ok_or(ArioError::ArithmeticOverflow)?;

        // Update config supply tracking
        let config = &mut ctx.accounts.config;
        config.locked_supply = config
            .locked_supply
            .checked_add(amount)
            .ok_or(ArioError::ArithmeticOverflow)?;
        config.circulating_supply = config
            .circulating_supply
            .checked_sub(amount)
            .ok_or(ArioError::ArithmeticUnderflow)?;

        emit!(VaultCreatedEvent {
            owner: vault.owner,
            vault_id: vault.vault_id,
            amount: vault.amount,
            end_timestamp: vault.end_timestamp,
            revocable: false,
        });

        Ok(())
    }
}

pub mod vaulted_transfer {
    use super::*;

    pub fn handler(
        ctx: Context<VaultedTransfer>,
        amount: u64,
        lock_duration_seconds: i64,
        revocable: bool,
    ) -> Result<()> {
        let config = &ctx.accounts.config;
        let clock = Clock::get()?;

        // Validate lock duration
        require!(
            lock_duration_seconds >= config.min_vault_duration,
            ArioError::LockDurationTooShort
        );
        require!(
            lock_duration_seconds <= config.max_vault_duration,
            ArioError::LockDurationTooLong
        );
        require!(amount > 0, ArioError::InvalidAmount);
        require!(
            amount >= ArioConfig::MIN_VAULT_SIZE,
            ArioError::VaultBelowMinimum
        );

        // Prevent self-vaulted-transfer (use create_vault instead)
        require!(
            ctx.accounts.sender.key() != ctx.accounts.recipient.key(),
            ArioError::SelfTransfer
        );

        // Transfer tokens to vault token account
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.sender_token_account.to_account_info(),
            to: ctx.accounts.vault_token_account.to_account_info(),
            authority: ctx.accounts.sender.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, amount)?;

        // Initialize vault for recipient
        let vault = &mut ctx.accounts.vault;
        let counter = &mut ctx.accounts.recipient_vault_counter;

        // Always set owner and bump (idempotent for init_if_needed)
        counter.owner = ctx.accounts.recipient.key();
        counter.bump = ctx.bumps.recipient_vault_counter;

        vault.owner = ctx.accounts.recipient.key();
        vault.vault_id = counter.next_id;
        vault.amount = amount;
        vault.start_timestamp = clock.unix_timestamp;
        vault.end_timestamp = clock
            .unix_timestamp
            .checked_add(lock_duration_seconds)
            .ok_or(ArioError::ArithmeticOverflow)?;
        vault.controller = if revocable {
            Some(ctx.accounts.sender.key())
        } else {
            None
        };
        vault.revocable = revocable;
        vault.bump = ctx.bumps.vault;

        // Increment counter
        counter.next_id = counter
            .next_id
            .checked_add(1)
            .ok_or(ArioError::ArithmeticOverflow)?;

        // Update config supply tracking
        let config = &mut ctx.accounts.config;
        config.locked_supply = config
            .locked_supply
            .checked_add(amount)
            .ok_or(ArioError::ArithmeticOverflow)?;
        config.circulating_supply = config
            .circulating_supply
            .checked_sub(amount)
            .ok_or(ArioError::ArithmeticUnderflow)?;

        emit!(VaultCreatedEvent {
            owner: vault.owner,
            vault_id: vault.vault_id,
            amount: vault.amount,
            end_timestamp: vault.end_timestamp,
            revocable,
        });

        Ok(())
    }
}

pub mod revoke_vault {
    use super::*;

    pub fn handler(ctx: Context<RevokeVault>) -> Result<()> {
        let vault = &ctx.accounts.vault;
        let clock = Clock::get()?;

        require!(vault.revocable, ArioError::VaultNotRevocable);
        require!(
            vault.controller == Some(ctx.accounts.controller.key()),
            ArioError::NotVaultController
        );
        // Security: cannot revoke after vault expires — funds belong to owner
        require!(
            !vault.is_unlocked(clock.unix_timestamp),
            ArioError::VaultExpired
        );

        let amount = vault.amount;
        let vault_id = vault.vault_id;
        let owner = vault.owner;

        // C1 dust DoS defense: sweep the full live balance (principal + any
        // attacker-injected dust) so close_account's zero-balance precondition
        // is met. Protocol supply accounting still tracks `vault.amount`.
        let live_balance = ctx.accounts.vault_token_account.amount;

        // Transfer tokens back to controller using PDA signer
        let owner_bytes = owner.to_bytes();
        let vault_id_bytes = vault_id.to_le_bytes();
        let seeds = &[
            VAULT_SEED,
            owner_bytes.as_ref(),
            vault_id_bytes.as_ref(),
            &[vault.bump],
        ];
        let signer_seeds = &[&seeds[..]];

        let cpi_accounts = SplTransfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.controller_token_account.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer_seeds,
        );
        token::transfer(cpi_ctx, live_balance)?;

        // L-4: Close the vault token account, returning rent to the controller
        let close_accounts = anchor_spl::token::CloseAccount {
            account: ctx.accounts.vault_token_account.to_account_info(),
            destination: ctx.accounts.controller.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        };
        let vault_seeds: &[&[u8]] = &[
            VAULT_SEED,
            owner_bytes.as_ref(),
            vault_id_bytes.as_ref(),
            &[vault.bump],
        ];
        anchor_spl::token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            close_accounts,
            &[vault_seeds],
        ))?;

        // Update config supply tracking
        let config = &mut ctx.accounts.config;
        config.locked_supply = config
            .locked_supply
            .checked_sub(amount)
            .ok_or(ArioError::ArithmeticUnderflow)?;
        config.circulating_supply = config
            .circulating_supply
            .checked_add(amount)
            .ok_or(ArioError::ArithmeticOverflow)?;

        emit!(VaultRevokedEvent {
            owner,
            vault_id,
            amount,
            controller: ctx.accounts.controller.key(),
            timestamp: clock.unix_timestamp,
        });

        // Vault account will be closed by close constraint

        Ok(())
    }
}

pub mod extend_vault {
    use super::*;

    pub fn handler(ctx: Context<ExtendVault>, additional_seconds: i64) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        let config = &ctx.accounts.config;
        let clock = Clock::get()?;

        require!(additional_seconds > 0, ArioError::InvalidParameter);

        // Cannot extend an expired vault (exclusive: can extend at exactly end_timestamp)
        require!(
            clock.unix_timestamp <= vault.end_timestamp,
            ArioError::VaultExpired
        );

        let new_end = vault
            .end_timestamp
            .checked_add(additional_seconds)
            .ok_or(ArioError::ArithmeticOverflow)?;

        // Validate: remaining time + extension doesn't exceed max
        // (matches Lua: checks remaining duration, not total lifetime)
        let remaining_plus_extension = new_end
            .checked_sub(clock.unix_timestamp)
            .ok_or(ArioError::ArithmeticUnderflow)?;
        require!(
            remaining_plus_extension <= config.max_vault_duration,
            ArioError::LockDurationTooLong
        );

        vault.end_timestamp = new_end;

        emit!(VaultExtendedEvent {
            owner: vault.owner,
            vault_id: vault.vault_id,
            new_end_timestamp: new_end,
            timestamp: clock.unix_timestamp,
        });

        msg!("Vault {} extended to {}", vault.vault_id, new_end);
        Ok(())
    }
}

pub mod increase_vault {
    use super::*;

    pub fn handler(ctx: Context<IncreaseVault>, amount: u64) -> Result<()> {
        let vault = &mut ctx.accounts.vault;
        let clock = Clock::get()?;

        require!(amount > 0, ArioError::InvalidAmount);
        require!(
            clock.unix_timestamp <= vault.end_timestamp,
            ArioError::VaultExpired
        );

        // Transfer additional tokens to vault
        let cpi_accounts = SplTransfer {
            from: ctx.accounts.owner_token_account.to_account_info(),
            to: ctx.accounts.vault_token_account.to_account_info(),
            authority: ctx.accounts.owner.to_account_info(),
        };
        let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
        token::transfer(cpi_ctx, amount)?;

        // Update vault amount
        vault.amount = vault
            .amount
            .checked_add(amount)
            .ok_or(ArioError::ArithmeticOverflow)?;

        let new_balance = vault.amount;
        let owner = vault.owner;
        let vault_id = vault.vault_id;

        // Update config supply tracking
        let config = &mut ctx.accounts.config;
        config.locked_supply = config
            .locked_supply
            .checked_add(amount)
            .ok_or(ArioError::ArithmeticOverflow)?;
        config.circulating_supply = config
            .circulating_supply
            .checked_sub(amount)
            .ok_or(ArioError::ArithmeticUnderflow)?;

        emit!(VaultIncreasedEvent {
            owner,
            vault_id,
            added_amount: amount,
            new_balance,
            timestamp: clock.unix_timestamp,
        });

        msg!("Vault {} increased by {}", vault_id, amount);
        Ok(())
    }
}

pub mod release_vault {
    use super::*;

    pub fn handler(ctx: Context<ReleaseVault>) -> Result<()> {
        let vault = &ctx.accounts.vault;
        let clock = Clock::get()?;

        require!(
            vault.is_unlocked(clock.unix_timestamp),
            ArioError::VaultLocked
        );

        let amount = vault.amount;
        let vault_id = vault.vault_id;
        let owner = vault.owner;

        // C1 dust DoS defense: sweep the full live balance (principal + any
        // attacker-injected dust) so close_account's zero-balance precondition
        // is met. Protocol supply accounting still tracks `vault.amount`.
        let live_balance = ctx.accounts.vault_token_account.amount;

        // Transfer tokens back to owner using PDA signer
        let owner_bytes = owner.to_bytes();
        let vault_id_bytes = vault_id.to_le_bytes();
        let seeds = &[
            VAULT_SEED,
            owner_bytes.as_ref(),
            vault_id_bytes.as_ref(),
            &[vault.bump],
        ];
        let signer_seeds = &[&seeds[..]];

        let cpi_accounts = SplTransfer {
            from: ctx.accounts.vault_token_account.to_account_info(),
            to: ctx.accounts.owner_token_account.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer_seeds,
        );
        token::transfer(cpi_ctx, live_balance)?;

        // L-4: Close the vault token account, returning rent to the vault owner
        let close_accounts = anchor_spl::token::CloseAccount {
            account: ctx.accounts.vault_token_account.to_account_info(),
            destination: ctx.accounts.owner.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        };
        let vault_seeds: &[&[u8]] = &[
            VAULT_SEED,
            owner_bytes.as_ref(),
            vault_id_bytes.as_ref(),
            &[vault.bump],
        ];
        anchor_spl::token::close_account(CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            close_accounts,
            &[vault_seeds],
        ))?;

        // Update config supply tracking
        let config = &mut ctx.accounts.config;
        config.locked_supply = config
            .locked_supply
            .checked_sub(amount)
            .ok_or(ArioError::ArithmeticUnderflow)?;
        config.circulating_supply = config
            .circulating_supply
            .checked_add(amount)
            .ok_or(ArioError::ArithmeticOverflow)?;

        emit!(VaultReleasedEvent {
            owner,
            vault_id,
            amount,
            timestamp: clock.unix_timestamp,
        });

        // Vault account will be closed by close constraint

        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct CreateVault<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, ArioConfig>>,

    #[account(
        init_if_needed,
        payer = owner,
        space = VaultCounter::SIZE,
        seeds = [VAULT_COUNTER_SEED, owner.key().as_ref()],
        bump,
    )]
    pub vault_counter: Box<Account<'info, VaultCounter>>,

    #[account(
        init,
        payer = owner,
        space = Vault::SIZE,
        seeds = [VAULT_SEED, owner.key().as_ref(), &vault_counter.next_id.to_le_bytes()],
        bump,
    )]
    pub vault: Box<Account<'info, Vault>>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == config.mint,
    )]
    pub owner_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = vault_token_account.owner == vault.key() @ ArioError::InvalidAccountState,
        constraint = vault_token_account.mint == config.mint,
    )]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct VaultedTransfer<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, ArioConfig>>,

    #[account(
        init_if_needed,
        payer = sender,
        space = VaultCounter::SIZE,
        seeds = [VAULT_COUNTER_SEED, recipient.key().as_ref()],
        bump,
    )]
    pub recipient_vault_counter: Box<Account<'info, VaultCounter>>,

    #[account(
        init,
        payer = sender,
        space = Vault::SIZE,
        seeds = [VAULT_SEED, recipient.key().as_ref(), &recipient_vault_counter.next_id.to_le_bytes()],
        bump,
    )]
    pub vault: Box<Account<'info, Vault>>,

    #[account(
        mut,
        constraint = sender_token_account.owner == sender.key(),
        constraint = sender_token_account.mint == config.mint,
    )]
    pub sender_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = vault_token_account.owner == vault.key() @ ArioError::InvalidAccountState,
        constraint = vault_token_account.mint == config.mint,
    )]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: Recipient address - doesn't need to sign
    pub recipient: UncheckedAccount<'info>,

    #[account(mut)]
    pub sender: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct RevokeVault<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArioConfig>,

    #[account(
        mut,
        seeds = [VAULT_SEED, vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        close = controller,
        constraint = vault.revocable @ ArioError::VaultNotRevocable,
        constraint = vault.controller == Some(controller.key()) @ ArioError::NotVaultController,
    )]
    pub vault: Account<'info, Vault>,

    #[account(
        mut,
        constraint = vault_token_account.owner == vault.key() @ ArioError::InvalidAccountState,
        constraint = vault_token_account.mint == config.mint,
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = controller_token_account.owner == controller.key() @ ArioError::InvalidOwner,
        constraint = controller_token_account.mint == config.mint,
    )]
    pub controller_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub controller: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ExtendVault<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArioConfig>,

    #[account(
        mut,
        seeds = [VAULT_SEED, owner.key().as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        constraint = vault.owner == owner.key() @ ArioError::InvalidOwner,
    )]
    pub vault: Account<'info, Vault>,

    pub owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct IncreaseVault<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArioConfig>,

    #[account(
        mut,
        seeds = [VAULT_SEED, owner.key().as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        constraint = vault.owner == owner.key() @ ArioError::InvalidOwner,
    )]
    pub vault: Account<'info, Vault>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == config.mint,
    )]
    pub owner_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = vault_token_account.owner == vault.key() @ ArioError::InvalidAccountState,
        constraint = vault_token_account.mint == config.mint,
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub owner: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct ReleaseVault<'info> {
    #[account(
        mut,
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArioConfig>,

    #[account(
        mut,
        seeds = [VAULT_SEED, vault.owner.as_ref(), &vault.vault_id.to_le_bytes()],
        bump = vault.bump,
        close = owner,
    )]
    pub vault: Account<'info, Vault>,

    #[account(
        mut,
        constraint = vault_token_account.owner == vault.key() @ ArioError::InvalidAccountState,
        constraint = vault_token_account.mint == config.mint,
    )]
    pub vault_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = owner_token_account.owner == owner.key(),
        constraint = owner_token_account.mint == config.mint,
    )]
    pub owner_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = owner.key() == vault.owner @ ArioError::InvalidOwner,
    )]
    pub owner: Signer<'info>,

    pub token_program: Program<'info, Token>,
}
