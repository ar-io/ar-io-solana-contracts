# Feature Matrix: Lua → Solana

This document maps every feature from the current AO Lua contract to its Solana implementation.

**3+1 Program Architecture:**
- **ario-core**: Token, Balances, Vaults, Primary Names
- **ario-gar**: Gateway Registry, Staking, Delegation, Epochs, Observations, Rewards
- **ario-arns**: ArNS Name Registry, Demand Factor, Reserved/Returned Names
- **ario-ant**: Arweave Name Token (Metaplex Core NFT), Records, Controllers

**Status Legend:**
- `[R]` Implemented, tested, and security reviewed
- `[T]` Implemented and tested
- `[P]` Surface committed (signature/skeleton) but pending downstream operational rollout
- `N/A` Not applicable to Solana

---

## Token System (F1-F3)

| ID | Feature | Lua Source | Solana Program | Instruction(s) | Status |
|----|---------|------------|----------------|-----------------|--------|
| F1 | Token Transfer | `token.lua`, `balances.lua` | ario-core | `transfer` | [R] |
| F2 | Balance Query | `balances.lua` | ario-core | SPL Token `getBalance` | [R] |
| F3 | Total Supply Tracking | `balances.lua` | ario-core | `initialize` (config tracks supply components) | [R] |

---

## Vault System (F4-F9)

| ID | Feature | Lua Source | Solana Program | Instruction(s) | Status |
|----|---------|------------|----------------|-----------------|--------|
| F4 | Create Vault | `vaults.lua` | ario-core | `create_vault` | [R] |
| F5 | Vaulted Transfer | `vaults.lua` | ario-core | `vaulted_transfer` | [R] |
| F6 | Revoke Vault | `vaults.lua` | ario-core | `revoke_vault` | [R] |
| F7 | Extend Vault | `vaults.lua` | ario-core | `extend_vault` | [R] |
| F8 | Increase Vault | `vaults.lua` | ario-core | `increase_vault` | [R] |
| F9 | Vault Release / Pruning | `vaults.lua`, `prune.lua` | ario-core | `release_vault` | [R] |

---

## Gateway Registry (F10-F22)

| ID | Feature | Lua Source | Solana Program | Instruction(s) | Status |
|----|---------|------------|----------------|-----------------|--------|
| F10 | Join Network | `gar.lua` | ario-gar | `join_network` | [R] |
| F11 | Leave Network | `gar.lua` | ario-gar | `leave_network` | [R] |
| F12 | Update Gateway Settings | `gar.lua` | ario-gar | `update_gateway_settings`, `update_observer_address` | [R] |
| F13 | Increase Operator Stake | `gar.lua` | ario-gar | `increase_operator_stake` | [R] |
| F14 | Decrease Operator Stake | `gar.lua` | ario-gar | `decrease_operator_stake` | [R] |
| F15 | Delegate Stake | `gar.lua` | ario-gar | `delegate_stake` | [R] |
| F16 | Decrease Delegate Stake | `gar.lua` | ario-gar | `decrease_delegate_stake`, `close_empty_delegation`, `claim_delegate_from_leaving_gateway` | [R] |
| F17 | Redelegate Stake | `gar.lua` | ario-gar | `redelegate_stake` | [R] |
| F18 | Cancel Withdrawal | `gar.lua` | ario-gar | `cancel_withdrawal` | [R] |
| F19 | Instant Withdrawal | `gar.lua` | ario-gar | `instant_withdrawal`, `claim_withdrawal` | [R] |
| F20 | Allow/Disallow Delegates | `gar.lua` | ario-gar | `allow_delegate`, `disallow_delegate`, `set_allowlist_enabled` | [R] |
| F21 | Gateway Pruning | `gar.lua`, `prune.lua` | ario-gar | `prune_gateway` | [R] |
| F22 | Gateway Query | `gar.lua` | ario-gar | (account fetch / indexer) | [R] |

---

## Epoch System (F23-F29)

