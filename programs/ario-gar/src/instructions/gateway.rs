use anchor_lang::prelude::*;
use anchor_lang::system_program::{self as anchor_system_program, Allocate, Assign, Transfer};
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::GarError;
use crate::is_valid_arweave_id;
use crate::state::*;
use crate::{
    GatewayFinalizedEvent, GatewayJoinedEvent, GatewayLeavingEvent, GatewayPrunedEvent,
    GatewaySettingsUpdatedEvent, JoinNetworkParams, ObserverAddressUpdatedEvent,
    UpdateGatewayParams, GATEWAY_LEAVE_PERIOD, GATEWAY_SETTINGS_FIELD_ALLOW_DELEGATED_STAKING,
    GATEWAY_SETTINGS_FIELD_DELEGATE_REWARD_SHARE_RATIO, GATEWAY_SETTINGS_FIELD_FQDN,
    GATEWAY_SETTINGS_FIELD_LABEL, GATEWAY_SETTINGS_FIELD_MIN_DELEGATE_STAKE,
    GATEWAY_SETTINGS_FIELD_NOTE, GATEWAY_SETTINGS_FIELD_PORT, GATEWAY_SETTINGS_FIELD_PROPERTIES,
    GATEWAY_SETTINGS_FIELD_PROTOCOL, MAX_DELEGATE_REWARD_SHARE,
};

pub fn join_network(ctx: Context<JoinNetwork>, params: JoinNetworkParams) -> Result<()> {
    let clock = Clock::get()?;
    let settings = &ctx.accounts.settings;

    // Validate minimum stake
    require!(
        params.operator_stake >= settings.min_operator_stake,
        GarError::InsufficientStake
    );

    // Validate inputs
    require!(
        !params.label.is_empty() && params.label.len() <= 64,
        GarError::InvalidLabel
    );
    require!(
        !params.fqdn.is_empty() && params.fqdn.len() <= 128,
        GarError::InvalidFqdn
    );
    require!(
        (params.delegate_reward_share_ratio as u16) * 100 <= MAX_DELEGATE_REWARD_SHARE,
        GarError::InvalidRewardShare
    );

    // Transfer stake to program
    let cpi_accounts = SplTransfer {
        from: ctx.accounts.operator_token_account.to_account_info(),
        to: ctx.accounts.stake_token_account.to_account_info(),
        authority: ctx.accounts.operator.to_account_info(),
    };
    let cpi_ctx = CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
    token::transfer(cpi_ctx, params.operator_stake)?;

    // Initialize gateway
    let gateway = &mut ctx.accounts.gateway;
    gateway.operator = ctx.accounts.operator.key();
    gateway.label = params.label;
    gateway.fqdn = params.fqdn;
    gateway.port = params.port;
    gateway.protocol = params.protocol;
    gateway.properties = params.properties.unwrap_or_default();
    require!(
        is_valid_arweave_id(&gateway.properties),
        GarError::InvalidParameter
    );
    gateway.note = match params.note {
        Some(ref n) => {
            require!(n.len() <= 256, GarError::InvalidParameter);
            n.clone()
        }
        None => String::new(),
    };

    // BUG-5: Validate min_delegation_amount meets global floor
    let min_delegation = params
        .min_delegate_stake
        .unwrap_or(settings.min_delegate_stake);
    require!(
        min_delegation >= settings.min_delegate_stake,
        GarError::DelegationBelowMinimum
    );

    gateway.operator_stake = params.operator_stake;
    gateway.total_delegated_stake = 0;
    gateway.status = GatewayStatus::Joined;
    gateway.start_timestamp = clock.unix_timestamp;
    gateway.leave_timestamp = None;
    // Snapshotted only at leave/prune time; zero while Joined.
    gateway.leave_epoch_duration = 0;
    gateway.stats = GatewayStats::default();
    gateway.weights = GatewayWeights::default();
    gateway.settings = GatewaySettings2 {
        allow_delegated_staking: params.allow_delegated_staking,
        delegate_reward_share_ratio: params.delegate_reward_share_ratio as u16 * 100,
        min_delegation_amount: min_delegation,
        allowlist_enabled: false,
    };
    // M3: Observer address (client passes operator key for default)
    gateway.observer_address = params.observer_address;

    // SHOULD-9: Initialize observer lookup for uniqueness enforcement
    let observer_lookup = &mut ctx.accounts.observer_lookup;
    observer_lookup.gateway = ctx.accounts.operator.key();
    observer_lookup.bump = ctx.bumps.observer_lookup;
    observer_lookup.version = OBSERVER_LOOKUP_VERSION;
    gateway.cumulative_reward_per_token = 0;
    gateway.bump = ctx.bumps.gateway;
    gateway.version = GATEWAY_VERSION;

    // Add to registry
    let mut registry = ctx.accounts.registry.load_mut()?;
    require!(
        (registry.count as usize) < GatewayRegistry::MAX_GATEWAYS,
        GarError::RegistryFull
    );

    let index = registry.count;
    registry.gateways[index as usize] = GatewaySlot {
        address: ctx.accounts.operator.key(),
        composite_weight: 0,
        start_timestamp: gateway.start_timestamp,
        status: GatewaySlot::STATUS_JOINED,
        _padding: [0; 7],
    };
    registry.count = registry
        .count
        .checked_add(1)
        .ok_or(GarError::ArithmeticOverflow)?;

    // Store registry index in gateway. The byte that used to hold
    // `is_registered` is preserved as `_reserved` for layout compatibility.
    gateway.registry_index = RegistryIndex {
        index,
        _reserved: 0,
    };

    emit!(GatewayJoinedEvent {
        operator: gateway.operator,
        stake: gateway.operator_stake,
        fqdn: gateway.fqdn.clone(),
        timestamp: clock.unix_timestamp,
    });

    // Supply counter: operator stake added
    let settings = &mut ctx.accounts.settings;
    settings.total_staked = settings
        .total_staked
        .checked_add(params.operator_stake)
        .ok_or(GarError::ArithmeticOverflow)?;

    Ok(())
}

