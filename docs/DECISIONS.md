# Architecture Decision Log

> [!IMPORTANT]
> **New ADRs go in [`adrs/`](adrs/) using the
> [MADR](https://adr.github.io/madr/) format**, one file per decision.
> See [`adrs/README.md`](adrs/README.md) for the workflow and the
> template at [`adrs/0000-template.md`](adrs/0000-template.md).
>
> **This file needs cleanup.** It was copied verbatim from the
> pre-split monorepo and contains the historical ADRs (nominally
> ADR-001 through ADR-018) authored before the contracts repo was
> extracted. It is still authoritative for the decisions it
> describes, but it has known organizational issues:
>
> * The numbering is **not unique** — ADR-011 and ADR-012 each appear
>   twice with different titles (lines 388, 720 and lines 529, 780),
>   making "ADR-011" / "ADR-012" ambiguous when cited.
> * Section formatting is free-form and inconsistent; not every entry
>   has the same Status / Deciders / Date conventions.
> * Some entries reference migration tooling, the SDK, or other
>   sibling repos that no longer live alongside this file.
>
> The cleanup plan (renumber duplicates, normalize sections, port
> entries into [`adrs/`](adrs/) one file each, then archive this
> file) is sketched in
> [`adrs/README.md` § "Migration plan"](adrs/README.md). It's not
> blocking — until that work happens, the entries below are still the
> canonical record.

This document records significant architectural decisions made during the AR.IO Solana migration.

---

## ADR-001: Target Platform Selection

**Date:** 2026-03-17
**Status:** Accepted
**Deciders:** AR.IO Team

### Context

The AR.IO network currently runs on AO (Arweave Object), which has become unstable due to Forward Research's Hyperbeam migration. We need to select a new compute platform.

### Decision

**Selected: Solana**
**Fallback: Base (Ethereum L2)**

### Rationale

**For Solana:**
- Existing business relationships with Solana ecosystem partners
- High performance (65k TPS, 400ms finality)
- Low transaction costs (~$0.0002)
- Strong NFT ecosystem (for ANTs as NFTs)

**Trade-offs Accepted:**
- RSA wallet migration requires off-chain service (no on-chain RSA verification)
- Mandatory indexer ($350-1100/month) due to getProgramAccounts restrictions
- Account model requires state decomposition into PDAs
- More complex development than EVM

### Consequences

- Must build RSA verification service for Arweave wallet migration
- Must budget for Helius/GenesysGo indexer infrastructure
- Must redesign state as PDA accounts
- Can leverage Jupiter, Metaplex, and Solana DeFi ecosystem

---

## ADR-002: ANT Architecture

**Date:** 2026-03-17
**Status:** Accepted
**Deciders:** AR.IO Team

### Context

Currently, each ArNS name has its own ANT (Arweave Name Token) process on AO. This is a separate smart contract per name, which is expensive and complex on most platforms.

### Decision

**Replace individual ANT processes with Metaplex NFTs**

### Rationale

- NFTs are the natural fit for unique, transferable name ownership
- Metaplex is the standard on Solana
- Single program manages all ANTs (vs thousands of separate contracts)
- Marketplace integration (Tensor, Magic Eden) enables trading
- Undername records stored in separate PDAs per NFT

### Consequences

- ANT "processes" become NFT mints
- Undername records stored separately (not in NFT metadata)
- Controllers stored as separate PDA accounts
- Transfer hooks may be needed for registry updates

---

## ADR-003: State Migration Strategy

**Date:** 2026-03-17
**Status:** Accepted
**Deciders:** AR.IO Team

### Context

We need to migrate all state from AO to Solana, including balances, gateways, names, epochs, and vaults.

### Decision

**Clean genesis with state snapshot and merkle proofs**

### Rationale

- Snapshot AO state at specific block height
- Generate merkle tree for each state type
- Import to Solana via admin-only batch instructions
- Merkle proofs enable verification
- No historical data (only current state)

### Alternatives Considered

1. **Live migration:** Too complex, risk of inconsistency
2. **Replay all transactions:** Not feasible, transactions are AO-specific
3. **Bridge-based:** Would require ongoing AO operation

### Consequences

- Need to build snapshot exporter for AO
- Need to build batch import instructions for Solana
- Historical epoch data not migrated (only current/recent)
- Clear cutover point (freeze AO, import to Solana, resume)

---

## ADR-004: RSA Wallet Migration

**Date:** 2026-03-17
**Status:** Accepted
**Deciders:** AR.IO Team

### Context

Existing AR.IO users have Arweave wallets using RSA-4096 signatures. Solana cannot verify RSA signatures on-chain.

### Decision

**Off-chain RSA verification service with on-chain attestation**

### Design

1. User signs message with RSA key: "I authorize migration to {solana_pubkey}"
2. Off-chain service verifies RSA signature
3. Service signs attestation (with trusted keypair)
4. User submits attestation to Solana to claim assets
5. One-time process per user

### Rationale

- Only viable path for RSA support on Solana
- Off-chain verification is computationally feasible
- Attestation model is common pattern
- One-time cost, not ongoing

### Alternatives Considered

1. **Solana precompile:** Doesn't exist for RSA
2. **ZK proof of RSA:** Too complex, expensive
3. **Manual admin migration:** Doesn't scale, trust issues

### Consequences

- Need to build and secure RSA verification service
- Service becomes trust point (must be transparent)
- Users who don't migrate lose access (need clear deadline)
- Can sunset service after migration window

---

## ADR-005: Epoch Automation

**Date:** 2026-03-17
**Status:** Accepted (Updated 2026-04-08)
**Deciders:** AR.IO Team

### Context

Epochs need to tick daily (create new epoch, distribute rewards). Solana has no native cron/scheduler.

### Options Considered

1. **Clockwork:** Solana-native automation platform — rejected (project is deprecated)
2. **Custom keeper:** Our own service calling tick instruction
3. **Economic incentive:** Anyone can call tick, gets small reward

### Decision

**Permissionless cranking with standalone bot + observer integration**

All epoch instructions (`create_epoch`, `tally_weights`, `prescribe_epoch`, `distribute_epoch`) are permissionless — callable by any wallet. Automation is provided by:

1. **Standalone cranker bot** (`cranker/`) — dedicated TypeScript service with Docker deployment, health endpoint, and Prometheus metrics
2. **Observer integration** — opt-in `ENABLE_EPOCH_CRANKING=true` flag in ar-io-observer, providing massive redundancy at zero extra cost

On-chain idempotency guards (`weights_tallied`, `prescriptions_done`, `rewards_distributed` flags) make it safe for multiple crankers to run simultaneously.

### Rationale

Permissionless design eliminates single points of failure. No Clockwork dependency (project is deprecated). No keeper rewards needed — gateway operators have self-interest in triggering reward distribution. Multiple crankers waste only ~0.001 SOL/day in duplicate transaction fees.

See `docs/EPOCH_CRANKER_ARCHITECTURE.md` for full solution architecture.

---

## ADR-006: Program Architecture

**Date:** 2026-03-17
**Status:** Accepted (Updated)
**Deciders:** Contract Architect, AR.IO Founder

### Context

How should we structure the Solana programs? One monolithic program or multiple specialized programs?

### Options Considered

1. **Single program:** All logic in one, simpler but monolithic
2. **5 programs:** Separate each domain (gateway, epoch, arns, ant, core)
3. **3+1 programs:** Consolidated domains for atomic transactions + standalone ANT program

### Decision

**3+1 programs:**

```
ario-core     - Token, Balances, Vaults, Primary Names
ario-gar      - Gateway Registry + Staking + Delegation + Epochs + Rewards
ario-arns     - Name Registry + Demand Factor + Reserved/Returned Names
ario-ant      - Arweave Name Token (Metaplex Core NFT), Undername Records, Controllers
```

### Rationale

**Why 3+1 programs (vs 5 or monolithic):**
- **Atomic transactions**: Gateway join can stake tokens + update registry atomically without complex CPI chains
- **Epoch efficiency**: Observer/name prescription and reward distribution in single program avoids CPI latency
- **Reduced CPI overhead**: Fewer cross-program invocations = lower compute costs per transaction
- **Simpler upgradeability**: Fewer programs to coordinate during upgrades
- **Name prescription without indexer**: ario-gar can read NameRegistry from ario-arns to prescribe names on-chain

**Why not single program:**
- Would exceed account limits (~64 per instruction)
- Would complicate upgrades (all or nothing)
- Cleaner separation of concerns between unrelated domains

**Why ario-gar consolidates gateway + epoch:**
- Epochs primarily operate on gateways (observer selection, rewards)
- Reward distribution needs to update gateway stats atomically
- Observer prescription uses gateway stake weights

**Why ario-ant is a separate 4th program:**
- ANTs are Metaplex Core NFTs with their own state (config, controllers, undername records)
- Separate program enables independent upgrade authority
- ANT operations don't need atomic access to ArNS pricing or registry state
- Clean separation: ario-arns handles name ownership/pricing, ario-ant handles per-ANT metadata and records

### Consequences

- ario-gar is the largest program (~1500 LOC)
- CPI only needed for token transfers from ario-core
- NameRegistry in ario-arns enables permissionless epoch name prescription
- GatewayRegistry in ario-gar enables permissionless observer selection
- ario-ant is a standalone 4th program (767 LOC, 85 unit + 76 integration tests)
- Each program has own upgrade authority (Squads multi-sig)

---

## ADR-007: Indexer Selection

**Date:** 2026-03-17
**Status:** Proposed
**Deciders:** Infrastructure Engineer

### Context

Public Solana RPCs disable or rate-limit getProgramAccounts. We need an indexer for enumeration queries (list all gateways, all names, etc.).

### Options

1. **Helius DAS API:** Comprehensive, includes NFT indexing
2. **GenesysGo:** Good performance, different pricing
3. **Triton:** Premium option
4. **Custom indexer:** Build our own with Geyser plugin

### Decision

**TBD - Likely Helius DAS API**

### Rationale

Helius provides good NFT support (for ANTs), DAS API is well-documented, pricing is reasonable.

---

## ADR-008: Multi-sig and Upgrade Authority

**Date:** 2026-03-17
**Status:** Proposed
**Deciders:** AR.IO Team

### Context

Program upgrades and critical operations need multi-sig control, similar to current VAOT on AO.

### Options

1. **Squads Protocol:** Standard Solana multi-sig
2. **Realms (SPL Governance):** More complex, DAO-oriented
3. **Custom:** Build our own

### Decision

**TBD - Likely Squads Protocol**

### Rationale

Squads is the standard, well-audited, supports timelocks.

---

## ADR-009: Import-Then-Claim Migration Strategy

**Date:** 2026-03-18
**Status:** Accepted
**Deciders:** AR.IO Founder

### Context

We need a concrete mechanism for moving state from AO to Solana. ADR-003 decided on "clean genesis with snapshot," but didn't specify how users would gain control of their migrated state given that Arweave RSA addresses can't be used on Solana.

### Decision

**Import-Then-Claim: Admin imports all state, users claim control via RSA attestation**

1. **Pre-registration phase (T-4w):** RSA verification service opens. Gateway operators, name owners, and token holders register their Arweave→Solana address mapping before cutover.
2. **Import phase (T-0):** Admin authority imports all AO state to Solana. Pre-registered users' state is imported directly to their Solana pubkey. Unregistered users' state goes to escrow PDAs keyed by SHA256(arweave_address).
3. **Claim phase (T+0 onward):** Unregistered users use the RSA service + claim app to prove Arweave ownership and transfer state from escrow to their Solana address.

### Alternatives Considered

1. **Claim-then-import:** Wait for all users to register before going live. Rejected: would delay network launch indefinitely.
2. **Admin-only migration:** Admin maps addresses manually. Rejected: doesn't scale, trust issues.
3. **Bridge-based:** Run both systems simultaneously. Rejected: complexity, requires ongoing AO operation.

### Rationale

- Network goes live immediately — gateways, names, and epochs are functional from minute one
- Active users (operators, name owners) pre-register and get a seamless experience
- Passive users (token holders) claim at their own pace — no one loses tokens
- Gateway operators MUST pre-register (no escrow gateways — they can't participate in epochs)
- Escrow claims remain active indefinitely (the "deadline" is for urgency messaging, not token seizure)

### Consequences

- Must build RSA verification service (off-chain)
- Must build claim web app (React + Arweave wallet + Solana wallet)
- Each program needs import instructions (admin-gated by `migration_active` flag)
- No on-chain escrow accounts or claim instructions needed — unclaimed state held by admin, transferred off-chain after RSA verification
- Need pre-registration campaign 4+ weeks before cutover
- Need batch import orchestrator, admin claim script, and verification suite
- See `docs/MIGRATION_ARCHITECTURE.md` for full specification

---

## ADR-010: Migration Authority Model

**Date:** 2026-03-18
**Status:** Accepted
**Deciders:** AR.IO Founder

### Context

The migration import phase requires a high-throughput authority key that can create thousands of accounts quickly. The steady-state program authority should be a Squads multi-sig. These have different requirements — multi-sig is too slow for batch imports, but a single key is too risky for production.

### Decision

**Two-tier authority: Multi-sig delegates to a migration hot key, then revokes**

1. **Development:** Single keypair for convenience
2. **Mainnet deploy:** Squads multi-sig (N-of-M) set as program `authority`
3. **Migration:** Multi-sig sets `migration_authority` to a dedicated hot key via config update
4. **Import phase:** Hot key executes batch imports (gated by `migration_active == true`)
5. **Post-import:** Multi-sig calls `finalize_migration`, setting `migration_active = false` permanently
6. **Steady state:** Hot key is inert (no instructions check `migration_authority` when `migration_active` is false). Multi-sig handles upgrades and config.

### Rationale

- Hot key enables fast batch imports (~17,500 transactions) without multi-sig approval per tx
- Hot key's power is scoped: only import instructions, only while `migration_active` is true
- Revocation is permanent and on-chain (`migration_active = false` cannot be re-enabled)
- Multi-sig retains full control of the programs at all times

### Consequences

- Programs need `migration_active`, `migration_authority`, and `attestation_key` fields in their config
- `finalize_migration` instruction is one-way (cannot re-enable imports)
- Squads member rotation handles long-term authority changes

---

## ADR-011: On-chain ANT ACL Registry + MPL Core Lifecycle Hook

**Date:** 2026-04-17
**Status:** Superseded by [ADR-012](#adr-012-paginated-per-user-ant-acl) (2026-04-27) — the single bounded `AntAcl { owned: Vec<Pubkey>, controlled: Vec<Pubkey> }` shape was replaced with a paginated `AclConfig` + `AclPage` per-user structure before the initial mainnet implementation shipped. The Phase 1 instruction surface (`register_acl`, `acl_record_owner`, `acl_record_controller`, `acl_remove_owner`, `acl_remove_controller`, `close_acl`) is replaced; the design rationale and the abandoned MPL Core Lifecycle Hook (Phase 2) discussion below remain accurate and informative for the program-level decision (on-chain reverse index vs. DAS / foundation indexer).
**Deciders:** AR.IO Founder, Contract Architect

### Context

Frontends need to answer "give me every ANT this address can manage" for two roles:
- **Owner** — the address holding the MPL Core asset
- **Controller** — addresses listed in the ANT's `AntControllers.controllers`

The naive approaches all have problems for AR.IO's decentralization goals:

| Approach | Owner lookup | Controller lookup | Decentralization | Cost |
|---|---|---|---|---|
| `getProgramAccounts` scan | O(N) full scan | O(N) full scan | Pure on-chain | Free but slow; many RPC providers throttle |
| Helius DAS / similar | `getAssetsByOwner` (fast) | Not indexed | Third-party dep | Per-request fee or quota |
| Foundation indexer (Yellowstone gRPC + Postgres) | Fast | Fast | Foundation-run SPOF | ~$5/mo + uptime |

On AO, this was solved by an `ar-io-ant-registry` contract: every ANT process "phoned home" with its owner + controller state, and frontends queried the registry for an ACL view. Solana lets us do the same thing, but **better** — because every ownership/controller mutation already passes through `ario-ant`, we can maintain the reverse-index atomically as a side effect of normal operation rather than relying on out-of-band convention.

### Decision

**Per-address `AntAcl` PDA accounts in `ario-ant`, maintained atomically by:**

1. **Phase 1 (this ADR):** Explicit ACL maintenance instructions (`register_acl`, `acl_record_owner`, `acl_record_controller`, `acl_remove_owner`, `acl_remove_controller`, `close_acl`) callable independently or bundled by the SDK alongside the operations they shadow. Existing instruction surfaces (`initialize`, `add_controller`, `remove_controller`, `reconcile`) are unchanged for backward compatibility.

2. **Phase 2 (abandoned — see "Phase 2 status update" below):** Originally proposed an MPL Core `LifecycleHook` External Plugin Adapter to atomically maintain the ACL on marketplace transfers. Investigation found this is unimplemented upstream and unlikely to ship soon; see addendum below for the revised drift-handling policy.

### Account shape

```
PDA: ["ant_acl", address.as_ref()]

#[account]
pub struct AntAcl {
    pub address: Pubkey,           // the wallet this ACL belongs to
    pub owned: Vec<Pubkey>,        // ANT mints (MPL Core asset addresses) this address owns
    pub controlled: Vec<Pubkey>,   // ANT mints this address is listed as a controller on
    pub bump: u8,
}
```

Caps: `MAX_ACL_OWNED = 1000`, `MAX_ACL_CONTROLLED = 1000`. Account grows via realloc on each entry add (32 bytes / entry, well under the 10KB per-tx realloc limit).

### Frontend lookup pattern

```typescript
// 2 RPC calls, no DAS, no scans, no foundation infra
const acl = await connection.getAccountInfo(deriveAntAcl(userAddress));
const ants = await connection.getMultipleAccountsInfo([
  ...acl.owned,
  ...acl.controlled,
]);
```

### Rationale

**Why on-chain ACL (vs DAS or foundation indexer):**
- Owner *and* controller queries served by the same primitive (DAS doesn't see custom Anchor PDAs)
- Zero foundation infrastructure to operate, fund, or be deplatformed from
- Operator gateways stay on a pure base-RPC diet (no DAS dependency, no API key)
- 2 RPC calls per "show my ANTs" — within free-tier budgets even for high-traffic frontends
- Mirrors the AO `ar-io-ant-registry` pattern that the ecosystem already understands

**Why explicit ACL instructions (Phase 1) rather than embedding maintenance into existing instructions:**
- Backward compatible — existing tests, migration importer, and SDK callers don't need to change
- Lets the SDK opt callers in atomically (bundle `add_controller` + `acl_record_controller` in one tx) while leaving raw / minimal callers unaffected
- Keeps each instruction's account list bounded — `reconcile` doesn't need to take 12 ACL accounts (2 owners + up to 10 controllers) all at once
- ACL drift is recoverable via the permissionless cleanup instructions; never breaks an ANT

**Why MPL Core Lifecycle Hook was originally proposed for Phase 2:**
- Atomic ACL maintenance for marketplace transfers (Tensor, Magic Eden), which bypass `ario-ant` and use raw `MPL_CORE::TransferV1`
- Only known approach that runs *inside* the transfer transaction without requiring all marketplaces to integrate against `ario-ant` directly
- Fallback to Phase 1 explicit instructions when a marketplace integration doesn't surface the hook accounts (low-probability but real risk)

### Phase 2 status update (2026-04-17): abandoned

After investigating the actual MPL Core source, **the Lifecycle Hook External Plugin Adapter is unimplemented upstream**. The plugin type and registry entries are defined, but `validate_transfer` is a no-op stub that returns `abstain!()` — MPL Core never CPIs into the configured `hooked_program`:

```rust
// programs/mpl-core/src/plugins/external/lifecycle_hook.rs
impl PluginValidation for LifecycleHook {
    fn validate_transfer(&self, _ctx: &PluginValidationContext)
        -> Result<ValidationResult, ProgramError> {
        abstain!()  // ← does NOT CPI to hooked_program
    }
}
```

Plugin creation is also explicitly blocked by [PR #106](https://github.com/metaplex-foundation/mpl-core/pull/106) until dispatch lands. Active upstream work to track:
- [PR #221](https://github.com/metaplex-foundation/mpl-core/pull/221) (open) — WIP "Plugin side effects with example" demonstrating the missing capability via a `TransferCount` plugin. Stalled on validation-cost concerns.
- [PR #257](https://github.com/metaplex-foundation/mpl-core/pull/257) (open) — Foundational refactor of the validation pipeline that would precede dispatch implementation.

The reserved `on_lifecycle_hook` instruction stub remains in `ario-ant` (rejecting all invocations with `InvalidHookCaller`) so the discriminator slot is reserved for the day MPL Core ships dispatch, with implementation guidance preserved as a block comment in `programs/ario-ant/src/lib.rs`.

**Revised drift-handling policy** (replaces the original Phase 2):

External transfers (marketplace fills, wallet "send NFT", third-party programs CPIing into `MPL_CORE::TransferV1`) cause `AntAcl` to drift. Rather than building a cranker bot or waiting for upstream, drift is handled by:

1. **Frontend render-time cross-check** — `AntAcl` is read as a hint; the frontend filters against canonical MPL Core asset state at render time, so users never see stale data. This is a correctness guarantee, not a performance optimization.
2. **"Claim ANT" UI flow** — users who acquire an ANT outside our SDK (gift, marketplace purchase, wallet move) sign one `acl_record_owner` transaction during onboarding to register the ANT in their `AntAcl`. Permissionless, idempotent, ~5,000 lamports rent.
3. **Permissionless cleanup** — `acl_remove_owner` / `acl_remove_controller` are available for anyone (including a community-run cranker) to prune stale entries. Not required for correctness; only matters if the index grows enough to affect read latency.

This is acceptable because (a) ARIO ANTs are functional assets tied to ArNS leases — secondary marketplace volume is expected to be near zero for years, (b) every in-SDK operation maintains the ACL atomically already, and (c) the only user-visible failure mode (newly-received ANT not in ACL) is naturally surfaced by a one-time claim during onboarding, which is arguably better UX than magic discovery.

### Alternatives considered

1. **`getProgramAccounts` with memcmp filter on `AntControllers`** — works but slow (O(total ANT count) scan), throttled or disabled on most public RPCs, and degrades with growth. Useful as a fallback debugging tool, not a frontend primitive.

2. **DAS for owners + foundation indexer for controllers** — ergonomic for frontend devs but creates two third-party dependencies (DAS provider + foundation indexer uptime). Foundation indexer is also a deplatformable single point of failure.

3. **Embed ACL writes into existing instructions** (instead of separate instructions) — cleaner caller API but requires breaking changes across migration importer, SDK, integration tests, and the snapshot pipeline. Phase 1 explicit instructions can be promoted to mandatory in a future ADR once all callers have migrated.

4. **Permanent Transfer Delegate plugin** — would force all transfers through `ario-ant`, breaking native MPL Core marketplace transfers entirely. Rejected: too invasive for what is fundamentally a read-side optimization.

5. **Helius Webhooks** — push notifications for owner changes. Rejected: requires foundation-funded webhook subscription, doesn't give frontends a chain-native query primitive, and doesn't cover controllers.

### Consequences

- **New `ario-ant` surface:** `AntAcl` account, `register_acl`, `acl_record_owner`, `acl_record_controller`, `acl_remove_owner`, `acl_remove_controller`, `close_acl` instructions
- **New error codes:** `AclEntryAlreadyExists`, `AclEntryNotFound`, `AclMaxEntriesReached`, `AclNotEmpty`, `AclAddressMismatch`, `NotCurrentOwner`, `NotCurrentController`, `StillCurrentOwner`, `StillCurrentController`
- **SDK additions:** `ANT.getANTsForAddress({ address, role })` reads `AntAcl` and fans out via `getMultipleAccountsInfo`. SDK write helpers bundle `acl_record_*` calls with their corresponding mutation in a single transaction.
- **Per-address rent:** ~73 bytes baseline + 32 bytes/entry. A user with 5 owned + 5 controlled ≈ 233 bytes ≈ 0.0017 SOL ≈ $0.34 in rent (refundable via `close_acl`).
- **Per-mutation overhead:** ~5–10k CU per ACL account written. Well within all program CU budgets (`ario-ant` is 200k CU default).
- **Migration tooling:** `migration/import` will populate `AntAcl` accounts as part of phase-2 ANT import (one extra CPI per existing controller). Existing snapshot format unchanged — ACL is derived from `AntControllers` + MPL Core asset owner.
- **Off-program transfer recovery:** if an ANT is transferred via raw MPL Core, the ACL goes stale. The frontend filters stale entries at render time using canonical MPL Core state (correctness preserved), and a one-time "Claim ANT" flow lets the new owner register the asset in their `AntAcl` via `acl_record_owner`. See "Phase 2 status update" above for the full drift-handling policy.
- **Reserved instruction surface:** `on_lifecycle_hook` is published with a defensive caller check (rejects all invocations with `InvalidHookCaller`) so the discriminator slot is reserved if MPL Core ever ships External Plugin Adapter dispatch. Implementation guidance preserved as a block comment in `programs/ario-ant/src/lib.rs`.

### Security considerations

- **CPI caller validation in the reserved hook stub:** the published `on_lifecycle_hook` instruction *must* verify the caller is `MPL_CORE_PROGRAM_ID` (e.g. via the instruction sysvar) to prevent spoofed transfer events from corrupting ACL state if it ever becomes reachable. Currently rejects all callers with `InvalidHookCaller`.
- **Asset ownership verification in `acl_record_owner`:** instruction must read the MPL Core asset and confirm the target address actually owns it before pushing to `acl.owned` — otherwise anyone could grow an arbitrary user's `owned` list.
- **Controller verification in `acl_record_controller`:** instruction must read the asset's `AntControllers` and confirm the target address is currently listed before pushing to `acl.controlled` — same DoS surface.
- **Removal verification in `acl_remove_*`:** the inverse — only allow removal if the address is *no longer* the owner / *no longer* a controller. This makes the cleanup instructions permissionless and safe (anyone can prune stale entries; no-one can prune live entries).
- **DoS surface bounded by `MAX_ACL_*` caps:** an attacker can fill a victim's lists with their own ANTs, but pays their own rent for the grief, and the victim can self-remove via `remove_controller` on each attacking ANT followed by `acl_remove_controller`. Cost-asymmetric in defender's favor.
- **Reentrancy:** none — Solana doesn't have reentrancy in the EVM sense. ACL writes happen after permission/state checks complete.

---

## ADR-012: Paginated per-user ANT ACL

**Date:** 2026-04-27
**Status:** Accepted
**Supersedes:** [ADR-011](#adr-011-on-chain-ant-acl-registry--mpl-core-lifecycle-hook) (Phase 1 instruction surface only — the on-chain-reverse-index program-level decision and the abandoned MPL Core Lifecycle Hook addendum from ADR-011 still hold)
**Deciders:** AR.IO Founder, Contract Architect

### Context

ADR-011 shipped a single-account per-user ACL — `AntAcl { owned: Vec<Pubkey>, controlled: Vec<Pubkey> }` — capped at `MAX_ACL_OWNED = MAX_ACL_CONTROLLED = 1000`. While reviewing the cap before mainnet, three things surfaced that the original ADR did not capture:

1. **The 1000 cap was set by an implementation detail, not the protocol.** The realloc-limit bound the ADR cites permits ~300k entries per side. The actual binding constraint is the BPF heap budget when Anchor deserializes `Vec<Pubkey>` for maintenance instructions: Solana's max heap is 256 KiB, and 1000 + 1000 entries leaves us at ~128 KiB used. Past ~1500 + 1500, `acl_record_*` / `acl_remove_*` panics with `memory allocation failed, out of memory`.

2. **The architectural ceiling is much higher and structural, not judgmental.** Per-address upper bound on `owned + controlled` is bounded by total ANT supply, which is bounded by `NameRegistry::MAX_NAMES = 50,000` (and we expect to remove that ceiling in a future scaling pass). The 1000 cap is 2 % of today's structural ceiling.

3. **The `Vec<Pubkey>` shape forces a heap-frame planner into every SDK ACL call.** `sdk/src/solana/acl-maintenance.ts` already prepends a `requestHeapFrame` instruction sized to the largest ACL it's about to load, just to keep the existing 1000+1000 cap working under the migration-imported test fixture. That's complexity the SDK should not be carrying.

We also clarified the access pattern: the ACL is consumed almost exclusively by frontends asking *"give me every ANT this address can manage"*. That's a Pattern C "true reverse-relationship" surface in the [Account Scaling Patterns](./ACCOUNT_SCALING_PATTERNS.md) taxonomy — the natural shape is a paginated head + pages addressed by integer index, not a bounded `Vec`.

Finally, since the original implementation has not yet been deployed to mainnet, we can rewrite the on-chain shape directly without an account-level migration.

### Decision

Replace the single `AntAcl` account with **a per-user paginated structure**: one `AclConfig` head + N `AclPage` accounts indexed by integer page id. The paginated design:

- Removes the protocol-level cap on ACL size (entries scale to the architectural ceiling; only per-page rent and CU bound writes).
- Eliminates the SDK heap-frame planner — each maintenance instruction touches at most one `AclPage` (~8.5 KiB), comfortably inside the default 32 KiB heap.
- Reads exclusively via `getAccountInfo` + `getMultipleAccountsInfo` — no `getProgramAccounts` dependency, no DAS, no foundation indexer.
- Encodes the `Owner` / `Controller` (and future) relationship as a single byte per entry, so future relationships (e.g. `UndernameOwner`) can be added by reserving codepoints without changing on-chain schema.
- Uses `u64` for all counts and page indices, matching the [Account Scaling Patterns](./ACCOUNT_SCALING_PATTERNS.md) convention. There is no protocol-level page-count cap.

### Account shape

```rust
// Seeds: ["acl_config", user.as_ref()]
#[account]
pub struct AclConfig {
    pub user: Pubkey,        // 32 — the wallet this ACL belongs to
    pub page_count: u64,     // 8  — number of allocated AclPage accounts
    pub total_entries: u64,  // 8  — sum across all pages (informational; SDK invariant)
    pub bump: u8,            // 1
}
// SIZE = 8 (disc) + 32 + 8 + 8 + 1 = 57 bytes

// Seeds: ["acl_page", user.as_ref(), &page_idx.to_le_bytes()]   (8-byte LE)
#[account]
pub struct AclPage {
    pub user: Pubkey,           // 32 — denormalized for Anchor account validation
    pub page_idx: u64,          // 8
    pub entries: Vec<AclEntry>, // ≤ MAX_ACL_PAGE_ENTRIES
    pub bump: u8,               // 1
}
// HEADER = 8 + 32 + 8 + 4 + 1 = 53 bytes; entries grow via realloc on each push.

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq)]
pub struct AclEntry {
    pub mint: Pubkey,  // 32 — MPL Core asset
    pub role: u8,      // 1  — see AclRole
}

#[repr(u8)]
pub enum AclRole {
    Owner = 0,
    Controller = 1,
    // Reserved: 2 = UndernameOwner, 3 = UndernameController, ... future relationships
}
```

**Page sizing.** `MAX_ACL_PAGE_ENTRIES = 256` puts a fully-loaded page at 53 + 256 * 33 ≈ 8.5 KiB — well under Solana's 10 KiB per-tx realloc limit, and well under the 32 KiB default BPF heap so maintenance ixs don't need a `requestHeapFrame` shim. 256 is a starting point; it can be raised in a future ADR if the realloc / heap budgets prove generous in practice.

### Instructions (`ario-ant`, all permissionless)

| Instruction | Effect | Validates |
|---|---|---|
| `register_acl_config(user)` | Init `AclConfig` head | n/a |
| `add_acl_page()` | Init `AclPage` at `config.page_count`, increment | seeds match `config.page_count` |
| `record_acl_owner()` | Append `(asset, Owner)` to provided page | MPL Core asset owner == `config.user` |
| `record_acl_controller()` | Append `(asset, Controller)` to provided page | `AntControllers.controllers` contains `config.user` |
| `remove_acl_owner()` | `swap_remove` matching entry from page | MPL Core asset owner != `config.user` |
| `remove_acl_controller()` | `swap_remove` matching entry from page | `AntControllers.controllers` does NOT contain `config.user` (or controllers PDA absent) |
| `close_acl_page()` | Close empty page IFF it's the last page | `entries.is_empty() && page_idx == page_count - 1`; refunds rent to beneficiary, decrements `page_count` |
| `close_acl_config()` | Close empty head | `page_count == 0`; refunds rent to beneficiary |

All `*_acl_*` instructions require both `AclConfig` (read-only, identifies the user) and `AclPage` (mutated, target of write). The SDK is responsible for choosing which page to write to (append rule) or which page contains the entry to remove (off-chain scan).

### SDK / orchestration policy

The SDK [`acl-maintenance.ts`](../sdk/src/solana/acl-maintenance.ts) planner is responsible for:

1. **Append placement.** Read the head; iterate pages from `0..page_count` and pick the first non-full page. If all full, prepend `add_acl_page()`. This keeps pages densely packed even under churn (any hole created by a `remove` is the next append target).
2. **Remove targeting.** Off-chain scan: read all pages via `getMultipleAccountsInfo`, locate the (mint, role) entry, and pass the containing page to `remove_acl_*`. Worst case: `O(page_count)` accounts read, ~`page_count / 100` batched calls.
3. **Rent reclamation.** When a remove leaves the last page empty, optionally call `close_acl_page` to refund the page's rent. Other empty pages are not auto-closed — they're the next append target.
4. **Eager reconciliation.** In-SDK transfers / `add_controller` / `remove_controller` / `transfer` / spawn bundle `record_acl_*` and `remove_acl_*` ixs in the same transaction as the canonical write, so the ACL never lags for SDK-mediated operations. External transfers are recovered via the explicit "Claim ANT" UI flow + permissionless cleanup ixs (the ADR-011 drift-handling policy still applies).

### Frontend lookup pattern

```typescript
// Read flow — no getProgramAccounts, no DAS, no foundation indexer:
const config = await rpc.getAccountInfo(deriveAclConfig(user));
if (!config) return { Owned: [], Controlled: [] };
const { page_count } = decodeAclConfig(config.data);
const pagePdas = await Promise.all(
  Array.from({ length: Number(page_count) }, (_, i) =>
    deriveAclPage(user, BigInt(i)),
  ),
);
const pages = await rpc.getMultipleAccountsInfo(pagePdas);  // ~page_count / 100 batches
const entries = pages.flatMap(p => p ? decodeAclPage(p.data).entries : []);
const owned = entries.filter(e => e.role === 0).map(e => e.mint);
const controlled = entries.filter(e => e.role === 1).map(e => e.mint);
```

For a typical user with a few hundred ANTs (single-digit page count), this is one round-trip after the head read.

### Rationale

**Why paginated head + pages (Pattern C from [`ACCOUNT_SCALING_PATTERNS.md`](./ACCOUNT_SCALING_PATTERNS.md)) over per-entry CDPDA?**

| | Paginated (this ADR) | Per-entry CDPDA |
|---|---|---|
| Rent (200 ANTs typical) | ~57 + 8500 ≈ 8.5 KiB ≈ $0.30 | 200 × ~73 B ≈ $0.74 |
| Rent (50,000 ANTs ceiling) | ~57 + 196 × 8500 ≈ 1.6 MiB ≈ $58 | 50,000 × ~73 B ≈ $185 |
| Append CU | ~5–10k (1 page + 1 head) | ~5k (1 PDA init) |
| Enumeration RPC | 1 head + `page_count / 100` batches | 1 indexed `getProgramAccounts` (provider-dependent) OR `page_count / 100` if we maintain an enumeration index |
| Public RPC reach | `getAccountInfo` + `getMultipleAccountsInfo` only | requires indexed `getProgramAccounts` (Helius / Triton custom indices) |
| Adds DAS / indexer dep | No | Yes |

The paginated shape matches the access pattern (read-all enumeration is the dominant query) and stays on the `getAccountInfo` / `getMultipleAccountsInfo` diet, which is reachable from any base RPC.

**Why a unified `role: u8` per entry rather than separate `owned` / `controlled` lists?**

- Adding new relationships (UndernameOwner, UndernameController, future delegations) becomes a codepoint reservation rather than a schema change.
- Halves the number of per-page slots that need to be sized — one shared `entries: Vec<AclEntry>` per page instead of two parallel `Vec<Pubkey>`s.
- Reads can filter by role client-side; writes target a single list. CU is unchanged.

**Why `u64` everywhere instead of `u16` / `u32` for counts and page indices?**

User directive: assume the need for `bigint` so we don't have to migrate the schema again when the structural ceiling lifts. The cost is ~4 bytes per head + ~6 bytes per page seed encoding (8-byte LE vs 2-byte LE) — negligible. Adopted as a project-wide convention for scaling-related counts and indices going forward.

**Why `swap_remove` (with holes tolerated until next append) rather than cross-page swap-with-last on every remove?**

- Single-page touch keeps each maintenance ix at one `AclPage` account, avoiding a second realloc + signer-seed dance.
- Density is preserved by the SDK's "append to first non-full page" rule — any hole becomes the next append target.
- Net-removal scenarios may leave sparse pages, but `close_acl_page` lets the user reclaim rent on the last page when it empties. Mid-list compaction can be added later if it becomes an operational concern.

**Why no reverse-lookup index (mint → owners)?**

Considered: an `AclReverse` PDA per `(mint, role)` listing every address whose ACL contains that entry, enabling a third-party reconcile to skip the off-chain scan.

| | With reverse index | Without (chosen) |
|---|---|---|
| Marginal rent at 50k ceiling | +50k × ~73 B × 2 roles ≈ ~$370 globally | $0 |
| Marginal write CU per `record_acl_*` | +1 PDA init / append | 0 |
| Reconcile RPC | 1 read | `page_count / 100` batches off-chain scan |
| Permissionless reconcile ix needs | (mint, role) hint | (mint, role, page_idx) hint |

Skipped: the reverse index adds rent and CU on every write to save one batched RPC call on a third-party reconcile that runs only when an ACL goes stale. SDK-eager close + UI-flagged staleness + permissionless cleanup covers the operational need without the global rent overhead.

**Subtle UX point on rent reclamation.** With swap-with-last + last-page-only close, rent is refunded **only** when an entire page becomes empty *and* it's the last page *and* `close_acl_page` is called. Per-entry removal does not refund rent — the page still occupies the data length its grow-to-256 watermark established. This is acceptable because pages are bounded (~8.5 KiB max) and most users will have one or two pages total; the refund mechanism exists primarily for users who close out their entire ACL.

### Alternatives considered

1. **Bigger Vec + bigger heap**: keep ADR-011's shape, raise the cap to e.g. 5,000 + 5,000, prepend `requestHeapFrame(256 KiB)` on every ACL ix. Rejected — pushes the binding constraint from the heap to the realloc limit (~10k entries per side per tx), still has a hard cap, and forces SDK heap planning forever.
2. **Per-entry CDPDA + indexed `getProgramAccounts`**: relies on a custom RPC index from Helius / Triton. Rejected — adds a third-party dependency for a base read pattern; rent for per-entry PDAs becomes 2-3x worse at the structural ceiling (per the table above).
3. **Hash-bucketed enumeration (Pattern B from the taxonomy)**: a fixed bucket count keyed by `hash(mint)` prefix. Rejected — Pattern B is for content-derived non-enumerable keys (e.g. ANT undernames). The ACL key is already enumerable by integer page index, so Pattern C is the natural fit.
4. **Cross-page swap-with-last on every remove**: keeps pages perfectly dense at the cost of two `AclPage` accounts on every remove ix. Rejected for v1 — adds CU + complexity for a density gain that the SDK's append rule largely captures already. Can revisit if mid-list compaction proves necessary.

### Consequences

- **Replaces `ario-ant` ACL surface.** `AntAcl` account, `register_acl`, `acl_record_owner`, `acl_record_controller`, `acl_remove_owner`, `acl_remove_controller`, `close_acl` are removed. New surface: `AclConfig`, `AclPage`, `register_acl_config`, `add_acl_page`, `record_acl_owner`, `record_acl_controller`, `remove_acl_owner`, `remove_acl_controller`, `close_acl_page`, `close_acl_config`.
- **PDA seed renames**: `["ant_acl", address]` → `["acl_config", user]` + `["acl_page", user, page_idx_le_bytes_8]`.
- **Constants removed**: `MAX_ACL_OWNED`, `MAX_ACL_CONTROLLED`. **Added**: `MAX_ACL_PAGE_ENTRIES = 256`.
- **SDK simplification**: `acl-maintenance.ts` heap-frame planner deleted; planner now reads the head + last page (or scans pages on remove) and emits `add_acl_page` + `record_acl_*` / `remove_acl_*` ixs. `ant-registry-readable.ts` reads via `getAccountInfo` (head) + `getMultipleAccountsInfo` (pages).
- **Migration tooling**: the ADR-011 `backfill-acl.ts` / `verify-acl.ts` snapshot patchers were removed once Phase 2 / Pass 3 of `migration/import` started emitting the paginated ACL ixs (`register_acl_config` + `add_acl_page` + `record_acl_*`) inline during the import run. Captured fixtures already include `AclConfig` + `AclPage` accounts; no separate backfill step is needed and no on-chain migration ix is required.
- **Integration tests**: `contracts/programs/ario-ant/tests/integration.rs` ACL section needs full replacement.
- **No on-chain account migration.** Initial implementation is being iterated; existing localnet / devnet `AntAcl` accounts (if any) are throwaway. Production deployment uses the new shape from genesis.

### Security considerations

- **Asset / controller verification on every record/remove.** Same model as ADR-011 — `record_acl_owner` reads the MPL Core asset and verifies `owner == config.user` before pushing; `record_acl_controller` reads `AntControllers` and verifies `config.user` is listed. `remove_*` instructions verify the inverse, so cleanup is safe to run permissionlessly.
- **No protocol-level cap means no DoS-via-fill.** ADR-011's `MAX_ACL_*` caps were defensive against "attacker fills victim's list with their own ANTs" — replaced here by the cost-asymmetric defense: the attacker pays per-page rent and per-write CU; the victim can self-remove via `remove_controller` on the attacking ANT followed by `acl_remove_controller`. Cost imbalance still favors the defender.
- **Page-account validation.** Anchor's PDA seed binding ensures a given `AclPage` belongs to exactly one `(user, page_idx)` pair; impossible to splice a page from another user's ACL into a write.
- **`add_acl_page` is the only ix that grows page count.** Verified via seed binding: the new page's `page_idx` must equal the current `page_count`, and `page_count` is `+= 1` atomically. Concurrent `add_acl_page` calls naturally conflict at the seed level.
- **`close_acl_page` only allows last-page closure.** Prevents middle-page hole creation and keeps `page_count` an honest upper bound on `page_idx`.
- **Reentrancy:** none — Solana doesn't have reentrancy in the EVM sense.

---

## Template for New Decisions

```markdown
## ADR-011: ArNS Rebrand and Multi-Protocol Resolution

**Date:** 2026-04-20
**Status:** Proposed
**Deciders:** AR.IO Team

### Context

The migration from AO to Solana changed the identity model: ArNS management now requires a Solana wallet (ANT NFT holder). Arweave wallets can no longer own or manage names. This creates a contradiction — a system called "Arweave Name System" that doesn't support Arweave identities.

Separately, tying the name system exclusively to Arweave storage limits the network's addressable market. IPFS alone has orders of magnitude more content than Arweave. Other decentralized storage protocols (Filecoin, future others) represent additional content that AR.IO gateways could serve.

### Decision

1. **Rebrand ArNS from "Arweave Name System" to "AR.IO Name System."** The acronym ArNS is unchanged. The `.ar` is not used as a TLD — resolution uses protocol-prefixed URIs (`ar://name`, `ipfs://name`) routed through AR.IO gateways.

2. **Extend the ANT record model to support multi-protocol content resolution.** ANT records will support a `protocol` field (defaulting to `arweave`) enabling names and undernames to point to content on any supported decentralized storage network.

3. **Extend the observation/incentive protocol to verify multi-protocol resolution.** Observers will prescribe content checks across all protocols a gateway has opted into. Gateways must correctly resolve content from each protocol to pass observation and earn rewards.

4. **IPFS is the first additional protocol target.** Arweave resolution remains the default and primary protocol.

### Rationale

**Why rebrand:**
- ArNS is an AR.IO protocol feature — gateways resolve names, observers validate, operators stake. The name system serves the AR.IO network, not Arweave directly.
- Arweave is the storage backend; Solana is the ownership layer; AR.IO is the network that ties them together. "AR.IO Name System" is architecturally honest.
- Eliminates the identity contradiction — managing your domain with a Solana wallet is natural when it's an AR.IO domain, not an "Arweave" domain.
- The acronym ArNS, the `ar://` protocol prefix, and all on-chain structures (PDA seeds, account names) remain unchanged.

**Why multi-protocol:**
- Positions AR.IO as the incentivized resolution network for decentralized storage, not just Arweave's CDN.
- Massively expands addressable content and user base (IPFS adoption dwarfs Arweave).
- Strengthens the economic flywheel — more protocols → more content → more name purchases → more demand factor → more ARIO utility.
- AR.IO gateways already run as infrastructure nodes; adding IPFS fetch capability is a natural extension.
- No existing user is affected — Arweave resolution is unchanged and remains the default.

**Why IPFS first:**
- Largest existing content library in decentralized storage.
- Every web3 developer already has IPFS content.
- Content-addressed (CIDs) — straightforward to verify in observation.
- Gateway integration is well-understood (many existing IPFS gateway implementations).

**Alternatives considered:**
- *Accept Solana-only, no rebrand* — Simplest, but limits narrative and alienates Arweave community.
- *Build Arweave signature verification on Solana* — Only solves identity for Ed25519 Arweave wallets (minority use RSA). Doesn't address the bigger multi-protocol opportunity.
- *Dual-chain AO+Solana operation* — Enormous complexity, defeats purpose of migration.

### Consequences

- Docs, marketing, comms, SDK help text: rename "Arweave Name System" → "AR.IO Name System" everywhere.
- ANT record schema extends with a `protocol` field (non-breaking — defaults to `arweave`).
- Gateway software gains multi-protocol fetch capability (Arweave first, IPFS second, others later).
- GAR may track per-gateway protocol capabilities (what each operator has opted into).
- Observer/prescriber logic extends to test resolution across multiple protocols.
- The Arweave identity concern is resolved by reframing: Solana wallet = domain registrar account, Arweave address = content authorship / linked profile metadata.
- No contract code changes required for the rebrand itself. Multi-protocol ANT records and gateway capabilities are future engineering work.

---

## ADR-012: ANT NFT Marketplace Metadata Architecture

**Date:** 2026-04-24
**Status:** Accepted
**Deciders:** AR.IO Team

### Context

ANTs are Metaplex Core NFTs. For marketplace display (Magic Eden, Tensor) and wallet rendering (Phantom, Solflare), NFTs need metadata: a name, image, and queryable traits. Two approaches exist:

1. **Off-chain JSON only** — Upload metadata JSON to Arweave, point the Core asset `uri` to it. Traits in the JSON `attributes` array.
2. **On-chain Attributes plugin** — Store traits directly on the Core asset via Metaplex Core's Attributes plugin. DAS-indexed, queryable, updateable without re-uploading JSON.

Key factors: ANTs have traits that change over time (Undername Limit increases, Type upgrades from lease to permabuy), and marketplace filtering by ArNS name/type is a strong user expectation. Zero ANTs have been minted on Solana yet — clean-slate opportunity.

### Decision

**Use both, with clear roles:**

- **Attributes plugin** (on-chain) for DAS-queryable traits: ArNS Name, Type (lease/permabuy), Undername Limit. Authority set to `Owner` so the current ANT NFT holder can sign trait updates directly.
- **Static metadata JSON** (on Arweave) for display: name, symbol, image, description. No traits in the JSON — they live on-chain.
- **Trait sync via CPI**: every ARIO-ARNS handler that mutates trait-affecting state (`buy_name`, `buy_returned_name`, `upgrade_name`, `increase_undername_limit`, `reassign_name`, `release_name`) CPIs into MPL Core's `UpdatePluginV1` to keep the on-chain plugin coherent with the source-of-truth `ArnsRecord` PDA. A permissionless `sync_attributes` instruction reconciles cases where the buyer/recipient was not the ANT owner at the time of the originating mutation.

At mint time (migration and post-migration), the Core asset is created with the Attributes plugin populated inline in the CreateV1 instruction.

### Rationale

- On-chain attributes are always current — no stale JSON problem when traits change
- DAS-indexed: marketplaces can filter "show all permabuy ANTs", sort by undername limit
- Updateable on-chain without Arweave re-upload — `increaseUndernameLimit` etc. update the attribute via in-handler CPI
- Plugin authority `Owner` (vs `UpdateAuthority`) decouples trait management from the asset's update-authority lifecycle: no need to transfer update authority during claim, the user owns the asset → they own the plugin
- Hand-rolled CPI (see `programs/ario-arns/src/mpl_core_cpi.rs`) avoids the `mpl-core` Rust crate, which doesn't compile on the Cargo 1.79 bundled with Solana 2.1.0 SBF tools
- Clean slate: no legacy assets to retrofit
- Cost is negligible: ~0.00125 SOL additional rent per ANT (~62.5 SOL for 50K ANTs)

### Consequences

- Migration minting (`phase2-ants.ts` + `migration/import/src/instructions/mint-nft.ts`) **always** serializes an Attributes plugin in CreateV1 with `authority=Owner`. The list is populated for ANTs that have an ArNS record at migration time, empty otherwise. This guarantees every migrated ANT — including "orphans" with no ArNS association — is `purchase`-ready post-cutover.
- Post-cutover ANT minting via the SDK (`spawnSolanaANT` in `sdk/src/solana/spawn-ant.ts`, exposed through `ANT.spawn` and the `spawn-ant` CLI) emits the same shape: empty Attributes plugin, Owner authority. Byte-pinned by `sdk/src/solana/spawn-ant.test.ts` against the migration mint's known-good fixtures.
- `buildCreateAntInstruction` is exported from `@ar.io/sdk/solana` for advanced callers who want to bundle the mint into a larger compound transaction.
- Snapshot produces `ant-nft-metadata.json` sidecar cross-referencing ANT state with ArNS records
- All 6 trait-mutating ARIO-ARNS handlers gained `mpl_core_program` and `system_program` accounts; `ant_asset` flipped from read-only to writable
- New `sync_attributes` instruction is the canonical recovery path for `purchase`/`reassign_name` cases where buyer ≠ ANT owner (CPI is skipped at runtime in those cases; the ANT owner can later reconcile permissionlessly)
- Cargo integration tests load a dumped `mpl_core.so` (committed at `programs/ario-arns/tests/fixtures/mpl_core.so`) and mint real ANTs via CreateV1 in setup; `BPF_OUT_DIR` must be set when running `cargo test -p ario-arns`
- The `AntConfig` PDA remains the source of truth for extended ANT metadata (name, ticker, logo, description, keywords); the Attributes plugin is a marketplace projection of selected ArNS traits
- Update Authority lifecycle for ANTs (who can patch metadata post-claim, etc.) is decided separately in ADR-013

---

## ADR-013: ANT Authority Lifecycle (Owner / UpdateAuthority / Plugin / Program)

**Date:** 2026-04-28
**Status:** Accepted
**Deciders:** AR.IO Team

### Context

Metaplex Core assets carry three independent authorities — Owner, Update Authority, and per-plugin Plugin Authority — and the AR.IO ANT design adds a fourth concern at the program level (BPFLoaderUpgradeable's program upgrade authority). `TransferV1` only changes Owner; the others stay where they were set at mint.

This matters most during the AO → Solana migration claim flow. Migrated ANTs are minted by the migration authority (so it transiently holds Owner, UpdateAuthority, and Plugin Authority). On claim, `TransferV1` moves Owner to the user but leaves UpdateAuthority with the migration authority by default. Without an explicit decision, users would land claimed ANTs with stealth-UA retained by AR.IO indefinitely — and we want a deliberate answer about whether that's the design or an accident.

A separate but adjacent concern is "what happens when AR.IO needs to ship a patch?" The four kinds of patches (program code fix, per-ANT account schema migration, per-ANT metadata fix, per-ANT trait fix) each gate on a different authority. Pinning down who controls each closes the question.

### Decision

**1. UpdateAuthority transfers to the user on claim.** The migration claim flow's `transferNft` is extended to issue `UpdateV1(newUpdateAuthority = user)` in the same transaction as `TransferV1`. After claim, migrated ANTs end up with the same authority shape as freshly-spawned ANTs: Owner = user, UpdateAuthority = user, Plugin Authority = `Owner` (= user). No stealth-UA at AR.IO.

**2. Post-cutover spawned ANTs already had this shape.** `spawnSolanaANT` in the SDK has always set `authority = signer.address` for CreateV1, which makes Owner and UpdateAuthority both default to the signer (the user). Confirmed in `sdk/src/solana/spawn-ant.ts`. No code change needed for that path.

**3. Program upgrade authority stays with AR.IO (BPFLoaderUpgradeable, not `--final`).** All four AR.IO programs are deployed upgradeable so AR.IO can ship code-level patches. The devnet test deploy used `--final` to halve the rent (~3.65 SOL vs ~7.3 SOL), but production deploys must NOT use `--final`. Captured in `docs/DEVNET_RUNBOOK.md`.

**4. Per-ANT account schema migrations use `migrate_ant`** (already designed in ADR-012's consequences). That instruction is permissionless — anyone can pay the realloc rent — so neither user nor AR.IO holds a privilege gate here, only the program-level upgrade authority that ships the new schema.

### Patch-layer reference

| Patch type | Mechanism | Authority required | Who has it post-claim |
|---|---|---|---|
| **Program code** (e.g. fix bug in `set_record`) | BPFLoaderUpgradeable program upgrade | Program upgrade authority | AR.IO |
| **Per-ANT account schema** (e.g. add field to AntConfig) | `migrate_ant` instruction (lazy realloc) | Permissionless | Anyone |
| **Per-ANT metadata** (e.g. fix typo, move JSON host) | UpdateV1 (re-upload + URI swap) | Update Authority | User |
| **Per-ANT traits** (e.g. fix Type/Undername Limit value) | UpdatePluginV1 (or `sync_attributes`) | Plugin Authority (= Owner) | User |

### Rationale

- **Sovereignty parity** between migrated and spawned ANTs. Users shouldn't get a different authority shape based on whether their ANT came from AO migration or was minted later. Enforcing parity at the claim boundary is the cheapest way to achieve this (one extra CPI, atomic with the transfer).
- **AR.IO retains all the structurally important patch capabilities** (program upgrade, schema migrations) without holding any per-ANT privilege. The cost of giving up UA is narrow: AR.IO loses the ability to batch-fix metadata across all 50K migrated ANTs. But the metadata-fix operation is also doable by individual users with their own Solana wallet (we've validated free Turbo uploads via `HexSolanaSigner`), so each user can self-recover.
- **Trust minimization at low cost.** Stealth-UA over 50K user-owned ANTs is a meaningful surface area for the AR.IO migration authority — a compromise of that key would let an attacker rewrite metadata for every ANT. Transferring UA at claim time eliminates that key from being a long-lived blast radius for migrated ANTs.
- **Conventional with NFT-collection norms.** Most modern NFT collections give the holder UpdateAuthority once the asset is sold/minted to them. Creators usually keep authority on the *collection* itself — but ARIO ANTs are standalone Core assets (no collection), so there's no collection-level lever AR.IO would have wanted to keep anyway.

### Consequences

- **Migration claim flow change:** `migration/import/src/claim-transfers.ts::transferNft` extended to bundle `UpdateV1(newUpdateAuthority = newOwner)` with the existing `TransferV1`. Atomic in one tx.
- **AR.IO can no longer batch-patch metadata** across migrated ANTs once they're claimed. Documented user-facing recovery path: re-upload JSON to Arweave (free <100KiB via Turbo) and call `UpdateV1` from your wallet. The SDK's `buildAntMetadata` helper is the canonical JSON shape.
- **Production deploy must use upgradeable programs.** The devnet harness's `--final --max-len <exact>` shortcut is a test-only pattern; the production runbook (`docs/DEVNET_RUNBOOK.md`) must keep BPFLoaderUpgradeable and a multisig'd upgrade authority.
- **No on-chain program changes** required for this ADR. The change is entirely in the migration import client (`claim-transfers.ts`), so it's hot-swappable per-claim and doesn't gate any program upgrade.
- BD-096 cross-references this ADR for the post-claim authority shape.

---

## ADR-014: Trustless ANT Escrow Program (Multi-Protocol Signature Verification)

**Date:** 2026-04-29
**Status:** Proposed
**Deciders:** AR.IO Team

### Context

The migration claim flow custodies ANTs in a foundation-controlled wallet and releases them to claimants based on Arweave/Ethereum-signed attestations posted to Arweave (verified off-chain by Turbo's bundler). This works for the migration but commits us to an off-chain attestation pattern for any future "Arweave wallet → Solana wallet" custody flow.

The original ADR-004 ruled out on-chain RSA verification because Solana didn't have the necessary big-integer arithmetic syscalls. That changed: Solana now ships `sol_big_mod_exp` (modular exponentiation for arbitrary-precision integers, ~10K CU per call for 4096-bit RSA) and has long had `secp256k1_recover` as a precompile. Both syscalls are stable and live on mainnet.

This means we can build a **fully trustless** ANT escrow program — no foundation authority, no oracle, no off-chain attestation crank — by verifying Arweave RSA-PSS-4096 and Ethereum ECDSA secp256k1 signatures directly on-chain.

### Decision

**Build `ario-ant-escrow`** as a standalone Solana program with:
- One PDA per escrowed ANT (seeds: `["escrow_ant", ant_mint]`)
- Multi-protocol recipient identity (Arweave RSA-PSS-4096 or Ethereum ECDSA secp256k1)
- Indefinite custody — no timeouts
- Depositor controls via `cancel_deposit` and `update_recipient`
- Claim via on-chain signature verification, no authority required
- Canonical message format reconstructed from accounts (no client-supplied message bytes), eliminating message-malleability attacks

Network distinction (`solana-mainnet` / `solana-devnet`) is baked into the program at compile time to prevent cross-network replay.

### Rationale

- **Trustless > authority-mediated.** A fully on-chain verifier is materially better than a foundation-controlled release gate; this should be the default custody pattern post-migration.
- **Solana platform now supports it.** `sol_big_mod_exp` makes RSA-PSS-4096 verification cheap (~30-60K CU). `secp256k1_recover` makes ECDSA cheap (~30-40K CU). Both fit in default per-instruction CU budget.
- **One program, two identity surfaces.** Covers the dominant Arweave and EVM identity stacks in one design; Solana → Solana (Ed25519) trivially extensible if needed.
- **No timeout = simplest mental model.** Depositor retains revocability via `update_recipient` and `cancel_deposit`, so we don't need timeout-based recovery semantics. Permanent ANTs (permabuy ArNS) are forever, which matches indefinite escrow semantics.

### Consequences

- New program adds ~661 bytes per active escrow (~$0.67 rent, refunded on claim/cancel/redirect).
- Auditing burden: PSS implementations are historically a CVE source. Audit must include differential testing against `arweave-js`, RustCrypto, OpenSSL, and NIST CAVP vectors.
- Operational responsibility: maintain published canonical-message-format spec so external integrators can produce signatures without reading the source.
- ADR-004's off-chain attestation pattern remains in use for the migration claim flow (already shipping); this ADR is forward-looking, not retroactive.
- Future: a `lock_deposit` extension could enable atomic-swap-style flows by making escrows irrevocable. Out of v1 scope.

### Reference

- Full technical design: `docs/ANT_ESCROW_DESIGN.md`. Includes account model, instruction set, canonical message format (binary spec), cryptographic verification pseudocode, threat model, performance budget, and rollout plan.
- Implementation plan: `docs/ANT_ESCROW_IMPLEMENTATION_PLAN.md`. Phase-by-phase breakdown with concrete file paths, acceptance criteria, parallelism map, and sign-off checklist for mainnet.

---

## ADR-015: Mint Authority Revoked at End of Migration

**Date:** 2026-04-29 (revised same day to drop atomicity overspec)
**Status:** Proposed
**Deciders:** AR.IO Founder

### Context

The ARIO SPL Token has a fixed total supply of 1 B as specified in the v2.1 whitepaper. None of the AR.IO programs (ario-core, ario-gar, ario-arns, ario-ant, ario-ant-escrow) contain `MintTo` calls — verified by `grep -rEn "token::mint_to|MintTo" contracts/programs/*/src/ | grep -v test` returning empty. Migration imports populate per-protocol PDAs (Balance / Vault / Gateway / Delegation / etc.); the SPL tokens themselves live in shared accounts (`protocol_token_account`, `stake_token_account`), the migration authority's ATA (escrow for unclaimed user balances), and per-vault `vault_token_account`s.

The original enshrinement plan (`docs/ENSHRINEMENT_PLAN.md` §1, ADR-010 era) called for a *post-deploy* revocation at "T+1m" — a soft target with no specific runbook owner. This left both the timing and the responsibility ambiguous.

A revised draft of this ADR proposed an *atomic* genesis tx (init + mint + revoke in one transaction), but that didn't survive contact with the migration architecture: the snapshot's `supplyData` splits the 1 B across at least four destination categories, including per-vault token accounts that number in the hundreds-to-thousands. A single atomic tx is not feasible.

### Decision

**Mint authority is held by the Squads multi-sig from genesis through the end of the migration import phase, then revoked permanently as the final step of `phase6-finalize`. Revocation is non-atomic with mint creation but is a non-optional, deterministic step in the migration runbook.**

Lifecycle:

1. **Phase A (deploy):** Mint created with authority = Squads multi-sig, freeze = `None`. 1 B minted across destinations as required by the snapshot's `supplyData` (`protocolBalance` → `protocol_token_account`, `stakedSupply + delegatedSupply + withdrawSupply` → `stake_token_account`, unclaimed `circulatingSupply` → migration authority's ATA, `lockedSupply` → per-vault `vault_token_account`s).
2. **Phase B (migration imports):** Phases 1-5 write PDAs. As needed, additional `MintTo` or `Transfer` txs fund any remaining destinations (notably per-vault token accounts). Multi-sig signs.
3. **Phase C (finalization, in `phase6-finalize.ts`):**
   - Step 1: `finalize_supply` on ario-core (bookkeeping)
   - Step 2: `migration/verify` suite confirms sum invariant: total SPL across all destinations = 1 B
   - Step 3: `finalize_migration` on all four programs (hot key becomes inert)
   - **Step 4 (NEW):** `SetAuthority(mint, None)` signed by multi-sig. Mint authority permanently `None`.
   - Step 5: Post-tx assertions: `mintInfo.mintAuthority === null && mintInfo.freezeAuthority === null && mintInfo.supply === 1_000_000_000_000_000`.

After Phase C step 4 confirms, no further minting is possible — ever. `WHITEPAPER_SOLANA_CHANGES.md #38` closes at this point with the SetAuthority tx signature as evidence.

Full runbook: `docs/MINT_GENESIS.md`.

### Rationale

- **Closes the loop.** The original ADR-010 plan left mint-authority revocation as a soft "T+1m or so" cleanup. This ADR makes it a non-optional step of the migration runbook itself, owned by `phase6-finalize.ts`. Phase 6 isn't "complete" until step 4 + step 5 assertions pass.
- **No minting requirement, ever.** Confirmed 2026-04-29 by founder. The protocol's economic model is fixed-supply by design; the whitepaper is unambiguous; all five programs ship without mint code. No future feature is anticipated to require minting.
- **Fits the migration architecture.** Atomic-genesis was incompatible with multi-destination supply distribution (per-vault token accounts in particular). A non-atomic but deterministic revocation step at the end of phase 6 is materially simpler and works with the existing migration phase structure.
- **Closes whitepaper item #38 deterministically.** `docs/WHITEPAPER_SOLANA_CHANGES.md #38` asks "Confirm whether the mint authority has been permanently revoked." Phase 6 step 4 answers this with the SetAuthority tx signature.
- **Eliminates one of six enshrinement surfaces.** The remaining five surfaces (in-program authority, migration authority, token account authority, BPFLoaderUpgradeable upgrade authority, ANT update authority) are unaffected. This decision narrows the enshrinement runbook by one entry.

### Alternatives considered

- **Atomic genesis (init + mint + revoke in one tx).** Rejected: incompatible with multi-destination supply distribution. Per-vault token accounts alone require hundreds-to-thousands of separate funding txs; can't fit in one. The complexity of a "split atomic genesis" outweighs its marginal security benefit over the deterministic phase-6 step.
- **Post-deploy revocation as a soft cleanup at T+1m (the original ADR-010 plan).** Rejected: leaves both timing and ownership ambiguous. Replaced by the deterministic phase-6 step.
- **Token-2022 with a permanent-disable mint extension at init.** Rejected: Token-2022 is younger, has thinner wallet/RPC support; the marginal benefit doesn't justify migrating off classic SPL Token.
- **Keep mint authority alive in steady state under a Squads multi-sig forever.** Rejected per founder commitment. The protocol is fixed-supply; live mint authority is a permanent attack surface for no operational benefit.

### Consequences

- **`migration/import/src/phases/phase6-finalize.ts` gets two new steps** (SetAuthority + post-tx assertions) after the existing `finalize_migration` loop. The instruction is constructed via `@solana/spl-token`'s `createSetAuthorityInstruction`.
- **`migration/import/src/devnet-setup.ts` is unchanged behaviorally** — it already creates the mint with a live authority and mints the initial supply. No SetAuthority during setup; that lives in phase 6 only.
- **A new `migration/import/src/mainnet-deploy.ts`** is needed for production (multi-sig as authority, real treasury PDA, etc.).
- **`docs/ENSHRINEMENT_PLAN.md` §1 is replaced** with a one-line reference to `docs/MINT_GENESIS.md`. The vague "T+1m: burn mint authority" line is removed from the enshrinement timeline; revocation is now subsumed by the migration runbook.
- **`docs/WHITEPAPER_SOLANA_CHANGES.md #38` closes** when phase 6 step 4 lands on mainnet. The SetAuthority tx signature replaces the "TBD" placeholder.
- **The 1 B supply is provably immutable** from the moment phase 6 step 4 confirms. Future protocol-upgrade-authority compromise still cannot mint more tokens (mint authority is independent of program upgrade authority). Only a new mint launch + holder migration could alter ARIO supply on Solana.
- **Founder commitment that no future feature will require minting is now load-bearing.** Recorded here so any future "let's mint reward inflation" or "let's mint a governance token from the same mint" proposal is blocked at the design stage by this ADR.

### Implementation gates

Phase 6 step 4 will not run on mainnet until:

- All phases 1-5 of the migration import have completed; phase 6 steps 1, 2, 3 have run cleanly.
- `migration/verify` confirms the sum invariant: SPL across all destinations = 1 B exactly (6-decimal precision).
- Devnet dry-run of the modified `phase6-finalize.ts` (including the new SetAuthority step + post-tx assertions): 3 successful runs.
- Full migration end-to-end on devnet against a multi-destination mint: snapshot → import → phase6 (with revocation) → SDK reads + writes all green.
- Mainnet deploy script (`mainnet-deploy.ts`) reviewed by ≥2 engineers + auditors.
- Squads multi-sig members confirmed for the phase-6 signing ceremony with backup signers identified.

### Reference

- Full runbook: `docs/MINT_GENESIS.md` (lifecycle phases, destinations, pre-Phase-C checklist, SetAuthority tx layout, risks, implementation steps).
- Code touched: `migration/import/src/phases/phase6-finalize.ts` (extend with steps 4 + 5); new `migration/import/src/mainnet-deploy.ts`.
- SPL Token reference: https://github.com/solana-labs/solana-program-library/tree/master/token

---

## ADR-016: Pluggable ANT Program via Asset Attributes Plugin

**Date:** 2026-05-01
**Status:** Accepted
**Deciders:** Core protocol team (post-Atticus review)

### Context

ARIO's ANT model lets each name point at a Metaplex Core asset that owns the per-mint state PDAs (controllers, undername records). The canonical implementation lives in `ario-ant`, but third parties want to ship their own Solana programs to manage that state — e.g. a marketplace-specific controller with extra hooks, or a curated profile namespace. Resolvers (SDK, ARIO-CORE BD-097) need to know which program owns a given asset's PDAs so they can derive the right addresses.

PR #40 first explored a registry-side approach: store an `ant_program: Pubkey` on `ArnsRecord`, populated at purchase / reassign time. Reviewer Atticus (2026-04-30) flagged this as a layering inversion: the program choice is intrinsic to the asset (the ANT NFT and its state PDAs), not the name binding. The proposed scheme also bloated `ArnsRecord` by 32 bytes per name and made `reassign_name` semantics complex (two new-program states diverged from one another).

### Decision

Store the program identifier as an entry in the asset's existing **Metaplex Core Attributes plugin**, under the key `ANT Program`, set at mint time.

```
MPL Core asset
└── Plugins
    └── Attributes (authority = Owner)
        ├── ArNS Name        ← already there (per-name)
        ├── Type             ← already there (per-name)
        ├── Undername Limit  ← already there (per-name)
        └── ANT Program      ← NEW (per-asset)
```

Resolvers read the trait off the asset; absence falls back to the canonical `ARIO_ANT_PROGRAM_ID`. The trait is treated as a best-effort routing hint — every parse failure (truncated buffer, malformed plugin, invalid base58) silently falls back to canonical to avoid griefing the holder out of their own name.

### Rationale

**Why on the asset, not on `ArnsRecord`:**
- One source of truth — the asset is the natural carrier for per-asset metadata.
- No `reassign_name` semantics to think about — the asset's program doesn't change when a different name is reassigned to a different asset.
- DAS / marketplace queryability for free — the same indexer that exposes ArNS Name / Type / Undername Limit also exposes ANT Program.
- Per-record storage stays as it was — no 32-byte hit on `ArnsRecord`.

**Why "Owner" plugin authority is fine:**
The current NFT holder is already the trust boundary for `ArNS Name` / `Type` / `Undername Limit` and can already redirect by minting a fresh asset under any program and calling `reassign_name`. Letting them flip `ANT Program` on the same asset is no weaker.

**Why we don't allowlist programs:**
Resolvers perform structural decoding (PDA derivation + Borsh layout). Third-party programs that match the canonical wire format work; ones that don't return `null` records to the SDK and `InvalidAccountState` to the contracts. No protocol-level approval gate — conformance is on the third party.

**Alternatives rejected:**
- *PR #40 — `ArnsRecord.ant_program`:* layering inversion, per-name bloat, complex reassign semantics. Closed unmerged.
- *Allowlist of programs in `ArioConfig`:* governance bottleneck, opens an upgrade-cadence question we don't need.
- *Drop the feature entirely (single canonical ANT program forever):* viable today (we're pre-genesis, no real users), but cuts off the third-party ANT program path before we've seen if it matters. The asset-side design is cheap enough to ship now and revisit if no one ever uses it.

### Consequences

- **On-chain:** ARIO-CORE's BD-097 path (`read_ant_record_owner`) takes an `ant_program_id: Pubkey` instruction parameter (Sprint 2 reshape — see amendment below). The PDA seeds pin (program_id, ant_mint, undername), so a caller lying about the program id fails the seed check.
- **On-chain:** Sync of the asset's Attributes plugin lives in `ario_ant::sync_attributes` (Sprint 3 reshape). The CPI reads the existing `ANT Program` trait first and re-emits it across every UpdatePluginV1 (which is a whole-list replace) so the override survives trait churn. ARIO-ARNS and ARIO-CORE no longer touch MPL Core directly.
- **SDK:** `SolanaANTReadable.fromAsset(rpc, mint)` async factory looks up the program from the asset and feeds it into the constructor; resolution paths (`requestPrimaryName`, `approvePrimaryName`) use the same lookup before deriving AntRecord PDAs. Mutation paths (`buyName`, `upgradeRecord`, `extendLease`, `increaseUndernameLimit`, `reassignName`, `buyReturnedName`) bundle `ant.sync_attributes` in the same transaction (Sprint 4 reshape).
- **Migration:** every imported ANT carries an explicit `ANT Program: <ARIO_ANT_PROGRAM_ID>` entry — no legacy ANTs without the trait post-cutover.
- **`spawnSolanaANT`:** writes `ANT Program: <antProgramId>` automatically (defaulting to canonical).
- **Documentation:** BD-100 captures the resolver behavior; ARNS / ANT specs note the trait as the routing key.

### Sprint 2-4 amendment (2026-05-04, post-Atticus follow-up)

Atticus's review of the original PR #44 implementation noted that ARIO-CORE and ARIO-ARNS were both doing MPL Core CPIs — a layering smear. The reshape moves all MPL CPI into ARIO-ANT (the program that owns the asset domain), and inverts the sync direction:

- **ARIO-ARNS is now MPL-agnostic.** No `mpl_core_cpi.rs`, no asset deserialization, no `try_sync_attributes` calls inside `buy_name` / `upgrade_name` / etc. The `sync_attributes` instruction is removed from ARIO-ARNS.
- **ARIO-CORE is now MPL-agnostic.** No `mpl_core.rs`, no asset reading. Primary-name authorization (`request_and_set_primary_name`, `approve_primary_name`, `remove_primary_name_for_base_name`, `request_and_set_primary_name_from_funding_plan`) takes `ant_program_id: Pubkey` and uses AntRecord-PDA-based auth (caller == `AntRecord.owner`) with the canonical `"@"` sentinel for base names.
- **ARIO-ANT is the only program touching MPL Core.** New `sync_attributes(name: String)` instruction reads the canonical `ArnsRecord` (typed `ario_arns::state::ArnsRecord` deserialization), validates `record.ant == asset.key()`, preserves the existing `ANT Program` trait, and CPIs `mpl_core::UpdatePluginV1`. Authority must be the asset owner (Owner-authority plugin).
- **SDK orchestrates `arns.<mutate>` + `ant.sync_attributes` in one transaction WHEN the caller owns the ANT.** The SDK pre-checks ownership via `fetchMplCoreOwner` and only bundles the sync ix when the signer is the asset holder. This preserves BD-095 (non-holder ArNS lease management — `extendLease` / `upgradeRecord` / `increaseUndernameLimit` are permissionless) and BD-096 (deferred trait sync for non-holder buyers); the actual ANT owner reconciles state later via the public `syncAttributes()` method. `extendLease` is excluded from bundling entirely — extend_lease changes only `end_timestamp`, which isn't mirrored in any Attributes-plugin trait. `release_name` is also NOT bundled (it closes the ArnsRecord PDA, which would fail sync's existence check).

The asset-side `ANT Program` trait remains the routing key (the original ADR-016 mechanism is unchanged from the resolver's perspective — only the *write path* moved).

### Reference

- Plan: `docs/PLUGGABLE_ANT_PROGRAM_PLAN.md` (V2 — asset-side)
- BD-100: `docs/BEHAVIORAL_DIFFERENCES.md`
- Code touched (post-Sprint-3 reshape):
  - `programs/ario-ant/src/mpl_core_cpi.rs`, `programs/ario-ant/src/lib.rs::sync_attributes` (new home)
  - `programs/ario-ant/Cargo.toml` (typed `ario-arns` cpi dep with `idl-build` propagation)
  - `programs/ario-arns/src/instructions/{purchase,manage,purchase_from_stake}.rs` (MPL CPI removed)
  - `programs/ario-core/src/instructions/primary_name.rs` (MPL asset reads removed; `ant_program_id` parameter on 4 ixs)
  - `sdk/src/solana/io-writeable.ts` (sync bundling, primary-name validation accounts helper)
  - `sdk/src/solana/spawn-ant.ts`, `migration/import/src/phases/phase2-ants.ts` (unchanged — still write `ANT Program` at mint time)

---

## ADR-017: Off-Chain Attestor for Arweave RSA-PSS Signature Verification

**Date:** 2026-05-06
**Status:** Accepted
**Deciders:** AR.IO core engineering

### Context

`ario-ant-escrow` (per ADR-014) verifies signatures over a canonical
claim message to release escrowed assets. Three signature schemes are
supported, one per recipient protocol:

- Ethereum: ECDSA secp256k1 + EIP-191 — verified on-chain via the
  native `secp256k1_recover` syscall (~25K CU, no issues).
- Arweave: RSA-PSS-4096 with SHA-256 + MGF1 — requires modular
  exponentiation of a 4096-bit base.
- Multi-sig (future): TBD per protocol.

The Arweave path turns out to be infeasible to verify fully on-chain
on Solana, by every avenue we tested:

| Modexp implementation | CU per claim | Verdict |
|---|---|---|
| `sol_big_mod_exp` syscall | ~10K CU when active | **Feature gate `EBq48m8irRKuE7ZnMTLvLg2UuGSqhe8s8oMqnmja1fJw` is blocked on every public Solana cluster** with no activation date. Cannot ship today. |
| `dashu` (heap big-int, via `solana-nostd-big-mod-exp` `no-syscall` feature) | 9.7M | ~7× over the 1.4M per-tx limit |
| Hand-rolled CIOS Montgomery (64-bit limbs, `u128` mac) | 17.7M | ~13× over (worse than dashu — BPF `u128` emulation overhead) |
| `crypto-bigint` 0.7.x | n/a | Pulls `cpubits` which requires Rust edition 2024; Solana SBF toolchain bundles 1.79 |
| `crypto-bigint` 0.6.x | n/a | Requires rustc 1.83; Solana SBF toolchain bundles 1.79 |
| `crypto-bigint` 0.5.5 | n/a | Compiles on 1.79 but Montgomery temporaries blow Solana's 4 KB BPF stack frame at runtime (`Access violation`) |
| Multi-transaction split | ~1M per tx × ~20 txs | Workable but adds significant protocol complexity (intermediate state, replay safety, partial-failure recovery) |

The numbers above are measured, not estimated — see test
`measure_cu_claim_arweave` in
`contracts/programs/ario-ant-escrow/tests/integration.rs` and the audit
trail in this PR series.

### Decision

Verify Arweave RSA-PSS-4096 signatures **off-chain** via a small AR.IO-
operated service ("attestor"), and have the on-chain program verify a
re-signed Ed25519 attestation over the SAME canonical message bytes:

1. User signs canonical message with their Arweave wallet (RSA-PSS-4096).
2. User POSTs `{rsa_modulus, rsa_signature, claim_params}` to the
   attestor ([`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor); extracted 2026-05-16 from `solana-ar-io/migration/attestor/`).
3. Attestor verifies the RSA-PSS signature via `node:crypto`
   (OpenSSL-backed, hardware-accelerated, ~5ms per request), then
   signs the SAME canonical message bytes with Ed25519.
4. Attestor returns `{ed25519_pubkey, ed25519_signature, canonical_message}`.
5. User builds a Solana transaction with two instructions:
   - `Ed25519Program` native sigverify ix (verifies the Ed25519 sig,
     ~720 CU, hardware-accelerated)
   - `claim_*_arweave_attested` ix (uses `sysvar::instructions`
     introspection to confirm the preceding Ed25519Program ix verified
     the SAME canonical message under `ATTESTOR_PUBKEY`)
6. The on-chain program reconstructs the canonical message from escrow
   state and confirms it matches the attestor-signed bytes. Then the
   release flow runs (asset transfer + escrow PDA close).

The attestor's Ed25519 pubkey is compiled into the program as the
`ATTESTOR_PUBKEY` constant in
`contracts/programs/ario-ant-escrow/src/state.rs`. Rotation requires
a `BPFLoaderUpgradeable` upgrade swapping the constant.

### Rationale

#### Why an attestor instead of multi-tx claim split?

The multi-tx path is mechanically possible but adds substantial
protocol complexity: intermediate state PDAs, replay-safety per-step,
partial-failure recovery, observability. The attestor approach is
simpler (one HTTPS request + one Solana tx) and the trust delta is
small (see below). Multi-tx remains an option if AR.IO ever wants to
move to a fully trustless Arweave claim path.

#### Why Ed25519 attestation instead of privileged authority signer?

We considered (and rejected for the migration scope) a design where the
attestor holds a Solana keypair and submits claim transactions
itself, with an on-chain `signer == ATTESTOR_AUTHORITY` check
replacing the Ed25519 introspection. Both designs have equivalent
trust models — compromise of either secret allows draining
Arweave-recipient escrows.

We chose Ed25519 introspection because:

- **No hot Solana wallet.** Attestor is offline-from-Solana; cannot
  be drained even partially. The only secret it holds authorises
  exactly one operation type (claim attestation), not arbitrary
  Solana actions.
- **Throughput.** Each user submits their own tx in parallel. A
  privileged-authority design serializes through one signer's nonce,
  bottlenecking peak burst at ~3 tx/sec.
- **Composability.** Users can bundle the claim ix with other ixs
  (e.g., `createAssociatedTokenAccount`) atomically. Authority-
  submitted txs are standalone.
- **Idiomatic Solana.** Mirrors how on-chain Ed25519 / secp256k1
  sigverify works for native message-signing flows. Reduces operator
  surprise during audit.

The privileged-authority design's primary advantage is operational
visibility — AR.IO sees the entire flow through to on-chain confirmation
rather than handing the user a sig and stopping. We close this gap
with a follow-up audit-log + dashboard (cross-references attestations
issued vs `claim_*_attested` txs landed) rather than an architecture
change.

#### Trust model

Compromise of the attestor's Ed25519 secret key allows an attacker to
mint valid attestations and drain ALL Arweave-recipient escrows. This
is equivalent to the trust assumption already inherent in the
migration architecture (AR.IO holds program upgrade authority,
operates the import/deposit phase, and runs the address-map
service). Adding "AR.IO operates an honest attestor for the claim
window" does not materially expand the trust surface.

Recovery: program upgrade swapping `ATTESTOR_PUBKEY` invalidates the
compromised key for all future claims. The runbook is in
[`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor)`/README.md` § "Key rotation" — measured at ~30
minutes end-to-end.

### Consequences

**Positive:**
- Arweave-recipient escrow claims become operationally feasible on
  Solana mainnet today (~50-77K CU vs the 1.4M tx limit).
- Architecture mirrors the existing Ethereum claim path (sigverify
  via native ix + introspection), keeping the on-chain code base
  consistent.
- Attestor service is stateless, ~50 MB RAM, single Docker container,
  ~$5-15/month to operate.
- Cryptographic primitives are entirely off-the-shelf: `node:crypto`
  for RSA-PSS, `@noble/ed25519` for Ed25519, Solana native sigverify
  for on-chain verification. No custom modexp code shipped.

**Negative (mitigated):**
- AR.IO is in the trust path for Arweave claims (as opposed to fully
  trustless RSA verification on-chain). Mitigated by program-upgrade-
  based key rotation and by the fact that the trust assumption is
  already inherent in the migration architecture.
- Migration claims now depend on attestor liveness. Mitigated by
  stateless service design (trivially horizontally scalable behind a
  load balancer if needed).
- Operational visibility into post-attestation claim flow is reduced
  vs a privileged-authority design. Mitigated by follow-up
  audit-log + dashboard work (deferred, non-blocking).
- The `claim_*_arweave_attested` instructions introduce an Ed25519
  pubkey constant (`ATTESTOR_PUBKEY`) that MUST be replaced before
  any deploy to a real cluster. Mitigated by the
  `contracts/scripts/check-attestor-pubkey.sh --strict` guardrail
  wired into `devnet-deploy.sh` (Phase 0 abort on test-value detection).

**Surface:**
- New on-chain instructions (3): `claim_ant_arweave_attested`,
  `claim_tokens_arweave_attested`, `claim_vault_arweave_attested`.
- New on-chain helper module: `contracts/programs/ario-ant-escrow/src/verify/attested.rs`
  (Ed25519Program ix introspection, ~190 LOC).
- New off-chain service: [`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor) (~1.4 KLOC TS + tests; extracted 2026-05-16 from `solana-ar-io/migration/attestor/`).
- New SDK surface: `EscrowAttestorClient`, `buildEd25519SigverifyIx`,
  6 new methods on `ANTEscrow` / `TokenEscrow` (3 high-level + 3
  ix-builder pairs).
- New error variants: `MissingAttestationInstruction`,
  `MalformedAttestationInstruction`, `AttestationSignerMismatch`,
  `AttestationMessageMismatch`.

**Files / commits:**
- Slice 1+2: `4e1815f` (attestor + 3 attested ixs + naming cleanup)
- Slice 3: `da0e937` (SDK attested claim methods + tests)
- Audit follow-ups: `d87ae61` (network auto-validation + cross-test),
  `f22f5fb` (response shape validation + deploy guardrail)

The on-chain `claim_*_arweave` ixs (which referenced
`sol_big_mod_exp` directly) were removed in commit 4ce73e4. They
prevented the BPF loader from accepting the .so on devnet/mainnet
because of the feature-gated syscall reference. The off-chain attestor
+ `claim_*_arweave_attested` ixs are the only Arweave path now.
## ADR-018: Anchor `#[event]` ABI Policy (Pre-Release Append-Only, Frozen at Mainnet Cutover)

**Date:** 2026-05-05
**Status:** Accepted
**Deciders:** vilenarios

### Context

The 8-PR event-emission rollout (BD-103) shipped 73 events across 5 programs covering 127+ emit sites. The events become part of every consumer's contract — once an indexer subscribes to `NamePurchasedEvent.cost: u64`, renaming the field or changing its type breaks every downstream decoder silently. Anchor's runtime won't catch this; the discriminator survives a field-shape change because it's derived from the *type name*, not the type *layout*.

We need a policy that:
1. Lets us iterate the wire shape rapidly **before** mainnet cutover (the rollout itself made breaking changes — PR-6.5 added `timestamp` to ~50 events, added `new_value` to ConfigUpdated/AntMetadataUpdated, added `expires_at` to NameReserved)
2. **Forbids** any breaking shape change after mainnet, when downstream indexers (portal, observer, marketplaces, third-party Dune-style dashboards) have started subscribing
3. Makes the boundary unambiguous so future contributors don't have to ask "is this safe?"

### Decision

Adopt a **two-phase ABI policy** for Anchor `#[event]` structs:

**Phase 1 — Pre-mainnet (until ARIO mainnet program deploy):**
- Adding new events: ✅ free
- Adding new fields to existing events: ✅ free
- Renaming / removing / retyping fields on existing events: ✅ free **but** requires a sweep PR that updates all emit sites, the IDL snapshot at `contracts/idl-event-snapshots.json`, and the SDK decoders (rerun `yarn codegen`)
- The ABI snapshot is **regenerated**, not enforced as a freeze, during this phase

**Phase 2 — Post-mainnet (after the production ARIO program is deployed and indexers are subscribing):**
- Adding new events: ✅ free
- **Existing event field shapes are immutable.** No rename, no reorder, no retype. This includes adding fields, because field positions matter for borsh decoding.
- To change a shipped event: ship a **new event with a new name**. Convention: `<EventName>V2` (e.g., `NamePurchasedEventV2`). Emit BOTH at the same site for at least one minor SDK release so indexers can migrate. Deprecate `EventName` in docs but keep emitting until at least one major SDK version has elapsed.
- The ABI snapshot becomes a CI freeze gate: `node contracts/scripts/idl-event-snapshot.mjs` must pass on every PR, and `--update` must NOT be run except during an explicit ABI-extension review.

### Rationale

**Why pre-release append-only is wrong for us right now:** every breaking change in PRs 1-6 had a justified reason (uniformity, data richness, parity with Lua). Forcing `*EventV2` versioning from day one would have produced 50+ deprecated events for the same audit cycle's findings — pure noise.

**Why post-mainnet append-only is the right contract:** indexers don't have the equivalent of a "redeploy" — once they ingest event N from slot S, they have to assume that event's shape forever. A retroactive shape change either silently corrupts their database or forces them to backfill, which is operationally expensive and damaging to the AR.IO ecosystem's trust.

**Why `*EventV2` instead of feature flags:** versioned event names are unambiguous in the discriminator dispatch table (`sha256("event:NamePurchasedEventV2")[..8]` differs from V1's). A flag would force every indexer to know about the flag and its semantics; new event names are self-describing.

**Why `[u8; 32]` for `ConfigUpdatedEvent.new_value` (ADR-related cross-ref):** the same logic applies inside an event field. Once shipped, `[u8; 32]` is a 32-byte slot we can reinterpret in software without changing the wire shape. That's why PR-6.5 chose it over a tagged union — future-proof under the freeze rule.

### Alternatives considered

- **Always require V2:** rejected — too costly during pre-release iteration.
- **Indefinite append-only:** rejected — pre-release iteration would have forced 5+ versions of common events for findings the audit would catch in a single sweep.
- **Schema versioning via a `version: u8` field on every event:** considered, rejected. Indexers consuming v1 binary still see the field; they'd need to be coded against v1 to ignore the new fields. The wire-level borsh layout still changes. Doesn't solve the actual problem.

### Consequences

- The `idl-event-snapshots.json` artifact's role changes phase-by-phase: artifact during pre-release (regenerated freely), CI gate post-mainnet (changes require a deliberate `--update` + review).
- Future contributors adding state-changing instructions: emit a new event freely. Don't modify existing event shapes post-cutover.
- SDK consumers should code against the `name` discriminator, not assume positional decoding. The provided `parseEventsFromLogs` / `parseTransactionEvents` helpers handle this correctly already.
- `EVENT_EMISSION_PLAN.md` (the original 2026-04 draft) used "`*EventV2`" as the policy across all phases. **This ADR supersedes that** — pre-release breaking changes are explicitly allowed.

### Cross-references

- BD-103: On-chain event coverage (the catalog this policy governs)
- `docs/EVENT_EMISSION_AUDIT.md`: gap matrix from the rollout
- `docs/EVENT_EMISSION_IMPLEMENTATION_PLAN.md`: 7-PR build plan
- `contracts/scripts/idl-event-snapshot.mjs`: enforcement tooling
- `sdk/src/solana/events.ts`: SDK consumer API (built against this policy)

---

## ADR-XXX: [Title]

**Date:** YYYY-MM-DD
**Status:** Proposed | Accepted | Deprecated | Superseded
**Deciders:** [Who made this decision]

### Context

[What is the issue? Why do we need to make a decision?]

### Decision

[What was decided]

### Rationale

[Why this decision was made, what alternatives were considered]

### Consequences

[What are the implications of this decision]
```
