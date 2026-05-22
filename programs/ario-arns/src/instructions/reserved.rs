use crate::error::ArnsError;
use crate::pricing::*;
use crate::state::*;
use crate::{
    NameReservedEvent, NameUnreservedEvent, NamesPrunedEvent, ReserveNameParams,
    ReservedNameClaimedEvent, PRUNED_KIND_EXPIRED_RESERVATION,
};
use anchor_lang::prelude::*;

pub mod reserve {
    use super::*;

    pub fn handler(ctx: Context<ReserveName>, params: ReserveNameParams) -> Result<()> {
        require!(
            is_valid_arns_name(&params.name),
            ArnsError::InvalidNameFormat
        );

        let clock = Clock::get()?;
        let reserved = &mut ctx.accounts.reserved_name;
        reserved.name = params.name.to_lowercase();
        reserved.reserved_for = params.reserved_for;
        reserved.expires_at = params.expires_at;
        reserved.reserved_by = ctx.accounts.authority.key();
        reserved.created_at = clock.unix_timestamp;
        reserved.bump = ctx.bumps.reserved_name;
        reserved.version = RESERVED_NAME_VERSION;

        emit!(NameReservedEvent {
            authority: ctx.accounts.authority.key(),
            name: reserved.name.clone(),
            target: params.reserved_for,
            expires_at: params.expires_at,
            timestamp: clock.unix_timestamp,
        });

        msg!("Name '{}' reserved", reserved.name);
        Ok(())
    }
}

pub mod claim {
    use super::*;

    pub fn handler(ctx: Context<ClaimReservedName>) -> Result<()> {
        let clock = Clock::get()?;
        emit!(ReservedNameClaimedEvent {
            claimer: ctx.accounts.authority.key(),
            name: ctx.accounts.reserved_name.name.clone(),
            timestamp: clock.unix_timestamp,
        });
        msg!(
            "Reserved name '{}' claimed by authority",
            ctx.accounts.reserved_name.name
        );
        // Account closed by close constraint
        Ok(())
    }
}

pub mod unreserve {
    use super::*;

    pub fn handler(ctx: Context<UnreserveName>) -> Result<()> {
        let clock = Clock::get()?;
        emit!(NameUnreservedEvent {
            authority: ctx.accounts.authority.key(),
            name: ctx.accounts.reserved_name.name.clone(),
            timestamp: clock.unix_timestamp,
        });
        msg!(
            "Reservation removed for '{}'",
            ctx.accounts.reserved_name.name
        );
        // Account closed by close constraint
        Ok(())
    }
}

/// GAP-9: Permissionless pruning of expired reserved names (matches Lua pruneReservedNames)
pub mod prune_expired_reservation {
    use super::*;

    pub fn handler(ctx: Context<PruneExpiredReservation>) -> Result<()> {
        let clock = Clock::get()?;
        let reserved = &ctx.accounts.reserved_name;

        // Require that the reservation has an expiry and that it has passed
        if let Some(expires_at) = reserved.expires_at {
            require!(
                clock.unix_timestamp >= expires_at,
                ArnsError::ReservationNotExpired
            );
        } else {
            // No expiry = permanent reservation, cannot prune
            return Err(ArnsError::ReservationNotExpired.into());
        }

        emit!(NamesPrunedEvent {
            pruner: ctx.accounts.payer.key(),
            kind: PRUNED_KIND_EXPIRED_RESERVATION,
            count: 1,
            timestamp: clock.unix_timestamp,
        });

        // Account closed by close constraint, rent returned to payer
        msg!("Expired reservation pruned for '{}'", reserved.name);
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
#[instruction(params: ReserveNameParams)]
pub struct ReserveName<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
        constraint = config.authority == authority.key() @ ArnsError::Unauthorized,
    )]
    pub config: Account<'info, ArnsConfig>,

    #[account(
        init,
        payer = authority,
        space = ReservedName::SIZE,
        seeds = [RESERVED_NAME_SEED, &crate::pricing::hash_name(&params.name)],
        bump,
    )]
    pub reserved_name: Account<'info, ReservedName>,

    #[account(mut)]
    pub authority: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct ClaimReservedName<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
        has_one = authority @ ArnsError::Unauthorized,
    )]
    pub config: Account<'info, ArnsConfig>,

    #[account(
        mut,
        seeds = [RESERVED_NAME_SEED, &crate::pricing::hash_name(&reserved_name.name)],
        bump = reserved_name.bump,
        close = authority,
    )]
    pub reserved_name: Account<'info, ReservedName>,

    #[account(mut)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct UnreserveName<'info> {
    #[account(
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
        constraint = config.authority == authority.key() @ ArnsError::Unauthorized,
    )]
    pub config: Account<'info, ArnsConfig>,

    #[account(
        mut,
        seeds = [RESERVED_NAME_SEED, &crate::pricing::hash_name(&reserved_name.name)],
        bump = reserved_name.bump,
        close = authority,
    )]
    pub reserved_name: Account<'info, ReservedName>,

    #[account(mut)]
    pub authority: Signer<'info>,
}

/// GAP-9: Permissionless pruning of expired reserved names
#[derive(Accounts)]
pub struct PruneExpiredReservation<'info> {
    #[account(
        mut,
        seeds = [RESERVED_NAME_SEED, &crate::pricing::hash_name(&reserved_name.name)],
        bump = reserved_name.bump,
        close = payer,
    )]
    pub reserved_name: Account<'info, ReservedName>,

    /// Anyone can prune expired reservations (permissionless)
    #[account(mut)]
    pub payer: Signer<'info>,
}
