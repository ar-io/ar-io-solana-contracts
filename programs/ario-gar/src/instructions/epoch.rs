use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;

use crate::error::GarError;
use crate::state::*;
use crate::{
    EpochClosedEvent, EpochCreatedEvent, EpochDurationUpdatedEvent, EpochPrescribedEvent,
    EpochWeightsTalliedEvent, EpochsToggledEvent, RATE_SCALE,
};

/// Enable/disable epoch processing.
/// GAR-007: Disabling uses a 7-day timelock. Enabling is instant (cancels any pending disable).
pub fn set_epochs_enabled(ctx: Context<UpdateEpochSettings>, enabled: bool) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let settings = &mut ctx.accounts.epoch_settings;
    if enabled {
        // Enabling is instant — also cancels any pending disable
        settings.enabled = true;
        settings.disable_at = 0;
        msg!("Epochs enabled");
    } else {
        // Disabling uses a timelock so operators can react
        let disable_at = now
            .checked_add(EpochSettings::EPOCH_DISABLE_DELAY)
            .ok_or(GarError::ArithmeticOverflow)?;
        settings.disable_at = disable_at;
        msg!("Epochs will be disabled at {}", disable_at);
    }

    // Emit on the *intent* — `enabled: true` flips state immediately,
    // `enabled: false` only schedules the disable (timelocked). Indexers
    // can pair this with the `current_epoch_index` advance/halt to derive
    // the effective state.
    emit!(EpochsToggledEvent {
        admin: ctx.accounts.authority.key(),
        enabled,
        timestamp: now,
    });

    Ok(())
}

