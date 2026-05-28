# Restoring the escrow active-vault re-lock path

> **Status:** Playbook only. The active re-lock path is currently **disabled**
> (ADR-022). This document captures the design + concrete steps needed if the
> "claim a still-locked vault and keep it locked for the remaining duration on
> Solana" capability is ever revived.

Pairs with:
- [`adrs/0022-escrow-disable-active-vault-relock.md`](adrs/0022-escrow-disable-active-vault-relock.md) — why the path was disabled, alternatives considered.
- [`adrs/0021-escrow-vault-relocks-non-revocable.md`](adrs/0021-escrow-vault-relocks-non-revocable.md) — the non-revocable invariant; restoration must preserve it.
- [`ACTIVE_VAULT_DISABLE_ROLLOUT.md`](ACTIVE_VAULT_DISABLE_ROLLOUT.md) — the rollout this would reverse.

## Step 0 — decide if you actually need it

The active re-lock **does not grant the claimant any liquidity** — it moves
still-locked tokens from escrow custody into an `ario_core` vault locked until
the same end time. Confirm there's a real product need (e.g. "claimant needs to
own a native Solana vault during the remaining lock for a downstream
integration") before reviving. If users just need their tokens, the unchanged
expired-liquid path already serves them — they wait.

## Recommended design — direct CPI (no introspection, 1:1 by construction)

Each active claim atomically locks **its own** escrowed amount via a direct CPI
into a new dedicated ario-core instruction. No payer wallet pass-through, no
sibling `vaulted_transfer`, no introspection surface, no reuse possibility.

### Why not just put back the old introspection?

The old design had a fundamental hole: nothing tied a `vaulted_transfer` to a
specific escrow/claim, so one sibling satisfied N claims (reuse / skim). Any
introspection-only fix would have to add such a binding (e.g. embedding an
escrow identifier into `vaulted_transfer`'s data and checking it) — that's an
ario-core change anyway, and more fragile than just doing the CPI. See ADR-022
"Rejected alternatives" for the full menu (sender binding, cardinality count,
one-claim-per-tx). Pick a different one only if direct CPI is infeasible.

## Step 1 — ario-core: add `vaulted_transfer_for_escrow`

`vaulted_transfer` currently hardcodes `payer = sender` on its `init`s. The
escrow PDA can't fund the vault/counter rent (it's rent-exempt only for itself,
and is closing in the same tx), so direct CPI needs a variant that **splits the
rent-payer from the token sender.** Don't modify `vaulted_transfer` in place —
that's an ABI break for direct (non-escrow) users.

In `programs/ario-core/src/instructions/vault.rs`:

```rust
#[derive(Accounts)]
pub struct VaultedTransferForEscrow<'info> {
    #[account(mut, seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArioConfig>>,

    #[account(
        init_if_needed,
        payer = rent_payer,                              // ← split: rent from a wallet
        space = VaultCounter::SIZE,
        seeds = [VAULT_COUNTER_SEED, recipient.key().as_ref()],
        bump,
    )]
    pub recipient_vault_counter: Box<Account<'info, VaultCounter>>,

    #[account(
        init,
        payer = rent_payer,                              // ← split: rent from a wallet
        space = Vault::SIZE,
        seeds = [VAULT_SEED, recipient.key().as_ref(), &recipient_vault_counter.next_id.to_le_bytes()],
        bump,
    )]
    pub vault: Box<Account<'info, Vault>>,

    #[account(
        mut,
        constraint = sender_token_account.owner == sender.key(),
        constraint = sender_token_account.mint == config.mint,
    )]
    pub sender_token_account: Box<Account<'info, TokenAccount>>,

    #[account(
        mut,
        constraint = vault_token_account.owner == vault.key() @ ArioError::InvalidAccountState,
        constraint = vault_token_account.mint == config.mint,
    )]
    pub vault_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: doesn't sign; vault owner = recipient.
    pub recipient: UncheckedAccount<'info>,

    /// Token source; PDA-signed by the caller (the escrow program).
    /// Must satisfy `sender != recipient` (enforced in handler).
    pub sender: Signer<'info>,

    /// Funds the vault + counter inits. Distinct from `sender` — that's the
    /// whole point of this variant.
    #[account(mut)]
    pub rent_payer: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}
```

