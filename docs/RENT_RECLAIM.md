# Rent Reclamation — Operator & User Guide

Reference for the 6 instructions added by ADR-019 in PR
`feat/rent-reclaim-mainnet`. Covers what each does, who can call it,
what rent gets back, and how to run them in production.

## Quick reference

| Instruction | Program | Caller | Purpose | Rent → |
|---|---|---|---|---|
| `close_ant_record` | ario-ant | NFT owner | Close one record PDA | caller |
| `close_ant_record_metadata_for_owner` | ario-ant | NFT owner | Close one metadata PDA | caller |
| `close_ant_controllers` | ario-ant | NFT owner | Close the `AntControllers` PDA | caller |
| `close_ant_config` | ario-ant | NFT owner | Close the `AntConfig` PDA | caller |
| `admin_purge_unclaimed_ant` | ario-ant-escrow | `ArioConfig.authority` | Burn an abandoned escrow ANT (5y grace) | authority |
| `admin_close_orphaned_ant_state` | ario-ant | `ArioConfig.authority` | Post-burn cleanup of per-ANT PDAs | authority |

## User flow: burn my ANT and recover my rent

You own an ANT NFT (post-claim) and want to delete it permanently.
Each PDA close refunds its rent to you. Order doesn't matter:

```
# 1. Close every AntRecord for the ANT (one ix per undername)
close_ant_record(undername="@")
close_ant_record(undername="blog")
close_ant_record(undername="docs")
# ...

# 2. Close any AntRecordMetadata PDAs (if metadata was ever set)
close_ant_record_metadata_for_owner(undername="@")
# ...

# 3. Close AntControllers
close_ant_controllers()

# 4. Close AntConfig
close_ant_config()

# 5. Burn the mpl-core asset itself (standard mpl-core ix, not ario-ant)
mpl_core::burn_v1()
```

Per-ANT recovery at 2026-05-21 sizes (~4.1 records avg, no metadata):
- AntConfig: ~0.00315 SOL
- AntControllers: ~0.00137 SOL
- 4 AntRecord × 0.00309 SOL = ~0.01236 SOL
- **Total: ~0.0169 SOL ≈ $1.44 at $85/SOL**

(The mpl-core BurnV1 itself doesn't refund the asset's ~0.00146 SOL —
mpl-core leaves the asset as a System-owned tombstone with original
rent. We accept this since closing your own ANT is rare.)

## Operator flow: purge abandoned escrow ANTs (5+ years post-cutover)

Once mainnet has been live for 5+ years, `EscrowAnt` PDAs deposited at
cutover and never claimed become eligible for purge. The
`migration_authority` (or whichever key is set as
`ArioConfig.authority`) sweeps them via cron:

```bash
# Dry-run: scan for eligible escrows but don't transact
RPC_URL=https://api.mainnet-beta.solana.com \
ARIO_ANT_ESCROW_PROGRAM_ID=<pubkey> \
  yarn tsx migration/import/src/purge-stale-escrow.ts --dry-run

# Live: process up to 100 at a time
RPC_URL=https://api.mainnet-beta.solana.com \
ARIO_ANT_ESCROW_PROGRAM_ID=<pubkey> \
AUTHORITY_KEYPAIR=/path/to/authority.json \
  yarn tsx migration/import/src/purge-stale-escrow.ts --limit 100
```

After purge, the mpl-core asset is marked `Uninitialized`. The
per-ANT PDAs (AntConfig/Controllers/Records) — if they were ever
initialized — remain. Sweep them via the companion script:

```bash
# Pass burned asset pubkeys (collected from purge-stale-escrow output)
RPC_URL=... ARIO_ANT_PROGRAM_ID=... AUTHORITY_KEYPAIR=... \
  yarn tsx migration/import/src/cleanup-orphaned-ant-state.ts \
    --asset <pubkey1> --asset <pubkey2> ...
```

## What's reclaimable, what's not

| Account | Rent (post-shrink) | Reclaimable? | How |
|---|---|---|---|
| ArnsRecord | ~0.00221 SOL | Eventually (lease expiry — separate ix not in scope here) | — |
| ANT NFT mint | ~0.00146 SOL | No (mpl-core BurnV1 limitation) | — |
| AntConfig | ~0.00315 SOL | Yes | `close_ant_config` or `admin_close_orphaned_ant_state` |
| AntControllers | ~0.00137 SOL | Yes | `close_ant_controllers` or `admin_close_orphaned_ant_state` |
| AntRecord (per record) | ~0.00309 SOL | Yes | `close_ant_record` or `admin_close_orphaned_ant_state` |
| AntRecordMetadata (per record) | ~0.00608 SOL | Yes | `close_ant_record_metadata_for_owner` / `close_orphaned_record_metadata` / `admin_close_orphaned_ant_state` |
| EscrowAnt (per escrow) | ~few k lamports | Yes | `cancel_deposit` (depositor) / `admin_purge_unclaimed_ant` (5y grace + authority) |

## Security model

- **User closes** are NFT-owner-gated via `read_mpl_core_owner` on
  every call. Stale owners (after NFT transfer) are rejected with
  `NotNftHolder`.
- **`admin_purge_unclaimed_ant`** is double-gated: signer must equal
  `ArioConfig.authority` AND
  `Clock::slot - escrow.deposit_slot >= UNCLAIMED_PURGE_GRACE_SLOTS`
  (~394M slots ≈ 5 years). Either gate rejects independently.
- **`admin_close_orphaned_ant_state`** requires the mpl-core asset
  to be in post-burn state (System-owned, empty data) — refuses on
  live assets via `AssetStillExists`. This prevents authority from
  closing state that a user could still reconcile.

## Grace period

`UNCLAIMED_PURGE_GRACE_SLOTS = 394_200_000` is hardcoded. At Solana's
~2.5 slots/s steady-state this is approximately 5 years. Slot-based
because `EscrowAnt.deposit_slot: u64` is the only on-chain time
anchor available — slot vs wall-clock drift at this horizon is
irrelevant. Changing the grace period requires a contract upgrade
(`UNCLAIMED_PURGE_GRACE_SLOTS` is a `pub const`, not a config field).

## Test coverage

- `programs/ario-ant/tests/integration.rs`: 9 tests covering happy paths,
  auth rejection, order independence, end-to-end close sequence,
  post-burn precondition.
- `programs/ario-ant-escrow/tests/integration.rs`: 3 tests covering auth
  rejection, time-gate rejection, and successful purge after
  `ctx.warp_to_slot()` past the grace.
- `programs/ario-ant-escrow/src/mpl_core_cpi.rs`: 2 unit tests pin
  the BurnV1 instruction-data byte layout + metas slot map.
