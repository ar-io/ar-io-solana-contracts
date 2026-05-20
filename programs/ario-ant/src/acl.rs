//! Per-user paginated ANT ACL (ADR-012).
//!
//! Replaces the single-account `AntAcl` design from ADR-011 with a
//! paginated structure that scales without protocol-level caps:
//!
//!   - `AclConfig(user)` — head record. Holds `page_count` + `total_entries`.
//!   - `AclPage(user, page_idx)` — content-derived PDA addressed by `u64`
//!     little-endian page index. Each page holds up to
//!     `MAX_ACL_PAGE_ENTRIES` `AclEntry { asset, role }` rows.
//!
//! The SDK reads `AclConfig` once and fans out to `[0..page_count)` page
//! PDAs via `getMultipleAccountsInfo`. There is no `getProgramAccounts`
//! dependency on the read path — see `docs/ACCOUNT_SCALING_PATTERNS.md`
//! Pattern C.
//!
//! All ACL instructions are *permissionless*: anyone can pay rent to bring
//! a user's ACL into agreement with canonical state. Each instruction
//! verifies the on-chain relationship before mutating the ACL — you cannot
//! push entries for assets the user doesn't actually own/control, and you
//! cannot remove entries while the relationship is still live. Cleanup is
//! safe to run from any caller.
//!
//! Page layout policy:
//!   - `add_acl_page` allocates the next page (`page_count`). Pages are
//!     allocated lazily on demand by the SDK when the last existing page
//!     is full.
//!   - `record_acl_*` appends an entry to the supplied page. The SDK is
//!     expected to pick the first non-full page so density is preserved.
//!   - `remove_acl_*` uses `Vec::swap_remove`. Pages can become temporarily
//!     sparse but the SDK's "first non-full page" append rule fills the
//!     hole on the next write.
//!   - `close_acl_page` closes only the *last* page (`page_count - 1`) and
//!     only when it is empty, so page indices remain dense.
//!   - `close_acl_config` closes the head when `page_count == 0`.

use anchor_lang::prelude::*;

use crate::error::AntError;
use crate::state::*;
use crate::{
    read_mpl_core_owner, AclEntryAddedEvent, AclEntryRemovedEvent, ACL_ROLE_CONTROLLER,
    ACL_ROLE_OWNER, MPL_CORE_PROGRAM_ID,
};

// =========================================
// SHARED HELPERS
// =========================================

/// Verify the asset is a Metaplex Core asset and return its current owner.
pub(crate) fn read_asset_owner(asset: &AccountInfo) -> Result<Pubkey> {
    require!(asset.owner == &MPL_CORE_PROGRAM_ID, AntError::InvalidAsset);
    let data = asset.try_borrow_data()?;
    read_mpl_core_owner(&data)
}

/// Realloc an `AclPage` to fit its current entries and rebalance rent.
/// Top-up on grow, refund to `payer` on shrink.
pub(crate) fn resync_page_size<'info>(
    page: &Account<'info, AclPage>,
    payer: &Signer<'info>,
    system_program: &Program<'info, System>,
) -> Result<()> {
    let new_size = page.current_size();
    let info = page.to_account_info();
    let current = info.data_len();
    if new_size == current {
        return Ok(());
    }

    let rent = Rent::get()?;
    let target_lamports = rent.minimum_balance(new_size);
    let lamports = info.lamports();

    if new_size > current {
        let topup = target_lamports.saturating_sub(lamports);
        if topup > 0 {
            anchor_lang::system_program::transfer(
                CpiContext::new(
                    system_program.to_account_info(),
                    anchor_lang::system_program::Transfer {
                        from: payer.to_account_info(),
                        to: info.clone(),
                    },
                ),
                topup,
            )?;
        }
        info.realloc(new_size, false)?;
    } else {
        info.realloc(new_size, false)?;
        let refund = lamports.saturating_sub(target_lamports);
        if refund > 0 {
            **info.try_borrow_mut_lamports()? -= refund;
            **payer.to_account_info().try_borrow_mut_lamports()? += refund;
        }
    }
    Ok(())
}

/// Append `(asset, role)` to a page, enforcing dedup + page capacity.
pub(crate) fn page_push_unique(page: &mut AclPage, asset: Pubkey, role: u8) -> Result<()> {
    require!(
        page.position_of(&asset, role).is_none(),
        AntError::AclEntryAlreadyExists
    );
    require!(
        page.entries.len() < MAX_ACL_PAGE_ENTRIES,
        AntError::AclPageFull
    );
    page.entries.push(AclEntry { asset, role });
    Ok(())
}