/// Leave the AR.IO network (F11)
/// Sets gateway to Leaving status. Stake splits into two vaults with
/// DIFFERENT lock periods, mirroring `gar.lua::leaveNetwork`:
///   - Protected exit vault (min portion): `GATEWAY_LEAVE_PERIOD` (90 days).
///     Uses `leaveLengthMs` in Lua via `createGatewayExitVault`.
///   - Excess vault (above-min portion): `settings.withdrawal_period`
///     (30 days default). Uses `withdrawLengthMs` in Lua via
///     `createGatewayWithdrawVault`.
/// Delegates are notified to withdraw (handled via separate delegate withdrawal txs).
pub fn leave_network<'info>(ctx: Context<'_, '_, 'info, 'info, LeaveNetwork<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let now = clock.unix_timestamp;
    // Protected exit vault gets the 90-day leave-network lock.
    let protected_available_at = now
        .checked_add(GATEWAY_LEAVE_PERIOD)
        .ok_or(GarError::ArithmeticOverflow)?;
    // Excess vault gets the shorter regular-withdrawal lock — same as if
    // the operator had used `withdraw_operator_stake` while staying joined.
    // Reading from settings (not the `WITHDRAWAL_LOCK_PERIOD` const) so the
    // `admin_set_withdrawal_period` lever applies here too.
    let excess_available_at = now
        .checked_add(ctx.accounts.settings.withdrawal_period)
        .ok_or(GarError::ArithmeticOverflow)?;
    let gateway = &mut ctx.accounts.gateway;

    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayNotJoined
    );

    gateway.status = GatewayStatus::Leaving;
    gateway.leave_timestamp = Some(now);
    // Freeze epoch_duration onto the gateway. `finalize_gone` uses this
    // (max'd against the live setting) to compute the GC eligibility window
    // so an admin shrinking epoch_duration post-leave can't retroactively
    // bring forward a leaver's GC.
    gateway.leave_epoch_duration = ctx.accounts.epoch_settings.epoch_duration;

    // Lua-faithful split (gar.lua::leaveNetwork lines 184-199):
    //   protected_amount = min(min_stake, operator_stake)
    //   excess_amount    = operator_stake - min_stake (saturating)
    // Always create the protected exit vault when something needs to vault;
    // create the excess vault only when there's a positive excess.
    let pre_stake = gateway.operator_stake;
    let min_stake = ctx.accounts.settings.min_operator_stake;
    let protected_amount = pre_stake.min(min_stake);
    let excess_amount = pre_stake.saturating_sub(min_stake);

    let counter = &mut ctx.accounts.withdrawal_counter;
    if counter.bump == 0 {
        counter.owner = ctx.accounts.operator.key();
        counter.bump = ctx.bumps.withdrawal_counter;
        counter.version = WITHDRAWAL_COUNTER_VERSION;
    }
    let exit_id = counter.next_id;
    let excess_id = exit_id.checked_add(1).ok_or(GarError::ArithmeticOverflow)?;

    // 1. Anchor-init exit vault.
    //
    // When `pre_stake == 0` (impossible from join_network's >= min check, but
    // documented for completeness), `protected_amount == 0` and we still
    // initialize a zero-amount placeholder for rent reclaim (matches the
    // `prune_gateway` L-6 behaviour). When `protected_amount > 0`, the vault
    // holds the min portion and is `is_protected: true`.
    {
        let exit = &mut ctx.accounts.withdrawal;
        exit.owner = ctx.accounts.operator.key();
        exit.withdrawal_id = exit_id;
        exit.gateway = gateway.operator;
        exit.amount = protected_amount;
        exit.created_at = now;
        exit.available_at = if protected_amount > 0 {
            protected_available_at
        } else {
            now
        };
        exit.is_delegate = false;
        exit.is_exit_vault = true;
        exit.is_protected = protected_amount > 0;
        exit.bump = ctx.bumps.withdrawal;
        exit.version = WITHDRAWAL_VERSION;
    }

    // 2. Manual create excess vault when warranted.
    if excess_amount > 0 {
        let excess_acc = ctx
            .accounts
            .excess_withdrawal
            .as_ref()
            .ok_or(GarError::MissingExcessWithdrawal)?
            .to_account_info();
        let operator_key = ctx.accounts.operator.key();
        let excess_id_bytes = excess_id.to_le_bytes();
        let (expected_pda, vault_bump) = Pubkey::find_program_address(
            &[WITHDRAWAL_SEED, operator_key.as_ref(), &excess_id_bytes],
            ctx.program_id,
        );
        require!(
            excess_acc.key() == expected_pda,
            GarError::InvalidExcessWithdrawalPda
        );
        require!(excess_acc.data_is_empty(), GarError::InvalidParameter);

        let lamports_required = Rent::get()?.minimum_balance(Withdrawal::SIZE);
        let signer_seeds: &[&[&[u8]]] = &[&[
            WITHDRAWAL_SEED,
            operator_key.as_ref(),
            &excess_id_bytes,
            &[vault_bump],
        ]];
        // Lamport-griefing defense: an attacker may pre-fund the next PDA
        // with 1 lamport; tolerate by transfer-then-allocate-then-assign
        // instead of `system_program::create_account` (which rejects on
        // pre-existing lamports).
        let existing = excess_acc.lamports();
        if existing < lamports_required {
            anchor_system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.operator.to_account_info(),
                        to: excess_acc.clone(),
                    },
                ),
                lamports_required - existing,
            )?;
        }
        anchor_system_program::allocate(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Allocate {
                    account_to_allocate: excess_acc.clone(),
                },
                signer_seeds,
            ),
            Withdrawal::SIZE as u64,
        )?;
        anchor_system_program::assign(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Assign {
                    account_to_assign: excess_acc.clone(),
                },
                signer_seeds,
            ),
            ctx.program_id,
        )?;

        let excess_state = Withdrawal {
            owner: operator_key,
            withdrawal_id: excess_id,
            gateway: gateway.operator,
            amount: excess_amount,
            created_at: now,
            available_at: excess_available_at,
            is_delegate: false,
            is_exit_vault: true,
            is_protected: false,
            bump: vault_bump,
            version: WITHDRAWAL_VERSION,
        };
        let mut data = excess_acc.try_borrow_mut_data()?;
        let mut cursor = std::io::Cursor::new(&mut data[..]);
        excess_state
            .try_serialize(&mut cursor)
            .map_err(|_| GarError::InvalidParameter)?;
    }

    // Counter advances by 2 when an excess vault was created, by 1 otherwise.
    // (We always init exit_id == counter.next_id; whether we *also* used
    // exit_id+1 determines the increment.)
    counter.next_id = if excess_amount > 0 {
        excess_id
            .checked_add(1)
            .ok_or(GarError::ArithmeticOverflow)?
    } else {
        exit_id.checked_add(1).ok_or(GarError::ArithmeticOverflow)?
    };

    // Zero out operator stake (all of it now lives in exit_vault + excess_vault).
    let gateway_stake = gateway.operator_stake;
    gateway.operator_stake = 0;

    // Mark the slot Leaving in place rather than swap-removing it. Iteration
    // sites (tally_weights, prescribe_epoch, distribute_epoch) skip Leaving
    // slots via composite_weight==0 and the status field. The slot is freed
    // later by the permissionless `finalize_gone` GC instruction once the
    // leave window expires AND all delegations are claimed.
    //
    // Why: keeping registry indices stable mid-epoch means `failure_counts[i]`
    // and observer bitmaps continue to refer to the same gateway throughout
    // an epoch. The previous swap-remove pattern silently re-attributed
    // failure tallies to whichever gateway took over the freed slot
    // (audit H2 / H3, 2026-04).
    //
    // Known limitation: `epoch.total_composite_weight` is NOT decremented
    // here. A leaver mid-epoch slightly biases prescribe_epoch's weighted
    // roulette toward later slots (probability skew ≪ 1 slot). Acceptable
    // pre-mainnet; documented for future cranker coordination.
    let mut registry = ctx.accounts.registry.load_mut()?;
    let index = gateway.registry_index.index as usize;
    require!(
        index < GatewayRegistry::MAX_GATEWAYS,
        GarError::InvalidParameter
    );
    require!((index as u32) < registry.count, GarError::InvalidParameter);
    let slot = &mut registry.gateways[index];
    require!(slot.address == gateway.operator, GarError::InvalidParameter);
    slot.composite_weight = 0;
    slot.status = GatewaySlot::STATUS_LEAVING;

    // SHOULD-9: Close observer lookup (last remaining_account)
    let remaining = ctx.remaining_accounts;
    if let Some(lookup_info) = remaining.last() {
        let (expected_pda, _) = Pubkey::find_program_address(
            &[OBSERVER_LOOKUP_SEED, gateway.observer_address.as_ref()],
            ctx.program_id,
        );
        if lookup_info.key() == expected_pda && lookup_info.is_writable {
            close_observer_lookup_account(lookup_info, &ctx.accounts.operator.to_account_info())?;
        }
    }

    emit!(GatewayLeavingEvent {
        operator: gateway.operator,
        timestamp: clock.unix_timestamp,
    });

    // Supply counter: operator stake moved to withdrawal
    let settings = &mut ctx.accounts.settings;
    settings.total_staked = settings.total_staked.saturating_sub(gateway_stake);
    settings.total_withdrawn = settings
        .total_withdrawn
        .checked_add(gateway_stake)
        .ok_or(GarError::ArithmeticOverflow)?;

    Ok(())
}

