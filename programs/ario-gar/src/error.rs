use anchor_lang::prelude::*;

#[error_code]
pub enum GarError {
    // =========================================
    // GATEWAY LIFECYCLE ERRORS
    // =========================================
    #[msg("Gateway already registered")]
    GatewayAlreadyRegistered,

    #[msg("Gateway not found")]
    GatewayNotFound,

    #[msg("Gateway is not in joined status")]
    GatewayNotJoined,

    #[msg("Gateway is in leaving state")]
    GatewayLeaving,

    #[msg("Gateway registry is full")]
    RegistryFull,

    #[msg("Gateway registry account already exists")]
    GatewayRegistryAlreadyExists,

    #[msg("Invalid FQDN format")]
    InvalidFqdn,

    #[msg("Invalid gateway label")]
    InvalidLabel,

    #[msg("Invalid reward share ratio - must be 0-95")]
    InvalidRewardShare,

    // =========================================
    // STAKING ERRORS
    // =========================================
    #[msg("Insufficient stake amount")]
    InsufficientStake,

    #[msg("Stake below minimum required")]
    StakeBelowMinimum,

    #[msg("Must leave network before withdrawing all stake")]
    MustLeaveFirst,

    #[msg("Invalid amount")]
    InvalidAmount,

    // =========================================
    // DELEGATION ERRORS
    // =========================================
    #[msg("Delegation not allowed for this gateway")]
    DelegationNotAllowed,

    #[msg("Delegation amount below gateway minimum")]
    DelegationBelowMinimum,

    #[msg("Delegate not in allowlist")]
    DelegateNotAllowed,

    #[msg("Delegation not found")]
    DelegationNotFound,

    #[msg("Delegate not eligible for reward distribution")]
    InvalidDelegateStatus,

    #[msg("Cannot redelegate to same gateway")]
    RedelegateSameGateway,

    #[msg("Cannot delegate to your own gateway - use increase_operator_stake instead")]
    CannotDelegateToSelf,

    // =========================================
    // WITHDRAWAL ERRORS
    // =========================================
    #[msg("Withdrawal not ready - lock period not elapsed")]
    WithdrawalNotReady,

    #[msg("Withdrawal not found")]
    WithdrawalNotFound,

    #[msg("Invalid withdrawal amount")]
    InvalidWithdrawalAmount,

    #[msg("Exit vault withdrawals cannot be expedited")]
    ExitVaultCannotBeExpedited,

    // =========================================
    // AUTHORIZATION ERRORS
    // =========================================
    #[msg("Not the gateway operator")]
    NotOperator,

    #[msg("Not the delegator")]
    NotDelegator,

    #[msg("Invalid owner")]
    InvalidOwner,

    #[msg("Unauthorized access")]
    Unauthorized,

    // =========================================
    // GENERAL ERRORS
    // =========================================
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,

    #[msg("Arithmetic underflow")]
    ArithmeticUnderflow,

    #[msg("Invalid parameter")]
    InvalidParameter,

    // =========================================
    // EPOCH ERRORS
    // =========================================
    #[msg("Epochs not enabled")]
    EpochsNotEnabled,

    #[msg("Epochs are already enabled — cannot mutate counter while cranker is live")]
    EpochsAlreadyEnabled,

