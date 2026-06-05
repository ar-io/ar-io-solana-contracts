use anchor_lang::prelude::*;
use anchor_lang::system_program::{self as anchor_system_program, Allocate, Assign, Transfer};
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::error::GarError;
use crate::state::*;
use crate::{
    FundingPlanAppliedEvent, ResidueVaultCreatedEvent, StakePaymentEvent, WithdrawalPaymentEvent,
};

/// Deduct from a delegator's delegation and transfer directly to protocol treasury.
/// No withdrawal vault is created — tokens go from stake pool to treasury immediately.
/// Designed for CPI from ario-arns (fund ArNS purchases from delegated stake).
/// Safe for direct calls: destination is always protocol_token_account (treasury),
/// so direct calls are effectively voluntary donations to the protocol.
pub fn deduct_delegation_for_payment(
    ctx: Context<DeductDelegationForPayment>,
    amount: u64,
) -> Result<()> {
    require!(amount > 0, GarError::InvalidAmount);

    let gateway = &mut ctx.accounts.gateway;
    let delegation = &mut ctx.accounts.delegation;

    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayNotJoined
    );

    // Settle pending rewards before deduction (same as decrease_delegate_stake)
    settle_delegate_rewards(gateway, delegation);

    require!(
        delegation.amount >= amount,
        GarError::InsufficientDelegationForPayment
    );

    let remaining = delegation
        .amount
        .checked_sub(amount)
        .ok_or(GarError::ArithmeticUnderflow)?;

    // Same invariant as decrease_delegate_stake: remaining must be 0 or >= min
    require!(
        remaining == 0 || remaining >= gateway.settings.min_delegation_amount,
        GarError::DelegationBelowMinimum
    );

    delegation.amount = remaining;
    gateway.total_delegated_stake = gateway
        .total_delegated_stake
        .checked_sub(amount)
        .ok_or(GarError::ArithmeticUnderflow)?;

    // Transfer from stake pool to protocol treasury using Settings PDA as authority
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
    token::transfer(cpi_ctx, amount)?;

    emit!(StakePaymentEvent {
        payer: ctx.accounts.delegator.key(),
        gateway: gateway.operator,
        amount,
        is_delegate: true,
        timestamp: Clock::get()?.unix_timestamp,
    });

    // Supply counter: delegated stake paid to protocol
    let settings = &mut ctx.accounts.settings;
    settings.total_delegated = settings.total_delegated.saturating_sub(amount);

    Ok(())
}

/// Deduct from a gateway operator's stake and transfer directly to protocol treasury.
/// No withdrawal vault is created — tokens go from stake pool to treasury immediately.
/// Operator cannot go below min_operator_stake via payment (preserves gateway viability).
pub fn deduct_operator_stake_for_payment(
    ctx: Context<DeductOperatorStakeForPayment>,
    amount: u64,
) -> Result<()> {
    require!(amount > 0, GarError::InvalidAmount);

    let gateway = &mut ctx.accounts.gateway;

    require!(
        gateway.status == GatewayStatus::Joined,
        GarError::GatewayNotJoined
    );

    require!(
        gateway.operator_stake >= amount,
        GarError::InsufficientOperatorStakeForPayment
    );

    let remaining = gateway
        .operator_stake
        .checked_sub(amount)
        .ok_or(GarError::ArithmeticUnderflow)?;

    // Cannot go below min_operator_stake via payment — unlike decrease_operator_stake
    // which allows 0 for leaving gateways, payment always preserves gateway viability
    require!(
        remaining >= ctx.accounts.settings.min_operator_stake,
        GarError::StakeBelowMinimum
    );

    gateway.operator_stake = remaining;

    // Transfer from stake pool to protocol treasury using Settings PDA as authority
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
    token::transfer(cpi_ctx, amount)?;

    emit!(StakePaymentEvent {
        payer: ctx.accounts.operator.key(),
        gateway: gateway.operator,
        amount,
        is_delegate: false,
        timestamp: Clock::get()?.unix_timestamp,
    });

    // Supply counter: operator stake paid to protocol
    let settings = &mut ctx.accounts.settings;
    settings.total_staked = settings.total_staked.saturating_sub(amount);

    Ok(())
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
pub struct DeductDelegationForPayment<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [GATEWAY_SEED, gateway.operator.as_ref()],
        bump = gateway.bump,
    )]
    pub gateway: Account<'info, Gateway>,

    #[account(
        mut,
        seeds = [DELEGATION_SEED, gateway.operator.as_ref(), delegator.key().as_ref()],
        bump = delegation.bump,
        constraint = delegation.delegator == delegator.key() @ GarError::NotDelegator,
    )]
    pub delegation: Account<'info, Delegation>,

    #[account(
        mut,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == settings.protocol_token_account @ GarError::InvalidParameter,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// The delegator must sign (authorizes deduction from their own stake)
    pub delegator: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

