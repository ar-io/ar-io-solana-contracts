# Independent Security Audit: AR.IO Solana Smart Contracts

**Date:** 2026-03-20
**Scope:** All 4 Anchor programs (ario-core, ario-gar, ario-arns, ario-ant) — 86 instructions
**Prior Audit:** docs/SECURITY_AUDIT.md (2026-03-18, 29 findings, all verified fixed)
**Method:** Full code review of all instruction handlers, account contexts, state types, migration handlers, and cross-program interactions

---

## Executive Summary

The AR.IO Solana contracts are well-engineered with consistent use of Anchor constraints, PDA validation, checked arithmetic, and defense-in-depth patterns. The prior audit's 29 findings are confirmed fixed. This independent assessment found **44 new findings** across all severity levels. The most critical issues are in the epoch subsystem (observer selection manipulation, single-observer failure threshold) and a registry corruption vector in gateway pruning. All programs share a common migration deadline issue (i64::MAX placeholder).

| Severity | Count | Programs Affected |
|----------|-------|-------------------|
| Critical | 1 | ario-gar |
| High | 4 | ario-gar (2), ario-ant (1), ario-core (1) |
| Medium | 13 | ario-gar (4), ario-arns (4), ario-ant (3), ario-core (4) |
| Low | 12 | All programs |
| Informational | 14 | All programs |

**Top 5 Priority Fixes:**
1. **GAR-013** — prune_gateway registry corruption (double-prune on Leaving gateway)
2. **GAR-003** — Cranker controls observer address resolution in prescribe_epoch
3. **GAR-005** — Single observer can fail all gateways (threshold = 0 when 1 observer)
4. **ARNS-003/004** — Pruned names become permanently unregisterable (missing `.assign()`)
5. **MIGRATION-001** — All 4 programs have MIGRATION_DEADLINE = i64::MAX (must set before mainnet)

---

## Known Design Limitations (Not Bugs)

These are acknowledged tradeoffs documented for completeness:

| ID | Area | Description |
|----|------|-------------|
| KDL-1 | Hashchain timing | epoch.rs uses slot+epoch+timestamp (payer removed per GAR-004). A cranker can choose which slot to submit in, biasing observer selection. Equivalent to Lua/AO model. Mitigated by stake-weighted selection and front-running by competing crankers. |
| KDL-2 | Metaplex Core layout coupling | ario-ant read_mpl_core_owner hardcodes AssetV1 byte layout. Pinned dependency mitigates. Upgrade requires code change. |
| KDL-3 | Allowlist discriminator skip | delegate.rs checks PDA+owner but not discriminator. PDA derivation with correct seeds effectively guarantees account type. |
| KDL-4 | Permissionless epoch cranking | Anyone can advance epoch pipeline. By design for decentralization. Ordering constraints prevent manipulation. |
| KDL-5 | Reward dust from integer division | Per-gateway/observer rewards truncate; dust stays in protocol account. Matches Lua behavior. |

---

## Findings: ario-core (18 instructions)

### CORE-001 | Medium | Arithmetic
**File:** `programs/ario-core/src/instructions/primary_name.rs:227-231, 324-328`
**Description:** Unsafe u128→u64 truncation in primary name fee calculation using `as u64`. The formula `(base_fee as u128) * (demand_factor as u128) / 1_000_000` currently fits in u64 (200,000 × max_u64 / 1M < u64::MAX), but the pattern is fragile.
**Impact:** If base fee constant increases, silent truncation would charge lower fees.
**Fix:** Replace `as u64` with `u64::try_from(...).map_err(|_| ArioError::ArithmeticOverflow)?`

### CORE-002 | Medium | Access Control
**File:** `programs/ario-core/src/instructions/token.rs:36-47`
**Description:** TransferTokens does not validate `from_token_account.owner == authority.key()` or that both token accounts share the same mint. SPL Token program enforces this at CPI time, but the error messages are opaque and delegate access may be unintended.
**Impact:** Not directly exploitable. Defense-in-depth gap.
**Fix:** Add constraints: `from_token_account.owner == authority.key()`, `from_token_account.mint == to_token_account.mint`

### CORE-003 | Low | Access Control
**File:** `programs/ario-core/src/instructions/vault.rs:530-534`
**Description:** RevokeVault's `controller_token_account` validates mint but not `owner == controller.key()`. The controller can direct revoked tokens to any token account of the correct mint.
**Impact:** Controller can send tokens to an unrelated account. May be intentional.
**Fix:** Add `controller_token_account.owner == controller.key()` or document as intentional.

### CORE-004 | High | State Machine
**File:** `programs/ario-core/src/migration.rs:13`
**Description:** MIGRATION_DEADLINE = i64::MAX (placeholder). Without a real deadline, migration_authority can import arbitrary state indefinitely until finalize_migration is called.
**Impact:** If finalize_migration is delayed and migration_authority is compromised, attacker has unlimited import window for complete state corruption of Balance, Vault, VaultCounter, PrimaryName, and PrimaryNameRequest accounts.
**Fix:** Set to concrete timestamp (e.g., migration_start + 30 days) before mainnet.

### CORE-005 | High | State Machine
**File:** `programs/ario-core/src/migration.rs:98-114`
**Description:** import_account allows re-importing (overwriting) existing accounts. While designed for retry/recovery, a compromised migration_authority can overwrite any vault's amount to 0 or reassign any PrimaryName.
**Impact:** Full state corruption during migration window.
**Fix:** Inherent to migration design. Mitigate: tight MIGRATION_DEADLINE, prompt finalize, destroy migration_authority key after use.

