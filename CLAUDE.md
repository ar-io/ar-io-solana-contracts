# CLAUDE.md

Guidance for Claude Code (claude.ai/code) and other AI assistants
working in this repository. Humans should read [`README.md`](README.md)
first; this file is the working notes / institutional memory layer that
agents need to avoid common mistakes.

> **Connecting to deployed clusters?** Program IDs and the ARIO mint
> live in [`program-ids/devnet.json`](program-ids/devnet.json) and
> [`program-ids/mainnet.json`](program-ids/mainnet.json). The on-chain
> `declare_id!()` literals only match a real keypair after the relevant
> deploy workflow has run — fresh checkouts of `develop` carry placeholder
> IDs. **Don't use `declare_id!()` as a source of truth for any cluster.**

## Project Overview

This repo is the Anchor workspace for the AR.IO protocol on Solana —
five Rust programs in one Cargo workspace, plus the localnet / release
tooling around them.

```
.
├── programs/
│   ├── ario-core/
│   ├── ario-gar/
│   ├── ario-arns/
│   ├── ario-ant/
│   └── ario-ant-escrow/
├── test-utils/        # shared event-log parser used by every program's tests
├── localnet/          # Surfpool feature-gate policy (sourced by start-localnet.sh)
├── program-ids/       # per-cluster program ID + mint manifests (committed)
├── clients/ts/        # @ar.io/solana-contracts — Codama-generated TS client,
│                      #   published in lockstep with contract releases
├── scripts/           # deploy / release / package tooling
├── docs/              # ADRs, behavioral diff, escrow specs (see "Reference Material")
└── .github/workflows/ # build-test, release, release-clients-ts, upgrade-devnet,
                       #   upgrade-mainnet, release-plz, docker-builder, codeql
```

Sister repositories — code that *depends on* this one but isn't in
scope here:

