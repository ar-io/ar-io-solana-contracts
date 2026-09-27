# Mainnet upgrade log

This file records each upgrade to the ar.io programs on Solana mainnet: what
changed, how to verify the deployed code, and who had to act. Add a row in the
same change that records a new upgrade, after the upgrade is verified on chain.

For program IDs, see [`program-ids/mainnet.json`](../program-ids/mainnet.json).
For the rollout plan behind the September 2026 upgrades, see
[`WAVE2_ROLLOUT.md`](WAVE2_ROLLOUT.md).

## Upgrades

The following table lists each mainnet program upgrade, oldest first:

| Date (UTC) | Program | Source | Slot | What changed |
|---|---|---|---|---|
| 2026-09-15 | `ario-gar` | `bbf6e6c` | 447280441 | ADR-0032 (an untallied gateway earns 0 instead of halting distribution) and ADR-0033 (a non-live epoch can't be tallied). #130, #132. |
| 2026-09-25 | `ario-gar` | `ce5831b` | 450460260 | ADR-0030 (gateway operations address, `update_gateway_metadata`), ADR-0031 (transferable epoch settings authority), gateway schema migration fixes. #127, #129, #137, #142. |
| 2026-09-25 | `ario-arns` | `6693717` | 450471477 | The gateway operator discount honors the operations address. Purchase price and undername cap fixes. #129, #142, #144, #146, #147. |
| 2026-09-27 | `ario-gar` | `e48b6b4` | 451023184 | ADR-0034 (a new epoch requires the previous one distributed), ADR-0036 (`finalize_gone` only between epochs, so observations stay valid through an epoch), ADR-0037 (delegated stake reconcile and supply counter resync instructions), 30-day prune lock for excess stake. #145, #149. |

On 2026-09-25, between the `ario-gar` and `ario-arns` upgrades, all 578
gateways were migrated to gateway schema 1.2.0 with `migrate_gateway`.

## Verification data

The following table gives the size and SHA-256 of the code each upgrade
deployed, and the upgrade transaction:

| Date (UTC) | Program | Code bytes | Code SHA-256 | Upgrade transaction |
|---|---|---|---|---|
| 2026-09-15 | `ario-gar` | 1,371,240 | `ccbd3fe395dee4af8c98aebf59a01feff5312b635d774fffb251314c8a965b9c` | `54hG5tWA1hq67EEJrJEEsXzcm3GwrZMfjU3atREuogDTTpT3GqGsEj13TW1iYVWnQiPjQZVHpxgD16P3WSiQ26ZU` |
| 2026-09-25 | `ario-gar` | 1,397,008 | `47a7ad067df2172603711333c777499dde1b708c16e0449800a87b271139b81c` | `3xJiPnUwU5DtKb5T8puhwC5rpg1sMtCGZ2sUTCdoceh1gGH7kiQzKNwBmfksiovkLGyjrcfSFtjBvDrGXa24jg56` |
| 2026-09-25 | `ario-arns` | 1,197,512 | `1c7a3cc0762c45ebab570a5ed732997ac633ef0d372d9d23be126e15982cf254` | `5RrG4SgNaTo8mewo5j2m8LbjU5VLrV3k7roqWewnuqDu7gUeQRLv6knVhM2iEZ4eA8KRgawhfGXoDHEtKQurJBjd` |
| 2026-09-27 | `ario-gar` | 1,425,728 | `006f4341344f59ad265ae1a7f2f332e638fa5b5f64c5ea3fa3f2946d50432bcb` | `R4Uzq6mj8jJNRHLfTY8UDYou7w6YF2xv5monn74Bav55SDzDBFLGhGJL2kjnizoxvmgMdacbd9x4QQTWyeS2mVc` |

Each artifact is the staging-validated build of the same commit with mainnet
program IDs. The two builds differ only in program-ID bytes.

## Verify the deployed code

The program account holds the code followed by zero padding, so hash only the
code bytes:

```bash
solana program dump PROGRAM_ID program.so --url https://api.mainnet-beta.solana.com
head -c CODE_BYTES program.so | sha256sum
```

Replace the following:

- `PROGRAM_ID`: the program's mainnet ID from `program-ids/mainnet.json`.
- `CODE_BYTES`: the code size from the verification table, without commas.

The hash matches the table's SHA-256 until a later upgrade replaces that
program's code.

## Who had to act

For the 2026-09-25 upgrades, no client had to change. Gateways migrated to
schema 1.2.0 decode with `@ar.io/solana-contracts` 1.3.0 and later.

For the 2026-09-27 `ario-gar` upgrade, three instructions gained a required
account. Clients must run `@ar.io/sdk` 4.4.0 or later (`@ar.io/solana-contracts`
1.4.0 or later) to call them:

| Instruction | Added account | Error from an older client |
|---|---|---|
| `create_epoch` | The previous epoch | `MissingLatestEpochAccount` (6103) |
| `finalize_gone` | The latest epoch | `MissingLatestEpochAccount` (6103) |
| `compound_delegation_rewards` | `GatewaySettings` | `AccountNotEnoughKeys` |

`save_observations` didn't change, so observers needed no update. During an
epoch, `finalize_gone` returns `LatestEpochUnfinished` (6102): retry it after
the epoch is distributed.