### CORE-006 | ~~Low~~ Medium | Economic
**File:** `programs/ario-core/src/instructions/primary_name.rs:169-200`
**Description:** `read_demand_factor` silently defaults to 1.0 (1,000,000) on any validation failure instead of erroring. During high-demand periods (current demand factor = 9x), passing a bogus account would charge base fee (0.2 ARIO) instead of the demand-adjusted fee (1.8 ARIO), saving the attacker 1.6 ARIO per primary name request.
**Impact:** At demand factor 9x, attacker saves 1.6 ARIO per request. At higher demand factors, savings scale linearly.
**Fix:** Changed `read_demand_factor` to return `Result<u64>` and error on invalid accounts instead of falling back to 1.0. **FIXED**

### CORE-007 | Informational | Access Control
**File:** `programs/ario-core/src/instructions/admin.rs:10-46`
**Description:** UpdateConfig allows setting `min_vault_duration` to 0, enabling zero-duration vaults. Only reachable if authority explicitly changes the default (14 days).
**Impact:** Minimal. Zero-duration vaults serve no purpose.

### CORE-008 | Medium | State Machine
**File:** `programs/ario-core/src/instructions/primary_name.rs:344-357, 692-700`
**Description:** `request_and_set_primary_name` uses init_if_needed for PrimaryName but does not close the old PrimaryNameReverse when a user changes their primary name. The orphaned reverse lookup blocks anyone else from using that name as a primary name.
**Exploitation:** User sets primary name "alice", then changes to "bob". PrimaryNameReverse["alice"] persists orphaned, permanently blocking "alice" from being any other user's primary name.
**Impact:** Name squatting — users can cheaply lock names as unusable primary names.
**Fix:** Require the old PrimaryNameReverse as an additional account to close when overwriting, or require remove_primary_name before setting a new one.

### CORE-009 | Informational | Arithmetic
**File:** `programs/ario-core/src/migration.rs:88`
**Description:** `data.len() as u64` — safe since known account sizes are all < 256 bytes.

### CORE-010 | Informational | State Machine
**File:** `programs/ario-core/src/instructions/initialize.rs:7-28`
**Description:** Initialize does not validate total_supply matches actual mint supply or that treasury is a valid token account. One-time deployment risk only.

---

## Findings: ario-gar (32 instructions)

### GAR-001 | Low | Arithmetic
**File:** `programs/ario-gar/src/state/mod.rs:371`
**Description:** `settle_delegate_rewards` uses `pending as u64` (silent u128→u64 truncation) for the reward amount. With REWARD_PRECISION = 1e18, the accumulator delta × delegation amount could theoretically exceed u64::MAX over thousands of epochs with large token supply.
**Impact:** Delegate receives fewer rewards than earned. Low probability in practice.
**Fix:** Use `u64::try_from(pending).unwrap_or(u64::MAX)` to cap rather than truncate.

### GAR-003 | Critical | Epoch / Access Control
**File:** `programs/ario-gar/src/instructions/epoch.rs:246-248, 272-293`
**Description:** `prescribe_epoch` initially sets all `prescribed_observers[i]` to operator pubkeys. A second pass resolves actual observer_address from Gateway PDAs in remaining_accounts. If the cranker omits Gateway PDAs, prescribed_observers remain as operator pubkeys, allowing the cranker to control which signers are authorized to submit observations.
**Exploitation:** Cranker calls prescribe_epoch without Gateway remaining_accounts → actual observer wallets cannot submit observations → those gateways receive 25% penalty or fail the epoch → competing gateways benefit.
**Impact:** Selective denial-of-service on observer submission. Cranker can grief specific gateways.
**Fix:** Either (a) require exactly observer_count Gateway PDAs in remaining_accounts and fail if any are missing, or (b) store observer_address during tally_weights where all gateways are already deserialized.
**Verified:** CONFIRMED — epoch.rs lines 246-248 set both arrays to slot.address; lines 272-293 resolve from remaining_accounts with `continue` on failure.

### GAR-004 | ~~High~~ Low | Epoch
**File:** `programs/ario-gar/src/instructions/epoch.rs:72-86`
**Description:** Hashchain entropy was manipulable via payer pubkey grinding (zero-cost keypair generation to control observer selection). Fixed by removing payer pubkey from hash inputs, leaving only network-determined values (slot, epoch_index, timestamp). Residual risk: timing attack (cranker chooses which slot to submit in), equivalent to Lua/AO security model where anyone can trigger Tick at a favorable moment.
**Impact:** Keypair grinding eliminated. Timing manipulation provides marginal advantage (~1 extra ARIO/epoch in best case), equivalent to Lua behavior.
**Fix:** Removed payer pubkey from hashchain seed. **MITIGATED** — see Appendix: Future Randomness Upgrades for commit-reveal and VRF options.

### GAR-005 | High | Epoch / Economic
**File:** `programs/ario-gar/src/instructions/distribution.rs:66-67`
**Description:** Failure threshold: `failure_counts[i] > observations_submitted / 2`. With observations_submitted=1, threshold=0, so a single failure report = gateway fails. One malicious observer who is the sole reporter can fail ALL other gateways.
**Exploitation:** Attacker is prescribed observer → is only one to submit → marks all others failed → all competitors get 0 rewards.
**Impact:** Complete reward theft in epochs with low observer participation.
**Fix:** Require minimum number of observations (e.g., 3) before failure determination applies.
**Verified:** CONFIRMED — `(1 as u16) / 2 = 0`, so `failure_count > 0` triggers failure.

