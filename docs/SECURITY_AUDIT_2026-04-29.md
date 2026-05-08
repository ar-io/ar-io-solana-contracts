# Security Audit — AR.IO Solana Programs

**Date**: 2026-04-29
**Branch**: feat/ario-ant-escrow
**Scope**: All 5 on-chain programs — `ario-core`, `ario-gar`, `ario-arns`, `ario-ant`, `ario-ant-escrow`
**Method**: Two-pass audit. Pass 1 — Solana Foundation security checklist applied per-program by 5 parallel auditors. Pass 2 — independent verification of every finding against actual code + intent docs (BD-XXX entries, ADRs, design docs, prior audits).
**Status**: Pre-mainnet. Some findings are pre-existing in `SECURITY_AUDIT.md` / `SECURITY_AUDIT_INDEPENDENT.md` and are cross-referenced.

---

## 1. Executive Summary

After verification, the portfolio holds up better than the first-pass audit suggested. Most claimed Highs survive verification, but several severity calls inverted:

- **ario-core's vault dust DoS was understated** — it is the only true Critical (was originally ranked High). Trivial 1-mARIO attack permanently bricks any vault, including all migration-imported vaults.
- **ario-core's "permissionless initialize" was overstated** — it's a known Anchor pattern documented in `SECURITY_AUDIT.md:322`, mitigated by atomic deploy+init. Real but downgraded from Critical to High.
- **ario-ant-escrow's "canonical message no recipient binding" was a false alarm** — the existing nonce check at `claim_arweave.rs:50` already invalidates pre-rotation signatures. Defense-in-depth recommendation only.
- **ario-arns's "returned-name auctions unbuyable" was overstated** — a permissionless workaround exists (anyone can `InitializeAccount` an SPL token account owned by the config PDA, then the handler's `is_protocol_initiator` branch executes correctly). Still a real UX/SDK gap.
- **One ario-gar finding (F15 missing bump) was REFUTED** — bump validation is correctly present.

### Final tally

| Program | Critical | High | Medium | Low | Info | New found in verification |
|---|---|---|---|---|---|---|
| ario-core | **1** | 1 | 3 | 3 | 5 | 4 |
| ario-gar | 0 | **3** | 5 | 6 | 6 | 4 |
| ario-arns | 0 | 0 | 4 | 9 | 7 | 4 |
| ario-ant | 0 | 1 | 1 | 6 | 4 | 4 |
| ario-ant-escrow | 0 | 0 | 3 | 6 | 5 | 2 |
| **Total** | **1** | **5** | **16** | **30** | **27** | **18** |

Net findings after verification: **79 confirmed** (excluding the 1 refuted), with 18 new sub-findings spotted during verification. Total addressable items: 95 (including by-design items that may still warrant mitigation).

---

## 2. Critical Findings

### C1. ario-core — Vault dust DoS bricks all locked tokens permanently
- **Severity**: **Critical** (promoted from High after verification)
- **Locations**: `programs/ario-core/src/instructions/vault.rs:372-378` (release_vault), `vault.rs:222-228` (revoke_vault) *[corrected 2026-04-30: original audit had ranges swapped]*
- **Verdict**: CONFIRMED
- **Code path verified**:
  ```rust
  token::transfer(cpi_ctx, amount)?;  // amount = stored vault.amount
  let close_accounts = anchor_spl::token::CloseAccount { ... };
  anchor_spl::token::close_account(CpiContext::new_with_signer(...))?;
  ```
  The H-1 fix from prior audits made `vault_token_account.owner == vault.key()` (a PDA), which made the address **publicly derivable**. SPL `CloseAccount` requires zero balance.
- **Attack**:
  1. Alice calls `create_vault(amount=1000_000000)`, locks 14 days.
  2. Mallory observes the tx, derives `vault_token_account` deterministically.
  3. Mallory transfers 1 mARIO to the address (~5000 lamports cost).
  4. 14 days later: Alice calls `release_vault`. `token::transfer(vault.amount)` succeeds, leaves 1 mARIO of dust. `close_account` reverts on non-zero balance. Tx atomically reverts. Tokens stay trapped.
  5. **No instruction can recover.** `revoke_vault` has the same flaw.
- **Impact**: Permanent loss of all locked tokens in any targeted vault. Migration-imported vaults inherit risk en masse. Cost to attacker: 1 mARIO + tx fees. Cost to victim: full vault balance.
- **Intent**: BD-053 documents the per-vault SPL Token Account design but says nothing about dust handling. The `close_account` call was added during the H-1 fix without considering the dust attack surface.
- **Recommendation** (cheapest first):
  1. Re-read live balance: `token::transfer(cpi_ctx, ctx.accounts.vault_token_account.amount)?;` — sweeps any dust to the owner along with the principal. Turns the attack into a small donation.
  2. Alternative: skip `close_account` if `vault_token_account.amount > 0` post-transfer; rent stays locked but principal recoverable.
  3. Add `drain_vault_dust` admin instruction as belt-and-braces.
- **Cross-references**: New issues N1 and N2 from the ario-core verification report relate directly to this:
  - N1: F2 escalates the L-4 cleanup risk; rent reclamation should be conditional on amount==0 post-transfer.
  - N2: `release_vault` uses stored `vault.amount` not live `vault_token_account.amount` — the entry point for the bug.

---

## 3. High Findings

### H1. ario-core — Permissionless `initialize` (deploy-time race)
- **Severity**: High (demoted from Critical after verification)
- **Location**: `programs/ario-core/src/instructions/initialize.rs:7-48`
- **Verdict**: CONFIRMED-BY-DESIGN
- **Code**: `payer: Signer<'info>` with no constraint binding to upgrade authority. `init` enforces single-call but doesn't gate who calls first.
- **Intent**: `SECURITY_AUDIT.md:322` already documents this: *"All four programs' initialize instructions have no authority check beyond init (one-time PDA). First caller becomes authority. Standard for Anchor but worth deploying + initializing atomically."*
- **Why demoted from Critical**: Established Anchor pattern with established mitigation (atomic deploy+init bundle). Window is bounded to one slot if the runbook is followed.
- **Why still High**: Treasury cannot be rotated post-init (see L1 below), so a missed init is permanently unfixable. Same shape exists in ario-gar, ario-arns, ario-ant initialize.
- **Recommendation**: Bind `Initialize` to `BPFLoaderUpgradeable::ProgramData.upgrade_authority_address` in all 4 programs (ario-ant already does this for `initialize_migration` — port the pattern). At minimum, codify atomic deploy+init in `DEVNET_RUNBOOK.md` as a CI-enforced requirement.

