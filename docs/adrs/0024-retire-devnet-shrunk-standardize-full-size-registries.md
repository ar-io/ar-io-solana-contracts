# ADR-024: Retire `devnet-shrunk`, Standardize Full-Size Registries

* **Status:** accepted
* **Date:** 2026-06-01
* **Deciders:** @vilenarios

> **TL;DR:** Remove the compile-time `devnet-shrunk` feature (and the
> now-orphaned admin-shrink instructions); every cluster — localnet,
> staging, mainnet — compiles the same production-sized zero-copy
> registries, so a single build artifact and a single published TS client
> match every deploy.

## Context and problem statement

Three large zero-copy accounts were sized by a compile-time Cargo feature
`devnet-shrunk`:

| Account | full-size | `devnet-shrunk` |
|---|---|---|
| `GatewayRegistry.gateways` (`MAX_GATEWAYS`) | 3,000 | 30 |
| `NameRegistry.names` (`INITIAL_CAPACITY`) | 50,000 | 200 |
| `Epoch.failure_counts` | 3,000 | 30 |

The intent was cheaper devnet rent and smaller test artifacts. In
practice it became a persistent foot-gun:

* **Artifact/account-size mismatch.** A `.so` built with one feature set
  silently rejects accounts created under the other
  (`#[cfg]`-conditional array lengths change the borsh/zero-copy
  layout). Downstream consumers (`@ar.io/sdk`, migration tooling) had to
  track which size a given deploy used.
* **IDL duplication.** Anchor's IDL generator emits *both* `#[cfg]`
  variants of a conditional field regardless of the active feature, so
  `clients/ts` carried a `dedupeCfgConditionalFields` codegen pass keyed
  off a `CLUSTER` env var purely to pick one size — extra machinery whose
  only job was to undo the feature.
* **Release-pipeline coupling.** `build-sbf.sh` mapped
  `BUILD_NETWORK=devnet → --features devnet-shrunk`, and the release
  workflows offered a `devnet` cluster choice. A staging release attempt
  failed because `build-sbf.sh` derived a non-existent
  `--features network-staging` from the cluster input — the shrink/cluster
  coupling had accreted enough special cases to break.
* **Test-path divergence.** `scripts/test-integration.sh` passed
  `--features devnet-shrunk` so tests exercised the *small* layout, not
  the layout that actually ships. The SDK is already cluster-agnostic
  (it reads registries by `count` + manual slot offsets, not by a
  hard-coded capacity — `io-readable.ts:392`), so the shrink bought
  nothing on the read path.

We are deploying to mainnet imminently and want one registry layout that
every environment — localnet, the staging dress-rehearsal (which runs on
Solana devnet), and mainnet — shares byte-for-byte.

The `admin_shrink_gateway_registry` / `admin_shrink_name_registry`
instructions existed only to walk a registry *back down* after a shrink;
with no shrink mode they are dead weight, and a shrink that drops
populated slots is a data-loss footgun we never want in production.

## Decision drivers

* One build artifact must match every cluster — no per-cluster
  account-size skew between localnet, staging, and mainnet.
* The published `@ar.io/solana-contracts` client must encode one set of
  sizes, so a single npm version works for every downstream consumer.
* Remove release-pipeline special cases that have already caused a failed
  staging release.
* Preserve Anchor error-code stability — error variants are positional
  (`6000 + index`), and downstream tables (cranker / observer
  `errors.ts`) pin those numbers.

## Considered options

1. **Option A — Retire `devnet-shrunk` entirely; all clusters full-size.**
   Delete the feature, the `#[cfg]` gates, and the admin-shrink
   instructions; staging maps to `network-devnet`.
2. **Option B — Keep `devnet-shrunk` but fix the staging mapping.**
   Patch only `build-sbf.sh` so staging stops deriving
   `network-staging`, leaving the dual-size machinery in place.
3. **Option C — Make registry sizes a runtime/config value.** Rejected as
   over-engineering: zero-copy layouts are fixed at compile time by
   design, and initial sizing is a protocol-wide policy, not a per-deploy
   knob (see the "Hardcoded initial capacity" decision in
   [`DECISIONS.md`](../DECISIONS.md)).

## Decision

> Adopt **Option A**. The `devnet-shrunk` Cargo feature is removed from
> all five program manifests; `GatewayRegistry`, `NameRegistry`, and
> `Epoch` are unconditionally full-size; `admin_shrink_gateway_registry`
> and `admin_shrink_name_registry` (handlers, dispatch arms, and account
> structs) are deleted. `staging` is normalized to the `network-devnet`
> attestor binding because it runs on Solana devnet; there is no
> `network-staging` feature. The release workflows offer only `staging`
> and `mainnet`.

