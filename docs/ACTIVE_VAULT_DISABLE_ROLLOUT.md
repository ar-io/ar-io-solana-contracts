# Rollout: disable the escrow active-vault re-lock path

Working tracker for the multi-repo change that disables the active
(still-locked) vault-claim re-lock path in `ario-ant-escrow`, closing the
reused / unbound `vaulted_transfer` finding (Codex). Pairs with the ADR
(see `docs/adrs/`). Delete or archive once all repos have shipped.

## Why

`claim_vault_arweave_attested` / `claim_vault_ethereum`, for a *still-locked*
vault, released escrow tokens to a wallet (`payer_token_account`) and then only
*introspected* the transaction for a matching `ario_core::vaulted_transfer`.
That check has no 1:1 binding: one `vaulted_transfer` can satisfy multiple
claim instructions, and the released tokens are not bound to the vault's source.
Result: a hand-crafted tx batching N still-locked claims for the same claimant
+ identical amount can leave `(N-1)×amount` liquid (lock bypass / relayer skim).
The revocable-controller-drain variant is already closed (#72 / ADR-021).

Decision (Phil, 2026-05-27/28): **disable the active path** — the simplest,
lowest-risk fix. It's pre-mainnet, nothing depends on the active path (the
migration takes vaults liquid-after-expiry), and the active re-lock gave the
claimant *no liquidity benefit* anyway. Direct-CPI reintroduction is recorded as
a future option if "claim early, stay locked" is ever required.

## The single intended behavior change

- **Before:** claiming a still-locked vault re-locked it into a fresh ario-core
  vault for the claimant (preserving remaining duration) via a sibling
  `vaulted_transfer`.
- **After:** claiming a still-locked vault is **rejected** with
  `VaultStillLocked`. The claimant waits until `vault_end_timestamp` and claims
  **liquid** via the (unchanged) expired path.

## Invariants that MUST NOT change (no regressions)

1. **Expired/liquid vault claim** — escrow → `claimant_token_account`, then close
   escrow token account + escrow PDA (`close = depositor`). Byte-identical.
2. **Attestation verification** — Arweave Ed25519 (introspects the preceding
   sigverify ix via `instructions_sysvar`) and Ethereum secp256k1
   (`verify_personal_sign`) are untouched. Asset-type / protocol / nonce guards
   untouched.
3. **`cancel_vault_deposit`**, `update_vault_recipient`, `deposit_vault`,
   ANT/token claim+cancel paths — untouched.
4. **No purge for token/vault escrows** (purge is ANT-only) — so a still-locked
   vault is never stranded by waiting; it sits in escrow until it unlocks.
5. **Events** — `EscrowClaimedEvent` shape unchanged (ABI append-only policy).

## Recon notes (current code, origin/develop @ 23ec50d)

- Both vault-claim handlers: verify attestation → `if clock < vault_end` ACTIVE
  branch (introspect + transfer-to-payer) `else` EXPIRED branch (liquid to
  claimant) → **then unconditionally close** the escrow token account + PDA.
  ⇒ the disable must `return Err(VaultStillLocked)` BEFORE the transfer/close.
- `instructions_sysvar`: **Arweave** uses it for the Ed25519 attestation (KEEP);
  **Ethereum** used it only for the now-removed vault introspection (secp256k1
  needs no sysvar) → **dropped from Ethereum** (don't leave a required-but-dead
  account in a custody ix; the two claim ixs already differ legitimately).
- `payer_token_account`: only used by the active path → **dropped** from both
  Accounts structs. `payer` (Signer / fee payer) stays.
- `vault_introspect.rs` becomes unused → removed. Error variants
  `MissingVaultedTransferInstruction` / `RevocableVaultUnsupported` retained
  (append-only error ABI), now unused.
- Generated TS client is **not** committed in this repo (only
  `clients/ts/src/canonical/ant-escrow.ts`, a hand-written message builder) — the
  `@ar.io/solana-contracts` codegen is produced at release. ⇒ SDK + frontend are
  downstream of a contract release.

## Cross-repo sequencing (lockstep)

Dropping `payer_token_account` is an ABI change to the two claim ixs, so order:

1. **`ar-io-solana-contracts`** — code + tests + docs/ADR. Merge + release →
   publishes `@ar.io/solana-contracts@<new>` with the updated claim builders.
2. **`ar-io-sdk`** (downstream) — bump `@ar.io/solana-contracts`, drop
   `payerTokenAccount` + `maybeBundleVaultedTransfer`, add a "locked until T"
   guard. CI can only go green after step 1 publishes.
3. **`ar-io-solana-escrow-app`** (frontend, downstream) — bump SDK; show
   "claimable after <unlock>" for locked vaults; claim liquid at/after expiry.
4. **`solana-ar-io`** — update the runner's expected-error string
   (`MissingVaultedTransferInstruction` → `VaultStillLocked`).

## Status

- [x] 1. Contracts: code
- [x] 2. Contracts: tests (69/69 escrow BPF suite green; clippy/fmt clean)
- [x] 3. Contracts: docs/ADR (ADR-022, BD-107, design/spec/CU/audit-brief)
- [x] 4. Contracts: verify + PR — **PR #74, CI green, awaiting review/merge**
- [ ] 5. SDK: changes + PR — *blocked on #74 merge + `@ar.io/solana-contracts` publish*
- [ ] 6. Frontend: UX + PR — *blocked on (5)*
- [ ] 7. Migration tooling: error string — *needs new escrow deployed to staging/devnet to validate*