#[derive(Accounts)]
pub struct DeductOperatorStakeForPayment<'info> {
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

    #[account(
        mut,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == settings.protocol_token_account @ GarError::InvalidParameter,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// The operator must sign (authorizes deduction from their own stake)
    pub operator: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

/// Deduct from an unlocked or still-locked Withdrawal vault and transfer to the
/// protocol treasury. Mutates only `withdrawal.amount`; never closes the vault
/// (use [`close_drained_withdrawal`] for cleanup once `amount == 0`).
///
/// Gateway-status-independent: unlike `cancel_withdrawal` (which re-stakes and
/// therefore requires `Joined`), this only moves tokens out of the stake pool
/// to the treasury. `is_exit_vault` and `is_delegate` flags don't affect
/// eligibility.
///
/// Partial drain: residue stays in the vault with the original `available_at`.
/// No minimum-residue constraint — `min_expedited_withdrawal_amount` only
/// gates `instant_withdrawal`, not payment.
pub fn deduct_withdrawal_for_payment(
    ctx: Context<DeductWithdrawalForPayment>,
    amount: u64,
) -> Result<()> {
    require!(amount > 0, GarError::InvalidAmount);

    let withdrawal = &mut ctx.accounts.withdrawal;

    // Lua-parity gate: the protected operator min-stake exit vault is
    // off-limits to fund-from drainage. Mirrors the absence of operator
    // exit vaults from Lua's `planVaultsDrawdown` (which only iterates
    // delegate vaults). Excess leave/prune vaults remain spendable.
    require!(!withdrawal.is_protected, GarError::ProtectedVault);

    require!(
        withdrawal.amount >= amount,
        GarError::InsufficientWithdrawalForPayment
    );

    let remaining = withdrawal
        .amount
        .checked_sub(amount)
        .ok_or(GarError::ArithmeticUnderflow)?;
    withdrawal.amount = remaining;

    let owner_key = withdrawal.owner;
    let withdrawal_id = withdrawal.withdrawal_id;

    // Transfer from stake pool to protocol treasury using Settings PDA as authority
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
    token::transfer(cpi_ctx, amount)?;

    emit!(WithdrawalPaymentEvent {
        owner: owner_key,
        withdrawal_id,
        amount,
        residue: remaining,
        timestamp: Clock::get()?.unix_timestamp,
    });

    // Supply counter: withdrawn tokens leave the system via payment
    let settings = &mut ctx.accounts.settings;
    settings.total_withdrawn = settings.total_withdrawn.saturating_sub(amount);

    Ok(())
}

#[derive(Accounts)]
pub struct DeductWithdrawalForPayment<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    #[account(
        mut,
        seeds = [WITHDRAWAL_SEED, owner.key().as_ref(), &withdrawal.withdrawal_id.to_le_bytes()],
        bump = withdrawal.bump,
        constraint = withdrawal.owner == owner.key() @ GarError::InvalidOwner,
    )]
    pub withdrawal: Account<'info, Withdrawal>,

    #[account(
        mut,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == settings.protocol_token_account @ GarError::InvalidParameter,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// The withdrawal owner must sign (authorizes payment from their vault)
    pub owner: Signer<'info>,

    pub token_program: Program<'info, Token>,
}

