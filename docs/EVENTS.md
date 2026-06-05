# AR.IO Solana Events — Overview

Single-page entry point for everything event-related. Every other
event doc references this; if you're new to the surface, start here.

> **TL;DR:** All five AR.IO Solana programs emit Anchor `#[event]` on
> every state-changing instruction (74 events, 127+ emit sites). Consume
> via `parseTransactionEvents(rpc, signature)` from
> `@ar.io/sdk/solana`. Events follow the Anchor 0.31 wire format (sha256
> discriminator + borsh body in `Program data:` log lines), so any
> Anchor-aware indexer (Helius, Triton, Subsquid, SubQuery) can decode
> them given our IDL.

## What you get

```ts
import { parseTransactionEvents, isEvent } from '@ar.io/sdk/solana';

const events = await parseTransactionEvents(rpc, signature);
for (const ev of events) {
  if (isEvent(ev, 'NamePurchasedEvent')) {
    portal.toast(`@${ev.data.name} bought for ${ev.data.cost} mARIO`);
  }
}
```

- **`AnyEvent`** — discriminated union over every event from every program. Tagged by `name` (event type) + `programId` (which program emitted it, including CPI attribution).
- **`AnyArio<Program>Event`** — per-program unions for filtering.
- **`parseTransactionEvents(rpc, sig)`** — fetches a tx, decodes events.
- **`parseEventsFromLogs(logs[, eventName])`** — already-fetched logs, with optional filter overload that narrows the return type.
- **`isEvent(ev, 'EventName')`** — generic type guard.

Full API in `sdk/src/solana/events.ts`.

## Coverage by program

| Program | Events | Highlights |
|---|---|---|
| `ario-core` (14) | Token transfer, vault CRUD, primary-name lifecycle, supply/migration finalized, config updates, admin-authority transfer (ADR-026) |
| `ario-gar` (31) | Gateway lifecycle, stake (operator/delegate/redelegate), withdrawals, epoch lifecycle (create→tally→prescribe→distribute→close), multi-source funding plan, admin-authority transfer (ADR-026) |
| `ario-arns` (13) | Name purchases (5 base events × `funding_source: u8` covering 25 emit variants), reassign/release, reservation lifecycle, prune, demand-factor updates, admin-authority transfer (ADR-026) |
| `ario-ant` (16) | Record CRUD + transfer + reconcile + sync_attributes + clear_attributes + asset transfer, controller add/remove, metadata (`field: u8`), record-metadata, ACL (`role: u8`), admin-authority transfer (ADR-026) |
| `ario-ant-escrow` (4) | Unified shapes for 15 instructions via `asset_type: u8` (ANT/Tokens/Vault) + `claim_protocol: u8` (Arweave/Ethereum) |

Full per-event field shapes: `BD-103` in
[`BEHAVIORAL_DIFFERENCES.md`](./BEHAVIORAL_DIFFERENCES.md). Lua-parity
mapping is in the same entry.

## Wire format

Standard Anchor 0.31. Each event is logged as:

```
Program data: <base64>
```

…where the decoded blob is `[discriminator(8) || borsh_payload]` and the discriminator is `sha256("event:<EventName>")[..8]`. The IDL's `events[]` declares the discriminator alongside the type's borsh schema; the SDK codegen verifies the two match at codegen time.

**Stable wire constants** (used as `u8` discriminator fields inside events):

```
FundingSource:  0=Balance  1=Delegation  2=OperatorStake  3=Withdrawal
                4=FundingPlan  5=Turbo (reserved)
PurchaseType:   0=Lease  1=Permabuy
PrunedKind:     0=ExpiredLease  1=Returned  2=ExpiredReservation
EscrowAsset:    0=ANT  1=Tokens  2=Vault
EscrowProtocol: 0=Arweave  1=Ethereum
AntMetadata:    0=Name  1=Ticker  2=Description  3=Keywords  4=Logo
AclRole:        0=Owner  1=Controller
ConfigField:    0=MinVaultDuration  1=MaxVaultDuration
                2=PrimaryNameRequestExpiry  3=NewAuthority
```

These values are **stable forever** — append-only post-mainnet (see
ABI policy).

## Field convention

Every state-changing event ends with `timestamp: i64` populated from
`Clock::get()`. Field order is **actor → identifier → payload →
discriminator → timestamp**. Examples:

```rust
#[event]
pub struct NamePurchasedEvent {
    pub buyer: Pubkey,           // actor
    pub name: String,             // identifier
    pub purchase_type: u8,        // payload (0=Lease, 1=Permabuy)
    pub years: u8,
    pub cost: u64,
    pub ant: Pubkey,
    pub funding_source: u8,       // discriminator
    pub timestamp: i64,
}
```

Field types are pure borsh primitives: `Pubkey`, `u8/u16/u32/u64`,
`i64`, `bool`, `String`, `Option<T>`, `[u8; N]` fixed-size arrays. No
`Vec<...>` (variable-size payloads complicate decoder allocation and
risk log truncation).

## Why `emit!` and not `emit_cpi!`

Anchor offers two event mechanisms:

- **`emit!`** — `sol_log_data` syscall; output appears in transaction
  logs as `Program data:`. **What we use.**
- **`emit_cpi!`** — self-CPI with payload as account data; output
  appears in inner instructions, not logs. Visible to Geyser plugins.

We chose `emit!` because:

- Largest event payload is <250 bytes; well under the per-tx log limit
- Never emit inside loops (verified by tests for batched ops); no
  truncation risk
- ~100 + 1/byte CU vs ~500+ for `emit_cpi!` — meaningful at scale
- All major indexers (Helius, Triton, Subsquid, SubQuery) consume
  log-based events natively given an IDL

If a future program ships with multi-KB payloads or per-iteration
emits, revisit. For our use case, `emit!` is correct.

## CPI attribution

When ario-arns CPIs into ario-gar's `deduct_delegation_for_payment`,
the resulting `StakePaymentEvent` is emitted by **ario-gar**, not
ario-arns. Our log walker tracks this:

```
Program <ARNS>  invoke [1]
  Program log: Instruction: BuyNameFromDelegation
  Program <CORE> invoke [2]                       ← stack push
    Program data: <TransferEvent base64>          ← attributed to CORE
  Program <CORE> success                          ← stack pop
  Program data: <NamePurchasedEvent base64>       ← attributed to ARNS
Program <ARNS>  success
```

Tested in both the synthetic-log unit suite (`events.test.ts`) and the
real-BPF localnet suite (`events.localnet.test.ts`).

## ABI policy

**Pre-mainnet (now):** breaking changes are explicitly allowed. PR-6.5
used this to add `timestamp` to ~50 events, add `new_value` to
ConfigUpdated/AntMetadataUpdated, add `expires_at` to NameReserved.

**Post-mainnet:** field shapes are immutable. New event → new name
(`*EventV2`). Both events emit at the same site for at least one
minor SDK release. The IDL snapshot at
`contracts/idl-event-snapshots.json` becomes a CI freeze gate.

Full rationale: `ADR-017` in [`DECISIONS.md`](./DECISIONS.md).

## Subscribing live (consumer patterns)

This SDK provides **decoders**, not transports. Pick a transport
that matches your needs:

### Pull (post-confirmation)

```ts
const result = await ario.buyName({ ... });
const events = await parseTransactionEvents(rpc, result.id);
```

Use when you submit the tx and want the events back. Simplest.

### WebSocket (live tx logs from a single program)

```ts
const subId = await rpcSubscriptions
  .logsSubscribe(
    { mentions: [ARIO_ARNS_PROGRAM_ADDRESS] },
    { commitment: 'confirmed' },
  )
  .subscribe();

for await (const notification of /* iterator */) {
  const events = parseEventsFromLogs(notification.value.logs);
  // ...
}
```

Use for in-process portals/dashboards. Handles tx ordering naturally.

### Helius / Triton webhooks

Register a webhook subscribing to `accountAddresses: [ARIO_*_PROGRAM_ADDRESS]`
with `transactionType: enhanced`. Helius decodes Anchor events
automatically when you upload our IDL. The decoded event JSON
matches our `AnyEvent` shape.

Use for production indexers / per-event notifications. Handles
backfill + retry.

### Geyser plugin

Not currently supported (`emit!` outputs go to logs, not account
writes). If a future use-case needs Geyser visibility, that's the
trigger to consider `emit_cpi!` for the affected events.

## Where to look in the codebase