This satisfies the drivers directly: a single `.so` and a single TS
client encode one layout, so there is no cluster skew and no dedup pass
that can pick the "wrong" size. The previously-failing staging release
path is removed rather than patched. `NameRegistry` retains its
post-deploy `admin_expand_name_registry` growth path (50K → 200K), so
losing the *shrink* direction costs nothing operationally — growth, not
shrink, is the lifecycle we actually use.

Error-code stability is preserved by **keeping the shrink error variants
reserved** rather than deleting them: `GarError::RegistryAlreadyShrunk` /
`ShrinkWouldLoseData` and the `ArnsError` equivalents stay in place (now
unused, marked "Reserved — formerly admin-shrink; do NOT reuse") so every
later variant keeps its `6000 + index` code and downstream `errors.ts`
tables don't drift.

Reversible? In principle yes, but reopening would require reintroducing a
dual-layout build and the SDK/release coupling we just deleted — the
trigger would have to be a concrete devnet-rent problem that the
read-by-count SDK path doesn't already neutralize. None exists today.

## Consequences

### Positive

* One `.so` per program matches localnet, staging, and mainnet
  byte-for-byte; no account-size mismatch class of bug.
* One published `@ar.io/solana-contracts` version works for every
  consumer; the `staging` vs `@latest` dist-tags differ only by release
  channel, not encoded sizes.
* `build-sbf.sh`, `test-integration.sh`, and both release workflows lose
  their shrink/cluster special-casing; the staging release path is fixed.
* `dedupeCfgConditionalFields` becomes a defensive no-op (no
  `#[cfg]`-conditional fields remain to duplicate).
* The data-loss-prone admin-shrink instructions are gone from the ABI.

### Negative / risks

* Devnet/localnet accounts now pay full-size rent
  (`GatewayRegistry` ~120 KB, `NameRegistry` 2 MB at 50K). Acceptable —
  test validators are ephemeral and staging is funded like mainnet.
* The instruction ABI loses two `admin_shrink_*` entries. No deployed
  client calls them (shrink was never part of a production flow), so this
  is a removal of unused surface, not a breaking change to a live path.

### Neutral

* `CLUSTER` survives in `clients/ts` codegen for logging and the
  defensive dedup tie-break only; it no longer changes generated output.
* Shrink error variants remain in the enums (reserved) — they occupy
  their codes forever to keep later variants stable.

## Implementation notes

Landed in one PR off `develop`:

* `programs/*/Cargo.toml` — drop `devnet-shrunk` feature.
* `ario-gar/src/state/mod.rs`, `ario-arns/src/state/mod.rs` — remove
  `#[cfg]` gates; `MAX_GATEWAYS = 3000`, `INITIAL_CAPACITY = 50_000`,
  `failure_counts: [u16; 3000]` unconditionally; collapse the size tests.
* `ario-gar` / `ario-arns` `lib.rs` + `instructions/initialize.rs` —
  delete `admin_shrink_*` dispatch, handlers, and account structs; keep
  `admin_expand_name_registry`.
* `ario-gar` / `ario-arns` `error.rs` — mark shrink variants reserved.
* `build-sbf.sh` — `BUILD_NETWORK=staging` normalizes to
  `network-devnet`; drop the `devnet-shrunk` branch.
* `scripts/test-integration.sh` — drop `--features devnet-shrunk`;
  escrow still gets `unsafe-allow-test-attestor-pubkey`.
* `.github/workflows/release.yml`, `release-clients-ts.yml` — cluster
  choice is `staging | mainnet`; staging gets the unique
  `-staging.<run>` npm prerelease suffix (previously devnet's).
* `clients/ts/scripts/codegen.mjs` — `CLUSTER` ∈ `{staging, mainnet}`,
  default `staging`; dedup is a documented defensive no-op.

## Related

* Code: `programs/ario-gar/src/state/mod.rs`,
  `programs/ario-arns/src/state/mod.rs`,
  `programs/ario-gar/src/instructions/initialize.rs`,
  `programs/ario-arns/src/instructions/initialize.rs`, `build-sbf.sh`,
  `scripts/test-integration.sh`, `clients/ts/scripts/codegen.mjs`
* Docs: [`DECISIONS.md`](../DECISIONS.md) ("Hardcoded initial capacity vs
  configurable"), [`clients/ts/README.md`](../../clients/ts/README.md)
  ("Cluster coupling")
* Supersedes the `devnet-shrunk` sizing and the `admin_shrink_*`
  instructions introduced alongside the NameRegistry dynamic-capacity work
  in `DECISIONS.md`.
