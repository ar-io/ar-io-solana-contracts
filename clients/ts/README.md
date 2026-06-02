# `@ar.io/solana-contracts`

Codama-generated TypeScript client for the AR.IO Solana programs.

| Program | Subpath |
|---|---|
| `ario-core` (ARIO SPL token, vaults, primary names) | `@ar.io/solana-contracts/core` |
| `ario-gar` (Gateway Address Registry, staking, epochs, rewards) | `@ar.io/solana-contracts/gar` |
| `ario-arns` (ArNS name registry, demand factor, pricing) | `@ar.io/solana-contracts/arns` |
| `ario-ant` (Arweave Name Token — Metaplex Core NFT) | `@ar.io/solana-contracts/ant` |
| `ario-ant-escrow` (trustless multi-protocol ANT custody) | `@ar.io/solana-contracts/ant-escrow` |

Built on [`@solana/kit`](https://github.com/anza-xyz/kit) (peer dependency).
Generated with [Codama](https://github.com/codama-idl/codama).

## Install

```bash
yarn add @ar.io/solana-contracts @solana/kit
# or
npm install @ar.io/solana-contracts @solana/kit
```

## Use

```ts
import { getBalanceEncoder } from '@ar.io/solana-contracts/core';
import { PurchaseType }      from '@ar.io/solana-contracts/arns';
import { GatewayStatus }     from '@ar.io/solana-contracts/gar';

const bytes = getBalanceEncoder().encode({
  owner: '...',
  amount: 1_000_000n,
  bump: 254,
});
```

Each subpath exposes Codama's standard surface:

- `accounts/` — typed encoders/decoders for every PDA (`getBalanceEncoder`,
  `getBalanceDecoder`, `getBalanceCodec`, etc.)
- `types/` — instruction-argument structs + enums (`PurchaseType`,
  `GatewayStatus`, `Protocol`)
- `instructions/` — async / sync instruction builders
  (`getJoinNetworkInstructionAsync`, etc.)
- `pdas/` — PDA derivation helpers (`findBalancePda`, etc.)
- `errors/` — typed Anchor error constants per program
- `programs/` — Codama's program factory + `identify*Instruction` helpers

## What is this?

The AR.IO Network is migrating from AO (Arweave Object) to Solana. The
on-chain programs live in [this
repo](https://github.com/ar-io/ar-io-solana-contracts) under `programs/`;
this package is the auto-generated TypeScript client all downstream
consumers (the public AR.IO SDK, migration tooling, third-party
integrators) import from.

Generated artifacts (`src/<program>/` + `lib/`) are not committed — every
release runs `anchor build && yarn codegen && yarn build` from a clean
checkout and publishes the resulting tarball.

## Versioning

Independent semver. Wire-format changes in the underlying Anchor programs
(IDL-affecting source edits) bump the major. Additive surface changes
bump the minor. Patch versions cover non-breaking codegen fixes.

The package version is **not** locked to the deployed program version —
program identity is given by the chain-deployed program IDs, not by the
client package version.

### Cluster coupling (resolved — ADR-024)

Historically two zero-copy registry accounts used
`#[cfg(feature = "devnet-shrunk")]` to swap fixed-array sizes between
mainnet and a shrunk devnet, so the Codama encoders (which embed those
sizes at codegen time) only matched one cluster at a time.

**That feature was retired (ADR-024).** Every cluster — localnet,
staging (on Solana devnet), and mainnet — now compiles the same
production-sized registries:

| Account | All clusters |
|---|---|
| `GatewayRegistry.gateways` | 3,000 slots |
| `NameRegistry.names` | 50,000 slots (expandable to 200,000) |
| `Epoch.failure_counts` | 3,000 slots |

So a single published package matches every deploy — there is no
encoder/account-size mismatch to reason about, and the `staging` vs
`@latest` dist-tags differ only by release channel, not by encoded
sizes.

## Local dev

```bash
git clone https://github.com/ar-io/ar-io-solana-contracts.git
cd ar-io-solana-contracts
anchor build              # produces target/idl/*.json
cd clients/ts
yarn install              # postinstall regenerates src/<program>/
yarn build:tsc            # compiles to lib/
```

`CLUSTER` (`staging` | `mainnet`, default `staging`) now only selects the
npm release channel; registry sizes are identical across clusters (see
"Cluster coupling" above).

## Publishing

CI workflow `.github/workflows/release-clients-ts.yml` is
[workflow_dispatch]-triggered with `cluster` (staging | mainnet),
optional `version` override, and optional `dry_run`.

Auth uses **npm OIDC Trusted Publishing** — same model as `@ar.io/sdk`.
No `NPM_TOKEN` secret in CI. The workflow declares `id-token: write`;
npm CLI ≥9.5 negotiates the OIDC handshake with npmjs.com and attests
both publisher identity AND build provenance.

### First publish (chicken-and-egg)

Trusted publishers are configured **per package** on npm, so the
package must exist before the trusted-publisher rule can be added.
One-time bootstrap:

1. Publish `0.1.0-staging.0` manually from a local checkout with
   `npm publish --access public --tag staging` (using your personal
   npm credentials).
2. On npmjs.com: navigate to `@ar.io/solana-contracts` → **Settings**
   → **Trusted Publishers** → **Add Trusted Publisher**:
   - Publisher: GitHub Actions
   - Organization: `ar-io`
   - Repository: `ar-io-solana-contracts`
   - Workflow filename: `release-clients-ts.yml`
   - Environment: *(leave blank)*
3. Subsequent releases (staging rebuilds, mainnet promotion, version
   bumps) all run through CI — no token, no manual step.

### Dist-tags

| Tag | Cluster | Use for |
|---|---|---|
| `@latest` | mainnet | `yarn add @ar.io/solana-contracts` |
| `@staging` | staging (Solana devnet) | `yarn add @ar.io/solana-contracts@staging` |

## License

[AGPL-3.0-or-later](./LICENSE).
