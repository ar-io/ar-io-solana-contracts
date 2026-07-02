# Escrow Program Security Audit Brief

**Program:** `ario-ant-escrow`
**Version:** v3 (generalized — ANTs + tokens + vaults + off-chain attestor for Arweave path)
**Branch:** `fix/rsa-software-modexp`
**Date:** 2026-05-06

---

## Scope

18 on-chain instructions in a single Anchor program that provides
trustless custody for three asset types, plus a single-purpose
off-chain Node.js service ([`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor),
extracted 2026-05-16 from `solana-ar-io/ar-io-solana-attestor/`) that
re-signs Arweave RSA-PSS signatures with Ed25519 for cheap on-chain
verification.

| Asset | Deposit | Claim | Cancel | Update |
|-------|---------|-------|--------|--------|
| ANT (Metaplex Core NFT) | `deposit_ant` | `claim_ant_arweave_attested`, `claim_ant_ethereum` | `cancel_deposit` | `update_recipient` |
| ARIO tokens (SPL) | `deposit_tokens` | `claim_tokens_arweave_attested`, `claim_tokens_ethereum` | `cancel_token_deposit` | `update_token_recipient` |
| Vaulted ARIO (time-locked) | `deposit_vault` | `claim_vault_arweave_attested`, `claim_vault_ethereum` | `cancel_vault_deposit` | `update_vault_recipient` |

The earlier on-chain RSA-PSS `claim_*_arweave` instructions were
removed in commit 4ce73e4 — they referenced the feature-gated
`sol_big_mod_exp` syscall which prevented the BPF loader from accepting
the .so on devnet/mainnet. Production Arweave claims now go through
the off-chain attestor service.

## Architecture

### Signature verification
- **Arweave (production path):** Off-chain attestor verifies RSA-PSS-4096
  via `node:crypto` and re-signs the canonical claim message with
  Ed25519. On-chain, the program reads `sysvar::instructions` to confirm
  that the instruction immediately preceding it is a Solana
  `Ed25519Program` native sigverify ix with `pubkey == ATTESTOR_PUBKEY`
  and `message == reconstructed canonical from escrow state`. Any
  mismatch reverts. The Ed25519 verification itself runs in the native
  Ed25519Program (~720 CU); our program does the introspection only.
- **Ethereum:** ECDSA secp256k1 with EIP-191 `personal_sign` via
  `secp256k1_recover` precompile.
- Both on-chain verifiers use XOR-accumulator constant-time comparison.

### Canonical message
- **ANT escrow** (`ANT_ESCROW_CLAIM_HEADER`): `ar.io ant-escrow claim v1\nnetwork:...\nant:...\nclaimant:...\nnonce:...`
- **Token + vault escrow** (`ESCROW_CLAIM_HEADER`): `ar.io escrow claim v2\nnetwork:...\ntype:...\nasset:...\namount:...\nclaimant:...\nnonce:...`
- Reconstructed entirely on-chain from account state — never client-supplied
- The off-chain attestor service uses byte-identical canonical builders
  (`ar-io-solana-attestor/src/canonical.ts` ↔ `contracts/programs/ario-ant-escrow/src/canonical.rs`)
  pinned by a cross-toolchain test that re-runs every CI build.

### Vault claim pattern
Vaults are claimable **at any time** (ADR-027). Settlement branches on the
remaining lock, read against ario-core's **live** `min_vault_duration`:

- `remaining >= min_vault_duration`: **re-lock** — inside one instruction the
  handler transfers exactly `amount` from escrow custody to
  `payer_token_account` (escrow-PDA-signed) and CPIs
  `ario_core::vaulted_transfer(amount, remaining, revocable=false)` with
  `sender = payer`, `recipient = claimant` (or `create_vault` when
  `payer == claimant`, since `vaulted_transfer` rejects sender == recipient).
  The claimant gets a native, non-revocable ario-core vault unlocking at the
  escrow's exact original `vault_end_timestamp`.
- `remaining < min_vault_duration` (including expired): **liquid** transfer
  to the claimant — i.e. a deliberate, bounded early-liquidity window of up
  to `min_vault_duration` (ADR-027).

**Trust argument for the pass-through** (vs. the removed introspection): each
claim performs its *own* CPI, so the credit and the equal-amount debit are
1:1 by construction and atomic — a CPI failure reverts the release, the payer
nets zero, and there is no consumable sibling to reuse. The claimant is bound
into the vault owner/recipient by the attested canonical message;
`revocable = false` is hardwired at the call site (ADR-021). Pinned by
`tests/cross_program_vault_claim.rs` (N-claims→N-vaults, payer-net-zero,
wrong-vault-PDA revert).

> **History — ADR-022 (2026-05-28):** the original active path used
> `sysvar::instructions` introspection of a sibling `vaulted_transfer`, which
> had no 1:1 claim↔re-lock binding — one sibling could satisfy multiple
> batched claims (lock bypass / relayer skim; Codex finding) — so it was
> removed (`VaultStillLocked`). **ADR-027 (2026-07-01)** restored the
> capability as the direct CPI above, against ario-core's unmodified ABI.
> `vault_introspect.rs` stays gone; `VaultStillLocked` is retained but
> unreachable (append-only error ABI); a still-locked claim submitted without
> the six trailing optional re-lock accounts fails `RelockAccountsMissing`
> before any token movement. The revocable-controller variant was
> independently closed by ADR-021 / BD-105.

### Account types
- `EscrowAnt`: 661 bytes — holds ANT custody metadata
- `EscrowToken`: 711 bytes — holds token/vault custody metadata + lock params

### PDA derivation
- ANT: `["escrow_ant", ant_mint]`
- Token: `["escrow_token", depositor, asset_id]`
- Vault: `["escrow_vault", depositor, asset_id]`

## Files in scope

### Core program
```
contracts/programs/ario-ant-escrow/src/
├── lib.rs                            — instruction dispatch, feature flags, network feature
├── state.rs                          — EscrowAnt + EscrowToken structs, nonce derivation, ATTESTOR_PUBKEY constant, ED25519_PROGRAM_ID
├── error.rs                          — 25 error codes (4 added for attestation: MissingAttestationInstruction, MalformedAttestationInstruction, AttestationSignerMismatch, AttestationMessageMismatch)
├── canonical.rs                      — ANT + escrow canonical message builders
├── mpl_core_cpi.rs                   — hand-rolled TransferV1 CPI
├── verify/
│   ├── attested.rs                   — Ed25519 sysvar introspection (production Arweave path)
│   └── ethereum.rs                   — ECDSA + EIP-191 implementation
└── instructions/
    ├── deposit.rs                    — deposit_ant
    ├── deposit_tokens.rs             — deposit_tokens
    ├── deposit_vault.rs              — deposit_vault
    ├── claim_arweave_attested.rs     — claim_ant_arweave_attested
    ├── claim_ethereum.rs             — claim_ant_ethereum
    ├── claim_tokens_arweave_attested.rs — claim_tokens_arweave_attested
    ├── claim_tokens_ethereum.rs      — claim_tokens_ethereum
    ├── claim_vault_arweave_attested.rs — claim_vault_arweave_attested (re-lock via CPI / liquid, ADR-027)
    ├── claim_vault_common.rs         — shared settle logic (re-lock CPI + liquid branches) for both vault claims
    ├── claim_vault_ethereum.rs       — claim_vault_ethereum
    ├── cancel.rs                     — cancel_deposit (ANT)
    ├── cancel_token_deposit.rs       — cancel_token_deposit
    ├── cancel_vault_deposit.rs       — cancel_vault_deposit
    ├── update_recipient.rs           — update_recipient (ANT)
    ├── update_token_recipient.rs     — update_token_recipient
    └── update_vault_recipient.rs     — update_vault_recipient
```

> The earlier `verify/arweave.rs` + `instructions/claim_*_arweave.rs`
> on-chain RSA-PSS verifier was removed in commit 4ce73e4. Production
> Arweave claims use the off-chain attestor (`ar-io-solana-attestor/`).

### Off-chain attestor service
```
ar-io-solana-attestor/
├── src/
│   ├── app.ts                  — Express handlers; /health and /attest
│   ├── index.ts                — entry point + listen
│   ├── canonical.ts            — TS mirror of contracts/.../canonical.rs
│   ├── canonical.cross.test.ts — Cross-test against Rust binary
│   ├── verify-rsa-pss.ts       — node:crypto RSA-PSS verifier (PSS padding, SHA-256, MGF1-SHA-256)
│   ├── attest.ts               — Ed25519 signer via @noble/ed25519
│   ├── keygen.ts               — Generates 32-byte seed, prints pubkey
│   └── config.ts               — Env loading + validation
├── Dockerfile                  — Multi-stage Alpine + Node 20, non-root user (uid 100)
├── docker-compose.yml          — Reference compose for local + staging
└── README.md                   — Deploy / ops / key rotation runbook
```

### Integration tests (2,691 lines)
```
contracts/programs/ario-ant-escrow/tests/integration.rs
```

### Fuzz targets
```
contracts/programs/ario-ant-escrow/fuzz/fuzz_targets/
├── verify_rsa_pss.rs          — RSA-PSS false positive detection
└── verify_personal_sign.rs    — ECDSA false positive detection
```

## Known security properties

1. **Replay protection (3 layers):** claimant pubkey in message, nonce rotated on update, PDA closed on claim
2. **Cross-network replay prevention:** network string baked at compile time; attestor service refuses to start if its `NETWORK` env var doesn't match the SDK config (caught at first attest call)
3. **MEV resistance:** anyone can submit, only named claimant receives
4. **PKCS#1 v1.5 downgrade prevention:** attestor's `node:crypto` verifier uses `RSA_PKCS1_PSS_PADDING` exclusively (the only RSA verifier left after the on-chain path was removed)
5. **ECDSA malleability prevention:** EIP-2 low-S enforcement
6. **Vault lock enforcement:** still-locked claims re-lock 1:1 into a claimant-owned non-revocable ario-core vault preserving the original unlock time via direct CPI (ADR-027); sub-`min_vault_duration` remainders deliver liquid (deliberate bounded window); a still-locked claim without the re-lock accounts fails `RelockAccountsMissing` before any token movement
7. **Token theft prevention:** `claimant_token_account.owner == claimant.key()` constraint
8. **Ed25519 introspection bounds-checking:** all offsets via `checked_add`; `*_ix_index` must equal `0xFFFF` (DATA_IN_SAME_IX) so the program rejects sigs/pubkeys/messages stored in *other* instructions
9. **Attestor pubkey baked into program:** `ATTESTOR_PUBKEY` constant in `state.rs` is verified byte-for-byte in `verify_attested_signature`; rotation requires a `BPFLoaderUpgradeable` upgrade (no runtime authority)
10. **Deploy guardrail:** `contracts/scripts/check-attestor-pubkey.sh --strict` blocks any deploy that still has the test ATTESTOR_PUBKEY (`AKnL4NN...` derived from public seed `[1u8; 32]`); wired into `devnet-deploy.sh` Phase 0

## Previously identified and fixed vulnerabilities

| Severity | Description | Fix |
|----------|-------------|-----|
| CRITICAL | `claimant_token_account` owner not validated — payer could redirect tokens | Added `owner == claimant.key()` constraint |
| MEDIUM | `ario_core_program` not address-pinned in vault claims | Added `address = ario_core::ID` |
| MEDIUM | Vault escrows had no cancel/update instructions | Added 2 instructions |
| MEDIUM | Frontend assetType deserialization checked wrong byte value | Fixed |

## Areas of concern for auditors

1. **Off-chain attestor trust boundary** — The Arweave production claim path
   delegates RSA-PSS verification to an off-chain Node.js service that
   re-signs with Ed25519. The attestor's Ed25519 secret IS the trust root
   for all Arweave claims. Compromise enables minting valid attestations
   for arbitrary (escrow, claimant) pairs. The on-chain program enforces
   only that `pubkey == ATTESTOR_PUBKEY` and `message == reconstructed
   canonical from escrow state`. Auditors should evaluate:
   - Attestor verifier correctness (`ar-io-solana-attestor/src/verify-rsa-pss.ts`):
     does it reject all PKCS#1 v1.5 sigs, mismatched salt lengths, modulus
     length errors? It uses `RSA_PKCS1_PSS_PADDING` only.
   - Canonical builder byte-equivalence: any drift between TS and Rust
     enables signature confusion. Pinned by the cross-test in
     `ar-io-solana-attestor/src/canonical.cross.test.ts`.
   - Container hardening: non-root user, read-only fs, no inbound network
     except the configured PORT, secret never logged.
   - Key rotation runbook (`ar-io-solana-attestor/README.md` § Key rotation):
     swap requires program upgrade — verify the procedure is documented
     and rehearsable.

2. **Ed25519 sysvar introspection (`verify/attested.rs`)** — Novel pattern.
   The program reads `sysvar::instructions`, walks the on-chain layout of
   the preceding Ed25519Program ix, and compares offsets/contents to the
   reconstructed canonical message. Verify:
   - All offsets are bounds-checked via `checked_add`.
   - The `signature_instruction_index`, `public_key_instruction_index`,
     and `message_instruction_index` fields all equal `0xFFFF` — anything
     else means the data lives in another instruction, which the program
     does not validate.
   - The preceding ix program id equals `Ed25519SigVerify111...` (not just
     a similar-looking program).
   - The program tolerates only the documented Ed25519Program ix layout
     (see Solana docs / SDK constants).

3. **Vault re-lock pass-through (ADR-027)** — The active path returned as a
   direct CPI with an atomic payer pass-through (the ADR-022 introspection and
   its reuse/skim hole stay gone). Verify: (a) the pass-through credit and the
   CPI debit are the same `amount` inside one instruction, with no path that
   leaves funds in `payer_token_account` on success or failure; (b) the CPI
   targets are pinned (`ario_core_program` `address = ario_core::ID`) and
   `revocable = false` is hardwired (ADR-021); (c) `min_vault_duration` is
   read from the ario-core config PDA (`seeds::program`-validated), not
   hardcoded; (d) the liquid fallback below the minimum is the intended,
   documented early-liquidity window; (e) a still-locked claim without the
   optional account set fails `RelockAccountsMissing` before any token
   movement. Known/pre-existing (unchanged by ADR-027): dust injected into
   `escrow_token_account` can DoS the claim-path `close_account` (no C1
   live-balance sweep here, unlike `ario_core::release_vault`).

4. **Canonical message reconstruction** — Cross-language byte-equivalence
   between Rust and TypeScript. Two surfaces: SDK
   (`sdk/src/solana/canonical-message.ts`) AND attestor service
   (`ar-io-solana-attestor/src/canonical.ts`). Any drift enables signature
   confusion. Both pinned by cross-tests against the Rust binary. Note
   the `recipient: <43-char base64url(sha256(recipient_pubkey))>` line
   added by the F-1 fix — that's the binding that prevents the attestor
   from issuing attestations under attacker-controlled moduli.

5. **mpl-core CPI wire format** — Hand-rolled TransferV1 encoding. If
   Metaplex Core updates the wire format, this breaks silently.

6. **Token account close ordering** — `close_account` CPI for the escrow's
   ATA runs after the token transfer. Verify no double-spend window.

7. **Nonce derivation entropy** — Initial nonce = `sha256(slot || mint || depositor)`.
   Predictable but useless without the recipient's private key.

8. **Attestor pubkey rotation** — The on-chain `ATTESTOR_PUBKEY` is a
   compile-time constant. Rotation requires `BPFLoaderUpgradeable` upgrade.
   The deploy guardrail
   (`contracts/scripts/check-attestor-pubkey.sh --strict`) blocks shipping
   the test pubkey to any cluster. Verify the guardrail is wired into
   every deploy path (currently `devnet-deploy.sh` Phase 0).

## Test coverage

| Suite | Count | Coverage |
|-------|-------|---------|
| Program unit tests | 57+ | Verifiers, canonical messages, state layout, MGF1 |
| Program integration tests | 45+ | All 18 instructions via BPF, real RSA + ECDSA sigs, Ed25519 attested path with deterministic test seed `[1u8; 32]` |
| SDK unit tests | 47+ | Canonical (ANT + escrow), hex encoding, TokenEscrow layout, attestor client (22 tests using Node `http.createServer`) |
| SDK cross-language | 18 | Rust binary vs TS for ANT (10) + escrow (8) vectors |
| Attestor service tests | 35 | RSA-PSS sign-and-verify round-trip, tampering detection, Ed25519 round-trip vs `ed25519_dalek`, full HTTP integration, canonical cross-test against Rust binary |
| Surfpool E2E | 8+ | ANT + token + vault on live validator |
| Fuzz probes | 12.6M runs | Zero false positives |

## Dependencies

```toml
anchor-lang = "0.31.1"
anchor-spl = "0.31.1"
solana-program = "2.1.0"
ario-core = { path = "../ario-core", features = ["cpi"] }  # for ario_core::ID only
```

## Build & test

```bash
# Build
cd contracts && ./build-sbf.sh --skip-check

# All tests
cargo test -p ario-ant-escrow --lib                          # 57 unit tests
cp programs/ario-ant-escrow/tests/fixtures/mpl_core.so target/deploy/
BPF_OUT_DIR=$(pwd)/target/deploy cargo test -p ario-ant-escrow --test integration  # 35 tests

# Fuzz (nightly)
cd programs/ario-ant-escrow
cargo +nightly fuzz run verify_rsa_pss -- -max_total_time=86400
cargo +nightly fuzz run verify_personal_sign -- -max_total_time=86400
```

## Reference documents

- `docs/ANT_ESCROW_DESIGN.md` — Full technical design (Arweave production
  path documented under "Arweave — production path (off-chain attestor →
  Ed25519 introspection)")
- `docs/ANT_ESCROW_PROTOCOL_SPEC.md` — Public protocol specification
  (§ 5.2a for `claim_ant_arweave_attested`, § 12 for token + vault attested
  variants)
- `docs/ANT_ESCROW_CU_BASELINE.md` — Compute unit measurements (legacy +
  attested paths)
- `docs/DECISIONS.md` ADR-014 — Original escrow architecture decision
- `docs/DECISIONS.md` ADR-017 — Off-chain attestor decision (covers all
  alternatives investigated, CU comparison, why Ed25519 over privileged
  authority)
- `ar-io-solana-attestor/README.md` — Attestor service deploy + ops + key
  rotation runbook