/// Update gateway settings (F12)
pub fn update_gateway_settings(
    ctx: Context<UpdateGatewaySettings>,
    params: UpdateGatewayParams,
) -> Result<()> {
    let gateway = &mut ctx.accounts.gateway;

    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayLeaving
    );

    // Build a u32 bitmask of which fields the caller actually mutated.
    // The bit is set when the corresponding `Option` is `Some(_)` and the
    // mutation passes validation — so a no-op tx (all fields None) emits
    // an event with `fields_changed == 0`. Indexers can filter trivially.
    let mut fields_changed: u32 = 0;

    if let Some(label) = params.label {
        require!(
            !label.is_empty() && label.len() <= 64,
            GarError::InvalidLabel
        );
        gateway.label = label;
        fields_changed |= GATEWAY_SETTINGS_FIELD_LABEL;
    }
    if let Some(fqdn) = params.fqdn {
        require!(!fqdn.is_empty() && fqdn.len() <= 128, GarError::InvalidFqdn);
        gateway.fqdn = fqdn;
        fields_changed |= GATEWAY_SETTINGS_FIELD_FQDN;
    }
    if let Some(port) = params.port {
        gateway.port = port;
        fields_changed |= GATEWAY_SETTINGS_FIELD_PORT;
    }
    if let Some(protocol) = params.protocol {
        gateway.protocol = protocol;
        fields_changed |= GATEWAY_SETTINGS_FIELD_PROTOCOL;
    }
    if let Some(properties) = params.properties {
        require!(is_valid_arweave_id(&properties), GarError::InvalidParameter);
        gateway.properties = properties;
        fields_changed |= GATEWAY_SETTINGS_FIELD_PROPERTIES;
    }
    if let Some(note) = params.note {
        require!(note.len() <= 256, GarError::InvalidParameter);
        gateway.note = note;
        fields_changed |= GATEWAY_SETTINGS_FIELD_NOTE;
    }
    if let Some(allow_delegated_staking) = params.allow_delegated_staking {
        gateway.settings.allow_delegated_staking = allow_delegated_staking;
        fields_changed |= GATEWAY_SETTINGS_FIELD_ALLOW_DELEGATED_STAKING;
    }
    if let Some(ratio) = params.delegate_reward_share_ratio {
        // Input ratio is 0-100 (whole percent), stored as 0-10000 (basis points)
        // for use in reward distribution: reward * ratio / 10_000
        require!(
            (ratio as u16) * 100 <= MAX_DELEGATE_REWARD_SHARE,
            GarError::InvalidRewardShare
        );
        gateway.settings.delegate_reward_share_ratio = ratio as u16 * 100;
        fields_changed |= GATEWAY_SETTINGS_FIELD_DELEGATE_REWARD_SHARE_RATIO;
    }
    if let Some(min_stake) = params.min_delegate_stake {
        require!(
            min_stake >= ctx.accounts.settings.min_delegate_stake,
            GarError::DelegationBelowMinimum
        );
        gateway.settings.min_delegation_amount = min_stake;
        fields_changed |= GATEWAY_SETTINGS_FIELD_MIN_DELEGATE_STAKE;
    }

    emit!(GatewaySettingsUpdatedEvent {
        operator: gateway.operator,
        fields_changed,
        timestamp: Clock::get()?.unix_timestamp,
    });

    Ok(())
}

