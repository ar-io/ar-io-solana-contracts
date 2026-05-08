use anchor_lang::prelude::*;

declare_id!("ARioGarProgramXXXXXXXXXXXXXXXXXXXXXXXXXXXXX");

pub mod error;
pub mod instructions;
pub mod migration;
pub mod state;

use instructions::*;
pub use migration::*;

// Re-export funding-plan types so external callers (tests, ArNS / ario-core
// CPI wrappers, SDK codegen via the IDL) can construct `FundingSourceSpec`
// without reaching into the private instructions module path.
pub use instructions::payment::{FundingSourceKind, FundingSourceSpec};

/// Validate a string as an Arweave ID (43 chars, alphanumeric/dash/underscore). Empty string allowed (unset).
pub(crate) fn is_valid_arweave_id(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if s.len() != 43 {
        return false;
    }
    s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// AR.IO GAR Program (Gateway Address Registry)
///
/// Consolidated program handling:
/// - Gateway join/leave network (F10-F11)
/// - Gateway settings management (F12)
/// - Operator stake management (F13-F14)
/// - Delegate stake management (F15-F17)
/// - Withdrawal management (F18-F19)
/// - Delegate allowlists (F20)
/// - Gateway pruning (F21)
/// - Epoch creation and ticking (F23-F24)
/// - Observation submission (F25-F26)
/// - Reward distribution (F27-F29)
#[program]
pub mod ario_gar {
    use super::*;

    // =========================================
    // INITIALIZATION
    // =========================================

    /// Initialize GAR program (once per deployment)
    pub fn initialize(ctx: Context<InitializeGar>, params: InitializeParams) -> Result<()> {
        instructions::initialize::initialize(ctx, params)
    }

    /// Create the GatewayRegistry zero-copy account (~120KB).
    /// Separated from [`initialize`] so large PDAs are not created in the same tx as settings init.
    pub fn create_gateway_registry(ctx: Context<CreateGatewayRegistry>) -> Result<()> {
        instructions::initialize::create_gateway_registry(ctx)
    }

    /// Initialize epoch settings
    pub fn initialize_epochs(
        ctx: Context<InitializeEpochs>,
        params: InitializeEpochParams,
    ) -> Result<()> {
        instructions::initialize::initialize_epochs(ctx, params)
    }

    /// Admin one-shot: transfer SPL `Owner` authority of `protocol_token_account`
    /// from the GatewaySettings PDA to a target authority (typically
    /// `ario-core`'s ArioConfig PDA, which signs SPL transfers in
    /// `ario_core::import_balance`'s genesis-distribution branch).
    ///
    /// Mainnet creates the treasury under ArioConfig from the start; this
    /// instruction is the migration path for already-deployed clusters.
    /// See `instructions::initialize::release_treasury_authority` for the
    /// motivation comment.
    pub fn release_treasury_authority(
        ctx: Context<ReleaseTreasuryAuthority>,
        new_authority: Pubkey,
    ) -> Result<()> {
        instructions::initialize::release_treasury_authority(ctx, new_authority)
    }

    /// Admin recovery — repair `GatewaySettings.mint` /
    /// `.stake_token_account` / `.protocol_token_account` for clusters
    /// where genesis was partially-initialized. See
    /// `instructions::initialize::admin_repair_settings` for the
    /// motivation comment + the matching `ario_core::admin_repair_config`
    /// for the cross-program companion. Pre-cutover only.
    pub fn admin_repair_settings(
        ctx: Context<AdminRepairSettings>,
        new_mint: Pubkey,
        new_stake_token_account: Pubkey,
        new_protocol_token_account: Pubkey,
    ) -> Result<()> {
        instructions::initialize::admin_repair_settings(
            ctx,
            new_mint,
            new_stake_token_account,
            new_protocol_token_account,
        )
    }

    // =========================================
    // GATEWAY LIFECYCLE (F10-F12)
    // =========================================

    /// Join the AR.IO network as a gateway operator (F10)
    ///
    /// SECURITY: Re-join after leave_network is structurally prevented because the gateway PDA
    /// uses `init` (not `init_if_needed`) and gateway accounts are never closed — they persist
    /// with Leaving status. If gateway account closure is ever added, a re-join cooldown must
    /// be implemented to prevent failed_consecutive counter resets.
    pub fn join_network(ctx: Context<JoinNetwork>, params: JoinNetworkParams) -> Result<()> {
        instructions::gateway::join_network(ctx, params)
    }

    /// Leave the AR.IO network (F11)
    /// Sets gateway to Leaving status with a 90-day leave period.
    /// Operator stake becomes withdrawable after leave period.
    /// Delegates are notified to withdraw (handled via separate delegate withdrawal txs).
    /// Matches Lua: gateway.status = "leaving", leaveLengthMs = 90 days
    pub fn leave_network<'info>(
        ctx: Context<'_, '_, 'info, 'info, LeaveNetwork<'info>>,
    ) -> Result<()> {
        instructions::gateway::leave_network(ctx)
    }

    /// Update gateway settings (F12)
    pub fn update_gateway_settings(
        ctx: Context<UpdateGatewaySettings>,
        params: UpdateGatewayParams,
    ) -> Result<()> {
        instructions::gateway::update_gateway_settings(ctx, params)
    }

    /// Update the observer address for a gateway (SHOULD-9)
    /// Separate instruction because it needs Anchor-managed init/close for the lookup PDAs.
    pub fn update_observer_address(
        ctx: Context<UpdateObserverAddress>,
        new_observer: Pubkey,
    ) -> Result<()> {
        instructions::gateway::update_observer_address(ctx, new_observer)
    }

    // =========================================
    // OPERATOR STAKE (F13-F14)
    // =========================================

    /// Increase operator stake (F13)
    pub fn increase_operator_stake(ctx: Context<IncreaseOperatorStake>, amount: u64) -> Result<()> {
        instructions::operator_stake::increase_operator_stake(ctx, amount)
    }

    /// Decrease operator stake - creates withdrawal (F14)
    pub fn decrease_operator_stake(ctx: Context<DecreaseOperatorStake>, amount: u64) -> Result<()> {
        instructions::operator_stake::decrease_operator_stake(ctx, amount)
    }

    // =========================================
    // DELEGATE STAKE (F15-F17)
    // =========================================

    /// Delegate stake to a gateway (F15)
    pub fn delegate_stake(ctx: Context<DelegateStake>, amount: u64) -> Result<()> {
        instructions::delegate::delegate_stake(ctx, amount)
    }

    /// Decrease delegated stake - creates withdrawal (F16)
    pub fn decrease_delegate_stake(ctx: Context<DecreaseDelegateStake>, amount: u64) -> Result<()> {
        instructions::delegate::decrease_delegate_stake(ctx, amount)
    }

    /// Close a delegation account with zero balance (permissionless cleanup)
    /// Matches Lua: delegation pruning when delegatedStake == 0
    pub fn close_empty_delegation(ctx: Context<CloseEmptyDelegation>) -> Result<()> {
        instructions::delegate::close_empty_delegation(ctx)
    }

    /// H2: Claim delegate stake from a leaving gateway.
    /// Matches Lua: gar.leaveNetwork -> kickDelegateFromGateway for each delegate.
    /// In Solana, delegates must claim individually (can't iterate PDAs on-chain).
    /// Creates a withdrawal vault for the delegate's full stake from the leaving gateway.
    pub fn claim_delegate_from_leaving_gateway(
        ctx: Context<ClaimDelegateFromLeavingGateway>,
    ) -> Result<()> {
        instructions::delegate::claim_delegate_from_leaving_gateway(ctx)
    }

    /// Redelegate stake from one gateway to another (F17)
    /// Fee = min(10% * redelegation_count, 60%) — first is free, resets every 7 days
    /// Fee goes to protocol. Net amount moves to target delegation.
    pub fn redelegate_stake(ctx: Context<RedelegateStake>, amount: u64) -> Result<()> {
        instructions::delegate::redelegate_stake(ctx, amount)
    }

    // =========================================
    // ALLOWLIST MANAGEMENT (F20)
    // =========================================

    /// Add a delegate to the gateway's allowlist (F20)
    /// Matches Lua: gar.allowDelegates
    pub fn allow_delegate(ctx: Context<AllowDelegate>) -> Result<()> {
        instructions::allowlist::allow_delegate(ctx)
    }

    /// Remove a delegate from the gateway's allowlist (F20)
    /// If delegate still has stake, they keep it but cannot add more after removal.
    /// Matches Lua: gar.disallowDelegates
    pub fn disallow_delegate(ctx: Context<DisallowDelegate>) -> Result<()> {
        instructions::allowlist::disallow_delegate(ctx)
    }

    /// Enable or disable the allowlist for a gateway
    pub fn set_allowlist_enabled(ctx: Context<UpdateGatewaySettings>, enabled: bool) -> Result<()> {
        instructions::allowlist::set_allowlist_enabled(ctx, enabled)
    }

    // =========================================
    // WITHDRAWAL MANAGEMENT (F18-F19)
    // =========================================

    /// Claim matured withdrawal
    pub fn claim_withdrawal(ctx: Context<ClaimWithdrawal>) -> Result<()> {
        instructions::withdrawal::claim_withdrawal(ctx)
    }

    /// Expedited (instant) withdrawal with time-decaying penalty (F19)
    /// Penalty decays linearly from max to min as time elapses in the withdrawal period.
    /// Matches Lua processInstantWithdrawal: penaltyRate = max - (max - min) * elapsed / total
    /// Applies to all withdrawals including exit vaults (leave_network/prune_gateway).
    pub fn instant_withdrawal(ctx: Context<InstantWithdrawal>) -> Result<()> {
        instructions::withdrawal::instant_withdrawal(ctx)
    }

    /// Cancel a pending withdrawal, returning stake to the gateway (F18)
    /// Matches Lua: gar.cancelGatewayWithdrawal — returns vault balance to operator_stake or delegate.amount
    pub fn cancel_withdrawal(ctx: Context<CancelWithdrawal>) -> Result<()> {
        instructions::withdrawal::cancel_withdrawal(ctx)
    }

    /// Close a fully-drained Withdrawal vault and refund rent to the owner.
    /// Permissionless cleanup; pairs with `deduct_withdrawal_for_payment`
    /// (which never closes the vault, even at zero).
    pub fn close_drained_withdrawal(ctx: Context<CloseDrainedWithdrawal>) -> Result<()> {
        instructions::withdrawal::close_drained_withdrawal(ctx)
    }

    // =========================================
    // EPOCH MANAGEMENT (F23-F24)
    // =========================================

    /// Enable/disable epoch processing
    pub fn set_epochs_enabled(ctx: Context<UpdateEpochSettings>, enabled: bool) -> Result<()> {
        instructions::epoch::set_epochs_enabled(ctx, enabled)
    }

    /// Create a new epoch (F23)
    /// This is permissionless - anyone can call when the previous epoch has ended
    pub fn create_epoch(ctx: Context<CreateEpoch>) -> Result<()> {
        instructions::epoch::create_epoch(ctx)
    }

    /// Tally weights for gateways in batches (permissionless crank).
    /// Computes weights for each gateway and caches composite_weight in registry slots.
    /// Accumulates total_composite_weight in the epoch.
    /// Call repeatedly with batches of gateway accounts until all are processed.
    pub fn tally_weights(ctx: Context<TallyWeights>, _epoch_index: u64) -> Result<()> {
        instructions::epoch::tally_weights(ctx, _epoch_index)
    }

    /// Prescribe observers and names for an epoch (single tx, deterministic selection).
    /// Uses weighted roulette from hashchain entropy. Computes per-unit rewards.
    pub fn prescribe_epoch(ctx: Context<PrescribeEpoch>, _epoch_index: u64) -> Result<()> {
        instructions::epoch::prescribe_epoch(ctx, _epoch_index)
    }

    /// Close a fully distributed epoch account, reclaiming rent.
    /// Permissionless — anyone can call once the epoch is distributed and
    /// at least `epoch_retention` epochs have passed.
    pub fn close_epoch(ctx: Context<CloseEpoch>, _epoch_index: u64) -> Result<()> {
        instructions::epoch::close_epoch(ctx, _epoch_index)
    }

    // =========================================
    // OBSERVATION SUBMISSION (F25-F26)
    // =========================================

    /// Submit observation report (F25)
    pub fn save_observations(
        ctx: Context<SaveObservations>,
        _epoch_index: u64,
        gateway_results: [u8; 375],
        gateway_count: u16,
        report_tx_id: [u8; 32],
    ) -> Result<()> {
        instructions::observation::save_observations(
            ctx,
            _epoch_index,
            gateway_results,
            gateway_count,
            report_tx_id,
        )
    }

    /// Close an observation PDA from a fully distributed epoch, reclaiming rent.
    /// Permissionless — anyone can call once the parent epoch is distributed.
    pub fn close_observation(ctx: Context<CloseObservation>, _epoch_index: u64) -> Result<()> {
        instructions::observation::close_observation(ctx, _epoch_index)
    }

    // =========================================
    // REWARD DISTRIBUTION (F27-F29)
    // =========================================

    /// Distribute rewards for an epoch in batches.
    /// Processes gateway accounts from remaining_accounts. Call repeatedly until all gateways are processed.
    pub fn distribute_epoch<'info>(
        ctx: Context<'_, '_, 'info, 'info, DistributeEpoch<'info>>,
        _epoch_index: u64,
    ) -> Result<()> {
        instructions::distribution::distribute_epoch(ctx, _epoch_index)
    }

    /// Compound delegation rewards by settling the reward-per-share accumulator.
    /// Delegates call this to materialize pending rewards into their delegation amount.
    pub fn compound_delegation_rewards(ctx: Context<CompoundDelegationRewards>) -> Result<()> {
        instructions::delegate::compound_delegation_rewards(ctx)
    }

    /// Prune a gateway that has exceeded maximum consecutive failures (F21)
    /// Permissionless — anyone can call if gateway has 30+ consecutive failures.
    /// Matches Lua: slash min operator stake, return remainder in withdrawal, remove from registry.
    pub fn prune_gateway<'info>(
        ctx: Context<'_, '_, 'info, 'info, PruneGateway<'info>>,
    ) -> Result<()> {
        instructions::gateway::prune_gateway(ctx)
    }

    /// Permissionless GC for a gateway in `Leaving` status whose leave window
    /// has expired and whose delegations have all been claimed. Sets the
    /// gateway to `Gone`, swap-removes its registry slot, and closes the
    /// Gateway PDA — caller receives the rent.
    pub fn finalize_gone<'info>(
        ctx: Context<'_, '_, 'info, 'info, FinalizeGone<'info>>,
    ) -> Result<()> {
        instructions::gateway::finalize_gone(ctx)
    }

    // =========================================
    // STAKE PAYMENT (for CPI from ario-arns)
    // =========================================

    /// Deduct from delegation and send to protocol treasury.
    /// Designed for CPI from ario-arns to fund ArNS purchases from delegated stake.
    pub fn deduct_delegation_for_payment(
        ctx: Context<DeductDelegationForPayment>,
        amount: u64,
    ) -> Result<()> {
        instructions::payment::deduct_delegation_for_payment(ctx, amount)
    }

    /// Deduct from operator stake and send to protocol treasury.
    /// Designed for CPI from ario-arns to fund ArNS purchases from operator stake.
    pub fn deduct_operator_stake_for_payment(
        ctx: Context<DeductOperatorStakeForPayment>,
        amount: u64,
    ) -> Result<()> {
        instructions::payment::deduct_operator_stake_for_payment(ctx, amount)
    }

    /// Deduct from a Withdrawal vault and send to protocol treasury.
    /// Used both directly (ArNS `_from_withdrawal` variants) and as a primitive
    /// inside `pay_from_funding_plan` for the multi-source path. Vault stays
    /// open even at zero — pair with `close_drained_withdrawal` for cleanup.
    /// Gateway-status-independent.
    pub fn deduct_withdrawal_for_payment(
        ctx: Context<DeductWithdrawalForPayment>,
        amount: u64,
    ) -> Result<()> {
        instructions::payment::deduct_withdrawal_for_payment(ctx, amount)
    }

    /// Apply a multi-source funding plan, transferring `expected_total` mARIO
    /// into the protocol treasury. Lua-faithful port of `gar.applyFundingPlan`:
    /// dispatches across Balance / Delegation / OperatorStake / Withdrawal
    /// sources, aggregates per-source bookkeeping, and issues at most two SPL
    /// transfers. Auto-creates a Withdrawal vault for sub-min Delegation
    /// residue. Single-gateway invariant: all Delegation/OperatorStake sources
    /// must reference the same gateway; Withdrawal/Balance are gateway-free.
    pub fn pay_from_funding_plan<'info>(
        ctx: Context<'_, '_, 'info, 'info, PayFromFundingPlan<'info>>,
        sources: Vec<FundingSourceSpec>,
        expected_total: u64,
        residue_vault_count: u8,
    ) -> Result<()> {
        instructions::payment::pay_from_funding_plan(
            ctx,
            sources,
            expected_total,
            residue_vault_count,
        )
    }

    // =========================================
    // MIGRATION OPERATIONS
    // =========================================

    /// Import a pre-serialized account during migration
    pub fn import_account(
        ctx: Context<ImportAccount>,
        seeds: Vec<Vec<u8>>,
        data: Vec<u8>,
    ) -> Result<()> {
        import_account_handler(ctx, seeds, data)
    }

    /// Import a gateway entry into the GatewayRegistry
    pub fn import_registry_entry(
        ctx: Context<ImportRegistryEntry>,
        entry_pubkey: Pubkey,
        weight: u64,
        start_timestamp: i64,
    ) -> Result<()> {
        import_registry_entry_handler(ctx, entry_pubkey, weight, start_timestamp)
    }

    /// Permanently disable migration imports (main authority only)
    pub fn finalize_migration(ctx: Context<FinalizeMigration>) -> Result<()> {
        finalize_migration_handler(ctx)
    }

    /// One-shot backfill of `GatewaySettings::arns_program_id` for accounts
    /// deployed before that field existed. Idempotent — safe to call on
    /// already-migrated accounts (returns early). See migration.rs for details.
    pub fn migrate_settings_set_arns_program_id(
        ctx: Context<MigrateSettingsSetArnsProgramId>,
        arns_program_id: Pubkey,
    ) -> Result<()> {
        migrate_settings_set_arns_program_id_handler(ctx, arns_program_id)
    }

    /// One-shot backfill of supply counter fields on GatewaySettings.
    /// Called after migration import with the correct totals computed from
    /// imported gateways, delegations, and withdrawals.
    pub fn migrate_settings_supply_counters(
        ctx: Context<MigrateSettingsSupplyCounters>,
        total_staked: u64,
        total_delegated: u64,
        total_withdrawn: u64,
    ) -> Result<()> {
        migrate_settings_supply_counters_handler(
            ctx,
            total_staked,
            total_delegated,
            total_withdrawn,
        )
    }
}