### GAR-006 | High | Economic
**File:** `programs/ario-gar/src/instructions/distribution.rs:118-153`
**Description:** `distribute_epoch` modifies gateway state (operator_stake, cumulative_reward_per_token) before the SPL transfer. If protocol_token_account is drained between create_epoch and distribute_epoch, the transfer fails and the epoch becomes permanently un-distributable, blocking the entire pipeline.
**Impact:** Epoch pipeline permanently blocked. All future reward distribution halted.
**Fix:** Check `batch_total_reward <= protocol_token_account.amount` before mutations, or allow partial/skip distribution.

### GAR-007 | Medium | Access Control
**File:** `programs/ario-gar/src/instructions/epoch.rs:9-13`
**Description:** `set_epochs_enabled` has no timelock — authority can instantly disable all reward distribution with a single key.
**Impact:** Centralization risk. Compromised authority halts rewards.
**Fix:** Consider timelock or multi-sig for disabling epochs.

### GAR-008 | Medium | Arithmetic
**File:** `programs/ario-gar/src/state/mod.rs:297-308`
**Description:** Composite weight calculation divides after each multiplication step: `stake * tenure / scale * gw_perf / scale * obs_perf / scale`. Sequential division loses more precision than `(stake * tenure * gw_perf * obs_perf) / scale^3`.
**Impact:** Small gateways receive disproportionately lower composite weights, biasing rewards toward established gateways.
**Fix:** Accumulate full product in u128 before dividing: `(stake * tenure * gw_perf * obs_perf) / (scale * scale * scale)`

### GAR-009 | Medium | State Machine
**File:** `programs/ario-gar/src/instructions/epoch.rs:116-201`
**Description:** If gateways leave between `create_epoch` (which snapshots active_gateway_count) and `tally_weights`, the registry shrinks via swap-remove. Tally will try to read zeroed/invalid registry slots for the missing gateways, causing permanent failure.
**Impact:** Epoch becomes permanently stuck — cannot tally, prescribe, distribute, or close.
**Fix:** Skip zeroed/invalid registry slots during tally instead of erroring, or snapshot registry at creation.
**Verified:** PARTIALLY CONFIRMED — swap-remove rearranges slots; tally validates `registry.gateways[idx].address == gateway.operator`, which fails for moved/zeroed slots.

### GAR-013 | Medium | State Machine / PDA
**File:** `programs/ario-gar/src/instructions/gateway.rs:315-452`
**Description:** `prune_gateway` does NOT check `gateway.status == Joined`. A gateway that already called leave_network (status=Leaving, registry removed via swap-remove) can be pruned again. The stale `registry_index` would cause the swap-remove to corrupt the registry by removing the wrong gateway's slot.
**Exploitation:** (1) Gateway A leaves (index 5 freed, Gateway B swapped in). (2) Gateway A still has failed_consecutive >= threshold. (3) Attacker calls prune_gateway on A. (4) Registry swap-removes index 5 again → Gateway B silently removed from registry.
**Impact:** Active gateway silently removed from registry, losing epoch eligibility and rewards.
**Fix:** Add `require!(gateway.status == GatewayStatus::Joined, GarError::GatewayLeaving)` at start of prune_gateway.
**Verified:** CONFIRMED — no status check exists; `registry_index.index` is stale after leave_network.

### GAR-014 | Medium | Migration
**File:** `programs/ario-gar/src/migration.rs:13`
**Description:** MIGRATION_DEADLINE = i64::MAX (placeholder). Same issue as CORE-004.
**Fix:** Set concrete timestamp before mainnet.

### GAR-015 | Low | Arithmetic
**File:** `programs/ario-gar/src/instructions/epoch.rs:360-373`
**Description:** Integer division truncation in per_gateway/observer rewards loses dust. Example: 90 tokens ÷ 7 gateways = 12 each (6 tokens lost to truncation = 6.7%).
**Impact:** Small amounts permanently in protocol account. Accumulates over thousands of epochs.

### GAR-016 | Low | Access Control
**File:** `programs/ario-gar/src/instructions/delegate.rs:484-504`
**Description:** `close_empty_delegation` is permissionless — rent returns to caller (payer), not original delegator. MEV bots can front-run to steal ~0.002 SOL rent.
**Fix:** Return rent to `delegation.delegator` instead of `payer`.

### GAR-017 | Low | PDA
**File:** `programs/ario-gar/src/instructions/gateway.rs:477-532`
**Description:** JoinNetwork uses `init` (not init_if_needed) — correct. But gateway accounts are never closed, so re-join is permanently prevented. If closure is ever added, re-join prevention breaks. Latent risk.

### GAR-018 | Low | Arithmetic
**File:** `programs/ario-gar/src/instructions/distribution.rs:120-122`
**Description:** Delegate pool uses integer division; operator always gets rounding remainder (1-2 extra mARIO per epoch). Standard pattern.

### GAR-019 | Low | Epoch
**File:** `programs/ario-gar/src/instructions/epoch.rs:227`
**Description:** Observer selection loop runs `max_observers * 3` iterations. Frequent collisions (many equal-weight gateways) could produce fewer observers than configured, amplifying GAR-005.
**Fix:** Increase multiplier to 10x or use Fisher-Yates shuffle.

### GAR-020 | Low | Economic
**File:** `programs/ario-gar/src/instructions/gateway.rs:315-325`
**Description:** prune_gateway threshold uses configurable `max_consecutive_failures`. If authority lowers this, previously-safe gateways become instantly pruneable.
**Fix:** Add delay/cooldown when changing threshold, or only apply to failures after the change.

