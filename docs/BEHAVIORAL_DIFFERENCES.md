# Behavioral Differences: Lua AO vs Solana

**Purpose:** UAT test oracle documenting every intentional behavioral difference between the Lua AO implementation and the Solana implementation. Each difference has a unique ID (BD-NNN) for traceability.

**How to use this document:** For each BD entry, verify the Solana behavior matches the "Solana Behavior" column, not the "Lua Behavior" column. The Lua behavior is documented for comparison only. Differences are intentional per the rationale given.

**Source references:**
- Lua: `/mnt/c/source/ar-io-network-process/src/` (gar.lua, arns.lua, demand.lua, vaults.lua, primary_names.lua, epochs.lua, balances.lua)
- Solana: `/mnt/c/source/solana-ar-io/contracts/programs/` (ario-core, ario-gar, ario-arns, ario-ant)
- Decisions: `/mnt/c/source/solana-ar-io/docs/DECISIONS.md` (ADR-001 through ADR-010)

---

## 1. Platform-Level Differences

These differences arise from the fundamental change in execution environment from AO (Arweave Object) to Solana.

### BD-001: Time Units — Milliseconds to Seconds

| | |
|---|---|
| **Lua Behavior** | All timestamps and durations are in **milliseconds**. Functions like `constants.daysToMs(90)`, `constants.yearsToMs(1)` convert to ms. Epoch duration is `durationMs`. Withdrawal period is `withdrawLengthMs`. |
| **Solana Behavior** | All timestamps and durations are in **seconds**. Solana's `Clock::get().unix_timestamp` returns seconds. Constants: `WITHDRAWAL_LOCK_PERIOD = 90 * 86_400` (seconds), `DEFAULT_EPOCH_DURATION = 86_400` (seconds). |
| **Rationale** | Solana's system clock is natively in seconds. Converting to milliseconds would add confusion and potential for off-by-1000x bugs. All Lua constant values are divided by 1000. |

### BD-002: Address Format — Arweave RSA to Solana Ed25519

| | |
|---|---|
| **Lua Behavior** | Addresses are 43-character base64url Arweave addresses derived from RSA-4096 public keys. Used as map keys for balances, gateways, delegates, etc. |
| **Solana Behavior** | Addresses are 32-byte Ed25519 public keys (`Pubkey`). Stored as `[u8; 32]` in PDA accounts. |
| **Rationale** | Solana uses Ed25519 natively. Users must map their Arweave addresses to Solana pubkeys during the import-then-claim migration (ADR-004, ADR-009). |

### BD-003: Account Model — Global Lua Tables to PDAs

| | |
|---|---|
| **Lua Behavior** | State is stored in global Lua tables: `Balances` (address -> amount), `Vaults` (address -> vaultId -> vault), `GatewayRegistry` (address -> gateway), `NameRegistry` (name -> record), `PrimaryNames` (names, owners, requests). All in a single process memory space. |
| **Solana Behavior** | State is decomposed into individual PDA accounts with deterministic seeds. Each vault, gateway, delegation, name record, etc. is a separate on-chain account. Relationships are encoded via seed derivation (e.g., `["vault", owner, vault_id]`). |
| **Rationale** | Solana requires explicit on-chain accounts. PDAs enable deterministic addressing and ownership verification. See ADR-006, PDA_SCHEMA.md. |

### BD-004: Transaction Atomicity — Message Processing to Solana Transactions

| | |
|---|---|
| **Lua Behavior** | Each AO message is processed atomically within a single process. All state mutations from a single handler are atomic. Multiple operations (e.g., debit balance + credit vault + update supply) happen within one Lua function call. |
| **Solana Behavior** | Each Solana transaction is atomic. However, complex operations may span multiple instructions within a single transaction, all of which succeed or fail together. Operations that touch accounts across programs use CPI. |
| **Rationale** | Program consolidation (3+1 programs) minimizes cross-program calls. Within a single program, atomicity is guaranteed by Solana's runtime. |

### BD-005: Token Model — In-Process Balances to SPL Token

| | |
|---|---|
| **Lua Behavior** | Token balances are stored in a global `Balances` Lua table. Transfer is `balances.reduceBalance(from, qty)` + `balances.increaseBalance(to, qty)`. The process itself manages all accounting. |
| **Solana Behavior** | ARIO is an SPL Token with a mint account. Balances are stored in standard SPL Token Accounts owned by users or PDAs. Transfers use `anchor_spl::token::transfer` CPI to the Token Program. |
| **Rationale** | SPL Token is the standard on Solana. Provides wallet compatibility (Phantom, Solflare), DEX integration (Jupiter), and removes the need for custom balance tracking. |

### BD-006: Token Decimals Preserved

| | |
|---|---|
| **Lua Behavior** | Token uses 6 decimals. 1 ARIO = 1,000,000 mARIO. All amounts are integers in mARIO. |
| **Solana Behavior** | Token uses 6 decimals (`TOKEN_DECIMALS = 6`). 1 ARIO = 1,000,000 mARIO (`ONE_TOKEN = 1_000_000`). All amounts are u64 in mARIO. |
| **Rationale** | Decimal count preserved for parity. No conversion needed during migration. |

---

## 2. Architectural Differences

These differences arise from design decisions about how to represent AO constructs on Solana.

### BD-010: ANT as Metaplex Core NFT

