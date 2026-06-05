# Primary-name `AntConfig` account — downstream rollout plan

Companion to **PR #91** (`fix/ario-core-primary-name-antconfig-owner`), which
reworks ario-core primary-name authorization to read the ANT owner from
`ario_ant`'s `AntConfig.last_known_owner` / `AntRecord` (freshness-gated)
instead of the stale per-record `last_reconciled_owner`. See BD-097 (mechanism)
and BD-109 (canonical-`ario_ant`-only / BYO-ANT limitation).

This is an **intentionally ABI-breaking** change: the four primary-name
instructions now require an additional `AntConfig` PDA as a trailing
`remaining_account`. A new contract paired with an un-updated client fails with
`UndernameRecordOwnerRequired` (or `InvalidParameter` for the funding-plan
variant). The rollout below sequences the SDK release ahead of any deploy.

## ABI delta

| instruction | remaining-accounts layout (new) |
|---|---|
| `request_primary_name` | `[arns, demandFactor]` — unchanged |
| `request_and_set_primary_name` | `[arns, demandFactor, antRecord, antConfig]` |
| `approve_primary_name` | `[arns, antRecord, antConfig]` |
| `remove_primary_name_for_base_name` | `[arns, antRecord(@), antConfig]` |
| `request_and_set_primary_name_from_funding_plan` | validation `[arns, demandFactor, antRecord, antConfig]`, `validation_account_count` 3→4 |

`AntConfig` PDA: seeds `["ant_config", ant_mint]`, owner `ario_ant::ID`
(or the asset's resolved `ANT Program`). Read-only.

The **IDL is unchanged** (`AntConfig` is a `remaining_account`, not a typed
account), so:
- `clients/ts` (`@ar.io/solana-contracts`) Codama codegen is byte-identical — no
  regen, no version bump required for the typed client.
- `idl-event-snapshots.json` / the IDL ABI check do not move.

## Downstream changes

### 1. `ar-io-sdk` — the only code change

**`src/solana/io-writeable.ts` → `_buildPrimaryNameValidationAccounts`**: after
the existing `antRecordPda` push, derive and append the `AntConfig` PDA under
the **same already-resolved `antProgram`**:

```ts
remaining.push({ address: antRecordPda, role: AccountRole.READONLY });
// NEW:
const [antConfigPda] = await getAntConfigPDA(antMint, antProgram);
remaining.push({ address: antConfigPda, role: AccountRole.READONLY });
```

- `getAntConfigPDA` already exists (`src/solana/pda.ts`, seeds `["ant_config", mint]`).
- One edit covers all four instructions — `requestAndSet`, `approve`,
  `removeForBaseName`, and the funding-plan variant all route through this
  builder, and the funding-plan's `validationAccountCount =
  args.validationAccounts.length` auto-derives. `requestPrimaryName` is untouched
  (skips the antRecord/antConfig block).
- Update the layout doc-comment (`io-writeable.ts` ~2264-2267).

### 2. `solana-ar-io` + apps — dependency bump only

Migration tooling (`migration/import` post-import `setPrimaryName` /
`approvePrimaryName` / `removePrimaryName`) goes through the ar-io-sdk write
methods, so it inherits the fix on an `@ar.io/sdk` bump. The `verify/` checks
already derive `antConfigPda` independently. No primary-name-builder code change
there. Apps (claim app, portal) bump `@ar.io/sdk`.

## Tests (`ar-io-sdk`)

**A. Unit — PDA parity** (`primary-name.test.ts`, no RPC; mirror the existing
`getAntRecordPDA` / `getPrimaryNameReversePDA` tests)
- `getAntConfigPDA` derives `["ant_config", mint]`.
- Cross-package agreement: SDK `getAntConfigPDA` === `@ar.io/solana-contracts`
  `findAntConfigPda` for canonical + a cluster-override program id. (Verify
  `findAntConfigPda` is exported by the pinned `@ar.io/solana-contracts`; if not,
  assert the raw seed.)

**B. Unit — builder layout** (new; reuse the `stubRpcReturningAsset` pattern from
`mpl-core.test.ts`)
- Stub RPC → serialized `ArnsRecord` (`processId = antMint`); stub
  `fetchAntProgramFromAsset` → canonical. Call
  `_buildPrimaryNameValidationAccounts` for each variant and assert array length,
  that the **last** element === `getAntConfigPDA(antMint, antProgram)`, the
  second-to-last is the `antRecord`, all `READONLY`, and `request` is unchanged.
  Fast ABI-ordering regression guard, no localnet needed.

**C. Localnet** (`io-writeable.localnet.test.ts`; extends the existing
`setPrimaryName` / request+approve cases) — base-name + **undername**
`setPrimaryName`, `approvePrimaryName` two-step, `removePrimaryNameForBaseName`,
and the funding-plan variant, all asserting success against a localnet running
the **updated `ario-core.so`** (built from contracts #91). This is the
integration gate.

**D. e2e** (`sdk.e2e.test.ts`; manual/gated) — `setPrimaryName` against devnet
after the upgraded contract is deployed.

## Release sequencing (hard gate)

1. Merge contracts **#91** → `develop`.
2. **ar-io-sdk PR**: builder change + tests A/B → validate localnet (C) against a
   locally-built #91 `.so` → merge.
3. **Release `@ar.io/sdk`** with the `AntConfig` account.
4. Bump `@ar.io/sdk` in `solana-ar-io` + apps.
5. **Only then** deploy the contracts upgrade: devnet → e2e (D) → mainnet.

🚫 Do **not** deploy the contract upgrade to any cluster before the SDK release
ships, or live primary-name flows break for un-updated clients.

## Risks / notes

- `AntConfig` must derive under the **same `antProgram`** as `AntRecord` — the
  builder resolves it once (security-gated against a spoofed `ANT Program`
  trait); reuse that value.
- BYO-ANT (BD-109): non-canonical ANT programs fall back to the configured
  program for both `AntRecord` and `AntConfig`; unsupported on these write paths
  until pluggable ANT-program owner-resolution lands.
- No `@ar.io/solana-contracts` bump / codegen regen needed (IDL unchanged) beyond
  confirming `findAntConfigPda` exists for test A.