/// Update the observer address for a gateway (SHOULD-9)
/// Separate instruction because it needs Anchor-managed init/close for the lookup PDAs.
pub fn update_observer_address(
    ctx: Context<UpdateObserverAddress>,
    new_observer: Pubkey,
) -> Result<()> {
    let gateway = &mut ctx.accounts.gateway;

    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayLeaving
    );
    require!(
        new_observer != gateway.observer_address,
        GarError::InvalidParameter
    );

    // Update gateway's observer address
    gateway.observer_address = new_observer;

    // Old observer_lookup is closed by Anchor's `close = operator` constraint
    // New observer_lookup is initialized by Anchor's `init` constraint
    let new_lookup = &mut ctx.accounts.new_observer_lookup;
    new_lookup.gateway = ctx.accounts.operator.key();
    new_lookup.bump = ctx.bumps.new_observer_lookup;
    new_lookup.version = OBSERVER_LOOKUP_VERSION;

    emit!(ObserverAddressUpdatedEvent {
        operator: gateway.operator,
        new_observer,
        timestamp: Clock::get()?.unix_timestamp,
    });

    Ok(())
}

/// Prune a gateway that has exceeded maximum consecutive failures (F21)
/// Permissionless — anyone can call if gateway has 30+ consecutive failures.
/// Matches Lua: slash min operator stake, return remainder in withdrawal, remove from registry.
pub fn prune_gateway<'info>(ctx: Context<'_, '_, 'info, 'info, PruneGateway<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let settings = &ctx.accounts.settings;
    let gateway = &mut ctx.accounts.gateway;

    // GAR-013: Only joined gateways can be pruned. A leaving gateway has already
    // been swap-removed from the registry; pruning it would use a stale index.
    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayNotJoined
    );

    // M-6: Use configurable max_consecutive_failures from epoch_settings
    require!(
        gateway.stats.failed_consecutive >= ctx.accounts.epoch_settings.max_consecutive_failures,
        GarError::GatewayNotEligible
    );

    // Slash: 100% of min operator stake goes to protocol
    // (matches Lua failedGatewaySlashRate = 1.0).
    let slash_amount = std::cmp::min(settings.min_operator_stake, gateway.operator_stake);
    let post_slash = gateway.operator_stake.saturating_sub(slash_amount);
    let now = clock.unix_timestamp;
    let available_at = now
        .checked_add(GATEWAY_LEAVE_PERIOD)
        .ok_or(GarError::ArithmeticOverflow)?;

    // Lua-faithful split — same shape as leave_network, but operating on
    // the post-slash remainder. (gar.lua::pruneGateways calls slashOperatorStake
    // then leaveNetwork, so this mirrors that flow.)
    //   protected_amount = min(min_stake, post_slash)
    //   excess_amount    = post_slash - min_stake (saturating)
    let min_stake = settings.min_operator_stake;
    let protected_amount = post_slash.min(min_stake);
    let excess_amount = post_slash.saturating_sub(min_stake);

    let counter = &mut ctx.accounts.withdrawal_counter;
    if counter.bump == 0 {
        counter.owner = gateway.operator;
        counter.bump = ctx.bumps.withdrawal_counter;
        counter.version = WITHDRAWAL_COUNTER_VERSION;
    }
    let exit_id = counter.next_id;
    let excess_id = exit_id.checked_add(1).ok_or(GarError::ArithmeticOverflow)?;

    // 1. Anchor-init exit vault. When `post_slash == 0` (full-slash edge:
    //    pre_stake <= min), the exit vault is a zero-amount, immediately-
    //    claimable placeholder for rent reclaim — preserves the existing
    //    L-6 behavior in the new shape.
    {
        let exit = &mut ctx.accounts.withdrawal;
        exit.owner = gateway.operator;
        exit.withdrawal_id = exit_id;
        exit.gateway = gateway.operator;
        exit.amount = protected_amount;
        exit.created_at = now;
        exit.available_at = if protected_amount > 0 {
            available_at
        } else {
            now
        };
        exit.is_delegate = false;
        exit.is_exit_vault = protected_amount > 0;
        exit.is_protected = protected_amount > 0;
        exit.bump = ctx.bumps.withdrawal;
        exit.version = WITHDRAWAL_VERSION;
    }

    // 2. Manual create excess vault when post_slash > min_stake.
    if excess_amount > 0 {
        let excess_acc = ctx
            .accounts
            .excess_withdrawal
            .as_ref()
            .ok_or(GarError::MissingExcessWithdrawal)?
            .to_account_info();
        let operator_key = gateway.operator;
        let excess_id_bytes = excess_id.to_le_bytes();
        let (expected_pda, vault_bump) = Pubkey::find_program_address(
            &[WITHDRAWAL_SEED, operator_key.as_ref(), &excess_id_bytes],
            ctx.program_id,
        );
        require!(
            excess_acc.key() == expected_pda,
            GarError::InvalidExcessWithdrawalPda
        );
        require!(excess_acc.data_is_empty(), GarError::InvalidParameter);

        let lamports_required = Rent::get()?.minimum_balance(Withdrawal::SIZE);
        let signer_seeds: &[&[&[u8]]] = &[&[
            WITHDRAWAL_SEED,
            operator_key.as_ref(),
            &excess_id_bytes,
            &[vault_bump],
        ]];
        let existing = excess_acc.lamports();
        if existing < lamports_required {
            anchor_system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.payer.to_account_info(),
                        to: excess_acc.clone(),
                    },
                ),
                lamports_required - existing,
            )?;
        }
        anchor_system_program::allocate(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Allocate {
                    account_to_allocate: excess_acc.clone(),
                },
                signer_seeds,
            ),
            Withdrawal::SIZE as u64,
        )?;
        anchor_system_program::assign(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Assign {
                    account_to_assign: excess_acc.clone(),
                },
                signer_seeds,
            ),
            ctx.program_id,
        )?;

        let excess_state = Withdrawal {
            owner: operator_key,
            withdrawal_id: excess_id,
            gateway: operator_key,
            amount: excess_amount,
            created_at: now,
            available_at,
            is_delegate: false,
            is_exit_vault: true,
            is_protected: false,
            bump: vault_bump,
            version: WITHDRAWAL_VERSION,
        };
        let mut data = excess_acc.try_borrow_mut_data()?;
        let mut cursor = std::io::Cursor::new(&mut data[..]);
        excess_state
            .try_serialize(&mut cursor)
            .map_err(|_| GarError::InvalidParameter)?;
    }

    counter.next_id = if excess_amount > 0 {
        excess_id
            .checked_add(1)
            .ok_or(GarError::ArithmeticOverflow)?
    } else {
        exit_id.checked_add(1).ok_or(GarError::ArithmeticOverflow)?
    };

    // H3: Transfer slashed tokens to protocol (matches Lua: slashed stake goes to protocol balance)
    if slash_amount > 0 {
        let settings_bump = ctx.accounts.settings.bump;
        let signer_seeds: &[&[&[u8]]] = &[&[SETTINGS_SEED, &[settings_bump]]];

        let cpi_accounts = SplTransfer {
            from: ctx.accounts.stake_token_account.to_account_info(),
            to: ctx.accounts.protocol_token_account.to_account_info(),
            authority: ctx.accounts.settings.to_account_info(),
        };
        let cpi_ctx = CpiContext::new_with_signer(
            ctx.accounts.token_program.to_account_info(),
            cpi_accounts,
            signer_seeds,
        );
        token::transfer(cpi_ctx, slash_amount)?;
    }

    // Zero out operator stake
    gateway.operator_stake = 0;

    // Mark as leaving and snapshot epoch_duration for finalize_gone's GC gate.
    gateway.status = GatewayStatus::Leaving;
    gateway.leave_timestamp = Some(clock.unix_timestamp);
    gateway.leave_epoch_duration = ctx.accounts.epoch_settings.epoch_duration;

    // Same in-place status flip as `leave_network`: slot stays occupied until
    // the permissionless `finalize_gone` GC reclaims it. See `leave_network`
    // for the full rationale (preserves registry indices mid-epoch).
    let mut registry = ctx.accounts.registry.load_mut()?;
    let index = gateway.registry_index.index as usize;
    require!(
        index < GatewayRegistry::MAX_GATEWAYS,
        GarError::InvalidParameter
    );
    require!((index as u32) < registry.count, GarError::InvalidParameter);
    let slot = &mut registry.gateways[index];
    require!(slot.address == gateway.operator, GarError::InvalidParameter);
    slot.composite_weight = 0;
    slot.status = GatewaySlot::STATUS_LEAVING;

    // SHOULD-9: Close observer lookup (last remaining_account)
    let remaining = ctx.remaining_accounts;
    if let Some(lookup_info) = remaining.last() {
        let (expected_pda, _) = Pubkey::find_program_address(
            &[OBSERVER_LOOKUP_SEED, gateway.observer_address.as_ref()],
            ctx.program_id,
        );
        if lookup_info.key() == expected_pda && lookup_info.is_writable {
            close_observer_lookup_account(lookup_info, &ctx.accounts.payer.to_account_info())?;
        }
    }

    // Supply counter: slash_amount leaves the system entirely; the remaining
    // post_slash (== protected_amount + excess_amount) moves into the
    // exit/excess vaults and is tracked under total_withdrawn.
    let prune_stake = slash_amount.saturating_add(post_slash);
    let settings = &mut ctx.accounts.settings;
    settings.total_staked = settings.total_staked.saturating_sub(prune_stake);
    settings.total_withdrawn = settings
        .total_withdrawn
        .checked_add(post_slash)
        .ok_or(GarError::ArithmeticOverflow)?;

    msg!(
        "Gateway {} pruned: slashed {}, exit_vault {} (protected), excess {}",
        gateway.operator,
        slash_amount,
        protected_amount,
        excess_amount
    );

    emit!(GatewayPrunedEvent {
        operator: gateway.operator,
        pruner: ctx.accounts.payer.key(),
        slashed_amount: slash_amount,
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

// =========================================
// SHOULD-9: Observer Lookup Helpers
// =========================================

/// Close an ObserverLookup account, returning rent to recipient.
pub(crate) fn close_observer_lookup_account<'info>(
    lookup_info: &AccountInfo<'info>,
    recipient_info: &AccountInfo<'info>,
) -> Result<()> {
    let dest_starting_lamports = recipient_info.lamports();
    **recipient_info.lamports.borrow_mut() = dest_starting_lamports
        .checked_add(lookup_info.lamports())
        .ok_or(GarError::ArithmeticOverflow)?;
    **lookup_info.lamports.borrow_mut() = 0;
    lookup_info.realloc(0, false)?;
    lookup_info.assign(&anchor_lang::solana_program::system_program::ID);
    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
#[instruction(params: JoinNetworkParams)]
pub struct JoinNetwork<'info> {
    #[account(
        mut,
        seeds = [REGISTRY_SEED],
        bump,
    )]
    pub registry: AccountLoader<'info, GatewayRegistry>,

    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Box<Account<'info, GatewaySettings>>,

    #[account(
        init,
        payer = operator,
        space = Gateway::SIZE,
        seeds = [GATEWAY_SEED, operator.key().as_ref()],
        bump,
    )]
    pub gateway: Box<Account<'info, Gateway>>,

    #[account(
        mut,
        constraint = operator_token_account.owner == operator.key(),
        constraint = operator_token_account.mint == settings.mint,
    )]
    pub operator_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = stake_token_account.mint == settings.mint,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Box<Account<'info, TokenAccount>>,

    /// SHOULD-9: Observer lookup PDA enforces observer address uniqueness.
    /// init fails if another gateway already claims this observer address.
    #[account(
        init,
        payer = operator,
        space = ObserverLookup::SIZE,
        seeds = [OBSERVER_LOOKUP_SEED, params.observer_address.as_ref()],
        bump,
    )]
    pub observer_lookup: Box<Account<'info, ObserverLookup>>,

    #[account(mut)]
    pub operator: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct LeaveNetwork<'info> {
    #[account(
        mut,
        seeds = [REGISTRY_SEED],
        bump,
    )]
    pub registry: AccountLoader<'info, GatewayRegistry>,

    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, operator.key().as_ref()],
        bump = gateway.bump,
        constraint = gateway.operator == operator.key() @ GarError::NotOperator,
    )]
    pub gateway: Account<'info, Gateway>,

    /// Read-only — only consulted to snapshot `epoch_duration` onto the
    /// gateway so `finalize_gone` can compute the GC eligibility window
    /// from a value frozen at leave time (defense against admin retuning).
    #[account(
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    #[account(
        init_if_needed,
        payer = operator,
        space = WithdrawalCounter::SIZE,
        seeds = [WITHDRAWAL_COUNTER_SEED, operator.key().as_ref()],
        bump,
    )]
    pub withdrawal_counter: Account<'info, WithdrawalCounter>,

    /// Anchor-init slot: holds the **protected exit vault** (`is_protected:
    /// true`) for `min(min_operator_stake, operator_stake)` — Lua's
    /// `createGatewayExitVault`. Always created when there's any stake to
    /// vault; an immediately-claimable zero-amount placeholder is kept for
    /// the rent-reclaim path (matches the prune L-6 behavior, generalized
    /// to leave for symmetry).
    #[account(
        init,
        payer = operator,
        space = Withdrawal::SIZE,
        seeds = [WITHDRAWAL_SEED, operator.key().as_ref(), &withdrawal_counter.next_id.to_le_bytes()],
        bump,
    )]
    pub withdrawal: Account<'info, Withdrawal>,

    /// Optional **excess vault** slot (`is_protected: false`) — Lua's
    /// `createGatewayWithdrawVault` for the `operator_stake - min_stake`
    /// portion. The handler creates it manually (system_program::create_account
    /// CPI) when `excess_amount > 0`; passes through unmodified when zero.
    /// Caller derives PDA off-chain using `WITHDRAWAL_COUNTER.next_id + 1`
    /// (i.e. the slot AFTER the exit vault).
    ///
    /// CHECK: Validated inside the handler — PDA derivation, ownership flip
    /// via system_program CPI, and the data write all run only when the
    /// excess amount is non-zero. Skipped slot stays unowned.
    #[account(mut)]
    pub excess_withdrawal: Option<UncheckedAccount<'info>>,

    #[account(mut)]
    pub operator: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct UpdateGatewaySettings<'info> {
    #[account(
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, operator.key().as_ref()],
        bump = gateway.bump,
        constraint = gateway.operator == operator.key() @ GarError::NotOperator,
    )]
    pub gateway: Account<'info, Gateway>,

    pub operator: Signer<'info>,
}

