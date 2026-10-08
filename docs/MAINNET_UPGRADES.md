# Mainnet upgrade log

This file records every deploy and upgrade of the ar.io programs on Solana
mainnet since the first deploy: what changed, how to verify the deployed code,
and who had to act. Add a row in the same change that records a new upgrade,
after the upgrade is verified on chain.

The event list is complete. It was rebuilt from each program's on-chain history
(every `Deploy` and `Upgrade` instruction on its ProgramData account) on
2026-09-29. Later events are added from chain once each upgrade is verified.
Source commits and code hashes for the earlier events come from the
records listed in [Where the records come from](#where-the-records-come-from).
Where no record survives, the table says so.

For program IDs, see [`program-ids/mainnet.json`](../program-ids/mainnet.json).
For the rollout plan behind the September 2026 upgrades, see
[`WAVE2_ROLLOUT.md`](WAVE2_ROLLOUT.md).

## Deploys and upgrades

The following table lists each event, oldest first:

| Date (UTC) | Program | Event | Slot | Source | What changed |
|---|---|---|---|---|---|
| 2026-06-05 | `ario-core` | Deploy | 424495212 | `05948ba` | First deploy. |
| 2026-06-05 | `ario-gar` | Deploy | 424496528 | `05948ba` | First deploy. |
| 2026-06-05 | `ario-arns` | Deploy | 424496691 | `05948ba` | First deploy. |
| 2026-06-05 | `ario-ant` | Deploy | 424496877 | `05948ba` | First deploy. |
| 2026-06-05 | `ario-ant-escrow` | Deploy | 424496900 | `05948ba` | First deploy. The escrow is deployed but not in use. |
| 2026-07-09 | `ario-ant-escrow` | Upgrade | 431869418 | `develop` plus an uncommitted attestor-key edit | ADR-027: still-locked vault claims re-lock into `ario-core` vaults. |
| 2026-08-10 | `ario-gar` | Upgrade | 438444576 | `a8ef07e` | Governable reward split; observation rent refunds the observer. #116. |
| 2026-08-10 | `ario-ant` | Upgrade | 438460707 | `a8ef07e` | ADR-028: a program PDA holds each ANT's UpdateAuthority. #118. |
| 2026-08-30 | `ario-gar` | Upgrade | 442893933 | `9202f28` | ADR-0029: epoch rent refunds the epoch's creator; `admin_close_orphaned_epoch_rent_receipt`. #121. |
| 2026-09-15 | `ario-gar` | Upgrade | 447280441 | `bbf6e6c` | ADR-0032 (an untallied gateway earns 0 instead of halting distribution) and ADR-0033 (a non-live epoch can't be tallied). #130, #132. |
| 2026-09-25 | `ario-gar` | Upgrade | 450460260 | `ce5831b` | ADR-0030 (gateway operations address, `update_gateway_metadata`), ADR-0031 (transferable epoch settings authority), gateway schema migration fixes. #127, #129, #137, #142. |
| 2026-09-25 | `ario-arns` | Upgrade | 450471477 | `6693717` | The gateway operator discount honors the operations address. Purchase price and undername cap fixes. #129, #142, #144, #146, #147. |
| 2026-09-27 | `ario-gar` | Upgrade | 451023184 | `e48b6b4` | ADR-0034 (a new epoch requires the previous one distributed; an epoch with no observations pays no rewards), ADR-0036 (`finalize_gone` only between epochs, so observations stay valid through an epoch), ADR-0037 (delegated stake reconcile and supply counter resync instructions), 30-day prune lock for excess stake. #145, #149. |
| 2026-10-08 | `ario-gar` | Upgrade | 454578042 | `fd41e24` | `admin_set_tenure_weight`, to correct the tenure weight duration set at genesis; tenure measured at the epoch start (BD-122); existing delegators may add stake below the gateway minimum (BD-121). #158. |

Programs were also enlarged with `ExtendProgram` before some upgrades, which
changes size only: `ario-ant-escrow` on 2026-07-09, `ario-gar` on 2026-07-27,
2026-08-30, 2026-09-27 and 2026-10-08, and `ario-ant` on 2026-08-10.

## Verification data

The following table gives the size and SHA-256 of the code each event
deployed, and its transaction. "Not recorded" means the code was replaced
before anyone kept a copy, so its hash can no longer be computed.

| Date (UTC) | Program | Code bytes | Code SHA-256 | Transaction |
|---|---|---|---|---|
| 2026-06-05 | `ario-core` | 929,056 | `10b6a76a89ecf6ee39f99c7d094d0cf53b72f7261c2ff985b4a37dcbcdeda2ba` | `5c8fMVUuvRKXaud1xbSpJ13bonDStRajBze5oLn6xqi8bGRsj3Um7E1ZjqGmKwjyscdovbChHVJaqjkYf55o8aXg` |
| 2026-06-05 | `ario-gar` | Not recorded | Not recorded | `4LUKKh9YQQnb1GovEWPEQkwieCSxjor3dasumw1WueSrnY6jMDLNfL9YFE61T9p597UPNKP4DJrKiNgePS39p74d` |
| 2026-06-05 | `ario-arns` | 1,198,808 | `725959f7db2b937ae7331c7960dadf56d02872fd16033bc434e3e95ce461439b` | `5AXk7qC6m9FnYmRLtZzoNCK8MLRuj3uTdpAfTNHAqz2JANCXRcSKRxAwruZHJZ3JxK5bpECvKeYgcsuHUtpyPv5b` |
| 2026-06-05 | `ario-ant` | Not recorded | Not recorded | `33j3V3npFwki8ca2ybCNVHGvoChz8W9k4Ghi5UZwbyhPVujjdxv8UiCLzqxHkTGRWrGRiix9qY82wB1qzegY1XMz` |
| 2026-06-05 | `ario-ant-escrow` | Not recorded | Not recorded | `3EU5EkjWvhdvG7NmxrT8sYyFExWk1zMVBz5n3xfRUusdWuKiJtPFGmiWxFDZmftuRW31N8Wtff98FcGZfWNs6Dh3` |
| 2026-07-09 | `ario-ant-escrow` | 614,800 | `f9af4f3e1ae7ded64679c60aed1ec995e9f8063e2abf0e67bc5e7c037b520a88` | `3FfNmg6sLsYqHr5mk761xfrVhSYSBcz9o6wqSDGyzEZBCPrWUjsCxsvUSCcAXvHfQfnHZtK1VSMRNiLZx6NewPwR` |
| 2026-08-10 | `ario-gar` | Not recorded | Not recorded | `5punUQQUEDWEHCGZ7MyCaih3JevHhpDBE1ksPk5CUkxuL9Za5KnmjptNkUfNsFsToARkBBYSz3SrmBQfPNRSiuLy` |
| 2026-08-10 | `ario-ant` | 932,768 | `e4777a7239acc0ed46017e90f345b45072ea6a84d20293b94e7b2c642be04eaa` | `3AynRMK7KKbuTV7C76jgyhg92xJthVZxoBYZQNVvXiHsNNbiCAXqzNCHjUyDiknsT13eYx6PfeXS46dtt92guy3A` |
| 2026-08-30 | `ario-gar` | 1,367,960 | `7f09326ff0766c2b7eede2f7d48a91ed62bec79df8257f99a7bcae82138e1603` | `2sibCJhB6RHC5vSipbicuZWq2sh5eDPKtTtXdooPXKkQUJ2euDCxSXG33DfENuRzewLty73821jvThJxgsxW1bJy` |
| 2026-09-15 | `ario-gar` | 1,371,240 | `ccbd3fe395dee4af8c98aebf59a01feff5312b635d774fffb251314c8a965b9c` | `54hG5tWA1hq67EEJrJEEsXzcm3GwrZMfjU3atREuogDTTpT3GqGsEj13TW1iYVWnQiPjQZVHpxgD16P3WSiQ26ZU` |
| 2026-09-25 | `ario-gar` | 1,397,008 | `47a7ad067df2172603711333c777499dde1b708c16e0449800a87b271139b81c` | `3xJiPnUwU5DtKb5T8puhwC5rpg1sMtCGZ2sUTCdoceh1gGH7kiQzKNwBmfksiovkLGyjrcfSFtjBvDrGXa24jg56` |
| 2026-09-25 | `ario-arns` | 1,197,512 | `1c7a3cc0762c45ebab570a5ed732997ac633ef0d372d9d23be126e15982cf254` | `5RrG4SgNaTo8mewo5j2m8LbjU5VLrV3k7roqWewnuqDu7gUeQRLv6knVhM2iEZ4eA8KRgawhfGXoDHEtKQurJBjd` |
| 2026-09-27 | `ario-gar` | 1,425,728 | `006f4341344f59ad265ae1a7f2f332e638fa5b5f64c5ea3fa3f2946d50432bcb` | `R4Uzq6mj8jJNRHLfTY8UDYou7w6YF2xv5monn74Bav55SDzDBFLGhGJL2kjnizoxvmgMdacbd9x4QQTWyeS2mVc` |
| 2026-10-08 | `ario-gar` | 1,431,096 | `2fc8c38e5a8fa7c0812487043561fef2a4ec2b4d0b7d6575ac44656645e1c6e8` | `4rX8MmsrY5uAk1yJCYZroSTWHWCDPghFJ1yGNVZoevK2kiXbfckECNCbquTDbqYJbXfeJxrPLjGYP4EKm2En45NA` |

From 2026-09-15 on, each artifact is the staging-validated build of the same
commit with mainnet program IDs. The two builds differ only in program-ID bytes.

## Where the records come from

- **Events, slots and transactions:** each program's on-chain history, read on
  2026-09-29. The 2026-10-08 upgrade and both admin operations dated 2026-08-10
  and 2026-10-08 were read from chain on 2026-10-08.
- **Code still on chain:** `ario-core` (06-05), `ario-ant-escrow` (07-09) and
  `ario-ant` (08-10) have not been upgraded since, so their hashes were computed
  from the live program on 2026-09-29.
- **Code since replaced:** the `ario-arns` 06-05 and `ario-gar` 08-30 hashes
  come from dumps of the live program taken just before the upgrades that
  replaced them (2026-09-25 and 2026-09-15). The escrow's 07-09 hash matches the
  record made on the day of that upgrade.
- **2026-06-05 source:** the cutover handoff written that day instructs
  rebuilding all five programs from `develop` at `05948ba`. The built code was
  not recorded, except where it is still on chain or was dumped later.
- **2026-07-09 source:** the escrow build included an attestor-key edit that
  was never committed, so no commit reproduces it exactly.
- **2026-08-10 source:** the release was merged to `main` the same day as #120,
  from `develop` at `a8ef07e`. #116 and #118 are its program changes.
- **2026-08-30 source:** #121 (`9202f28`) is the only `ario-gar` change between
  the 08-10 and 08-30 upgrades. The `ario-gar` source didn't change between
  `9202f28` and the upgrade.
- **2026-10-08 source:** built from `develop` at the #158 merge commit
  `fd41e24` and reproduced byte for byte from it. Staging has run the same
  commit since 2026-10-07 (code SHA-256 `f032a8cf…`); the two builds differ only
  in program IDs.

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

## Admin operations

The following changes to program state were made with admin instructions, not
upgrades:

- **2026-08-10:** the epoch reward split changed from 90/10 to 80/20
  (gateways/observers) with `admin_set_reward_ratios`
  (`3gzZ3SbdVhyGtiAkL61tiq8i7uG2nA62Lf78Pj35NBwHhsfmCfNygBxQxBsjNB3776ZZ9Ff11qr4GECWGSh2B9Vs`,
  slot 438473095), after the `ario-gar` upgrade that added the instruction.
- **2026-09-25:** all 578 gateways were migrated to gateway schema 1.2.0 with
  `migrate_gateway`, between the `ario-gar` and `ario-arns` upgrades.
- **2026-09-28:** the delegated-stake correction from ADR-0037. 132
  `admin_reconcile_delegated_stake` transactions lowered each over-counted
  gateway's `total_delegated_stake` to the sum of its Delegation accounts,
  removing 2,226,210.675676 ARIO counted for AO delegators who had no Solana
  address at migration. One `admin_resync_supply_counters` transaction
  (`3hy3a9A5KP9ncrdjrerXEVec5Kyy3bF6vkBqi2LDKrpDQCtx2zpvKYWfA11DkPiMFKFAYmF8m2hiYQQpxrdqTsy3`,
  slot 451425301) then set `total_delegated` to 10,787,031.814673 ARIO. No
  tokens moved: the transactions touched no token account, and the stake pool
  balance was unchanged. Those delegators' ARIO is held for them through the
  claims service.
- **2026-10-08:** `EpochSettings.tenure_weight_duration` changed from 3,600
  seconds to 15,552,000 (180 days, the value Lua used) with
  `admin_set_tenure_weight`
  (`4BKKD53NG4V2NKZFVpME8v2Lxnbzgi1vdLQDm2KCvCfLSvauHLNtH6WirprZDMGPQTf9vaLKy8FE24Gw2RjbG5dN`,
  slot 454578245). The 3,600 was a devnet value passed at genesis, which gave
  every gateway the maximum tenure weight four hours after joining. Tenure
  affects only observer selection. The new value applies from the next tally.

## Who had to act

This section starts with the 2026-09-15 upgrade. Client impact for earlier
events wasn't recorded here.

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

From the 2026-09-27 `ario-gar` upgrade, an epoch that ends with no observations
pays no rewards. Distribution marks it complete without touching gateway stats
or the treasury, and emits `EpochSkippedNoObservationsEvent` followed by
`EpochDistributedEvent`. The tokens stay in the treasury and fund later epochs.
Paying an unobserved epoch would record a pass for every gateway, which resets
failure streaks and raises the pass rates that pruning and the ArNS discount
depend on. See the addendum to ADR-0034.

To remove departed gateways between epochs, a cranker must run `@ar.io/sdk`
4.5.0 or later. Crankers on earlier versions call `finalize_gone` only from
mid-epoch cleanup, where it always returns 6102.

For the 2026-10-08 `ario-gar` upgrade, no client had to change: it adds one
instruction and one event (`TenureWeightUpdatedEvent`) and changes no account
layout or error code. Two behaviours changed:

- `delegate_stake` applies the gateway's minimum only to a new delegator. A
  client that refused top-ups below the minimum for existing delegators, such
  as the network portal or the `@ar.io/sdk` CLI, can now allow any amount above
  zero.
- Observer selection weights use 180-day tenure measured at the epoch start,
  from the first tally after the 2026-10-08 change. Gateways that joined
  recently become less likely to be prescribed.