| ID | Feature | Lua Source | Solana Program | Instruction(s) | Status |
|----|---------|------------|----------------|-----------------|--------|
| F23 | Epoch Creation | `epochs.lua`, `tick.lua` | ario-gar | `create_epoch`, `initialize_epochs`, `set_epochs_enabled` | [R] |
| F24 | Observer Selection | `epochs.lua` | ario-gar | `prescribe_epoch` (weighted roulette) | [R] |
| F25 | Name Prescription | `epochs.lua` | ario-gar | `prescribe_epoch` (name selection from NameRegistry) | [R] |
| F26 | Save Observations | `epochs.lua` | ario-gar | `save_observations` | [R] |
| F27 | Reward Distribution | `epochs.lua`, `tick.lua` | ario-gar | `distribute_epoch`, `compound_delegation_rewards` | [R] |
| F28 | Performance Tracking | `epochs.lua` | ario-gar | `tally_weights` (gateway stats updated in `distribute_epoch`) | [R] |
| F29 | Epoch Query | `epochs.lua` | ario-gar | (account fetch), `close_epoch`, `close_observation` | [R] |

---

## ArNS Registry (F30-F41)

| ID | Feature | Lua Source | Solana Program | Instruction(s) | Status |
|----|---------|------------|----------------|-----------------|--------|
| F30 | Buy Name | `arns.lua` | ario-arns | `buy_name` | [R] |
| F31 | Upgrade Name | `arns.lua` | ario-arns | `upgrade_name` | [R] |
| F32 | Extend Lease | `arns.lua` | ario-arns | `extend_lease` | [R] |
| F33 | Increase Undername Limit | `arns.lua` | ario-arns | `increase_undername_limit` | [R] |
| F34 | Reassign Name | `arns.lua` | ario-arns | `reassign_name` | [R] |
| F35 | Release Name | `arns.lua` | ario-arns | `release_name` | [R] |
| F36 | Returned Name Purchase | `arns.lua` | ario-arns | `buy_returned_name` | [R] |
| F37 | Name Expiration / Pruning | `arns.lua`, `prune.lua` | ario-arns | `prune_expired_names`, `prune_returned_names`, `prune_name_to_returned` | [R] |
| F38 | Reserved Names | `arns.lua` | ario-arns | `reserve_name`, `claim_reserved_name`, `unreserve_name`, `prune_expired_reservation` | [R] |
| F39 | Token Cost Query | `arns.lua` | ario-arns | `get_token_cost` | [R] |
| F40 | Demand Factor Pricing | `demand.lua` | ario-arns | `update_demand_factor` | [R] |
| F41 | Name Query | `arns.lua` | ario-arns | (account fetch / indexer) | [R] |
| F41a | ANT Trait Sync | _Solana-only_ | ario-arns | `sync_attributes` (+ in-handler CPIs from F30-F35, F36) | [R] |
| F41b | Pluggable ANT Program | _Solana-only_ | (asset Attributes plugin) | Per-asset `ANT Program` entry — resolvers and ARIO-CORE BD-097 derive AntRecord PDAs against the named program (canonical fallback when absent). ADR-016 / BD-100. | [R] |

---

## Primary Names (F42-F46)

| ID | Feature | Lua Source | Solana Program | Instruction(s) | Status |
|----|---------|------------|----------------|-----------------|--------|
| F42 | Request Primary Name | `primary_names.lua` | ario-core | `request_primary_name`, `request_and_set_primary_name` | [R] |
| F43 | Approve Primary Name | `primary_names.lua` | ario-core | `approve_primary_name` | [R] |
| F44 | Remove Primary Names | `primary_names.lua` | ario-core | `remove_primary_name`, `remove_primary_name_for_base_name` | [R] |
| F45 | Primary Name Lookup | `primary_names.lua` | ario-core | (account fetch) | [R] |
| F46 | Request Pruning | `primary_names.lua` | ario-core | `close_expired_request` | [R] |

---

## System Operations (F47-F55)