#[derive(Accounts)]
#[instruction(new_observer: Pubkey)]
pub struct UpdateObserverAddress<'info> {
    #[account(
        mut,
        seeds = [GATEWAY_SEED, operator.key().as_ref()],
        bump = gateway.bump,
        constraint = gateway.operator == operator.key() @ GarError::NotOperator,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        mut,
        seeds = [OBSERVER_LOOKUP_SEED, gateway.observer_address.as_ref()],
        bump = old_observer_lookup.bump,
        close = operator,
    )]
    pub old_observer_lookup: Account<'info, ObserverLookup>,

    #[account(
        init,
        payer = operator,
        space = ObserverLookup::SIZE,
        seeds = [OBSERVER_LOOKUP_SEED, new_observer.as_ref()],
        bump,
    )]
    pub new_observer_lookup: Account<'info, ObserverLookup>,

    #[account(mut)]
    pub operator: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction()]
pub struct PruneGateway<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Box<Account<'info, GatewaySettings>>,

    #[account(
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
    )]
    pub epoch_settings: Box<Account<'info, EpochSettings>>,

    #[account(
        mut,
        seeds = [REGISTRY_SEED],
        bump,
    )]
    pub registry: AccountLoader<'info, GatewayRegistry>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, gateway.operator.as_ref()],
        bump = gateway.bump,
    )]
    pub gateway: Box<Account<'info, Gateway>>,

    #[account(
        init_if_needed,
        payer = payer,
        space = WithdrawalCounter::SIZE,
        seeds = [WITHDRAWAL_COUNTER_SEED, gateway.operator.as_ref()],
        bump,
    )]
    pub withdrawal_counter: Box<Account<'info, WithdrawalCounter>>,

    /// Anchor-init slot. Either the protected exit vault (`is_protected:
    /// true`) for the post-slash min portion, OR an empty zero-amount
    /// placeholder when the slash consumed everything (full-slash edge
    /// case; placeholder is immediately claimable so rent can be reclaimed).
    #[account(
        init,
        payer = payer,
        space = Withdrawal::SIZE,
        seeds = [WITHDRAWAL_SEED, gateway.operator.as_ref(), &withdrawal_counter.next_id.to_le_bytes()],
        bump,
    )]
    pub withdrawal: Box<Account<'info, Withdrawal>>,

    /// Optional excess vault slot (`is_protected: false`) — created
    /// manually when post-slash stake exceeds `min_operator_stake`. PDA
    /// derived from `WITHDRAWAL_COUNTER.next_id + 1`.
    ///
    /// CHECK: Validated inside the handler before any write — PDA match,
    /// empty-data, lamport top-up, allocate + assign via system_program.
    #[account(mut)]
    pub excess_withdrawal: Option<UncheckedAccount<'info>>,

    /// Stake token account holding gateway tokens (PDA-controlled)
    #[account(
        mut,
        constraint = stake_token_account.mint == settings.mint,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Box<Account<'info, TokenAccount>>,

    /// Protocol token account to receive slashed tokens
    #[account(
        mut,
        constraint = protocol_token_account.mint == settings.mint,
        constraint = protocol_token_account.key() == settings.protocol_token_account @ GarError::InvalidParameter,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    /// Anyone can call prune (permissionless crank)
    #[account(mut)]
    pub payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