// =========================================
// CONSTANTS
// =========================================

pub const RATE_SCALE: u64 = 1_000_000;
pub const WITHDRAWAL_LOCK_PERIOD: i64 = 30 * 86_400; // 30 days
pub const GATEWAY_LEAVE_PERIOD: i64 = 90 * 86_400; // 90 days (matches Lua leaveLengthMs)
pub const MAX_EXPEDITED_WITHDRAWAL_PENALTY: u64 = 500_000; // 50% (matches Lua maxExpeditedWithdrawalPenaltyRate)
pub const MIN_EXPEDITED_WITHDRAWAL_PENALTY: u64 = 100_000; // 10% (matches Lua minExpeditedWithdrawalPenaltyRate)
pub const MIN_EXPEDITED_WITHDRAWAL_AMOUNT: u64 = 1_000_000; // 1 ARIO (matches Lua MIN_WITHDRAWAL_AMOUNT)
pub const DEFAULT_EPOCH_DURATION: i64 = 86_400; // 24 hours
pub const MAX_DELEGATE_REWARD_SHARE: u16 = 9500; // 95% in basis points

// Distribution constants (matches Lua DistributionSettings)
pub const GATEWAY_OPERATOR_REWARD_RATE: u64 = 900_000; // 90% (scaled by RATE_SCALE)
pub const OBSERVER_REWARD_RATE: u64 = 100_000; // 10% (scaled by RATE_SCALE)
pub const MAX_REWARD_RATE: u64 = 1_000; // 0.1% per epoch (scaled by RATE_SCALE)
pub const MIN_REWARD_RATE: u64 = 500; // 0.05% per epoch (scaled by RATE_SCALE)
pub const REWARD_DECAY_START_EPOCH: u64 = 365;
pub const REWARD_DECAY_LAST_EPOCH: u64 = 547;
pub const MISSED_OBSERVATION_PENALTY: u64 = 250_000; // 25% (scaled by RATE_SCALE)