/// Update `epoch_duration` post-init, re-anchoring `genesis_timestamp`
/// so the next-to-be-created epoch starts at `now`.
///
/// Without the re-anchor, changing `epoch_duration` would push the next
/// epoch's `start_timestamp = genesis + current_epoch_index * new_duration`
/// either far into the future (longer duration) or far into the past
/// (shorter duration), breaking `create_epoch`'s `clock >= start_timestamp`
/// check or causing the cranker to chew through a backlog of imaginary
/// stale epochs.
///
/// Re-anchor math: solve `epoch_start[current_epoch_index] == now` for
/// `genesis_timestamp`, yielding
/// `new_genesis = now - current_epoch_index * new_duration`.
///
/// Authority-only. Existing already-created Epoch PDAs are unaffected
/// (their start/end timestamps were stamped at create time). They age
/// into closability via the normal retention window as the cranker
/// advances `current_epoch_index` past `idx + 7`.
pub fn admin_set_epoch_duration(
    ctx: Context<UpdateEpochSettings>,
    new_duration: i64,
) -> Result<()> {
    require!(new_duration >= 60, GarError::InvalidParameter);

    let clock = Clock::get()?;
    let settings = &mut ctx.accounts.epoch_settings;

    let old_duration = settings.epoch_duration;
    let old_genesis = settings.genesis_timestamp;
    let current_idx = settings.current_epoch_index as i64;

    let new_genesis = clock
        .unix_timestamp
        .checked_sub(
            current_idx
                .checked_mul(new_duration)
                .ok_or(GarError::ArithmeticOverflow)?,
        )
        .ok_or(GarError::ArithmeticOverflow)?;

    settings.epoch_duration = new_duration;
    settings.genesis_timestamp = new_genesis;

    emit!(EpochDurationUpdatedEvent {
        admin: ctx.accounts.authority.key(),
        old_duration,
        new_duration,
        old_genesis_timestamp: old_genesis,
        new_genesis_timestamp: new_genesis,
        current_epoch_index: settings.current_epoch_index,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "epoch_duration {} → {} sec; genesis re-anchored {} → {} at idx={}",
        old_duration,
        new_duration,
        old_genesis,
        new_genesis,
        settings.current_epoch_index
    );

    Ok(())
}

/// Authority-gated one-shot to set `EpochSettings.current_epoch_index` to
/// a non-zero starting value, AND re-anchor `genesis_timestamp` so the
/// first `create_epoch` call fires immediately for that index.
///
/// Use case — AO → Solana cutover. AO's epoch counter doesn't carry
/// over by default (`initialize_epochs` always sets `current_epoch_index
/// = 0`). For epoch-number continuity with AO at cutover, the operator
/// calls this immediately after `initialize_epochs` and before
/// `set_epochs_enabled(true)`.
///
/// Example: AO closed at epoch 405. Solana operator calls
///   admin_set_current_epoch_index(406)
/// which sets:
///   current_epoch_index = 406
///   genesis_timestamp   = now - 406 * epoch_duration
///     (so wall-clock-derived `epoch_start` for index 406 ≈ now,
///     letting the cranker create_epoch immediately rather than
///     having to grind through 406 no-op create_epoch calls.)
///
/// Reward-decay constants (`REWARD_DECAY_START_EPOCH = 365`,
/// `REWARD_DECAY_LAST_EPOCH = 547`) are authored against AO's epoch
/// numbering — this ix preserves the decay schedule continuity.
///
/// One-shot by design:
///   - Pre-condition: `current_epoch_index == 0` (never run after the
///     counter has advanced; protects against mid-operation jumps).
///   - Pre-condition: `enabled == false` (no cranker racing this).
/// After the cutover, the counter advances only via `create_epoch`.
pub fn admin_set_current_epoch_index(
    ctx: Context<UpdateEpochSettings>,
    new_index: u64,
) -> Result<()> {
    require!(new_index > 0, GarError::InvalidParameter);
    // Loose upper bound — ~273 years at 1-day epochs; catches operator
    // typos (e.g. extra zero) without blocking any realistic AO carry-over.
    require!(new_index <= 100_000, GarError::InvalidParameter);

    let settings = &mut ctx.accounts.epoch_settings;
    require!(!settings.enabled, GarError::EpochsAlreadyEnabled);
    require!(
        settings.current_epoch_index == 0,
        GarError::EpochCounterAlreadyAdvanced
    );

    let clock = Clock::get()?;
    let old_genesis = settings.genesis_timestamp;
    let new_genesis = clock
        .unix_timestamp
        .checked_sub(
            (new_index as i64)
                .checked_mul(settings.epoch_duration)
                .ok_or(GarError::ArithmeticOverflow)?,
        )
        .ok_or(GarError::ArithmeticOverflow)?;

    settings.current_epoch_index = new_index;
    settings.genesis_timestamp = new_genesis;

    emit!(crate::EpochCounterAdvancedEvent {
        admin: ctx.accounts.authority.key(),
        new_index,
        old_genesis_timestamp: old_genesis,
        new_genesis_timestamp: new_genesis,
        epoch_duration: settings.epoch_duration,
        timestamp: clock.unix_timestamp,
    });

    msg!(
        "EpochSettings.current_epoch_index 0 → {}; genesis re-anchored {} → {} (duration={}s)",
        new_index,
        old_genesis,
        new_genesis,
        settings.epoch_duration
    );

    Ok(())
}

/// Close the EpochSettings PDA, refunding rent to the recorded authority.
///
/// Authority-only. Intended for fresh-init / authority-led recovery: closing
/// + reinitializing is the only path to overwrite the immutable init params
/// (`epoch_duration`, `prescribed_observer_count`, `prescribed_name_count`,
/// `min_observer_stake`, `slash_rate`, `tenure_weight_duration`,
/// `max_tenure_weight`) once they've been written by `initialize_epochs`.
/// `initialize_epochs` uses `init` (not `init_if_needed`), so it cannot be
/// called twice on the same PDA without a close in between.
///
/// Mid-cycle closure orphans existing Epoch PDAs and halts the cranker —
/// the existing per-epoch PDAs remain on-chain but new `create_epoch` calls
/// will fail until `initialize_epochs` runs again.
///
/// Deliberately NOT gated on `!enabled`: `set_epochs_enabled(false)` is a
/// 7-day timelock (the `enabled` flag doesn't actually flip until `disable_at`
/// elapses + the next `create_epoch` runs), which would force authority-led
/// recovery to wait a week. The close is destructive regardless of timing,
/// and the authority can already do anything else dangerous.
pub fn close_epoch_settings(_ctx: Context<CloseEpochSettings>) -> Result<()> {
    // Anchor's `close = authority` constraint on the Accounts struct
    // does the lamport transfer + zero-out.
    Ok(())
}

/// Recovery-only: close an Epoch PDA without the normal distribute /
/// retention / observation-closed gates that `close_epoch` enforces.
///
/// The use case: after a `close_epoch_settings` + `initialize_epochs`
/// reinit, `EpochSettings.current_epoch_index` resets to 0. If the
/// cluster previously cycled through Epochs 0..N before reinit, those
/// PDAs still exist on chain at seeds `[EPOCH_SEED, i.to_le_bytes()]`,
/// and the next `create_epoch` collides with the orphaned PDA at the
/// same address. Permissionless `close_epoch` cannot clean these up
/// because the orphans never finished `distribute_rewards` and don't
/// satisfy the retention gate.
///
/// **Closing an in-flight epoch (one with submitted-but-unclosed
/// Observations) orphans those Observation PDAs**, breaking
/// `close_observation`'s parent-reference invariant and leaving their
/// rent stranded. Only run this on epochs that are genuinely orphaned —
/// i.e., after a reinit, against indices that the new lifecycle won't
/// re-use.
///
/// Authority-gated AND `migration_active`-gated: after mainnet
/// `finalize_migration` flips `GatewaySettings.migration_active` to
/// false, this ix becomes inert. By design, it is migration-window
/// recovery infrastructure only.
pub fn admin_close_stale_epoch(
    _ctx: Context<AdminCloseStaleEpoch>,
    _epoch_index: u64,
) -> Result<()> {
    // Anchor's `close = authority` on the Epoch account does the rest.
    Ok(())
}

/// Create a new epoch (F23)
/// This is permissionless - anyone can call when the previous epoch has ended
pub fn create_epoch(ctx: Context<CreateEpoch>) -> Result<()> {
    let clock = Clock::get()?;
    let epoch_settings = &mut ctx.accounts.epoch_settings;

    require!(epoch_settings.enabled, GarError::EpochsNotEnabled);

    // GAR-007: Check timelock — if disable_at is set and we've passed it, disable epochs now
    if epoch_settings.disable_at > 0 && clock.unix_timestamp >= epoch_settings.disable_at {
        epoch_settings.enabled = false;
        epoch_settings.disable_at = 0;
        return Err(error!(GarError::EpochsNotEnabled));
    }

    let expected_epoch_index = epoch_settings.current_epoch_index;
    let epoch_start = epoch_settings
        .genesis_timestamp
        .checked_add(
            (expected_epoch_index as i64)
                .checked_mul(epoch_settings.epoch_duration)
                .ok_or(GarError::ArithmeticOverflow)?,
        )
        .ok_or(GarError::ArithmeticOverflow)?;

    require!(
        clock.unix_timestamp >= epoch_start,
        GarError::EpochNotStarted
    );

    // Compute reward rate for this epoch (linear decay from 0.1% to 0.05%)
    let reward_rate = compute_reward_rate(
        expected_epoch_index,
        epoch_settings.max_reward_rate,
        epoch_settings.min_reward_rate,
        epoch_settings.reward_decay_start_epoch,
        epoch_settings.reward_decay_last_epoch,
    );

    // Get active gateway count from registry
    let registry = ctx.accounts.registry.load()?;
    let active_gateways = registry.count;

    // GAR-004: Generate hashchain entropy from network-determined values only.
    // Excludes payer pubkey to prevent zero-cost keypair grinding attacks.
    // Remaining inputs (slot, epoch_index, timestamp) are set by the Solana runtime,
    // matching the Lua/AO security model where entropy comes from the L1 block hash.
    // A timing attack (cranker choosing which slot to submit in) remains possible but
    // is equivalent to the Lua version and can be mitigated post-launch via commit-reveal
    // or Switchboard VRF if manipulation is observed.
    let slot_bytes = clock.slot.to_le_bytes();
    let epoch_bytes = expected_epoch_index.to_le_bytes();
    let ts_bytes = clock.unix_timestamp.to_le_bytes();
    let mut hash_input = [0u8; 24]; // 8 + 8 + 8
    hash_input[..8].copy_from_slice(&slot_bytes);
    hash_input[8..16].copy_from_slice(&epoch_bytes);
    hash_input[16..24].copy_from_slice(&ts_bytes);
    let hashchain = anchor_lang::solana_program::hash::hash(&hash_input);

    // Read protocol balance from token account for reward calculation
    let protocol_balance = ctx.accounts.protocol_token_account.amount;
    let total_eligible = (protocol_balance as u128)
        .checked_mul(reward_rate as u128)
        .unwrap_or(0)
        .checked_div(RATE_SCALE as u128)
        .unwrap_or(0) as u64;

    // Initialize epoch (zero-copy)
    let mut epoch = ctx.accounts.epoch.load_init()?;
    epoch.epoch_index = expected_epoch_index;
    epoch.start_timestamp = epoch_start;
    epoch.end_timestamp = epoch_start
        .checked_add(epoch_settings.epoch_duration)
        .ok_or(GarError::ArithmeticOverflow)?;
    epoch.observer_count = 0;
    epoch.name_count = 0;
    epoch.observations_submitted = 0;
    epoch.observations_closed = 0;
    epoch.rewards_distributed = 0;
    epoch.total_eligible_rewards = total_eligible;
    epoch.per_gateway_reward = 0;
    epoch.per_observer_reward = 0;
    epoch.reward_rate = reward_rate;
    epoch.active_gateway_count = active_gateways;
    epoch.distribution_index = 0;
    epoch.set_total_composite_weight(0);
    epoch.tally_index = 0;
    epoch.weights_tallied = 0;
    epoch.prescriptions_done = 0;
    epoch.hashchain = hashchain.to_bytes();
    epoch.bump = ctx.bumps.epoch;
    epoch.version_bytes = [
        EPOCH_VERSION.major,
        EPOCH_VERSION.minor,
        EPOCH_VERSION.patch,
    ];

    // Increment current epoch for next creation
    epoch_settings.current_epoch_index = epoch_settings
        .current_epoch_index
        .checked_add(1)
        .ok_or(GarError::ArithmeticOverflow)?;

    emit!(EpochCreatedEvent {
        epoch_index: epoch.epoch_index,
        start_timestamp: epoch.start_timestamp,
        end_timestamp: epoch.end_timestamp,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

/// Tally weights for gateways in batches (permissionless crank).
/// Computes weights for each gateway and caches composite_weight in registry slots.
/// Accumulates total_composite_weight in the epoch.
/// Call repeatedly with batches of gateway accounts until all are processed.
pub fn tally_weights(ctx: Context<TallyWeights>, _epoch_index: u64) -> Result<()> {
    let clock = Clock::get()?;
    let settings = &ctx.accounts.settings;
    let epoch_settings = &ctx.accounts.epoch_settings;
    let mut epoch = ctx.accounts.epoch.load_mut()?;
    let mut registry = ctx.accounts.registry.load_mut()?;

    require!(epoch.weights_tallied == 0, GarError::WeightsAlreadyTallied);

    let active_count = epoch.active_gateway_count as usize;

    for account_info in ctx.remaining_accounts.iter() {
        let idx = epoch.tally_index as usize;
        if idx >= active_count {
            break;
        }

        // GAR-009: defense-in-depth skip for any registry slot whose address
        // was cleared (legacy default-zero pattern). Under the in-place
        // status flip, slots are never zeroed mid-epoch — finalize_gone is
        // the only caller that can shrink the registry, and it runs after
        // all referencing epochs have closed. Kept as belt-and-braces.
        if registry.gateways[idx].address == Pubkey::default() {
            registry.gateways[idx].composite_weight = 0;
            epoch.tally_index += 1;
            continue;
        }

        // Validate account
        require!(
            account_info.owner == ctx.program_id,
            GarError::InvalidGatewayAccount
        );

        // Deserialize gateway
        let gateway_data = account_info.try_borrow_data()?;
        let gateway = Gateway::try_deserialize(&mut &gateway_data[..])
            .map_err(|_| error!(GarError::InvalidGatewayAccount))?;

        // H-1: PDA-validate gateway account
        let (expected_pda, _) = Pubkey::find_program_address(
            &[GATEWAY_SEED, gateway.operator.as_ref()],
            ctx.program_id,
        );
        require!(
            account_info.key() == expected_pda,
            GarError::InvalidGatewayAccount
        );

        // Validate address matches registry
        require!(
            registry.gateways[idx].address == gateway.operator,
            GarError::InvalidGatewayAccount
        );

        // Skip non-Joined gateways (Leaving slots stay occupied under the
        // in-place departure model). Cache zero weight so prescribe_epoch's
        // roulette and fallback both naturally bypass the slot. distribute_epoch
        // exempts leavers from the weights_epoch freshness check, so we don't
        // need to write the gateway PDA here.
        if gateway.status != GatewayStatus::Joined {
            registry.gateways[idx].composite_weight = 0;
            drop(gateway_data);
            epoch.tally_index += 1;
            continue;
        }

        // Compute weights
        let total_stake = gateway.total_stake();
        let weights = GatewayWeights::compute(
            total_stake,
            settings.min_operator_stake,
            gateway.start_timestamp,
            clock.unix_timestamp,
            epoch_settings.tenure_weight_duration,
            epoch_settings.max_tenure_weight,
            &gateway.stats,
            RATE_SCALE,
        );

        // SHOULD-13: Exclude gateways that joined after the epoch started
        let effective_composite = if gateway.start_timestamp > epoch.start_timestamp {
            0
        } else {
            weights.composite_weight
        };

        // Cache composite weight in registry slot
        registry.gateways[idx].composite_weight = effective_composite;

        // Accumulate total
        epoch.add_composite_weight(effective_composite);

        // Write weights back to gateway (need mutable access)
        drop(gateway_data);
        {
            let mut data = account_info.try_borrow_mut_data()?;
            let mut slice: &[u8] = &data[8..];
            let mut gw =
                Gateway::deserialize(&mut slice).map_err(|_| GarError::InvalidGatewayAccount)?;
            gw.weights = weights;
            // M-5: Bind weights to this epoch so distribute_epoch can verify freshness
            gw.weights.weights_epoch = epoch.epoch_index;
            let dst = &mut data[8..];
            let mut cursor = std::io::Cursor::new(dst);
            gw.serialize(&mut cursor)
                .map_err(|_| GarError::InvalidGatewayAccount)?;
        }

        epoch.tally_index += 1;
    }

    // Capture the pre-transition flag so we can emit the summary event
    // exactly once — on the call that actually flips weights_tallied
    // from 0 → 1, never inside the loop. Avoids the per-batch log spam
    // that would otherwise risk truncation under heavy fan-out.
    let was_tallied_before = epoch.weights_tallied;
    if epoch.tally_index >= epoch.active_gateway_count {
        epoch.weights_tallied = 1;
    }
    if was_tallied_before == 0 && epoch.weights_tallied == 1 {
        let epoch_index = epoch.epoch_index;
        let gateway_count = epoch.active_gateway_count;
        let total_weight: u64 = u64::try_from(epoch.total_composite_weight()).unwrap_or(u64::MAX);
        emit!(EpochWeightsTalliedEvent {
            epoch_index,
            gateway_count,
            total_weight,
            timestamp: clock.unix_timestamp,
        });
    }

    Ok(())
}

/// Prescribe observers and names for an epoch (single tx, deterministic selection).
/// Uses weighted roulette from hashchain entropy. Computes per-unit rewards.
///
/// # `remaining_accounts` contract
///
/// `remaining_accounts` is a **lookup table for the SELECTED observers**, not
/// the full registry. It contains:
///   - one `Gateway` PDA per SELECTED observer — at most
///     `epoch_settings.prescribed_observer_count`, hard-capped at 50 by the
///     `prescribed_observers: [Pubkey; 50]` state field — in any order (each is
///     resolved by PDA, so position is not significant); and
///   - OPTIONALLY, the `NameRegistry` account as the LAST entry. It is NOT
///     required: if `remaining_accounts` is exactly the selected Gateway PDAs,
///     the name-prescription leg is skipped and the epoch is still marked
///     prescribed. When supplied it MUST be last, since the handler reads
///     `remaining_accounts.last()` for it.
///
/// So the upper bound is ~51 accounts regardless of registry size. That is well
/// under `MAX_TX_ACCOUNT_LOCKS = 64`, but 50 account keys still exceed the
/// 1232-byte transaction-size limit, so callers submit prescribe as a v0 tx
/// compressed against an Address Lookup Table (see `@ar.io/sdk`). Passing
/// **every** registry gateway (e.g. a cranker calling
/// `getAllRegistryGatewayPDAs`) blows both limits on large registries and the
/// tx is rejected at pre-flight.
///
/// The selection is computed **on-chain** here from `epoch.hashchain` and the
/// *live* `registry.gateways[0..epoch.active_gateway_count].composite_weight`.
/// Only `epoch.hashchain` is frozen (written once at `create_epoch`, see
/// `Epoch::hashchain`); the registry weights are read LIVE and CAN change after
/// `weights_tallied == 1` — a gateway that leaves zeroes its slot (see the
/// live-total rationale below). Clients mirror this exact algorithm off-chain to
/// learn which Gateway PDAs to supply, and MUST re-read the registry weights
/// when building the transaction rather than caching a tally-time snapshot. The
/// canonical client mirror is `@ar.io/sdk`'s `predictPrescribedObservers`; a
/// standalone Rust reference lives at
/// `programs/ario-gar/examples/predict_prescribed_observers.rs`.
///
/// Supplying FEWER than the selected set (e.g. a gateway left between the
/// client's read and tx landing) fails with `InvalidGatewayAccount` — the
/// caller should retry once with a fresh prediction.
pub fn prescribe_epoch(ctx: Context<PrescribeEpoch>, _epoch_index: u64) -> Result<()> {
    let epoch_settings = &ctx.accounts.epoch_settings;
    let mut epoch = ctx.accounts.epoch.load_mut()?;
    let registry = ctx.accounts.registry.load()?;

    require!(epoch.weights_tallied != 0, GarError::WeightsNotTallied);
    require!(
        epoch.prescriptions_done == 0,
        GarError::PrescriptionsAlreadyDone
    );

    let active_count = epoch.active_gateway_count as usize;

    // --- Observer selection (weighted roulette) ---
    //
    // Sample modulo the *live* sum of current registry weights, not the
    // `epoch.total_composite_weight` snapshot captured at tally. A gateway
    // that calls `leave_network` (or hits permissionless `prune_gateway`)
    // after `tally_weights` and before `prescribe_epoch` has its slot weight
    // zeroed in place but the epoch total is not decremented (see
    // `gateway.rs::leave_network`). Using the stale total opens a window
    // where `random_value` falls into the leaver's now-empty weight range,
    // and the previous fallback ("pick the last non-zero slot") collapsed
    // that range onto a single tail slot — giving any downstream gateway
    // ~`leaver_weight / total` selection share regardless of its own weight
    // (Codex 2026-05-28 finding). The live recompute eliminates the dead
    // range entirely, so no fallback is needed.
    let mut total_weight: u128 = 0;
    for i in 0..active_count {
        total_weight = total_weight.saturating_add(registry.gateways[i].composite_weight as u128);
    }

    let max_observers = std::cmp::min(
        epoch_settings.prescribed_observer_count as usize,
        active_count,
    );

    let mut hash = anchor_lang::solana_program::hash::hash(&epoch.hashchain);
    let mut selected_count = 0usize;

    if total_weight > 0 && active_count > 0 {
        // GAR-019: Use 10x multiplier to reduce chance of selecting fewer observers
        // than configured when there are many equal-weight gateways
        for _ in 0..max_observers * 10 {
            if selected_count >= max_observers {
                break;
            }

            let hash_bytes = hash.to_bytes();
            let random_value =
                u128::from_le_bytes(hash_bytes[..16].try_into().unwrap()) % total_weight;

            // Walk registry, accumulate weight. Slots with composite_weight==0
            // (Leaving gateways, late-joiners) contribute zero to the cumulative
            // sum, so the random pointer cannot land inside their range. The
            // `slot.composite_weight > 0` guard rejects any zero-weight slot
            // that a future change might surface here (audit NEW-2).
            //
            // With `total_weight` computed live above, every `random_value` in
            // `[0, total_weight)` lands inside an actual non-zero slot — there
            // is no "off the end" case to fall back from.
            let mut cumulative: u128 = 0;
            for i in 0..active_count {
                let slot = &registry.gateways[i];
                cumulative += slot.composite_weight as u128;
                if cumulative > random_value && slot.composite_weight > 0 {
                    // Check if already selected
                    if !epoch.prescribed_observer_gateways[..selected_count].contains(&slot.address)
                    {
                        epoch.prescribed_observer_gateways[selected_count] = slot.address;
                        epoch.prescribed_observers[selected_count] = slot.address;
                        selected_count += 1;
                    }
                    break;
                }
            }

            hash = anchor_lang::solana_program::hash::hash(&hash.to_bytes());
        }
    }
    epoch.observer_count = selected_count as u8;

    // --- Resolve observer addresses from remaining_accounts ---
    // GAR-003: The cranker MUST supply a valid Gateway PDA for every selected
    // observer. Each is PDA-validated, owner-checked, and cross-checked against
    // the on-chain selection so a missing or spoofed account fails the tx —
    // same security bar as the original positional design (no silent fallback
    // to operator pubkey, which may not be the observer wallet).
    //
    // Position-tolerant: gateway PDAs may appear in any slot of remaining_accounts.
    // Selection inside this handler picks observers in roulette order from the
    // hashchain, while the cranker naturally enumerates the registry in slot
    // order — the two orderings only align by chance, so requiring positional
    // match (the previous design) silently broke for any active_count > 1.
    // We search remaining_accounts by expected PDA instead. NameRegistry stays
    // at remaining.last() by convention (see name-selection block below).
    //
    // Cost: O(selected_count * remaining.len()) pubkey compares + selected_count
    // PDA derivations. With protocol caps (max 50 observers, ≤~64 accounts per
    // tx in practice), well under the 1M CU budget.
    let remaining = ctx.remaining_accounts;
    for i in 0..selected_count {
        let observer_gateway = epoch.prescribed_observer_gateways[i];

        // H-2: PDA-validate gateway account for observer resolution.
        let (expected_pda, _) = Pubkey::find_program_address(
            &[GATEWAY_SEED, observer_gateway.as_ref()],
            ctx.program_id,
        );

        let account_info = remaining
            .iter()
            .find(|a| a.key() == expected_pda)
            .ok_or(error!(GarError::InvalidGatewayAccount))?;

        require!(
            account_info.owner == ctx.program_id,
            GarError::InvalidGatewayAccount
        );

        let data = account_info.try_borrow_data()?;
        let gateway = Gateway::try_deserialize(&mut &data[..])
            .map_err(|_| error!(GarError::InvalidGatewayAccount))?;
        require!(
            gateway.operator == observer_gateway,
            GarError::InvalidGatewayAccount
        );
        epoch.prescribed_observers[i] = gateway.observer_address;
    }

    // --- Name selection ---
    // Select prescribed names from NameRegistry (always the LAST remaining_account).
    // The cranker passes gateway PDAs first, then the NameRegistry last.
    // Using `remaining.len() - 1` instead of `selected_count` ensures correct indexing
    // even when selected_count == 0 (all gateways had zero composite weight), matching
    // the Lua behaviour where name prescription is independent of observer selection.
    if !remaining.is_empty() && remaining.len() > selected_count {
        let name_reg_info = &remaining[remaining.len() - 1];

        // H-4: Validate NameRegistry account is owned by ario-arns program.
        // Read the arns program id from settings (pinned at init time, see
        // GatewaySettings::arns_program_id) so the contract has no hardcoded
        // pubkey literal that would have to be patched per deployment.
        let arns_program_id = ctx.accounts.settings.arns_program_id;
        require!(
            name_reg_info.owner == &arns_program_id,
            GarError::InvalidNameRegistry
        );

        // H-4: Validate PDA derivation
        let (expected_name_reg, _) =
            Pubkey::find_program_address(&[ARNS_NAME_REGISTRY_SEED], &arns_program_id);
        require!(
            name_reg_info.key() == expected_name_reg,
            GarError::InvalidNameRegistry
        );

        let name_data = name_reg_info.try_borrow_data()?;
        if let Some((name_count_raw, names_offset)) = read_name_registry_header(&name_data) {
            let active_names = name_count_raw as usize;
            let prescribed_count = std::cmp::min(
                epoch_settings.prescribed_name_count as usize,
                std::cmp::min(active_names, 2), // max 2 prescribed names
            );

            if active_names > 0 && prescribed_count > 0 {
                // Use hashchain-derived entropy for name selection (matches Lua)
                let mut name_hash = anchor_lang::solana_program::hash::hash(&epoch.hashchain);
                let mut names_selected = 0usize;

                for _ in 0..prescribed_count * 3 {
                    if names_selected >= prescribed_count {
                        break;
                    }

                    let hash_bytes = name_hash.to_bytes();
                    let random_idx = u64::from_le_bytes(hash_bytes[..8].try_into().unwrap())
                        as usize
                        % active_names;

                    // Linear probing (matches Lua)
                    for probe in 0..active_names {
                        let idx = (random_idx + probe) % active_names;
                        if let Some(name_hash_bytes) =
                            read_name_entry(&name_data, names_offset, idx)
                        {
                            if !epoch.prescribed_names[..names_selected].contains(&name_hash_bytes)
                            {
                                epoch.prescribed_names[names_selected] = name_hash_bytes;
                                names_selected += 1;
                                break;
                            }
                        }
                    }

                    name_hash = anchor_lang::solana_program::hash::hash(&name_hash.to_bytes());
                }
                epoch.name_count = names_selected as u8;
            }
        }
    }

    // --- Compute per-unit rewards ---
    //
    // Divide by the count of *reward-eligible* gateways (those whose
    // composite_weight was non-zero after tally) rather than the registry
    // slot count. Leaving slots and late-joiners both got composite_weight=0
    // in tally, so they're naturally excluded. This prevents reward dilution
    // when leavers occupy registry slots between leave_network and
    // finalize_gone (audit reviewer probe #6 / Lua parity with
    // `epochs.computeTotalEligibleRewardsForEpoch`).
    let mut joined_count: u64 = 0;
    for i in 0..active_count {
        if registry.gateways[i].composite_weight > 0 {
            joined_count = joined_count.saturating_add(1);
        }
    }

    if joined_count > 0 {
        let total = epoch.total_eligible_rewards;
        epoch.per_gateway_reward = (total as u128)
            .checked_mul(epoch_settings.gateway_reward_ratio as u128)
            .unwrap_or(0)
            .checked_div(RATE_SCALE as u128)
            .unwrap_or(0) as u64
            / joined_count;

        if selected_count > 0 {
            epoch.per_observer_reward = (total as u128)
                .checked_mul(epoch_settings.observer_reward_ratio as u128)
                .unwrap_or(0)
                .checked_div(RATE_SCALE as u128)
                .unwrap_or(0) as u64
                / selected_count as u64;
        }
    }

    epoch.prescriptions_done = 1;

    emit!(EpochPrescribedEvent {
        epoch_index: epoch.epoch_index,
        observer_count: epoch.observer_count,
        per_gateway_reward: epoch.per_gateway_reward,
        per_observer_reward: epoch.per_observer_reward,
        timestamp: Clock::get()?.unix_timestamp,
    });

    Ok(())
}

/// Close a fully distributed epoch account, reclaiming rent.
/// Permissionless — anyone can call once the epoch is distributed and
/// at least `epoch_retention` epochs have passed.
pub fn close_epoch(ctx: Context<CloseEpoch>, _epoch_index: u64) -> Result<()> {
    let epoch = ctx.accounts.epoch.load()?;
    let epoch_settings = &ctx.accounts.epoch_settings;

    // Must be fully distributed
    require!(epoch.rewards_distributed != 0, GarError::EpochNotCloseable);

    // Must be past retention window (current - closed >= 7 epochs minimum)
    let current = epoch_settings.current_epoch_index;
    let min_gap: u64 = 7;
    require!(
        current >= epoch.epoch_index.saturating_add(min_gap),
        GarError::EpochNotCloseable
    );

    // M8: every Observation PDA created for this epoch must be closed
    // before the parent Epoch closes. Otherwise the orphan PDAs lose
    // their parent reference (`close_observation` requires the Epoch to
    // increment `observations_closed`) and rent is permanently stranded.
    require!(
        epoch.observations_closed == epoch.observations_submitted,
        GarError::EpochObservationsNotClosed
    );

    let epoch_index = epoch.epoch_index;
    drop(epoch);

    // Capture the rent lamports BEFORE Anchor's `close = payer` constraint
    // moves them on tx-exit. The full lamport balance becomes the rent
    // refund, so this matches the post-close payer delta exactly.
    let rent_recovered = ctx.accounts.epoch.to_account_info().lamports();

    emit!(EpochClosedEvent {
        epoch_index,
        rent_recovered,
        timestamp: Clock::get()?.unix_timestamp,
    });

    // Account is closed by the close constraint in CloseEpoch context
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct UpdateEpochSettings<'info> {
    #[account(
        mut,
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
        has_one = authority @ GarError::Unauthorized,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct CloseEpochSettings<'info> {
    #[account(
        mut,
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
        has_one = authority @ GarError::Unauthorized,
        close = authority,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    #[account(mut)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(epoch_index: u64)]
pub struct AdminCloseStaleEpoch<'info> {
    /// Authority gate via `has_one` (matches `EpochSettings.authority`)
    /// + `migration_active` gate via `GatewaySettings`. After mainnet
    /// `finalize_migration` flips `migration_active` to false, this ix
    /// becomes inert.
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
        constraint = settings.migration_active @ GarError::MigrationInactive,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
        has_one = authority @ GarError::Unauthorized,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    #[account(
        mut,
        seeds = [EPOCH_SEED, &epoch_index.to_le_bytes()],
        bump,
        close = authority,
    )]
    pub epoch: AccountLoader<'info, Epoch>,

    #[account(mut)]
    pub authority: Signer<'info>,
}

#[derive(Accounts)]
pub struct CreateEpoch<'info> {
    #[account(
        mut,
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    #[account(
        init,
        payer = payer,
        space = 8 + Epoch::SIZE,
        seeds = [EPOCH_SEED, &epoch_settings.current_epoch_index.to_le_bytes()],
        bump,
    )]
    pub epoch: AccountLoader<'info, Epoch>,

    #[account(
        seeds = [REGISTRY_SEED],
        bump,
    )]
    pub registry: AccountLoader<'info, GatewayRegistry>,

    /// GAR settings PDA — used to validate protocol_token_account address
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    /// Protocol token account to read balance for reward calculation
    #[account(
        constraint = protocol_token_account.mint == settings.mint,
        constraint = protocol_token_account.key() == settings.protocol_token_account @ GarError::InvalidParameter,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub payer: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(epoch_index: u64)]