| ID | Feature | Lua Source | Solana Program | Instruction(s) | Status |
|----|---------|------------|----------------|-----------------|--------|
| F47 | Input Validation | `main.lua` | (all) | Anchor constraints + `require!` checks | [R] |
| F48 | State Pruning | `prune.lua` | (all) | `release_vault`, `prune_gateway`, `prune_expired_names`, `prune_returned_names`, `prune_name_to_returned`, `close_expired_request`, `close_empty_delegation`, `close_epoch`, `close_observation`, `prune_expired_reservation` | [R] |
| F49 | Protocol Info | `main.lua` | ario-core | `ArioConfig` account (deserialized by client) | [R] |
| F50 | Paginated Queries | `utils.lua` | (indexer) | Helius / `getProgramAccounts` | N/A |
| F51 | Event Logging | `ario_event.lua` | (all) | Anchor `emit!` events | [R] |
| F52 | Hyperbeam Sync | `hb.lua` | N/A | N/A | N/A |
| F53 | Program Upgrades | N/A | (all) | BPF loader + Squads multisig | [R] |
| F54 | Multi-Source Funding | `main.lua` | ario-arns, ario-gar | Fund-from-stakes via CPI: 10 variants (`buy_name`, `buy_returned_name`, `upgrade_name`, `extend_lease`, `increase_undername_limit` × `_from_delegation` / `_from_operator_stake`). CPI targets: `deduct_delegation_for_payment`, `deduct_operator_stake_for_payment` in ario-gar. Operator stake variants are Solana-only (not in Lua). Multi-gateway cascade (`fundFrom: "any"`) not yet ported — SDK routes to single source per tx. See BD-047. | [T] |
| F55 | Gateway Operator Discount | `arns.lua` | ario-arns | Cross-program read of `Gateway` account (20% discount) | [R] |

---

## State Reading (F56-F61)

| ID | Feature | Lua Source | Solana Implementation | Status |
|----|---------|------------|-----------------------|--------|
| F56 | Balance Queries | `balances.lua` | SPL Token `getBalance` / `getTokenAccountsByOwner` | [R] |
| F57 | Gateway Queries | `gar.lua` | `GatewayRegistry` (zero-copy) + indexer | [R] |
| F58 | Name Queries | `arns.lua` | `NameRegistry` (zero-copy) + indexer | [R] |
| F59 | Epoch Queries | `epochs.lua` | `Epoch` account fetch | [R] |
| F60 | Primary Name Queries | `primary_names.lua` | `PrimaryName` / `PrimaryNameReverse` account fetch | [R] |
| F61 | Permissionless Access | (all) | Public RPC | [R] |

---

## ANT Features (ANT-1 through ANT-6)

Each ANT is a Metaplex Core NFT with an **Attributes plugin** for DAS-queryable marketplace traits (ArNS Name, Type, Undername Limit). Plugin authority is `Owner` so the current NFT holder can sign their own trait updates. The plugin is kept in sync with the source-of-truth `ArnsRecord` PDA by `UpdatePluginV1` CPIs from every ARIO-ARNS handler that mutates trait state (F30, F31, F33, F34, F35, F36); see also `sync_attributes` (F41a) for permissionless reconciliation. The `uri` field points to a static Arweave JSON for image/description. See ADR-012 and BD-096.