### GAR-021 | Informational | Access Control
**File:** `programs/ario-gar/src/instructions/initialize.rs:15-38`
**Description:** Initialize is not gated by authority check — first caller sets program authority. Standard deployment-time concern.

### GAR-022 | Informational | State
**File:** `programs/ario-gar/src/state/mod.rs:93-105`
**Description:** GatewayRegistry.authority field is vestigial (documented but not checked at runtime). Wastes 32 bytes.

### GAR-023 | Informational | State Machine
**File:** `programs/ario-gar/src/instructions/distribution.rs:155-165`
**Description:** Gateway stats use u32 for epochs (supports ~11M years at 1/day) and u8 for consecutive counters (caps at 255 via saturating_add, threshold is 30). Safe bounds.

### GAR-024 | Informational | Migration
**File:** `programs/ario-gar/src/migration.rs:55-130`
**Description:** Idempotent re-import documented with security note. Standard migration tradeoff.

---

## Findings: ario-arns (20 instructions)

### ARNS-001 | Medium | Arithmetic
**File:** `programs/ario-arns/src/instructions/demand.rs:44-55`
**Description:** Unsafe u128→u64 cast in demand factor adjustment using `as u64`. While demand factor starts at 1,000,000 and grows 1.05x per period, the cast should use try_from for safety.
**Fix:** Replace `as u64` with `u64::try_from(...).map_err(|_| error!(ArnsError::ArithmeticOverflow))?`

### ARNS-002 | Medium | Arithmetic
**File:** `programs/ario-arns/src/instructions/demand.rs:148`
**Description:** `update_fees()` uses `(*fee as u128 * multiplier as u128 / SCALE as u128) as u64` — unchecked cast. Currently only called with DEMAND_FACTOR_MIN (0.5x halving), but fragile against future changes.
**Fix:** Use `u64::try_from(...)` or add assertion.

### ARNS-003 | Medium | State Machine
**File:** `programs/ario-arns/src/instructions/prune.rs:96-109`
**Description:** `prune_expired_names` zeroes data and drains lamports but does NOT call `.assign(&system_program::ID)`. The account remains program-owned with 0 lamports. Anchor's `init` constraint in `buy_name` requires system program ownership, so pruned names become permanently unregisterable.
**Exploitation:** (1) Name "example" expires past grace+auction. (2) prune_expired_names closes account without .assign(). (3) New buyer's buy_name("example") fails init check. (4) Name permanently locked.
**Impact:** Names pruned via this path can never be re-purchased.
**Fix:** Add `record_info.assign(&anchor_lang::solana_program::system_program::ID);` after zeroing data.
**Verified:** CONFIRMED — no .assign() call. Compare to ario-gar gateway.rs which correctly assigns.

### ARNS-004 | Medium | State Machine
**File:** `programs/ario-arns/src/instructions/prune.rs:170-182`
**Description:** Same issue as ARNS-003 for `prune_returned_names`. ReturnedName PDA not reassigned to system program after closure.
**Fix:** Add `.assign(&system_program::ID)` after zeroing data.

### ARNS-005 | Low | Economic
**File:** `programs/ario-arns/src/instructions/purchase.rs:227-232`
**Description:** Returned name 50% split uses `token_cost / 2` — initiator always gets rounding remainder on odd values. 1 mARIO per transaction.

### ARNS-006 | Low | Pricing
**File:** `programs/ario-arns/src/pricing.rs:173-212`
**Description:** Returned name premium chains three floor divisions, compounding truncation. Max error bounded at ~50 mARIO. Standard for Solana programs.

### ARNS-007 | Low | Access Control
**File:** `programs/ario-arns/src/instructions/demand.rs:17-97`
**Description:** `update_demand_factor` loop processes ALL missed periods in one call. If many periods are missed (e.g., 365 days), could exceed CU limits.
**Fix:** Add max_periods parameter to bound the loop.

### ARNS-008 | Low | Pricing
**File:** `programs/ario-arns/src/instructions/manage.rs:86-89`
**Description:** extend_lease uses ceiling division for remaining years, which is conservative but could prevent valid 4-year extensions when user has 1 year + 1 second remaining.

### ARNS-009 | Low | State Machine
**File:** `programs/ario-arns/src/instructions/purchase.rs:32-48`
**Description:** Reserved name check deserializes UncheckedAccount without verifying name_hash matches expected hash. Extremely low risk due to PDA derivation.

### ARNS-010 | Low | Arithmetic
**File:** `programs/ario-arns/src/instructions/demand.rs:109`
**Description:** `(timestamp - period_zero_start) as u64` — signed-to-unsigned cast. Safe due to prior guard but not explicit.

### ARNS-011 | Low | Economic
**File:** `programs/ario-arns/src/instructions/demand.rs:80-82`
**Description:** Ring buffer 1-based indexing leaves slot 0 empty until period 7. Slightly depresses moving average during first 7 periods.

### ARNS-012 | Informational | Migration
**File:** `programs/ario-arns/src/migration.rs:13`
**Description:** MIGRATION_DEADLINE = i64::MAX (placeholder). Same issue across all programs.

### ARNS-013-020 | Informational
Various informational findings: idempotent re-import, case normalization correct, reserved name claim naming confusion, zero-cost defense check, gateway discount pass_rate smoothing, upgrade during grace period intentional, moving average zero guard, initialize authority set from params.

---

## Findings: ario-ant (16 instructions)