// =========================================
// GATEWAY SETTINGS FIELD BITMASK
// =========================================
// `GatewaySettingsUpdatedEvent::fields_changed` is a u32 bitmask of which
// fields were mutated by `update_gateway_settings`. Stable wire encoding —
// indexers depend on the bit positions; do not renumber existing entries.
// Keep in sync with the optional fields on `UpdateGatewayParams`.

pub const GATEWAY_SETTINGS_FIELD_LABEL: u32 = 1 << 0;
pub const GATEWAY_SETTINGS_FIELD_FQDN: u32 = 1 << 1;
pub const GATEWAY_SETTINGS_FIELD_PORT: u32 = 1 << 2;
pub const GATEWAY_SETTINGS_FIELD_PROTOCOL: u32 = 1 << 3;
pub const GATEWAY_SETTINGS_FIELD_PROPERTIES: u32 = 1 << 4;
pub const GATEWAY_SETTINGS_FIELD_NOTE: u32 = 1 << 5;
/// Reserved: `auto_stake` was removed from `GatewaySettings` in commit cfc7a8b.
/// The bit is left allocated (and never set) so post-rollout indexers
/// don't observe a renumber if the field comes back. Per ADR-017,
/// discriminator values are append-only.
pub const GATEWAY_SETTINGS_FIELD_AUTO_STAKE: u32 = 1 << 6;
pub const GATEWAY_SETTINGS_FIELD_ALLOW_DELEGATED_STAKING: u32 = 1 << 7;
pub const GATEWAY_SETTINGS_FIELD_DELEGATE_REWARD_SHARE_RATIO: u32 = 1 << 8;
pub const GATEWAY_SETTINGS_FIELD_MIN_DELEGATE_STAKE: u32 = 1 << 9;

