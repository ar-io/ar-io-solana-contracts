use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    instruction::{AccountMeta, Instruction},
    program::invoke_signed,
};
use anchor_spl::token::{Token, TokenAccount};

use crate::error::GarError;
use crate::state::*;
use crate::{EpochDistributedEvent, ARIO_CORE_PROGRAM_ID, RATE_SCALE};

/// Per-gateway intermediate computed during distribute_epoch's first pass.
/// Holds the deserialized Gateway + the scalar per-gateway computations so
/// the second pass can scale rewards proportionally to the actual treasury
/// balance and apply both stake credit and stats updates atomically.
struct PendingDistribution<'a, 'info> {
    account_info: &'a AccountInfo<'info>,
    gateway: Gateway,
    /// Pre-scale total reward (operator share + delegate share, before
    /// shrinking to fit treasury). Zero for leavers / failed-without-observe.
    full_reward: u64,
    is_leaving: bool,
    is_prescribed: bool,
    is_observed: bool,
    failed: bool,
}

/// Distribute rewards for an epoch in batches (replaces initialize_distribution + distribute_rewards_batch + credit_gateway_rewards + credit_delegate_rewards + record_epoch_failure).
/// Processes gateway accounts from remaining_accounts. Call repeatedly until all gateways are processed.
///
/// Two-pass to fix audit M5: previously the in-memory accounting credited
/// each gateway its full reward but the SPL transfer was capped at the
/// available treasury balance. When the protocol was short, gateways' on-chain
/// stake records exceeded the actual stake_token balance — first-come-first-
/// served withdrawal griefing. The two-pass loop computes all rewards in
/// pass 1, sums them, scales each one proportionally to the actual transfer
/// amount in pass 2, then writes both stake and stats with the scaled values.
/// Accounting and tokens stay in sync.
pub fn distribute_epoch<'info>(
    ctx: Context<'_, '_, 'info, 'info, DistributeEpoch<'info>>,
    _epoch_index: u64,
) -> Result<()> {
    let clock = Clock::get()?;
    let epoch_settings = &ctx.accounts.epoch_settings;
    let mut epoch = ctx.accounts.epoch.load_mut()?;
    let registry = ctx.accounts.registry.load()?;

    require!(
        epoch.prescriptions_done != 0,
        GarError::PrescriptionsNotDone
    );
    require!(
        clock.unix_timestamp >= epoch.end_timestamp,
        GarError::EpochInProgress
    );
    require!(
        epoch.rewards_distributed == 0,
        GarError::RewardsAlreadyDistributed
    );

    let active_count = epoch.active_gateway_count as usize;
    let observations_submitted = epoch.observations_submitted;
    let per_gateway = epoch.per_gateway_reward;
    let per_observer = epoch.per_observer_reward;
    let observer_count = epoch.observer_count as usize;
    let penalty_rate = epoch_settings.missed_observation_penalty_rate;

    let mut dist_idx = epoch.distribution_index as usize;
    let mut batch_total_reward: u64 = 0;

    // ----------------------------------------------------------------------
    // Pass 1 — deserialize every gateway in this batch, validate, compute
    // its full (pre-scale) reward and observation status. Buffer everything
    // needed by pass 2; do NOT mutate gateway state yet.
    // ----------------------------------------------------------------------
    let mut pending: Vec<PendingDistribution<'_, 'info>> =
        Vec::with_capacity(ctx.remaining_accounts.len());

    for account_info in ctx.remaining_accounts.iter() {
        if dist_idx >= active_count {
            break;
        }

        require!(
            account_info.owner == ctx.program_id,
            GarError::InvalidGatewayAccount
        );
        require!(account_info.is_writable, GarError::InvalidGatewayAccount);

        // Deserialize gateway (fresh copy on the heap; we mutate it in pass 2)
        let data = account_info.try_borrow_data()?;
        let gateway = Gateway::try_deserialize(&mut &data[..])
            .map_err(|_| error!(GarError::InvalidGatewayAccount))?;
        drop(data);

        // H-1: PDA-validate gateway account
        let (expected_pda, _) = Pubkey::find_program_address(
            &[GATEWAY_SEED, gateway.operator.as_ref()],
            ctx.program_id,
        );
        require!(
            account_info.key() == expected_pda,
            GarError::InvalidGatewayAccount
        );

        // Validate it matches registry
        require!(
            registry.gateways[dist_idx].address == gateway.operator,
            GarError::InvalidGatewayAccount
        );

        // Skip leaving gateways (matches Lua: gateway.status ~= "leaving").
        // Leavers skip tally so their weights_epoch is stale; they get reward
        // 0 regardless, so the freshness check is exempted for them.
        let is_leaving = gateway.status == GatewayStatus::Leaving;
        if !is_leaving {
            require!(
                gateway.weights.weights_epoch == epoch.epoch_index,
                GarError::WeightsNotTallied
            );
        }

        // Pass/fail determination
        let failed = observations_submitted > 0
            && epoch.failure_counts[dist_idx] > (observations_submitted as u16) / 2;

        // Prescribed-observer / observed lookups
        let prescribed_idx = epoch.prescribed_observer_gateways[..observer_count]
            .iter()
            .position(|p| *p == gateway.operator);
        let is_prescribed = prescribed_idx.is_some();
        let is_observed = if let Some(pidx) = prescribed_idx {
            epoch.is_observed(pidx)
        } else {
            false
        };

        // 6-scenario reward calculation (matches Lua exactly):
        //   1. passed + prescribed + observed  → per_gateway + per_observer
        //   2. passed + prescribed + !observed → per_gateway * (1 - penalty)
        //   3. passed + !prescribed            → per_gateway
        //   4. failed + prescribed + observed  → per_observer
        //   5. failed + prescribed + !observed → 0
        //   6. failed + !prescribed            → 0
        let full_reward = if is_leaving {
            0
        } else if !failed {
            if is_prescribed {
                if is_observed {
                    per_gateway.saturating_add(per_observer)
                } else {
                    let docked = (per_gateway as u128)
                        .saturating_mul((RATE_SCALE as u128).saturating_sub(penalty_rate as u128))
                        / RATE_SCALE as u128;
                    docked as u64
                }
            } else {
                per_gateway
            }
        } else if is_prescribed && is_observed {
            per_observer
        } else {
            0
        };

        if full_reward > 0 {
            batch_total_reward = batch_total_reward
                .checked_add(full_reward)
                .ok_or(GarError::ArithmeticOverflow)?;
        }

        pending.push(PendingDistribution {
            account_info,
            gateway,
            full_reward,
            is_leaving,
            is_prescribed,
            is_observed,
            failed,
        });

        dist_idx += 1;
    }

    // ----------------------------------------------------------------------
    // Compute the actual transfer amount and the global scale factor.
    // GAR-006 + audit M5: if the protocol account was drained between
    // create_epoch and distribute_epoch, cap the SPL transfer at available
    // balance AND scale every gateway's in-memory reward by the same ratio
    // so on-chain stake records stay consistent with the SPL transfer.
    // ----------------------------------------------------------------------
    let available = ctx.accounts.protocol_token_account.amount;
    let transfer_amount = batch_total_reward.min(available);

    // ----------------------------------------------------------------------
    // Pass 2 — scale, apply, serialize.
    // ----------------------------------------------------------------------
    let mut total_operator_rewards: u64 = 0;
    for p in pending.iter_mut() {
        // Scale this gateway's reward proportionally. u128 to avoid overflow
        // on `full_reward * transfer_amount` (each up to ~5e15, product ~2.5e31).
        let scaled_reward: u64 = if batch_total_reward > 0 && p.full_reward > 0 {
            ((p.full_reward as u128).saturating_mul(transfer_amount as u128)
                / batch_total_reward as u128) as u64
        } else {
            0
        };

        if scaled_reward > 0 {
            // Re-split operator vs delegates from the SCALED reward so both
            // halves shrink in the same ratio.
            let delegate_pool = if p.gateway.settings.delegate_reward_share_ratio > 0
                && p.gateway.total_delegated_stake > 0
            {
                ((scaled_reward as u128) * (p.gateway.settings.delegate_reward_share_ratio as u128)
                    / 10_000) as u64
            } else {
                0
            };
            let operator_reward = scaled_reward.saturating_sub(delegate_pool);

            // Operator: always compounds into operator_stake for Joined gateways.
            // (auto_stake was removed — rewards always compound on Solana;
            // operators can decrease_operator_stake to withdraw.)
            if operator_reward > 0 && p.gateway.status == GatewayStatus::Joined {
                p.gateway.operator_stake = p.gateway.operator_stake.saturating_add(operator_reward);
                total_operator_rewards = total_operator_rewards.saturating_add(operator_reward);
            }

            // Delegate share via the accumulator. Both checked ops are
            // unreachable at current bounds (delegate_pool ≤ u64::MAX so
            // ×REWARD_PRECISION (1e18) tops out at ~1.84e37 ≪ u128::MAX,
            // and the outer `if` gates total_delegated_stake > 0). The `?`
            // is defense-in-depth: if a future bound regression breaks
            // either invariant, the epoch fails loudly with
            // ArithmeticOverflow instead of silently zeroing delegate
            // accruals for the batch.
            if delegate_pool > 0 && p.gateway.total_delegated_stake > 0 {
                let increment = (delegate_pool as u128)
                    .checked_mul(REWARD_PRECISION)
                    .ok_or(GarError::ArithmeticOverflow)?
                    .checked_div(p.gateway.total_delegated_stake as u128)
                    .ok_or(GarError::ArithmeticOverflow)?;
                p.gateway.cumulative_reward_per_token = p
                    .gateway
                    .cumulative_reward_per_token
                    .saturating_add(increment);
            }
        }

        // Stats update — Lua-parity skip for leaving gateways. Only ticks
        // when the gateway actually participated in this epoch.
        if !p.is_leaving {
            p.gateway.stats.total_epochs = p.gateway.stats.total_epochs.saturating_add(1);
            if !p.failed {
                p.gateway.stats.passed_epochs = p.gateway.stats.passed_epochs.saturating_add(1);
                p.gateway.stats.passed_consecutive =
                    p.gateway.stats.passed_consecutive.saturating_add(1);
                p.gateway.stats.failed_consecutive = 0;
            } else {
                p.gateway.stats.failed_epochs = p.gateway.stats.failed_epochs.saturating_add(1);
                p.gateway.stats.failed_consecutive =
                    p.gateway.stats.failed_consecutive.saturating_add(1);
                p.gateway.stats.passed_consecutive = 0;
            }
            if p.is_prescribed {
                p.gateway.stats.prescribed_epochs =
                    p.gateway.stats.prescribed_epochs.saturating_add(1);
            }
            if p.is_prescribed && p.is_observed {
                p.gateway.stats.observed_epochs = p.gateway.stats.observed_epochs.saturating_add(1);
            }
        }

        // Serialize the mutated gateway back
        let mut data = p.account_info.try_borrow_mut_data()?;
        let dst = &mut data[8..];
        let mut cursor = std::io::Cursor::new(dst);
        p.gateway
            .serialize(&mut cursor)
            .map_err(|_| GarError::InvalidGatewayAccount)?;
    }

    // ----------------------------------------------------------------------
    // Single SPL transfer for the (possibly capped) batch total. The
    // protocol-treasury SPL authority lives on `ario-core`'s ArioConfig
    // PDA (set during migration bootstrap via
    // `ario_gar::release_treasury_authority`), so we CPI into
    // `ario_core::release_treasury_to_recipient` instead of signing the
    // SPL transfer directly. The hand-rolled `invoke_signed` (vs Anchor
    // CPI) avoids a circular Cargo dep with the existing
    // `ario-core → ario-gar` cpi dep used by primary-name fund-from
    // variants.
    //
    // ario-core verifies on its side:
    //   - source == ArioConfig.treasury (pinned)
    //   - destination == gar_settings.stake_token_account (limits the
    //     blast radius even if this CPI were spoofed)
    //   - `gar_settings` is a real signer derived from ario-gar's
    //     program ID (only ario-gar code can produce this signature)
    // ----------------------------------------------------------------------
    if transfer_amount > 0 {
        let settings_bump = ctx.accounts.settings.bump;
        let signer_seeds: &[&[&[u8]]] = &[&[SETTINGS_SEED, &[settings_bump]]];

        // Anchor instruction discriminator for global:release_treasury_to_recipient.
        // Hard-coded as a manual CPI; ario-gar can't depend on ario-core
        // crate (would cycle with ario-core → ario-gar cpi dep).
        let mut data: Vec<u8> = Vec::with_capacity(8 + 8);
        data.extend_from_slice(
            &anchor_lang::solana_program::hash::hash(b"global:release_treasury_to_recipient")
                .to_bytes()[..8],
        );
        data.extend_from_slice(&transfer_amount.to_le_bytes());

        let cpi_ix = Instruction {
            program_id: ARIO_CORE_PROGRAM_ID,
            accounts: vec![
                AccountMeta::new_readonly(ctx.accounts.ario_config.key(), false),
                AccountMeta::new(ctx.accounts.protocol_token_account.key(), false),
                AccountMeta::new(ctx.accounts.stake_token_account.key(), false),
                AccountMeta::new_readonly(ctx.accounts.settings.key(), true),
                AccountMeta::new_readonly(ctx.accounts.token_program.key(), false),
            ],
            data,
        };

        invoke_signed(
            &cpi_ix,
            &[
                ctx.accounts.ario_config.to_account_info(),
                ctx.accounts.protocol_token_account.to_account_info(),
                ctx.accounts.stake_token_account.to_account_info(),
                ctx.accounts.settings.to_account_info(),
                ctx.accounts.token_program.to_account_info(),
                ctx.accounts.ario_core_program.to_account_info(),
            ],
            signer_seeds,
        )?;
    }

    if transfer_amount < batch_total_reward {
        msg!(
            "WARNING: Insufficient protocol balance. Requested {} but only {} available; pro-rated to {}.",
            batch_total_reward,
            available,
            transfer_amount
        );
    }

    epoch.distribution_index = dist_idx as u32;
    if dist_idx >= active_count {
        epoch.rewards_distributed = 1;

        emit!(EpochDistributedEvent {
            epoch_index: epoch.epoch_index,
            gateways_processed: active_count as u32,
            total_eligible_rewards: epoch.total_eligible_rewards,
            timestamp: clock.unix_timestamp,
        });
    }

    // Need to drop the epoch borrow before mutating settings
    drop(epoch);
    drop(registry);

    // Supply counter: staked increased by total rewards compounded in this batch
    if total_operator_rewards > 0 {
        let settings = &mut ctx.accounts.settings;
        settings.total_staked = settings
            .total_staked
            .checked_add(total_operator_rewards)
            .ok_or(GarError::ArithmeticOverflow)?;
    }

    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
#[instruction(epoch_index: u64)]
pub struct DistributeEpoch<'info> {
    #[account(
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    #[account(
        mut,
        seeds = [EPOCH_SEED, &epoch_index.to_le_bytes()],
        bump,
    )]
    pub epoch: AccountLoader<'info, Epoch>,

    #[account(
        seeds = [REGISTRY_SEED],
        bump,
    )]
    pub registry: AccountLoader<'info, GatewayRegistry>,

    /// GAR settings PDA — signs the CPI into `ario_core::release_treasury_to_recipient`
    /// (the SPL transfer itself is signed inside ario-core by ArioConfig).
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    /// Protocol token account (source of reward tokens). Owned by
    /// ario-core's ArioConfig PDA; ario-core signs the SPL transfer.
    /// We constrain only the address (matches the settings-pinned value)
    /// — the SPL-level owner mismatch with `settings` here is intentional
    /// since ario-core now holds treasury authority.
    #[account(
        mut,
        constraint = protocol_token_account.mint == settings.mint,
        constraint = protocol_token_account.key() == settings.protocol_token_account @ GarError::InvalidParameter,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// Stake token account (destination for staked rewards)
    #[account(
        mut,
        constraint = stake_token_account.mint == settings.mint,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Account<'info, TokenAccount>,

    /// ario-core's `ArioConfig` PDA. ario-core's
    /// `release_treasury_to_recipient` ix verifies this is the canonical
    /// config PDA (via its own `seeds = [CONFIG_SEED]` constraint).
    /// We pass it through as `AccountInfo` here to avoid importing
    /// ario-core types (would create a Cargo dep cycle).
    ///
    /// CHECK: derivation verified by `seeds::program = ARIO_CORE_PROGRAM_ID`.
    #[account(
        seeds = [b"ario_config"],
        bump,
        seeds::program = ARIO_CORE_PROGRAM_ID,
    )]
    pub ario_config: AccountInfo<'info>,

    /// CHECK: address-pinned to the canonical ario-core program ID, which
    /// is patched at build time by `build-sbf.sh --sync-from-manifest`
    /// from `program-ids/<cluster>.json`. The constraint guarantees the
    /// downstream `invoke_signed` target can't be substituted by the
    /// caller.
    #[account(address = ARIO_CORE_PROGRAM_ID)]
    pub ario_core_program: AccountInfo<'info>,

    pub payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
}
