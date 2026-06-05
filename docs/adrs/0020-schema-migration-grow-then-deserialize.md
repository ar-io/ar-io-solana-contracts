# ADR-020: Schema-Migration Loading — Grow-Then-Deserialize, Append-Only Versioning

- Status: accepted
- Date: 2026-05-27
- Deciders: protocol engineering

## Context

PR #53 added `version: SchemaVersion` (3 bytes) to the record PDAs across
ario-core / ario-gar / ario-arns / ario-ant, and PR #56/#62 wired per-account
`migrate_*` instructions plus `migrate_*_version` bootstrap arms (`{0,0,0}`
→ `1.0.0`).

A security review found the migration mechanism could not load the very
accounts it was meant to migrate. Two compounding defects:

1. **Deserialize-before-realloc (the core bomb).** Anchor builds
   `Account<'info, T>` by borsh-deserializing the **new** layout *before* the
   `realloc` constraint runs. An account created under the older (shorter)
   layout is `SCHEMA_VERSION_SIZE` bytes short, so `T::try_deserialize` hits
   Borsh **EOF → `AccountDidNotDeserialize` (3003)** before the account can be
   grown. This freezes the account for **every** typed-`Account<T>` load — not
   just `migrate_*` — and the bootstrap arm is unreachable. Proven with a
   `migrate_balance` PoC (error `0xbbb`).

2. **Non-append-only field placement.** `ArnsRecord` inserted `version`
   *before* the variable-length `name`, so an old record's `name` length-prefix
   was read as the version and the string length was corrupted (worse than
   EOF — undeserializable). `EscrowAnt`/`EscrowToken` historically had a 1-byte
   `version` at the front; the u8→`SchemaVersion` change shifted every field
   and corrupted the cached `bump`.

Deployment reality at decision time: mainnet undeployed; devnet deployed
before the versioning commit (pre-`286b965`) and therefore already
un-upgradeable in place → fresh-redeployed regardless. So **no value-bearing
cluster locks any layout**, and the real risk being closed is the **first
post-mainnet-launch schema bump**.

## Decision

1. **`migrate_*` must NOT load the target as a typed `Account<T>`.** It loads
   it as `UncheckedAccount`, calls `schema_migration::grow_account` (rent
   top-up + `realloc` to the canonical SIZE, growing pre-version accounts and
   trimming over-allocated ones, zero-filling any appended tail) **first**,
   then borsh-deserializes, runs the version arm, and writes back via
   `write_account`.

2. **`write_account` must serialize through a temp buffer + index copy**, never
   `try_serialize(&mut *account.try_borrow_mut_data()?)`. `Write for &mut [u8]`
   advances the slice reference, and because that reference *is* the account's
   data ref, a direct write **truncates the account to 0 bytes**. (Anchor's own
   `exit` uses a non-advancing `BpfWriter` for the same reason.)

3. **Versioned accounts are append-only.** `version` (and every future field)
   lives at the byte-**end**, after any variable-length field. New fields are
   appended at the end (or carved from a `_reserved` tail). Reordering an
   existing field is forbidden post-launch. `ArnsRecord.version` was reordered
   to after `name` as a one-time pre-launch correction.

4. **PDA validation** stays declarative (`seeds + bump` on the
   `UncheckedAccount`) where the seed uses passed-in accounts/args; where the
   seed derives from stored data (e.g. `PrimaryNameReverse.name`,
   `ArnsRecord.name_hash`), the handler re-derives and matches the PDA after
   deserialize (`realloc` already enforced program ownership; `try_deserialize`
   checked the discriminator).

### Exception: reserved-padding accounts (ario-ant-escrow)

`EscrowAnt`/`EscrowToken` carry a fixed `_reserved` tail and a **fixed total
SIZE**; additive fields carve from `_reserved` so the account is *never*
short and never reallocs. With a fixed-size `version` field this layout is
both EOF-free and shift-free, so escrow's `migrate_*` correctly keep typed
`Account<T>` (proven by `migration_e2e`, 4 simulated versions). **Constraint:**
if a future escrow field ever exceeds the remaining `_reserved`, that change
MUST convert escrow's `migrate_*` to the grow-then-deserialize pattern above.

### Zero-copy registries

`GatewayRegistry` / `Epoch` (`AccountLoader`, `version_bytes: [u8; 3]`) are
created once at full size (not `init`) and recreated fresh per deploy, so they
have no in-place size-migration path and are out of scope for the EOF defect.

## Consequences

- The first post-launch schema bump of any borsh `#[account]` works: a
  permissionless `migrate_*` crank must sweep all accounts after the upgrade;
  un-migrated accounts fail to load until swept (clean failure, no data loss).
- Per-account regression tests build **genuine** pre-version accounts (old
  SIZE, no version bytes) and assert migrate grows + stamps + preserves — these
  are the standing guard against reintroduction. Tests that build full-size
  accounts with `version={0,0,0}` are insufficient (they skip the grow path).
- Operational: devnet is fresh-redeployed for this change; mainnet launches on
  the append-only layout. Off-chain consumers regenerate for the
  `ArnsRecord` reorder (memcmp offsets for `owner`/`ant` are unchanged).