### H2. ario-gar — Mid-epoch `leave_network`/`prune_gateway` corrupts `failure_counts` (slot aliasing)
- **Severity**: High
- **Locations**: `observation.rs:57-68`, `distribution.rs:66-67`, `gateway.rs:165-201` (leave_network swap-remove), `gateway.rs:399-436` (prune_gateway swap-remove)
- **Verdict**: CONFIRMED — **NOT covered by any prior audit**. New finding.
- **Code path**: Observers' `gateway_results` bitmaps are indexed by current registry position. `leave_network` and `prune_gateway` both swap-remove the leaving gateway with the last registry entry. After the swap, `epoch.failure_counts[i]` accumulates failures attributed to the old gateway + passes attributed to the new gateway that took the slot.
- **Attack scenarios**:
  - **Defensive**: A failing gateway on the verge of being slashed calls `leave_network`. Their failure_counts are inherited by an innocent gateway that swap-removed into their slot. The innocent operator gets `failed_consecutive` incremented and may be pruned.
  - **Offensive**: A gateway operator at `failed_consecutive = N-1` waits for a known-bad gateway elsewhere to leave/be-pruned, then lands at the contaminated slot.
- **Impact**: Innocent gateways receive 0 reward (not even `per_gateway`); cumulatively, repeated mid-epoch leaves can prune honest operators by inheriting tally votes.
- **Intent**: GAR-009 in `SECURITY_AUDIT_INDEPENDENT.md` covered the same root cause for `tally_weights` (default-slot skip added). The fix was not propagated to either `distribute_epoch` (see H3) or to the `failure_counts` aliasing surface.
- **Recommendation**: Two paths, choose one:
  1. Block `leave_network` / `prune_gateway` while `current_epoch.distribution_index < current_epoch.active_gateway_count`. Operators wait until the active epoch finishes distribution before exiting.
  2. Re-key failure_counts by `Pubkey` (or by `gateway.registry_index` snapshotted at `create_epoch`, with swap-remove updating both ends atomically). Pubkey-keyed is simpler — store `[Pubkey; max_observers]` of failed-gateway addresses per observer, aggregated in `tally_weights`.

### H3. ario-gar — `distribute_epoch` permanently stalls if any gateway leaves between create and distribute
- **Severity**: High
- **Location**: `programs/ario-gar/src/instructions/distribution.rs:34-57`
- **Verdict**: CONFIRMED — same root cause as GAR-009 but the fix only landed in `tally_weights`.
- **Code path**: After `leave_network`/`prune_gateway`, registry has `Pubkey::default()` slots in `[registry.count, active_gateway_count)`. `distribute_epoch`'s validation `registry.gateways[dist_idx].address == gateway.operator` cannot be satisfied. `distribution_index` never advances past `registry.count`. Epoch becomes immortal — `close_epoch` requires `rewards_distributed != 0`.
- **Impact**: Permanent DoS of epoch reward distribution. Recoverable only by program upgrade. ~9.4 KB rent locked per stuck epoch.
- **Recommendation**: Mirror the GAR-009 skip (`epoch.rs:154-161`) into `distribute_epoch`:
  ```rust
  if registry.gateways[dist_idx].address == Pubkey::default() {
      dist_idx += 1;
      continue;
  }
  ```
  Decide whether the default-slot iteration consumes a `remaining_accounts` entry (consistent with `tally_weights`) or not (cleaner) and document.

### H4. ario-gar — Observer/name selection entropy is grindable
- **Severity**: High (BY-DESIGN, future-VRF target)
- **Location**: `programs/ario-gar/src/instructions/epoch.rs:79-86`
- **Verdict**: CONFIRMED-BY-DESIGN. Comment lines 72-78 acknowledge the timing attack residual risk; GAR-004 in `SECURITY_AUDIT_INDEPENDENT.md` documents the design intent of a future VRF upgrade.
- **Code path**: Hashchain seed = `clock.slot ‖ epoch_index ‖ unix_timestamp`. All inputs publicly observable in advance. A `create_epoch` caller can simulate slots offline and submit at the favorable one. With a 24h epoch and ~400ms slots, ~216k candidate slots vs ~50 observer picks.
- **Impact**: A determined gateway operator can capture observer selection (10% bonus) and name prescription continuously.
- **Why still High**: Solana's slot timing is finer-grained than AO's tick interval, making the attack practical. The "future-VRF" mitigation is documented but not yet implemented. Pre-mainnet, this is exploitable from day 1.
- **Recommendation**: Switchboard VRF or Pyth Entropy. Short-term commit-reveal: sample `slot_hashes` at `prescribe_epoch` time (not `create_epoch`), with a forced `K`-slot delay. The current design has the right abstraction layer; only the entropy source needs swapping.

### H5. ario-ant — MPL Core AssetV1 layout is hand-decoded with no version pin
- **Severity**: High (BY-DESIGN per ADR-012)
- **Location**: `programs/ario-ant/src/lib.rs:570-579`
- **Verdict**: CONFIRMED-BY-DESIGN
- **Code**:
  ```rust
  fn read_mpl_core_owner(data: &[u8]) -> Result<Pubkey> {
      require!(data.len() >= 33, AntError::InvalidAsset);
      require!(data[0] == 1, AntError::InvalidAsset);
      let owner_bytes: [u8; 32] = data[1..33].try_into()...;
      Ok(Pubkey::from(owner_bytes))
  }
  ```
- **Intent**: ADR-012 (`docs/DECISIONS.md:482`) documents the choice: *"avoids the mpl-core Rust crate, which doesn't compile on Cargo 1.79 bundled with Solana 2.1.0 SBF tools."* Already tracked as ANT-003 / KDL-2 in `SECURITY_AUDIT_INDEPENDENT.md:38, 292, 38`.
- **Why still High**: This function is the sole gateway to NFT-holder identity for **every authorization check** in ario-ant. A single MPL Core upgrade that reorders AssetV1 fields silently flips authorization across all ANTs. Same hand-decoded pattern appears in 3 other programs (see Theme A below).
- **Recommendation**: Centralize MPL Core deserialization in a workspace crate (`mpl-core-readonly`). Pin the deployed mpl-core program-data hash on each cluster via CI. Add a startup-log canary that asserts AssetV1 layout against a fixture extracted from devnet.

---

## 4. Medium Findings

### M1. ario-core — `import_account` does not bind discriminator to seed template
- **Location**: `programs/ario-core/src/migration.rs:41-72`
- **Verdict**: CONFIRMED-BY-DESIGN (documented in inline `SECURITY:` comment lines 101-103; ADR-010)
- **Mitigations in place**: `MIGRATION_DEADLINE = 2026-06-18` + `finalize_migration`.
- **Recommendation**: Per-discriminator seed-template check (cheap, ~50 CU). Independent audit M-1 covers same shape.