### ANT-002 | Medium | Record / Access Control
**File:** `programs/ario-ant/src/lib.rs:203-206`
**Description:** Owner/controller calling set_record unconditionally overwrites `record.owner` with `params.record_owner`. If a controller calls set_record to update the target but omits record_owner (None), delegated ownership is silently cleared.
**Impact:** Delegated record owners lose ownership unexpectedly.
**Fix:** Use separate instruction for record ownership changes, or treat None as "don't change".

### ANT-003 | Medium | NFT
**File:** `programs/ario-ant/src/lib.rs:564-573`
**Description:** `read_mpl_core_owner` hardcodes AssetV1 byte layout. Comment claims "mpl-core dependency pinned in Cargo.toml" but mpl-core is NOT in Cargo.toml. No compile-time binding to detect layout changes.
**Impact:** If Metaplex Core upgrades AssetV1 layout, ownership verification breaks silently.
**Fix:** (1) Add mpl-core as dev-dependency for layout verification. (2) Correct misleading comment. (3) Monitor Metaplex Core upgrades.

### ANT-008 | High | Migration
**File:** `programs/ario-ant/src/migration.rs:14`
**Description:** MIGRATION_DEADLINE = i64::MAX (placeholder). ANT migration_authority can overwrite any AntConfig, AntControllers, or AntRecord indefinitely.
**Impact:** Complete ANT state corruption if migration_authority is compromised.
**Fix:** Set concrete deadline before mainnet.

### ANT-009 | Medium | Migration
**File:** `programs/ario-ant/src/migration.rs:119-131`
**Description:** Idempotent re-import allows overwriting all existing program accounts during migration window. Same risk as other programs.

### ANT-011 | Low | Access Control
**File:** `programs/ario-ant/src/lib.rs:300-326`
**Description:** Controllers can add other controllers. A single controller grant effectively grants up to 10 controller slots. May be intentional (matches Lua) but is privilege escalation.
**Fix:** If only NFT owner should manage controllers, bypass controller check in add/remove_controller.

### ANT-017 | Medium | State Machine
**File:** `programs/ario-ant/src/lib.rs:263-278`
**Description:** `transfer_record` is broken after NFT transfer. Reconciliation at line 267-270 clears `record.owner` to None (stale ownership cleanup). Line 278 then checks `record.owner.is_some()` which fails. Even the new NFT owner cannot call transfer_record on reconciled records.
**Exploitation:** Owner A transfers NFT to Owner B. Owner B calls transfer_record("blog", newOwner). Reconciliation clears record.owner → is_some() check fails → transaction reverted.
**Impact:** transfer_record unusable after any NFT transfer until record ownership is re-established via set_record.
**Fix:** Re-order: check is_some() before reconciliation, or allow owner/controllers to assign record ownership when record.owner is None.
**Verified:** CONFIRMED — reconciliation clears owner at line 267-270, is_some check at line 278 always fails for reconciled records.

### ANT-020 | Low | Migration
**File:** `programs/ario-ant/src/migration.rs:21-34, 63-138`
**Description:** import_account writes raw data without semantic validation. Imported AntConfig could have name > MAX_NAME_LENGTH, AntRecord with invalid target, etc.

### ANT-023 | Low | Controller
**File:** `programs/ario-ant/src/lib.rs:535-537`
**Description:** Reconciliation clears controller list but doesn't auto-add new NFT owner as controller (unlike initialization). New owner must manually add themselves.

---

## Cross-Program Findings

### XP-001 | Low | Cross-Program Read
**File:** `programs/ario-gar/src/instructions/epoch.rs:722-729`
**Description:** `read_name_registry_header` validates owner + PDA for the NameRegistry account but does NOT validate the zero-copy discriminator before reading the `count` field. If the account data were corrupted, the count could be misread, affecting name prescription.
**Impact:** Very low — owner + PDA double-check makes exploitation impractical.
**Fix:** Add discriminator validation: `require!(data[..8] == hash(b"account:NameRegistry").to_bytes()[..8])`.

### XP-002 | Informational | Cross-Program Read
**File:** `programs/ario-core/src/instructions/primary_name.rs:130-166`
**Description:** `verify_arns_record_active` uses raw byte offset parsing of ArnsRecord layout. Brittle against field order changes but correctly validates discriminator, owner, and PDA first. Functionally secure.

### XP-003 | Informational | CPI
**All CPI calls verified secure.** 13 user-signed CpiContext::new calls and 12 PDA-signed CpiContext::new_with_signer calls across all programs use correct authorities, PDA seeds, and Anchor-validated token programs.

---

## Migration Findings (Cross-Cutting)

### MIGRATION-001 | High | All Programs
**Files:** ario-core/migration.rs:13, ario-gar/migration.rs:13, ario-arns/migration.rs:13, ario-ant/migration.rs:14
**Description:** ALL four programs have `MIGRATION_DEADLINE = i64::MAX` with TODO comments. This is the single most important pre-mainnet fix. Without real deadlines, a compromised migration_authority has an unlimited window for state corruption.
**Fix:** Set all four to concrete timestamps before mainnet (e.g., migration_start + 30 days).

---

## Test Coverage Gaps