* [`ar-io/solana-ar-io`](https://github.com/ar-io/solana-ar-io) — AO →
  Solana migration tooling (snapshot exporter, import orchestrator,
  claim/escrow web apps, attestor service, downstream node forks).
  When CLAUDE.md notes mention `migration/`, `sdk/`, `cranker/`,
  `comms/` etc. — that's where they live.
* [`ar-io/ar-io-sdk`](https://github.com/ar-io/ar-io-sdk) — TypeScript
  SDK that consumes the IDLs published from this repo.
* [`ar-io/ar-io-cranker`](https://github.com/ar-io/ar-io-cranker) —
  standalone epoch cranker.

## Architecture: 4 + 1 Programs

* **ario-core** — ARIO SPL token (mint/transfer), user vaults
  (time-locked tokens), primary name resolution.
* **ario-gar** — Gateway Address Registry; operator/delegate staking,
  withdrawals, epoch lifecycle, observer selection, observation
  submission, batched reward distribution.
* **ario-arns** — ArNS name registry (buy/lease/permabuy), demand-factor
  pricing, reserved/returned names, lease management.
* **ario-ant** — Arweave Name Token as a Metaplex Core NFT (Attributes
  plugin for DAS traits), undername records, controller management.
* **ario-ant-escrow** — Trustless multi-protocol escrow for ANTs
  (Metaplex Core), ARIO tokens (SPL), and time-locked vaults
  (15 instructions). Arweave claims go through the off-chain attestor
  service that re-signs the canonical claim with Ed25519, verified
  on-chain via the native sigverify program + sysvar
  instruction-introspection (ADR-017). Ethereum verifies on-chain via
  `secp256k1_recover`. Vault claims use instruction introspection to
  enforce time-locks via `ario-core::vaulted_transfer`. The earlier
  on-chain RSA-PSS path was removed because `sol_big_mod_exp` is
  feature-gated on devnet/mainnet.

**CPI flow:** ario-gar and ario-arns call into ario-core for token ops.
ario-arns calls into ario-gar for stake-funded ArNS purchases
(fund-from-stakes). ario-ant reads `ArnsRecord` PDAs (owned by
ario-arns) when syncing the Metaplex Core Attributes plugin.

**Program structure.** ario-core / ario-gar / ario-arns are modularized
with `lib.rs` dispatching to `src/instructions/` modules (each with
`error.rs` and `migration.rs`). ario-ant is inline in `lib.rs` (not
modularized); state in flat `state.rs` (not `state/mod.rs`); validates
Metaplex Core ownership by checking `owner == MPL_CORE_PROGRAM_ID` (no
mpl-token-metadata dep). **AntRecord is split** into hot `AntRecord`
PDA and cold `AntRecordMeta` PDA (both keyed by `(ant_mint, undername)`)
to reduce per-record rent.

**ario-arns pricing math** in `pricing.rs` uses u128 intermediates for
overflow safety. Depends on `ario-gar` with `cpi` feature for gateway
operator-discount verification and fund-from-stakes CPI.

### ANTs as Metaplex Core NFTs

ANTs are Metaplex Core NFTs with an **Attributes plugin** (ArNS Name,
Type, Undername Limit) for DAS / marketplace queryability. Plugin
authority is `Owner` so the current holder can sign trait updates. The
migration claim flow transfers Owner AND UpdateAuthority to the user
atomically (ADR-013). AR.IO retains program-level upgrade authority via
BPFLoaderUpgradeable — **production deploys must NOT use `--final`**.

**Every minted ANT carries the Attributes plugin** (populated at mint
time for ANTs with an active ArNS record, empty otherwise). The
empty-plugin form keeps every ANT `purchase`-ready: `ario_arns::buy_record`
CPIs `UpdatePluginV1` to populate traits, which requires the plugin
already exists. Every ARIO-ARNS handler that mutates trait-affecting
state keeps the plugin coherent with `ArnsRecord` via this CPI;
permissionless `sync_attributes` reconciles cases where the buyer
wasn't the ANT owner.

The CPI is hand-rolled in `programs/ario-arns/src/mpl_core_cpi.rs` and
`programs/ario-ant-escrow/src/mpl_core_cpi.rs` (the `mpl-core` crate
doesn't compile on Cargo 1.79). Schema versioning via `migrate_ant`
(`version: u8` + `realloc`). See ADR-012 / BD-096.

### Zero-copy registries

Two large zero-copy accounts enable permissionless enumeration without
an indexer:

* `GatewayRegistry` (ario-gar) — 3,000 slots / 120KB, used for observer
  selection.
* `NameRegistry` (ario-arns) — 200,000 slots / 2MB, used for epoch name
  prescription.

Too large to create via `init` (exceeds Solana's 10KB
`MAX_PERMITTED_DATA_INCREASE`). **Tests must pre-create these accounts**
with the correct discriminator before the test starts:

```rust
let disc = hash(b"account:GatewayRegistry");
data[..8].copy_from_slice(&disc.to_bytes()[..8]);
pt.add_account(registry_key, Account { data, .. });
```

### Key constants

Defined in `ario-core/src/constants.rs` and shared. Critical gotchas:

* All times in **seconds** (not ms like the Lua source).
* Token decimals: 6 (1 ARIO = 1,000,000 mARIO via `ONE_TOKEN`).
* `RATE_SCALE = 1_000_000` for fixed-point percentage math
  (500_000 = 50%).
* Variable-length names hashed for PDA seeds:
  `hash(name.to_lowercase().as_bytes())`.
* See [`docs/COMPUTE_AND_LIMITS.md`](docs/COMPUTE_AND_LIMITS.md) for PDA
  seed patterns, account sizes, batching, fixed-point math.

### Name validation

ArNS names (validated in `ario-arns/src/pricing.rs`): 1–51 chars,
lowercase alphanumeric + hyphens, must start/end alphanumeric. **Length
43 is prohibited** (Arweave address collision). Single-char names must
be alphanumeric (no hyphens).

## Cross-Program Patterns

**CPI for token transfers** uses two patterns:

* Normal: `CpiContext::new(token_program, accounts)` when signer is a
  wallet.
* PDA signer: `CpiContext::new_with_signer(token_program, accounts,
  signer_seeds)` when a PDA (vault, config) authorizes the transfer.

**Cross-program reads** (no CPI). ario-arns reads ario-gar `Gateway`
accounts via `remaining_accounts` to verify gateway-operator status for
the 20% ArNS discount. Validated by checking owner, PDA seeds, and
deserialized state.

**Fund-from-stakes CPI** (ario-arns → ario-gar → SPL Token). Each
fee-paying ArNS instruction has `_from_delegation` and
`_from_operator_stake` variants in `purchase_from_stake.rs` /
`manage_from_stake.rs`. These CPI into ario-gar's
`deduct_delegation_for_payment` / `deduct_operator_stake_for_payment`
(in `payment.rs`) which transfer directly from `stake_token_account` to
`protocol_token_account` using the Settings PDA as signer. No
withdrawal vault, no lock period, no penalty. CPI depth = 2 (Solana max
= 4). ario-gar accounts use `UncheckedAccount` in ario-arns structs
(validated by ario-gar during CPI). For `buy_returned_name`, protocol
share is paid from stake via CPI; initiator share from buyer's wallet
via direct SPL transfer.

**Treasury release CPI** (ario-gar → ario-core → SPL Token). The
`protocol_token_account` SPL `Owner` authority lives on the
`ArioConfig` PDA (ario-core), permanently. Fresh deploys set this at
`initialize` time; legacy deployments arrived at this state via
`release_treasury_authority` (now removed) and stay there. **Only
ario-core can sign SPL transfers FROM the treasury.**

`ario-gar::distribute_epoch` therefore CPIs into
`ario_core::release_treasury_to_recipient` instead of signing the SPL
transfer directly. The CPI is **hand-rolled `invoke_signed`** (not
Anchor's typed CPI) because adding `ario-core` as a Cargo dep on
`ario-gar` would cycle with the existing `ario-core → ario-gar` cpi
dep used by fund-from-stakes. The hand-roll uses
`global:release_treasury_to_recipient` as the 8-byte discriminator and
signs with `[SETTINGS_SEED, settings.bump]` so the inner ix's
`gar_settings: signer` constraint is satisfied. ario-gar's source has
`pub const ARIO_CORE_PROGRAM_ID` (single line, `#[rustfmt::skip]`'d;
patched by `build-sbf.sh --sync-from-manifest` from
`program-ids/<cluster>.json`) used as `address =` on the
`ario_core_program` account to pin the CPI target.

ario-core's `release_treasury_to_recipient` verifies the caller is the
canonical ario-gar program via `seeds::program = config.gar_program`
on `gar_settings` — the GAR program ID is stored in `ArioConfig`
(mirrors the existing `arns_program` storage pattern). Fresh deploys
set `gar_program` at `initialize` via `InitializeParams.gar_program`;
legacy deployments populate it once via `admin_set_gar_program`
(authority-gated, reallocs the PDA by 32 bytes to accommodate the
appended field). The transfer destination is locked to
`gar_settings.stake_token_account` so even a buggy or compromised
ario-gar can only redirect treasury funds into GAR's own stake pool —
never an arbitrary recipient. CPI depth = 3 (gar → core → spl_token),
under Solana's 4-hop limit.

`ArioConfig.gar_program` is appended after `bump` in the struct so a
realloc on existing accounts only adds zero-filled trailing bytes
without shifting any existing field offsets. Other field-ordering
changes are NOT backward-compatible — see "ArioConfig" in the
schema-evolution checklist in [`docs/DECISIONS.md`](docs/DECISIONS.md)
before modifying.

## Build & Test Commands

> Comprehensive testing guide (patterns, troubleshooting, Surfpool
> cheatcodes, how to add tests): [`TESTING.md`](TESTING.md). The release
> flow is documented in [`README.md`](README.md) ("Release flow").

### First-time setup

```bash
git clone https://github.com/ar-io/ar-io-solana-contracts.git
cd ar-io-solana-contracts

# 1. Toolchain (one-time — see "Toolchain" below).
sh -c "$(curl -sSfL https://release.anza.xyz/v2.1.0/install)"
cargo install --git https://github.com/coral-xyz/anchor --tag v0.31.1 avm --locked
avm install 0.31.1 && avm use 0.31.1

# 2. Build (also generates target/idl/*.json).
anchor build

# 3. Test.
cargo test --workspace
```

After IDL changes (`anchor build` mutating an `ario_*` IDL), the in-tree
typed client at [`clients/ts/`](clients/ts/) (`@ar.io/solana-contracts`,
Codama-generated, `@solana/kit`-based) must be regenerated:

```bash
cd clients/ts && yarn codegen && yarn build:tsc
```

The npm package publishes from `.github/workflows/release-clients-ts.yml`,
coupled to contract releases — bump the contract, the client republishes.
Downstream SDK consumers (`ar-io-sdk`, etc.) then update their dep on
`@ar.io/solana-contracts`. The IDL ABI stability check
(`scripts/idl-event-snapshot.mjs`) catches breaking event changes; Anchor
IDLs are otherwise additive-safe.

### Contracts (Rust/Anchor)

```bash
cargo check                                     # fast, no BPF
./build-sbf.sh                                  # check + build; aborts on declare_id!() drift
./build-sbf.sh --sync                           # auto-sync declare_id!() to deploy keypairs;
                                                #   restores source on EXIT/INT/TERM/HUP
./build-sbf.sh --check-only                     # drift check only (pre-commit / CI)
./build-sbf.sh --skip-check                     # build regardless of drift

cargo test                                      # all tests
cargo test -p ario-core                         # one program
cargo test -p ario-gar test_join_network        # specific test

# ario-arns CPIs into Metaplex Core (UpdatePluginV1 trait sync) and needs
# mpl_core.so staged + BPF_OUT_DIR set:
cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/
BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-arns

# Devnet deploy (idempotent — re-runs upgrade against the same program IDs)
bash scripts/devnet-deploy.sh

# Local validator
bash scripts/start-localnet.sh
```

`scripts/start-localnet.sh` env toggles: `SKIP_BUILD=1` (use existing
`target/deploy/*.so`), `SURFPOOL_PORT=<n>`,
`SURFPOOL_SKIP_MAINNET_INACTIVE_DISABLES=1`,
`SURFPOOL_ENABLE_ALL_SVM_FEATURES=1`.

**`BPF_OUT_DIR` pitfall:** Set it ONLY for `ario-arns` (and the
`ario-ant-escrow` event-coverage tests). For
`ario-core` / `ario-gar` / `ario-ant`, leave it unset — otherwise
`solana-program-test` loads stale prebuilt `.so` files from
`target/deploy/` instead of the just-compiled native processor, and
recently-added instructions fail with `InstructionFallbackNotFound
(101)`. Fix: `unset BPF_OUT_DIR` or delete the stale
`target/deploy/<program>.so` first.

**`declare_id!()` drift:** `cargo build-sbf` / `anchor build` embed the
`declare_id!("...")` literal into the `.so`. Deployments use
keypair-derived program IDs from `target/deploy/*-keypair.json`. When
they mismatch, Anchor's runtime check rejects the first CPI with
`DeclaredProgramIdMismatch (#4100)`. Source placeholders
(`ARioGarProgramXXX...`) are intentional — they never match a real
keypair. Localnet flows use `./build-sbf.sh --sync`. For devnet/mainnet,
the deploy keypair IS production, so the deploy workflows commit
`anchor keys sync` results back to the branch.

**`idl-build` feature must propagate through cpi deps.** When program A
depends on program B with `features = ["cpi"]` and you `anchor build`,
Cargo's feature unifier doesn't auto-activate `idl-build` on B through
the cpi edge. If B (or anything B transitively pulls in like
`anchor-spl`) emits IDL, you'll see `error[E0599]: no associated
function or constant named DISCRIMINATOR found for struct
anchor_spl::token::TokenAccount` and the build aborts. **Fix:** in A's
`Cargo.toml`, ensure `idl-build = ["anchor-lang/idl-build", ...,
"<B>/idl-build"]`. Current chain: `ario-ant/idl-build →
ario-arns/idl-build → ario-gar/idl-build → anchor-spl/idl-build`. New
program-to-program cpi deps need a parallel entry.

## Toolchain

* Anchor 0.31.1, Solana 2.1.x (Agave CLI),
  [Surfpool](https://github.com/solana-foundation/surfpool) (localnet
  RPC).
* `Cargo.toml` pins `solana-*` =2.1.0 and several workspace deps
  (`blake3`, `proc-macro-crate`, `indexmap`, `unicode-segmentation`,
  with explicit per-program versions) so `cargo-build-sbf` (Cargo
  **1.79** / rustc **1.79**) can resolve the dep graph.
* Rust edition 2021. Key deps: `anchor-lang` (with `init-if-needed`),
  `anchor-spl`, `bytemuck` (zero-copy), `solana-program-test`.
* License: AGPL-3.0-or-later.

### Surfpool SVM feature gates (localnet)

`scripts/start-localnet.sh` sources
[`localnet/surfpool-svm-features.sh`](localnet/surfpool-svm-features.sh),
which is the **canonical list** of `--disable-feature` tokens (and
optional `--features-all`). Default policy: keep Surfnet
**mainnet-beta-style** so BPF and tooling fail early when they depend
on syscalls or runtime knobs production hasn't flipped on yet — not
"turn on every experimental SVM feature."

Env toggles:

* `SURFPOOL_SKIP_MAINNET_INACTIVE_DISABLES=1` — omit the scripted
  `--disable-feature` list.
* `SURFPOOL_ENABLE_ALL_SVM_FEATURES=1` — prepend `--features-all`
  (explicit disables still apply afterward).

Symbolic tokens come only from Surfpool's `lookup_feature_by_name()`
table; raw pubkeys from `solana feature status` usually fail unless
Surfpool's pinned `agave-feature-set` lists them. Many mainnet-inactive
rows have no CLI token on Surfpool 1.1.x and can't be mirrored until
upstream adds aliases.

**Practical consequence:** with `enable-big-mod-exp-syscall` disabled,
`ario-ant-escrow` does not load (BPF links `sol_big_mod_exp` for
Arweave RSA-PSS). Use `SURFPOOL_SKIP_MAINNET_INACTIVE_DISABLES=1` when
you need escrow RSA paths on Surfnet.

**Contract planning:** treat **active mainnet SVM behavior** as the
portability baseline. Don't design production programs or CPIs around
experimental flags / optional syscalls / inactive precompiles. If you
need them for research, gate tests behind env (Surfpool skips or
separate clusters) rather than baking them into the default protocol
path.

## Code Conventions

### PDA `bump` doc comments

Every stored `bump` field on an Anchor `#[account]` struct (or anywhere
`ctx.bumps.<account>` is captured into state) must have a doc comment
explaining (1) it's the canonical bump for that PDA's seeds, and (2)
it's stored to skip the on-chain `find_program_address` search on
subsequent loads — passing it via `seeds = [...], bump = self.bump`
saves ~1.5k CU per access. Comments may reference the seed pattern in
[`docs/COMPUTE_AND_LIMITS.md`](docs/COMPUTE_AND_LIMITS.md) rather than
repeating it. Applies to `#[account]` structs in `state.rs` /
`state/mod.rs`, account-context structs in `instructions/*.rs`, and any
`init` / `realloc` site that captures a bump into state.

### Anchor `#[event]` ABI policy

Per ADR-017: shipped events are append-only. The borsh layout (field
name + type + order + discriminator) of any event that has been emitted
on a public cluster is **permanent** — every indexer / decoder
subscribed to the event depends on it. To change a shipped event's
shape, ship a new `*EventV2` and deprecate the old one in
[`docs/EVENTS.md`](docs/EVENTS.md); never mutate in place.

The frozen surface lives in
[`idl-event-snapshots.json`](idl-event-snapshots.json). After
`anchor build`:

```bash
node scripts/idl-event-snapshot.mjs           # check vs snapshot
node scripts/idl-event-snapshot.mjs --update  # bless intentional additions
```

Adding a new event always lands in two commits (new event in source +
snapshot bump). The CI `build-test.yml` workflow runs the check on
every PR.

## Reference Material

[`README.md`](README.md) is the entry point. Beyond it, the most useful
docs (start here, not the alphabetical list at the end):

* [`TESTING.md`](TESTING.md) — testing patterns, Surfpool cheatcodes,
  troubleshooting, how to add tests.
* [`docs/adrs/`](docs/adrs/) — Architecture Decision Records, MADR
  format, one file per decision. See
  [`docs/adrs/README.md`](docs/adrs/README.md) for the index and
  workflow; new decisions go here.
* [`docs/DECISIONS.md`](docs/DECISIONS.md) — historical ADR-001
  through ADR-018 (still authoritative, pending migration into
  `docs/adrs/`). Recent: ADR-014 trustless escrow, ADR-015 mint
  authority revocation, ADR-016 pluggable ANT program, ADR-017
  off-chain attestor, ADR-018 Anchor `#[event]` ABI policy.
* [`docs/EVENTS.md`](docs/EVENTS.md) — single-page overview of the
  event surface, wire format, ABI policy, subscription patterns, "how
  to add a new event."
* [`docs/BEHAVIORAL_DIFFERENCES.md`](docs/BEHAVIORAL_DIFFERENCES.md) —
  intentional Lua → Solana differences (BD-001 …). Check before
  flagging a "bug" — it may be intentional. **BD-103 catalogs the full
  event surface.**
* [`docs/COMPUTE_AND_LIMITS.md`](docs/COMPUTE_AND_LIMITS.md) — CU
  budgets, account sizes, PDA seeds, batching, fixed-point math.
* [`docs/ACCOUNT_SCALING_PATTERNS.md`](docs/ACCOUNT_SCALING_PATTERNS.md)
  — canonical taxonomy (Pattern A/B/C); referenced by ADR-012.
* [`docs/FEATURE_MATRIX.md`](docs/FEATURE_MATRIX.md) — F1-F61 + ANT-1-6
  Lua-to-Solana mapping.
* [`docs/WORKFLOWS.md`](docs/WORKFLOWS.md) — protocol workflows by
  actor type.
* [`docs/FUNDING_MODES.md`](docs/FUNDING_MODES.md) — integrator guide
  for fund-from-stakes (balance / delegation / operator / withdrawal /
  plan).
* [`docs/ANT_ESCROW_DESIGN.md`](docs/ANT_ESCROW_DESIGN.md) +
  [`PROTOCOL_SPEC`](docs/ANT_ESCROW_PROTOCOL_SPEC.md) +
  [`CU_BASELINE`](docs/ANT_ESCROW_CU_BASELINE.md) — trustless
  multi-protocol ANT custody (ADR-014). The auditor handoff is in
  [`ESCROW_SECURITY_AUDIT_BRIEF`](docs/ESCROW_SECURITY_AUDIT_BRIEF.md);
  off-chain attestor review in
  [`ATTESTOR_SECURITY_REVIEW`](docs/ATTESTOR_SECURITY_REVIEW.md).
* [`docs/SECURITY_AUDIT_2026-04-29.md`](docs/SECURITY_AUDIT_2026-04-29.md)
  + [`SECURITY_AUDIT_INDEPENDENT.md`](docs/SECURITY_AUDIT_INDEPENDENT.md)
  — most recent internal pass and the independent-audit findings.

The IDLs at `target/idl/<program>.json` (regenerated by `anchor build`)
plus the `program-ids/` manifests are the authoritative source of truth
for instruction ABIs and on-chain addresses.

### Optional: Solana Dev Skill

The [`solana-foundation/solana-dev-skill`](https://github.com/solana-foundation/solana-dev-skill)
Claude Code skill packages current (Jan 2026) Solana ecosystem
references — security vulnerability patterns, Anchor / Pinocchio guides,
LiteSVM / Mollusk / Surfpool testing notes, version compatibility, common
errors. When installed it activates automatically on relevant prompts
(security review, toolchain issues, testing patterns, etc.). Install
user-level once:

```bash
git clone https://github.com/solana-foundation/solana-dev-skill /tmp/sds && \
  /tmp/sds/install.sh && rm -rf /tmp/sds   # installs to ~/.claude/skills/solana-dev/
```

The skill is **not vendored into this repo** — it's a per-developer
opt-in. Project-specific conventions in this CLAUDE.md still take
precedence over generic skill guidance when they conflict (e.g. our
PDA bump-doc-comment rule, our event ABI policy, our `BPF_OUT_DIR`
pitfall).

### External source we're porting from

Clone these as siblings when porting Lua semantics to Rust — every BD-NNN
entry in `BEHAVIORAL_DIFFERENCES.md` cites them by file/line:

* [`ar-io/ar-io-network-process`](https://github.com/ar-io/ar-io-network-process)
  (Lua) — `src/gar.lua`, `src/arns.lua`, `src/demand.lua`,
  `src/balances.lua`, `src/vaults.lua`.
* [`ar-io/ar-io-ant-process`](https://github.com/ar-io/ar-io-ant-process)
  — ANT process Lua source.

### Docs maintenance

* **New ADRs** use [MADR](https://adr.github.io/madr/) format, one
  file per decision in [`docs/adrs/`](docs/adrs/). Copy
  [`docs/adrs/0000-template.md`](docs/adrs/0000-template.md), pick the
  next free four-digit number from
  [`docs/adrs/README.md`](docs/adrs/README.md), and update the index
  in the same PR. Once merged to `develop`, an ADR's body is
  append-only — supersede with a new ADR rather than editing in place.
  The historical `docs/DECISIONS.md` still holds ADR-001..ADR-018; new
  entries do **not** go there.
* **New behavioral differences vs Lua** get a `BD-NNN` entry in
  [`docs/BEHAVIORAL_DIFFERENCES.md`](docs/BEHAVIORAL_DIFFERENCES.md).
* **New Anchor events:** append the type, run
  `node scripts/idl-event-snapshot.mjs --update`, document in
  [`docs/EVENTS.md`](docs/EVENTS.md), and bump the relevant section in
  `BEHAVIORAL_DIFFERENCES` (BD-103 catalogs the full surface).
* **New CPI edges:** update the diagram in [`README.md`](README.md)
  and verify the `idl-build` feature chain in the originating crate's
  `Cargo.toml` (see "Build & Test Commands" → `idl-build` notes).
* **New PDA layouts:** update
  [`docs/COMPUTE_AND_LIMITS.md`](docs/COMPUTE_AND_LIMITS.md) (seed
  pattern + size) and add the bump-doc-comment per the **PDA `bump` doc
  comments** rule in "Code Conventions" above.
* **Archiving superseded docs:** move into
  [`docs/archive/`](docs/archive/) with a banner pointing at the
  successor. Don't delete — external trackers link to specific files
  in `docs/`. Full policy in
  [`docs/archive/README.md`](docs/archive/README.md).
