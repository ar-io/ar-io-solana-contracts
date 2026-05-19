# ANT Program Schema Version Bump

How to bump the on-chain schema version for `ario-ant` account types
(`AntConfig`, `AntControllers`, `AntRecord`, `AntRecordMetadata`).

## When to Use

Use this workflow when:
- Adding a new field to any ANT account struct
- Changing the layout of an existing field
- Any on-chain change that requires existing PDAs to be migrated

## Semver Rules

Versions use `SchemaVersion { major, minor, patch }` (3 bytes on-wire).

| Bump    | When                                              |
|---------|---------------------------------------------------|
| `major` | Breaking layout change (field removed/reordered/type changed) |
| `minor` | Additive layout change (new field appended, default = zero)   |
| `patch` | Logic-only change (no layout change; optional for audits)     |

## Step-by-Step Checklist

### 1. Bump the version constant in `state.rs`

File: `programs/ario-ant/src/state.rs`

Update the relevant constant:

```rust
pub const ANT_CONFIG_VERSION: SchemaVersion = SchemaVersion::new(1, 1, 0); // was 1.0.0
```

### 2. Add the new field to the account struct

Append the field **after** `version` (or after the last added field).
Update `SIZE` to account for the new bytes.

```rust
pub struct AntConfig {
    // ... existing fields ...
    pub version: SchemaVersion,
    pub new_field: u64,  // <-- appended
}

impl AntConfig {
    pub const SIZE: usize = /* previous */ + 8; // u64
}
```

### 3. Add a migration arm in `schema_migration.rs`

File: `programs/ario-ant/src/schema_migration.rs`

Add a new match arm in the relevant `migrate_*_version` function that
populates the new field with a sensible default and advances the version:

```rust
pub fn migrate_config_version(config: &mut AntConfig) -> Result<()> {
    while config.version < ANT_CONFIG_VERSION {
        match config.version {
            SchemaVersion { major: 1, minor: 0, patch: 0 } => {
                config.new_field = 0; // default
                config.version = SchemaVersion::new(1, 1, 0);
            }
            _ => return err!(AntError::UnknownSchemaVersion),
        }
    }
    Ok(())
}
```

Each arm MUST advance `config.version` so the loop terminates.

### 4. Update `migration.rs` borsh validators (if applicable)

If the bump affects accounts that go through import/migration validation
(`validate_ant_*_borsh_payload`), update the manual parser to read the
correct number of bytes for the new layout.

### 5. Update the SIZE assertion test

In `state.rs` tests, update the sanity-check constant:

```rust
assert_eq!(AntConfig::SIZE, 768); // previous was 760
```

### 6. Run checks

```bash
# Production build (no feature flags)
cargo check -p ario-ant

# Unit tests
cargo test -p ario-ant --test integration

# Migration E2E tests (if updating the migration-test feature)
cargo test -p ario-ant --features migration-test --test migration_e2e
```

### 7. Update the `migration-test` feature (optional)

If you want E2E coverage for the new schema step, update:

1. `state.rs` — add a `#[cfg(feature = "migration-test")]` field and
   bump `ANT_CONFIG_VERSION` under that feature to the next version.
2. `schema_migration.rs` — add the corresponding
   `#[cfg(feature = "migration-test")]` arm.
3. `tests/migration_e2e.rs` — add/update test cases and layout structs.

### 8. Update the IDL event snapshot

```bash
anchor build
node scripts/idl-event-snapshot.mjs        # check
node scripts/idl-event-snapshot.mjs --update  # bless if intentional
```

## Key Files

| File | Role |
|------|------|
| `programs/ario-ant/src/state.rs` | Account structs, SIZE, version constants |
| `programs/ario-ant/src/schema_migration.rs` | Migration dispatch (while-loop + match) |
| `programs/ario-ant/src/lib.rs` | `migrate_ant` / `migrate_ant_record` / `migrate_ant_record_metadata` instructions |
| `programs/ario-ant/src/migration.rs` | Import/borsh validators (manual parsing) |
| `programs/ario-ant/src/error.rs` | `UnknownSchemaVersion`, `AlreadyLatestVersion` |
| `programs/ario-ant/tests/migration_e2e.rs` | Feature-gated multi-step migration tests |

## Common Pitfalls

- **Field ordering matters.** New fields must be appended after `version`
  (or after the last appended field). Inserting in the middle breaks
  existing borsh-serialized accounts.
- **SIZE must match reality.** If SIZE is too small Anchor's `realloc`
  truncates data; too large wastes rent.
- **`SchemaVersion` comparisons are lexicographic** (`major` first, then
  `minor`, then `patch`). The `Ord` derive handles this correctly.
- **Don't forget `AntControllers` / `AntRecord` / `AntRecordMetadata`**
  if your change spans multiple account types — each has its own version
  constant and migration function.
- **The `migration-test` feature must never leak into production.**
  All test sentinel fields and bumped constants are gated behind
  `#[cfg(feature = "migration-test")]`.