pub struct TallyWeights<'info> {
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

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
        mut,
        seeds = [REGISTRY_SEED],
        bump,
    )]
    pub registry: AccountLoader<'info, GatewayRegistry>,

    pub payer: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(epoch_index: u64)]
pub struct PrescribeEpoch<'info> {
    /// Read-only — provides `arns_program_id` for NameRegistry validation
    /// (formerly a hardcoded literal in this file). No write access required.
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

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

    pub payer: Signer<'info>,
}

/// Close a distributed epoch account, reclaiming rent.
/// The epoch's zero-copy account is closed and rent returned to payer.
#[derive(Accounts)]
#[instruction(epoch_index: u64)]
pub struct CloseEpoch<'info> {
    #[account(
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    #[account(
        mut,
        seeds = [EPOCH_SEED, &epoch_index.to_le_bytes()],
        bump,
        close = payer,
    )]
    pub epoch: AccountLoader<'info, Epoch>,

    #[account(mut)]
    pub payer: Signer<'info>,
}

#[cfg(test)]
mod prescribe_roulette_math {
    //! Pure-math tests for the `prescribe_epoch` weighted roulette. The
    //! production loop walks `registry.gateways[..active_count]` directly;
    //! this mirror lets us exhaust the random_value space against arbitrary
    //! synthetic registries without spinning up `solana-program-test`.
    //!
    //! These tests document and lock down the fix for the Codex 2026-05-28
    //! finding ("stale total + biased fallback selects later or last
    //! non-zero slots"). See the "Sample modulo the *live* sum" comment in
    //! `prescribe_epoch`.
    fn live_total(weights: &[u64]) -> u128 {
        weights
            .iter()
            .fold(0u128, |acc, &w| acc.saturating_add(w as u128))
    }