// =========================================
// PARAMETER TYPES
// =========================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeParams {
    pub authority: Pubkey,
    pub migration_authority: Pubkey,
    /// C-1: Address of the stake token account to pin in settings
    pub stake_token_account: Pubkey,
    /// C-2: Address of the protocol token account to pin in settings
    pub protocol_token_account: Pubkey,
    /// H-4: Address of the ario-arns program (used by `prescribe_epoch` to
    /// validate the NameRegistry account it reads from).
    pub arns_program_id: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeEpochParams {
    pub authority: Pubkey,
    pub epoch_duration: i64,
    pub observer_count: u8,
    pub name_count: u8,
    pub min_observer_stake: u64,
    pub slash_rate: u16,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct JoinNetworkParams {
    pub operator_stake: u64,
    pub label: String,
    pub fqdn: String,
    pub port: u16,
    pub protocol: state::Protocol,
    pub properties: Option<String>,
    pub note: Option<String>,
    pub allow_delegated_staking: bool,
    pub delegate_reward_share_ratio: u8,
    pub min_delegate_stake: Option<u64>,
    /// M3: Separate observer address (client passes operator key for default)
    pub observer_address: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Default)]
pub struct UpdateGatewayParams {
    pub label: Option<String>,
    pub fqdn: Option<String>,
    pub port: Option<u16>,
    pub protocol: Option<state::Protocol>,
    pub properties: Option<String>,
    pub note: Option<String>,
    pub allow_delegated_staking: Option<bool>,
    pub delegate_reward_share_ratio: Option<u8>,
    pub min_delegate_stake: Option<u64>,
}

