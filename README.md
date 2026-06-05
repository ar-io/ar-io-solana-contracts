# ar-io-solana-contracts

The Solana programs that power the [AR.IO Network](https://ar.io). Five
Anchor programs in one Cargo workspace so that integration tests, CPI
graphs, and release tooling can treat them as a single deployable unit.

| Program             | Crate               | What it does |
|---------------------|---------------------|--------------|
| `ario-core`         | `programs/ario-core` | ARIO SPL token (mint/transfer), time-locked vaults, primary-name resolution |
| `ario-gar`          | `programs/ario-gar`  | Gateway Address Registry — operator + delegate staking, withdrawals, epoch lifecycle, observer selection, batched reward distribution |
| `ario-arns`         | `programs/ario-arns` | ArNS name registry — buy / lease / permabuy, demand-factor pricing, reserved + returned names |
| `ario-ant`          | `programs/ario-ant`  | Arweave Name Token (ANT) as a Metaplex Core NFT with Attributes plugin, undername records, controller ACL |
| `ario-ant-escrow`   | `programs/ario-ant-escrow` | Multi-protocol trustless escrow for ANTs, ARIO tokens, and time-locked vaults — Arweave (Ed25519 attestor), Ethereum (`secp256k1_recover`), and instruction-introspection vault claims |

CPI graph (`A → B` means A invokes B):

```
ario-arns ──cpi──▶ ario-gar ──cpi──▶ ario-core (SPL token via anchor-spl)
   │                  │
   │                  └─cpi (fund-from-stake)─▶ ario-core
   │
   └─cpi (UpdatePluginV1)─▶ Metaplex Core    (ANT trait sync)

ario-core ──cpi──▶ ario-gar     (primary-name fund-from-stake variants)

ario-ant  ──reads──▶ ario-arns  (ArnsRecord PDA — Attributes plugin sync)

ario-ant-escrow ──introspects──▶ ario-core::vaulted_transfer  (vault claims)
                ──cpi──▶ Metaplex Core            (transfer ANT to claimant)
                ──cpi──▶ Ed25519 / Secp256k1 native programs (sigverify)
```

Solana max CPI depth is 4. The deepest path here (user → `ario-arns` →
`ario-gar` → SPL Token = 3 hops) stays one hop under the limit; ADRs
in [`docs/DECISIONS.md`](docs/DECISIONS.md) record why each cross-program
edge exists.

---

## Status

* **Devnet**: deployed by CI on every merge to `develop`. Program IDs
  pinned in [`program-ids/staging.json`](program-ids/staging.json).
* **Mainnet**: upgrades staged by CI on every merge to `main`, executed
  by the AR.IO Squads multisig. Program IDs pinned in
  [`program-ids/mainnet.json`](program-ids/mainnet.json).

---

## Repository layout

```
.
├── Anchor.toml                  # workspace anchor config (program IDs per cluster)
├── Cargo.toml                   # cargo workspace + pinned solana-* deps
├── build-sbf.sh                 # BPF build wrapper with declare_id drift guard
├── idl-event-snapshots.json     # frozen Anchor #[event] ABI (ADR-017)
├── localnet/
│   └── surfpool-svm-features.sh # canonical Surfpool feature gate list
├── program-ids/                 # per-cluster program ID manifests (committed)
│   ├── staging.json
│   └── mainnet.json
├── programs/
│   ├── ario-core/
│   ├── ario-gar/
│   ├── ario-arns/
│   ├── ario-ant/
│   └── ario-ant-escrow/         # also has fuzz/ targets
├── test-utils/                  # shared event-log parser used by every program's tests
├── scripts/
│   ├── check-attestor-pubkey.sh
│   ├── cu-baseline.sh
│   ├── devnet-deploy.sh
│   ├── fund-devnet.sh
│   ├── idl-event-snapshot.mjs
│   ├── mainnet-prepare-upgrade.sh
│   ├── package-release.sh
│   └── start-localnet.sh        # standalone Surfpool harness
├── docs/                        # design docs / ADRs / behavioral diff
└── .github/
    ├── CODEOWNERS
    ├── PULL_REQUEST_TEMPLATE.md
    └── workflows/
        ├── build-test.yml
        ├── codeql.yml
        ├── docker-builder.yml
        ├── release.yml
        ├── release-clients-ts.yml
        ├── release-plz.yml
        ├── upgrade-devnet.yml
        └── upgrade-mainnet.yml
```

---

## Toolchain

| Tool                | Version    | Notes |
|---------------------|------------|-------|
| Rust (host)         | `1.90.0`   | fmt, clippy, avm, `cargo test`. Pinned in `rust-toolchain.toml`; auto-installed by `rustup`. |
| Rust (BPF)          | `1.79.0`   | bundled inside `cargo-build-sbf`; Cargo.lock must stay parseable by its Cargo 1.79 (see note below). |
| Solana (Agave) CLI  | `2.1.0`    | newer 2.x releases drop Cargo 1.79 manifest support |
| Anchor              | `0.31.1`   | `avm install 0.31.1 && avm use 0.31.1` |
| [Surfpool](https://github.com/solana-foundation/surfpool) | `1.2.x` | local validator with mainnet-style SVM gates. **Use `1.2.x` specifically** — `1.1.x` lacks `--skip-blockhash-check` (added in 1.2.0, used by `scripts/start-localnet.sh`); `1.3.x` changed `--rpc-port` → `--port` (script not yet adapted). |
| `cargo-fuzz` *(optional)* | latest | for the escrow signature-verifier fuzz targets |

`Cargo.toml` pins several workspace deps (`solana-*=2.1.0`,
`blake3=1.5.5`, `proc-macro-crate=3.2.0`, `indexmap=2.11.0`,
`unicode-segmentation=1.12.0`) so `cargo-build-sbf`'s bundled Cargo 1.79
can resolve the dep graph. **Transitive deps that use `edition = "2024"`
in their manifest also break Cargo 1.79** — `Cargo.lock` pins
`time-macros` and `time` to pre-edition2024 versions for this reason.
Don't bump these without verifying the BPF build still works.

---

## First-time setup

```bash
git clone https://github.com/ar-io/ar-io-solana-contracts.git
cd ar-io-solana-contracts

# 1. Install the toolchain (one-time — see the table above).
sh -c "$(curl -sSfL https://release.anza.xyz/v2.1.0/install)"
cargo install --git https://github.com/coral-xyz/anchor --tag v0.31.1 avm --locked
avm install 0.31.1 && avm use 0.31.1

# 2. Build everything (this also generates target/idl/*.json).
anchor build

# 3. Run the unit + integration test suite.
cargo test --workspace
```

For the event-coverage tests you'll need a BPF build first:

```bash
bash build-sbf.sh
BPF_OUT_DIR="$(pwd)/target/deploy" cargo test --workspace
```

`ario-arns` (and the `ario-ant-escrow` integration tests) CPI into
Metaplex Core's `UpdatePluginV1`. The required `mpl_core.so` fixture is
checked into `programs/ario-arns/tests/fixtures/`. Stage it once:

```bash
cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/
BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-arns
```

See [`TESTING.md`](TESTING.md) for the full testing guide
(boilerplate, zero-copy registries, CU assertions, Surfpool cheatcodes,
how to add a new test).

Optional — run the same fast checks CI runs before push (rustfmt +
clippy, no Anchor install required):

```bash
bash scripts/install-git-hooks.sh
```

---

## Development guide

### Branching model (two-stage release)

```
feature branch ─PR─▶ develop ─PR─▶ main
                       │              │
                       │              └─▶ upgrade-mainnet workflow
                       │                  → buffer staging, multisig hand-off
                       │
                       └─▶ upgrade-devnet workflow
                           → live devnet upgrade + tarball release
```

* Open PRs against `develop`. CI runs `build-test.yml` (lint, build,
  full test suite, IDL ABI stability check, escrow fuzz smoke) before
  the PR is mergeable.
* Merging to `develop` triggers `upgrade-devnet.yml`: full build, deploy
  to devnet, refresh `program-ids/staging.json` (auto-committed back to
  `develop`), and publish a versioned release tarball with IDLs + .so +
  keypairs so downstream clients can update or run their own Surfpool.
* Cutting a mainnet release happens by opening a `develop → main` PR.
  Required reviewers / branch protection on `main` are the human gate.
  Merging triggers `upgrade-mainnet.yml`: build with mainnet feature
  flags, stage upgrade buffers, transfer buffer authority to the
  Squads V3 multisig vault (authority index 1), attach a buffer manifest
  to a draft GitHub release. The multisig signers vote and execute the
  upgrade separately from the legacy Squads (V3) app (CI never holds the
  upgrade key).

### Day-to-day commands

```bash
# Fast iteration loop on a single program (host target — no BPF build)
cargo check -p ario-arns
cargo test  -p ario-arns

# Full BPF build (idempotent; refuses to build if declare_id! drifted)
bash build-sbf.sh

# Localnet end-to-end with all five programs preloaded into Surfpool
bash scripts/start-localnet.sh
# → writes localnet/out/localnet.env (RPC URL + program IDs)

# Targeted dev (skip BPF rebuild when source is unchanged)
SKIP_BUILD=1 bash scripts/start-localnet.sh

# IDL event ABI stability (run after anchor build)
node scripts/idl-event-snapshot.mjs            # check
node scripts/idl-event-snapshot.mjs --update   # bless intentional additions

# CU regression tracking on event-emission PRs
bash scripts/cu-baseline.sh           # capture baseline
bash scripts/cu-baseline.sh --diff    # show deltas
```

### Common gotchas

* **`DeclaredProgramIdMismatch (#4100)`** at runtime — `declare_id!()`
  doesn't match the keypair under `target/deploy/`. Fix:
  `bash build-sbf.sh --sync` rewrites source, builds, then restores it
  on EXIT (canonical localnet flow). For devnet/mainnet, the
  `release-*` workflows commit the synced source back to the branch.
* **`InstructionFallbackNotFound (101)`** for a brand-new instruction in
  `ario-core` / `ario-gar` / `ario-ant` tests — `BPF_OUT_DIR` is set and
  `solana-program-test` is loading a stale `target/deploy/<crate>.so`.
  Either rebuild (`anchor build`) or `unset BPF_OUT_DIR` to fall back to
  the just-compiled native processor.
* **`AccountNotExecutable`** in `ario-arns` tests — `mpl_core.so` is
  missing from `target/deploy/`. See "First-time setup" above.
* **`idl-build` errors mentioning `anchor_spl::token::TokenAccount`** —
  Anchor 0.31's feature unifier doesn't auto-activate `idl-build`
  through `cpi` edges. Make sure the new edge propagates `idl-build` in
  the parent crate's feature spec; the existing chain is
  `ario-ant/idl-build → ario-arns/idl-build → ario-gar/idl-build →
  anchor-spl/idl-build`.

The full troubleshooting tree is in [`TESTING.md`](TESTING.md).

---

## Release flow

### Devnet — automated, on every merge to `develop`

CI is **upgrade-only**: it can push new bytecode to programs that already
exist on devnet, but it cannot mint new program IDs. The only secret CI
holds is the upgrade authority keypair. Program keypairs live in the
original deployer's offline custody and are only needed for first
deploys (a manual operator action — see below).

`upgrade-devnet.yml` does the following:

1. Reuses the `build-test.yml` job (full lint + build + test).
2. Passes `secrets.DEVNET_AUTHORITY_KEY_JSON` straight into the deploy
   step's environment as `AUTHORITY_KEY_JSON`. The keypair is **never
   written to disk** — `scripts/devnet-deploy.sh` feeds it to solana via
   bash process substitution
   (`solana --keypair <(printf %s "$AUTHORITY_KEY_JSON") …`), so the
   bytes only ever live in process memory and an anonymous pipe. The
   workflow will not produce a keypair file in `target/deploy/`,
   `~/.config/solana/`, or anywhere else on the runner.
3. Runs `scripts/devnet-deploy.sh`:
   1. Reads `program-ids/staging.json` for the live program IDs.
   2. Calls `bash build-sbf.sh --sync-from-manifest`, which patches each
      program's `declare_id!()` in source from the manifest, builds the
      `.so` files, then restores source on EXIT. The `.so`s in
      `target/deploy/` carry the correct on-chain IDs; the source tree
      is unchanged.
   3. For each program with a non-null entry, runs raw
      `solana program deploy --program-id <PUBKEY> …` against
      `vars.DEVNET_RPC_URL`, signed by the in-memory authority key.
      Programs with `null` entries (today `ario_ant_escrow`) are
      skipped — first deploys never happen in CI.
   4. Updates `program-ids/staging.json` with the new `deployer` and
      per-program `deployed_at` timestamps. `.programs` is **never**
      overwritten by CI.
4. Auto-commits & pushes `program-ids/staging.json` if it changed.
5. Runs `scripts/package-release.sh` and uploads
   `release/ar-io-solana-contracts-devnet-<ts>-<sha>.tar.gz` as both a
   workflow artifact and a GitHub pre-release.

The bundle includes:

* `idl/*.json` — Anchor IDLs (one per program with a non-null manifest
  entry)
* `so/*.so` — compiled BPF binaries (same set)
* `program-ids.json` — same content as the in-repo
  `program-ids/staging.json`
* `VERSION` — version, cluster, git SHA, build timestamp, toolchain
  versions
* `SHA256SUMS` — checksums

The bundle deliberately does **not** include program keypairs (the
in-repo `<prog>-keypair.json` pattern is `.gitignore`d, and
`scripts/package-release.sh` actively refuses to bundle them via the
removed `INCLUDE_KEYPAIRS` knob). Anyone with a program keypair could
deploy a malicious `.so` at the canonical program ID on a fresh cluster
(Surfpool, a private fork, even an attacker-run devnet) and trick
downstream tooling that connects to that cluster.

Required GitHub repo configuration (one-time):

* Environment **`devnet`** with one secret:
  * `DEVNET_AUTHORITY_KEY_JSON` — JSON keypair (Solana CLI format) that
    holds upgrade authority on every deployed devnet program. CI uses
    it to sign upgrades. **Recovery from compromise is a race**: rotate
    via `solana program set-upgrade-authority` from a machine that
    still has the current key BEFORE updating this secret. If the
    attacker rotates first, the only recourse is to redeploy at fresh
    program IDs and update every downstream consumer.
* Variable `DEVNET_RPC_URL` (default `https://api.devnet.solana.com`).

#### First deploys (manual operator action)

CI cannot do the very first deploy of a program — that requires the
program keypair, which CI does not have. When a new program needs to
land on devnet (today: only `ario_ant_escrow`):

1. On a maintainer laptop, generate or load the program keypair into
   `target/deploy/<prog>-keypair.json`.
2. Place a copy of the devnet upgrade-authority keypair (the same one
   stored in `secrets.DEVNET_AUTHORITY_KEY_JSON`) at
   `target/deploy/devnet-authority-keypair.json`. Locally this is a
   file because you're hand-running individual `solana` commands; CI
   keeps it in env only.
3. Sync `declare_id!()` in source for that program (e.g.
   `anchor keys sync --program-name ario_ant_escrow`), build, and
   deploy with the program keypair as signer:
   ```bash
   anchor build
   solana program deploy \
     --program-id target/deploy/ario_ant_escrow-keypair.json \
     --keypair target/deploy/devnet-authority-keypair.json \
     --url https://api.devnet.solana.com \
     target/deploy/ario_ant_escrow.so
   ```
4. Transfer the upgrade authority to the same key CI uses (so
   subsequent upgrades flow through CI):
   ```bash
   solana program set-upgrade-authority <new_program_id> \
     --new-upgrade-authority <DEVNET_AUTHORITY_PUBKEY>
   ```
5. Add the resulting program ID to `program-ids/staging.json` under
   `.programs.<name>` and commit. CI will pick it up automatically on
   the next merge to `develop`.

### Mainnet — gated, on every merge to `main`

`upgrade-mainnet.yml`:

1. Reuses `build-test.yml`.
2. Builds with `BUILD_NETWORK=mainnet` (selects the correct
   compile-time canonical-message string in `ario-ant-escrow`).
3. Verifies `declare_id!()` matches `program-ids/mainnet.json` exactly
   (drift here would brick the upgrade).
4. Runs `scripts/mainnet-prepare-upgrade.sh`:
   * Verifies the V3 vault (`SQUADS_V3_VAULT`, falling back to the legacy
     `SQUADS_MULTISIG_PUBKEY` var) is System-owned — rejecting a multisig
     *config* account or a V4 multisig.
   * `solana program extend`s any program whose new `.so` exceeds its
     on-chain ProgramData capacity (permissionless; the Execute would
     otherwise fail on size).
   * Generates a fresh buffer keypair per program.
   * `solana program write-buffer` uploads the new `.so` to that buffer.
   * `solana program set-buffer-authority` transfers control of the
     buffer to the **Squads V3 vault** (authority index 1).
   * Emits `release/upgrade-<sha>/buffer-manifest.json` listing
     `{program, buffer, buffer_sha256, so_size_bytes}`.
5. Publishes a draft GitHub release with the bundle + manifest attached.

The Squads V3 multisig signers then (from the **legacy** Squads app, not
app.squads.so):

1. Independently fetch each buffer (`solana program dump <buffer>
   /tmp/<prog>.so`) and verify `shasum -a 256` against
   `buffer_sha256` from the manifest.
2. In Developers → Programs, "Add upgrade" with the buffer + spill, then
   "Verify authority" (the buffer authority is already the vault).
3. Approve to threshold and execute. The new bytecode is now live.

Required GitHub repo configuration:

* Environment **`mainnet`** with **required reviewers** (≥2 core
  engineers) and a wait timer of at least 30 minutes. Secrets:
  * `MAINNET_BUFFER_AUTHORITY_KEYPAIR` — transient hot wallet that pays
    for buffer rent. Lamports are reclaimed via `solana program close
    <buffer>` after the upgrade lands.
* Variables:
  * `MAINNET_RPC_URL` (default `https://api.mainnet-beta.solana.com`).
  * `SQUADS_MULTISIG_PUBKEY` — **the Squads V3 vault PDA (authority index
    1)** that holds upgrade authority and receives buffer authority. NOT the
    multisig config account. (The script reads this as `SQUADS_V3_VAULT` and
    verifies it is System-owned, so a wrong value is caught before staging.)
    Optionally also set `SQUADS_V3_MULTISIG` (the config account) for a
    SMPL-ownership cross-check.

> **Why this split.** CI runs without an upgrade key. Buffer staging
> is reversible (the multisig can `program close` the buffers if
> something is wrong) but the upgrade itself isn't, so the multisig
> approval is what gates the live state change.

### Local snapshot for downstream clients

Downstream clients that prefer not to depend on devnet's availability
can integrate against a local Surfpool with the published bundle:

```bash
# 1. Pull the latest devnet release tarball (e.g. via gh release download).
gh release download --repo ar-io/ar-io-solana-contracts --pattern '*.tar.gz'
tar xzf ar-io-solana-contracts-devnet-*.tar.gz

# 2. Boot Surfpool with the .so + keypairs from the bundle.
cd ar-io-solana-contracts-devnet-*/
surfpool start --no-tui \
  $(for p in ario_core ario_gar ario_arns ario_ant ario_ant_escrow; do
      echo --bpf-program "$(jq -r .programs.$p program-ids.json)" so/$p.so
    done) \
  --bpf-program CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d \
                so/mpl_core.so   # if your bundle includes it
```

This gives a deterministic, internet-free dev environment that exactly
matches the devnet deploy. (We may publish prebaked Surfpool snapshots
later — see [`docs/DECISIONS.md`](docs/DECISIONS.md) — but the bundle
above is sufficient for now and avoids putting Solana RPC / cloning
in CI.)

---

## Documentation

| File | What's in it |
|------|--------------|
| [`TESTING.md`](TESTING.md) | Test architecture, Surfpool cheatcodes, troubleshooting, how to add a test |
| [`docs/adrs/`](docs/adrs/) | Architecture Decision Records — MADR format, one file per decision (workflow + index in [`adrs/README.md`](docs/adrs/README.md)) |
| [`docs/DECISIONS.md`](docs/DECISIONS.md) | Historical ADR-001 through ADR-018 — superseded by `docs/adrs/` for new decisions |
| [`docs/archive/`](docs/archive/) | Superseded docs, kept for permalink stability — see policy in [`archive/README.md`](docs/archive/README.md) |
| [`docs/EVENTS.md`](docs/EVENTS.md) | Anchor `#[event]` surface, ABI policy, subscription patterns |
| [`docs/COMPUTE_AND_LIMITS.md`](docs/COMPUTE_AND_LIMITS.md) | CU budgets, account sizes, PDA seed patterns, fixed-point math |
| [`docs/ACCOUNT_SCALING_PATTERNS.md`](docs/ACCOUNT_SCALING_PATTERNS.md) | Patterns A/B/C for scaling Anchor accounts |
| [`docs/BEHAVIORAL_DIFFERENCES.md`](docs/BEHAVIORAL_DIFFERENCES.md) | Intentional Lua → Solana semantic deltas (BD-001 …) |
| [`docs/FEATURE_MATRIX.md`](docs/FEATURE_MATRIX.md) | Lua F-1 .. F-61 / ANT-1..6 feature mapping |
| [`docs/WORKFLOWS.md`](docs/WORKFLOWS.md) | Protocol workflows by actor type |
| [`docs/FUNDING_MODES.md`](docs/FUNDING_MODES.md) | Fund-from-stakes integration guide |
| [`docs/ANT_ESCROW_DESIGN.md`](docs/ANT_ESCROW_DESIGN.md) + [`PROTOCOL_SPEC`](docs/ANT_ESCROW_PROTOCOL_SPEC.md) | Trustless multi-protocol escrow |
| [`docs/ATTESTOR_SECURITY_REVIEW.md`](docs/ATTESTOR_SECURITY_REVIEW.md) | Off-chain Arweave attestor service design |
| [`docs/SECURITY_AUDIT_2026-04-29.md`](docs/SECURITY_AUDIT_2026-04-29.md) + [`INDEPENDENT`](docs/SECURITY_AUDIT_INDEPENDENT.md) | Latest security audit passes |

---

## Related repositories

| Repo | What it contains |
|------|------------------|
| [`ar-io/solana-ar-io`](https://github.com/ar-io/solana-ar-io) | AO → Solana migration tooling: snapshot exporter, import orchestrator, claim/escrow web apps, localnet harness, downstream node forks |
| [`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor) | Off-chain attestor service (extracted from `solana-ar-io/migration/attestor/`). Verifies Arweave RSA-PSS-4096 sigs and re-signs the canonical claim message with Ed25519 for the on-chain `ario-ant-escrow` program — ADR-017 |
| [`ar-io/ar-io-sdk`](https://github.com/ar-io/ar-io-sdk) | TypeScript SDK with dual AO + Solana backends; consumes the IDLs published from this repo |
| [`ar-io/ar-io-cranker`](https://github.com/ar-io/ar-io-cranker) | Standalone epoch cranker (also embedded in ar-io-observer) |

---

## Contributing

1. Read [`docs/DECISIONS.md`](docs/DECISIONS.md) and the relevant ADR
   before making non-trivial protocol changes — there's usually history.
2. Open a PR against `develop`. The PR template walks through the test /
   IDL ABI / behavioral-diff checklist.
3. CI must be green before merge. The escrow fuzz smoke job is
   intentionally short (~30s/target) — when changing
   `programs/ario-ant-escrow/src/verify/`, please run
   `cargo +nightly fuzz run <target> -- -max_total_time=600` locally.
4. Behavioral changes vs the AO Lua source go in
   [`docs/BEHAVIORAL_DIFFERENCES.md`](docs/BEHAVIORAL_DIFFERENCES.md)
   under a new BD-NNN entry.
5. New `#[event]` types are append-only per
   [ADR-017](docs/DECISIONS.md). Run
   `node scripts/idl-event-snapshot.mjs --update` and commit the
   updated `idl-event-snapshots.json` in the same PR.

---

## License

AGPL-3.0-or-later. See [`LICENSE`](LICENSE).