| ID | Feature | Current | Solana Program | Instruction(s) | Status |
|----|---------|---------|----------------|-----------------|--------|
| ANT-1 | Set Record | ANT process | ario-ant | `set_record` | [R] |
| ANT-2 | Remove Record | ANT process | ario-ant | `remove_record` | [R] |
| ANT-3 | Get Records | ANT process | ario-ant | `AntRecord` account fetch | [R] |
| ANT-4 | Transfer ANT | ANT process | ario-ant | Metaplex Core native transfer, `reconcile` | [R] |
| ANT-5 | Add Controller | ANT process | ario-ant | `add_controller` | [R] |
| ANT-6 | Remove Controller | ANT process | ario-ant | `remove_controller` | [R] |
| ANT-7 | ACL: List ANTs by address (owned + controlled) | `ar-io-ant-registry` AO process | ario-ant | `AntAcl` account fetch + `register_acl` / `acl_record_owner` / `acl_record_controller` / `acl_remove_owner` / `acl_remove_controller` / `close_acl` (ADR-011) | [R] |
| ANT-8 | ACL: Drift handling for off-SDK transfers | n/a (AO didn't have marketplace transfers) | ario-ant | Render-time cross-check + one-time "Claim ANT" flow via `acl_record_owner`. `on_lifecycle_hook` discriminator reserved but unreachable (MPL Core dispatch is unimplemented upstream — see ADR-011 Phase 2 status update) | [R] |

**Additional ANT instructions:** `initialize`, `transfer_record`, `set_name`, `set_ticker`, `set_description`, `set_keywords`, `set_logo`, `reconcile`

---

## Migration Instructions (all programs)

| Program | Instruction | Purpose |
|---------|-------------|---------|
| ario-core | `import_account` | Import pre-serialized account during migration |
| ario-core | `finalize_migration` | Permanently disable migration imports |
| ario-core | `finalize_supply` | Set supply totals during migration |
| ario-gar | `import_account` | Import pre-serialized account during migration |
| ario-gar | `import_registry_entry` | Import gateway into GatewayRegistry |
| ario-gar | `finalize_migration` | Permanently disable migration imports |
| ario-arns | `import_account` | Import pre-serialized account during migration |
| ario-arns | `import_registry_entry` | Import name into NameRegistry |
| ario-arns | `finalize_migration` | Permanently disable migration imports |
| ario-ant | `initialize_migration` | Initialize migration config for ANT program |
| ario-ant | `import_account` | Import pre-serialized account during migration |
| ario-ant | `finalize_migration` | Permanently disable migration imports |

---

## Admin Instructions

| Program | Instruction | Purpose |
|---------|-------------|---------|
| ario-core | `update_config` | Update vault durations, primary name expiry, authority |
| ario-gar | `initialize` | Initialize GAR settings and registry |
| ario-gar | `initialize_epochs` | Initialize epoch settings |
| ario-gar | `set_epochs_enabled` | Enable/disable epoch processing |

---

## Progress Summary

| Domain | Total | Reviewed | N/A |
|--------|-------|----------|-----|
| Token (F1-F3) | 3 | 3 | 0 |
| Vault (F4-F9) | 6 | 6 | 0 |
| Gateway (F10-F22) | 13 | 13 | 0 |
| Epoch (F23-F29) | 7 | 7 | 0 |
| ArNS (F30-F41) | 12 | 12 | 0 |
| Primary (F42-F46) | 5 | 5 | 0 |
| System (F47-F55) | 9 | 7 | 2 |
| Reading (F56-F61) | 6 | 6 | 0 |
| ANT (ANT-1-8) | 8 | 7 | 0 |
| **TOTAL** | **69** | **66** | **2** |

---

## Test Coverage

| Program | Unit Tests | Integration Tests | Total |
|---------|------------|-------------------|-------|
| ario-core | 56 | 59 | 115 |
| ario-gar | 79 | 77 | 156 |
| ario-arns | 90 | 60 | 150 |
| ario-ant | 85 | 76 | 161 |
| **TOTAL** | **310** | **272** | **582** |

---

## Security Audit v2

Completed. All findings resolved:

| Severity | Count | Status |
|----------|-------|--------|
| Critical | 3 | Fixed |
| High | 6 | Fixed |
| Medium | 10 | Fixed |
| Low | 10 | Fixed |
| **Total** | **29** | **All Fixed** |

---

## Parity Audit

Full Lua-to-Solana behavioral parity audit completed. All findings resolved:

| ID | Category | Description | Status |
|----|----------|-------------|--------|
| BUG-1 | Bug | Primary name uniqueness not enforced (reverse lookup PDA added) | Resolved |
| BUG-2 | Bug | Expired ArNS lease allows primary name approval (active check added) | Resolved |
| BUG-3 | Bug | Base name length 43 not prohibited in primary name validation | Resolved |
| BUG-4 | Bug | Redelegation fee not transferred from stake to protocol | Resolved |
| BUG-5 | Bug | Gateway min_delegation_amount below global floor allowed | Resolved |
| BUG-6 | Bug | Observation gateway_count not validated against epoch | Resolved |
| BUG-7 | Bug | Prune gateway slash transferred wrong direction | Resolved |
| BUG-8 | Bug | Demand factor not applied to primary name request fee | Resolved |
| SHOULD-9 | Should | Observer address uniqueness enforcement (ObserverLookup PDA) | Resolved |
| SHOULD-10 | Should | Gateways joining mid-epoch excluded from weight/rewards | Resolved |
| SHOULD-11 | Should | Reward distribution verifies weights computed for correct epoch | Resolved |
| SHOULD-12 | Should | Leaving gateways excluded from rewards | Resolved |
| SHOULD-13 | Should | Gateway weight computation bound to epoch start timestamp | Resolved |