// =========================================
// EVENTS
// =========================================

#[event]
pub struct GatewayJoinedEvent {
    pub operator: Pubkey,
    pub stake: u64,
    pub fqdn: String,
    pub timestamp: i64,
}

#[event]
pub struct GatewayLeavingEvent {
    pub operator: Pubkey,
    pub timestamp: i64,
}

#[event]
pub struct DelegationEvent {
    pub delegator: Pubkey,
    pub gateway: Pubkey,
    pub amount: u64,
    pub total: u64,
    pub timestamp: i64,
}

#[event]
pub struct WithdrawalCreatedEvent {
    pub owner: Pubkey,
    pub withdrawal_id: u64,
    pub amount: u64,
    pub available_at: i64,
    pub timestamp: i64,
}

#[event]
pub struct WithdrawalClaimedEvent {
    pub owner: Pubkey,
    pub withdrawal_id: u64,
    pub amount: u64,
    pub timestamp: i64,
}

#[event]
pub struct InstantWithdrawalEvent {
    pub owner: Pubkey,
    pub withdrawal_id: u64,
    pub amount: u64,
    pub fee: u64,
    pub payout: u64,
    pub timestamp: i64,
}

#[event]
pub struct WithdrawalCancelledEvent {
    pub owner: Pubkey,
    pub gateway: Pubkey,
    pub withdrawal_id: u64,
    pub amount: u64,
    pub is_delegate: bool,
    pub timestamp: i64,
}