| | |
|---|---|
| **Lua Behavior** | Each ANT is a separate AO process with its own process ID. Ownership = the entity that spawned or was transferred the process. The ANT process stores records, controllers, name, ticker, logo, description, keywords in its process state. Transfer is via AO message passing. |
| **Solana Behavior** | Each ANT is a Metaplex Core NFT (`AssetV1`) with an **Attributes plugin** for DAS-queryable traits (ArNS Name, Type, Undername Limit). Ownership = NFT holder (readable from byte 1-32 of the asset account data). Extended state (records, controllers, metadata) stored in separate PDAs keyed by the asset pubkey: `["ant_config", asset]`, `["ant_controllers", asset]`, `["ant_record", asset, undername_hash]`. Transfer is via Metaplex Core transfer instruction (marketplace compatible). The Core asset `uri` field points to a static Arweave JSON document (`ar://{txId}`) containing name, image, and description — traits are **not** in the JSON (they're on-chain in the Attributes plugin, always current and DAS-indexed). Wallets resolve `ar://{txId}` → `https://arweave.net/{txId}`. |
| **Rationale** | NFTs are the natural fit for unique, transferable name ownership on Solana. Enables marketplace integration (Tensor, Magic Eden) with DAS-queryable traits for filtering/sorting. On-chain Attributes avoid stale off-chain JSON when traits change (e.g., `increaseUndernameLimit`). The `ar://` protocol avoids gateway/domain dependency and aligns with the AR.IO Wayfinder SDK. See ADR-002, ADR-012. |

### BD-011: Lazy Ownership Reconciliation

| | |
|---|---|
| **Lua Behavior** | No reconciliation needed. The ANT process knows its owner at all times. Controller management is explicit via messages to the ANT process. |
| **Solana Behavior** | When an ANT NFT is transferred via Metaplex (e.g., marketplace sale), the ario-ant program is not notified. On the next write operation (set_record, add_controller, etc.), the program reads the current NFT owner from the Metaplex Core asset account and compares it to `config.last_known_owner`. If changed: (1) all controllers are cleared, (2) `last_known_owner` is updated. A permissionless `reconcile` instruction also exists. Record-level `owner` fields are lazily cleared on the next record access when `record.last_reconciled_owner != config.last_known_owner`. |
| **Rationale** | Solana programs cannot intercept Metaplex transfers. Lazy reconciliation is the standard pattern and avoids requiring transfer hooks that would complicate marketplace compatibility. |

### BD-012: Separate observer_address Field on Gateway

| | |
|---|---|
| **Lua Behavior** | `gateway.observerAddress` is a field on the gateway object. Defaults to the gateway operator address. Can be set to any address. Used as the key in `prescribedObservers` lookup (`observer -> gatewayAddress`). |
| **Solana Behavior** | `gateway.observer_address` is a `Pubkey` field on the Gateway PDA. Defaults to operator key. Updated via a dedicated `update_observer_address` instruction (separate from `update_gateway_settings`) because it requires creating/closing ObserverLookup PDAs for uniqueness enforcement. |
| **Rationale** | Observer address updates require PDA lifecycle management (init new lookup, close old lookup), which needs separate Anchor account validation contexts. This cannot be combined with `update_gateway_settings`. |

### BD-013: ObserverLookup PDA for Uniqueness

| | |
|---|---|
| **Lua Behavior** | No uniqueness enforcement on observer addresses. Multiple gateways could theoretically share the same observer address. |
| **Solana Behavior** | An `ObserverLookup` PDA (seeds: `["observer_lookup", observer_pubkey]`) is created when a gateway joins or updates its observer address. The PDA's `init` constraint ensures only one gateway can claim a given observer address. The lookup is closed when the gateway leaves or changes observer. |
| **Rationale** | Prevents two gateways from using the same observer key, which would cause observation submission conflicts. Enforced at the account creation level (Anchor `init` fails if account already exists). |

### BD-014: PrimaryNameReverse PDA for Name Uniqueness

| | |
|---|---|
| **Lua Behavior** | Name uniqueness enforced in-memory: `PrimaryNames.names[name] = owner` is a simple map. Two users cannot hold the same primary name because setting a new one overwrites the map entry. Checked via `primaryNames.getAddressForPrimaryName(name)`. |
| **Solana Behavior** | A `PrimaryNameReverse` PDA (seeds: `["primary_name_reverse", hash(name)]`) tracks name-to-owner reverse mapping. When setting a primary name, the instruction checks: if the reverse PDA already has an owner that is not the current initiator, the transaction fails with `PrimaryNameAlreadySet`. The PDA is closed when the primary name is removed. |
| **Rationale** | On Solana, we cannot iterate all PrimaryName PDAs to check for duplicates. A reverse lookup PDA provides O(1) uniqueness verification without an indexer. |

### BD-015: Program Consolidation (5 Domains to 3+1 Programs)

| | |
|---|---|
| **Lua Behavior** | Conceptually one process with modules: balances, vaults, gar, arns, demand, epochs, primary_names. All share the same address space. |
| **Solana Behavior** | Four programs: **ario-core** (token config, vaults, primary names), **ario-gar** (gateway registry, staking, delegation, epochs, rewards), **ario-arns** (name registry, demand factor, pricing), **ario-ant** (ANT NFT state, records, controllers). CPI flow: ario-gar and ario-arns call ario-core for token operations. ario-arns reads ario-gar Gateway accounts for discount verification via `remaining_accounts`. |
| **Rationale** | Minimizes CPI overhead while keeping concerns separated. Epoch operations (observer selection, reward distribution) co-located with gateway data for atomicity. See ADR-006. |

---

## 3. Epoch & Rewards Differences

### BD-020: Epoch Lifecycle — Single tick() to 6-Step Pipeline

| | |
|---|---|
| **Lua Behavior** | A single `tick()` call (or more precisely, `epochs.createAndPrescribeNewEpoch` + `epochs.distributeEpoch` triggered by any incoming message) handles: (1) create epoch, (2) compute gateway weights, (3) select observers (weighted random), (4) select prescribed names, (5) compute eligible rewards, (6) all in one atomic message. Distribution happens on the next tick after epoch end. |
| **Solana Behavior** | Six separate permissionless instructions form a pipeline: (1) `create_epoch` — initializes epoch account, computes reward rate. (2) `tally_weights` — computes gateway weights in batches (iterates registry via remaining_accounts). (3) `prescribe_epoch` — selects observers and names using hashchain entropy. (4) `save_observations` — observers submit during epoch. (5) `distribute_epoch` — distributes rewards in batches of up to 15 gateways per transaction. (6) `close_epoch` — reclaims rent after retention window. |
| **Rationale** | Solana's compute budget (200K-1.4M CU) and account limits (64 per tx) make single-tx epoch processing impossible for large gateway sets. Batching is required. Each step is permissionless (anyone can crank). |

### BD-021: Batched Reward Distribution (15 Gateways per TX)

| | |
|---|---|
| **Lua Behavior** | `epochs.distributeEpoch` iterates all gateways in one pass, distributing rewards to operators and delegates atomically. For each gateway: compute reward, split operator/delegate shares, transfer to balance or increase stake. |
| **Solana Behavior** | `distribute_epoch` processes gateway accounts passed as `remaining_accounts`, up to `DISTRIBUTION_BATCH_SIZE = 15` per transaction. The epoch tracks `distribution_index` to resume across calls. `rewards_distributed` flag is set when all gateways are processed. A single SPL transfer moves the batch total from protocol token account to stake token account. |
| **Rationale** | Each gateway requires deserialization, computation, and write-back (~80K CU). 15 gateways per tx stays within compute budget. The `distribution_index` cursor enables permissionless resumption. |

### BD-022: Operator Rewards Always Compound to Stake

| | |
|---|---|
| **Lua Behavior** | Operator reward handling depends on `gateway.settings.autoStake`: If true, `gar.increaseOperatorStake(gatewayAddress, actualOperatorReward)` adds to operator stake. If false, `balances.transfer(gatewayAddress, ao.id, actualOperatorReward)` sends to the operator's wallet balance. |
| **Solana Behavior** | Operator rewards are always added directly to `gateway.operator_stake`. There is no auto-stake toggle for reward distribution. The operator can later call `decrease_operator_stake` to withdraw excess stake. |
| **Rationale** | Distributing to individual operator token accounts would require passing each operator's token account as a remaining_account, drastically increasing transaction complexity and cost. Compounding to stake is simpler; operators can decrease_operator_stake to realize gains. The `auto_stake` setting is preserved on the Gateway struct but is not consulted during distribution. |

### BD-023: Reward-Per-Share Accumulator for Delegates

| | |
|---|---|
| **Lua Behavior** | During distribution, each delegate's reward is computed individually: `math.floor((delegate.delegatedStake / gateway.totalDelegatedStake) * eligibleDelegateRewards)`. The reward is directly added to `delegate.delegatedStake` via `gar.increaseExistingDelegateStake`. |
| **Solana Behavior** | Delegate rewards use a Sushi-style reward-per-share accumulator. During distribution: `gateway.cumulative_reward_per_token += (delegate_pool * REWARD_PRECISION) / gateway.total_delegated_stake` where `REWARD_PRECISION = 1e18`. Each Delegation PDA tracks `reward_debt`. When a delegate interacts (stake, unstake, claim rewards), `settle_delegate_rewards` computes: `pending = delegation.amount * (gateway.cumulative_reward_per_token - delegation.reward_debt) / REWARD_PRECISION` and adds it to `delegation.amount`. |
| **Rationale** | Distribution cannot iterate all delegation PDAs on-chain (no way to enumerate PDAs in Solana). The accumulator pattern requires O(1) work per gateway during distribution, deferring per-delegate settlement to each delegate's next interaction. This is the standard pattern for DeFi reward distribution on Solana (used by Marinade, Raydium, etc.). |

### BD-024: Pull-Based Delegate Exit from Leaving Gateway

| | |
|---|---|
| **Lua Behavior** | When a gateway calls `gar.leaveNetwork`, the function iterates all delegates via `for delegateAddress, delegate in pairs(gateway.delegates)` and creates withdrawal vaults for each one atomically. Delegates are "kicked" in the same transaction. |
| **Solana Behavior** | When an operator calls `leave_network`, only the operator's stake is converted to a Withdrawal PDA. Delegates must individually call `claim_delegate_from_leaving_gateway` to create their own Withdrawal PDAs. The instruction verifies `gateway.status == Leaving`, settles pending rewards, creates a Withdrawal for the delegate's full stake, and zeros out the Delegation. |
| **Rationale** | Solana cannot iterate PDAs on-chain. Delegates must pull their own exits. The gateway's `Leaving` status signals to delegates they should claim. SDKs/frontends should surface this prominently. |

### BD-025: Separate tally_weights Step

| | |
|---|---|
| **Lua Behavior** | Gateway weights are computed as part of `epochs.computePrescribedObserversForEpoch` via `gar.getGatewayWeightsAtTimestamp`. Weights are computed for all gateways in one pass during epoch creation and then stored on the gateway objects (`gar.updateGatewayWeights`). |
| **Solana Behavior** | `tally_weights` is a separate permissionless instruction called in batches after `create_epoch` and before `prescribe_epoch`. It reads gateway accounts from `remaining_accounts`, computes each gateway's `GatewayWeights` (stake weight, tenure weight, performance ratios, composite weight), stores composite_weight in the `GatewayRegistry` slot, and accumulates `total_composite_weight` in the epoch. The `weights_tallied` flag must be set before `prescribe_epoch` can proceed. |
| **Rationale** | Weight computation for 2000 gateways cannot fit in one transaction. Batching via `tally_index` cursor allows processing ~20-30 gateways per tx. |

### BD-026: Active Gateway Filtering in Tally

| | |
|---|---|
| **Lua Behavior** | `gar.getActiveGatewayAddressesBeforeTimestamp(epochStartTimestamp)` filters gateways that were active (joined) before the epoch start. Only these are eligible for epoch participation. |
| **Solana Behavior** | The GatewayRegistry contains only active (Joined) gateways — leaving gateways are removed from the registry during `leave_network` (swap-remove pattern). During `tally_weights`, gateways that joined after the epoch started receive `effective_composite = 0` (they are tallied but cannot be selected as observers or receive gateway reward weight). Additionally, during `distribute_epoch`, leaving gateways receive reward = 0. |
| **Rationale** | The registry only contains active gateways, so no explicit timestamp filter is needed. The `start_timestamp > epoch.start_timestamp` check during tally provides equivalent filtering for newly-joined gateways. |

### BD-027: Observer Selection — Weighted Random vs Blockhash Entropy

| | |
|---|---|
| **Lua Behavior** | Observer selection uses crypto.random seeded by the hashchain, with weighted roulette: `random / (2^31 - 1)` produces a float in [0, 1], compared against `cumulativeNormalizedCompositeWeight`. Uses normalized weights (sum to 1.0). Entropy comes from AO's hashchain (a chain of SHA256 hashes). |
| **Solana Behavior** | Observer selection uses hashchain entropy derived from `hash(slot_bytes || epoch_index_bytes || timestamp_bytes)` (24-byte input). Weighted roulette uses u128 arithmetic: `random_value = u128::from_le_bytes(hash[..16]) % total_composite_weight`, compared against cumulative raw composite weights (not normalized). Hash is re-hashed between selections: `hash = sha256(hash)`. To reduce the chance of under-selection when many gateways have equal weight, the loop iterates up to `max_observers * 10` times, skipping already-selected gateways (GAR-019). After selection, each chosen gateway's `observer_address` is resolved from the Gateway PDA via `remaining_accounts`, since the observer wallet can differ from the operator pubkey (see BD-012). This resolution is PDA-validated to prevent cranker manipulation (GAR-003). |
| **Rationale** | Solana does not have AO's hashchain. Slot + epoch index + timestamp provides sufficient entropy. Using raw composite weights instead of normalized weights avoids floating-point and is more natural in Solana's integer arithmetic. The 10x retry multiplier (GAR-019) addresses the birthday-problem-like collision rate when equal-weight gateways dominate the registry. The selection probability distribution is equivalent to Lua's. |

### BD-028: Epoch Index Tracking

| | |
|---|---|
| **Lua Behavior** | Epoch index is computed from timestamp: `math.floor((timestamp - epochZeroStartTimestamp) / durationMs)`. Epochs are stored in `Epochs[epochIndex]` table and removed after distribution. |
| **Solana Behavior** | Epoch index is tracked in `EpochSettings.current_epoch_index` and incremented by `create_epoch`. Each epoch is a separate PDA: `["epoch", epoch_index_bytes]`. Epochs can be closed (rent reclaimed) via `close_epoch` after a retention window of 7 epochs. |
| **Rationale** | PDA-per-epoch enables permissionless creation and avoids unbounded state growth. Epoch accounts are large (zero-copy) and should be cleaned up after distribution. |

---

## 4. ArNS & Pricing Differences

### BD-030: Combined request_and_set_primary_name Instruction

| | |
|---|---|
| **Lua Behavior** | `primaryNames.createPrimaryNameRequest` checks if `record.processId == initiator`. If so, it calls `primaryNames.setPrimaryNameFromRequest` immediately (auto-approve). Otherwise, it stores a request for asynchronous approval. This is a single function with a conditional branch. |
| **Solana Behavior** | Two separate instructions: (1) `request_primary_name` — always creates a PrimaryNameRequest PDA for asynchronous approval. (2) `request_and_set_primary_name` — authorizes the caller as the effective ANT owner for the name's undername (`@` for base names): the explicit per-record `AntRecord.owner` delegate if set, else the ANT-level `AntConfig.last_known_owner` snapshot — both read from canonical `ario_ant` PDAs passed as `remaining_accounts` `[2]` (AntRecord) + `[3]` (AntConfig), not the Metaplex Core asset directly (see BD-097/BD-109). Sets the primary name immediately (no request PDA created). Both charge the same fee (base_fee * demand_factor). |
| **Rationale** | Solana requires different account contexts for the two paths. `request_and_set_primary_name` needs PrimaryName + PrimaryNameReverse init, while `request_primary_name` needs PrimaryNameRequest init. Separating them avoids paying for unused account allocations. Authorization via ANT NFT holder matches Lua's `record.processId == initiator` semantics (see BD-095). |

### BD-031: Primary Name Request Fee — Implemented (2026-05-04)

| | |
|---|---|
| **Lua Behavior** | Request cost computed via `arns.getTokenCost({ intent = "Primary-Name-Request", ... })` which internally computes `baseFeeForNameLength(51) * UNDERNAME_LEASE_FEE_PERCENTAGE * demand_factor`. Fee is paid via the funding plan mechanism (`gar.getFundingPlan`, `gar.applyFundingPlan`) which can draw from balance, stakes, or both. |
| **Solana Behavior** | Fee = `PRIMARY_NAME_REQUEST_BASE_FEE_{LEASE,PERMABUY} * demand_factor / DEMAND_FACTOR_SCALE`, selected from the ArnsRecord `purchase_type` (lease = 0.2 ARIO × DF, permabuy = 1.0 ARIO × DF). Two payment paths: (1) the original direct SPL transfer ix (`request_primary_name`, `request_and_set_primary_name`) — buyer-ATA → treasury — for the simple balance-only case; (2) two new funding-plan variants (`request_primary_name_from_funding_plan`, `request_and_set_primary_name_from_funding_plan`) that CPI into ario-gar's `pay_from_funding_plan`, supporting the full Lua-style multi-source flow (balance + delegations + withdrawals). No single-source primary-name variants — Lua's `primaryNames.createPrimaryNameRequest` has no parallel single-source path; a 1-source funding plan covers that case with marginal CU overhead. |
| **Update (purchase-type fee)** | Originally Solana matched Lua, which uses `UNDERNAME_LEASE_FEE_PERCENTAGE` unconditionally (lease rate for both lease and permabuy primary names). The fee now varies by purchase type to match the v3.0.0 whitepaper (§9.3/§12.3, "equivalent purchase type") — permabuy primary names pay the permabuy undername rate (5×). This **intentionally diverges from Lua**; the whitepaper is canonical. See `docs/WHITEPAPER_COMPARISON.md` #3. |
| **Rationale** | Closed by Phase 3 of `docs/FUND_FROM_PLAN.md` (commit on `feat/fund-from-multisource`). New CPI direction (ario-core → ario-gar) — pattern matches the audited ario-arns CPI surface. CPI depth: user → ario-core → ario-gar → SPL Token = 3 hops, under Solana's 4-hop limit. |

### BD-032: Gateway Operator Discount — Implementation Differences Only

| | |
|---|---|
| **Lua Behavior** | Gateway operator discount (20%) requires: (1) not leaving, (2) `tenureWeight >= 1` (180 days of operation), (3) `gatewayPerformanceRatio >= 0.90` (90% epoch pass rate). Checked via `gar.isEligibleForArNSDiscount()` which reads cached `gateway.weights` updated once per epoch. |
| **Solana Behavior** | Same three conditions: (1) `status == Joined`, (2) `clock.unix_timestamp - start_timestamp >= 15_552_000` (180 days), (3) `(1 + passed_epochs) * 1_000_000 / (1 + total_epochs) >= 900_000` (90%). Computed live at transaction time rather than from cached weights. |
| **Rationale** | Functionally equivalent. The only difference is that Lua reads cached weights (up to 24h stale) while Solana computes tenure and performance live from on-chain state, which is slightly more accurate at threshold boundaries. Solana also adds PDA ownership validation and operator == signer checks required by the account model. |

### BD-033: Reserved Name Auto-Cleanup on Purchase

| | |
|---|---|
| **Lua Behavior** | `arns.addRecord` explicitly checks and removes the reserved name entry: `if arns.getReservedName(name) then NameRegistry.reserved[name] = nil end`. Reserved names have an optional `target` field — only that target address can purchase. |
| **Solana Behavior** | Reserved names are stored in a `ReservedName` PDA. When a name is purchased, the instruction verifies the buyer matches the reserved target (if any). The ReservedName PDA is closed (rent reclaimed) as part of the purchase transaction. |
| **Rationale** | Functionally equivalent. PDA closure is the Solana-idiomatic way to "remove" an entry while reclaiming rent. |

### BD-034: get_token_cost Is Simulation-Only

| | |
|---|---|
| **Lua Behavior** | `arns.getTokenCost(params)` is a pure function that computes the cost of a name operation. It can be called as a dry-run handler (responding to `Get-Token-Cost` messages) or internally during purchase. |
| **Solana Behavior** | Token cost computation is available both on-chain and off-chain. The `get_token_cost` instruction (`ario-arns/src/instructions/cost.rs`) is a view-only instruction designed to be called via `simulateTransaction` — it reads `DemandFactor` state, computes the cost, and writes the result as return data without modifying state. Clients can also compute costs locally using the SDK's pricing functions (which replicate the same math from `ario-arns/src/pricing.rs`). |
| **Rationale** | The on-chain view instruction guarantees exact parity with what `buy_name` will charge, while `simulateTransaction` makes it free to call (no tx fees, no state commitment). This is the standard Solana "view function" pattern. Clients that prefer local computation can read the `DemandFactor` PDA via RPC and apply the same formulas. |

### BD-035: Demand Factor as Separate PDA

| | |
|---|---|
| **Lua Behavior** | Demand factor is a global `DemandFactor` table updated by `demand.updateDemandFactor(timestamp)` on each tick. Includes trailing period purchases, revenues, current period stats, and the `currentDemandFactor` value. |
| **Solana Behavior** | The demand factor is stored in a `DemandFactor` PDA (seeds: `["demand_factor"]`) in the ario-arns program. The current demand factor value is a u64 scaled by `DEMAND_FACTOR_SCALE = 1_000_000` (1.0 = 1_000_000). Other programs (ario-core for primary name fees) read it via `remaining_accounts` by validating the PDA derivation and discriminator. |
| **Rationale** | Cross-program reads via `remaining_accounts` avoid CPI overhead. The PDA provides a stable, deterministic address for the demand factor. |

---

## 5. Staking & Delegation Differences

### BD-040: Withdrawal as Separate PDA with Counter

| | |
|---|---|
| **Lua Behavior** | Withdrawals are stored as vaults within the gateway object: `gateway.vaults[msgId] = { balance, startTimestamp, endTimestamp }`. The vault ID is the AO message ID of the withdrawal request. Delegate vaults are stored in `gateway.delegates[delegateAddress].vaults[msgId]`. |
| **Solana Behavior** | Each withdrawal is a separate `Withdrawal` PDA: `["withdrawal", owner, withdrawal_id_bytes]`. A `WithdrawalCounter` PDA per user tracks `next_id` (monotonically increasing u64). The Withdrawal stores: owner, withdrawal_id, gateway, amount, created_at, available_at, is_delegate (bool), is_exit_vault (bool). Claimed via `claim_withdrawal` which does SPL transfer and closes the account. |
| **Rationale** | PDAs are the Solana-native way to store per-entity state. A counter ensures unique, deterministic PDA derivation. Message IDs do not exist on Solana. |

### BD-041: AllowlistEntry PDA per Entry

| | |
|---|---|
| **Lua Behavior** | Allowlist is a lookup table stored in gateway settings: `gateway.settings.allowedDelegatesLookup[address] = true`. Managed via `gar.allowDelegates(operator, delegates)` and `gar.disallowDelegates(operator, delegates)` which add/remove entries from the table. |
| **Solana Behavior** | Each allowlist entry is a separate `AllowlistEntry` PDA: `["allowlist", gateway_operator, delegate]`. Created by `allow_delegate` (init), removed by `disallow_delegate` (close). An `allowlist_enabled` boolean on the gateway controls whether the allowlist is enforced. When enabled, `delegate_stake` checks for the AllowlistEntry PDA via `remaining_accounts[0]`. Delegates with existing stake > 0 bypass the allowlist check. |
| **Rationale** | Solana cannot store unbounded maps. Individual PDAs per entry enable O(1) lookup and are rent-reclaimable. The `allowlist_enabled` toggle avoids checking PDAs when the feature is off. |

### BD-042: Delegation PDA per Gateway-Delegator Pair

| | |
|---|---|
| **Lua Behavior** | Delegations are nested in the gateway object: `gateway.delegates[delegateAddress] = { delegatedStake, startTimestamp, vaults }`. A single global lookup. |
| **Solana Behavior** | Each delegation is a separate `Delegation` PDA: `["delegation", gateway_operator, delegator]`. Stores: gateway, delegator, amount, start_timestamp, reward_debt, bump. Initialized on first stake via `init_if_needed`. |
| **Rationale** | PDAs are the only way to store per-pair state on Solana. The seed pattern ensures one delegation per gateway-delegator pair, matching the Lua 1:1 relationship. |

### BD-043: RedelegationRecord PDA

| | |
|---|---|
| **Lua Behavior** | Redelegation fee tracking is stored in the gateway: fee resets are tracked by a per-delegator timestamp. The `redelegationFeeResetTimestamp` and `redelegationCount` are tracked on the delegate's record within the gateway. |
| **Solana Behavior** | A `RedelegationRecord` PDA per delegator (seeds: `["redelegation", delegator]`) tracks: `redelegation_count`, `last_redelegation_at`, `fee_reset_at`. Fee rate = `min(10% * count, 60%)` with first being free. Resets if `current_timestamp >= fee_reset_at`. `FEE_RESET_INTERVAL = 7 * 86_400` seconds. |
| **Rationale** | Redelegation tracking is per-delegator (not per-gateway-delegator pair) because the fee penalty applies globally to the delegator's redelegation activity. A separate PDA avoids bloating the Delegation account. |

### BD-044: close_empty_delegation for Rent Reclaim

| | |
|---|---|
| **Lua Behavior** | When a delegate's stake reaches zero, the entry is simply removed from the table: `gateway.delegates[delegateAddress] = nil`. No explicit cleanup needed. |
| **Solana Behavior** | Delegation PDAs with `amount == 0` persist until explicitly closed via `close_empty_delegation`. This is a permissionless instruction — anyone can close empty delegations, returning rent to the delegator. Pending rewards are settled before closing. |
| **Rationale** | Solana accounts persist and consume rent until explicitly closed. This instruction enables cleanup and rent reclamation for zero-balance delegations that remain after full withdrawal. |

### BD-045: Operator Self-Delegation Prevention

| | |
|---|---|
| **Lua Behavior** | No explicit check preventing an operator from delegating to their own gateway. The operator simply adds to their operator stake instead. |
| **Solana Behavior** | `delegate_stake` and `redelegate_stake` explicitly reject `delegator == gateway.operator` with `CannotDelegateToSelf` error. |
| **Rationale** | Prevents confusion between operator stake (which has different withdrawal rules) and delegated stake. Operators should use `increase_operator_stake`. |

### BD-046: Max Delegates per Gateway — 10,000 Cap

| | |
|---|---|
| **Lua Behavior** | No hard cap on the number of delegates per gateway. The `gateway.delegates` table grows dynamically. |
| **Solana Behavior** | `max_delegates_per_gateway = 10_000` (set in `GatewaySettings` during initialization). `delegate_stake` rejects new delegations when the gateway has reached this limit. Existing delegations are unaffected. |
| **Rationale** | Prevents unbounded PDA creation per gateway, which would make enumeration and reward distribution impractical. 10,000 delegates provides ample capacity; the limit is stored in `GatewaySettings` and can be adjusted via program upgrade. |

### BD-047: Fund-from-Operator-Stake for ArNS Purchases

| | |
|---|---|
| **Lua Behavior** | `gar.getFundingPlan` supports `fundFrom = "balance"`, `"stakes"`, or `"any"`, but only draws from delegated stakes — gateway operator stakes (minimum or excess) are not eligible as a funding source for ArNS purchases. |
| **Solana Behavior** | Five additional `_from_operator_stake` instruction variants exist: `buy_name_from_operator_stake`, `buy_returned_name_from_operator_stake`, `upgrade_name_from_operator_stake`, `extend_lease_from_operator_stake`, `increase_undername_limit_from_operator_stake`. These CPI into `deduct_operator_stake_for_payment` in ario-gar, which enforces `remaining >= min_operator_stake` — the operator can only spend **excess** stake above the minimum. No withdrawal vault is created; tokens transfer directly to the protocol treasury. |
| **Rationale** | Convenience feature: operators with excess stake should not have to wait through the withdrawal period to fund ArNS purchases. The minimum-stake floor preserves gateway viability. The operator must sign the transaction, so no unauthorized deduction is possible. |

### BD-048: Max Undernames per Name — 10,000 Cap

| | |
|---|---|
| **Lua Behavior** | No hard cap on the number of undernames per top-level ArNS name. The `record.undernameLimit` can be increased without bound. |
| **Solana Behavior** | `MAX_UNDERNAME_LIMIT = 10_000` (in `ario-core/src/constants.rs`). `increase_undername_limit` rejects increases that would push the total above this cap. |
| **Rationale** | Prevents unbounded undername records per ANT, which would make enumeration and account management impractical. 10,000 undernames per name provides ample capacity for foreseeable use cases. |

---

## 6. Vault Differences

### BD-050: Individual release_vault vs Batch Prune

| | |
|---|---|
| **Lua Behavior** | Expired vaults are pruned in batch via `vaults.pruneVaults(currentTimestamp)` which iterates all vaults and releases any that have passed their `endTimestamp`. This can be triggered lazily by any message. |
| **Solana Behavior** | Each vault must be individually released via `release_vault`. The caller (vault owner or anyone — it's permissionless after expiry) passes the specific Vault PDA and its associated token account. Tokens are transferred back to the owner's token account, and both the vault PDA and vault token account are closed. |
| **Rationale** | Solana cannot iterate PDAs on-chain. Each vault release requires specific account references. SDKs/frontends should track vault expirations and prompt users. |

### BD-051: Extend/Increase Vault — Inclusive Boundary Check

| | |
|---|---|
| **Lua Behavior** | `vaults.extendVault`: `assert(currentTimestamp <= vault.endTimestamp, "Vault has ended.")` — inclusive, can extend at exactly the end timestamp. `vaults.increaseVault`: `assert(currentTimestamp <= vault.endTimestamp, "Vault has ended.")` — same inclusive check. |
| **Solana Behavior** | `extend_vault`: `require!(clock.unix_timestamp <= vault.end_timestamp, ArioError::VaultExpired)` — inclusive, matches Lua. `increase_vault`: `require!(clock.unix_timestamp <= vault.end_timestamp, ArioError::VaultExpired)` — inclusive, matches Lua. |
| **Rationale** | Intentionally preserved. The `<=` check allows operations at the exact expiry moment, matching Lua behavior exactly. |

### BD-052: Revoke Vault — Matches Lua Exactly

| | |
|---|---|
| **Lua Behavior** | `vaults.revokeVault`: `assert(currentTimestamp < vault.endTimestamp, "Vault has ended.")` — strict less-than. Cannot revoke at or after expiry. |
| **Solana Behavior** | `revoke_vault`: `require!(!vault.is_unlocked(clock.unix_timestamp), ArioError::VaultExpired)` where `is_unlocked` returns `current_timestamp >= end_timestamp`. So `!is_unlocked` = `current_timestamp < end_timestamp` — strict less-than, matching Lua exactly. |
| **Rationale** | No difference. Both use strict `<` for revocation and `>=` for release. |

### BD-053: Vault Token Account per Vault

| | |
|---|---|
| **Lua Behavior** | Vault balance is a number in the vault object. No separate token account — just `vault.balance` integer in the global Lua table. |
| **Solana Behavior** | Each vault has its own SPL Token Account (PDA-owned by the vault PDA). Tokens are physically transferred to this account on creation and transferred back on release/revoke. The token account is closed when the vault is released or revoked, returning rent to the appropriate party. |
| **Rationale** | SPL Token requires actual token accounts. Having each vault own its tokens via a dedicated account provides clear accounting and enables the vault PDA to sign transfers. |

### BD-054: Minimum Vault Size

| | |
|---|---|
| **Lua Behavior** | No explicit minimum vault size in the Lua vaults module (only checks `qty > 0`). However, `constants.MIN_VAULT_SIZE` may be referenced elsewhere. |
| **Solana Behavior** | `MIN_VAULT_SIZE = 100_000_000` (100 ARIO). Both `create_vault` and `vaulted_transfer` enforce `amount >= ArioConfig::MIN_VAULT_SIZE`. |
| **Rationale** | Prevents dust vaults that cost more in rent than they hold. 100 ARIO minimum ensures economic viability of vault accounts. |

---

## 7. Migration-Specific Differences

These features exist only in the Solana implementation and have no Lua equivalent.

### BD-060: import_account Instruction

| | |
|---|---|
| **Lua Behavior** | N/A — no migration concept exists in the Lua implementation. |
| **Solana Behavior** | Each program has an `import_account` instruction gated by `migration_active == true` and signed by `migration_authority`. Accepts arbitrary seeds and serialized data, creates a PDA with those seeds and writes the data. Used during the import phase to bootstrap all state from the AO snapshot. |
| **Rationale** | Required for the import-then-claim migration strategy (ADR-009, ADR-010). Enables the admin to pre-populate all program state from the AO snapshot before going live. |

### BD-061: import_registry_entry for Zero-Copy Registries

| | |
|---|---|
| **Lua Behavior** | N/A. |
| **Solana Behavior** | The GatewayRegistry and NameRegistry are zero-copy accounts too large to create via `init`. They are pre-allocated by the migration orchestrator and populated entry-by-entry. |
| **Rationale** | Solana's `MAX_PERMITTED_DATA_INCREASE` (10KB) prevents creating 64KB+ accounts in a single instruction. Pre-allocation + incremental population is required. |

### BD-062: finalize_migration — Permanent Disable

| | |
|---|---|
| **Lua Behavior** | N/A. |
| **Solana Behavior** | `finalize_migration` sets `migration_active = false` permanently. Signed by the main authority (multi-sig), not the migration authority. Once called, no further imports are possible — the flag cannot be re-enabled. |
| **Rationale** | One-way kill switch ensures that the migration hot key becomes permanently inert after import completes. Signed by the main authority to prevent the migration authority from prematurely disabling itself. See ADR-010. |

### BD-063: migration_active Flag

| | |
|---|---|
| **Lua Behavior** | N/A. |
| **Solana Behavior** | Boolean flag on each program's config account (`ArioConfig.migration_active`, `GarSettings.migration_active`, `AntMigrationConfig.migration_active`). When true, `import_account` instructions are accepted. When false, they are rejected. |
| **Rationale** | Guards import instructions so they can only run during the migration window. Prevents post-migration state injection. |

### BD-064: finalize_supply — Supply Reconciliation

| | |
|---|---|
| **Lua Behavior** | N/A. |
| **Solana Behavior** | `finalize_supply` sets `total_supply`, `protocol_balance`, `circulating_supply`, and `locked_supply` on the ArioConfig. Called after all account imports to reconcile supply totals with the actual imported state. |
| **Rationale** | Supply counters must match the sum of all imported vaults, staked tokens, and balances. A dedicated instruction ensures the admin can set correct values after all imports. |

---

## 8. Omitted / Not Applicable

These Lua features are intentionally not ported to Solana, or are handled differently at the infrastructure level.

### BD-070: Pagination — Off-Chain via Indexer

| | |
|---|---|
| **Lua Behavior** | `getPaginatedRecords`, `getPaginatedVaults`, `getPaginatedBalances`, `getPaginatedPrimaryNames`, etc. support cursor-based pagination with sort and filter. Implemented via `utils.paginateTableWithCursor`. |
| **Solana Behavior** | Not implemented on-chain. All enumeration and pagination is handled by an off-chain indexer (Helius DAS API or similar). The GatewayRegistry (2000 slots) and NameRegistry (10000 slots) zero-copy accounts enable permissionless on-chain enumeration for epoch operations, but client-facing pagination uses the indexer. |
| **Rationale** | `getProgramAccounts` is rate-limited or disabled on public RPCs. An indexer is mandatory for production Solana deployments. On-chain pagination would waste compute units. See ADR-007. |

### BD-071: Hyperbeam Sync — N/A

| | |
|---|---|
| **Lua Behavior** | `HyperbeamSync` tables track which state has changed for synchronization with the Hyperbeam infrastructure. Referenced throughout primary_names.lua, arns.lua, etc. |
| **Solana Behavior** | Not applicable. Solana has no Hyperbeam equivalent. State changes are visible via transaction logs, Anchor events (`emit!`), and indexer subscriptions. |
| **Rationale** | Hyperbeam is an AO-specific infrastructure component. Solana uses its own event and indexing infrastructure. |

### BD-072: AO Eval Handler — N/A

| | |
|---|---|
| **Lua Behavior** | Supports `Eval` messages that execute arbitrary Lua code in the process context. Used for debugging and ad-hoc operations. |
| **Solana Behavior** | Not applicable. Solana programs are compiled and deployed; runtime code execution is not possible. Admin operations are handled via specific instructions (update_config, etc.) gated by authority signatures. |
| **Rationale** | Solana is a compiled, non-eval environment. The `Eval` capability is inherently an AO feature with no Solana equivalent. |

### BD-073: @ Record Removal Blocked in ANT

| | |
|---|---|
| **Lua Behavior** | Lua ANT processes do not have an explicit removal prevention for the `@` record — it's implicitly always present as part of the ANT initialization. Some implementations may allow removal. |
| **Solana Behavior** | `remove_record` explicitly checks `record.undername != "@"` and returns `CannotRemoveRootRecord` error. The `@` record is created during `initialize` and can never be deleted — only updated via `set_record`. |
| **Rationale** | The `@` record is the root record that every ANT must have. Preventing its removal ensures ANT resolution always works. This is a safety improvement. |

### BD-074: Maximum 10 Controllers per ANT

| | |
|---|---|
| **Lua Behavior** | No explicit limit on the number of controllers per ANT process. Controlled by the `MaxControllers` constant if set, but often unlimited in practice. |
| **Solana Behavior** | `MAX_CONTROLLERS = 10`. The `add_controller` instruction enforces `controllers.controllers.len() < MAX_CONTROLLERS`. The AntControllers PDA is sized for exactly 10 controllers. |
| **Rationale** | Account size must be fixed at creation. 10 controllers is a generous limit that keeps the PDA size reasonable. The constant matches `MAX_CONTROLLERS_PER_ANT` in ario-core's constants. |

### BD-075: Observer Selection — No Weighted Random (Uses Blockhash)

| | |
|---|---|
| **Lua Behavior** | Uses AO's `crypto.random` seeded by hashchain for weighted random selection. The random function produces values in `[0, 2^31-1]`, normalized to `[0, 1]` for the cumulative weight comparison. |
| **Solana Behavior** | Uses `hash(slot || epoch_index)` as initial entropy, then iteratively `hash(hash)` for subsequent selections. Random value is `u128::from_le_bytes(hash[..16]) % total_composite_weight` — using 128-bit modular arithmetic instead of floating-point normalization. |
| **Rationale** | Equivalent probability distribution without floating-point. Solana's slot hash provides sufficient entropy for deterministic but unpredictable observer selection. The modulo bias is negligible for practical weight values. |

### BD-076: getFundingPlan / applyFundingPlan — Closed (Lua parity at planner level; one open contract gap)

| | |
|---|---|
| **Lua Behavior** | `gar.getFundingPlan(from, amount, fundFrom)` computes how to fund a payment from balance, vaults (oldest endTimestamp first), excess delegated stake across multiple gateways (per perf+stake order), and minimum delegated stake (auto-vaulting sub-min residue per gateway). `gar.applyFundingPlan(plan, msgId, timestamp)` executes the plan by drawing from each chosen source and creating exit vaults for sub-min delegation residues. `fundFrom` is "balance" \| "stakes" \| "any". |
| **Solana Behavior** | Multi-gateway delegation composition shipped: `pay_from_funding_plan(sources: Vec<FundingSourceSpec>, expected_total: u64, residue_vault_count: u8)` accepts up to `MAX_FUNDING_SOURCES = 5` sources spanning up to `MAX_DELEGATION_SOURCES = 3` distinct gateways. Per-source remaining_accounts layout: Balance 0 slots, Delegation 2 slots `[gateway_pda, delegation_pda]`, OperatorStake 1 slot `[gateway_pda]`, Withdrawal 1 slot `[withdrawal_pda]`, followed by N residue_vault PDAs (one per Delegation going sub-min). Aggregates per-source bookkeeping into ≤ 2 SPL transfers (one from stake pool, one from payer ATA) and auto-creates Withdrawal residue vaults sequenced from `WithdrawalCounter.next_id` when Delegation sources drain to sub-`min_delegation_amount` (matches Lua per-gateway). Operator-stake residue is hard-rejected (Solana extension; Lua doesn't fund-from operator stake). Wrapper variants `_from_withdrawal` and `_from_funding_plan` ship for all 5 ArNS fee-paying ix; primary-name gets `_from_funding_plan` × 2. SDK planner (`sdk/src/solana/funding-plan.ts`) implements Lua-faithful sort order and tracks per-source bound gateways via `FundingPlan.gatewayPerSource` + `residueDelegationIndexes`. |
| **Lua-parity refinements (2026-05-04)** | Three planner-level divergences closed: (1) `'stakes'` mode now draws **withdrawal vaults too** before delegations (Lua's `planVaultsDrawdown` runs unconditionally after the balance gate; pre-fix the SDK skipped vaults under `'stakes'`). (2) Stage-4 floor pass **re-sorts by `gatewayPerformanceRatio` asc** + tie-breaks (matches `planMinimumStakesDrawdown` re-sort at `gar.lua:1587`); previously reused Stage-3's excess-desc order so floors were drained from gateways with the most excess instead of the worst performance. (3) `'plan'` mode now **requires explicit `params.sources`** — pre-fix it silently fell through to `'any'` discovery, masking what the doc described as a distinct caller-supplied mode. See `funding-plan.ts` + `funding-plan.test.ts` for the new behavioral assertions. |
| **Caps** | `MAX_FUNDING_SOURCES = 5`, `MAX_DELEGATION_SOURCES = 3`. SDK rejects plans exceeding either cap; on-chain handler defends with `TooManyFundingSources` / `TooManyDelegationSources` / `DuplicateGatewayInSources` / `MismatchedResidueVaultCount` / `MissingResidueVault`. |
| **Solana extensions vs Lua** | `'withdrawal'` mode (vault-only path, no Lua equivalent), `OperatorStake` source via opt-in `fundAsOperator` (Lua never funds from operator stake), and operator-side `Withdrawal` PDAs (`is_delegate: false`) included in the `'any'`/`'stakes'` pool (Lua's `planVaultsDrawdown` only iterates delegate vaults). The third item is what surfaces the open contract-level gap below. |
| **Closed gap** | The leave-vault split landed (BD-102, 2026-05-04): `leave_network` and `prune_gateway` now produce a protected exit vault (min portion, `is_protected: true`) + an optional excess vault (above-min portion, `is_protected: false`). The protected vault is rejected by `instant_withdrawal` and `deduct_withdrawal_for_payment` so the 90-day lock on the operator min stake is enforced. SDK funding-plan filters `is_protected: true` from the discovered source pool. |

### BD-077: Registry Caps — 3,000 Gateways, 200,000 Names

| | |
|---|---|
| **Lua Behavior** | No hard cap on the number of gateways or names. The Lua tables grow dynamically. |
| **Solana Behavior** | `GatewayRegistry` has `MAX_GATEWAYS = 3,000` slots (120KB account). `NameRegistry` has `MAX_NAMES = 200,000` slots (8MB account). `join_network` fails with `RegistryFull` if the gateway cap is reached. Name purchase fails if the name registry is full. Both are well under Solana's 10MB account limit. |
| **Rationale** | Zero-copy accounts must be pre-allocated with a fixed size. 3,000 gateways is the maximum that keeps the Epoch account under Solana's 10KB `MAX_PERMITTED_DATA_INCREASE` limit (Epoch embeds `failure_counts: [u16; 3000]`). 200,000 names provides multi-year runway under aggressive growth assumptions; the next ceiling is ~225K (10MB account limit), at which point sharded registries become required. Both can be increased further via program upgrade + incremental `realloc`. |

### BD-078: Slashing Implemented via prune_gateway

| | |
|---|---|
| **Lua Behavior** | The `failedGatewaySlashRate` constant exists (`failedGatewaySlashRate = 1.0`, 100%). When a gateway accumulates `maxConsecutiveFailures` (30) consecutive failures, it is pruned and its minimum operator stake is slashed. |
| **Solana Behavior** | `prune_gateway` enforces `failed_consecutive >= max_consecutive_failures` (30). It slashes `min(min_operator_stake, gateway.operator_stake)` — 100% of the minimum stake (20,000 ARIO). Slashed tokens are transferred via SPL CPI to the protocol token account. Any remaining stake above the minimum is placed in a Withdrawal PDA for the operator. |
| **Rationale** | Direct port of Lua behavior. Slashing is enforced at prune time (permissionless), not during `distribute_epoch`. |

### BD-079: No AO Message ID References

| | |
|---|---|
| **Lua Behavior** | AO message IDs (`msgId`) are used as vault IDs, report transaction IDs, and general identifiers. They are 43-character Arweave addresses. |
| **Solana Behavior** | Monotonically increasing u64 counters replace message IDs for vaults and withdrawals (e.g., `vault_counter.next_id`, `withdrawal_counter.next_id`). Report transaction IDs are stored as `[u8; 32]` (Solana transaction hashes or Arweave TX IDs). |
| **Rationale** | Solana does not have AO-style message IDs. Sequential counters are deterministic and PDA-derivable. |

### BD-080: Expedited Withdrawal — All Withdrawals Eligible (Matches Lua)

| | |
|---|---|
| **Lua Behavior** | The `processInstantWithdrawal` function applies to any pending vault with a penalty based on time elapsed. There is no distinction between exit vaults (from leave_network) and regular withdrawal vaults for expedited withdrawal eligibility. |
| **Solana Behavior** | `instant_withdrawal` applies to all withdrawals, including exit vaults from `leave_network` and `prune_gateway`. The same time-decaying penalty applies (50% at creation, linearly decaying to 10% near expiry). The `is_exit_vault` flag is preserved on the Withdrawal struct for tracking purposes but does not restrict expedited withdrawal. |
| **Rationale** | Matches Lua behavior. Operators and delegates can pay a penalty to expedite any withdrawal, providing liquidity when needed. |

### BD-081: "Fund From Stakes" Payment Option — Implemented (multi-source via Phase 1.5/2/3)

| | |
|---|---|
| **Lua Behavior** | Name purchases, primary name requests, and other fee-paying operations support `fundFrom = "any" \| "balance" \| "stakes"`. When "stakes" or "any", the system uses the funding-plan mechanism to compose payment across balance + vaults + delegations, creating exit vaults for sub-min delegation residues. |
| **Solana Behavior** | Implemented across four payment paths per fee-paying ix: (1) **balance** — direct buyer-ATA → treasury SPL transfer (the original ix), (2) **stakes** via `_from_delegation` / `_from_operator_stake` (CPI into ario-gar deduct ix; tokens go directly from stake pool to treasury, no vault/lock/penalty), (3) **withdrawal** via `_from_withdrawal` (CPI into `deduct_withdrawal_for_payment`; partial-drain supported, vault stays open at zero until cleanup ix runs), (4) **funding plan** via `_from_funding_plan` (CPI into `pay_from_funding_plan`; multi-source aggregation; auto-vault residue on sub-min Delegation drain — matches Lua). For `buy_returned_name` across all four paths, the protocol share goes through the funding source and the initiator share stays a direct buyer-ATA → initiator-ATA SPL transfer. Primary-name requests gain (1) and (4) via Phase 3. SDK exposes `--fund-from {balance,stakes,withdrawal,plan,any}`; the `'any'` picker (full Lua-faithful multi-source planner) lands in Phase 5. See BD-076 for the remaining multi-gateway-delegation gap. |

### BD-082: Epoch Observation Window

| | |
|---|---|
| **Lua Behavior** | Observations can be submitted after `epochStartTimestamp` and before `epochEndTimestamp`. The full epoch duration is the observation window. |
| **Solana Behavior** | Same: observations can be submitted between `epoch.start_timestamp` and `epoch.end_timestamp`. The `OBSERVATION_WINDOW_SECONDS = 3_600` constant is defined but the current implementation uses the full epoch window for submission, matching Lua. |
| **Rationale** | Matches Lua behavior. The observation window constant is available for future tightening if desired. |

### BD-083: Observation Failure Tracking — Bitmap vs Per-Gateway Map

| | |
|---|---|
| **Lua Behavior** | `epoch.observations.failureSummaries[failedGatewayAddress]` is a table of observer addresses who marked that gateway as failed. Each observer's report adds their address to the list. |
| **Solana Behavior** | Observers submit a `gateway_results` bitmap (256 bytes = 2048 bits, one bit per gateway). Bit 1 = passed, bit 0 = failed. The epoch maintains `failure_counts[gateway_index]` (u16 array) which is incremented for each observer that marks a gateway as failed. During distribution, a gateway is considered failed if `failure_counts[i] > observations_submitted / 2`. |
| **Rationale** | Bitmap representation is dramatically more compact (256 bytes per observation vs. N gateway addresses per observation). Gateway ordering is determined by the GatewayRegistry index at epoch creation time. |

### BD-084: Reward Rate Computation

| | |
|---|---|
| **Lua Behavior** | `epochs.getRewardRateForEpoch(epochIndex)` computes a linearly decaying reward rate from `maximumRewardRate` (0.1%) to `minimumRewardRate` (0.05%) between epochs `rewardDecayStartEpoch` (365) and `rewardDecayLastEpoch` (547). |
| **Solana Behavior** | `compute_reward_rate(epoch_index, max_rate, min_rate, decay_start, decay_last)` implements the same linear interpolation. Rates are scaled by `RATE_SCALE = 1_000_000`. `MAX_REWARD_RATE = 1_000` (0.1%), `MIN_REWARD_RATE = 500` (0.05%), `REWARD_DECAY_START_EPOCH = 365`, `REWARD_DECAY_LAST_EPOCH = 547`. |
| **Rationale** | Direct port of the Lua logic with integer scaling. Values match exactly. |

### BD-085: Self-Transfer Prevention

| | |
|---|---|
| **Lua Behavior** | `balances.transfer`: `assert(from ~= recipient, "Cannot transfer to self")` — checks address equality. |
| **Solana Behavior** | The custom `ario-core::transfer` ix is **deprecated** (see `docs/REMOVE_CUSTOM_TRANSFER_PLAN.md`); the SDK now builds standard SPL `transferChecked`, which has **no self-transfer guard** — a no-op same-ATA transfer succeeds. The deprecated ix retained `require!(from_token_account.key() != to_token_account.key(), ArioError::SelfTransfer)` for any straggler caller — checks token-account equality, not wallet equality. |
| **Rationale** | SPL Token operates on token accounts, not wallets — a user may have multiple ATAs for the same mint. The original guard was defensive against AO-style address aliasing that doesn't exist on Solana. Standard SPL transfers are well-understood and don't need our wrapper; relying on them removes the ATA pre-existence bug class entirely (wallets auto-create ATAs for standard SPL transfers). |

### BD-086: Epoch Rewards — Leaving Gateways Get Zero

| | |
|---|---|
| **Lua Behavior** | `epochs.distributeEpoch`: `if gateway and totalEligibleRewardsForGateway and gateway.status ~= "leaving" then` — leaving gateways are explicitly skipped during reward distribution. |
| **Solana Behavior** | `distribute_epoch`: `let is_leaving = gateway.status == GatewayStatus::Leaving; let reward = if is_leaving { 0 } ...` — leaving gateways receive reward = 0. Stats are still updated (total_epochs incremented, pass/fail tracked). |
| **Rationale** | Direct port. Leaving gateways are excluded from rewards but their epoch participation is still tracked for historical accuracy. Note: leaving gateways are removed from the registry during `leave_network`, so in practice they should not appear in distribution batches unless they left during the current epoch. |

### BD-087: Delegate Eligibility for Distribution

| | |
|---|---|
| **Lua Behavior** | `gar.isDelegateEligibleForDistributions(gateway, delegateAddress)` checks that the delegate still exists on the gateway and has `delegatedStake > 0`. If not eligible, the delegate's share goes to `totalRewardsForMissingDelegates` and is effectively forfeited (not given to operator). |
| **Solana Behavior** | Delegate rewards are handled via the reward-per-share accumulator (BD-023). During distribution, the total delegate pool is added to the accumulator. Only delegates who still have `delegation.amount > 0` when they call `compound_delegation_rewards` (or any interaction that triggers `settle_delegate_rewards`) will receive their share. If a delegate has withdrawn, their unclaimed accumulated rewards are effectively distributed to remaining delegates (the denominator is current `total_delegated_stake`). |
| **Rationale** | The accumulator pattern inherently handles the "missing delegate" case — unclaimed shares are proportionally absorbed by remaining delegates rather than being forfeited. This is slightly different from Lua where missing delegate rewards are not redistributed. |

### BD-088: Gateway Stats Field Naming

| | |
|---|---|
| **Lua Behavior** | Gateway stats fields: `prescribedEpochCount`, `observedEpochCount`, `totalEpochCount`, `passedEpochCount`, `failedEpochCount`, `failedConsecutiveEpochs`, `passedConsecutiveEpochs`. |
| **Solana Behavior** | Gateway stats fields: `prescribed_epochs`, `observed_epochs`, `total_epochs`, `passed_epochs`, `failed_epochs`, `failed_consecutive`, `passed_consecutive`. Snake_case per Rust convention. Values and semantics are identical. |
| **Rationale** | Rust naming convention (snake_case). No semantic difference. |

### BD-089: Epoch Prescribed Names — Hash-Based Selection

| | |
|---|---|
| **Lua Behavior** | Names are sorted by their base64url hash, then selected using hashchain-derived random indices with linear probing. Names are stored and referenced by their string value. |
| **Solana Behavior** | Names in the NameRegistry are stored as 32-byte SHA256 hashes. Selection uses the same hashchain-derived random indices with linear probing. Prescribed names in the epoch account are stored as `[[u8; 32]; 2]` (hash values, not strings). |
| **Rationale** | PDA seeds use hashed names (BD-003). Storing hashes instead of variable-length strings enables fixed-size arrays in the zero-copy epoch account. The selection algorithm is equivalent. |

### BD-090: Compute Budget Differences

| | |
|---|---|
| **Lua Behavior** | No compute budget concept. AO processes have no per-message compute limits. |
| **Solana Behavior** | Transactions have compute unit (CU) limits: default 200K, max 1.4M. Integration tests use: 400K CU for ario-core/ario-arns, 1M CU for ario-gar. Complex operations (epoch distribution, weight tallying) are batched to stay within limits. |
| **Rationale** | Solana's runtime enforces compute budgets. Batching and multi-tx pipelines are the architectural response. |

### BD-091: Event Emission

| | |
|---|---|
| **Lua Behavior** | State changes result in AO "notices" (messages sent to relevant parties) and return values from handler functions. No formal event system. |
| **Solana Behavior** | Anchor `emit!()` macro emits structured events (e.g., `GatewayJoinedEvent`, `VaultCreatedEvent`, `EpochCreatedEvent`, `WithdrawalClaimedEvent`). These are logged in transaction metadata and queryable by indexers. |
| **Rationale** | Anchor events are the standard pattern for Solana program observability. They replace AO's notice system and enable indexer-driven UIs. |

### BD-092: Primary Name — ANT NFT Holder Authorization

| | |
|---|---|
| **Lua Behavior** | `primaryNames.approvePrimaryNameRequest`: only `record.processId == from` can approve — the ArNS record's process ID (ANT) must match the approver. |
| **Solana Behavior** | `approve_primary_name`, `request_and_set_primary_name`, and `remove_primary_name_for_base_name` authorize the signer as the effective ANT owner resolved from **canonical `ario_ant` state**, not the Metaplex Core asset directly (ario-core is MPL-agnostic per ADR-016). `read_ant_record_owner` / `read_ant_config_last_known_owner` are hard-pinned to `ario_ant::ID` and return: the explicit per-record `AntRecord.owner` delegate **only when the record is reconciled** (`last_reconciled_owner == AntConfig.last_known_owner`), otherwise the ANT-level `AntConfig.last_known_owner`. The `ArnsRecord.owner` field is not checked. See BD-097 (layout + freshness gate) and BD-109 (the canonical-only / BYO-ANT limitation). |
| **Rationale** | Matches Lua semantics where `record.processId` (the ANT) is the sole authority. On Solana, ario-core is MPL-agnostic (ADR-016) and can't read the live NFT holder, so it resolves the **effective owner** from `ario_ant`'s canonical snapshot — the explicit `AntRecord.owner` delegate when the record is reconciled, else `AntConfig.last_known_owner` (updated by `ario_ant::transfer`). Authority follows that snapshot, which tracks the NFT holder once a wrapped transfer (or permissionless `reconcile`) has run — so name authority still transfers with the ANT, just mediated by the canonical snapshot rather than a direct token-owner read. See BD-097 (mechanism), BD-109 (BYO-ANT limit), and BD-095 (broader ArNS auth model). |

### BD-093: ArNS Name Validation — Lowercase Enforcement

| | |
|---|---|
| **Lua Behavior** | Names are validated against regex patterns and then lowercased: `string.lower(name)`. Validation may accept uppercase but storage is always lowercase. |
| **Solana Behavior** | `is_valid_arns_name` explicitly rejects uppercase characters: `if b.is_ascii_uppercase() { return false; }`. Names must be submitted already lowercased. Hash derivation also lowercases: `hash_name` calls `name.to_lowercase()` before hashing. |
| **Rationale** | Enforcing lowercase at validation time prevents ambiguity. Since PDA seeds use the hash of the lowercased name, accepting uppercase would create a mismatch between what the user typed and what PDA was derived. |

### BD-094: Demand Factor Update Mechanism

| | |
|---|---|
| **Lua Behavior** | `demand.updateDemandFactor(currentTimestamp)` is called on every message tick. If a new period has started, it adjusts the demand factor up or down based on trailing purchase/revenue comparisons. Multiple periods can be caught up in one call. Fees are permanently adjusted downward after `maxPeriodsAtMinDemandFactor` consecutive minimum periods. |
| **Solana Behavior** | The DemandFactor PDA stores the current demand factor value. Updates are triggered by a dedicated instruction (or as a side effect of name purchases). The adjustment logic (up/down based on trailing averages, fee floor reset) follows the same algorithm with integer arithmetic. |
| **Rationale** | Direct port with integer-scaled arithmetic. The on-chain update cadence may differ from Lua's per-message updates — Solana updates happen when explicitly triggered. |

### BD-095: ArNS Name Management Authorization — Two Tiers

| | |
|---|---|
| **Lua Behavior** | ArNS records have no `owner` field. Of the five management operations, **only two require ANT-process authorization**: `reassignName` (`assertValidReassignName` checks `record.processId == from`) and `releaseRecord` (the `Release-Name` handler checks `record.processId == msg.From`). The other three — `extendLease`, `upgradeRecord`, `increaseUndernameLimit` — have **no caller-side authorization**, only a balance check via `gar.getFundingPlan(from, ...)`. Anyone with sufficient ARIO can extend/upgrade/increase a name they don't control. |
| **Solana Behavior** | Mirrors Lua's two-tier model. `reassign_name` and `release_name` require the caller to be the current Metaplex Core NFT holder (verified via `read_mpl_core_owner`); these emit `ArnsError::NotAntHolder` on mismatch. `extend_lease`, `upgrade_name`, and `increase_undername_limit` (plus their `_from_delegation` and `_from_operator_stake` variants) are **permissionless** — any ARIO holder pays from their own wallet or stake pool. The `ArnsRecord.owner` field exists but is informational only and is never used for authorization. **(2026-05-27: the ADR-016 MPL-agnostic reshape temporarily regressed both handlers to `caller == record.owner`; restored to current-holder auth — see BD-106.)** |
| **Trait sync interaction** | The Metaplex Attributes plugin authority is `Owner`, so trait-mutating CPIs (`UpdatePluginV1`) require the ANT NFT holder as signer. When the caller of `upgrade_name` / `increase_undername_limit` is the holder, traits sync inline. When not, `try_sync_attributes` skips the CPI silently (logs a `msg!`) and the on-chain `ArnsRecord` becomes the authoritative source of truth — the ANT holder reconciles traits later via the permissionless `sync_attributes` instruction. The `_from_*` stake variants don't carry MPL Core CPI accounts and never sync traits inline; same recovery path. `extend_lease` and its stake variants change no trait-relevant state, so no sync is needed at all. |
| **Rationale** | Direct parity with Lua and the whitepaper's "any ARIO holder can pay to extend/upgrade/increase" model. Reassign and release stay holder-gated because they are stewardship transitions: reassigning hands control of a name to a different ANT, and releasing returns the name to the auction pool. On AO, those checks are encoded as `record.processId == from`; on Solana, "being the ANT" translates to "holding the NFT", so marketplace transfers still implicitly transfer reassign/release authority without any extra instruction. |

### BD-096: ANT On-Chain Trait Sync via UpdatePluginV1 CPI

| | |
|---|---|
| **Lua Behavior** | ANTs on AO are processes with no notion of NFT traits. Marketplaces don't exist in the AO ecosystem in the same shape — display attributes are out of scope. |
| **Solana Behavior** | Each ANT is a Metaplex Core NFT carrying an Attributes plugin (`ArNS Name`, `Type`, `Undername Limit`) for DAS-queryable traits. **Every minted ANT carries the plugin** (populated for ANTs with an ArNS record at mint, empty otherwise) — the migration mint and the SDK `spawnSolanaANT` / `ANT.spawn` both emit it. Plugin authority is `Owner` so the ANT NFT holder can sign trait updates. Every ARIO-ARNS handler that mutates trait-affecting state CPIs into MPL Core's `UpdatePluginV1` to keep the plugin coherent with the source-of-truth `ArnsRecord` PDA: `buy_name` and `buy_returned_name` set traits at purchase, `upgrade_name` flips Type to `permabuy`, `increase_undername_limit` writes the new limit, `release_name` clears traits, `reassign_name` clears traits on the OLD asset (the new asset's traits are deferred to a permissionless `sync_attributes` call by its current holder). When the buyer in a purchase is not the ANT owner, the CPI is skipped at runtime and `sync_attributes` is the recovery path. |
| **Rationale** | Marketplaces (Tensor, Magic Eden, Phantom) and DAS providers (Helius, Triton) surface Core's on-chain attribute lists via `getAsset`. Embedding traits in the NFT itself avoids the stale-JSON problem when traits change (undername limit increase, lease → permabuy upgrade) and lets users filter/sort by trait without a separate indexer. The "empty plugin on every ANT" invariant means `buy_record` can rely on `UpdatePluginV1` (which requires the plugin to exist) without falling back to `AddPluginV1`. After the migration claim flow runs, the user holds the asset's Update Authority too (per ADR-013) — they can patch their own metadata and traits without AR.IO's intervention. See ADR-012, ADR-013, and `programs/ario-arns/src/mpl_core_cpi.rs`. |

### BD-097: Primary Name — Undername Record Owner Authorization

| | |
|---|---|
| **Lua Behavior** | Only the ANT process owner could create/approve a primary name request (per BD-092). Undername records had no separate owner concept — there was just the ANT's process ID. |
| **Solana Behavior** | `request_and_set_primary_name`, `approve_primary_name`, and `remove_primary_name_for_base_name` authorize the **effective owner** of the name's `AntRecord` (the `@` record for base names, the undername record otherwise), computed by `read_ant_record_owner` from two canonical `ario_ant` accounts: the `AntRecord` PDA (`remaining_accounts[2]` for request_and_set, `[1]` for approve/remove) and the `AntConfig` PDA (`[3]` / `[2]`). Both are validated (owner program == `ario_ant::ID`, PDA derivation, discriminator, Borsh layout). The effective owner mirrors ario-ant's reconciliation invariant: if the record is **stale** (`AntRecord.last_reconciled_owner != AntConfig.last_known_owner`, i.e. the NFT transferred and no ario-ant op has touched this record yet) it is `AntConfig.last_known_owner` (the current ANT owner) — the stale per-record `owner` delegate and stale `last_reconciled_owner` are both ignored; if the record is **reconciled**, it is the explicit `AntRecord.owner` delegate when set, else `AntConfig.last_known_owner`. So a former delegate (or former holder) can never override the current ANT owner once the wrapped `ario_ant::transfer` has run. |
| **Rationale** | Solana ANTs introduce a new actor — the per-undername record owner — distinct from the AO model where the ANT process was the sole authority. Letting a *reconciled* record owner set a primary name for *their* undername without going through the ANT holder lets undername delegations be useful for personal naming (e.g. an ANT holder grants `alice_company.ar` to Alice; Alice can claim that as her primary name without contacting the holder for every request). The freshness gate (`last_reconciled_owner == last_known_owner`) is exactly ario-ant's own H-7 clear-on-transfer test applied read-only, closing the post-transfer stale-delegate and stale-implicit-owner windows without ario-core reading the MPL Core asset (ADR-016). `AntConfig.last_known_owner` — not the per-record `last_reconciled_owner` — is the implicit-owner source because the wrapped `transfer` updates the former immediately but touches no `AntRecord`. See BD-109 for the canonical-`ario_ant`-only (BYO-ANT) limitation. Always-pass policy in the SDK supplies the AntRecord + AntConfig PDAs for the relevant name; ANT-holder callers are covered by the same path. See `PLAN_undername_primary_name.md`. |

---

### BD-110: Stake-Funded ArNS Variants Defer Trait Sync

| | |
|---|---|
| **Lua Behavior** | AO has no stake-funded ArNS purchase variants and no NFT trait surface to keep coherent. |
| **Solana Behavior** | The `_from_delegation` and `_from_operator_stake` siblings of every fee-paying ArNS handler (`buy_name_from_*`, `buy_returned_name_from_*`, `upgrade_name_from_*`, `increase_undername_limit_from_*`) intentionally **do not** carry an `ant_asset` account in their `Accounts` structs. As a result, they cannot CPI `UpdatePluginV1` to sync the MPL Core Attributes plugin — that sync happens via the permissionless `sync_attributes` instruction, which the new owner (or any cranker) calls after the purchase to populate traits. The wallet-funded variants (`buy_name`, `buy_returned_name`, etc.) DO sync inline when the buyer holds the ANT NFT. |
| **Rationale** | Stake-funded paths execute under the gateway operator's or delegator's signer authority — neither of whom necessarily holds the ANT NFT, and the plugin's `Owner` authority requires the NFT holder to sign `UpdatePluginV1`. Forcing the operator/delegator to coordinate with the future ANT holder for an inline sync would be brittle and out-of-band. Deferring to `sync_attributes` (which reads the Gateway-PDA-validated `ArnsRecord` and writes traits when called by the current ANT owner) is the cleaner separation. Audit notes: M11 / N-ARNS-2 originally rated this as a missing CPI; verified 2026-04-30 the design is correct under the option-C registry semantics. |

---

### BD-098: Returned-Name Auctions for Protocol-Initiated Returns

| | |
|---|---|
| **Lua Behavior** | When an expired name is pruned in the AO process, it goes into a Dutch auction. The protocol receives 100% of the auction proceeds (no separate initiator). |
| **Solana Behavior** | `prune_to_returned` (permissionless cleanup of expired names past their grace period) sets `returned_name.initiator = config_pda`. Subsequent `buy_returned_name` calls take an `initiator_token_account` whose `owner` field must equal the stored initiator. For protocol-initiated returns the initiator is the config PDA — anyone can permissionlessly create such a token account via SPL `InitializeAccount` (which accepts an arbitrary `owner` pubkey with no signature). The handler's `is_protocol_initiator == true` branch then runs: 100% of `token_cost` flows to the protocol token account, the initiator vault is touched but receives nothing. |
| **Rationale** | The constraint shape (`initiator_token_account.owner == returned_name.initiator`) is uniform across the `release_name → buy` and `prune_to_returned → buy` paths, so handler logic and account ordering stay simple. The "create-the-vault-permissionlessly" workaround is well-known SPL behavior and the cranker can set up the vault before the auction window closes. Audit M9 originally rated protocol-initiated returns as "unbuyable in practice" because SDK callers without permissionless-init knowledge would be stuck — that's a UX/SDK gap, not a security issue. SDK-side helper to auto-create the vault when needed is a follow-up; on-chain behavior is correct. See `programs/ario-arns/src/instructions/purchase.rs::buy_returned_name`. |

---

### BD-099: Demand Factor Manipulation via Permissionless Manage Variants

| | |
|---|---|
| **Lua Behavior** | `extend_lease`, `upgrade_name`, and `increase_undername_limit` are owner-authorized in AO. Only the ArNS record holder can run them, so demand-factor counters (`purchases_this_period`, `revenue_this_period`) are driven exclusively by holder activity. |
| **Solana Behavior** | The same handlers are permissionless on Solana (matches the post-2026-04 Lua spec — see PR `feat(arns/permissionless)` at commit `91ed230`). Anyone with ARIO can call `extend_lease` or `upgrade_name` against any name they don't own; the cost is paid from the caller's own token account, and the operation succeeds. As a side effect, the caller's payment ticks the demand-factor's per-period counters and can elevate prices for the rest of the period. |
| **Rationale** | A whale could spam these handlers to bid up `demand_factor` and indirectly raise prices for legitimate buyers within the same period. Cost to the attacker is real (each tx pays the actual upgrade/extend price). Lua behaves the same way after its own permissionless conversion, so this isn't a Solana divergence — but it's worth noting because the optics differ from how observers might expect demand factor to evolve. No code mitigation; documented for transparency and to inform demand-factor monitoring expectations. |

### BD-108: prescribe_epoch Roulette Modulus Is the Live Registry Sum, Not the Tally Snapshot (2026-05-28)

| | |
|---|---|
| **Lua Behavior** | AO has a single continuous process — there is no `tally_weights` / `leave_network` ordering window between fixing a "total weight" and using it for observer selection. Selection draws from the live registry every time. |
| **Solana Behavior (before)** | `prescribe_epoch` sampled `random_value % epoch.total_composite_weight`, where the total was the snapshot accumulated by `tally_weights`. `leave_network` and `prune_gateway` zero the leaver's slot weight in-place but never decrement `epoch.total_composite_weight` (registry indices are preserved mid-epoch to keep `failure_counts[i]` stable — see BD-102). A leaver between tally and prescribe therefore made the modulus stale by the leaver's full weight, opening a dead random_value range that the inner walk could not cover. A fallback ("if !found, select the last non-zero slot") attributed every dead-range hit to a single tail slot, collapsing observer selection probability onto whichever gateway sat at the highest non-zero index. With weights `[1, 100, 1]` and the middle leaving, the tail gateway received ~101/102 selection share instead of ~1/2. |
| **Solana Behavior (now)** | `prescribe_epoch` recomputes `total_weight` as a live walk-and-sum of `registry.gateways[..active_count]` immediately before the roulette loop, eliminating the dead range entirely. The `if !found` fallback is removed because every `random_value` now lands inside an actual non-zero slot. `epoch.total_composite_weight` remains the tally snapshot (still reported by `EpochWeightsTalliedEvent`) but is no longer load-bearing for selection. See ADR-023. |
| **Why** | Found by a Codex security review, 2026-05-28. The selection bias is high-severity for protocol integrity: prescribed observers submit `gateway_results` that drive `failure_counts`, observer/missed-observer rewards, and downstream `prune_gateway` eligibility. The bug was also self-reinforcing — biased observers can fail-report a high-weight gateway, eventually making it prunable, and a permissionless `prune_gateway` in the tally→prescribe window re-triggers the bias for the next epoch. The live recompute is O(active_count) ≤ 3000, trivial relative to the CU budget, and keeps the leave/prune paths unchanged (no new accounts on `LeaveNetwork`/`PruneGateway`, no cross-flow race risk). |
| **Indexer impact** | None on the wire format: no IDL changes, no event-shape changes, no state-struct changes. Indexers that decode `Epoch.total_composite_weight` should be aware its semantics shifted from "the modulus prescribe will sample on" to "the tally snapshot at the moment `weights_tallied` flipped to 1" (a more honest framing — it was already stale-after-leave under the old code). Indexers that derived observer-selection probabilities from `Epoch.total_composite_weight` were always modeling the buggy distribution; the source of truth is now `sum(registry.gateways[..active_count].composite_weight)`. |

### BD-109: Name-Ownership-Gated Flows Are Canonical-ANT-Only — BYO ANT Programs Unsupported (2026-06-02)

| | |
|---|---|
| **Lua Behavior** | AO has one canonical ANT process module; `processId == from` authorizes every name-stewardship op uniformly. There is no "which program owns this asset" question (see BD-100), so primary-name / reassign / release all resolve ownership against the single ANT process with no per-asset routing. |
| **Solana Behavior** | The name-ownership-gated flows resolve the ANT holder through program-specific paths that assume the **canonical** ANT, so a third-party ("bring your own") ANT program — one named via the asset's `ANT Program` Attributes trait (BD-100) — cannot drive any of them: <br>• **Primary-name** (`ario_core`: `request_and_set_primary_name`, `approve_primary_name`, `remove_primary_name_for_base_name`, and the `*_from_funding_plan` variant) is **hard-pinned to `ario_ant::ID`** in `read_ant_record_owner` (the canonical-lockdown `require!`) and now reads the **`ario_ant` `AntConfig.last_known_owner`** PDA for the implicit-owner fallback. Both the program pin and the `["ant_config", mint]` PDA are `ario_ant`-specific. <br>• **Reassign / release** (`ario_arns`: `reassign_name`, `release_name`) authorize against `read_mpl_core_owner(ant_asset)` — i.e. they assume the ANT is a **Metaplex Core** asset (BD-106). A non-MPL-Core ANT program is unsupported. <br>Net: BD-100's pluggable `ANT Program` trait currently routes only **read** paths (resolvers / PDA derivation); these **write-authorization** paths ignore it and behave as canonical-only. |
| **Why** | `ario_core` is MPL-agnostic per ADR-016 and cannot read the live NFT owner, so primary-name authorization must trust an owner snapshot maintained by *some* ANT program. Until pluggable resolution exists it trusts the canonical `ario_ant` `AntConfig` (the only program whose snapshot layout/PDA ario-core knows). Pinning to `ario_ant::ID` is the safe default — without it, an attacker-deployed program could forge an `AntConfig`/`AntRecord` and satisfy the seed/owner checks (the program-id-spoof attack the lockdown closes). Reassign/release deliberately read the MPL asset owner directly (a free read, no `mpl-core` dep) to restore Lua's current-holder parity (BD-106), which couples them to MPL Core rather than to an arbitrary ANT program. |
| **Fix path (deferred)** | Add **pluggable ANT-program owner-resolution** to `ario_core` (and generalize `ario_arns`): read the asset's `ANT Program` Attributes-plugin trait (BD-100, the same routing key resolvers already use), and for a third-party program load *its* owner-snapshot account (the conformance contract for `AntRecord`/`AntConfig` byte-compatibility is already documented in `read_ant_record_owner` / `read_ant_config_last_known_owner`). That lets a BYO ANT participate in primary-name/reassign/release without `ario_core` reading MPL Core directly. Tracked as an ADR-016 follow-up. |
| **Integrator impact** | None for canonical ANTs (the overwhelming majority). Anyone shipping a custom ANT program per BD-100 should know these specific flows will reject (`NotAntHolder` / `InvalidAccountState`) for their assets until the fix lands; all *other* ANT operations and all *read* resolution already honor the `ANT Program` trait. |

### BD-105: Migrated Vaults Are Non-Revocable (2026-05-27)

| | |
|---|---|
| **Lua Behavior** | AO `vaults` can be revocable: a grantor (`vaultedTransfer` sender) locks tokens for a beneficiary and may `revokeVault` to claw them back before expiry. Revocability + revoker identity are part of AO vault state. |
| **Solana Behavior** | `ario-ant-escrow` neither accepts nor produces revocable vaults: `deposit_vault` rejects `revocable=true` (`RevocableVaultUnsupported`) and active-vault claim re-locks must be non-revocable (`verify_vaulted_transfer_in_tx`). Claimants own the re-locked vault and withdraw via `release_vault` at expiry. `ario_core::vaulted_transfer(revocable=true)`/`revoke_vault` remain valid for *direct* (non-escrow) use — only the escrow opts out. |
| **Why** | The escrow has no field for the legitimate revoker, so a revocable re-lock could only assign control to the unbound claim-tx payer (`vaulted_transfer` sets `controller = sender`, and forbids `sender == recipient` so it can't be the claimant) — a theft vector (Codex finding). The migration importer (`solana-ar-io` `batch-escrow.ts`) already deposits every vault non-revocable, so no AO revocability is actually lost. See ADR-021 for the decision and the rejected alternatives (grantor-binding; authority-as-revoker). |
| **Solana-only nuance** | `EscrowToken.vault_revocable` is retained for layout/ABI stability but is always `false`. Faithful preservation of AO clawback would require capturing the grantor in escrow state + a second Arweave→Solana attestation for that grantor — out of scope for the migration. **(2026-05-28: the active-vault re-lock path that `verify_vaulted_transfer_in_tx` guarded was subsequently removed entirely — see BD-107; `deposit_vault`'s revocable rejection still stands.)** |

### BD-107: Escrow Active-Vault Claims Disabled — Liquid-at-Expiry Only (2026-05-28)

| | |
|---|---|
| **Lua Behavior** | AO has no escrow; migrated vaults retain their original lock and unlock naturally. There is no "claim a still-locked vault early and re-lock it" concept. |
| **Solana Behavior (before)** | `claim_vault_arweave_attested` / `claim_vault_ethereum` had an **active** path: claiming a still-locked vault released tokens to a wallet (`payer_token_account`) and required a sibling `ario_core::vaulted_transfer` (verified by transaction introspection) to re-lock them for the claimant, preserving the remaining duration. |
| **Solana Behavior (now)** | The active path is **removed**. While `clock < vault_end_timestamp` a vault claim is rejected with `VaultStillLocked`. Vaults are claimable only after `vault_end_timestamp`, delivering **liquid** tokens to the claimant (the unchanged expired path). `payer_token_account` was dropped from both claim ixs (and the now-dead `instructions_sysvar` from the Ethereum claim; the Arweave claim keeps it for the Ed25519 attestation). The `vault_introspect` module was removed. |
| **Why** | The introspection re-lock had no 1:1 binding between a claim and the `vaulted_transfer` it credited (it matched amount/duration/recipient/non-revocable and scanned read-only without consuming), so one `vaulted_transfer` could satisfy N batched claims for the same claimant + identical amount — locking once and leaving `(N-1)×amount` liquid (lock bypass / relayer skim; Codex finding). The active re-lock granted the claimant no liquidity (locked→locked), nothing depended on it (the migration claims vaults liquid-after-expiry), token/vault escrows are never purged (so waiting can't strand funds), and escrow is pre-mainnet. Disabling closes the vector by construction without an ario-core change. The heavier direct-CPI alternative (which would keep early-claim-with-preserved-lock) is recorded in ADR-022 as the path to revive the feature if ever needed. The revocable-controller variant was already closed by ADR-021 / BD-105. |
| **Scope / unchanged** | Expired liquid claims, attestation verification, asset-type/protocol/nonce guards, escrow close, `deposit_vault` (incl. its revocable rejection), `cancel_vault_deposit`, `update_vault_recipient`, and all ANT/token claim paths are unchanged. The `MissingVaultedTransferInstruction` / `RevocableVaultUnsupported` error variants are retained (append-only error ABI). See ADR-022. |

### BD-106: ArNS Reassign/Release Authorize Against Current ANT Holder, Restored (2026-05-27)

| | |
|---|---|
| **Lua Behavior** | `reassignName` / `releaseRecord` authorize via `record.processId == from` — the controlling ANT, not a frozen buyer identity. Transferring the ANT implicitly moves reassign/release authority (BD-095). |
| **Regression** | The ADR-016 "MPL-agnostic" reshape (which dropped ario-arns's `mpl-core` crate dep and its `UpdatePluginV1` trait-sync CPI) over-corrected and rewrote both handlers' authorization to `caller == ctx.accounts.arns_record.owner`. But `ArnsRecord.owner` is written once at `buy_name` and **never updated** — so after a holder sold/transferred the ANT, the *prior* buyer retained reassign/release authority indefinitely while the new holder could never obtain it. The in-code comment claimed a compensating SDK flow (`ant.transfer → claim-name → reassign`) that does not exist (there is no `claim-name` instruction). High-severity authorization bypass; live on `develop`. Found by a Codex security review, 2026-05. |
| **Solana Behavior (restored)** | `reassign_name` and `release_name` again authorize against the **current** Metaplex Core NFT holder. Each `Accounts` struct gains an `ant_asset` account constrained to `arns_record.ant` (the ANT the name is currently bound to) and to `owner == MPL_CORE_PROGRAM_ID`; the handler reads the asset's owner bytes via a local `read_mpl_core_owner` (a copy of `ario_ant`'s helper) and requires `caller == owner`, else `ArnsError::NotAntHolder`. A raw owner-byte read needs neither the `mpl-core` crate nor a CPI, so it stays ADR-016-compatible. `ArnsRecord.owner` remains as an informational-only field. The `_from_delegation` / `_from_operator_stake` variants are unaffected — reassign/release have no fee-paying stake variants. |
| **Integrator impact** | `reassign_name` and `release_name` now take an additional `ant_asset` account (the current `record.ant` asset, read-only). SDK / tooling that builds these instructions must pass it — see the `solana-ar-io` follow-up. |
| **Why** | Direct parity with Lua's `record.processId == from` and the whitepaper model where holding the ANT NFT confers stewardship. The reshape's mistake was conflating "ario-arns no longer *writes* MPL Core" (true post-ADR-016) with "ario-arns can no longer *read* the holder" (false — a read is free). See BD-095 for the two-tier authorization model this restores. |

### BD-104: Epoch Counter Carry-Over from AO at Solana Genesis (2026-05-19)

| | |
|---|---|
| **Lua Behavior** | N/A — AO is a single continuous process; the epoch counter has no "genesis" concept. It advances monotonically from process spawn forward. At any cutover the operator sees a single AO epoch number (e.g. 405). |
| **Solana Behavior** | `initialize_epochs` hardcodes `current_epoch_index = 0` and `genesis_timestamp = clock.unix_timestamp` (initialize.rs:223-224). Solana therefore starts a *fresh* epoch counter at genesis. The cutover-friendly admin lever `admin_set_current_epoch_index(new_index)` (added 2026-05-19, see `instructions/epoch.rs`) lets the migration authority set `current_epoch_index` to any non-zero value once — typically AO's last-finalized-epoch + 1 — AND re-anchors `genesis_timestamp` to `now - new_index * epoch_duration` so the first `create_epoch` fires immediately for that index. Pre-conditions: `enabled == false` AND `current_epoch_index == 0` (one-shot; permanently locks after first use or after `create_epoch` advances the counter). |
| **Why carry over** | Reward decay (`REWARD_DECAY_START_EPOCH = 365`, `REWARD_DECAY_LAST_EPOCH = 547`) was authored against AO's epoch numbering. Fresh-starting at 0 would reset the entire decay schedule, giving migrated gateways an extra ~1 year of full rewards. Carrying over preserves the decay schedule continuity, plus operator dashboards / indexers / monitoring see uninterrupted epoch numbering across the cutover. Without this lever the alternative was either grinding through hundreds of `create_epoch` no-ops via the cranker, or accepting the decay reset. |
| **Solana-only nuance** | The instruction is irreversible — once `current_epoch_index != 0`, the lever rejects with `EpochCounterAlreadyAdvanced`. There's no `admin_reset_current_epoch_index`. If genesis is botched, recovery requires `close_epoch_settings` → `initialize_epochs` → `admin_set_current_epoch_index` (all authority-gated). The 100,000 upper bound (~273 years at daily epochs) is operator-typo defense, not a real cap. |
| **Event** | `EpochCounterAdvancedEvent { admin, new_index, old_genesis_timestamp, new_genesis_timestamp, epoch_duration, timestamp }` — indexers need this to know epochs 0..(new_index-1) were never actually created on-chain; otherwise an indexer scanning `EpochCreatedEvent`s would see epoch numbers start mid-range and might log warnings or treat earlier indices as missing data. |

### BD-103: On-chain Event Coverage (2026-05-05)

| | |
|---|---|
| **Lua Behavior** | Each state-changing handler emits an AO "notice" via `ao.send({Target=..., Tags={Action="<X>-Notice"}})`. There are dozens of these — `Joined-Network-Notice`, `Stake-Notice`, `Buy-Record-Notice`, `Set-Record-Notice`, `Vault-Notice`, etc. — typically including the actor address, the affected resource, and the per-handler payload (cost, amount, settings). Reads are *also* notice-bearing in AO because the message-based model requires a response per query. There is no separate "events" namespace; everything is a notice. |
| **Solana Behavior** | Anchor `#[event]` emits surface as `Program data: <base64>` log lines that decode to `[discriminator(8) || borsh_payload]`. **Total surface:  74 events / 127+ emit sites across 5 programs:** ario-core (13), ario-gar (30), ario-arns (12), ario-ant (15), ario-ant-escrow (4). Every state-changing instruction emits exactly one event after all state mutations succeed (batched ops emit one summary event per tx, never inside the loop, to avoid log-truncation). The SDK exposes `parseTransactionEvents(rpc, signature)` and `parseEventsFromLogs(logs)` returning typed discriminated unions; see `sdk/src/solana/events.ts`. CPI-emitted events are correctly attributed to the emitting program (the log walker tracks `Program <id> invoke/success` frames). |
| **Lua-parity coverage** | Most Lua notices have a 1:1 Solana event: `Joined-Network-Notice`→`GatewayJoinedEvent`, `Stake-Notice`→`DelegationEvent`, `Buy-Record-Notice`→`NamePurchasedEvent`, `Set-Record-Notice`→`RecordSetEvent`, `Vault-Notice`→`VaultCreatedEvent`/`VaultExtendedEvent`/`VaultIncreasedEvent`, `Reserve-Name-Notice`→`NameReservedEvent`, `Update-Gateway-Settings-Notice`→`GatewaySettingsUpdatedEvent`, etc. Lua's per-field setters (`Set-Name-Notice`, `Set-Ticker-Notice`, ...) collapse onto one Solana shape with a `field: u8` discriminator (`AntMetadataUpdatedEvent`). |
| **Solana-only events** | (a) ANT NFT-level transfer / reconcile / Attributes-sync (Lua had no asset abstraction); (b) ACL events (Lua had no per-asset ACL pages); (c) `EpochWeightsTalliedEvent` / `EpochClosedEvent` (Solana batches what Lua does inline in epoch tick); (d) all 4 ario-ant-escrow events (entire program is Solana-only); (e) `*MigrationFinalizedEvent` / `SupplyFinalizedEvent` / `ConfigUpdatedEvent` / `*ToggledEvent` (one-time / admin watershed markers indexers need); (f) `StakePaymentEvent` / `WithdrawalPaymentEvent` / `FundingPlanAppliedEvent` / `ResidueVaultCreatedEvent` (multi-source funding payment paths). |
| **Lua reads we deliberately don't port** | `Record-Notice`, `Records-Notice`, `Balance-Notice`, `Controllers-Notice`, `State-Notice` — read paths are served by RPC on Solana. No transaction, no event, no cost. |
| **Wire encoding** | All discriminator constants are stable forever (`u8` / `u32` / `[u8; 32]` / `[u8; 64]`). Examples: `FUNDING_SOURCE_*` 0..5 (`Balance / Delegation / OperatorStake / Withdrawal / FundingPlan / Turbo`), `PURCHASE_TYPE_*` 0/1, `PRUNED_KIND_*` 0/1/2, `ESCROW_ASSET_*` 0/1/2, `PROTOCOL_*` 0/1, `ANT_METADATA_FIELD_*` 0..4, `ACL_ROLE_*` 0/1, `CORE_CONFIG_FIELD_*` 0..3. See ADR-017 for the policy. |
| **Field shape uniformity** | Every state-changing event ends with `timestamp: i64` populated from `Clock::get()`. Convention: actor → identifier → payload → discriminator → timestamp. |
| **Audit trail** | `docs/EVENT_EMISSION_AUDIT.md` (gap analysis), `docs/EVENT_EMISSION_IMPLEMENTATION_PLAN.md` (build plan), `contracts/idl-event-snapshots.json` (ABI snapshot). 8 PRs in the rollout: PR-0 test infra → PR-1..6 per-program emit additions → PR-6.5 consistency cleanup → PR-7 SDK decoders → PR-8 e2e → PR-9 docs (this entry). |
| **Rationale** | Pre-2026 we had ~20 events covering only ario-core + ario-gar (and even those incomplete — gar gained 15 instructions post-original-plan with no emits). Indexers (portal "recent purchases" feed, ANT marketplace transfer sync, escrow claim app status, observer/cranker dashboards) were forced to either snapshot every account every block or reverse-engineer instruction data. The audit identified 49 missing events / 91 missing emits; the rollout closed 100% of those gaps and added the SDK decoder layer that makes consumption trivial. |

---

### BD-102: leave_network Two-Vault Split — Closed (2026-05-04)

| | |
|---|---|
| **Lua Behavior** | `gar.leaveNetwork` (`gar.lua:178-214`) creates **two** vaults with **different lock periods**: (1) a protected exit vault holding `min(minStake, operatorStake)` (typically the 20k ARIO floor), keyed by the gateway address, locked for `leaveLengthMs` (**90 days**) via `createGatewayExitVault` (`gar.lua:2061-2069`), **cannot be instantly withdrawn** ("This vault is protected"); and (2) a regular withdraw vault holding `operatorStake - minStake` (the excess), keyed by `msgId`, locked for `withdrawLengthMs` (**30 days**) via `createGatewayWithdrawVault` (`gar.lua:2047-2055`), expedite-able with penalty. The funding-plan mechanism never reaches operator vaults at all (`planVaultsDrawdown` only iterates `gatewayInfo.delegate.vaults`), so the min portion is doubly protected. |
| **Solana Behavior** | `leave_network` (`gateway.rs::leave_network`) and `prune_gateway` (`gateway.rs::prune_gateway`) produce **0, 1, or 2** `Withdrawal` PDAs depending on the relationship between `operator_stake` and `min_operator_stake`, mirroring Lua's split exactly — INCLUDING the different lock periods: protected exit vault uses `GATEWAY_LEAVE_PERIOD` (90 days, hardcoded const), excess vault uses `settings.withdrawal_period` (30 days default, configurable via `admin_set_withdrawal_period`). Layout: `Withdrawal` gained a fourth flag `is_protected: bool` (offset 106; SIZE 107 → 108). `is_protected: true` is set on the min-portion exit vault; the excess vault is `is_protected: false`. `instant_withdrawal` and `deduct_withdrawal_for_payment` reject when `is_protected: true` with `GarError::ProtectedVault`. SDK `fetchUserWithdrawals` filters protected vaults from the funding-plan source pool. The 90-day lock on the operator min stake is now actually enforced — the only path out is `claim_withdrawal` after the lock expires. |
| **Excess-lock correction (2026-05-18)** | Original BD-102 (2026-05-04) incorrectly described `withdrawLengthMs` as 90 days. The Lua source has it as 30 days. The Solana port matched the (incorrect) doc and locked the excess vault for 90 days too. Corrected: `leave_network` now uses `settings.withdrawal_period` for the excess vault's `available_at`, restoring Lua parity. Same fix in `claim_delegate_from_leaving_gateway` (was using the hardcoded `WITHDRAWAL_LOCK_PERIOD` const; now reads from settings so the admin lever applies). |
| **Edge cases (Lua-faithful)** | `pre_stake >= 2*min` → exit (min, protected) + excess (`pre - 2*min`); `min < pre < 2*min` → exit only (sub-min size, protected); `pre == min` → exit only (full min, protected); `pre == 0` → zero-amount placeholder for rent reclaim. For prune, the same matrix applies on the post-slash remainder. |
| **Account schema** | `LeaveNetwork` and `PruneGateway` accounts gain an `Option<UncheckedAccount>` `excess_withdrawal` slot. Anchor `init` always creates the exit vault; the excess vault is created via `system_program::{transfer, allocate, assign}` CPIs in the handler when `excess_amount > 0` (matches the residue-vault pattern in `pay_from_funding_plan`). Counter advances by 1 or 2 depending on whether the excess vault was created. New errors: `GarError::ProtectedVault`, `MissingExcessWithdrawal`, `InvalidExcessWithdrawalPda`. |
| **Migration** | None required pre-mainnet. The contract is not yet deployed to mainnet; devnet can be wiped and redeployed with the new layout from day one. Any combined-vault legacy state on a cluster that ships this without a fresh redeploy would need a one-shot `backfill_withdrawal_is_protected` admin ix to flip the flag + realloc the 1-byte SIZE delta — not implemented since the use-case doesn't apply to current deployment plan. |

### BD-101: prescribe_epoch Accepts remaining_accounts in Any Order

| | |
|---|---|
| **Lua Behavior** | N/A — Lua dispatches observer prescription internally without an account-passing protocol; the Solana port has no Lua analogue for the cranker's account-providing role. |
| **Solana Behavior** | `prescribe_epoch` runs weighted-roulette selection from on-chain state (registry composite_weights + epoch hashchain) and writes results into `epoch.prescribed_observer_gateways[0..selected_count]` in **selection order**. The cranker MUST supply a Gateway PDA for every selected observer in `remaining_accounts`, but those PDAs may appear in **any position** — the handler searches `remaining_accounts` by expected PDA rather than indexing positionally. NameRegistry (when name prescription is active) must still be the LAST entry by convention. |
| **Rationale** | The original positional design (commit history pre-2026-05) silently failed for any `active_gateway_count > 1` whose hashchain selection didn't happen to mirror registry-iteration order — a 5/6 chance of failure for 3 equal-weight gateways. The cranker enumerates the registry in slot order, but selection picks observers in roulette order; the two orderings only align by chance. The order-tolerant handler matches the cranker's natural behavior with no protocol change. Selection determinism, security checks (program-owned, PDA-validated, deserialize, operator cross-check), and observer authentication (`observer_address` from the validated gateway, never from the caller) are unchanged. CU cost is `O(selected_count × remaining.len())` pubkey compares + `selected_count` PDA derivations — well under the 1M CU budget at protocol caps (max 50 observers, ≤~24 accounts per tx). See regression test `test_prescribe_epoch_accepts_unordered_remaining_accounts` in `programs/ario-gar/tests/integration.rs`. |

---

### BD-100: ANT Program Selection — Asset Attributes Plugin

| | |
|---|---|
| **Lua Behavior** | AO has a single canonical ANT process module. Every ANT is spawned from that module; there is no "which program owns this asset's state" question, and no per-asset routing. |
| **Solana Behavior** | Each ANT carries an `ANT Program` entry in its Metaplex Core Attributes plugin, naming the Solana program that owns the asset's per-mint state PDAs (controllers, undername records). Resolvers (SDK `SolanaANTReadable.fromAsset`, ARIO-CORE BD-097 path, ARIO-ARNS trait sync) read the trait off the asset and derive PDAs against the named program. Absence — or any parse failure (truncated buffer, malformed plugin, invalid base58) — falls back silently to the canonical `ARIO_ANT_PROGRAM_ID`. |
| **Rationale** | The AO model has one program; the Solana model lets third parties ship their own ANT programs (curated marketplaces, profile namespaces, etc.). Storing the routing key on the asset (rather than on `ArnsRecord`) keeps it intrinsic to the NFT — the program choice doesn't change with name reassignment, and DAS / marketplace indexers expose it for free alongside the existing ArNS Name / Type / Undername Limit traits. Strict on-chain validation would let a holder grief themselves out of resolution by setting a malformed value, so the parse is intentionally lenient — invalid trait → canonical fallback. Plugin authority is `Owner`, matching the other ArNS-related traits. See ADR-016. |
| **Sprint 3 amendment (2026-05-04)** | Trait *writes* moved from ARIO-ARNS into a dedicated `ario_ant::sync_attributes` instruction. The SDK conditionally bundles `arns.<mutate>` + `ant.sync_attributes` in one transaction for `buyName`, `buyReturnedName`, `upgradeRecord`, `increaseUndernameLimit`, and `reassignName` — the bundle fires only when the SDK signer owns the ANT. `extendLease` is excluded (changes no trait-relevant state per the row above). This preserves BD-095 (non-holder lease management) and the original BD-096 deferred-sync behavior for non-holder buyers. The actual ANT owner reconciles deferred state via the public `syncAttributes()` method (the `sync_attributes` ix's `authority` signer requirement comes from MPL Core's Owner-authority Attributes plugin). `releaseName` is NOT bundled at all because the release ix closes the ArnsRecord PDA — a follow-up `sync_attributes` would fail the PDA-existence + `record.ant == asset.key()` checks. **Consequences for stale traits on assets:** ⓐ a released name leaves stale `ArNS Name` / `Type` / `Undername Limit` traits on the asset until the next holder buys a different name and the next sync_attributes runs; ⓑ a *reassigned* name leaves stale traits on the OLD asset (pre-reshape, ARIO-ARNS' `reassign_name` directly cleared OLD traits via UpdatePluginV1 — that CPI is gone, and `sync_attributes` against the OLD asset would fail the post-reassign `record.ant == asset.key()` check); ⓒ `_from_delegation` / `_from_operator_stake` purchases never bundle (the stake-payer typically isn't the ANT owner). Off-chain resolvers MUST treat the on-chain `ArnsRecord` PDA as the source of truth — a trait pointing at a name without a live record (or pointing at an asset that isn't `record.ant`) means the trait is stale, not that the asset still owns the name. Read paths (the `ANT Program` routing trait — the one BD-100 covers) are unaffected. |
| **Sprint 5 amendment (2026-05-05)** | Added `ario_ant::clear_attributes` — the asset-side recovery path for the stale-trait scenarios the Sprint 3 amendment documented (releases, reassigns, non-holder buys). Asset owner signs (Owner-authority plugin); ix wipes `ArNS Name` / `Type` / `Undername Limit` from the plugin while preserving `ANT Program` (the routing key MUST survive — clearing it would silently revert custom-program ANTs to canonical resolution). No `arns_record` account: the whole point is recovery when no live record exists or when the live record no longer points at this asset. Strictly additive — no existing flow breaks. SDK exposure (composing `clear_attributes` into `releaseName` and `reassignName` bundles when signer owns the OLD asset) is a follow-up. **Multi-name semantics on Solana**: an ANT may have multiple ArnsRecords pointing at it (legal but rare). The Attributes plugin reflects ONE name at a time — whichever was most recently synced via `sync_attributes`. Each call replaces the whole list (UpdatePluginV1 is whole-list-replace). Searching DAS for an associated-but-not-currently-canonical name returns no match for that ANT; the canonical `ArnsRecord` PDA remains the source of truth for "does this ANT own this name." Future v2 work could write multiple `ArNS Name` entries (Attributes plugin allows duplicate keys), making every associated name DAS-searchable; deferred until concrete demand. |

---

## Summary Statistics

| Category | Count |
|---|---|
| Platform-Level | 6 (BD-001 through BD-006) |
| Architectural | 6 (BD-010 through BD-015) |
| Epoch & Rewards | 9 (BD-020 through BD-028) |
| ArNS & Pricing | 6 (BD-030 through BD-035) |
| Staking & Delegation | 9 (BD-040 through BD-048) |
| Vaults | 5 (BD-050 through BD-054) |
| Migration-Specific | 5 (BD-060 through BD-064) |
| Omitted/N/A | 25 (BD-070 through BD-094) |
| ANT NFT / Marketplace | 2 (BD-095, BD-096) |
| Primary Name Authorization | 2 (BD-097, BD-109) |
| ANT Program Routing | 1 (BD-100) |
| Cranker Protocol | 1 (BD-101) |
| **Total** | **77** |

---

## Appendix: Key Constants Cross-Reference

| Constant | Lua Value | Solana Value | Notes |
|---|---|---|---|
| Token decimals | 6 | `TOKEN_DECIMALS = 6` | Same |
| Withdrawal lock | 90 days (in ms) | `WITHDRAWAL_LOCK_PERIOD = 2_592_000` (seconds) | Solana shortens to 30 days |
| Gateway leave | 90 days (in ms) | `GATEWAY_LEAVE_PERIOD = 7_776_000` (seconds) | Same duration |
| Min operator stake | 10,000 ARIO | `MIN_OPERATOR_STAKE = 20_000_000_000` (mARIO) | Solana doubles to 20,000 ARIO |
| Min delegation | 10 ARIO | `MIN_DELEGATION_AMOUNT = 10_000_000` (mARIO) | Same |
| Epoch duration | 86,400,000 ms | `DEFAULT_EPOCH_DURATION = 86_400` (seconds) | Same duration |
| Max observers | 50 | `MAX_OBSERVERS_PER_EPOCH = 50` | Same |
| Grace period | 14 days (in ms) | `LEASE_GRACE_PERIOD = 1_209_600` (seconds) | Same duration |
| Primary name expiry | 7 days (in ms) | `PRIMARY_NAME_REQUEST_EXPIRY = 604_800` (seconds) | Same duration |
| Reward rate max | 0.001 (0.1%) | `MAX_REWARD_RATE = 1_000` (scaled by 1e6) | Same |
| Reward rate min | 0.0005 (0.05%) | `MIN_REWARD_RATE = 500` (scaled by 1e6) | Same |
| Gateway reward ratio | 0.9 (90%) | `GATEWAY_OPERATOR_REWARD_RATE = 900_000` | Same |
| Observer reward ratio | 0.1 (10%) | `OBSERVER_REWARD_RATE = 100_000` | Same |
| Missed observation penalty | 0.25 (25%) | `MISSED_OBSERVATION_PENALTY = 250_000` | Same |
| Max delegate share | 95% | `MAX_DELEGATE_REWARD_SHARE = 9500` (basis points) | Same |
| Distribution batch | N/A (all at once) | `DISTRIBUTION_BATCH_SIZE = 15` | Solana-specific |
| Registry cap (gateways) | Unlimited | `MAX_GATEWAYS = 3_000` | Solana-specific |
| Registry cap (names) | Unlimited | `MAX_NAMES = 200_000` | Solana-specific |
| Max controllers | Unlimited (or soft) | `MAX_CONTROLLERS_PER_ANT = 10` | Solana-specific |
| Max delegates/gateway | Unlimited | `max_delegates_per_gateway = 10_000` | Solana-specific |
| Max undernames/name | Unlimited | `MAX_UNDERNAME_LIMIT = 10_000` | Solana-specific |
| Redelegation fee reset | 7 days (in ms) | `REDELEGATION_FEE_RESET_INTERVAL = 604_800` (seconds) | Same duration |
| Min redelegation penalty | 10% | `MIN_REDELEGATION_PENALTY = 100_000` | Same |
| Max redelegation penalty | 60% | `MAX_REDELEGATION_PENALTY = 600_000` | Same |
| Returned name premium | 50x over 14 days | `RETURNED_NAME_MAX_MULTIPLIER = 50`, `RETURNED_NAME_DURATION_SECONDS = 1_209_600` | Same |
| Min vault duration | varies (in ms) | `DEFAULT_MIN_VAULT_DURATION = 1_209_600` (14 days, seconds) | Same |
| Max vault duration | varies (in ms) | `DEFAULT_MAX_VAULT_DURATION = 6_311_520_000` (200 years, seconds) | Same |
| Tenure weight duration | 180 days (in ms) | `TENURE_WEIGHT_DURATION = 15_552_000` (seconds) | Same |
| Max tenure weight | 4 | `MAX_TENURE_WEIGHT = 4` | Same |
| Rate scale | 1.0 (floating) | `RATE_SCALE = 1_000_000` (fixed-point) | Different representation, same semantics |
| Reward precision | N/A (floating) | `REWARD_PRECISION = 1e18` (u128) | Solana-specific accumulator precision |