// =========================================================================
// pay_from_funding_plan — multi-source composite payment (Phase 1.5)
// =========================================================================
//
// Lua-faithful port of `gar.applyFundingPlan` (ar-io-network-process/src/gar.lua:1629).
// One ix dispatches across N source kinds (Balance / Delegation / OperatorStake /
// Withdrawal), aggregates the bookkeeping, and issues at most two SPL transfers
// (one from the stake pool, one from the payer's ATA). When a Delegation source
// drains to a sub-`min_delegation_amount` residue, a fresh Withdrawal vault is
// auto-created for the residue — matching Lua's behavior and giving users a
// clean way to spend mixed sources without orphaning dust below the gateway
// minimum.

/// Tagged enum describing one source in a funding plan.
///
/// Multi-gateway: each source carries its own gateway PDA in remaining_accounts
/// (no single shared `gateway` slot). Each source consumes a known number of
/// `remaining_accounts` slots in declaration order — the handler iterates and
/// slices. See `docs/MULTI_GATEWAY_FUNDING_PLAN.md` for the full layout.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FundingSourceKind {
    /// Pull from the payer's SPL ATA. Consumes 0 remaining_accounts; uses the
    /// fixed `payer_token_account` slot (which must be Some).
    Balance,
    /// Pull from a Delegation on a specific gateway. Consumes 2
    /// remaining_accounts in order: [gateway PDA (mut), delegation PDA (mut)].
    /// Each Delegation source independently picks its gateway — multi-gateway
    /// plans pass distinct gateway pubkeys for distinct Delegation sources.
    Delegation,
    /// Pull from operator stake on a specific gateway. Caller must be
    /// `gateway.operator`. Consumes 1 remaining_account: [gateway PDA (mut)].
    /// Solana extension to Lua — Lua's funding plans never touch operator
    /// stake. Sub-min residue is REJECTED (preserves gateway viability),
    /// unlike delegation which auto-vaults.
    OperatorStake,
    /// Pull from a Withdrawal PDA owned by the payer. Gateway-independent
    /// (the global stake pool holds the tokens; `withdrawal.gateway` is
    /// metadata). Consumes 1 remaining_account: [withdrawal PDA (mut)].
    Withdrawal,
}

/// One entry in the funding plan vector.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct FundingSourceSpec {
    pub kind: FundingSourceKind,
    pub amount: u64,
}