#[event]
pub struct StakePaymentEvent {
    pub payer: Pubkey,
    pub gateway: Pubkey,
    pub amount: u64,
    pub is_delegate: bool,
    pub timestamp: i64,
}

#[event]
pub struct WithdrawalPaymentEvent {
    pub owner: Pubkey,
    pub withdrawal_id: u64,
    pub amount: u64,
    pub residue: u64,
    pub timestamp: i64,
}

/// Summary marker for a multi-source funding plan. Per-gateway and per-source
/// detail is exposed via the per-source `StakePaymentEvent` (Delegation +
/// OperatorStake), `WithdrawalPaymentEvent` (Withdrawal), and
/// `ResidueVaultCreatedEvent` (sub-min Delegation residue) emitted alongside.
#[event]
pub struct FundingPlanAppliedEvent {
    pub payer: Pubkey,
    pub total_funded: u64,
    pub source_count: u8,
    pub created_residue_vault: bool,
    pub timestamp: i64,
}

#[event]
pub struct ResidueVaultCreatedEvent {
    pub owner: Pubkey,
    pub withdrawal_id: u64,
    pub gateway: Pubkey,
    pub amount: u64,
    pub available_at: i64,
    pub timestamp: i64,
}

#[event]
pub struct EpochCreatedEvent {
    pub epoch_index: u64,
    pub start_timestamp: i64,
    pub end_timestamp: i64,
    pub timestamp: i64,
}

#[event]
pub struct EpochPrescribedEvent {
    pub epoch_index: u64,
    pub observer_count: u8,
    pub per_gateway_reward: u64,
    pub per_observer_reward: u64,
    pub timestamp: i64,
}

#[event]
pub struct ObservationSubmittedEvent {
    pub epoch_index: u64,
    pub observer: Pubkey,
    pub gateway_count: u16,
    pub report_tx_id: [u8; 32],
    pub timestamp: i64,
}

#[event]
pub struct EpochDistributedEvent {
    pub epoch_index: u64,
    pub gateways_processed: u32,
    pub total_eligible_rewards: u64,
    pub timestamp: i64,
}

// -----------------------------------------------------------------------------
// PR-6 fill-in events. Append-only. Do NOT mutate any of the 15 events above
// (ABI freeze; ADR-017). Field name + type + order is the wire encoding —
// once shipped, never changed.
// -----------------------------------------------------------------------------

/// Emitted by `update_gateway_settings` when one or more settings fields
/// were mutated. `fields_changed` is a u32 bitmask of `GATEWAY_SETTINGS_FIELD_*`
/// constants — kept compact (vs a Vec<String> of names) so events fit
/// comfortably under the per-tx log budget.
#[event]
pub struct GatewaySettingsUpdatedEvent {
    pub operator: Pubkey,
    pub fields_changed: u32,
    pub timestamp: i64,
}

/// Emitted by `update_observer_address` when the gateway's observer address
/// changes. Indexers can use this to refresh observer-keyed lookups.
#[event]
pub struct ObserverAddressUpdatedEvent {
    pub operator: Pubkey,
    pub new_observer: Pubkey,
    pub timestamp: i64,
}

/// Emitted by `increase_operator_stake`. Symmetric with
/// `WithdrawalCreatedEvent` (decrease side).
#[event]
pub struct OperatorStakeIncreasedEvent {
    pub operator: Pubkey,
    pub added: u64,
    pub new_total: u64,
    pub timestamp: i64,
}

/// Emitted by `decrease_delegate_stake`. Parallel to the existing
/// `DelegationEvent` (which fires on increase + claim flows). Indexers
/// can pair this with `WithdrawalCreatedEvent` for the post-state of
/// the locked tokens.
#[event]
pub struct DelegationDecreasedEvent {
    pub delegator: Pubkey,
    pub gateway: Pubkey,
    pub decrease: u64,
    pub new_total: u64,
    pub timestamp: i64,
}

/// Emitted by `close_empty_delegation` when the delegation PDA is
/// closed and rent reclaimed.
#[event]
pub struct DelegationClosedEvent {
    pub delegator: Pubkey,
    pub gateway: Pubkey,
    pub timestamp: i64,
}

/// Emitted by `redelegate_stake`. `amount` is the gross amount moved;
/// `fee` is the protocol cut (0% on first redelegation, then 10/20/.../60%
/// per redelegation_count, with a 7-day reset).
#[event]
pub struct RedelegationEvent {
    pub delegator: Pubkey,
    pub from_gateway: Pubkey,
    pub to_gateway: Pubkey,
    pub amount: u64,
    pub fee: u64,
    pub timestamp: i64,
}

/// Emitted by both `allow_delegate` (`allowed: true`) and
/// `disallow_delegate` (`allowed: false`) — a single event shape covers
/// both directions to keep the indexer schema flat.
#[event]
pub struct DelegateAllowlistedEvent {
    pub operator: Pubkey,
    pub delegate: Pubkey,
    pub allowed: bool,
    pub timestamp: i64,
}

/// Emitted by `set_allowlist_enabled` whenever the per-gateway allowlist
/// toggle flips.
#[event]
pub struct AllowlistToggledEvent {
    pub operator: Pubkey,
    pub enabled: bool,
    pub timestamp: i64,
}

