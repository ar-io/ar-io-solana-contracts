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

    // CU-safety cap. Rolling the full state machine period-by-period costs
    // ~3k CU/period, so a multi-thousand-period gap (a long stretch with no
    // pricing tx and no `update_demand_factor` crank) would exceed the tx
    // budget. We therefore process at most one normal period and fast-forward
    // the rest in CLOSED FORM rather than skipping them:
    //
    //   * The first missed period may carry real activity from the last known
    //     period (`purchases_this_period` / `revenue_this_period`), so it runs
    //     through the full increasing/decreasing + ring-buffer state machine.
    //   * Every subsequent period is provably zero-activity (the counters are
    //     reset at the end of each period and no new activity occurs between
    //     rollovers; `is_demand_increasing` short-circuits to `false` whenever
    //     `*_this_period == 0`, independent of the ring buffer). The zero-
    //     activity evolution is therefore deterministic and periodic, so
    //     `advance_zero_activity_periods` applies every decay and permanent
    //     fee-halving in O(1) and leaves `current_period` fully current.
    //
    // This preserves the lazy-rollover invariant (state reflects `now` after
    // the call, so pricing reads and activity writes land in the right period)
    // WITHOUT the old destructive skip that discarded fee-halvings. The closed
    // form is verified byte-for-byte against the per-period loop by
    // `test_closed_form_matches_brute_force` and `test_maybe_roll_large_gap_*`.
    const MAX_ROLLOVER_PERIODS: u64 = 100;
    let periods_to_roll = current_period_for_timestamp - last_known_period;

    if periods_to_roll <= MAX_ROLLOVER_PERIODS {
        for _ in 0..periods_to_roll {
            roll_one_period(demand)?;
        }
    } else {
        // First period through the full state machine (may be non-zero
        // activity), then closed-form fast-forward of the zero-activity tail.
        roll_one_period(demand)?;
        let remaining = current_period_for_timestamp - demand.current_period;
        advance_zero_activity_periods(demand, remaining)?;
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

/// Roll the demand state forward by exactly one period: the per-period state
/// machine — demand-trend adjustment (±factor), floor clamp + permanent
/// fee-halving, ring-buffer write, counter reset, and period advance. Reads
/// `purchases_this_period` / `revenue_this_period` for the period being closed.
fn roll_one_period(demand: &mut DemandFactor) -> Result<()> {
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
    Ok(())
}

/// Effect of one ZERO-ACTIVITY period on `(factor, consecutive)`, returning the
/// new values plus whether a permanent fee-halving occurred this period. Pure;
/// does not touch fees or the ring buffer. Mirrors the not-increasing branch of
/// `roll_one_period` exactly — every zero-activity period takes that branch
/// because `is_demand_increasing` returns `false` whenever the current period
/// had no purchases/revenue, independent of the ring buffer.
fn zero_activity_step(factor: u64, consecutive: u32) -> (u64, u32, bool) {
    // factor * 0.985 while above the floor. factor ≤ DEMAND_FACTOR_SCALE here,
    // so `factor * DOWN_ADJUSTMENT` ≤ 1e6 * 985_000 < 2^50 — no u128 overflow,
    // and the result fits u64. Same value as roll_one_period's checked math.
    let mut factor = factor;
    if factor > DEMAND_FACTOR_MIN {
        factor = ((factor as u128) * (DEMAND_FACTOR_DOWN_ADJUSTMENT as u128)
            / (DEMAND_FACTOR_SCALE as u128)) as u64;
    }

    if factor <= DEMAND_FACTOR_MIN {
        if consecutive >= MAX_PERIODS_AT_MIN_DEMAND_FACTOR as u32 {
            // Permanent halving: factor resets to SCALE, consecutive to 0.
            (DEMAND_FACTOR_SCALE, 0, true)
        } else {
            (DEMAND_FACTOR_MIN, consecutive + 1, false)
        }
    } else {
        (factor, 0, false)
    }
}

/// Number of zero-activity periods in one full demand cycle starting from the
/// canonical boundary (`factor == SCALE`, `consecutive == 0`): decay to the
/// floor, dwell, then a single permanent halving that returns to the boundary.
/// Constant for fixed protocol parameters, but computed by simulation so it
/// tracks any constant changes. Bounded (~53 steps).
fn zero_activity_cycle_length() -> u64 {
    let mut factor = DEMAND_FACTOR_SCALE;
    let mut consecutive: u32 = 0;
    let mut steps: u64 = 0;
    loop {
        steps += 1;
        let (f, c, halved) = zero_activity_step(factor, consecutive);
        factor = f;
        consecutive = c;
        if halved {
            return steps;
        }
    }
}

/// Advance the demand state by `n` zero-activity periods — exactly equivalent
/// to calling `roll_one_period` `n` times with no activity, but O(1) in `n`.
///
/// PRECONDITION: `n > MOVING_AVG_PERIOD_COUNT` (guaranteed at the only call
/// site, where the gap exceeds `MAX_ROLLOVER_PERIODS`), so the trailing ring
/// buffers end fully zeroed. Verified byte-for-byte against the per-period loop
/// by `test_closed_form_matches_brute_force`.
fn advance_zero_activity_periods(demand: &mut DemandFactor, n: u64) -> Result<()> {
    if n == 0 {
        return Ok(());
    }

    // After >7 zero-activity periods every ring-buffer slot has been overwritten
    // with 0, and the in-progress counters were reset by the first period.
    demand.trailing_period_purchases = [0u64; MOVING_AVG_PERIOD_COUNT];
    demand.trailing_period_revenues = [0u64; MOVING_AVG_PERIOD_COUNT];
    demand.purchases_this_period = 0;
    demand.revenue_this_period = 0;

    let mut remaining = n;
    let mut factor = demand.current_demand_factor;
    let mut consecutive = demand.consecutive_periods_with_min_demand_factor;

    // Phase 1: step to the next canonical boundary (factor == SCALE &&
    // consecutive == 0), applying any halvings along the way. Bounded by one
    // cycle (~53 steps).
    while remaining > 0 && !(factor == DEMAND_FACTOR_SCALE && consecutive == 0) {
        let (f, c, halved) = zero_activity_step(factor, consecutive);
        factor = f;
        consecutive = c;
        if halved {
            update_fees(&mut demand.fees, DEMAND_FACTOR_MIN)?;
        }
        remaining -= 1;
    }

    if remaining > 0 {
        // Phase 2: at a canonical boundary, apply whole cycles in closed form.
        // Each cycle halves the fee table once and returns to the boundary, so
        // only the halvings need applying (factor/consecutive stay canonical).
        // Stop early once fees have floored to zero (further halvings are
        // no-ops) to keep the work bounded for very large gaps.
        let cycle_len = zero_activity_cycle_length();
        let full_cycles = remaining / cycle_len;
        remaining %= cycle_len;

        let mut applied = 0u64;
        while applied < full_cycles && demand.fees.iter().any(|&f| f != 0) {
            update_fees(&mut demand.fees, DEMAND_FACTOR_MIN)?;
            applied += 1;
        }
        // factor/consecutive remain canonical (SCALE, 0) after whole cycles.

        // Phase 3: residual partial cycle (< cycle_len → no halving completes,
        // but apply defensively if the model ever changes).
        for _ in 0..remaining {
            let (f, c, halved) = zero_activity_step(factor, consecutive);
            factor = f;
            consecutive = c;
            if halved {
                update_fees(&mut demand.fees, DEMAND_FACTOR_MIN)?;
            }
        }
    }

    demand.current_demand_factor = factor;
    demand.consecutive_periods_with_min_demand_factor = consecutive;
    demand.current_period = demand
        .current_period
        .checked_add(n)
        .ok_or(ArnsError::ArithmeticOverflow)?;
    Ok(())
}

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

    /// Assert two demand states are equal across every field the rollover
    /// mutates.
    fn assert_demand_eq(a: &DemandFactor, b: &DemandFactor, ctx: &str) {
        assert_eq!(
            a.current_demand_factor, b.current_demand_factor,
            "factor — {ctx}"
        );
        assert_eq!(
            a.consecutive_periods_with_min_demand_factor,
            b.consecutive_periods_with_min_demand_factor,
            "consecutive — {ctx}"
        );
        assert_eq!(a.fees, b.fees, "fees — {ctx}");
        assert_eq!(a.current_period, b.current_period, "period — {ctx}");
        assert_eq!(
            a.trailing_period_revenues, b.trailing_period_revenues,
            "ring revenues — {ctx}"
        );
        assert_eq!(
            a.trailing_period_purchases, b.trailing_period_purchases,
            "ring purchases — {ctx}"
        );
    }

    /// The O(1) closed-form `advance_zero_activity_periods` must be byte-for-byte
    /// identical to running the real per-period loop `n` times with no activity,
    /// for every gap size and from a variety of starting states. This is the
    /// load-bearing correctness proof — if it passes for all `n`, the fast path
    /// equals the slow path by construction.
    #[test]
    fn test_closed_form_matches_brute_force() {
        let mut halved_fees = GENESIS_FEES;
        halved_fees.iter_mut().for_each(|f| *f /= 4); // already halved twice

        let starts: Vec<(u64, u32, [u64; NUM_NAME_LENGTH_FEES])> = vec![
            (DEMAND_FACTOR_SCALE, 0, GENESIS_FEES), // canonical boundary
            (700_000, 0, GENESIS_FEES),             // mid-decay
            (DEMAND_FACTOR_MIN, 3, GENESIS_FEES),   // at floor, counting
            (DEMAND_FACTOR_SCALE, 0, halved_fees),  // fees pre-halved
        ];

        for (factor, consec, fees) in starts {
            // `n` spans many full halving cycles (cycle length ~53) so Phase 2
            // (whole-cycle halvings) is exercised, not just the edge phases.
            for n in (MOVING_AVG_PERIOD_COUNT as u64 + 1)..=600 {
                let mut brute = make_test_demand(10, 0);
                brute.current_demand_factor = factor;
                brute.consecutive_periods_with_min_demand_factor = consec;
                brute.fees = fees;
                for _ in 0..n {
                    roll_one_period(&mut brute).unwrap();
                }

                let mut closed = make_test_demand(10, 0);
                closed.current_demand_factor = factor;
                closed.consecutive_periods_with_min_demand_factor = consec;
                closed.fees = fees;
                advance_zero_activity_periods(&mut closed, n).unwrap();

                assert_demand_eq(&closed, &brute, &format!("start=({factor},{consec}) n={n}"));
            }
        }
    }

    /// A pathological gap (years of silence) must floor the fees to zero without
    /// overflow, and still match the brute-force loop.
    #[test]
    fn test_closed_form_huge_gap_floors_fees() {
        let n = 10_000u64;
        let mut brute = make_test_demand(1, 0);
        for _ in 0..n {
            roll_one_period(&mut brute).unwrap();
        }
        let mut closed = make_test_demand(1, 0);
        advance_zero_activity_periods(&mut closed, n).unwrap();
        assert_demand_eq(&closed, &brute, "huge gap n=10000");
        assert!(
            closed.fees.iter().all(|&f| f == 0),
            "fees should floor to zero after a multi-year idle gap"
        );
    }

    /// End-to-end: `maybe_roll_demand_period` (capped + closed-form) must equal
    /// an UNCAPPED per-period roll for any gap, including the first period
    /// carrying real activity. Proves the cap branch no longer discards
    /// decay/halvings.
    #[test]
    fn test_maybe_roll_large_gap_matches_full_loop() {
        let period_len = PERIOD_LENGTH_SECONDS as i64;
        // (start_period, gap, initial revenue activity on the first period)
        let cases: Vec<(u64, u64, u64)> = vec![
            (1, 101, 0), // just over the cap
            (1, 150, 0),
            (1, 300, 0),         // mirrors the Codex PoC (300 idle periods)
            (5, 300, 2_000_000), // first period has real activity
            (1, 1_000, 0),
            (3, 1_234, 500_000),
        ];

        for (start, gap, init_rev) in cases {
            let target_period = start + gap;
            let timestamp = (target_period as i64 - 1) * period_len;

            let mut reference = make_test_demand(start, 0);
            reference.revenue_this_period = init_rev;
            for _ in 0..gap {
                roll_one_period(&mut reference).unwrap();
            }

            let mut actual = make_test_demand(start, 0);
            actual.revenue_this_period = init_rev;
            maybe_roll_demand_period(&mut actual, timestamp).unwrap();

            assert_eq!(
                actual.current_period, target_period,
                "gap={gap}: must be fully current"
            );
            assert_demand_eq(&actual, &reference, &format!("gap={gap}"));
        }
    }

    /// After rolling a >100-period gap the state is BOTH fully current AND
    /// correct: a second call at the same timestamp is a no-op (nothing left to
    /// recover), and the idle gap actually applied multiple permanent halvings
    /// — unlike the old destructive skip which applied ~1.

    #[test]
    fn test_maybe_roll_large_gap_is_current_and_idempotent() {
        let period_len = PERIOD_LENGTH_SECONDS as i64;
        let target = 1 + 300;
        let ts = (target as i64 - 1) * period_len;

        let mut df = make_test_demand(1, 0);
        maybe_roll_demand_period(&mut df, ts).unwrap();
        assert_eq!(df.current_period, target);

        let factor_after = df.current_demand_factor;
        let fees_after = df.fees;

        // Second call at the same timestamp changes nothing.
        maybe_roll_demand_period(&mut df, ts).unwrap();
        assert_eq!(df.current_period, target);
        assert_eq!(df.current_demand_factor, factor_after);
        assert_eq!(df.fees, fees_after);

        // 300 idle periods → several halvings (genesis fees cut to <= 1/16),
        // not the single halving the buggy destructive cap produced.
        assert!(
            df.fees[0] <= GENESIS_FEES[0] / 16,
            "300 idle periods must apply several permanent halvings"
        );
    }
}