| File | Purpose |
|---|---|
| `contracts/programs/<prog>/src/lib.rs` (or `state/mod.rs`) | `#[event]` struct definitions |
| `contracts/programs/<prog>/src/instructions/*.rs` | `emit!()` call sites |
| `sdk/src/solana/events.ts` | Public API: `parseTransactionEvents`, `parseEventsFromLogs`, `isEvent`, `AnyEvent` |
| `sdk/src/solana/generated/<prog>/events/<name>.ts` | Per-event encoder/decoder/codec (auto-generated; never edit) |
| `sdk/scripts/events-codegen.mjs` | Code generator (run via `yarn codegen`) |
| `contracts/idl-event-snapshots.json` | ABI snapshot pin (regenerated pre-mainnet, frozen at cutover) |
| `contracts/scripts/idl-event-snapshot.mjs` | Snapshot check / update tool |
| `sdk/src/solana/events.test.ts` | Unit test layer (synthetic logs) |
| `sdk/src/solana/events.localnet.test.ts` | Real-BPF localnet test layer |
| `contracts/test-utils/src/lib.rs` | Rust-side test helpers (`expect_event!`, `bpf_required!`) |

## Adding a new event (step-by-step)

1. Add `#[event] pub struct MyNewEvent { ... }` to the program's `lib.rs` (or wherever existing events live in that program).
2. Add `emit!(MyNewEvent { ... });` at the end of the relevant handler, after all state mutations succeed. **Never inside a loop.**
3. Run `cd contracts && anchor build --no-docs --skip-lint` — refreshes the IDL.
4. Run `cd sdk && yarn codegen` — regenerates the SDK decoder (Codama for accounts/instructions, our codegen for events).
5. Add a unit test in `sdk/src/solana/events.test.ts` for round-trip.
6. Run `node contracts/scripts/idl-event-snapshot.mjs --update` (pre-mainnet) or fail CI on shape changes (post-mainnet).
7. Update `BD-103` in `BEHAVIORAL_DIFFERENCES.md` if the event has Lua parity.

## Recipes

### Helius webhook → typed events

Helius decodes Anchor events automatically when you upload our IDL. The simplest production setup:

```ts
// 1. Upload contracts/target/idl/ario_arns.json (etc.) to Helius
//    via the IDL upload API: https://docs.helius.dev/anchor-idls
//
// 2. Register a webhook subscribing to the program ID:
//    POST https://api.helius.xyz/v0/webhooks
//    {
//      "webhookURL": "https://my-indexer.example/ar-io-events",
//      "transactionTypes": ["ANY"],
//      "accountAddresses": ["ARioArnsProgXXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX"],
//      "webhookType": "enhanced"
//    }
//
// 3. Webhook payloads include `events.programs[].name` and decoded args.
//    To map them onto our SDK's typed AnyEvent shape:

import {
  parseEventsFromLogs,
  type AnyEvent,
} from '@ar.io/sdk/solana';

app.post('/ar-io-events', (req, res) => {
  for (const tx of req.body) {
    const events: AnyEvent[] = parseEventsFromLogs(tx.meta.logMessages ?? []);
    for (const ev of events) {
      // ev.name is narrow-typed; ev.data.* is fully typed
      myDB.insertEvent(ev);
    }
  }
  res.sendStatus(200);
});
```

Helius's own `events.programs[].name` parsing is fine, but using
`parseEventsFromLogs` keeps your indexer code consistent with anything
using the SDK directly (CLI tools, in-process subscribers).

### Filtering by event type at fetch time

```ts
// Pull only NamePurchasedEvents from a tx; the type narrows.
const purchases = parseEventsFromLogs(logs, 'NamePurchasedEvent');
//                                              ^^^^^^^^^^^^^^^^^^
// purchases: Array<{ programId, name: 'NamePurchasedEvent', data: NamePurchasedEvent }>
purchases.forEach((ev) => console.log(ev.data.buyer, ev.data.cost));
```

### Reconstructing event history for an address

Solana RPC retains ~2 weeks of tx history on public RPCs (longer with
archive nodes / Helius / Triton). For "every event involving address
X":

```ts
// 1. Fetch the address's recent signatures
const sigs = await rpc.getSignaturesForAddress(address, { limit: 1000 }).send();

// 2. Decode each tx's events
const events: AnyEvent[] = [];
for (const { signature } of sigs) {
  events.push(...await parseTransactionEvents(rpc, signature));
}

// 3. Filter to events that mention the address (varies per event type)
const involvesUser = events.filter(
  (ev) =>
    ('buyer'    in ev.data && ev.data.buyer    === address) ||
    ('owner'    in ev.data && ev.data.owner    === address) ||
    ('caller'   in ev.data && ev.data.caller   === address) ||
    ('delegator' in ev.data && ev.data.delegator === address),
);
```