    /// Mirrors the production walk-and-sum at the top of `prescribe_epoch`'s
    /// inner hash loop. Returns `None` only when the caller passes a
    /// `random_value >= sum(weights)` (which the production code cannot do
    /// because it computes `random_value % live_total`), or when all weights
    /// are zero. The production code never reaches the previous "last
    /// non-zero" fallback because there is no dead range to fall off of.
    fn roulette_select(weights: &[u64], random_value: u128) -> Option<usize> {
        let mut cumulative: u128 = 0;
        for (i, &w) in weights.iter().enumerate() {
            cumulative = cumulative.saturating_add(w as u128);
            if cumulative > random_value && w > 0 {
                return Some(i);
            }
        }
        None
    }

    #[test]
    fn live_total_excludes_leaver_weight() {
        // Pre-tally: [small, big_leaver, accomplice] with composite [1, 100, 1].
        // Post-leave: leaver's slot zeroed in place → [1, 0, 1].
        // Pre-fix prescribe_epoch sampled `random_value % 102` (the stale
        // epoch.total_composite_weight). Post-fix it samples `random_value % 2`.
        let pre_tally = [1u64, 100, 1];
        let post_leave = [1u64, 0, 1];
        assert_eq!(live_total(&pre_tally), 102);
        assert_eq!(live_total(&post_leave), 2);
    }