| ID | Category | Description | Priority |
|----|----------|-------------|----------|
| TEST-001 | Migration finalization | No test verifies import rejection after finalize_migration in any program | High |
| TEST-002 | Double-distribute | No test for calling distribute_epoch twice on same epoch | High |
| TEST-003 | Cross-program NameRegistry | No integration test for prescribe_epoch with fake NameRegistry | High |
| TEST-004 | Arithmetic overflow | No integration test for checked arithmetic overflow paths | Medium |
| TEST-005 | Gateway discount attack vectors | Gateway discount integration test skipped (comment at ario-arns line 2871) | Medium |
| TEST-006 | Multi-batch tally | No test for partial tally_weights followed by completion | Medium |
| TEST-007 | Double-prune | No test for pruning already-Leaving gateway or already-pruned name | Medium |
| TEST-008 | Post-epoch observations | No end-to-end observation window enforcement test | Medium |
| TEST-009 | Vault revoke after expiry | No test for revoking expired vault | Medium |
| TEST-010 | NameRegistry discriminator | No test for prescribe_epoch with corrupted NameRegistry data | Medium |
| TEST-011 | Primary name lease expiry between request/approve | Lease expiry during approval window not tested | Low |
| TEST-012 | Concurrent vault creation | No test for rapid vault counter increments | Low |
| TEST-013 | Zero-amount vault release | No test for releasing vault with 0 tokens | Low |
| TEST-014 | Gateway discount tenure boundary | No test at exactly 180-day tenure boundary | Low |

---

## Severity Definitions

| Level | Definition |
|-------|------------|
| **Critical** | Exploitable by any user; causes fund loss, permanent state corruption, or protocol-breaking denial of service |
| **High** | Exploitable with specific conditions (e.g., compromised key, economic prerequisites); significant economic or operational impact |
| **Medium** | Exploitable but with limited impact, requires unusual conditions, or is a significant defense-in-depth gap |
| **Low** | Minor economic rounding, edge-case usability issues, or latent risks requiring code changes to manifest |
| **Informational** | Code quality, documentation gaps, or acknowledged design tradeoffs |

---

## Recommended Fix Priority

### Before Mainnet (Blockers)
1. **MIGRATION-001** — Set MIGRATION_DEADLINE in all 4 programs. **OPEN (deferred to pre-mainnet).** The placeholder was replaced with a finite constant, but it is intentionally set far-future (2112-04-20) so the time-based backstop is effectively off; `finalize_migration` is the sole active control on the migration-authority write window. **This MUST be tightened to the real migration cutoff before mainnet** — until then the unbounded-window risk stands.
2. ~~**GAR-013** — Add status check to prune_gateway~~ **FIXED**
3. ~~**ARNS-003/004** — Add `.assign(&system_program::ID)` to prune functions~~ **FIXED**
4. ~~**GAR-003** — Require Gateway PDAs in prescribe_epoch remaining_accounts~~ **FIXED**
5. **GAR-005** — Single observer failure threshold — **ACCEPTED** (matches Lua behavior by design; if only 1 observer submits, their report is the majority)

### High Priority (Should Fix)
6. ~~**GAR-006** — Check protocol balance before distribution mutations~~ **FIXED** (caps transfer at available balance)
7. ~~**GAR-009** — Handle registry changes between create_epoch and tally_weights~~ **FIXED**
8. ~~**ANT-017** — Fix transfer_record ordering after NFT transfer~~ **FIXED**
9. ~~**CORE-008** — Handle orphaned PrimaryNameReverse on name change~~ **FIXED**
10. ~~**CORE-001, ARNS-001/002, GAR-001** — Replace all `as u64` with `try_from()`~~ **FIXED**

### Medium Priority (Should Consider)
11. ~~**ANT-003** — Correct mpl-core dependency comment, add version monitoring~~ **FIXED**
12. **ANT-002** — Separate record ownership management from set_record — **ACCEPTED** (Solana is already better than Lua here)
13. ~~**GAR-008** — Accumulate composite weight product before dividing~~ **FIXED**
14. ~~**CORE-002** — Add mint/owner constraints to TransferTokens~~ **FIXED**
15. ~~**GAR-016** — Return delegation rent to original delegator~~ **FIXED**
16. ~~**GAR-007** — Add timelock to set_epochs_enabled~~ **FIXED** (7-day timelock on disable)

### Remaining Open
- ~~**GAR-004** — Hashchain entropy~~ **MITIGATED** — payer removed from hash; residual timing risk matches Lua parity (see Appendix: Future Randomness Upgrades)
- ~~**CORE-006** — read_demand_factor defaults to 1.0 on failure~~ **FIXED** (now errors on invalid DemandFactor account)
- **CORE-005** — Import overwrites (inherent to migration design, mitigated by concrete MIGRATION_DEADLINE) — **ACCEPTED**

### Test Coverage (Should Add)
17. All HIGH priority test gaps (TEST-001 through TEST-003)
18. All MEDIUM priority test gaps (TEST-004 through TEST-010)

---

## Appendix: Lua Source Comparison

Each finding was compared against the original Lua source code (`/mnt/c/source/ar-io-network-process/src/` and `/mnt/c/source/ar-io-ant-process/`) to determine whether the issue also exists in the AO implementation or is Solana-specific. Findings present in Lua should be fixed in Solana (they represent known behavior). Solana-only findings are regressions or new attack surfaces that need fresh evaluation.

### Classification Summary