/// Emitted by `set_epochs_enabled`. Note: enabling is instant (also
/// cancels any pending disable). Disabling is timelocked — this event
/// fires on the *intent* to toggle; the actual disable effect is
/// gated on `disable_at` inside `create_epoch`.
#[event]
pub struct EpochsToggledEvent {
    pub admin: Pubkey,
    pub enabled: bool,
    pub timestamp: i64,
}

/// Emitted EXACTLY ONCE per epoch by `tally_weights`, on the final batch
/// (the call that transitions `weights_tallied` from 0 to 1). Mid-batch
/// calls are silent. Mirrors the `EpochDistributedEvent` summary pattern.
#[event]
pub struct EpochWeightsTalliedEvent {
    pub epoch_index: u64,
    pub gateway_count: u32,
    pub total_weight: u64,
    pub timestamp: i64,
}

/// Emitted by `close_epoch` when the epoch PDA is closed. `rent_recovered`
/// is the lamport delta refunded to the caller (captured pre/post account
/// close). Marker for retention-window pruning in indexers.
#[event]
pub struct EpochClosedEvent {
    pub epoch_index: u64,
    pub rent_recovered: u64,
    pub timestamp: i64,
}

/// Emitted by `compound_delegation_rewards`. `compounded` is the pending
/// reward newly added to the delegation amount.
#[event]
pub struct RewardsCompoundedEvent {
    pub delegator: Pubkey,
    pub gateway: Pubkey,
    pub compounded: u64,
    pub timestamp: i64,
}

/// Emitted by `prune_gateway` (permissionless slash for failed gateways).
/// `slashed_amount` is the protocol-confiscated portion (0 if the gateway
/// had less than `min_operator_stake`).
#[event]
pub struct GatewayPrunedEvent {
    pub operator: Pubkey,
    pub pruner: Pubkey,
    pub slashed_amount: u64,
    pub timestamp: i64,
}

/// Emitted by `finalize_gone` when a Leaving gateway's PDA + registry
/// slot are reclaimed. Pairs with `GatewayPrunedEvent` / `GatewayLeavingEvent`
/// to signal "gateway gone, drop from caches."
#[event]
pub struct GatewayFinalizedEvent {
    pub operator: Pubkey,
    pub pruner: Pubkey,
    pub timestamp: i64,
}

/// Emitted by `finalize_migration` when the GAR migration window is
/// permanently closed. `gateway_count` is the registry count at finalize
/// time; `slot` is the runtime slot for downstream timestamping.
#[event]
pub struct GarMigrationFinalizedEvent {
    pub admin: Pubkey,
    pub gateway_count: u32,
    pub slot: u64,
    pub timestamp: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================
    // Phase 4: Constants Validation
    // =========================================

    #[test]
    fn withdrawal_period_30_days() {
        assert_eq!(WITHDRAWAL_LOCK_PERIOD, 30 * 86_400);
        assert_eq!(WITHDRAWAL_LOCK_PERIOD, 2_592_000);
    }

    #[test]
    fn leave_period_90_days() {
        assert_eq!(GATEWAY_LEAVE_PERIOD, 90 * 86_400);
        assert_eq!(GATEWAY_LEAVE_PERIOD, 7_776_000);
    }

    #[test]
    fn max_penalty_50pct() {
        assert_eq!(MAX_EXPEDITED_WITHDRAWAL_PENALTY, 500_000);
    }

    #[test]
    fn min_penalty_10pct() {
        assert_eq!(MIN_EXPEDITED_WITHDRAWAL_PENALTY, 100_000);
    }

    #[test]
    fn min_withdrawal_1_ario() {
        assert_eq!(MIN_EXPEDITED_WITHDRAWAL_AMOUNT, 1_000_000);
    }

    #[test]
    fn gateway_reward_rate_90pct() {
        assert_eq!(GATEWAY_OPERATOR_REWARD_RATE, 900_000);
    }

    #[test]
    fn observer_reward_rate_10pct() {
        assert_eq!(OBSERVER_REWARD_RATE, 100_000);
    }

    #[test]
    fn max_reward_rate_01pct() {
        assert_eq!(MAX_REWARD_RATE, 1_000);
    }

    #[test]
    fn min_reward_rate_005pct() {
        assert_eq!(MIN_REWARD_RATE, 500);
    }

    #[test]
    fn decay_start_365() {
        assert_eq!(REWARD_DECAY_START_EPOCH, 365);
    }

    #[test]
    fn decay_last_547() {
        assert_eq!(REWARD_DECAY_LAST_EPOCH, 547);
        assert_eq!(REWARD_DECAY_LAST_EPOCH, 365 + 182);
    }

    #[test]
    fn missed_obs_penalty_25pct() {
        assert_eq!(MISSED_OBSERVATION_PENALTY, 250_000);
    }

    #[test]
    fn max_delegate_share_95pct() {
        assert_eq!(MAX_DELEGATE_REWARD_SHARE, 9500);
    }

    #[test]
    fn min_operator_stake_20k() {
        assert_eq!(state::Gateway::MIN_OPERATOR_STAKE, 20_000_000_000);
    }

