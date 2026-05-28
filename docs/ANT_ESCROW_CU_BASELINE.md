# Escrow Compute-Unit Baselines

CU usage measured per instruction via `solana-program-test` simulation
(`banks_client.simulate_transaction`). See
`programs/ario-ant-escrow/tests/integration.rs::measure_cu_*` tests.

## ANT Escrow Instructions

| Instruction                     | Measured (CU) | Budget | Notes |
|---------------------------------|---------------|--------|-------|
| `deposit_ant`                   |    54,477     | 200K   | Account init + mpl-core TransferV1 |
| `cancel_deposit`                |    38,755     | 200K   | TransferV1 + close |
| `update_recipient`              |    16,023     | 200K   | Field update + nonce hash |
| `claim_ant_arweave_attested`    |    77,011     | 200K   | **Arweave production path.** Ed25519 sysvar introspection + canonical reconstruction + TransferV1 + close. The preceding Ed25519Program native sigverify ix costs ~720 CU and is invisible to this program. |
| `claim_ant_ethereum`            |    90,595     | 200K   | `secp256k1_recover` (~25K) + 2× keccak (~10K) + TransferV1 + close |

## Token Escrow Instructions

| Instruction                          | Measured (CU) | Budget | Notes |
|--------------------------------------|---------------|--------|-------|
| `deposit_tokens`                     |    33,032     | 200K   | Account init + SPL transfer to escrow token account |
| `cancel_token_deposit`               |    31,208     | 200K   | SPL transfer back + close |
| `claim_tokens_arweave_attested`      |    48,431     | 200K   | **Arweave production path.** Ed25519 sysvar introspection + canonical reconstruction + SPL transfer + close. |
| `claim_tokens_ethereum`              |    73,826     | 200K   | `secp256k1_recover` + 2× keccak + SPL transfer + close |

## Vault Escrow Instructions

| Instruction                                  | Measured (CU) | Budget | Notes |
|----------------------------------------------|---------------|--------|-------|
| `deposit_vault`                              |   ~35,000     | 200K   | Account init + vault metadata copy; similar to `deposit_tokens` |
| `claim_vault_arweave_attested` (expired)     |    50,204     | 200K   | **Arweave production path, expired vault.** Liquid SPL transfer. |
| `claim_vault_ethereum` (expired)             |    same as `claim_tokens_ethereum` | 200K | Expired vaults use liquid SPL transfer. |

> **ADR-022 (2026-05-28):** active (still-locked) vault claims are now rejected
> early with `VaultStillLocked` (cheap; no token movement). The former active
> re-lock path and its `sysvar::instructions` loop were removed, so the prior
> "(active)" rows (~80K Arweave / ~150K Ethereum) no longer exist.

All claim paths fit comfortably within the 200K default CU budget — no
`SetComputeUnitLimit(400_000)` needed. The earlier on-chain RSA-PSS
variants (`claim_*_arweave`) were removed in commit 4ce73e4 because they
referenced the feature-gated `sol_big_mod_exp` syscall which prevented
the BPF loader from accepting the .so on devnet/mainnet. See ADR-017
(`docs/DECISIONS.md`) for the architecture rationale and
[`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor) for the off-chain service.

## Estimate vs measured gap

The design doc estimates were bottom-up calculations of the core logic cost
(CPI, hash, field writes). The measured values include Anchor framework
overhead that wasn't in the estimates:

- Account discriminator validation (~500 CU per account)
- Borsh deserialization of `EscrowAnt` (661 bytes, ~2-3K CU)
- PDA seed re-derivation via `seeds` + `bump` constraints (~1-2K CU)
- `has_one` / `address` / `constraint` checks (~500 CU each)
- `close` constraint bookkeeping (~1K CU)

This ~10-15K CU overhead is consistent across all Anchor programs in this
workspace and is not specific to the escrow program.

## Methodology

```rust
// In tests/integration.rs
let result = ctx.banks_client
    .simulate_transaction(tx)
    .await
    .unwrap();
let consumed = result
    .simulation_details
    .expect("simulate must report CU")
    .units_consumed;
println!("[cu] instruction_name: {}", consumed);
```

Numbers are sensitive to:
- `solana-program-test` version (currently 2.1.0)
- Anchor version (0.31.1)
- mpl-core program version pinned via the `mpl_core.so` fixture

Re-run after upgrading any of those and update this table.

## Regression detection

A CI step should fail the build if any measured value exceeds the prior
baseline by more than 10%. Until that's wired up, run measurements
manually after any change to:

- `src/verify/attested.rs` (Ed25519 sysvar introspection)
- `src/verify/ethereum.rs` (precompile call shape)
- `src/canonical.rs` (message length affects keccak/sha input size)
- `src/mpl_core_cpi.rs` (TransferV1 CPI account list)
- Token/vault instruction handlers (SPL transfer + close)

## Tx size (informational)

The Arweave production path (`claim_ant_arweave_attested`) is two
instructions in one transaction:

| Component | Bytes |
|-----------|-------|
| Tx header + 1 sig | ~136 |
| Ed25519Program ix data (16 header + 32 pubkey + 64 sig + ~210 message) | ~322 |
| Ed25519Program account metas (program id only) | ~34 |
| Claim ix account metas (8 accounts × 32 + flags incl. `instructions_sysvar`) | ~290 |
| Claim ix data: 8 disc + 32 nonce | 40 |
| **Total** | **~822 / 1232** |

Comfortably under the 1232-byte limit. Address Lookup Tables not needed.

`claim_ant_ethereum` is ~485 bytes (sig is 65 not 512).