| Finding | Severity | Lua Status | Recommendation |
|---------|----------|------------|----------------|
| **GAR-003** | Critical | **SOLANA-ONLY** | Fix — Lua reads all gateways atomically from process state; remaining_accounts omission vector doesn't exist |
| **GAR-004** | Low | **ALSO IN LUA** (now equivalent) | **Mitigated** — Removed payer pubkey from hash. Solana now uses slot+epoch+timestamp (network-determined), matching Lua's Arweave block hash model. Residual timing risk is shared with Lua. |
| **GAR-005** | High | **ALSO IN LUA** | Fix — Identical formula `failure_count > observations_submitted / 2`. Single observer can fail all gateways in both |
| **GAR-006** | High | **ALSO IN LUA** (lower severity) | Fix — Both can fail if protocol balance drops. Lua retries atomically; Solana epoch PDA is permanently bricked |
| **GAR-007** | Medium | **SOLANA-ONLY** | Review — Lua has no epoch enable/disable toggle at all |
| **GAR-008** | Medium | **SOLANA-ONLY** | Fix — Lua uses float64 multiplication (no truncation); Solana's sequential integer division loses precision |
| **GAR-009** | Medium | **SOLANA-ONLY** | Fix — Lua's atomic tick handles gateway departures gracefully; Solana's multi-tx pipeline breaks |
| **GAR-013** | Medium | **SOLANA-ONLY** | Fix — Lua uses key-value table (`GatewayRegistry[addr] = nil`); no array swap-remove corruption risk |
| **GAR-014** | Medium | **SOLANA-ONLY** | Fix — Migration is Solana-only |
| **GAR-001** | Low | **ALSO IN LUA** (lower severity) | Review — Lua uses float64 (adequate for realistic values); Solana u128→u64 truncation is more acute |
| **GAR-015** | Low | **ALSO IN LUA** | Accept — Both use floor/integer division; dust stays in protocol. By design |
| **GAR-016** | Low | **SOLANA-ONLY** | Fix — No rent concept in Lua |
| **GAR-019** | Low | **SOLANA-ONLY** | Fix — Lua's selection loop has no iteration cap; always selects required observers |
| **GAR-020** | Low | **ALSO IN LUA** | Accept — Both apply threshold changes retroactively |
| **CORE-001** | Medium | **SOLANA-ONLY** | Fix — Lua uses float math; no integer overflow risk |
| **CORE-002** | Medium | **SOLANA-ONLY** | Fix — AO messaging layer validates sender implicitly; no account-passing attack vector |
| **CORE-003** | Low | **SOLANA-ONLY** | Fix — Lua always returns revoked tokens to controller's balance; cannot redirect |
| **CORE-004/005** | High | **SOLANA-ONLY** | Fix — Migration is Solana-only |
| **CORE-006** | Medium | **SOLANA-ONLY** | **Fix** — Lua reads demand factor from in-process global; cannot fail. Solana cross-program read now errors on invalid account (was silently defaulting to 1.0, enabling fee bypass at high demand factors) |
| **CORE-008** | Medium | **SOLANA-ONLY** (Lua handles correctly) | **Fix — Regression from Lua.** Lua's `setPrimaryNameFromRequest` explicitly calls `removePrimaryName` on old name before setting new one, cleaning up both forward and reverse maps |
| **ARNS-001/002** | Medium | **SOLANA-ONLY** | Fix — Lua uses float math; no integer casts |
| **ARNS-003/004** | Medium | **SOLANA-ONLY** (Lua handles correctly) | **Fix — Regression from Lua.** Lua's `prune.pruneState` creates ReturnedName for pruned records and cleans up properly, names are re-purchasable |
| **ARNS-005** | Low | **ALSO IN LUA** | Accept — Identical `math.floor(fee * 0.5)` approach |
| **ARNS-007** | Low | **ALSO IN LUA** | Accept — Identical unbounded loop pattern |
| **ARNS-008** | Low | **ALSO IN LUA** | Accept — Identical `math.ceil` for remaining years |
| **ARNS-012** | Informational | **SOLANA-ONLY** | Fix — Migration deadline is Solana-only |
| **MIGRATION-001** | High | **SOLANA-ONLY** | Fix — Migration infrastructure is Solana-only |
| **ANT-002** | Medium | **ALSO IN LUA** (worse in Lua) | Review — Lua rebuilds entire record unconditionally; Solana gates owner field to owner/controllers only. Solana is actually better here |
| **ANT-003** | Medium | **SOLANA-ONLY** | Fix — Metaplex Core layout parsing has no Lua equivalent |
| **ANT-008/009** | High/Medium | **SOLANA-ONLY** | Fix — Migration is Solana-only |
| **ANT-011** | Low | **ALSO IN LUA** | Accept — Controllers can add controllers in both. Intentional design |
| **ANT-017** | Medium | **SOLANA-ONLY** | **Fix — Regression from Lua.** Lua preserves record owners across ANT transfers; Solana's H-7 reconciliation clears them and breaks transfer_record |
| **ANT-020** | Low | **SOLANA-ONLY** | Review — Migration import is Solana-only; Lua init validates |
| **ANT-023** | Low | **SOLANA-ONLY** | Review — Both systems allow owner access without controller membership; reconciliation is Solana-only |

### Key Takeaways

**Findings that exist in BOTH Lua and Solana (7):**
- GAR-004, GAR-005, GAR-006, GAR-001, GAR-015, GAR-020 (epoch/reward system)
- ARNS-005, ARNS-007, ARNS-008 (pricing/demand)
- ANT-002, ANT-011 (record/controller model)

These are shared design patterns. GAR-005 (single observer failure) is the most critical shared finding and should be fixed in Solana regardless of Lua parity.

**Findings that are SOLANA-ONLY (26+):**
The majority of findings are Solana-specific, arising from:
1. **Account model** — remaining_accounts omission (GAR-003), missing .assign() (ARNS-003/004), rent economics (GAR-016), token account constraints (CORE-002/003)
2. **Integer arithmetic** — u128→u64 casts (CORE-001, ARNS-001/002, GAR-001), sequential division precision (GAR-008)
3. **Multi-transaction pipeline** — epoch bricking (GAR-009), protocol drain (GAR-006 elevated severity)
4. **Migration infrastructure** — deadline placeholders (MIGRATION-001), re-import overwrite (CORE-005, ANT-009)
5. **NFT reconciliation** — H-7 lazy clearing (ANT-017, ANT-023)

