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

### Cluster coupling (current limitation)

Two zero-copy registry accounts use `#[cfg(feature = "devnet-shrunk")]`
to swap fixed-array sizes between mainnet and devnet:

| Account | Mainnet | Devnet |
|---|---|---|
| `GatewayRegistry.gateways` | 3,000 slots | 30 slots |
| `NameRegistry.names` | 200,000 slots | 200 slots |
| `Epoch.failure_counts` | 3,000 slots | 30 slots |

The Codama encoders embed these sizes at codegen time. **The 0.x line of
this package targets the devnet sizes** (matches the currently-deployed
devnet binaries + the published `@ar.io/sdk@solana` client). When the
contracts land on mainnet at production sizes, expect a major bump
(`1.0.0`+) targeting those sizes. We may switch to cluster-suffixed
dist-tags (e.g. `@ar.io/solana-contracts@devnet` vs `@latest`) at that
point — TBD.

In the meantime, downstream consumers building against a deploy of the
contracts with the opposite feature flag will get encoder/account-size
mismatches at runtime. The registries used by migration tooling
(`Balance`, `Gateway`, `ArnsRecord`, `Vault`, etc.) are NOT affected —
only the three large zero-copy registries above.

## Local dev

```bash
git clone https://github.com/ar-io/ar-io-solana-contracts.git
cd ar-io-solana-contracts
anchor build              # produces target/idl/*.json
cd clients/ts
yarn install              # postinstall regenerates src/<program>/
yarn build:tsc            # compiles to lib/
```

## License

[AGPL-3.0-or-later](./LICENSE).
