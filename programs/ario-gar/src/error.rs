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

    // =========================================
    // CLOSE_OBSERVATION RENT-REFUND ERROR
    // Appended at the end to keep existing error codes stable for
    // downstream decoders (cranker/observer).
    // =========================================
    #[msg("Supplied observer account does not match the observation's recorded observer")]
    WrongObserverAccount,

    // =========================================
    // EPOCH RENT-RECEIPT ERRORS (ADR-0029)
    // Appended at the end to keep existing error codes stable for
    // downstream decoders (cranker/observer).
    // =========================================
    // NB: keep #[msg] on ONE line. Anchor copies the literal into the IDL
    // verbatim, so a `\`-continued string leaks its escape and indentation
    // into the rendered message every downstream decoder and explorer shows.
    #[msg("This epoch has a rent receipt: close_epoch requires the receipt PDA and its recorded creator as writable remaining_accounts")]
    MissingEpochRentReceipt,

    #[msg(
        "Supplied epoch rent receipt is not this program's ['epoch_rent_receipt', epoch_index] PDA"
    )]
    InvalidEpochRentReceipt,

    #[msg("Supplied creator account does not match the rent receipt's recorded creator")]
    WrongEpochCreator,

    // One line, unwrapped: Anchor copies the literal into the IDL verbatim, so
    // a `\`-continued message reaches downstream decoders with the escape and
    // indentation still in it.
    #[msg("Epoch still exists — close_epoch is the path that refunds its rent to the creator")]
    EpochStillExists,

    // ADR-0032. Distinct from WeightsNotTallied on purpose: that one means a
    // gateway simply has no weights for this epoch and is owed nothing; this
    // one means a gateway that IS in the epoch's reward divisor has had its
    // weights destroyed by another epoch's tally. The epoch can no longer be
    // paid correctly and needs a human decision, not a retry.
    #[msg("A gateway in this epoch's reward set had its weights overwritten by another epoch's tally; this epoch can no longer be distributed correctly")]
    EpochWeightsClobbered,

    // ADR-0033. Only the live epoch (current_epoch_index - 1) may be tallied:
    // weights are per-tally rather than per-epoch, so tallying an older epoch
    // would overwrite the live epoch's weights and destroy its payout. A stale
    // epoch stays closeable by `admin_close_stale_epoch`, which carries no tally
    // requirement.
    #[msg("Only the live epoch may be tallied; tallying an older epoch would destroy the live epoch's weights")]
    EpochNoLongerLive,

    // ── APPEND-ONLY BELOW ──────────────────────────────────────────────────
    // ADR-035. Anchor assigns error codes by POSITION in this enum, so a
    // variant inserted anywhere above renumbers every variant after it and
    // breaks every client matching on the numeric code -- including the
    // mainnet-live EpochWeightsClobbered (6097) and EpochNoLongerLive (6098).
    // New variants go here, at the end, always.
    // `scripts/error-code-snapshot.mjs` enforces this in CI.

    // 6099 -- already published in develop's snapshot; do not move.
    #[msg("Gateway predates the 1.1.0 layout and cannot be migrated in place")]
    PreV110GatewayLayout,

    // 6100 -- ADR-0030.
    #[msg("Signer is neither the gateway operator nor its operations address")]
    NotGatewayAuthority,

    // 6101 -- ADR-0030. A gateway below 1.2.0 has no real operations_address:
    // the bytes after `version` are whatever an earlier, longer serialization
    // left behind. Run the permissionless `migrate_gateway` first.
    #[msg("Gateway has not been migrated to the layout that carries an operations address; run migrate_gateway first")]
    GatewayNotMigrated,

    // 6102 -- ADR-0034 / ADR-0036. The shared "latest epoch is finished"
    // predicate, enforced by `create_epoch` (an unfinished epoch must not be
    // superseded) and by `finalize_gone` (registry positions are frozen while
    // an epoch is unfinished). Finished means `rewards_distributed == 1`, or
    // the Epoch account no longer exists.
    #[msg("The latest epoch is not finished: distribute it, or write it off with admin_close_stale_epoch, before continuing")]
    LatestEpochUnfinished,

    // 6103 -- ADR-0034 / ADR-0036. The caller did not pass the latest Epoch
    // PDA in `remaining_accounts`, so the predicate above cannot be evaluated.
    // Distinct from `LatestEpochUnfinished` so an un-upgraded client gets a
    // diagnosis ("you are missing an account") rather than a false "the epoch
    // is unfinished".
    #[msg("The latest epoch's account must be supplied in remaining_accounts")]
    MissingLatestEpochAccount,

    // 6104 -- ADR-0037. `admin_reconcile_delegated_stake` guards against a
    // stale plan: the gateway's counter moved between the off-chain read and
    // this call. Re-read and retry.
    #[msg(
        "Gateway's delegated-stake counter does not match the expected value; the plan is stale"
    )]
    StaleDelegatedStakeCounter,

    // 6105 -- ADR-0037. `expected_counter - Σ delegation.amount` did not equal
    // `expected_removed`, or `expected_removed` was zero. This is what makes a
    // missing Delegation account fail closed: the two sides of the plan come
    // from independent sources (getProgramAccounts and the genesis snapshot)
    // and must agree. Also rejects any call that would RAISE the counter.
    #[msg("Reconcile amount does not agree with the delegations supplied; the counter may only be lowered to their sum")]
    DelegationReconcileMismatch,

    // 6106 -- ADR-0037. The same Delegation account was passed twice, which
    // would double-count its amount and under-remove from the counter.
    #[msg("A delegation account was supplied more than once")]
    DuplicateDelegationAccount,

    // 6107 -- ADR-0037. A `remaining_accounts` entry was not a Delegation of
    // this gateway at its canonical PDA: wrong owner, wrong discriminator,
    // undeserializable, a different gateway's delegation, or an off-curve
    // address.
    #[msg("A supplied account is not a canonical Delegation account of this gateway")]
    InvalidDelegationAccount,

    // 6108 -- ADR-0037. `admin_resync_supply_counters` guards against a stale
    // read the same way the reconcile does: both expected values must match
    // what is stored before either is overwritten.
    #[msg("Supply counters do not match the expected values; the resync plan is stale")]
    StaleSupplyCounters,
}