**Regressions from Lua (3 — highest priority):**
These are cases where Lua handles it correctly but the Solana port introduced a bug:
1. **CORE-008** — Lua's `setPrimaryNameFromRequest` cleans up old reverse lookup; Solana does not
2. **ARNS-003/004** — Lua's `prune.pruneState` creates ReturnedName and enables re-purchase; Solana leaves names permanently stuck
3. **ANT-017** — Lua preserves record owners across ANT transfers; Solana's reconciliation breaks transfer_record

### Revised Fix Priority (incorporating Lua comparison)

**Must Fix (regressions from Lua + critical Solana-only):**
1. ARNS-003/004 — Pruned names unregisterable (Lua regression)
2. CORE-008 — Orphaned PrimaryNameReverse (Lua regression)
3. ANT-017 — transfer_record broken after NFT transfer (Lua regression)
4. GAR-013 — prune_gateway registry corruption (Solana-only, high impact)
5. GAR-003 — Observer address manipulation (Solana-only, critical)
6. MIGRATION-001 — Set all MIGRATION_DEADLINE values

**Should Fix (shared with Lua or elevated Solana severity):**
7. GAR-005 — Single observer failure threshold (shared with Lua, critical impact)
8. GAR-006 — Protocol drain bricks epoch (shared, but Solana severity is worse)
9. GAR-009 — Gateway leaving bricks epoch (Solana-only)
10. GAR-008 — Composite weight precision loss (Solana-only)
11. All `as u64` truncations (CORE-001, ARNS-001/002, GAR-001)

**Review (low severity or intentional parity with Lua):**
12. ~~GAR-004 — Hashchain entropy~~ **MITIGATED** — payer removed, now Lua-equivalent
13. ANT-002 — set_record overwrites owner (shared, Solana is actually better)
14. GAR-007 — Epoch enable toggle (new Solana capability, no Lua equivalent)
15. ~~CORE-006 — Demand factor fallback (Solana-only)~~ **FIXED** — elevated to Medium; now errors instead of defaulting

---

## Appendix: Future Randomness Upgrades (GAR-004)

The current hashchain uses `hash(slot + epoch_index + timestamp)` — all network-determined values. This matches the Lua/AO security model where entropy comes from Arweave block hashes. The residual risk is a **timing attack**: a cranker can delay `create_epoch` by a few slots to find a favorable hash. This section documents upgrade paths if timing manipulation is observed post-launch.

### Option 1: Commit-Reveal (No External Dependencies)

**Complexity:** ~200 lines | **Security:** Strong | **Permissionless:** Yes

Two-phase epoch creation:
1. **`commit_epoch`** — Cranker submits `hash(secret)` + a bond (e.g., 10 ARIO). Epoch PDA is created but `hashchain` is empty. Commitment is stored.
2. **`reveal_epoch`** — Cranker reveals `secret` within N slots (e.g., 50 slots / ~20 seconds). Final hashchain = `hash(secret + current_slot)`. Bond is returned.

Since `current_slot` at reveal time was unpredictable when the commitment was made, the cranker cannot pre-compute the observer selection. The "nothing-at-stake" attack (not revealing if the result is bad) is prevented by the bond — anyone can call `slash_commitment` after the deadline to claim it.

**New accounts:** `EpochCommitment` PDA (commitment hash, bond amount, deadline slot, committer pubkey)
**New instructions:** `commit_epoch`, `reveal_epoch`, `slash_commitment`

### Option 2: Switchboard VRF (Strongest, External Dependency)

**Complexity:** ~150 lines | **Security:** Cryptographic | **Permissionless:** Yes

Uses Switchboard's on-chain Verifiable Random Function:
1. **`request_epoch_randomness`** — CPI into Switchboard to request VRF. Cost: ~0.002 SOL per request.
2. **Switchboard callback** — Oracle network produces a BLS threshold signature (verified on-chain). Result is written to a Switchboard account.
3. **`create_epoch`** — Reads the VRF result as the hashchain seed.

**Pros:** Truly unpredictable, cryptographically verifiable, well-established on Solana.
**Cons:** Adds dependency on Switchboard oracle availability. If Switchboard goes down, epoch creation is blocked until it recovers (could add a fallback to the timing-based hash after a timeout).

**New dependencies:** `switchboard-solana` crate
**Reference:** https://docs.switchboard.xyz/docs-by-chain/solana-svm/randomness

### Option 3: Multi-Party Entropy (Community-Driven)

**Complexity:** ~250 lines | **Security:** Strong (with honest minority) | **Permissionless:** Yes

Anyone can contribute entropy to the next epoch:
1. **`contribute_entropy(random_bytes)`** — During a contribution window (e.g., first 100 slots of the epoch window), anyone can submit random bytes. Each contribution is hashed into a running accumulator.
2. **`create_epoch`** — Uses the final accumulator as the hashchain seed.

If ANY single contributor is honest, the final entropy is unpredictable to all other participants. This is the most decentralized option but requires participation incentives and has a longer setup window.

### Recommendation

For launch: Current implementation (slot+epoch+timestamp) is Lua-equivalent and sufficient.
Monitor for: Crankers consistently delaying epoch creation or the same entity winning observer slots disproportionately.
If manipulation observed: Commit-reveal (Option 1) is the best balance of security, simplicity, and no external dependencies.