/// `swap_remove` an `(asset, role)` entry from a page.
pub(crate) fn page_swap_remove(page: &mut AclPage, asset: &Pubkey, role: u8) -> Result<()> {
    let pos = page
        .position_of(asset, role)
        .ok_or(error!(AntError::AclEntryNotFound))?;
    page.entries.swap_remove(pos);
    Ok(())
}

// =========================================
// HEAD: AclConfig (per-user)
// =========================================

pub fn register_acl_config_handler(ctx: Context<RegisterAclConfig>, user: Pubkey) -> Result<()> {
    let cfg = &mut ctx.accounts.acl_config;
    cfg.user = user;
    cfg.page_count = 0;
    cfg.total_entries = 0;
    cfg.bump = ctx.bumps.acl_config;
    cfg.version = ACL_CONFIG_VERSION;
    msg!("AclConfig initialized for {}", user);
    Ok(())
}

pub fn close_acl_config_handler(ctx: Context<CloseAclConfig>) -> Result<()> {
    let cfg = &ctx.accounts.acl_config;
    require!(cfg.page_count == 0, AntError::AclConfigNotEmpty);
    msg!("AclConfig closed for {}", cfg.user);
    // Lamports refund handled by the `close = beneficiary` constraint.
    Ok(())
}

// =========================================
// PAGES: AclPage (deterministically addressed)
// =========================================

pub fn add_acl_page_handler(ctx: Context<AddAclPage>) -> Result<()> {
    let cfg = &mut ctx.accounts.acl_config;
    let page = &mut ctx.accounts.acl_page;

    page.user = cfg.user;
    page.page_idx = cfg.page_count;
    page.entries = Vec::new();
    page.bump = ctx.bumps.acl_page;
    page.version = ACL_PAGE_VERSION;

    cfg.page_count = cfg
        .page_count
        .checked_add(1)
        .expect("page_count overflowed u64 — impossible at any realistic scale");

    msg!(
        "AclPage[{}] allocated for {} (page_count={})",
        page.page_idx,
        cfg.user,
        cfg.page_count
    );
    Ok(())
}

pub fn close_acl_page_handler(ctx: Context<CloseAclPage>) -> Result<()> {
    let cfg = &mut ctx.accounts.acl_config;
    let page = &ctx.accounts.acl_page;

    require!(page.user == cfg.user, AntError::AclPageUserMismatch);
    require!(
        cfg.page_count > 0 && page.page_idx == cfg.page_count - 1,
        AntError::AclPageNotLast
    );
    require!(page.entries.is_empty(), AntError::AclPageNotEmpty);

    cfg.page_count -= 1;
    msg!(
        "AclPage[{}] closed for {} (page_count={})",
        page.page_idx,
        cfg.user,
        cfg.page_count
    );
    // Lamports refund handled by the `close = beneficiary` constraint.
    Ok(())
}

// =========================================
// ENTRIES: record / remove
// =========================================

/// Common preflight: validate that the supplied `AclPage` belongs to this
/// user and is in-range relative to `AclConfig.page_count`.
pub(crate) fn assert_page_belongs(cfg: &AclConfig, page: &AclPage) -> Result<()> {
    require!(page.user == cfg.user, AntError::AclPageUserMismatch);
    require!(page.page_idx < cfg.page_count, AntError::AclPageOutOfBounds);
    Ok(())
}

pub fn record_acl_owner_handler(ctx: Context<RecordAclOwner>) -> Result<()> {
    let nft_owner = read_asset_owner(&ctx.accounts.asset)?;
    require!(
        nft_owner == ctx.accounts.acl_config.user,
        AntError::NotCurrentOwner
    );
    assert_page_belongs(&ctx.accounts.acl_config, &ctx.accounts.acl_page)?;

    let asset_key = ctx.accounts.asset.key();
    page_push_unique(&mut ctx.accounts.acl_page, asset_key, AclRole::Owner as u8)?;
    resync_page_size(
        &ctx.accounts.acl_page,
        &ctx.accounts.payer,
        &ctx.accounts.system_program,
    )?;

    let cfg = &mut ctx.accounts.acl_config;
    cfg.total_entries = cfg.total_entries.saturating_add(1);

    emit!(AclEntryAddedEvent {
        mint: asset_key,
        address: cfg.user,
        role: ACL_ROLE_OWNER,
        timestamp: Clock::get()?.unix_timestamp,
    });

    msg!(
        "ACL[+owner]: user={} asset={} page={}",
        cfg.user,
        asset_key,
        ctx.accounts.acl_page.page_idx
    );
    Ok(())
}

