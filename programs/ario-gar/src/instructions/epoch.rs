use anchor_lang::prelude::*;
use anchor_spl::token::TokenAccount;

use crate::error::GarError;
use crate::state::*;
use crate::{
    EpochClosedEvent, EpochCreatedEvent, EpochPrescribedEvent, EpochWeightsTalliedEvent,
    EpochsToggledEvent, RATE_SCALE,
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
    let total_weight = epoch.total_composite_weight();

    // --- Observer selection (weighted roulette) ---
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
            // (Leaving gateways, late-joiners, default-zeroed slots) are skipped
            // — their cumulative contribution is zero, so the random pointer
            // never lands inside their range. The eligibility filter on the
            // selection branch additionally rejects any zero-weight slot that
            // a future change might surface here (audit NEW-2).
            let mut cumulative: u128 = 0;
            let mut found = false;
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
                    found = true;
                    break;
                }
            }
            if !found && active_count > 0 {
                // Fallback: select the last slot with non-zero weight (i.e.,
                // the last reward-eligible Joined gateway). Walk backward to
                // skip Leaving slots / late-joiners that may sit at higher
                // indices (audit NEW-2 — fallback must not pick a leaver or
                // a Pubkey::default placeholder).
                for i in (0..active_count).rev() {
                    let slot = &registry.gateways[i];
                    if slot.composite_weight == 0 {
                        continue;
                    }
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