### M2. ario-core — No integration tests for any migration instruction
- **Location**: `programs/ario-core/tests/integration.rs` (61 tests; none cover `import_account`/`import_balance`/`finalize_*`)
- **Verdict**: CONFIRMED. Highest-risk single transaction batch in the project has zero test coverage.
- **Recommendation**: Add round-trip tests per discriminator type, authority gating, deadline gating, post-finalize rejection.

### M3. ario-core — `verify_arns_record_active` fails open on malformed/short ArnsRecord data
- **Location**: `programs/ario-core/src/instructions/primary_name.rs:130-166`
- **Verdict**: CONFIRMED. Every length check uses `if`, not `require!`; failure path falls through to `Ok(())`. `unwrap_or([0; 4])` and `unwrap_or([0; 8])` silently substitute zeros.
- **Recommendation**: Convert outer `if`s to `require!`. Long-term: switch to `AccountLoader<ArnsRecord>` from a shared `ario-types` crate.

### M4. ario-gar — `migrate_settings_set_arns_program_id` has no `migration_active`/`MIGRATION_DEADLINE` gate
- **Severity**: demoted from High → Medium after verification
- **Location**: `programs/ario-gar/src/migration.rs:193-246`
- **Verdict**: PARTIAL / BY-DESIGN. Inline comment (lines 187-192) explicitly states `migration_active` is intentionally NOT checked: *"purpose of this instruction is to repair settings deployed before the field existed."*
- **Why still Medium**: Authority can write any pubkey (no executable-program validation). See N-GAR-1 below.
- **Recommendation**: Add `arns_program_id.executable == true` check (verifiable via account info), or restrict the setter to a one-shot fuse.

### M5. ario-gar — `distribute_epoch` over-credits gateway operator stake when protocol balance insufficient
- **Location**: `programs/ario-gar/src/instructions/distribution.rs:118-153, 185-216`
- **Verdict**: CONFIRMED. **GAR-006 fix is incomplete** — only capped the SPL transfer, not the in-memory accounting.
- **Attack**: If protocol treasury was drained between `create_epoch` (snapshotted total_eligible_rewards) and `distribute_epoch` (executes transfer), recorded `gateway.operator_stake` exceeds actual stake_token_account balance. First-come-first-served withdrawal griefing.
- **Recommendation**: Pro-rate per-gateway `operator_reward` and `cumulative_reward_per_token` increment by `transfer_amount / batch_total_reward` so accounting and tokens stay in sync.

### M6. ario-gar — Migration imports do not deduplicate `GatewayRegistry` slot addresses
- **Location**: `programs/ario-gar/src/migration.rs:139-166` (esp. 158-163)
- **Verdict**: CONFIRMED-BY-DESIGN (relies on snapshot tooling)
- **Recommendation**: Cheaper than full O(N) scan: have handler also take `name_hash`/operator-pubkey and require it match a paired account passed alongside. Add explicit duplicate-detection to `migration/import` orchestrator.

### M7. ario-gar — Imported observer addresses skip the `ObserverLookup` uniqueness PDA
- **Location**: `programs/ario-gar/src/migration.rs:55-137` vs `gateway.rs:524-532`
- **Verdict**: CONFIRMED. `join_network` enforces uniqueness via init constraint on `ObserverLookup` PDA at `[OBSERVER_LOOKUP_SEED, observer_address]`. Migration imports don't create this PDA.
- **Impact**: Two imported gateways could share `observer_address`. After finalize_migration, the conflict is permanent and blocks `update_observer_address` for both.
- **Recommendation**: Add `import_observer_lookup` instruction OR include ObserverLookup PDAs in the existing import set.

### M8. ario-gar — Closed epochs orphan their `Observation` PDAs (rent permanently lost)
- **Location**: `programs/ario-gar/src/instructions/epoch.rs:435-452`, `observation.rs:87-95`
- **Verdict**: CONFIRMED. `close_observation` requires loading parent epoch via PDA seeds — fails after epoch is closed.
- **Recommendation**: Either (a) require `observations_closed == observer_count` before allowing `close_epoch`, or (b) remove the AccountLoader<Epoch> requirement from `close_observation` and validate purely via stored `epoch_index`.

### M9. ario-arns — Protocol-initiated returned-name auctions are broken in practice
- **Severity**: demoted from High → Medium after verification
- **Locations**: `programs/ario-arns/src/instructions/purchase.rs:519-524`; same constraint in `purchase_from_stake.rs:734-739, 820-823`
- **Verdict**: PARTIAL. Original claim "unbuyable" was wrong — SPL `InitializeAccount` accepts arbitrary `owner` field with no signature, so anyone can permissionlessly create a token account owned by the `arns_config` PDA. Then the handler's `is_protocol_initiator=true` branch executes correctly (skips transfer). Real UX/SDK gap: integrators using the SDK pattern see all protocol-initiated auctions fail with `AccountNotInitialized` until manual ATA creation.
- **Test gap**: `tests/integration.rs:1358` (`test_buy_returned_name`) only covers `release_name → buy` (initiator == caller). NO test exercises `prune_to_returned → buy_returned_name`.
- **Recommendation**: Drop the constraint; gate the ownership check inside the handler when `!is_protocol_initiator`. Add the missing integration test.

### M10. ario-arns — `reassign_name` allows no-op reassign that wipes plugin
- **Location**: `programs/ario-arns/src/instructions/manage.rs:260-307`
- **Verdict**: CONFIRMED. Handler unconditionally clears the plugin on the OLD asset (lines 292-299) with `update_attributes_plugin(..., &[])`, then sets `record.ant = new_ant`. No `require!(new_ant != record.ant)` guard.
- **Attack**: ANT holder repeatedly calls `reassign_name(record.ant)` (no-op reassign) — wipes plugin each tx. Marketplaces/DAS see flickering traits.
- **Recommendation**: One-line fix at top of handler: `require!(new_ant != record.ant, ArnsError::InvalidParameter);`.

### M11. ario-arns — Fund-from-stake purchase variants do not sync MPL Core Attributes plugin
- **Location**: `programs/ario-arns/src/instructions/purchase_from_stake.rs:69-560` (4 variants), `manage_from_stake.rs` (4 variants)
- **Verdict**: CONFIRMED. ADR-012 (`docs/DECISIONS.md:472`) explicitly mandates: *"every ARIO-ARNS handler that mutates trait-affecting state CPIs into MPL Core's UpdatePluginV1."* The 8 `_from_stake` variants violate this design promise.
- **Impact**: DAS / marketplaces show stale traits until someone manually calls `sync_attributes`. Front-running window for trait-dependent UIs.
- **Recommendation**: Add `mpl_core_program` and `system_program` to all 8 fund-from-stake account structs and call `update_attributes_plugin` post-mutation. ABI-breaking change — bundle into one PR.

