use crate::error::ArnsError;
use crate::state::*;
use crate::DemandFactorUpdatedEvent;
use anchor_lang::prelude::*;

pub mod update {
    use super::*;

    /// Update demand factor for new period(s).
    /// Permissionless instruction — delegates to `maybe_roll_demand_period`.
    ///
    /// Emits `DemandFactorUpdatedEvent` ONLY when the call advanced state
    /// (period changed or fees got halved). No-op calls inside the same
    /// period intentionally don't emit — saves indexer bandwidth and
    /// avoids polluting recent-activity feeds.
    pub fn handler(ctx: Context<UpdateDemandFactor>) -> Result<()> {
        let clock = Clock::get()?;
        let demand = &mut ctx.accounts.demand_factor;

        // Snapshot enough pre-state to detect (a) period rolled,
        // (b) fee schedule got halved (the only state-mutation
        // signals callers care about).
        let pre_period = demand.current_period;
        let pre_fee_zero = demand.fees[0];

        maybe_roll_demand_period(demand, clock.unix_timestamp)?;

        let advanced = demand.current_period != pre_period;
        let fees_halved = demand.fees[0] != pre_fee_zero;

        if advanced || fees_halved {
            emit!(DemandFactorUpdatedEvent {
                caller: ctx.accounts.payer.key(),
                new_demand_factor: demand.current_demand_factor,
                period_index: demand.current_period,
                fees_halved,
                timestamp: clock.unix_timestamp,
            });
        }
        Ok(())
    }
}

/// Lazily rolls the demand factor forward if the current timestamp has crossed
/// one or more period boundaries since the last update. No-op (~160 CU) when
/// still within the same period.
///
/// Called both by the standalone `update_demand_factor` instruction and inline
/// by each pricing handler (buy_name, extend_lease, etc.) before it reads
/// `demand.current_demand_factor` or `demand.fees` — matching the Lua/AO
/// `tick()` behavior where the demand factor rolls on every incoming message.
pub fn maybe_roll_demand_period(demand: &mut DemandFactor, timestamp: i64) -> Result<()> {
    let current_period_for_timestamp =
        get_period_for_timestamp(timestamp, demand.period_zero_start_timestamp);

    // No update needed if still in same period
    if current_period_for_timestamp <= demand.current_period {
        return Ok(());
    }

    let last_known_period = demand.current_period;

    // Safety cap: if too many periods were missed, fast-forward to avoid CU
    // exhaustion. After ~46 periods of zero activity the factor hits the floor;
    // after 7 more the fees halve. 100 iterations covers ≈1.8 halving cycles
    // and costs ~300k CU — safe inside a pricing tx with a 1M budget.
    // NOTE: This cap also applies to the standalone update_demand_factor
    // instruction (which delegates here). To catch up a >100-period gap,
    // call update_demand_factor multiple times.
    const MAX_ROLLOVER_PERIODS: u64 = 100;
    let periods_to_roll = current_period_for_timestamp - last_known_period;
    let effective_start = if periods_to_roll > MAX_ROLLOVER_PERIODS {
        // Skip the distant periods (all had zero activity → ring buffer is
        // irrelevant). Reset counters so the loop starts clean.
        let skip_to = current_period_for_timestamp - MAX_ROLLOVER_PERIODS;
        demand.current_period = skip_to;
        demand.purchases_this_period = 0;
        demand.revenue_this_period = 0;
        skip_to + 1
    } else {
        last_known_period + 1
    };

    // Process each missed period.
    // NOTE: Skipped periods are intentionally treated as zero-activity periods,
    // matching the original Lua process behavior. This drives the demand factor
    // down during periods of inactivity, which is the intended economic design.
    for _period in effective_start..=current_period_for_timestamp {
        // Check if demand is increasing
        let demand_increasing = is_demand_increasing(demand);

        if demand_increasing {
            // Increase: factor * 1.05
            demand.current_demand_factor = u64::try_from(
                (demand.current_demand_factor as u128)
                    .checked_mul(DEMAND_FACTOR_UP_ADJUSTMENT as u128)
                    .ok_or(ArnsError::ArithmeticOverflow)?
                    .checked_div(DEMAND_FACTOR_SCALE as u128)
                    .ok_or(ArnsError::ArithmeticOverflow)?,
            )
            .map_err(|_| error!(ArnsError::ArithmeticOverflow))?;
        } else if demand.current_demand_factor > DEMAND_FACTOR_MIN {
            // Decrease: factor * 0.985
            demand.current_demand_factor = u64::try_from(
                (demand.current_demand_factor as u128)
                    .checked_mul(DEMAND_FACTOR_DOWN_ADJUSTMENT as u128)
                    .ok_or(ArnsError::ArithmeticOverflow)?
                    .checked_div(DEMAND_FACTOR_SCALE as u128)
                    .ok_or(ArnsError::ArithmeticOverflow)?,
            )
            .map_err(|_| error!(ArnsError::ArithmeticOverflow))?;
        }

        // Floor check
        if demand.current_demand_factor <= DEMAND_FACTOR_MIN {
            demand.current_demand_factor = DEMAND_FACTOR_MIN;

            if demand.consecutive_periods_with_min_demand_factor
                >= MAX_PERIODS_AT_MIN_DEMAND_FACTOR as u32
            {
                // Permanently halve all fees and reset
                update_fees(&mut demand.fees, DEMAND_FACTOR_MIN)?;
                demand.current_demand_factor = DEMAND_FACTOR_SCALE;
                demand.consecutive_periods_with_min_demand_factor = 0;
            } else {
                demand.consecutive_periods_with_min_demand_factor = demand
                    .consecutive_periods_with_min_demand_factor
                    .checked_add(1)
                    .ok_or(ArnsError::ArithmeticOverflow)?;
            }
        } else {
            demand.consecutive_periods_with_min_demand_factor = 0;
        }

        // Write current period data to ring buffer
        let ring_idx = (demand.current_period as usize) % MOVING_AVG_PERIOD_COUNT;
        demand.trailing_period_purchases[ring_idx] = demand.purchases_this_period;
        demand.trailing_period_revenues[ring_idx] = demand.revenue_this_period;

        // Reset counters for next period
        demand.purchases_this_period = 0;
        demand.revenue_this_period = 0;

        // Advance period
        demand.current_period = demand
            .current_period
            .checked_add(1)
            .ok_or(ArnsError::ArithmeticOverflow)?;
    }

    msg!(
        "Demand factor updated to {} (period {})",
        demand.current_demand_factor,
        demand.current_period
    );
    Ok(())
}

