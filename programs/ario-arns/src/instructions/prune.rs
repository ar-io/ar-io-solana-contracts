use crate::error::ArnsError;
use crate::state::*;
use crate::{NamesPrunedEvent, PRUNED_KIND_EXPIRED_LEASE, PRUNED_KIND_RETURNED};
use anchor_lang::prelude::*;

pub mod prune_expired {
    use super::*;

    /// Prune expired name leases past their grace period.
    /// This is permissionless - anyone can call it.
    /// Expired records are passed as remaining_accounts in pairs: [ArnsRecord, ReturnedName_init]
    /// Each expired ArnsRecord is closed and a ReturnedName is created.
    pub fn handler(ctx: Context<PruneExpiredNames>, max_names: u8) -> Result<()> {
        let clock = Clock::get()?;
        let config = &mut ctx.accounts.config;
        let remaining = ctx.remaining_accounts;
        let grace_period = GRACE_PERIOD_SECONDS;

        // Each record to prune is passed as a single remaining_account (the ArnsRecord PDA)
        // We close the account and refund rent to payer
        let count = std::cmp::min(max_names as usize, remaining.len());
        let mut pruned = 0u16;

        for i in 0..count {
            let record_info = &remaining[i];

            // Verify it's owned by this program
            if record_info.owner != ctx.program_id {
                continue;
            }

            // Deserialize the ArnsRecord
            let data = record_info.try_borrow_data()?;
            if data.len() < 8 {
                continue;
            }
            let mut slice: &[u8] = &data[8..];
            let record = match ArnsRecord::deserialize(&mut slice) {
                Ok(r) => r,
                Err(_) => continue,
            };
            drop(data);

            // Check if expired past grace period AND past the Dutch auction window.
            // Names that are only past the grace period must go through
            // prune_name_to_returned → Dutch auction (buy_returned_name) first.
            // This handler only cleans up records that are past both thresholds.
            let auction_window = config.return_auction_duration_seconds;
            let is_expired_past_auction = match record.purchase_type {
                PurchaseType::Permabuy => false,
                PurchaseType::Lease => match record.end_timestamp {
                    Some(end) => {
                        let direct_prune_threshold = end
                            .saturating_add(grace_period)
                            .saturating_add(auction_window);
                        clock.unix_timestamp > direct_prune_threshold
                    }
                    None => false,
                },
            };

            if !is_expired_past_auction {
                continue;
            }

            // Verify PDA derivation
            let (expected_pda, _) = Pubkey::find_program_address(
                &[ARNS_RECORD_SEED, record.name_hash.as_ref()],
                ctx.program_id,
            );
            if record_info.key() != expected_pda {
                continue;
            }

            // M5: Remove from NameRegistry (swap-remove via byte-offset
            // helper, ADR-020 dynamic-capacity layout).
            {
                let registry_info = &ctx.accounts.name_registry;
                let mut registry_data = registry_info.try_borrow_mut_data()?;
                remove_name_entry_by_hash(&mut registry_data, record.name_hash);
            }

            // Close the account: transfer lamports to payer
            let payer_info = ctx.accounts.payer.to_account_info();
            let dest_lamports = payer_info.lamports();
            let record_lamports = record_info.lamports();
            **payer_info.try_borrow_mut_lamports()? = dest_lamports
                .checked_add(record_lamports)
                .ok_or(ArnsError::ArithmeticOverflow)?;
            **record_info.try_borrow_mut_lamports()? = 0;
            // Zero out data to mark as closed
            let mut data = record_info.try_borrow_mut_data()?;
            for byte in data.iter_mut() {
                *byte = 0;
            }
            drop(data);
            // Reassign to system program so Anchor's `init` works on re-purchase
            record_info.assign(&anchor_lang::solana_program::system_program::ID);

            pruned += 1;
        }

        if pruned > 0 {
            config.total_names_registered =
                config.total_names_registered.saturating_sub(pruned as u64);
        }

        // ONE summary event per tx (never inside the loop above — log
        // truncation guard per EVENT_EMISSION_PLAN). `count = 0` is a
        // valid payload when nothing in remaining_accounts was actually
        // expired.
        emit!(NamesPrunedEvent {
            pruner: ctx.accounts.payer.key(),
            kind: PRUNED_KIND_EXPIRED_LEASE,
            count: pruned,
            timestamp: clock.unix_timestamp,
        });

        msg!("Pruned {} expired names", pruned);
        Ok(())
    }
}

pub mod prune_returned {
    use super::*;