### M12. ario-ant — `import_account` allows hot migration authority to overwrite live records pre-finalization
- **Location**: `programs/ario-ant/src/migration.rs:219-301` (esp. 277-298)
- **Verdict**: CONFIRMED-BY-DESIGN. Inline `SECURITY:` comment + already documented as MIGRATION-001 (FIXED with deadline) in `SECURITY_AUDIT_INDEPENDENT.md`.
- **Specific attack scenario**: Hot key compromise → write `last_known_owner = current_nft_owner` (read off-chain) AND `controllers = [attacker]`. Reconciliation in `state.rs:289-304` sees no change → does NOT clear → attacker is now a legitimate controller with full record/metadata authority.
- **Recommendation**: Tighten `MIGRATION_DEADLINE` to T+72h after cutover. Operate migration_authority as cold multisig for sensitive imports. Consider deprecating idempotent re-import in favor of strictly create-only after first successful import.

---

## 5. Low Findings (summary table)

| ID | Program | Title | Verdict | File:Line |
|---|---|---|---|---|
| L1 | ario-core | Treasury cannot be rotated post-init (compounds H1) | CONFIRMED-BY-DESIGN | `admin.rs:7-47` |
| L2 | ario-core | `Clock` can move backwards on validator restart | INFO (downgraded) | `vault.rs:255-283` |
| L3 | ario-core | `ImportAccount` accepts unbounded `seeds` | CONFIRMED | `migration.rs:41-99` |
| L4 | ario-gar | F1 corruption has no repair path (NEW) | CONFIRMED | n/a — design gap |
| L5 | ario-gar | `redelegate_stake` validates `delegator_token_account` but never uses it | CONFIRMED | `delegate.rs:243-247, 633` |
| L6 | ario-gar | `space = 8 + EpochSettings::SIZE` over-allocates by 8 bytes | CONFIRMED | `initialize.rs:206`, `observation.rs:114` |
| L7 | ario-gar | `join_network` does not validate `params.observer_address != Pubkey::default()` | CONFIRMED | `gateway.rs:75-76` |
| L8 | ario-gar | `cancel_withdrawal` rejects when delegation disabled | INFO (Lua parity) | `withdrawal.rs:156-159` |
| L9 | ario-arns | `read_mpl_core_owner` truncation defense-in-depth | CONFIRMED (low risk) | `state/mod.rs:465-473` |
| L10 | ario-arns | `reassign_name` grace-period guard is dead code | CONFIRMED | `manage.rs:273-279` |
| L11 | ario-arns | `sync_attributes` no rate limit (self-griefing only) | CONFIRMED-BY-DESIGN | `manage.rs:646-678` |
| L12 | ario-arns | ~~`extend_lease` lacks mpl_core/system_program (latent)~~ → **N/A** *[2026-04-30: end_timestamp not surfaced as MPL Core trait; absence is intentional]* | RECLASSIFIED | `manage.rs:451-491` |
| L13 | ario-arns | `try_apply_gateway_discount` reads only remaining_accounts[0] | CONFIRMED-BY-DESIGN | `pricing.rs:260-330` |
| L14 | ario-arns | `import_account` does not cross-validate name_hash against seeds | CONFIRMED-BY-DESIGN | `migration.rs:44-126` |
| L15 | ario-arns | `release_name` does not verify registry removal succeeded | CONFIRMED-BY-DESIGN | `manage.rs:365-382` |
| L16 | ario-arns | `buy_name to_lowercase` consistency for non-ASCII (unreachable) | CONFIRMED | `purchase.rs:142, 318` |
| L17 | ario-ant | `record.last_reconciled_owner` desync window | CONFIRMED-BY-DESIGN | `lib.rs:165-178` |
| L18 | ario-ant | `set_record` lets record-delegate update content | CONFIRMED-BY-DESIGN | `lib.rs:206-221` |
| L19 | ario-ant | `initialize` sets owner as both owner and controller | CONFIRMED-BY-DESIGN | `lib.rs:93-96` |
| L20 | ario-ant | `read_mpl_core_owner` doesn't require asset writable (correct) | CONFIRMED-BY-DESIGN | `lib.rs:570-579` |
| L21 | ario-ant-escrow | Canonical message defense-in-depth (was H, downgraded) | PARTIAL | `canonical.rs:65-73` |
| L22 | ario-ant-escrow | Missing AssetV1 discriminator check on claim/cancel paths | CONFIRMED | `cancel.rs:65-72`, `claim_*.rs` |
| L23 | ario-ant-escrow | Escrow does not custody UpdateAuthority (NEW concern, was Info) | PARTIAL | `mpl_core_cpi.rs` |
| L24 | ario-ant-escrow | EIP-191 length prefix uses `usize::to_string()` (correct) | CONFIRMED | `verify/ethereum.rs:81-87` |
| L25 | ario-ant-escrow | PSS heap allocations (~1.5KB; well under 32KB) | CONFIRMED-BY-DESIGN | `verify/arweave.rs:150,175,200-211` |
| L26 | ario-ant-escrow | `is_s_low` not constant-time on BPF (`s` is public anyway) | CONFIRMED-BY-DESIGN | `verify/ethereum.rs:132-153` |

### L23 needs special attention — the new escrow concern

ADR-013 (`docs/DECISIONS.md:500-547`) states that AR.IO ANTs are minted with `Owner == UpdateAuthority`, and the migration claim flow CPIs **both** `TransferV1` + `UpdateV1` atomically. ADR-014 (escrow) does NOT mention UpdateAuthority. After escrow claim, recipient gets `Owner = claimant` but `UpdateAuthority = depositor`.

**A malicious depositor can subsequently fire `UpdateV1` to swap the metadata URI on the recipient's asset post-claim.** Plugin Authority (= Owner) gates trait edits, so on-chain Attributes traits are safe — but the JSON URI (which holds image/description) is mutable by the wrong party.

**Recommendation**: Add `UpdateV1(newUpdateAuthority = claimant)` to both `claim_arweave` and `claim_ethereum` CPIs. Mirror `migration/import/src/claim-transfers.ts::transferNft`. Cancel-deposit unaffected (depositor reclaims UA they already had).

---

## 6. Informational Findings (summary table)