// =========================================
// HELPER FUNCTIONS
// =========================================

/// Get the period index for a given timestamp (1-based).
pub fn get_period_for_timestamp(timestamp: i64, period_zero_start: i64) -> u64 {
    if timestamp < period_zero_start {
        return 1;
    }
    let elapsed = (timestamp - period_zero_start) as u64;
    (elapsed / PERIOD_LENGTH_SECONDS as u64) + 1
}

/// Check if demand is increasing based on trailing averages.
/// Matches Lua: branches on settings.criteria ("revenue" or "purchases").
fn is_demand_increasing(demand: &DemandFactor) -> bool {
    if demand.criteria == DEMAND_CRITERIA_PURCHASES {
        if demand.purchases_this_period == 0 {
            return false;
        }
        let avg = mvg_avg_trailing_purchases(demand);
        demand.purchases_this_period > avg
    } else {
        // Default: revenue-based
        if demand.revenue_this_period == 0 {
            return false;
        }
        let avg = mvg_avg_trailing_revenues(demand);
        demand.revenue_this_period > avg
    }
}

/// Compute moving average of trailing period purchase counts.
fn mvg_avg_trailing_purchases(demand: &DemandFactor) -> u64 {
    let sum: u64 = demand.trailing_period_purchases.iter().sum();
    sum / MOVING_AVG_PERIOD_COUNT as u64
}

/// Compute moving average of trailing period revenues.
fn mvg_avg_trailing_revenues(demand: &DemandFactor) -> u64 {
    let sum: u64 = demand.trailing_period_revenues.iter().sum();
    sum / MOVING_AVG_PERIOD_COUNT as u64
}