Handler:
- **Hardcode `revocable = false`** (no `revocable` param). The escrow only ever
  produces non-revocable re-locks; this is now structurally enforced (ADR-021).
- Reuse the `vaulted_transfer` body via a shared private helper
  (`fn do_vaulted_transfer(...)`) so the two instructions share logic by
  construction. Same `MIN_VAULT_SIZE`, `min_vault_duration`/`max_vault_duration`
  bounds, `sender != recipient` check, token transfer, vault init.
- Same `controller = None` (since revocable is hardcoded false).
- Idempotent error variants (`InvalidAmount`, `LockDurationTooShort`,
  `LockDurationTooLong`, `VaultBelowMinimum`, `SelfTransfer`,
  `InvalidAccountState`, `ArithmeticOverflow`) — all already exist.

> **Trap (`min_vault_duration`):** ario-core requires
> `lock_duration_seconds >= config.min_vault_duration` (currently 14 days). A
> claim made when `remaining` is below that floor will fail the CPI. Decide:
> (a) fall through to the **liquid** path when `remaining < min_vault_duration`
> (recommended — preserves "always claimable when near expiry"), or (b) reject
> with a precise error. Document the choice in the new ADR.

## Step 2 — ario-ant-escrow: restore the active branch via CPI

In `programs/ario-ant-escrow/src/instructions/claim_vault_arweave_attested.rs`
and `claim_vault_ethereum.rs`:

1. Replace the `require!(clock >= vault_end_timestamp, VaultStillLocked)` with:
   ```rust
   if clock.unix_timestamp < vault_end_timestamp {
       // Active path — direct CPI into the new ario-core instruction.
       let remaining = vault_end_timestamp
           .checked_sub(clock.unix_timestamp)
           .ok_or(EscrowError::ArithmeticOverflow)?;

       // (If you chose option (a) for the min-duration trap, fall through to
       // the liquid path here when `remaining < config.min_vault_duration`.)

       let cpi_accounts = ario_core::cpi::accounts::VaultedTransferForEscrow {
           config: ctx.accounts.ario_core_config.to_account_info(),
           recipient_vault_counter: ctx.accounts.recipient_vault_counter.to_account_info(),
           vault: ctx.accounts.vault.to_account_info(),
           sender_token_account: ctx.accounts.escrow_token_account.to_account_info(),
           vault_token_account: ctx.accounts.vault_token_account.to_account_info(),
           recipient: ctx.accounts.claimant.to_account_info(),
           sender: ctx.accounts.escrow.to_account_info(),         // PDA-signed
           rent_payer: ctx.accounts.payer.to_account_info(),
           token_program: ctx.accounts.token_program.to_account_info(),
           system_program: ctx.accounts.system_program.to_account_info(),
       };
       let cpi_ctx = CpiContext::new_with_signer(
           ctx.accounts.ario_core_program.to_account_info(),
           cpi_accounts,
           signer_seeds, // ESCROW_VAULT_SEED, depositor, asset_id, bump
       );
       ario_core::cpi::vaulted_transfer_for_escrow(cpi_ctx, amount, remaining)?;
   } else {
       // Expired path — unchanged (liquid transfer to claimant_token_account).
   }
   ```
2. **Don't** restore `payer_token_account`. Direct CPI sources tokens from the
   escrow PDA itself, so the wallet pass-through is permanently gone.
3. **Don't** restore `instructions_sysvar` on `ClaimVaultEthereum` (still not
   needed — secp256k1 verifies via syscall).
4. **Add** to *both* `Accounts` structs: `ario_core_config`,
   `recipient_vault_counter`, `vault`, `vault_token_account`, `ario_core_program`
   (pinned `address = ario_core::ID`).

`vault` / `recipient_vault_counter` are derived from
`(VAULT_COUNTER_SEED, recipient)` and `(VAULT_SEED, recipient, next_id)`. The
caller (SDK) must read `recipient_vault_counter.next_id` ahead of time and pass
the matching derived `vault` PDA — same pattern the SDK already used in the
introspection-era `maybeBundleVaultedTransfer` (see deleted code at
`ar-io-sdk` `src/solana/escrow.ts` history pre-rollout commit).

