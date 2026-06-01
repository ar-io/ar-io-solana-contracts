# Testing Guide

Practical guide for running, writing, and debugging tests across the AR.IO Solana programs.

## Quick Reference

```bash
# Run all program tests (requires anchor build first)
cargo test -p ario-core
cargo test -p ario-gar
cargo test -p ario-ant

# ario-arns requires Metaplex Core .so (see "ario-arns Setup" below)
cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/
BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-arns

# Run a single test
cargo test -p ario-gar test_join_network

# Run proptest suite only
cargo test -p ario-arns proptests
cargo test -p ario-gar proptests
```

## First-Time Setup

Before running any contract tests:

```bash
anchor build              # Generates target/idl/*.json AND target/deploy/*.so
```

`anchor build` must complete before `cargo test` because tests load the compiled `.so` via `solana-program-test`. If you only changed Rust source (no IDL changes), `cargo test` alone is sufficient — SPT compiles the program as a native library.

**Exception: ario-arns** — see next section.

## Git hooks (optional)

This repo ships [version-controlled hooks](https://git-scm.com/docs/githooks) under `.githooks/` (plain bash — no Husky / `package.json`). They mirror the **fast** part of `.github/workflows/build-test.yml` (`fmt-clippy` job): `cargo fmt --all -- --check` and `cargo clippy --workspace --all-targets --no-deps -- -D warnings`.

Install once per clone:

```bash
bash scripts/install-git-hooks.sh
```

That sets `git config core.hooksPath .githooks` for this repository only.

* **Emergency bypass:** `AR_IO_SKIP_PREPUSH=1 git push …`
* **Full CI parity** still needs Anchor, Solana, BPF fixtures, etc. Run `anchor build`, `cargo test`, and `node scripts/idl-event-snapshot.mjs` as in CI before merging large changes — or rely on GitHub Actions on the PR.

## ario-arns Setup (Metaplex Core)

ario-arns tests CPI into Metaplex Core's `UpdatePluginV1` for ANT trait sync. The real `mpl_core.so` binary must be in `target/deploy/`:

```bash
# Copy the committed fixture
cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/

# Set BPF_OUT_DIR so solana-program-test finds it
BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-arns
```

**If you forget this**, tests fail with `AccountNotExecutable` or silent CPI failures. The `.so` is dumped from mainnet and committed in the repo. To refresh it:

```bash
solana program dump CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d \
  programs/ario-arns/tests/fixtures/mpl_core.so \
  --url https://api.mainnet-beta.solana.com
```

## Test Architecture

All four programs use `solana-program-test` (SPT) with `#[tokio::test]` async tests in `programs/<name>/tests/integration.rs`. No Bankrun, no LiteSVM.

### Required Boilerplate

Every test file needs these three elements:

**1. Unsafe lifetime bridge** — Anchor's `entry` expects tied lifetimes; SPT provides independent ones:

```rust
fn anchor_processor(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> ProgramResult {
    unsafe {
        let accounts: &[AccountInfo] = std::mem::transmute(accounts);
        my_program::entry(program_id, accounts, data)
    }
}
```

This is safe because SPT guarantees AccountInfo references outlive the call. Copy verbatim when adding test files.

**2. Error assertion macro:**

```rust
macro_rules! assert_anchor_error {
    ($result:expr, $error:expr) => {
        let expected_code = anchor_lang::error::ERROR_CODE_OFFSET + $error as u32;
        match $result {
            Err(BanksClientError::TransactionError(
                TransactionError::InstructionError(_, InstructionError::Custom(code)),
            )) => {
                assert_eq!(code, expected_code, "Expected {}, got code {}", stringify!($error), code);
            }
            Err(e) => panic!("Expected {}, got: {:?}", stringify!($error), e),
            Ok(()) => panic!("Expected {}, but succeeded", stringify!($error)),
        }
    };
}
```

`ERROR_CODE_OFFSET` is 6000 in Anchor. Custom error enum variants are numbered starting from 0, so `MyError::Foo` (variant 3) becomes error code 6003.

**3. Compute budget:**

```rust
let mut pt = ProgramTest::new("my_program", my_program::ID, processor!(anchor_processor));
pt.set_compute_max_units(400_000); // Match the program's on-chain budget
```

| Program   | Budget      |
|-----------|-------------|
| ario-core | 400,000 CU  |
| ario-gar  | 1,000,000 CU|
| ario-arns | 1,000,000 CU|
| ario-ant  | 200,000 CU  |

### Zero-Copy Registry Pre-Allocation

`GatewayRegistry` (120KB, 3000 slots) and `NameRegistry` (2MB, 200K slots) exceed Solana's 10KB `MAX_PERMITTED_DATA_INCREASE` limit. Tests must pre-create them before `pt.start_with_context()`:

```rust
let registry_size = 8 + GatewayRegistry::SIZE;
let mut data = vec![0u8; registry_size];

// Write Anchor zero-copy discriminator
let disc = hash(b"account:GatewayRegistry");
data[..8].copy_from_slice(&disc.to_bytes()[..8]);

// Write authority at offset 8 (repr(C) layout)
data[8..40].copy_from_slice(authority.as_ref());
// count at offset 40 — leave as 0

pt.add_account(registry_key, Account {
    lamports: rent.minimum_balance(registry_size),
    data,
    owner: my_program::ID,
    executable: false,
    rent_epoch: 0,
});
```

See `program_test_with_gar()` in ario-gar and `program_test_with_registry()` in ario-arns for complete examples.

### CU Consumption Testing

Use `process_transaction_with_metadata()` to capture actual CU usage:

```rust
let result = ctx.banks_client.process_transaction_with_metadata(tx).await.unwrap();
assert!(result.result.is_ok());
let cu = result.metadata.unwrap().compute_units_consumed;
assert!(cu < 200_000, "Used {} CU, expected < 200_000", cu);
```

This catches CU regressions that would cause on-chain failures. Set thresholds conservatively below the program's budget.

### Clock Manipulation

For time-dependent logic (vaults, leases, epochs, demand factor periods):

```rust
let mut clock = ctx.banks_client.get_sysvar::<Clock>().await.unwrap();
clock.unix_timestamp += 14 * 86_400 + 1; // Warp past 14-day lock
ctx.set_sysvar(&clock);
```

**Gotcha:** All timestamps in the contracts are **seconds** (not milliseconds like the Lua source).

### Multi-Instruction Transaction Testing

To test revival attacks or multi-step atomic flows, bundle instructions in one transaction:

```rust
let tx = Transaction::new_signed_with_payer(
    &[instruction_1, instruction_2],  // Both execute atomically
    Some(&payer), &[&signer], blockhash,
);
let result = ctx.banks_client.process_transaction(tx).await;
assert!(result.is_err(), "Second instruction should fail after first closes the account");
```

### Asserting Anchor `#[event]` Emissions (BPF-only)

The shared `ario-test-utils` crate (`contracts/test-utils/`) provides log
parsing helpers for `emit!()` calls — used by every event-coverage PR.
Pattern:

```rust
#[tokio::test]
async fn buy_name_emits_purchase_event() {
    ario_test_utils::bpf_required!();      // skip if BPF_OUT_DIR unset

    // ... build & submit a buy_name tx ...
    let result = ctx.banks_client.process_transaction_with_metadata(tx).await.unwrap();
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    use ario_arns::NamePurchasedEvent;
    let ev = expect_event!(&logs, NamePurchasedEvent);
    assert_eq!(ev.buyer, payer.pubkey());
    assert_eq!(ev.cost, 100_000_000);
    assert_eq!(ev.funding_source, 0); // 0 = Balance
}
```

**Why BPF-only:** `solana-program-test` 2.1.0's `SyscallStubs` overrides
`sol_log` (so `msg!` works in native dispatch) but **not**
`sol_log_data` (the syscall behind `emit!`). Without `BPF_OUT_DIR` set,
events fall through to a default stub that `eprintln!`s to stderr but
never lands in `log_messages`. The `bpf_required!()` macro skips
cleanly so `cargo test` without BPF doesn't false-fail; CI runs the
suite with BPF set.

**To run event tests locally:**

```bash
bash build-sbf.sh                                # fresh BPF build
BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-core
```

**Available helpers** (from `ario_test_utils`):

| Helper | Purpose |
|---|---|
| `bpf_required!()` | Skip the test if BPF_OUT_DIR is unset |
| `expect_event!(logs, Ty)` | Parse first event of `Ty`; panic with logs if absent |
| `expect_event_count!(logs, Ty, n)` | Assert exactly `n` events of `Ty`; useful for batched ix |
| `assert_no_event!(logs, Ty)` | Assert no event of `Ty` (revert paths) |
| `parse_event::<T>(logs)` | Returns `Option<T>` |
| `parse_all_events::<T>(logs)` | Returns `Vec<T>` |
| `has_event::<T>(logs)` | Returns `bool` |

**ABI stability check:** events ship as a permanent ABI. Run after
`anchor build` to verify no shipped event got renamed/reordered:

```bash
node scripts/idl-event-snapshot.mjs           # check vs snapshot
node scripts/idl-event-snapshot.mjs --update  # bless new events
```

The snapshot lives at `idl-event-snapshots.json`. Per
ADR-017: shipped events are append-only — to change a shipped event's
shape, ship a `*EventV2` alongside.

**CU regression tracking:** capture per-test CU baselines and diff
across event-PR changes:

```bash
bash scripts/cu-baseline.sh                  # capture all programs
bash scripts/cu-baseline.sh --diff           # show CU deltas
```

Tracks `+5%` regression budget. Tool, not a CI gate — surface deltas
in PR descriptions so reviewers see the cost.

## Localnet Testing with Surfpool

Surfpool provides a local Solana network for integration testing with realistic state.

### Starting Localnet

```bash
# Build all 5 programs and start a Surfpool validator with them preloaded
bash scripts/start-localnet.sh

# Skip the BPF rebuild for faster iteration when source hasn't changed
SKIP_BUILD=1 bash scripts/start-localnet.sh
```

The script writes program IDs and the RPC URL to `localnet/out/localnet.env`
(gitignored). Source it from downstream tooling (the SDK, the migration
import pipeline, indexer test fixtures) to point them at the local cluster.

### Surfpool Cheatcodes for Testing

Surfpool exposes `surfnet_*` RPC methods on port 8899:

```bash
# Time travel (epoch lifecycle testing)
curl -X POST http://127.0.0.1:8899 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","id":1,"method":"surfnet_timeTravel","params":[{"absoluteSlot":250000000}]}'

# Pause/resume block production
curl ... -d '{"method":"surfnet_pauseClock","params":[]}'
curl ... -d '{"method":"surfnet_resumeClock","params":[]}'

# Set account state directly
curl ... -d '{"method":"surfnet_setAccount","params":["<pubkey>",{"lamports":1000000000,"data":"...","owner":"..."}]}'

# Profile transaction CU usage
curl ... -d '{"method":"surfnet_profileTransaction","params":["<base64_tx>"]}'

# Export full state snapshot
curl ... -d '{"method":"surfnet_exportSnapshot","params":[]}'
```

### Surfpool vs solana-program-test

| Use case | Tool |
|----------|------|
| Unit testing a single instruction | `cargo test` (SPT) |
| Testing CPI chains | `cargo test` (SPT, load real .so) |
| Testing with realistic mainnet state | Surfpool (`--network mainnet`) |
| Testing downstream SDK / indexer / CLI against deployed programs | Surfpool + `start-localnet.sh` |
| CI pipeline | `cargo test` for programs, Surfpool for integration |

## Troubleshooting

### "AccountNotExecutable" in ario-arns tests

Missing `mpl_core.so`. Run:
```bash
cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/
BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-arns
```

### "DeclaredProgramIdMismatch" (#4100)

The `declare_id!()` in source doesn't match the keypair in `target/deploy/`. Fix:
```bash
./build-sbf.sh --sync    # Auto-syncs, builds, restores source
```

### "exceeded CUs meter" or "Computational budget exceeded"

The instruction used more CU than `set_compute_max_units()`. Either:
- Increase the test's compute budget
- Optimize the instruction (check for unnecessary account loads, CPI overhead)
- Profile with `process_transaction_with_metadata()` to see actual CU consumed

### Tests pass locally but fail in CI

1. Check `anchor build` ran before `cargo test`
2. Check `mpl_core.so` is available for ario-arns
3. Check `BPF_OUT_DIR` is set correctly
4. Ensure x86_64 Linux (ARM cannot run BPF tests)

### Proptest failures

Proptest generates random inputs to find invariant violations. When it fails:
1. The output shows the **minimal failing case** (proptest shrinks automatically)
2. Fix the invariant violation in the source code
3. Re-run — proptest uses a seed file (`.proptest-regressions`) to replay known failures

### "lock file version 4 requires -Znext-lockfile-bump"

Your Rust toolchain is too old. The workspace requires Rust 1.79+ (the `cargo-build-sbf` bundled version). Use the correct toolchain or update.

## Adding New Tests

### New test case in an existing program

1. Add to the bottom of `programs/<name>/tests/integration.rs`
2. Reuse existing helpers (`create_mint`, `create_token_account`, `mint_tokens`, etc.)
3. Use `assert_anchor_error!` for error path tests
4. Use `process_transaction_with_metadata()` if you need CU assertions
5. Follow the naming convention: `test_<feature>_<scenario>`

### New program test file

If splitting tests into multiple files:

1. Copy the boilerplate (anchor_processor, assert_anchor_error macro, helpers)
2. Each file gets its own `ProgramTest` instances — they don't share state
3. For ario-arns: ensure `program_test_with_registry()` loads `mpl_core.so`

### New proptest property

Add inside the existing `mod proptests` block in the source file:

```rust
proptest! {
    #[test]
    fn my_new_property(
        input in 0u64..=u64::MAX,
    ) {
        let result = my_function(input);
        prop_assert!(result.is_ok(), "must never panic for input {}", input);
    }
}
```

Proptest runs 256 cases by default. Increase with `#![proptest_config(ProptestConfig::with_cases(1000))]`.

## Migration & Import Validation

State migration tooling (snapshot exporter, AO → Solana import orchestrator,
escrow / claim flows) lives in the
[`solana-ar-io`](https://github.com/ar-io/solana-ar-io) repository under
`migration/`. This repo is contracts-only — point migration tooling at a
local Surfpool (via `bash scripts/start-localnet.sh`) or at the published
devnet program IDs (see `program-ids/staging.json`) when you need an
end-to-end run.