| ID | Program | Title |
|---|---|---|
| I1 | ario-core | Mint authority not held by program (intended; off-chain multisig) |
| I2 | ario-core | `name` not deduplicated by content (intentional; reverse_lookup PDA enforces uniqueness) |
| I3 | ario-core | No event emission for primary-name removal / vault extend/increase |
| I4 | ario-core | `unwrap_or_default()` upgrade-authority pattern (workspace-wide; see Theme C) |
| I5 | ario-core | `ArioError::AlreadyInitialized` is dead code |
| I6 | ario-gar | `set_epochs_enabled` re-enable is instant; only disable has 7-day timelock |
| I7 | ario-gar | `prescribe_epoch` worst-case CU `O(observers² + observers·active_count·10)` |
| I8 | ario-gar | `distribute_epoch` saturating_add for stake credits (unreachable) |
| I9 | ario-gar | Sybil cost to fill `GatewayRegistry` (~3M ARIO net) |
| I10 | ario-gar | `import_account_handler` allows idempotent overwrite (documented) |
| I11 | ario-gar | `RedelegationRecord` PDA never closed (NEW; rent leak) |
| I12 | ario-gar | `close_epoch` min_gap hardcoded to 7 (NEW; should use epoch_settings) |
| I13 | ario-arns | `prune_returned_names` rent flows to first-mover (standard MEV pattern) |
| I14 | ario-arns | Stale comment in `buy_name` handler |
| I15 | ario-arns | `MAX_ROLLOVER_PERIODS = 100` lazy-roll cap |
| I16 | ario-arns | `name_registry` swap-remove is O(N) linear scan |
| I17 | ario-arns | Hand-rolled MPL Core CPI byte-perfect (Cargo 1.79 / Theme A) |
| I18 | ario-arns | CPI authority binding into ario-gar correctly enforces signer == staker (POSITIVE) |
| I19 | ario-ant | No fuse on `initialize_migration` for upgrade-authority leak before init |
| I20 | ario-ant | `MIGRATION_DEADLINE` hardcoded |
| I21 | ario-ant | `migrate_ant` body is a stub (forward-compat issue) |
| I22 | ario-ant | `RemoveRecord` returns rent to caller (could be controller, not owner) |
| I23 | ario-ant-escrow | Nonce derivable from public state (anti-replay only, not secret) |
| I24 | ario-ant-escrow | Compile-time network feature flag correctly enforced |
| I25 | ario-ant-escrow | Fuzz coverage gaps — **PARTIAL** *[2026-04-30: fuzz targets `verify_personal_sign` + `verify_rsa_pss` and corpus added in 1190271; positive-path differential fuzz still missing]* |
| I26 | ario-ant-escrow | ~~Integration tests don't cover claim paths e2e~~ — **FIXED** *[2026-04-30: three e2e claim tests added in 9c17551 at `tests/integration.rs:1115-1284`]* |
| I27 | ario-ant-escrow | `update_recipient` requires Solana signer (design intent per ADR-014) |

---

## 7. New Issues Discovered During Verification

The verification pass also found 18 issues that were not in the first-pass audit:

### N-CORE-1 [Informational] `MIGRATION_DEADLINE` hardcoded with brittle pre-deploy step
- **Location**: `programs/ario-core/src/migration.rs:13`
- Comment "Update before mainnet if migration date changes." Should be a config field set during `initialize`.

### N-CORE-2 [Low] `release_vault` uses stored `vault.amount` not live `vault_token_account.amount`
- The entry point for C1. See remediation under C1.

### N-CORE-3 [Informational] `import_balance` uses typed account; `import_account` doesn't (asymmetric)
- Suggests deprecating generic `import_account` in favor of typed handlers per account kind.

### N-CORE-4 [Low] Vault `close_account` was added in L-4 cleanup but BD-053 design didn't mandate it
- Rent reclamation should be conditional on `vault_token_account.amount == 0` post-transfer.

### N-GAR-1 [Medium] `migrate_settings_set_arns_program_id` does not validate `arns_program_id` is executable
- **Location**: `programs/ario-gar/src/migration.rs:240`
- Writes pubkey raw without verifying `account.executable == true` or `owner == BPFLoaderUpgradeable`. A typo bricks `prescribe_epoch:347-355`.
- Combined with M4: only safety is the authority's keyboard.
- **Recommendation**: Pass the actual program account; `require!(arns_program_account.executable, ...)`.

### N-GAR-2 [Low] `Epoch.failure_counts` corruption from H2 has no repair instruction
- After mid-epoch leave/prune corrupts `failure_counts`, no admin/permissionless fix path. Affected gateways have no redress.
- **Recommendation**: Either prevent the corruption (per H2 fix), or add an admin `repair_failure_counts(epoch_index, idx, new_value)` for emergency cleanup.

### N-GAR-3 [Informational] `RedelegationRecord` PDA never closed — **DUPLICATE of I11**
- **Location**: `programs/ario-gar/src/instructions/delegate.rs:619-626`
- Uses `init_if_needed` but no `close_redelegation_record` instruction. Stale records leak rent.
- *[2026-04-30: same finding as I11; track under I11 only.]*

### N-GAR-4 [Informational] `close_epoch` min_gap hardcoded to 7 epochs — **DUPLICATE of I12**
- **Location**: `programs/ario-gar/src/instructions/epoch.rs:444`
- Should be parameterized via `epoch_settings.epoch_retention` for admin tunability.
- *[2026-04-30: same finding as I12; track under I12 only.]*

### N-ARNS-1 [Informational] `prune_to_returned` permissionless rent transfer
- **Location**: `programs/ario-arns/src/instructions/prune.rs:201-261, 315`
- Original lease holder paid `ArnsRecord` rent; ~0.003 SOL value transfers to pruner. Standard MEV pattern but worth a BD entry for transparency.

### N-ARNS-2 [Medium] `manage_from_stake` variants also lack plugin sync (extension of M11)
- `upgrade_name_from_*` and `increase_undername_limit_from_*` mutate trait-affecting state but don't CPI `UpdatePluginV1`. ADR-012 lists these as plugin-syncing; the `_from_stake` siblings inherit the responsibility.
- **Recommendation**: Bundle into the M11 fix.

### N-ARNS-3 [Medium] `reassign_name` allows reassign to any MPL Core asset
- **Location**: `programs/ario-arns/src/instructions/manage.rs:282-286`
- Only checks `*new_ant_asset.owner == MPL_CORE_PROGRAM_ID`. Any MPL Core asset (even one without an Attributes plugin, even from an unrelated program) can be reassigned in. `sync_attributes` recovery would then fail.
- **Recommendation**: Require new asset to carry an Attributes plugin (read first 8 bytes of plugin region, assert plugin variant 6); long-term, require AR.IO collection key.

### N-ARNS-4 [Informational] Blanket reservation may block name forever
- **Location**: `programs/ario-arns/src/instructions/purchase.rs:67-83`
- A reservation with `reserved_for=None` and `expires_at=None` blocks the name without an `unreserve_name` admin escape. Worth confirming this is the intended admin-only escape.

### N-ANT-1 [Informational] `migrate_ant` only reaches `AntConfig`
- `AntControllers` and `AntRecord` aren't in `MigrateAnt` accounts and have no `version: u8` field. Future schema changes to those types require new instructions. Adding `version` now (1 byte) makes future migration feasible.