For longer-than-2-weeks history, subscribe a Helius webhook from day
one and persist events into your own DB. The SDK doesn't ship a
historical-replay helper because the right answer depends on your
storage backend.

## FAQ / common errors

**`parseTransactionEvents()` returns `[]` — why?**

1. **Tx not yet confirmed.** The default commitment is `'confirmed'`. If you call this immediately after `sendTransaction`, the RPC may not have the tx yet. Either wait for `confirmed` (the SDK writeable methods do this for you), or pass `{ commitment: 'finalized' }` for stricter ordering.
2. **Tx failed before the emit.** Anchor `emit!` runs after state mutations. If the instruction reverted earlier, no event was logged. Check `tx.meta.err` first.
3. **Wrong RPC cluster.** Devnet vs mainnet vs localnet — the signature only resolves on the cluster where the tx was submitted.
4. **Tx older than RPC retention.** Public RPCs retain ~2 weeks. Use Helius/Triton for older history.
5. **Events from a non-AR.IO program in the same tx.** They're silently skipped by the dispatch table (unknown discriminators). The AR.IO events still decode.

**My event has a `bigint` field but my JSON serializer crashes.**

`bigint` doesn't `JSON.stringify` natively. Pre-stringify with a replacer:

```ts
JSON.stringify(event, (_k, v) => (typeof v === 'bigint' ? v.toString() : v));
```

**`Option<T>` field decodes as `{__option: 'Some', value: T}` — is that intentional?**

Yes. `@solana/kit`'s `getOptionDecoder` returns a tagged union to preserve the borsh wire format symmetrically. To convert to nullable:

```ts
const target = ev.data.target.__option === 'Some' ? ev.data.target.value : null;
```

We could have unwrapped to `T | null` in our wrapper but chose not to — it'd lose the symmetry with kit's encoder API which accepts both `OptionOrNullable<T>` shapes.

**My CPI emit got attributed to the wrong program.**

The log walker follows Solana's standard `Program <id> invoke [N]` / `Program <id> success` framing. If you have a custom program in the tx that emits non-standard frames, attribution may slip. File a repro — we can extend the parser. The shipped parser handles every Anchor program correctly.

**Adding a new program — do I need to update events.ts?**

Yes. Add it to:
1. `sdk/scripts/events-codegen.mjs` `PROGRAMS` array
2. `sdk/src/solana/events.ts` — the `groups` array in `buildDispatch()` (so the discriminator dispatch knows about its events)
3. The per-program `*EventByName` mapped type (auto-extends `AnyEvent`)

`yarn codegen` regenerates per-event decoders; the dispatch table picks them up at first call.

## Cross-references

- **Catalog:** [`BEHAVIORAL_DIFFERENCES.md` § BD-103](./BEHAVIORAL_DIFFERENCES.md)
- **ABI policy:** [`DECISIONS.md` § ADR-017](./DECISIONS.md)
- **Gap analysis (rollout history):** [`EVENT_EMISSION_AUDIT.md`](./EVENT_EMISSION_AUDIT.md)
- **Build plan (rollout history):** [`EVENT_EMISSION_IMPLEMENTATION_PLAN.md`](./EVENT_EMISSION_IMPLEMENTATION_PLAN.md)
- **Superseded original draft:** [`EVENT_EMISSION_PLAN.md`](./EVENT_EMISSION_PLAN.md) (kept for context)
- **Per-instruction surface:** the IDLs (`contracts/target/idl/<program>.json`) + Codama-generated SDK clients are the source of truth. `INSTRUCTION_REFERENCE.md` was archived to `docs/archive/` in commit e7792bfc because it drifted faster than it could be maintained.
- **Cranker subscription pattern:** [`EPOCH_CRANKER_ARCHITECTURE.md`](./EPOCH_CRANKER_ARCHITECTURE.md)
- **SDK consumer code:** `sdk/src/solana/events.ts`
- **Solana foundation reference:** [`@solana/kit` codec docs](https://www.solanakit.com/docs/codecs)