    /// Prune returned names past their auction window.
    /// This is permissionless - anyone can call it.
    /// Expired ReturnedName accounts are passed as remaining_accounts.
    pub fn handler(ctx: Context<PruneReturnedNames>, max_names: u8) -> Result<()> {
        let clock = Clock::get()?;
        let remaining = ctx.remaining_accounts;
        let duration = RETURNED_NAME_DURATION_SECONDS;

        let count = std::cmp::min(max_names as usize, remaining.len());
        let mut pruned = 0u16;

        for i in 0..count {
            let returned_info = &remaining[i];

            if returned_info.owner != ctx.program_id {
                continue;
            }

            let data = returned_info.try_borrow_data()?;
            if data.len() < 8 {
                continue;
            }
            let mut slice: &[u8] = &data[8..];
            let returned = match ReturnedName::deserialize(&mut slice) {
                Ok(r) => r,
                Err(_) => continue,
            };
            drop(data);

            // Check if past auction duration
            let end_ts = returned.returned_at.saturating_add(duration);
            if clock.unix_timestamp < end_ts {
                continue;
            }

            // Verify PDA
            let (expected_pda, _) = Pubkey::find_program_address(
                &[RETURNED_NAME_SEED, returned.name_hash.as_ref()],
                ctx.program_id,
            );
            if returned_info.key() != expected_pda {
                continue;
            }

            // Close the account
            let payer_info = ctx.accounts.payer.to_account_info();
            let dest_lamports = payer_info.lamports();
            let returned_lamports = returned_info.lamports();
            **payer_info.try_borrow_mut_lamports()? = dest_lamports
                .checked_add(returned_lamports)
                .ok_or(ArnsError::ArithmeticOverflow)?;
            **returned_info.try_borrow_mut_lamports()? = 0;
            let mut data = returned_info.try_borrow_mut_data()?;
            for byte in data.iter_mut() {
                *byte = 0;
            }
            drop(data);
            // Reassign to system program so Anchor's `init` works on re-purchase
            returned_info.assign(&anchor_lang::solana_program::system_program::ID);

            pruned += 1;
        }

        emit!(NamesPrunedEvent {
            pruner: ctx.accounts.payer.key(),
            kind: PRUNED_KIND_RETURNED,
            count: pruned,
            timestamp: clock.unix_timestamp,
        });

        msg!("Pruned {} returned names", pruned);
        Ok(())
    }
}

pub mod prune_to_returned {
    use super::*;

    /// Prune a single expired name and create a ReturnedName for Dutch auction.
    /// This is permissionless - anyone can call it for any expired-past-grace name.
    pub fn handler(ctx: Context<PruneToReturned>) -> Result<()> {
        let clock = Clock::get()?;
        let config = &mut ctx.accounts.config;
        let record = &ctx.accounts.arns_record;

        // Verify record is expired past grace period
        let is_expired_past_grace = match record.purchase_type {
            PurchaseType::Permabuy => false,
            PurchaseType::Lease => match record.end_timestamp {
                Some(end) => clock.unix_timestamp > end.saturating_add(config.grace_period_seconds),
                None => false,
            },
        };
        require!(is_expired_past_grace, ArnsError::NameStillActive);

        let name = record.name.clone();
        let name_hash = record.name_hash;

        // Initialize returned name
        let returned = &mut ctx.accounts.returned_name;
        returned.name_hash = name_hash;
        returned.name = name.clone();
        returned.returned_at = clock.unix_timestamp;
        returned.initiator = config.key(); // Protocol-initiated (100% to protocol on buy)
        returned.bump = ctx.bumps.returned_name;

        // Update prune schedule
        let prune_ts = clock
            .unix_timestamp
            .checked_add(config.return_auction_duration_seconds)
            .ok_or(ArnsError::ArithmeticOverflow)?;
        if prune_ts < config.next_returned_names_prune_timestamp {
            config.next_returned_names_prune_timestamp = prune_ts;
        }

        config.total_names_registered = config.total_names_registered.saturating_sub(1);

        // Remove from name registry (swap-remove via byte-offset helper,
        // ADR-020 dynamic-capacity layout).
        {
            let registry_info = &ctx.accounts.name_registry;
            let mut registry_data = registry_info.try_borrow_mut_data()?;
            remove_name_entry_by_hash(&mut registry_data, name_hash);
        }

        // ArnsRecord closed by close constraint

        // Single-record prune; reuse NamesPrunedEvent with count=1 +
        // EXPIRED_LEASE kind (this is the same lifecycle stage as the
        // batch prune_expired path, just one at a time).
        emit!(NamesPrunedEvent {
            pruner: ctx.accounts.payer.key(),
            kind: PRUNED_KIND_EXPIRED_LEASE,
            count: 1,
            timestamp: clock.unix_timestamp,
        });

        msg!("Expired name '{}' pruned to returned name auction", name);
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct PruneExpiredNames<'info> {
    #[account(
        mut,
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArnsConfig>,

    /// M5: NameRegistry for removing pruned entries
    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PruneReturnedNames<'info> {
    #[account(
        mut,
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArnsConfig>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct PruneToReturned<'info> {
    #[account(
        mut,
        seeds = [ARNS_CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArnsConfig>,

    #[account(
        mut,
        seeds = [ARNS_RECORD_SEED, arns_record.name_hash.as_ref()],
        bump = arns_record.bump,
        close = payer,
    )]
    pub arns_record: Account<'info, ArnsRecord>,

    #[account(
        init,
        payer = payer,
        space = ReturnedName::SIZE,
        seeds = [RETURNED_NAME_SEED, arns_record.name_hash.as_ref()],
        bump,
    )]
    pub returned_name: Account<'info, ReturnedName>,

    /// CHECK: Variable-size NameRegistry (ADR-020 dynamic-capacity).
    /// Handler uses byte-offset helpers.
    #[account(mut, seeds = [NAME_REGISTRY_SEED], bump)]
    pub name_registry: AccountInfo<'info>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}