    #[test]
    fn no_dead_range_post_leave() {
        // With live_total = 2, every random_value in [0, 2) lands inside an
        // actual non-zero slot — the production code's inner loop always
        // terminates with a selection. The previous "if !found" fallback
        // (epoch.rs pre-fix) was only reachable because the modulus was the
        // stale, larger total; with the fix in place, that branch becomes
        // structurally dead code and is removed.
        let post_leave = [1u64, 0, 1];
        let total = live_total(&post_leave);
        for rv in 0..total {
            let pick = roulette_select(&post_leave, rv)
                .unwrap_or_else(|| panic!("random_value {rv} fell off the end of {post_leave:?}"));
            assert_ne!(pick, 1, "leaver slot (index 1) must never be selected");
            assert!(
                post_leave[pick] > 0,
                "selected slot must have non-zero composite_weight"
            );
        }
    }

    #[test]
    fn distribution_is_uniform_post_leave() {
        // [1, 0, 1] over live_total=2: each surviving slot wins exactly half
        // the random_value space — recovering the unbiased 1:1 ratio between
        // the small1 and accomplice gateways that pre-fix would have been
        // skewed to 1:101 by the dead-range-plus-fallback bug.
        let post_leave = [1u64, 0, 1];
        let mut hits = [0usize; 3];
        for rv in 0..live_total(&post_leave) {
            hits[roulette_select(&post_leave, rv).unwrap()] += 1;
        }
        assert_eq!(hits, [1, 0, 1]);
    }