/// Permanently multiply all fees by the given multiplier (scaled by DEMAND_FACTOR_SCALE).
/// When called with DEMAND_FACTOR_MIN (500_000 = 0.5), this halves all fees.
fn update_fees(fees: &mut [u64; NUM_NAME_LENGTH_FEES], multiplier: u64) -> Result<()> {
    for fee in fees.iter_mut() {
        *fee = u64::try_from(
            (*fee as u128)
                .checked_mul(multiplier as u128)
                .ok_or(ArnsError::ArithmeticOverflow)?
                .checked_div(DEMAND_FACTOR_SCALE as u128)
                .ok_or(ArnsError::ArithmeticOverflow)?,
        )
        .map_err(|_| error!(ArnsError::ArithmeticOverflow))?;
    }
    Ok(())
}

// =========================================
// ACCOUNT CONTEXT
// =========================================

#[derive(Accounts)]
pub struct UpdateDemandFactor<'info> {
    #[account(
        mut,
        seeds = [DEMAND_FACTOR_SEED],
        bump = demand_factor.bump,
    )]
    pub demand_factor: Account<'info, DemandFactor>,

    #[account(mut)]
    pub payer: Signer<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_test_demand(period: u64, period_zero_start: i64) -> DemandFactor {
        DemandFactor {
            current_demand_factor: DEMAND_FACTOR_SCALE,
            current_period: period,
            purchases_this_period: 0,
            revenue_this_period: 0,
            consecutive_periods_with_min_demand_factor: 0,
            trailing_period_purchases: [0; MOVING_AVG_PERIOD_COUNT],
            trailing_period_revenues: [0; MOVING_AVG_PERIOD_COUNT],
            fees: GENESIS_FEES,
            period_zero_start_timestamp: period_zero_start,
            criteria: DEMAND_CRITERIA_REVENUE,
            bump: 0,
            version: DEMAND_FACTOR_VERSION,
        }
    }

    #[test]
    fn test_maybe_roll_same_period_noop() {
        let mut df = make_test_demand(1, 0);
        df.purchases_this_period = 5;
        df.revenue_this_period = 1_000_000;
        let original_factor = df.current_demand_factor;
        let original_purchases = df.purchases_this_period;

        // Timestamp still in period 1 (< 86400)
        maybe_roll_demand_period(&mut df, 50_000).unwrap();

        assert_eq!(df.current_period, 1);
        assert_eq!(df.current_demand_factor, original_factor);
        assert_eq!(df.purchases_this_period, original_purchases);
    }

    #[test]
    fn test_maybe_roll_advances_one_period_no_activity() {
        let mut df = make_test_demand(1, 0);
        // No activity in period 1
        df.purchases_this_period = 0;
        df.revenue_this_period = 0;

        // Timestamp in period 2 (86401 seconds)
        maybe_roll_demand_period(&mut df, 86_401).unwrap();

        assert_eq!(df.current_period, 2);
        // Factor decreased (no activity → 0.985x)
        assert_eq!(df.current_demand_factor, DEMAND_FACTOR_DOWN_ADJUSTMENT);
        // Counters reset for new period
        assert_eq!(df.purchases_this_period, 0);
        assert_eq!(df.revenue_this_period, 0);
    }

    #[test]
    fn test_maybe_roll_with_activity_increases_factor() {
        let mut df = make_test_demand(1, 0);
        // Period 1 had revenue above trailing avg (trailing is all zeros → avg = 0)
        df.revenue_this_period = 1_000_000;
        df.purchases_this_period = 5;

        // Roll into period 2
        maybe_roll_demand_period(&mut df, 86_401).unwrap();

        assert_eq!(df.current_period, 2);
        // Factor increased (activity above avg → 1.05x)
        assert_eq!(df.current_demand_factor, DEMAND_FACTOR_UP_ADJUSTMENT);
        // Period 1 activity saved in ring buffer (index = 1 % 7 = 1)
        assert_eq!(df.trailing_period_revenues[1], 1_000_000);
        assert_eq!(df.trailing_period_purchases[1], 5);
        // Counters reset
        assert_eq!(df.purchases_this_period, 0);
        assert_eq!(df.revenue_this_period, 0);
    }
}