    #[msg(
        "Epoch counter has already advanced past zero — admin_set_current_epoch_index is one-shot"
    )]
    EpochCounterAlreadyAdvanced,

    #[msg("Epoch not started yet")]
    EpochNotStarted,

    #[msg("Epoch has ended")]
    EpochEnded,

    #[msg("Epoch still in progress")]
    EpochInProgress,

    #[msg("Not a prescribed observer for this epoch")]
    NotPrescribedObserver,

    #[msg("Observer has already submitted observations")]
    AlreadyObserved,

    #[msg("Rewards already distributed")]
    RewardsAlreadyDistributed,

    #[msg("Reward distribution not complete")]
    DistributionIncomplete,

    #[msg("Invalid observation report")]
    InvalidObservation,

    #[msg("Gateway not eligible for epoch")]
    GatewayNotEligible,

    #[msg("Epoch already exists")]
    EpochAlreadyExists,

    #[msg("No more observers available to prescribe")]
    NoObserversAvailable,

    #[msg("No names available to prescribe")]
    NoNamesAvailable,

    #[msg("Invalid epoch index")]
    InvalidEpochIndex,

    #[msg("Composite weights have already been tallied for this epoch")]
    WeightsAlreadyTallied,

    #[msg("Composite weights have not been tallied yet")]
    WeightsNotTallied,

    #[msg("Invalid gateway account")]
    InvalidGatewayAccount,

    #[msg("Epoch prescriptions not yet complete")]
    PrescriptionsNotDone,

    #[msg("Epoch prescriptions already complete")]
    PrescriptionsAlreadyDone,

    #[msg("Invalid name registry account")]
    InvalidNameRegistry,

    #[msg("Epoch not yet closeable — must be distributed and past retention")]
    EpochNotCloseable,

    // =========================================
    // MIGRATION ERRORS
    // =========================================
    #[msg("Migration is not active")]
    MigrationInactive,

    #[msg("Migration has already been finalized")]
    MigrationAlreadyFinalized,

    #[msg("Invalid account data for migration import")]
    InvalidAccountData,

    #[msg("PDA derivation does not match target account")]
    InvalidPda,

    #[msg("Observer address already in use by another gateway")]
    ObserverAddressTaken,

    #[msg("Migration deadline has passed")]
    MigrationExpired,

    // =========================================
    // STAKE PAYMENT ERRORS
    // =========================================
    #[msg("Insufficient delegation for payment")]
    InsufficientDelegationForPayment,

    #[msg("Insufficient operator stake for payment")]
    InsufficientOperatorStakeForPayment,

    #[msg("Insufficient withdrawal balance for payment")]
    InsufficientWithdrawalForPayment,

    #[msg("Withdrawal vault still holds funds — cannot close")]
    WithdrawalNotDrained,

    // =========================================
    // FUNDING-PLAN ERRORS (pay_from_funding_plan)
    // =========================================
    #[msg("Funding plan is empty")]
    EmptyFundingPlan,

    #[msg("Funding plan exceeds the per-tx source cap")]
    TooManyFundingSources,

    #[msg("Sum of source amounts does not match the expected total")]
    FundingPlanAmountMismatch,

    #[msg("Funding plan source has zero amount")]
    ZeroFundingSourceAmount,

    #[msg("Delegation/OperatorStake source requires the gateway account")]
    MissingGatewayForFundingSource,

    #[msg("Balance source requires the payer's token account")]
    MissingPayerTokenAccountForFundingSource,

    #[msg("Funding plan declared more sources than were passed in remaining_accounts")]
    MissingFundingSourceAccount,

    #[msg("remaining_accounts contained more entries than the funding plan declared")]
    ExtraneousFundingSourceAccount,

    #[msg("Funding plan may include at most one Delegation source per gateway")]
    OnlyOneDelegationSource,

    #[msg("Funding plan may include at most one OperatorStake source")]
    OnlyOneOperatorStakeSource,

    #[msg("Funding plan exceeds the per-tx Delegation source cap")]
    TooManyDelegationSources,

    #[msg("Two or more sources reference the same gateway — aggregate them client-side")]
    DuplicateGatewayInSources,

    #[msg("residue_vault_count does not match the number of sub-min Delegation residues")]
    MismatchedResidueVaultCount,

    #[msg("Missing residue_vault slot in remaining_accounts")]
    MissingResidueVault,

    // =========================================
    // FINALIZE_GONE ERRORS
    // =========================================
    #[msg("finalize_gone: gateway is not in Leaving status")]
    GatewayNotLeaving,

    #[msg("finalize_gone: leave window has not yet expired")]
    LeaveWindowNotExpired,

    #[msg("finalize_gone: outstanding delegations must be claimed first")]
    DelegationsOutstanding,

    #[msg("close_epoch: observation PDAs must be closed first")]
    EpochObservationsNotClosed,

    #[msg(
        "Vault is protected (operator min-stake exit vault); cannot be \
         expedited or spent. Use claim_withdrawal after the lock expires."
    )]
    ProtectedVault,

    #[msg(
        "leave_network/prune_gateway: post-min stake is positive but \
         excess_withdrawal account was not supplied"
    )]
    MissingExcessWithdrawal,

    #[msg(
        "Supplied excess_withdrawal PDA does not match the expected derivation \
         (must be ['withdrawal', operator, withdrawal_counter.next_id + 1])"
    )]
    InvalidExcessWithdrawalPda,

    // =========================================
    // RESERVED — formerly admin-shrink (registry recovery), removed when
    // `devnet-shrunk` was retired. Kept (unused) to preserve GarError codes
    // so downstream decoders (cranker/observer) don't shift. Do NOT reuse.
    // =========================================
    #[msg("Reserved (formerly RegistryAlreadyShrunk)")]
    RegistryAlreadyShrunk,

    #[msg("Reserved (formerly ShrinkWouldLoseData)")]
    ShrinkWouldLoseData,

    // =========================================
    // SCHEMA MIGRATION ERRORS
    // =========================================
    #[msg("Account is already at the latest schema version")]
    AlreadyLatestVersion,

    #[msg("Unknown schema version — no migration path exists from this version")]
    UnknownSchemaVersion,

    // =========================================
    // DELEGATION LIFECYCLE ERRORS (Fix #6)
    // Appended at the end to keep existing error codes stable.
    // =========================================
    #[msg("Cannot re-enable delegation while delegates still have stake; crank claim_delegate_from_disabled_gateway first")]
    DelegatesStillActive,

    #[msg(
        "Cannot re-enable delegation until the disable cooldown (withdrawal period) has elapsed"
    )]
    DelegationCooldownActive,

    #[msg("Delegation must be disabled on this gateway for this operation")]
    DelegationNotDisabled,
}
