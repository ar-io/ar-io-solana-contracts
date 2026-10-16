# Reclaim rent from expired, unclaimed ANTs

At migration, the unclaimed ANTs were minted to the cold custody wallet
`45ZuEb1Jk7pbjshD1BVasBekAXhimdWuJjyswQzMyTB1` for the claims service to hand
out. When an ANT's ArNS name has expired and been pruned, no ArNS record points
at it any more, and the ANT returns to AR.IO (decided 2026-10-15). This tool
closes those ANTs' state and burns the assets, returning their rent to the
custody wallet.

## What it does

A **candidate** is an MPL Core asset that:

- has the custody wallet as its Core owner;
- carries a non-empty `ArNS Name` trait, so it once had a name;
- is referenced by **no** ArnsRecord (`record.ant`), so the name is neither
  active nor in its grace period;
- has its `AntConfig` and `AntControllers` PDAs;
- is not in the `--keep` list.

For each candidate, using only **owner-gated** instructions:

1. `ario_ant::close_ant_record_metadata_for_owner` for each metadata PDA
2. `ario_ant::close_ant_record` for each record PDA
3. `ario_ant::close_ant_controllers`, `ario_ant::close_ant_config`
4. MPL Core `BurnV1`

All rent goes to the owner. No admin key, no migration window and no program
change are needed.

**State is closed before the burn.** The owner-gated closes read the owner from
the live asset, so burning first would leave the state reachable only through
the admin-only `admin_close_orphaned_ant_state`.

**Instructions are packed into as few transactions as fit, with the burn
last.** 582 of the 584 ANTs fit in one transaction, so each is reclaimed
atomically. The 2 largest (21 and 24 records) take two transactions.

**A burned asset is not fully reclaimed.** MPL Core `BurnV1` leaves a 1-byte,
Core-owned tombstone holding the 1-byte rent minimum (655,320 lamports on
mainnet). The estimates subtract it.

**It is idempotent.** Every run re-reads chain state and sends only what is
still needed. If a run stops partway through an ANT, the next run closes that
ANT's remaining records and then burns it. Finished ANTs are reported as
`done`, and nothing is sent for them.

**Guards are re-checked against live state before any transaction for an
ANT:** the asset must still be an AssetV1, owned by the signer, with its
`ArNS Name` trait. A targeted scan must show that no ArnsRecord points at it.
Every transaction is simulated before it is sent, and the run stops at the
first simulation failure or incomplete ANT.

## Numbers (dry run 2026-10-15)

| | |
|---|---|
| ANTs held by custody | 2,165 |
| still referenced by an ArNS record (active or in grace) | 1,581 — untouched |
| candidates | 584 |
| transactions | 586 (582 × 1, 2 × 2), every one simulated OK on mainnet |
| reclaimable | **7.214122 SOL**, after the 584 tombstones and before fees (~0.004 SOL at the default priority fee) |

## Run it

```bash
cd scripts/reclaim-expired-ants
npm ci

# 1. Dry run: builds and simulates every transaction; nothing is signed.
RPC_URL=<mainnet rpc> node reclaim.mjs --json dryrun.json

# 2. One ANT for real, then check it on chain.
RPC_URL=<mainnet rpc> AUTHORITY_KEYPAIR=~/ario-mainnet-keys/authority-keypair.json \
  node reclaim.mjs --asset <ADDR> --execute

# 3. The rest. Re-running is safe; finished ANTs are skipped.
RPC_URL=<mainnet rpc> AUTHORITY_KEYPAIR=~/ario-mainnet-keys/authority-keypair.json \
  node reclaim.mjs --execute --json run.json
```

The signer must be the custody wallet; the script refuses any other key on
mainnet. `--limit N` processes the first N candidates (sorted by name).
`--keep FILE` takes a JSON array of asset addresses never to touch.

**Before step 2, retire these ANTs in the claims ledger**
(`ar-io-solana-attestor`, packages/claims). It still lists them as outstanding
ANT claims, and an approved dispatch of a burned ANT would fail.

## Rehearse on a fork

```bash
surfpool start --rpc-url <mainnet rpc> --port 18899 --ws-port 18900 --no-tui &

# Hand a few candidates to a throwaway key on the fork (fork only; also tops
# up Surfpool's higher rent for the burn tombstone).
node fork-prepare.mjs --keypair /tmp/throwaway.json --asset A --asset B

DISCOVERY_RPC_URL=<mainnet rpc> AUTHORITY_KEYPAIR=/tmp/throwaway.json \
  node reclaim.mjs --cluster fork --execute --asset A --asset B
```

On a fork the scans read mainnet (`DISCOVERY_RPC_URL`), and every live check
and transaction goes to the fork.

### Rehearsal 2026-10-15 (Surfpool 1.2.1, mainnet fork at slot 457,369,897)

- **10 ANTs fully reclaimed.** They covered 1, 2, 4 and 9 records in one
  transaction each, and 21 and 24 records in two.
  - **Resume:** `thiagoarns` had 3 of its 11 records closed by hand first; the
    run closed only the remaining 8, then burned it.
- **After each ANT:** config, controllers and every record were closed, and the
  asset was a 1-byte Core tombstone.
- **The balance reconciled exactly:** +0.050418 SOL net for an estimate of
  0.050432 SOL, the difference being the two transaction fees.
- **Idempotency:** re-runs over the same ANTs sent nothing and reported them
  `done`. A final run over all 10, one of them new, reclaimed only the new one.
- **Guards:**
  - an ANT with an active name, handed to the signer on the fork and passed
    explicitly, was excluded as still referenced, and was left untouched;
  - an ANT not owned by the signer was skipped as `owner-is-…`, and nothing
    was sent.