pub fn record_acl_controller_handler(ctx: Context<RecordAclController>) -> Result<()> {
    require!(
        ctx.accounts
            .ant_controllers
            .controllers
            .contains(&ctx.accounts.acl_config.user),
        AntError::NotCurrentController
    );
    assert_page_belongs(&ctx.accounts.acl_config, &ctx.accounts.acl_page)?;

    let asset_key = ctx.accounts.asset.key();
    page_push_unique(
        &mut ctx.accounts.acl_page,
        asset_key,
        AclRole::Controller as u8,
    )?;
    resync_page_size(
        &ctx.accounts.acl_page,
        &ctx.accounts.payer,
        &ctx.accounts.system_program,
    )?;

    let cfg = &mut ctx.accounts.acl_config;
    cfg.total_entries = cfg.total_entries.saturating_add(1);

    emit!(AclEntryAddedEvent {
        mint: asset_key,
        address: cfg.user,
        role: ACL_ROLE_CONTROLLER,
        timestamp: Clock::get()?.unix_timestamp,
    });

    msg!(
        "ACL[+controller]: user={} asset={} page={}",
        cfg.user,
        asset_key,
        ctx.accounts.acl_page.page_idx
    );
    Ok(())
}

pub fn remove_acl_owner_handler(ctx: Context<RecordAclOwner>) -> Result<()> {
    let nft_owner = read_asset_owner(&ctx.accounts.asset)?;
    require!(
        nft_owner != ctx.accounts.acl_config.user,
        AntError::StillCurrentOwner
    );
    assert_page_belongs(&ctx.accounts.acl_config, &ctx.accounts.acl_page)?;

    let asset_key = ctx.accounts.asset.key();
    page_swap_remove(&mut ctx.accounts.acl_page, &asset_key, AclRole::Owner as u8)?;
    // Note: we intentionally do NOT realloc/shrink the page on remove.
    // Pages can stay sparse until they are appended to again — keeping the
    // high-water-mark allocation avoids per-remove rent churn (refund + top
    // up dance) and the SDK's "append to first non-full page" rule fills
    // holes before allocating new pages.

    let cfg = &mut ctx.accounts.acl_config;
    cfg.total_entries = cfg.total_entries.saturating_sub(1);

    emit!(AclEntryRemovedEvent {
        mint: asset_key,
        address: cfg.user,
        role: ACL_ROLE_OWNER,
        timestamp: Clock::get()?.unix_timestamp,
    });

    msg!(
        "ACL[-owner]: user={} asset={} page={}",
        cfg.user,
        asset_key,
        ctx.accounts.acl_page.page_idx
    );
    Ok(())
}

pub fn remove_acl_controller_handler(ctx: Context<RecordAclController>) -> Result<()> {
    require!(
        !ctx.accounts
            .ant_controllers
            .controllers
            .contains(&ctx.accounts.acl_config.user),
        AntError::StillCurrentController
    );
    assert_page_belongs(&ctx.accounts.acl_config, &ctx.accounts.acl_page)?;

    let asset_key = ctx.accounts.asset.key();
    page_swap_remove(
        &mut ctx.accounts.acl_page,
        &asset_key,
        AclRole::Controller as u8,
    )?;

    let cfg = &mut ctx.accounts.acl_config;
    cfg.total_entries = cfg.total_entries.saturating_sub(1);

    emit!(AclEntryRemovedEvent {
        mint: asset_key,
        address: cfg.user,
        role: ACL_ROLE_CONTROLLER,
        timestamp: Clock::get()?.unix_timestamp,
    });

    msg!(
        "ACL[-controller]: user={} asset={} page={}",
        cfg.user,
        asset_key,
        ctx.accounts.acl_page.page_idx
    );
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
#[instruction(user: Pubkey)]
pub struct RegisterAclConfig<'info> {
    #[account(
        init,
        payer = payer,
        space = AclConfig::SIZE,
        seeds = [ACL_CONFIG_SEED, user.as_ref()],
        bump,
    )]
    pub acl_config: Account<'info, AclConfig>,

    /// Anyone can pay to register an ACL config for any user (permissionless).
    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct CloseAclConfig<'info> {
    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, acl_config.user.as_ref()],
        bump = acl_config.bump,
        close = beneficiary,
    )]
    pub acl_config: Account<'info, AclConfig>,

    /// Receives the rent refund. Must equal `acl_config.user` so funds
    /// always flow back to the wallet that owns the relationship.
    /// CHECK: pubkey-only; lamports flow here via the `close` constraint.
    #[account(
        mut,
        constraint = beneficiary.key() == acl_config.user @ AntError::AclConfigUserMismatch,
    )]
    pub beneficiary: AccountInfo<'info>,
}