// =========================================
// FINALIZE_GONE — permissionless GC for departed gateways
// =========================================
//
// Closes a `Leaving` gateway's PDA and reclaims its registry slot once:
//   1. The leave period has elapsed (`leave_timestamp + GATEWAY_LEAVE_PERIOD`).
//   2. Enough epochs have closed that no live epoch references this slot
//      (`+ 7 * max(snapshot, current_setting)` — `7` is the `min_gap` used
//      by `close_epoch`; the `max` is defense-in-depth so an admin shrinking
//      `epoch_duration` post-leave can't bring forward GC).
//   3. All delegations have been claimed (`gateway.total_delegated_stake == 0`).
//      Without this, closing the Gateway PDA would strand undelegated stake;
//      `claim_delegate_from_leaving_gateway` is permissionless so the cranker
//      can drain delegations before invoking finalize_gone.
//
// Caller pays the tx and receives the closed PDA's rent as the cleanup
// incentive. Anyone can call.

/// Compute the leave-window expiry timestamp for finalize_gone, taking the
/// max of the snapshot (frozen at leave time) and the live `epoch_duration`
/// so admin retuning of `epoch_settings.epoch_duration` cannot retroactively
/// shorten a leaver's GC window.
fn finalize_gone_expiry(gateway: &Gateway, current_epoch_duration: i64) -> Result<i64> {
    let leave_ts = gateway.leave_timestamp.ok_or(GarError::GatewayNotLeaving)?;
    let snapshot = gateway.leave_epoch_duration;
    // Defense-in-depth: take the longer of the snapshot or the current value.
    // Using saturating ops to avoid panicking on pathological zero-init or
    // admin misconfiguration; the caller is permissionless and we'd rather
    // surface a clean error than overflow.
    let effective_duration = snapshot.max(current_epoch_duration).max(0);
    let epoch_window = effective_duration.saturating_mul(7);
    let total = GATEWAY_LEAVE_PERIOD.saturating_add(epoch_window);
    Ok(leave_ts.saturating_add(total))
}