    #[test]
    fn pre_fix_dead_range_matches_codex_poc() {
        // Locks down the Codex 2026-05-28 PoC arithmetic. With weights
        // [1, 0, 1] but the *stale* modulus 102, only random_value=0 picks
        // slot 0 and only random_value=1 picks slot 2 via the main walk;
        // random_value in [2, 101] (100 values, ~98% of the space) falls
        // off the end. The pre-fix fallback attributed every one of those
        // 100 to the last non-zero slot (slot 2) — producing the 101/102
        // vs 1/102 bias. The fix eliminates the dead range, so this test
        // also documents *why* the fallback was load-bearing on the bug
        // and is now safe to delete.
        let post_leave = [1u64, 0, 1];
        let stale_total = 102u128;
        let mut main_hits = [0usize; 3];
        let mut dead_range = 0usize;
        for rv in 0..stale_total {
            match roulette_select(&post_leave, rv) {
                Some(idx) => main_hits[idx] += 1,
                None => dead_range += 1,
            }
        }
        assert_eq!(main_hits, [1, 0, 1]);
        assert_eq!(dead_range, 100);
    }

    #[test]
    fn all_zero_weights_returns_none() {
        // Outer guard `if total_weight > 0` skips the loop, but for
        // defense-in-depth the inner walk must return None rather than
        // panicking or selecting slot 0 when nothing is eligible.
        let weights = [0u64, 0, 0];
        assert_eq!(live_total(&weights), 0);
        assert_eq!(roulette_select(&weights, 0), None);
    }

    #[test]
    fn single_nonzero_slot_wins_everything() {
        // The all-but-one-left case: only slot 2 is reward-eligible, every
        // random_value in [0, 5) selects it.
        let weights = [0u64, 0, 5, 0];
        assert_eq!(live_total(&weights), 5);
        for rv in 0..live_total(&weights) {
            assert_eq!(roulette_select(&weights, rv), Some(2));
        }
    }

    #[test]
    fn leaver_at_tail_is_skipped() {
        // The pre-fix fallback walked backward from the last slot; this
        // could have re-selected a stale-tail leaver if `composite_weight`
        // weren't checked. With the fix, the fallback is gone — but the
        // forward walk's `slot.composite_weight > 0` guard still rejects a
        // zero-weight tail. Lock that down so a future refactor can't
        // regress (audit NEW-2 — fallback must not pick a leaver).
        let weights = [1u64, 1, 0];
        assert_eq!(live_total(&weights), 2);
        for rv in 0..live_total(&weights) {
            let pick = roulette_select(&weights, rv).unwrap();
            assert_ne!(pick, 2, "tail leaver (zero weight) must not be selected");
        }
    }
}