### N-ANT-2 [Informational] F1 source comment was already corrected
- Current text at `lib.rs:566-568` no longer claims a Cargo pin; it warns to monitor. ANT-003 follow-up.

### N-ANT-3 [Informational] `unwrap_or_default()` upgrade-authority pattern duplicated across all 4 programs
- See Theme C below.

### N-ANT-4 [Informational] No runtime canary for hand-rolled MPL Core AssetV1 layout
- See Theme A below.

### N-ESCROW-1 [Informational] `update_recipient` takes no asset account
- Doesn't validate ANT is still in escrow custody. Bounded grief — depositor only damages own escrow.

### N-ESCROW-2 [Low] L21 + L23 interaction stronger than each in isolation
- Without recipient-pubkey binding in canonical message AND without UA transfer, a depositor who first re-targets with `update_recipient`, watches new recipient claim, and retains UpdateAuthority can subsequently rewrite the metadata URI on the claimed ANT.
- Both fixes (canonical message v2 + UpdateV1 in claim CPI) close the combined surface.

---

## 8. Cross-Cutting Themes (verified)

### Theme A: Hand-rolled MPL Core trust boundary (4 of 5 programs)
ario-core, ario-arns, ario-ant, ario-ant-escrow each independently decode MPL Core asset bytes by hardcoded offsets. Per ADR-012 this is a deliberate workaround for Cargo 1.79 / mpl-core compile incompat — but there's no centralized layout, no version pin, no compile-time IDL/fixture test, and no runtime canary.

**Systemic fix**: Create `mpl-core-readonly` crate inside this workspace. All four programs depend on it. Inside it: byte-layout, magic-byte assertions, and a fixture test against a real captured AssetV1 blob from devnet. CI: pin the deployed mpl-core program-hash and fail the build on drift. Add a runtime sanity log on first interaction per upgrade.

Affected: H5 (ario-ant), L9 (ario-arns), L22 (ario-ant-escrow), and the equivalent in ario-core's `primary_name.rs`.

### Theme B: Migration import handlers are too generous (3 of 5 programs)
ario-core, ario-gar, ario-arns each expose `import_account` / `import_registry_entry` to a hot `migration_authority` key with: (a) overwrite-allowed-during-window semantics, (b) inadequate seed-vs-discriminator cross-binding, (c) no duplicate detection in registry imports (gar, arns), (d) no field-vs-seed consistency checks.

`MIGRATION_DEADLINE` (2026-06-18) + `finalize_migration` are real mitigations and well-documented (ADR-010, MIGRATION-001 marked FIXED). The in-window blast radius remains large: hot-key compromise = arbitrary state corruption.

**Systemic fix**: Single shared migration-validation helper:
1. Build an explicit `discriminator → expected_seed_template` table.
2. Cross-check data-fields against seed-fields (e.g., `record.name_hash == seeds[1]`).
3. Reject duplicates in any registry-style account.
4. Flip to "import-once mode" after bulk-load (separate flag stops overwrite while leaving `finalize_migration` as final fuse).
5. Tighten `MIGRATION_DEADLINE` to T+72h after cutover.

Affected: M1, M6, M7, M12, plus N-GAR-1.

### Theme C: Bootstrap (initialize-time) authority capture (all 5 programs)
ario-core's H1 is the most exposed because it controls the treasury, but every program has the same pattern. The window is small (one slot) but real — and a deploy script that fails for any reason (RPC retry, slot skip) opens the window for a mempool watcher.

Plus the related `unwrap_or_default()` pattern (N-ANT-3, I4): every program has the same `program_data.upgrade_authority_address.unwrap_or_default()` constraint that silently coerces revoked authority to `Pubkey::default()` (System Program — fails closed by accident, but fragile).

**Systemic fix**: 
- Every program's `Initialize` constrains `payer.key() == ProgramData.upgrade_authority_address` via the BPFLoaderUpgradeable program-data PDA.
- ario-ant already does this for `initialize_migration` — port the pattern to the other four programs' `initialize`.
- Replace `unwrap_or_default()` with explicit `.ok_or(error!(...))?` workspace-wide.
- Alternative bootstrap: bundle initialize into the same tx as `solana program deploy --upgrade-authority` via the deploy runbook.

### Theme D: Cross-program byte-layout reads (ario-core ↔ ario-arns ↔ ario-gar)
ario-core reads `ArnsRecord` and `DemandFactor` from ario-arns by hand-decoded offsets (M3, partial M3-related observations). ario-arns reads `Gateway` from ario-gar by hand-decoded offsets. All three programs evolve independently. A field-reorder in any of them silently breaks consumers — fail-open in some paths.

**Systemic fix**: Publish a `ario-types` crate at the workspace root with canonical struct layouts and versioned discriminators. Every cross-program reader uses `try_deserialize_unchecked` from this crate. Layout changes become a coordinated workspace-wide event.

### Theme E: Test coverage gaps on the privileged paths (4 of 5 programs)
- ario-core: zero integration tests for any migration instruction (M2).
- ario-gar: no test for gateway-leaves-mid-epoch (would have caught H2 + H3).
- ario-arns: no test for `prune_to_returned → buy_returned_name` (would have caught M9).
- ario-ant-escrow: no integration test for either `claim_*` end-to-end (I26).

The pattern: highest-privilege paths (migration, claim, reward distribution) have lowest test coverage.

**Systemic fix**: Before mainnet, add regression tests for each of these gaps. Each would have caught real bugs in this audit.

### Theme F: Account closure / residue assumptions
- ario-core vault: SPL `close_account` zero-balance precondition (C1).
- ario-gar: `close_epoch` orphans Observation PDAs (M8).
- ario-gar: `RedelegationRecord` never closed (N-GAR-3).
- ario-ant: `migrate_ant` realloc shrinks credit lamports to attacker payer (latent — F2 in original audit, downgraded to Low).
- ario-arns: trusts caller-paid rent accounting on closure paths.

**Systemic fix**: Establish a workspace closure pattern (read live state, drain to expected recipient, zero discriminator, then close), and audit every `close = X` constraint against it.

---

## 9. By-Design Findings Cross-Referenced to Existing Audits

Several findings from this pass duplicate items already tracked in the prior audits:

| This audit | Prior audit | Status |
|---|---|---|
| H1 (initialize squat) | `SECURITY_AUDIT.md:322` | Documented; awaiting atomic-deploy enforcement |
| H4 (entropy grindable) | GAR-004 in `SECURITY_AUDIT_INDEPENDENT.md` | Documented; future VRF |
| H5 (MPL Core layout) | ANT-003 / KDL-2 | Documented; ADR-012 |
| M1 (import seed-vs-disc) | M-1 in `SECURITY_AUDIT_INDEPENDENT.md` | Mitigated by MIGRATION_DEADLINE + finalize |
| M3 (verify_arns_record_active) | Related to M-3 | Discriminator added; byte-layout coupling not fixed |
| M5 (over-credit on insufficient balance) | GAR-006 | Marked FIXED but fix is incomplete |
| M12 (import_account hot key) | MIGRATION-001 | Marked FIXED with deadline |

Items that are **NEW** (not in prior audits) include: **C1 (vault dust DoS), H2 (failure_counts swap-remove aliasing), H3 (distribute_epoch stuck), M9 (returned-name auctions broken), M10 (no-op reassign wipes plugin), M11 (fund-from-stake skip plugin sync), L23 (escrow doesn't custody UpdateAuthority)**, plus all 18 N-XXX findings from verification.

---

## 10. Recommended Remediation Order (before mainnet)

### Phase 1 — Critical & High (must-fix)
1. **C1** (1-line fix): `release_vault` and `revoke_vault` transfer `vault_token_account.amount` (live) instead of `vault.amount` (stored).
2. **H2 + H3** (medium fix): Block `leave_network`/`prune_gateway` mid-epoch OR re-key failure_counts by pubkey + mirror GAR-009 default-slot skip into `distribute_epoch`.
3. **M9** (small fix): drop the `initiator_token_account.owner` constraint and gate inside handler; add the missing test.
4. **M11 + N-ARNS-2** (ABI-breaking): add MPL Core sync to all 8 `_from_stake` purchase/manage variants. Bundle as one PR.
5. **M10** (1-line fix): `require!(new_ant != record.ant)` in `reassign_name`.
6. **L23** (atomic-CPI fix): bundle `UpdateV1(newUpdateAuthority = claimant)` into both escrow `claim_*` handlers.
7. **H1 + Theme C**: bind every program's `Initialize` to upgrade authority. Port the `initialize_migration` pattern.

### Phase 2 — Medium hardening
8. **H5 + Theme A**: extract `mpl-core-readonly` crate; pin layout; add CI canary.
9. **M1, M6, M7 + Theme B**: tighten migration import handlers; add seed-vs-discriminator binding; deduplicate registry imports; add ObserverLookup to import set.
10. **M5**: pro-rate `distribute_epoch` accounting by transfer ratio.
11. **M8**: require all Observations closed before `close_epoch`.
12. **M3, M4**: convert `verify_arns_record_active` `if`s to `require!`; add migration integration tests.
13. **N-GAR-1**: validate `arns_program_id` is executable.

### Phase 3 — Future-VRF (mainnet-blocker if still grinding-vulnerable on launch)
14. **H4**: Switchboard VRF or Pyth Entropy for `prescribe_epoch`; fall back to `slot_hashes`-based commit-reveal at minimum.

### Phase 4 — Test coverage (parallel with Phase 1)
- ario-core: migration handler tests
- ario-gar: leave-mid-epoch + prune-mid-epoch regression tests
- ario-arns: `prune_to_returned → buy_returned_name` test
- ario-ant-escrow: `claim_*` e2e tests with real RSA-PSS / secp256k1 sigs

### Phase 5 — Then sweep Lows + Informationals by program

---

## 11. External Audit Recommendation

After Phases 1–3 land, request external audit (Neodyme / OtterSec / Halborn). The Highs and shared themes are the kind of issues a fresh independent reviewer adds the most value to. C1 specifically should be remediated and re-tested before any audit engagement to avoid burning auditor time on a known-trivial fix.

---

## 12. Appendix — Per-Program Verification Tallies

| Program | Confirmed | By-Design | Partial | Refuted | Severity Up | Severity Down |
|---|---|---|---|---|---|---|
| ario-core | 7 | 4 | 1 | 0 | 1 (F2 → Critical) | 1 (F1 → High) |
| ario-gar | 12 | 3 | 3 | 1 | 0 | 4 |
| ario-arns | 8 | 5 | 3 | 0 | 0 | 4 |
| ario-ant | 6 | 5 | 2 | 0 | 0 | 3 |
| ario-ant-escrow | 4 | 7 | 2 | 1 | 1 (F13 → Low) | 1 (F1 → Low) |

**Refuted items** (false positives in original audit):
- ario-gar F15: bump validation IS present (`epoch.rs:524-528`)
- ario-ant-escrow F10: read-only claimant IS correct per MPL Core TransferV1 spec

---

## 13. Addendum — 2026-04-30 Re-Verification Pass

A second-pass re-verification was performed on 2026-04-30 against `main` (post-merge of `feat/ario-ant-escrow` PR #27, `feat/arns/permissionless` PR #33, and `feat/ant/rent-split` PR #34). Five parallel program-scoped audit agents re-checked all findings and applied the [Solana Foundation security checklist](https://github.com/solana-foundation/solana-dev-skill/blob/main/skill/references/security.md) end-to-end. Status changes are inlined above; new findings the original audit missed are listed here.

### 13.1 Status changes since 2026-04-29

- **C1**: line ranges corrected (release/revoke were swapped).
- **L12**: reclassified to N/A — `extend_lease` intentionally lacks `mpl_core_program` because `end_timestamp` is not surfaced as an MPL Core trait.
- **I25**: PARTIAL — fuzz targets and corpus added; positive-path differential fuzz still missing.
- **I26**: FIXED — three e2e claim tests landed in `tests/integration.rs:1115-1284`.
- **N-GAR-3 / N-GAR-4**: marked as duplicates of I11 / I12.
- **I22**: lever doubled by ant rent split (rent leak now affects both `AntRecord` and `AntRecordMetadata` PDAs on `RemoveRecord`). Severity remains Low; impact mention updated.
- **I21 / N-ANT-1**: exacerbated by ant rent split — three account types (`AntRecord`, `AntRecordMetadata`, `AntControllers`) still lack `version: u8` fields, increasing the cost of any future schema migration.
- **N-ARNS-2**: mechanically strengthened by permissionless merge — `ant_asset` was *removed* from `_from_stake` manage variant account structs (`manage_from_stake.rs:406-410, 472-473`), making sync impossible by design rather than just absent. Recovery is forced to `sync_attributes`.
- **I14**: stale comment at `purchase.rs:197` ("try_sync_attributes disabled for diagnostic") now actively contradicts live code at `purchase.rs:170`. Cleanup item.

### 13.2 New findings (skill-driven sweep)

#### NEW-1 [Medium] ario-ant — Orphaned `AntRecordMetadata` PDA on `RemoveRecord`
- **Location**: `programs/ario-ant/src/lib.rs::RemoveRecord`
- **Verdict**: CONFIRMED
- `record_metadata` is `Option<Account<…>>`. If a caller forgets to pass the metadata PDA when one exists for the `(ant_mint, undername)` pair, the core record closes but the metadata PDA orphans — rent permanently locked. No `has_one` / cross-reference enforces "if a metadata PDA exists for this record, it must be passed."
- **Family**: Theme F (account closure / residue assumptions). Same shape as M8 (ario-gar `Observation` PDAs orphaned by `close_epoch`).
- **Recommendation**: either make `record_metadata` non-optional and gate the close on its presence/absence at runtime, OR add a permissionless `close_orphaned_record_metadata` instruction with `close = original_payer`.

#### NEW-2 [Medium] ario-gar — `prescribe_epoch` fallback can select `Pubkey::default()` post-leave_network
- **Location**: `programs/ario-gar/src/instructions/epoch.rs:288-299`
- **Verdict**: CONFIRMED — same root cause as H3
- The "select last" fallback branch picks from `active_count - 1` even when that slot has been zeroed by a prior `leave_network` swap-remove. With `total_weight > 0` but a zeroed last-slot, the dedup check passes and `Pubkey::default()` is recorded as a "prescribed observer". Subsequent `save_observations` resolution via `remaining_accounts` raises a key-mismatch error → DoS of the prescription path.
- **Family**: Linked to H2/H3 root cause. Closes when option C (Lua-style status flag) lands; band-aid fix is a `Pubkey::default()` skip in the fallback branch.

#### NEW-3 [Low–Medium] ario-gar — `distribute_epoch` accepts any program-owned Gateway without checking `registry_index.is_registered`
- **Location**: `programs/ario-gar/src/instructions/distribution.rs:37-57`
- **Verdict**: CONFIRMED
- Validation requires program-ownership, writability, deserialization as `Gateway`, and PDA-match — but does NOT require `gateway.registry_index.is_registered == true`. A re-joined gateway whose old slot was zeroed could pass validation if the registry happens to still hold its operator pubkey at `dist_idx`.
- **Family**: H3 family / data-matching gap.
- **Recommendation**: add `require!(gateway.registry_index.is_registered, …)` and assert `registry.gateways[dist_idx].address == gateway.operator`.

#### NEW-4 [Low] ario-gar — `prescribe_epoch` silent-fail when observer count is 0
- **Location**: `programs/ario-gar/src/instructions/epoch.rs:340`
- **Verdict**: CONFIRMED (latent UX)
- If observer count is 0 and the cranker passes zero `remaining_accounts`, names are silently not prescribed (no error, no event). Operationally invisible failure.
- **Recommendation**: `require!(remaining.len() > selected_count, …)` or emit an explicit "no names prescribed" event.

#### NEW-5 [Informational] ario-arns — Permissionless `extend_lease` / `upgrade_name` enables demand-factor manipulation
- **Location**: `programs/ario-arns/src/instructions/manage.rs::extend_lease, upgrade_name`
- **Verdict**: CONFIRMED — intentional Lua parity, but worth documenting
- Any ARIO holder can now bid up `demand_factor.purchases_this_period` and `revenue_this_period` by extending or upgrading names they don't own. A whale can cheaply manipulate the demand factor, indirectly raising prices for legitimate buyers within the same period.
- **Recommendation**: Add a BD entry. No code fix needed (Lua does the same thing).

#### NEW-6 [Informational] ario-core — `import_account_handler` re-import path doesn't re-check existing PDA's discriminator
- **Location**: `programs/ario-core/src/migration.rs:104-111`
- **Verdict**: CONFIRMED
- When `account_info` is non-empty (re-import), checks `owner == ctx.program_id` and `data_len() == expected_size` but does NOT check the existing 8-byte discriminator matches `disc`. A previously-imported account whose seed-derivation collides with a different discriminator's `expected_size` could be overwritten with a different account type's data.
- **Practical exploitability**: requires hash collision on first 8 bytes of `hash("account:X")` — practically infeasible. Worth tightening anyway; ~50 CU.

#### NEW-7 [Informational] ario-arns — `manage_from_stake` variants explicitly removed `ant_asset` (intentional, but undocumented)
- **Location**: `programs/ario-arns/src/instructions/manage_from_stake.rs:406-410, 472-473`
- **Verdict**: CONFIRMED
- Subsumed by N-ARNS-2 status update above. Worth a BD entry: stake-funded manage variants intentionally cannot sync MPL Core Attributes; recovery is permissionless `sync_attributes`.

### 13.3 Reaffirmed (no change) findings worth re-emphasizing

- **L23 (escrow UpdateAuthority custody)**: zero references to `UpdateV1`, `update_authority`, or `new_update_authority` in `programs/ario-ant-escrow/src/`. The `migration/import/src/claim-transfers.ts::transferNft` pattern was not ported. Combined with L21, depositor can rewrite metadata URI on the recipient's claimed asset post-claim. The Codama refactor in `b14b04e` was off-chain SDK only — did not address on-chain.
- **L22 (AssetV1 discriminator on claim/cancel)**: `cancel.rs:70-74`, `claim_arweave.rs:118-122`, `claim_ethereum.rs:89-93` only check `ant_asset.owner == &MPL_CORE_PROGRAM_ID` (account-owner program), not the inner-data `data[0] == 1` AssetV1 discriminator. Practical exploitation is bounded (the inner CPI reverts), but parity with the `deposit.rs` path is the right ask.
- **Audit miscitation**: M1 SECURITY comment is at `migration.rs:102-103`, audit said `101-103`. Cosmetic.

### 13.4 Phase 1 remediation plan (this branch: `security-audit-fixes`)

7 commits, all pre-devnet, ABI breaks are acceptable:

1. **PR-7 (this addendum)** — audit doc cleanup
2. **PR-1** — Vault dust (C1) + ant orphaned-metadata closure (NEW-1)
3. **PR-2** — Epoch lifecycle integrity, option C (Lua-style status flag in place of swap-remove): H2 + H3 + M5 + NEW-2 + NEW-3 + NEW-4
4. **PR-3** — ArNS plugin coherence + reassign safety: M9 + M10 + M11 + N-ARNS-2 + N-ARNS-3 + I14 stale-comment cleanup
5. **PR-4** — `Initialize` binding to upgrade authority across 4 programs: H1 + Theme C
6. **PR-5** — Escrow UpdateAuthority custody (L23) + AssetV1 disc check (L22)
7. **PR-6** — Schema versioning sweep on ant types: N-ANT-1 + I21 (add `version: u8` to `AntRecord`, `AntRecordMetadata`, `AntControllers`)

Phase 2/3 (post external audit): Theme A (`mpl-core-readonly` crate), Theme B (migration import hardening), H4 (VRF for `prescribe_epoch`).

---

*End of audit (addendum 2026-04-30).*