pub fn finalize_gone<'info>(ctx: Context<'_, '_, 'info, 'info, FinalizeGone<'info>>) -> Result<()> {
    let clock = Clock::get()?;
    let gateway = &mut ctx.accounts.gateway;

    // Status gate
    require!(
        gateway.status == GatewayStatus::Leaving,
        GarError::GatewayNotLeaving
    );

    // Time gate
    let expiry = finalize_gone_expiry(gateway, ctx.accounts.epoch_settings.epoch_duration)?;
    require!(
        clock.unix_timestamp >= expiry,
        GarError::LeaveWindowNotExpired
    );

    // Delegation gate (load-bearing — without this, closing the Gateway PDA
    // would strand any unclaimed delegate stake)
    require!(
        gateway.total_delegated_stake == 0,
        GarError::DelegationsOutstanding
    );

    // Mark Gone. The PDA is closed by Anchor's `close = caller` on exit.
    gateway.status = GatewayStatus::Gone;

    // Reclaim the registry slot. If this is the last slot, just zero it and
    // decrement count. Otherwise, swap the last slot into this index AND
    // update the swapped gateway's `registry_index.index` so future lookups
    // resolve correctly. Mirrors the swap-remove pattern previously inside
    // `leave_network`/`prune_gateway`.
    let mut registry = ctx.accounts.registry.load_mut()?;
    require!(registry.count > 0, GarError::InvalidParameter);
    let index = gateway.registry_index.index as usize;
    require!(
        index < GatewayRegistry::MAX_GATEWAYS,
        GarError::InvalidParameter
    );
    require!((index as u32) < registry.count, GarError::InvalidParameter);
    require!(
        registry.gateways[index].address == gateway.operator,
        GarError::InvalidParameter
    );

    let last_index = (registry.count - 1) as usize;
    if index != last_index {
        // Move last slot into this index
        let swapped_slot = registry.gateways[last_index];
        registry.gateways[index] = swapped_slot;

        // Update the swapped gateway's stored registry_index.index via
        // remaining_accounts[0]. Cranker MUST pass the swapped Gateway PDA
        // (writable) at this position when index != last_index.
        let remaining = ctx.remaining_accounts;
        require!(!remaining.is_empty(), GarError::InvalidParameter);
        let swapped_info = &remaining[0];
        require!(swapped_info.is_writable, GarError::InvalidParameter);
        require!(
            swapped_info.owner == ctx.program_id,
            GarError::InvalidParameter
        );

        let (expected_pda, _) = Pubkey::find_program_address(
            &[GATEWAY_SEED, swapped_slot.address.as_ref()],
            ctx.program_id,
        );
        require!(
            swapped_info.key() == expected_pda,
            GarError::InvalidParameter
        );

        // Deserialize, update index, re-serialize.
        let data = swapped_info.try_borrow_data()?;
        let mut slice: &[u8] = &data[8..];
        let mut swapped_gw =
            Gateway::deserialize(&mut slice).map_err(|_| GarError::InvalidParameter)?;
        swapped_gw.registry_index.index = index as u32;
        drop(data);

        let mut data = swapped_info.try_borrow_mut_data()?;
        let dst = &mut data[8..];
        let mut cursor = std::io::Cursor::new(dst);
        swapped_gw
            .serialize(&mut cursor)
            .map_err(|_| GarError::InvalidParameter)?;
    }

    // Zero the (now-vacated) last slot and shrink the registry.
    registry.gateways[last_index] = GatewaySlot {
        address: Pubkey::default(),
        composite_weight: 0,
        start_timestamp: 0,
        status: GatewaySlot::STATUS_JOINED,
        _padding: [0; 7],
    };
    registry.count = registry.count.saturating_sub(1);

    emit!(GatewayFinalizedEvent {
        operator: gateway.operator,
        pruner: ctx.accounts.caller.key(),
        timestamp: clock.unix_timestamp,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct FinalizeGone<'info> {
    #[account(
        mut,
        seeds = [GATEWAY_SEED, gateway.operator.as_ref()],
        bump = gateway.bump,
        close = caller,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        mut,
        seeds = [REGISTRY_SEED],
        bump,
    )]
    pub registry: AccountLoader<'info, GatewayRegistry>,

    /// Read-only — used to compute the GC eligibility window
    /// against `max(gateway.leave_epoch_duration, epoch_settings.epoch_duration)`.
    #[account(
        seeds = [EPOCH_SETTINGS_SEED],
        bump = epoch_settings.bump,
    )]
    pub epoch_settings: Account<'info, EpochSettings>,

    /// Permissionless: anyone willing to pay the tx fee can crank GC and
    /// receives the Gateway PDA's rent as the cleanup incentive.
    #[account(mut)]
    pub caller: Signer<'info>,
}
