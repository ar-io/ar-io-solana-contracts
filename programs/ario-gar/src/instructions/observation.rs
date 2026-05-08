use anchor_lang::prelude::*;

use crate::error::GarError;
use crate::state::*;
use crate::ObservationSubmittedEvent;

/// Submit observation report (F25)
pub fn save_observations(
    ctx: Context<SaveObservations>,
    _epoch_index: u64,
    gateway_results: [u8; 375],
    gateway_count: u16,
    report_tx_id: [u8; 32],
) -> Result<()> {
    let clock = Clock::get()?;
    let mut epoch = ctx.accounts.epoch.load_mut()?;

    require!(
        epoch.prescriptions_done != 0,
        GarError::PrescriptionsNotDone
    );
    require!(
        clock.unix_timestamp >= epoch.start_timestamp,
        GarError::EpochNotStarted
    );
    require!(
        clock.unix_timestamp < epoch.end_timestamp,
        GarError::EpochEnded
    );

    // M-5: Require gateway_count matches epoch's active gateway count
    require!(
        gateway_count as u32 == epoch.active_gateway_count,
        GarError::InvalidObservation
    );

    // Verify observer is prescribed
    let observer_key = ctx.accounts.observer.key();
    let observer_count = epoch.observer_count as usize;
    let observer_idx = epoch.prescribed_observers[..observer_count]
        .iter()
        .position(|o| *o == observer_key);
    require!(observer_idx.is_some(), GarError::NotPrescribedObserver);

    // Mark this observer as having submitted
    epoch.set_observed(observer_idx.unwrap());

    // Observation PDA prevents double-submission (init constraint)

    // Save observation
    let observation = &mut ctx.accounts.observation;
    observation.epoch_index = epoch.epoch_index;
    observation.observer = observer_key;
    observation.gateway_results = gateway_results;
    observation.gateway_count = gateway_count;
    observation.report_tx_id = report_tx_id;
    observation.submitted_at = clock.unix_timestamp;
    observation.bump = ctx.bumps.observation;

    // Running tally: increment failure_counts for failed gateways
    let active = epoch.active_gateway_count as usize;
    for i in 0..std::cmp::min(active, gateway_count as usize) {
        let byte_idx = i / 8;
        let bit_idx = i % 8;
        if byte_idx < 375 {
            let passed = (gateway_results[byte_idx] >> bit_idx) & 1;
            if passed == 0 {
                epoch.failure_counts[i] = epoch.failure_counts[i].saturating_add(1);
            }
        }
    }

    epoch.observations_submitted = epoch.observations_submitted.saturating_add(1);

    // Note: observed_epochs stat is updated during distribute_epoch (not here)
    // since distribute_epoch has the full picture of who was prescribed and observed.

    emit!(ObservationSubmittedEvent {
        epoch_index: epoch.epoch_index,
        observer: observation.observer,
        gateway_count,
        report_tx_id,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

/// Close an observation PDA from a fully distributed epoch, reclaiming rent.
/// Permissionless — anyone can call once the parent epoch is distributed.
/// Increments `epoch.observations_closed` so `close_epoch` can verify all
/// observation rent has been recovered before closing the parent (audit M8).
pub fn close_observation(ctx: Context<CloseObservation>, _epoch_index: u64) -> Result<()> {
    let mut epoch = ctx.accounts.epoch.load_mut()?;

    // Parent epoch must be distributed
    require!(epoch.rewards_distributed != 0, GarError::EpochNotCloseable);

    epoch.observations_closed = epoch.observations_closed.saturating_add(1);

    // Observation account is closed by the close constraint in CloseObservation context
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
#[instruction(epoch_index: u64)]
pub struct SaveObservations<'info> {
    #[account(
        mut,
        seeds = [EPOCH_SEED, &epoch_index.to_le_bytes()],
        bump,
    )]
    pub epoch: AccountLoader<'info, Epoch>,

    #[account(
        init,
        payer = observer,
        space = 8 + Observation::SIZE,
        seeds = [OBSERVATION_SEED, &epoch_index.to_le_bytes(), observer.key().as_ref()],
        bump,
    )]
    pub observation: Account<'info, Observation>,

    #[account(mut)]
    pub observer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

/// Close an observation PDA from a distributed epoch, reclaiming rent.
#[derive(Accounts)]
#[instruction(epoch_index: u64)]
pub struct CloseObservation<'info> {
    #[account(
        mut,
        seeds = [EPOCH_SEED, &epoch_index.to_le_bytes()],
        bump,
    )]
    pub epoch: AccountLoader<'info, Epoch>,

    #[account(
        mut,
        seeds = [OBSERVATION_SEED, &epoch_index.to_le_bytes(), observation.observer.as_ref()],
        bump = observation.bump,
        close = payer,
        constraint = observation.epoch_index == epoch_index @ GarError::InvalidEpochIndex,
    )]
    pub observation: Account<'info, Observation>,

    #[account(mut)]
    pub payer: Signer<'info>,
}