    #[test]
    fn epoch_duration_1_day() {
        assert_eq!(DEFAULT_EPOCH_DURATION, 86_400);
    }

    #[test]
    fn rate_scale_value() {
        assert_eq!(RATE_SCALE, 1_000_000);
    }

    // =========================================
    // 1D. Expedited Withdrawal Penalty Math Tests
    // =========================================

    /// Compute penalty rate using the same formula as instant_withdrawal
    fn compute_penalty_rate(
        elapsed: i64,
        total_period: i64,
        max_penalty: u64,
        min_penalty: u64,
    ) -> u64 {
        if total_period == 0 || elapsed >= total_period {
            return min_penalty;
        }
        let decay = (max_penalty.saturating_sub(min_penalty) as u128)
            .checked_mul(elapsed as u128)
            .unwrap_or(0)
            .checked_div(total_period as u128)
            .unwrap_or(0) as u64;
        let rate_after_decay = max_penalty.saturating_sub(decay);
        rate_after_decay.max(min_penalty).min(max_penalty)
    }

    #[test]
    fn penalty_at_start() {
        let rate = compute_penalty_rate(0, 2_592_000, 500_000, 100_000);
        assert_eq!(rate, 500_000); // 50%
    }

    #[test]
    fn penalty_at_halfway() {
        let rate = compute_penalty_rate(
            15 * 86_400, // 15 days
            30 * 86_400, // 30 days
            500_000,
            100_000,
        );
        // decay = 400_000 * 15 / 30 = 200_000
        // rate = 500_000 - 200_000 = 300_000
        assert_eq!(rate, 300_000); // 30%
    }

    #[test]
    fn penalty_near_end() {
        let total = 30 * 86_400i64;
        let elapsed = total - 1; // 1 second before end
        let rate = compute_penalty_rate(elapsed, total, 500_000, 100_000);
        // decay = 400_000 * (2591999) / 2592000 = ~399_999
        // rate = 500_000 - 399_999 = 100_001
        // clamped to max(100_001, 100_000) = 100_001
        assert!(rate >= 100_000 && rate <= 100_100);
    }

    #[test]
    fn penalty_at_full_period() {
        let rate = compute_penalty_rate(2_592_000, 2_592_000, 500_000, 100_000);
        assert_eq!(rate, 100_000); // min 10%
    }

    #[test]
    fn penalty_zero_period() {
        let rate = compute_penalty_rate(0, 0, 500_000, 100_000);
        assert_eq!(rate, 100_000); // min when period=0
    }

    // =========================================
    // 1G. Reward Distribution Math Tests
    // =========================================

    #[test]
    fn gateway_reward_split_calculation() {
        // Protocol balance = 500_000_000_000 (500B mARIO)
        // reward_rate = 1_000 (0.1%)
        // total_eligible = 500B * 1000 / 1_000_000 = 500_000_000 (500M)
        let protocol_balance: u128 = 500_000_000_000;
        let reward_rate: u128 = 1_000;
        let total_eligible = (protocol_balance * reward_rate / 1_000_000) as u64;
        assert_eq!(total_eligible, 500_000_000);
    }

    #[test]
    fn per_gateway_and_observer_reward() {
        let total_eligible: u128 = 500_000_000;
        let num_gateways: u128 = 5;
        let num_observers: u128 = 5;

        // gateway pool = 90%
        let gateway_pool = total_eligible * 900_000 / 1_000_000;
        let per_gateway = gateway_pool / num_gateways;
        assert_eq!(per_gateway, 90_000_000); // 90M each

        // observer pool = 10%
        let observer_pool = total_eligible * 100_000 / 1_000_000;
        let per_observer = observer_pool / num_observers;
        assert_eq!(per_observer, 10_000_000); // 10M each
    }

    // =========================================
    // 1H. Delegate Reward Split Tests
    // =========================================

    #[test]
    fn delegate_share_5pct() {
        // delegate_reward_share_ratio = 500 basis points = 5%
        // gateway_reward = 90_000_000
        let gateway_reward: u128 = 90_000_000;
        let share_ratio: u128 = 500; // basis points
        let delegate_total = gateway_reward * share_ratio / 10_000;
        let operator_reward = gateway_reward - delegate_total;
        assert_eq!(delegate_total, 4_500_000);
        assert_eq!(operator_reward, 85_500_000);
    }

    #[test]
    fn delegate_share_0pct() {
        let gateway_reward: u128 = 90_000_000;
        let share_ratio: u128 = 0;
        let delegate_total = gateway_reward * share_ratio / 10_000;
        assert_eq!(delegate_total, 0);
    }

    #[test]
    fn delegate_proportional_split() {
        let delegate_total: u128 = 4_500_000;
        let delegate_a_stake: u128 = 6_000_000_000; // 60%
        let delegate_b_stake: u128 = 4_000_000_000; // 40%
        let total_delegated = delegate_a_stake + delegate_b_stake;

        let a_reward = delegate_total * delegate_a_stake / total_delegated;
        let b_reward = delegate_total * delegate_b_stake / total_delegated;

        assert_eq!(a_reward, 2_700_000); // 60% of 4.5M
        assert_eq!(b_reward, 1_800_000); // 40% of 4.5M
    }
}