/// Apply a multi-source funding plan, transferring `expected_total` mARIO into
/// the protocol treasury. Multi-gateway: each Delegation/OperatorStake source
/// carries its own gateway PDA via `remaining_accounts`.
///
/// Designed for CPI from ArNS / primary-name `_from_funding_plan` wrappers, but
/// also callable directly (bookkeeping ends up in the protocol treasury either
/// way — direct calls are voluntary donations).
///
/// `remaining_accounts` layout:
///   For each source in declaration order:
///     - Balance       → 0 entries
///     - Delegation    → 2 entries: [gateway_pda (mut), delegation_pda (mut)]
///     - OperatorStake → 1 entry:  [gateway_pda (mut)]
///     - Withdrawal    → 1 entry:  [withdrawal_pda (mut)]
///   Followed by `residue_vault_count` entries — residue Withdrawal PDAs in
///   Delegation-declaration order, one per Delegation source that drains to
///   sub-`min_delegation_amount`.
///
/// Caps:
///   - `MAX_FUNDING_SOURCES` (5) — total source count
///   - `MAX_DELEGATION_SOURCES` (3) — Delegation sub-cap (bounds residue overhead)
///   - one Delegation/OperatorStake per gateway per call
///     (rejected with `DuplicateGatewayInSources`; SDK aggregates client-side)
pub fn pay_from_funding_plan<'info>(
    ctx: Context<'_, '_, 'info, 'info, PayFromFundingPlan<'info>>,
    sources: Vec<FundingSourceSpec>,
    expected_total: u64,
    residue_vault_count: u8,
) -> Result<()> {
    let event_now = Clock::get()?.unix_timestamp;
    require!(!sources.is_empty(), GarError::EmptyFundingPlan);
    require!(
        sources.len() <= MAX_FUNDING_SOURCES,
        GarError::TooManyFundingSources
    );

    // Pre-flight: count Delegations + Operator stakes for cap enforcement.
    let mut delegation_count = 0usize;
    let mut operator_stake_count = 0usize;
    for s in &sources {
        match s.kind {
            FundingSourceKind::Delegation => delegation_count += 1,
            FundingSourceKind::OperatorStake => operator_stake_count += 1,
            _ => {}
        }
    }
    require!(
        delegation_count <= MAX_DELEGATION_SOURCES,
        GarError::TooManyDelegationSources
    );
    // OperatorStake stays single-per-call; the user can only ever operate one
    // gateway, and OperatorStake from another payer's gateway would fail the
    // `gateway.operator == payer` check anyway. This guard is defense-in-depth.
    require!(
        operator_stake_count <= 1,
        GarError::OnlyOneOperatorStakeSource
    );

    // Sum invariant: caller computes `expected_total` from cost; on-chain
    // verifies the source-amount Vec sums to it. Caller can't sneak in a
    // larger draw than the wrapper ix asked for.
    let mut computed_total: u64 = 0;
    for s in &sources {
        require!(s.amount > 0, GarError::ZeroFundingSourceAmount);
        computed_total = computed_total
            .checked_add(s.amount)
            .ok_or(GarError::ArithmeticOverflow)?;
    }
    require!(
        computed_total == expected_total,
        GarError::FundingPlanAmountMismatch
    );

    let mut remaining_iter = ctx.remaining_accounts.iter();
    let mut balance_drain: u64 = 0;
    let mut stake_pool_drain: u64 = 0;
    // Supply counter accumulators — aggregated across all sources, applied once.
    let mut counter_delegated_sub: u64 = 0;
    let mut counter_staked_sub: u64 = 0;
    let mut counter_withdrawn_sub: u64 = 0;
    // Per-Delegation residue tracking: (gateway_operator, residue_amount) in
    // Delegation-declaration order. Used to consume the trailing residue_vault
    // slots after source iteration.
    let mut residue_targets: Vec<(Pubkey, u64)> = Vec::with_capacity(MAX_DELEGATION_SOURCES);
    // Gateways already touched by Delegation/OperatorStake sources. We reject
    // duplicates rather than aggregating because aggregation has subtle
    // settle-rewards interactions (each settle_delegate_rewards call mutates
    // the gateway accumulator); the SDK aggregates client-side instead.
    let mut gateways_seen: Vec<Pubkey> = Vec::with_capacity(MAX_DELEGATION_SOURCES + 1);
    let payer_key = ctx.accounts.payer.key();

    for source in &sources {
        match source.kind {
            FundingSourceKind::Balance => {
                require!(
                    ctx.accounts.payer_token_account.is_some(),
                    GarError::MissingPayerTokenAccountForFundingSource
                );
                balance_drain = balance_drain
                    .checked_add(source.amount)
                    .ok_or(GarError::ArithmeticOverflow)?;
            }

            FundingSourceKind::Delegation => {
                let gateway_info = remaining_iter
                    .next()
                    .ok_or(GarError::MissingFundingSourceAccount)?;
                let delegation_info = remaining_iter
                    .next()
                    .ok_or(GarError::MissingFundingSourceAccount)?;

                let mut gateway = Account::<Gateway>::try_from(gateway_info)?;
                // PDA-validate the gateway against its own seeds. `Account<Gateway>`
                // pins the program-owner; this re-derivation pins it to a real
                // Gateway PDA (no spoofed gateway accounts).
                let (expected_gateway_pda, _) = Pubkey::find_program_address(
                    &[GATEWAY_SEED, gateway.operator.as_ref()],
                    ctx.program_id,
                );
                require!(
                    gateway_info.key() == expected_gateway_pda,
                    GarError::InvalidGatewayAccount
                );
                require!(
                    !gateways_seen.contains(&gateway.operator),
                    GarError::DuplicateGatewayInSources
                );
                gateways_seen.push(gateway.operator);
                require!(
                    gateway.status == GatewayStatus::Joined,
                    GarError::GatewayNotJoined
                );

                let mut delegation = Account::<Delegation>::try_from(delegation_info)?;
                let (expected_pda, _) = Pubkey::find_program_address(
                    &[
                        DELEGATION_SEED,
                        gateway.operator.as_ref(),
                        payer_key.as_ref(),
                    ],
                    ctx.program_id,
                );
                require!(
                    delegation_info.key() == expected_pda,
                    GarError::InvalidParameter
                );
                require!(delegation.delegator == payer_key, GarError::NotDelegator);

                // Settle pending rewards before deduction (matches the existing
                // single-source path; per-Delegation, since each gateway has
                // its own reward accumulator).
                settle_delegate_rewards(&mut gateway, &mut delegation);

                require!(
                    delegation.amount >= source.amount,
                    GarError::InsufficientDelegationForPayment
                );

                let post = delegation
                    .amount
                    .checked_sub(source.amount)
                    .ok_or(GarError::ArithmeticUnderflow)?;
                let min = gateway.settings.min_delegation_amount;

                if post == 0 || post >= min {
                    delegation.amount = post;
                    gateway.total_delegated_stake = gateway
                        .total_delegated_stake
                        .checked_sub(source.amount)
                        .ok_or(GarError::ArithmeticUnderflow)?;
                } else {
                    // Sub-min residue → auto-vault. Drain the delegation entirely;
                    // residue is moved into a new Withdrawal PDA from the trailing
                    // residue_vault sub-array of remaining_accounts. Both the
                    // deduction AND the residue leave delegated stake; the residue
                    // tokens stay in the stake pool until claim_withdrawal.
                    residue_targets.push((gateway.operator, post));
                    delegation.amount = 0;
                    let combined = source
                        .amount
                        .checked_add(post)
                        .ok_or(GarError::ArithmeticOverflow)?;
                    gateway.total_delegated_stake = gateway
                        .total_delegated_stake
                        .checked_sub(combined)
                        .ok_or(GarError::ArithmeticUnderflow)?;
                }

                let gateway_operator = gateway.operator;
                delegation.exit(ctx.program_id)?;
                gateway.exit(ctx.program_id)?;
                stake_pool_drain = stake_pool_drain
                    .checked_add(source.amount)
                    .ok_or(GarError::ArithmeticOverflow)?;

                emit!(StakePaymentEvent {
                    payer: payer_key,
                    gateway: gateway_operator,
                    amount: source.amount,
                    is_delegate: true,
                    timestamp: event_now,
                });
                counter_delegated_sub = counter_delegated_sub.saturating_add(source.amount);
            }

            FundingSourceKind::OperatorStake => {
                let gateway_info = remaining_iter
                    .next()
                    .ok_or(GarError::MissingFundingSourceAccount)?;
                let mut gateway = Account::<Gateway>::try_from(gateway_info)?;
                let (expected_gateway_pda, _) = Pubkey::find_program_address(
                    &[GATEWAY_SEED, gateway.operator.as_ref()],
                    ctx.program_id,
                );
                require!(
                    gateway_info.key() == expected_gateway_pda,
                    GarError::InvalidGatewayAccount
                );
                require!(
                    !gateways_seen.contains(&gateway.operator),
                    GarError::DuplicateGatewayInSources
                );
                gateways_seen.push(gateway.operator);
                require!(
                    gateway.status == GatewayStatus::Joined,
                    GarError::GatewayNotJoined
                );
                require!(gateway.operator == payer_key, GarError::NotOperator);
                require!(
                    gateway.operator_stake >= source.amount,
                    GarError::InsufficientOperatorStakeForPayment
                );

                let post = gateway
                    .operator_stake
                    .checked_sub(source.amount)
                    .ok_or(GarError::ArithmeticUnderflow)?;
                // HARD REJECT sub-min residue. Operator stake going below min
                // would tank the gateway's epoch-eligibility — different
                // rationale from delegation, where a sub-min residue auto-vaults.
                require!(
                    post >= ctx.accounts.settings.min_operator_stake,
                    GarError::StakeBelowMinimum
                );
                gateway.operator_stake = post;
                let gateway_operator = gateway.operator;
                gateway.exit(ctx.program_id)?;

                stake_pool_drain = stake_pool_drain
                    .checked_add(source.amount)
                    .ok_or(GarError::ArithmeticOverflow)?;

                emit!(StakePaymentEvent {
                    payer: payer_key,
                    gateway: gateway_operator,
                    amount: source.amount,
                    is_delegate: false,
                    timestamp: event_now,
                });
                counter_staked_sub = counter_staked_sub.saturating_add(source.amount);
            }

            FundingSourceKind::Withdrawal => {
                let withdrawal_info = remaining_iter
                    .next()
                    .ok_or(GarError::MissingFundingSourceAccount)?;
                require!(
                    withdrawal_info.owner == ctx.program_id,
                    GarError::InvalidParameter
                );
                let mut withdrawal = Account::<Withdrawal>::try_from(withdrawal_info)?;

                require!(withdrawal.owner == payer_key, GarError::InvalidOwner);
                let (expected_pda, _) = Pubkey::find_program_address(
                    &[
                        WITHDRAWAL_SEED,
                        withdrawal.owner.as_ref(),
                        &withdrawal.withdrawal_id.to_le_bytes(),
                    ],
                    ctx.program_id,
                );
                require!(
                    withdrawal_info.key() == expected_pda,
                    GarError::InvalidParameter
                );
                require!(
                    withdrawal.amount >= source.amount,
                    GarError::InsufficientWithdrawalForPayment
                );

                withdrawal.amount = withdrawal
                    .amount
                    .checked_sub(source.amount)
                    .ok_or(GarError::ArithmeticUnderflow)?;
                let withdrawal_id = withdrawal.withdrawal_id;
                let post_amount = withdrawal.amount;
                withdrawal.exit(ctx.program_id)?;

                stake_pool_drain = stake_pool_drain
                    .checked_add(source.amount)
                    .ok_or(GarError::ArithmeticOverflow)?;

                emit!(WithdrawalPaymentEvent {
                    owner: payer_key,
                    withdrawal_id,
                    amount: source.amount,
                    residue: post_amount,
                    timestamp: event_now,
                });
                counter_withdrawn_sub = counter_withdrawn_sub.saturating_add(source.amount);
            }
        }
    }

    // Validate the residue_vault_count arg against the actual residues found.
    // Caller must pre-compute exactly which Delegations will go sub-min and
    // pass that many vault PDAs in the trailing slots.
    require!(
        residue_targets.len() == residue_vault_count as usize,
        GarError::MismatchedResidueVaultCount
    );

    // SPL transfers — at most two per call (one from stake pool, one from payer ATA).
    if stake_pool_drain > 0 {
        let settings_bump = ctx.accounts.settings.bump;
        let signer_seeds: &[&[&[u8]]] = &[&[SETTINGS_SEED, &[settings_bump]]];
        token::transfer(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.to_account_info(),
                SplTransfer {
                    from: ctx.accounts.stake_token_account.to_account_info(),
                    to: ctx.accounts.protocol_token_account.to_account_info(),
                    authority: ctx.accounts.settings.to_account_info(),
                },
                signer_seeds,
            ),
            stake_pool_drain,
        )?;
    }
    if balance_drain > 0 {
        // Unwrap is safe — we already required Some when walking sources.
        let payer_ata = ctx.accounts.payer_token_account.as_ref().unwrap();
        token::transfer(
            CpiContext::new(
                ctx.accounts.token_program.to_account_info(),
                SplTransfer {
                    from: payer_ata.to_account_info(),
                    to: ctx.accounts.protocol_token_account.to_account_info(),
                    authority: ctx.accounts.payer.to_account_info(),
                },
            ),
            balance_drain,
        )?;
    }

    // Auto-vault each residue from its trailing residue_vault slot. PDAs are
    // derived sequentially from `withdrawal_counter.next_id`, advancing the
    // counter by exactly `residue_targets.len()`. Mirrors the
    // ClaimDelegateFromLeavingGateway init pattern (delegate.rs:574-590) but
    // does the create_account + Borsh write manually because the slot count
    // is variable.
    {
        let counter = &mut ctx.accounts.withdrawal_counter;
        if counter.bump == 0 {
            counter.owner = ctx.accounts.payer.key();
            counter.bump = ctx.bumps.withdrawal_counter;
            counter.version = WITHDRAWAL_COUNTER_VERSION;
        }
    }
    let now = Clock::get()?.unix_timestamp;
    let withdrawal_period = ctx.accounts.settings.withdrawal_period;
    for (gateway_operator, residue) in residue_targets.iter() {
        let new_vault_info = remaining_iter.next().ok_or(GarError::MissingResidueVault)?;

        let withdrawal_id = {
            let counter = &mut ctx.accounts.withdrawal_counter;
            let id = counter.next_id;
            counter.next_id = counter
                .next_id
                .checked_add(1)
                .ok_or(GarError::ArithmeticOverflow)?;
            id
        };
        let withdrawal_id_bytes = withdrawal_id.to_le_bytes();

        let (expected_pda, vault_bump) = Pubkey::find_program_address(
            &[WITHDRAWAL_SEED, payer_key.as_ref(), &withdrawal_id_bytes],
            ctx.program_id,
        );
        require!(
            new_vault_info.key() == expected_pda,
            GarError::InvalidParameter
        );
        require!(new_vault_info.data_is_empty(), GarError::InvalidParameter);

        let lamports_required = Rent::get()?.minimum_balance(Withdrawal::SIZE);
        let signer_seeds: &[&[&[u8]]] = &[&[
            WITHDRAWAL_SEED,
            payer_key.as_ref(),
            &withdrawal_id_bytes,
            &[vault_bump],
        ]];
        // Lamport-griefing defense (Solana sec checklist #12): an attacker
        // can predict the next residue PDA from `WithdrawalCounter.next_id`
        // and pre-fund it with 1 lamport. `system_program::create_account`
        // would then fail with AccountAlreadyInUse, DoS-ing the user's
        // sub-min draw. Mirror Anchor's `init` constraint behaviour: top
        // up the deficit (if any), then Allocate + Assign — both of which
        // tolerate pre-existing lamports.
        let existing = new_vault_info.lamports();
        if existing < lamports_required {
            let deficit = lamports_required - existing;
            anchor_system_program::transfer(
                CpiContext::new(
                    ctx.accounts.system_program.to_account_info(),
                    Transfer {
                        from: ctx.accounts.payer.to_account_info(),
                        to: new_vault_info.clone(),
                    },
                ),
                deficit,
            )?;
        }
        anchor_system_program::allocate(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Allocate {
                    account_to_allocate: new_vault_info.clone(),
                },
                signer_seeds,
            ),
            Withdrawal::SIZE as u64,
        )?;
        anchor_system_program::assign(
            CpiContext::new_with_signer(
                ctx.accounts.system_program.to_account_info(),
                Assign {
                    account_to_assign: new_vault_info.clone(),
                },
                signer_seeds,
            ),
            ctx.program_id,
        )?;

        let available_at = now
            .checked_add(withdrawal_period)
            .ok_or(GarError::ArithmeticOverflow)?;
        let withdrawal_state = Withdrawal {
            owner: payer_key,
            withdrawal_id,
            gateway: *gateway_operator,
            amount: *residue,
            created_at: now,
            available_at,
            // Delegation residues are always delegate-side and never exit
            // vaults (the gateway is still Joined — sub-min was reached
            // inside a fund-from payment, not via leave_network/prune).
            // Not protected — auto-vault residue behaves like any other
            // delegate withdrawal (claim after lock, expedite-able).
            is_delegate: true,
            is_exit_vault: false,
            is_protected: false,
            bump: vault_bump,
            version: WITHDRAWAL_VERSION,
        };
        let mut data = new_vault_info.try_borrow_mut_data()?;
        let mut cursor = std::io::Cursor::new(&mut data[..]);
        withdrawal_state
            .try_serialize(&mut cursor)
            .map_err(|_| GarError::InvalidParameter)?;

        emit!(ResidueVaultCreatedEvent {
            owner: payer_key,
            withdrawal_id,
            gateway: *gateway_operator,
            amount: *residue,
            available_at,
            timestamp: event_now,
        });
    }

    // After consuming per-source slots + residue slots, no leftovers should
    // remain. Catches off-by-one bugs in caller's account list.
    require!(
        remaining_iter.next().is_none(),
        GarError::ExtraneousFundingSourceAccount
    );

    // Supply counters: apply accumulated drains + residue vault transitions.
    // Residue vaults move tokens from delegated → withdrawn (the delegation
    // drain already subtracted the full amount from total_delegated; the
    // residue portion was re-materialized as a Withdrawal vault above).
    let mut counter_withdrawn_add: u64 = 0;
    for (_gw, residue) in &residue_targets {
        counter_withdrawn_add = counter_withdrawn_add.saturating_add(*residue);
    }
    let settings = &mut ctx.accounts.settings;
    settings.total_delegated = settings
        .total_delegated
        .saturating_sub(counter_delegated_sub);
    settings.total_staked = settings.total_staked.saturating_sub(counter_staked_sub);
    settings.total_withdrawn = settings
        .total_withdrawn
        .saturating_sub(counter_withdrawn_sub)
        .checked_add(counter_withdrawn_add)
        .ok_or(GarError::ArithmeticOverflow)?;

    emit!(FundingPlanAppliedEvent {
        payer: payer_key,
        total_funded: expected_total,
        source_count: sources.len() as u8,
        created_residue_vault: !residue_targets.is_empty(),
        timestamp: event_now,
    });

    Ok(())
}