CPI depth: claim (top) → `vaulted_transfer_for_escrow` (1) → SPL transfer +
system init (2). Under the 4 limit.

## Step 3 — tests

Restore + expand from the deleted `tests/cross_program_vault_claim.rs` (in git
history of the disable PR). Add specifically:

- **1:1 lock guarantee:** build one tx with N active claims for N escrows
  sharing `amount` + `claimant`; assert **N distinct vaults** are created, each
  with the full `amount`. The old reuse construction is now structurally
  impossible (no sibling to share), but this is the explicit regression for it.
- **PDA-signed sender:** assert the new vault's `owner == claimant` and the
  funded amount matches the escrow's. Implicitly verifies the escrow PDA
  successfully signed the CPI.
- **Near-expiry fallback:** if you chose option (a), set up a vault with
  `remaining < min_vault_duration` and assert it claims liquid (no vault
  created). If (b), assert the precise error.
- Expired-vault path remains unchanged.
- `deposit_vault` revocable rejection remains unchanged.

## Step 4 — SDK (`ar-io-sdk`)

In `src/solana/escrow.ts`, restore `maybeBundleVaultedTransfer`-equivalent
logic — but **inside the single claim ix** (no sibling, no
`vaulted_transfer` import):

- `claimVaultArweave` / `claimVaultEthereum`: when
  `escrow.vaultEndTimestamp > now`, read the claimant's `VaultCounter.next_id`
  (use the existing `getNextVaultId` helper), derive the new `vault` PDA + its
  ATA, add them to the claim ix's accounts (plus `arioCoreConfig`,
  `recipientVaultCounter`, `arioCoreProgram`).
- Remove the post-disable "locked until T" guard.
- The vault ATA must exist before the CPI runs; bundle a
  `CreateATAIdempotent` for it as an earlier ix.

## Step 5 — Frontend (`ar-io-solana-escrow-app`)

Restore the "claim early, stay locked" option in the claim UI for vaults where
`vaultEndTimestamp > now`. Display the projected new vault's unlock date
(unchanged from the escrow's `vault_end_timestamp`). Continue to support the
"wait until expiry, claim liquid" path.

## Step 6 — docs

- New ADR superseding ADR-022 for the active path (ADR-022 stays in place as
  the historical decision and rationale).
- New BD entry (next free `BD-NNN`).
- Reverse the doc edits made by the disable PR (#74) in `ANT_ESCROW_DESIGN`,
  `ANT_ESCROW_PROTOCOL_SPEC`, `ANT_ESCROW_CU_BASELINE`,
  `ESCROW_SECURITY_AUDIT_BRIEF` — replace the "active path removed (ADR-022)"
  callouts with the new direct-CPI description and new ADR pointer.
- Move this file to `docs/archive/` (per the archive policy in
  [`docs/archive/README.md`](archive/README.md)) once restoration ships.

## Cross-repo lockstep (mirror of the disable rollout)

Same sequence applies in reverse:
1. ario-core + ario-ant-escrow changes → contract release → publishes new
   `@ar.io/solana-contracts` with the restored claim ABI (+ new ario-core ix).
2. `ar-io-sdk` PR: bumps the dep, restores active-vault claim construction.
3. `ar-io-solana-escrow-app` PR: bumps SDK; restores "claim early, stay
   locked" UX.
4. `solana-ar-io` runner: extend `escrow-claim-runner.ts`'s Phase A to *also*
   exercise the new active happy path (a sibling vaulted_transfer is no longer
   needed — the claim ix does the CPI itself).

## Why this is comparatively cheap to revive

- Disabling was deletion-shaped (~1k LOC removed); restoration is a confined
  add (one ario-core ix + a CPI block in two escrow handlers + the SDK helper).
- All trust-relevant invariants are already documented (ADR-021 non-revocable,
  ADR-022 reuse/redirection root cause). The restoration must not regress them
  — the design above is structured so it doesn't (escrow PDA is the sender,
  1:1 atomic, `revocable = false` hardcoded).
- No state migration is needed — disabling didn't change any account layout;
  restoration just re-enables a code path against the same on-chain shape.
