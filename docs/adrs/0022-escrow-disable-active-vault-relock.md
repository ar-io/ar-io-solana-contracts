# ADR-022: Disable the Escrow Active-Vault Re-lock Path

- Status: accepted
- Date: 2026-05-28
- Deciders: protocol engineering
- Related: ADR-014 (trustless escrow), ADR-017 (off-chain attestor), ADR-021
  (escrow vault re-locks are non-revocable — this ADR removes the active
  re-lock path that ADR-021 hardened), BD-105

## Context

`ario-ant-escrow` custodies time-locked ARIO vault positions for the AO →
Solana migration. A vault claim had two outcomes:

- **Expired** (`clock >= vault_end_timestamp`): liquid SPL transfer straight to
  the claimant. (Unchanged by this ADR.)
- **Active** (still locked): the handler released the escrowed tokens to a
  wallet (`payer_token_account`) and then merely *introspected* the transaction
  for a matching sibling `ario_core::vaulted_transfer` (via
  `vault_introspect::verify_vaulted_transfer_in_tx`) to re-lock them for the
  claimant, preserving the remaining duration.

A security review (Codex) found the introspection has **no 1:1 binding between
a claim and the re-lock it credits**:

- The verifier matched only `amount`, `lock_duration`, `recipient`, and
  (post-ADR-021) non-revocable. It did **not** bind the `vaulted_transfer`'s
  `sender_token_account` to the `payer_token_account` that just received the
  release, nor `sender` to the payer, nor enforce ordering or **uniqueness**.
- It scans the instructions sysvar read-only and never marks a sibling
  "consumed", so **one `vaulted_transfer` can satisfy multiple claim
  instructions** in the same transaction.

⇒ A hand-crafted transaction batching N still-locked claims for the **same
claimant and identical `amount`**, plus a single `vaulted_transfer`, passes all
N claims while locking only `1×amount` — leaving `(N-1)×amount` liquid in the
payer's account (lock bypass; relayer skim from the claimant's locked position).
The revocable-controller-drain variant was already closed by ADR-021; this is
the residual, distinct, non-revocable reuse/redirection vector.

Key facts that shaped the decision:

1. **The active re-lock grants the claimant no liquidity** — it only moves
   *still-locked* tokens from escrow custody into an `ario_core` vault locked
   until the same end time. Disabling it changes nothing about *when* an honest
   user can spend.
2. **Nothing depends on the active path.** The migration tooling
   (`solana-ar-io`) claims vaults **liquid after expiry** and treats active
   claims as "should reject"; no shipped tool composes the
   `[claim, vaulted_transfer]` transaction.
3. **No purge for token/vault escrows** (purge is ANT-only). A still-locked
   vault is never stranded by waiting — it sits in escrow until it unlocks.
4. **Pre-mainnet.** Escrow is not yet deployed to mainnet, so removing the path
   carries no live-state migration.

## Decision

**Remove the active-vault re-lock path entirely. An active (still-locked) vault
claim is rejected with `VaultStillLocked`. Vaults are claimable only after
`vault_end_timestamp`, at which point they deliver liquid tokens to the
claimant (the unchanged expired path).**

- `claim_vault_arweave_attested` and `claim_vault_ethereum`: replace the
  active branch with `require!(clock >= vault_end_timestamp, VaultStillLocked)`,
  evaluated *before* any token movement or account close. The expired/liquid
  path, attestation verification (Ed25519 introspection for Arweave; secp256k1
  for Ethereum), asset-type/protocol/nonce guards, and escrow close are
  byte-for-byte unchanged.
- Drop `payer_token_account` from both claim Accounts structs (only the active
  path used it). Drop the now-dead `instructions_sysvar` from
  `ClaimVaultEthereum` (secp256k1 needs no sysvar); **keep** it on
  `ClaimVaultArweaveAttested` (the Ed25519 attestation introspection needs it).
- Remove the `vault_introspect` module. The `MissingVaultedTransferInstruction`
  and `RevocableVaultUnsupported` error variants are **retained** (append-only
  error ABI) though now unused by the claim path; `RevocableVaultUnsupported` is
  still emitted by `deposit_vault` (ADR-021).
- New error variant `VaultStillLocked` (appended).

This eliminates the reuse/redirection vector **by construction** — there is no
introspected sibling to reuse and no wallet redirection.

## Rejected alternatives

- **Direct CPI re-lock** — have the escrow CPI `ario_core::vaulted_transfer`
  itself with the escrow PDA as `sender` and `escrow_token_account` as the
  source, making each claim atomically lock its own amount (1:1 by
  construction). This *keeps* the early-claim-with-preserved-lock feature but is
  far heavier: `vaulted_transfer` inits the vault/counter with `payer = sender`,
  and the escrow PDA can't fund that rent, so it needs a new ario-core
  instruction splitting the rent-payer from the sender, plus an escrow rework
  and a larger SDK change. Disproportionate for a feature nothing uses and that
  grants no liquidity. **Recorded as the design starting point if "claim early,
  stay locked" is ever a deliberate product requirement.**
- **Harden introspection (bind `sender_token_account`/`sender`)** — necessary
  but *insufficient*: it does not stop reuse (one sibling pulling from the
  payer's account still satisfies N claims).
- **One-active-claim-per-transaction** (count own claim ixs in the sysvar,
  require exactly one) — sound and keeps the feature without an ario-core
  change, but retains the introspection complexity (and a full-instruction scan
  requirement) for a feature nothing uses. Not worth it now; folded into the
  direct-CPI option if the feature is ever revived.

## Consequences

- **One intended behavior change:** active vault claims now fail
  (`VaultStillLocked`) instead of re-locking. All other behavior — expired
  liquid claims, attestation, deposit, cancel, recipient-update, ANT/token
  claims — is unchanged. Documented as BD-107.
- The reuse/redirection vector is gone by construction; no ario-core change.
- **Cross-repo (lockstep, ABI):** dropping `payer_token_account` (+ the Ethereum
  sysvar) changes the two claim-ix account lists. `@ar.io/solana-contracts`
  regenerates on release; `ar-io-sdk` drops the sibling-bundling +
  `payerTokenAccount` arg and adds a "locked until `vault_end`" guard;
  `ar-io-solana-escrow-app` surfaces "claimable after <unlock>" and claims
  liquid at/after expiry; `solana-ar-io`'s runner updates its expected-error
  string (`MissingVaultedTransferInstruction` → `VaultStillLocked`). Tracked in
  `docs/ACTIVE_VAULT_DISABLE_ROLLOUT.md`.
- Reversible pre-mainnet: the direct-CPI design above can reintroduce the
  feature later without reverting this ADR's security posture.