#[derive(Accounts)]
pub struct PayFromFundingPlan<'info> {
    #[account(
        mut,
        seeds = [SETTINGS_SEED],
        bump = settings.bump,
    )]
    pub settings: Account<'info, GatewaySettings>,

    // Multi-gateway: per-source gateway PDAs live in `remaining_accounts` in
    // declaration order, NOT in a fixed slot here. This lets one funding plan
    // draw from delegations on multiple gateways in a single tx — closing the
    // last BD-076 gap. See `docs/MULTI_GATEWAY_FUNDING_PLAN.md`.
    #[account(
        mut,
        constraint = stake_token_account.key() == settings.stake_token_account @ GarError::InvalidParameter,
    )]
    pub stake_token_account: Account<'info, TokenAccount>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == settings.protocol_token_account @ GarError::InvalidParameter,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// Required when any Balance source is present (the payer's ATA).
    /// Owner is verified at SPL-transfer time (Token Program enforces it).
    #[account(mut)]
    pub payer_token_account: Option<Account<'info, TokenAccount>>,

    /// Funder + signer — must match the source PDAs' owner field for every
    /// Delegation / Withdrawal / OperatorStake source.
    #[account(mut)]
    pub payer: Signer<'info>,

    pub token_program: Program<'info, Token>,

    /// Always passed. `init_if_needed` so first-time vault creators don't
    /// need a separate setup tx. Cost is ~0.0034 SOL once per user (paid
    /// even for plans that don't end up creating a residue vault — the
    /// alternative is doubling the ix surface, which we prefer to avoid).
    #[account(
        init_if_needed,
        payer = payer,
        space = WithdrawalCounter::SIZE,
        seeds = [WITHDRAWAL_COUNTER_SEED, payer.key().as_ref()],
        bump,
    )]
    pub withdrawal_counter: Account<'info, WithdrawalCounter>,

    pub system_program: Program<'info, System>,
    // Residue Withdrawal vault PDAs (one per Delegation source that drains
    // to sub-min) live in `remaining_accounts` AFTER the per-source slots,
    // in Delegation-declaration order. Caller's `residue_vault_count: u8`
    // arg tells the handler how many trailing slots to consume. SDK
    // pre-derives them by reading `withdrawal_counter.next_id` and
    // incrementing.
}