/// Allocate the next `AclPage` for a user (`page_idx == page_count`).
#[derive(Accounts)]
pub struct AddAclPage<'info> {
    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, acl_config.user.as_ref()],
        bump = acl_config.bump,
    )]
    pub acl_config: Account<'info, AclConfig>,

    #[account(
        init,
        payer = payer,
        space = AclPage::MIN_SIZE,
        seeds = [
            ACL_PAGE_SEED,
            acl_config.user.as_ref(),
            &acl_config.page_count.to_le_bytes(),
        ],
        bump,
    )]
    pub acl_page: Account<'info, AclPage>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

/// Close the last `AclPage` (`page_idx == page_count - 1`) when empty.
#[derive(Accounts)]
pub struct CloseAclPage<'info> {
    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, acl_config.user.as_ref()],
        bump = acl_config.bump,
    )]
    pub acl_config: Account<'info, AclConfig>,

    #[account(
        mut,
        seeds = [
            ACL_PAGE_SEED,
            acl_config.user.as_ref(),
            &acl_page.page_idx.to_le_bytes(),
        ],
        bump = acl_page.bump,
        close = beneficiary,
    )]
    pub acl_page: Account<'info, AclPage>,

    /// Receives the rent refund. Must equal `acl_config.user`.
    /// CHECK: pubkey-only; lamports flow here via the `close` constraint.
    #[account(
        mut,
        constraint = beneficiary.key() == acl_config.user @ AntError::AclConfigUserMismatch,
    )]
    pub beneficiary: AccountInfo<'info>,
}

/// Owner record/remove uses the asset's MPL Core account to verify
/// ownership.
#[derive(Accounts)]
pub struct RecordAclOwner<'info> {
    /// CHECK: Metaplex Core asset; ownership verified in handler via
    /// `read_asset_owner`. Owner program is checked there as well.
    pub asset: AccountInfo<'info>,

    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, acl_config.user.as_ref()],
        bump = acl_config.bump,
    )]
    pub acl_config: Account<'info, AclConfig>,

    /// Caller-supplied page. Validated against `acl_config` in handler.
    #[account(
        mut,
        seeds = [
            ACL_PAGE_SEED,
            acl_config.user.as_ref(),
            &acl_page.page_idx.to_le_bytes(),
        ],
        bump = acl_page.bump,
    )]
    pub acl_page: Account<'info, AclPage>,

    /// Pays for realloc growth on append; receives refunds the program
    /// chooses to issue (currently none — pages do not shrink on remove).
    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

/// Controller record/remove uses `AntControllers` to verify membership.
#[derive(Accounts)]
pub struct RecordAclController<'info> {
    /// CHECK: Metaplex Core asset. Only the key is recorded; ownership
    /// program is enforced via the constraint and the controllers PDA is
    /// bound to the asset's key.
    #[account(
        constraint = asset.owner == &MPL_CORE_PROGRAM_ID @ AntError::InvalidAsset,
    )]
    pub asset: AccountInfo<'info>,

    #[account(
        seeds = [ANT_CONTROLLERS_SEED, asset.key().as_ref()],
        bump = ant_controllers.bump,
    )]
    pub ant_controllers: Account<'info, AntControllers>,

    #[account(
        mut,
        seeds = [ACL_CONFIG_SEED, acl_config.user.as_ref()],
        bump = acl_config.bump,
    )]
    pub acl_config: Account<'info, AclConfig>,

    #[account(
        mut,
        seeds = [
            ACL_PAGE_SEED,
            acl_config.user.as_ref(),
            &acl_page.page_idx.to_le_bytes(),
        ],
        bump = acl_page.bump,
    )]
    pub acl_page: Account<'info, AclPage>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}
