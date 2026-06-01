# Program ID Manifests

Each file in this directory pins the live program IDs for one cluster.
Downstream clients (SDK, migration tooling, indexers, dApps) read these
files at build time so they don't have to hard-code addresses.

| File           | Cluster        | Source of truth | Updated by |
|----------------|----------------|-----------------|------------|
| `staging.json`  | devnet (staging) | `scripts/devnet-deploy.sh` | `release-devnet` workflow on every merge to `develop` (commit + push) |
| `mainnet.json` | mainnet-beta   | original mainnet deploy + Squads multisig upgrades | manual PR on the rare occasion a program is redeployed at a new ID |
| `localnet.json` *(optional)* | localnet | `scripts/start-localnet.sh` writes `localnet/out/localnet.env`; consumers read that instead | not committed |

## Format

```json
{
  "cluster":  "<rpc url or 'mainnet'/'devnet'>",
  "deployer": "<authority pubkey>",
  "deployed_at": {
    "ario_core":       "<ISO 8601 timestamp>",
    "ario_gar":        "<ISO 8601 timestamp>",
    "ario_arns":       "<ISO 8601 timestamp>",
    "ario_ant":        "<ISO 8601 timestamp>",
    "ario_ant_escrow": "<ISO 8601 timestamp or null>"
  },
  "programs": {
    "ario_core":       "<program id>",
    "ario_gar":        "<program id>",
    "ario_arns":       "<program id>",
    "ario_ant":        "<program id>",
    "ario_ant_escrow": "<program id or null if not yet deployed>"
  },
  "mints": {
    "ario": "<ARIO SPL token mint pubkey>"
  },
  "external": {
    "mpl_core": "CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d"
  }
}
```

* **`programs`** — the live AR.IO programs on this cluster. Any value
  may be `null` to indicate "not yet deployed on this cluster"; the
  release workflows refuse to run a mainnet deploy with `null` entries
  but tolerate them on devnet (so a partial cluster bring-up is
  representable).
* **`deployed_at`** — an object keyed by program name. Each value is
  the ISO-8601 timestamp at which that program was last deployed (or
  upgraded) on this cluster. Programs are timestamped independently
  because each `solana program deploy --program-id` call lands in its
  own slot, and on devnet they're often re-upgraded one at a time. A
  `null` entry mirrors a `null` entry under `programs` (not yet
  deployed).
* **`mints`** — well-known SPL token mint pubkeys created on this
  cluster. Today only `ario` (the ARIO token mint, owned by the
  `ario-core` program). Listed here because downstream tooling
  (associated-token-account derivation, balance queries, the SDK's
  ARIO-specific helpers) needs to know the mint address but cannot
  derive it from program IDs alone.
* **`external`** — program IDs we depend on but don't own (Metaplex
  Core for the ANT NFT plugin path).
* **`upgrade_authority` / `upgrade_authority_kind`** *(mainnet only)* —
  the program-upgrade authority pubkey and how it's secured.

## Why this is committed

* Reproducible builds — the same `git checkout` plus a known cluster
  always resolves to a known program ID set.
* Devnet is volatile but predictable: the `release-devnet` workflow
  deploys *upgrades* against the same program IDs (`solana program
  deploy --program-id`), so `staging.json` only changes when an ID is
  intentionally rotated.
* Mainnet IDs are baked into `declare_id!()` — a CI step checks that
  `programs/<crate>/src/lib.rs` matches `mainnet.json` before any
  buffer-staging step runs, so drift can't silently land.

## Usage from downstream

```ts
// SDK example
import staging from '@ar.io/solana-contracts/program-ids/staging.json';
const arioCore = staging.programs.ario_core;
```

```bash
# Shell tooling
jq -r .programs.ario_core program-ids/staging.json
```
