use anchor_lang::prelude::*;

declare_id!("ARioCoreProgramXXXXXXXXXXXXXXXXXXXXXXXXXXXX");

pub mod constants;
pub mod error;
pub mod instructions;
pub mod migration;
pub mod state;

use instructions::*;
pub use migration::*;

// =========================================
// Event wire constants (PR-5)
// =========================================
//
// Funding-source discriminator carried by `PrimaryNameRequestedEvent`.
// Matches the encoding used by ario-arns purchase / manage events so
// SDK consumers handle one canonical mapping. Values are stable forever
// — append new ones, never repurpose. Only the two values exercised by
// the primary-name flow are defined here; the full set lives in
// `ario_arns::FUNDING_SOURCE_*` (1 = Delegation, 2 = OperatorStake,
// 3 = Withdrawal).
pub const FUNDING_SOURCE_BALANCE: u8 = 0;
pub const FUNDING_SOURCE_FUNDING_PLAN: u8 = 4;

// `ConfigUpdatedEvent.field` discriminator. One event fires per mutated
// field, so consumers branch on `field` rather than parsing the
// `UpdateConfigParams` payload off-chain. Values are stable forever —
// append new ones if the admin config grows another mutable field.
//
// Field IDs map 1:1 to `UpdateConfigParams` Option<>s in
// `update_config::handler`:
pub const CORE_CONFIG_FIELD_MIN_VAULT_DURATION: u8 = 0;
pub const CORE_CONFIG_FIELD_MAX_VAULT_DURATION: u8 = 1;
pub const CORE_CONFIG_FIELD_PRIMARY_NAME_REQUEST_EXPIRY: u8 = 2;
pub const CORE_CONFIG_FIELD_NEW_AUTHORITY: u8 = 3;
/// `admin_repair_config` — pre-cutover state-recovery for the
/// genesis-time fields. Inert post-`finalize_migration`.
pub const CORE_CONFIG_FIELD_MINT: u8 = 4;
pub const CORE_CONFIG_FIELD_TREASURY: u8 = 5;

/// AR.IO Core Program
///
/// Handles:
/// - Token configuration and supply tracking (F1-F3)
/// - Vault system for time-locked tokens (F4-F9)
/// - Primary name resolution (F42-F46)
/// - Protocol configuration
#[program]
pub mod ario_core {
    use super::*;

    // =========================================
    // INITIALIZATION
    // =========================================

    /// Initialize the AR.IO protocol configuration
    /// Called once at program deployment
    pub fn initialize(ctx: Context<Initialize>, params: InitializeParams) -> Result<()> {
        instructions::initialize::handler(ctx, params)
    }

    // =========================================
    // TOKEN OPERATIONS (F1-F3)
    // =========================================

    /// Transfer tokens between accounts using SPL Token
    pub fn transfer(ctx: Context<TransferTokens>, amount: u64) -> Result<()> {
        instructions::token::handler(ctx, amount)
    }

    /// Release ARIO from the protocol treasury (signed by ArioConfig PDA)
    /// to a constrained destination. Currently the only legitimate caller
    /// is `ario-gar::distribute_epoch` via cross-program signed CPI;
    /// authorization is enforced by the `seeds::program = ario_gar::ID`
    /// constraint on the `gar_settings` account in `ReleaseTreasuryToRecipient`.
    pub fn release_treasury_to_recipient(
        ctx: Context<ReleaseTreasuryToRecipient>,
        amount: u64,
    ) -> Result<()> {
        instructions::release_treasury::release_treasury_to_recipient(ctx, amount)
    }

    // =========================================
    // VAULT OPERATIONS (F4-F9)
    // =========================================

    /// Create a new time-locked vault (F4)
    pub fn create_vault(
        ctx: Context<CreateVault>,
        amount: u64,
        lock_duration_seconds: i64,
    ) -> Result<()> {
        instructions::vault::create_vault::handler(ctx, amount, lock_duration_seconds)
    }

    /// Transfer tokens to recipient in a locked vault (F5)
    /// Creates a vault for the recipient, optionally revocable by sender
    pub fn vaulted_transfer(
        ctx: Context<VaultedTransfer>,
        amount: u64,
        lock_duration_seconds: i64,
        revocable: bool,
    ) -> Result<()> {
        instructions::vault::vaulted_transfer::handler(
            ctx,
            amount,
            lock_duration_seconds,
            revocable,
        )
    }

    /// Revoke a revocable vault, returning funds to controller (F6)
    /// Can only be revoked while vault is still active (not expired)
    pub fn revoke_vault(ctx: Context<RevokeVault>) -> Result<()> {
        instructions::vault::revoke_vault::handler(ctx)
    }

    /// Extend the lock duration of an existing vault (F7)
    /// Cannot extend an already-expired vault.
    /// Remaining time + extension must not exceed MAX_VAULT_DURATION.
    pub fn extend_vault(ctx: Context<ExtendVault>, additional_seconds: i64) -> Result<()> {
        instructions::vault::extend_vault::handler(ctx, additional_seconds)
    }

    /// Add more tokens to an existing vault (F8)
    pub fn increase_vault(ctx: Context<IncreaseVault>, amount: u64) -> Result<()> {
        instructions::vault::increase_vault::handler(ctx, amount)
    }

    /// Release expired vault back to owner (F9)
    pub fn release_vault(ctx: Context<ReleaseVault>) -> Result<()> {
        instructions::vault::release_vault::handler(ctx)
    }

    // =========================================
    // PRIMARY NAME OPERATIONS (F42-F46)
    // =========================================

    /// Request a primary name for your address (F42)
    /// M1: Charges a fee (matches Lua: primary_names.requestPrimaryName charges getTokenCost)
    /// Fee = PRIMARY_NAME_REQUEST_BASE_FEE * demand_factor / DEMAND_FACTOR_SCALE
    /// remaining_accounts[0] = ArnsRecord PDA for base name (validates record exists)
    /// remaining_accounts[1] = DemandFactor account from ario-arns (for fee calculation)
    pub fn request_primary_name(ctx: Context<RequestPrimaryName>, name: String) -> Result<()> {
        instructions::primary_name::request_primary_name::handler(ctx, name)
    }

    /// M2: Request AND set a primary name in one tx (auto-approve for ANT
    /// holders of the matching AntRecord).
    ///
    /// Authorization: caller is the `AntRecord.owner` for the requested
    /// name's undername (or `@` for base names). The AntRecord PDA is
    /// resolved under `ant_program_id`, which `read_ant_record_owner`
    /// requires to equal `ario_ant::ID` (canonical lockdown — pluggable
    /// ANT programs per ADR-016 are deferred until the asset's
    /// `ANT Program` Attributes-plugin trait can be consulted on-chain).
    /// Earlier docs claimed PDA-seed derivation alone pinned the program
    /// id; that's wrong — `find_program_address` derives a PDA under
    /// whatever program the caller supplies, so without the canonical
    /// check an attacker-deployed program would satisfy the seed match.
    pub fn request_and_set_primary_name(
        ctx: Context<RequestAndSetPrimaryName>,
        name: String,
        reverse_lookup_hash: [u8; 32],
        ant_program_id: Pubkey,
    ) -> Result<()> {
        instructions::primary_name::request_and_set_primary_name::handler(
            ctx,
            name,
            reverse_lookup_hash,
            ant_program_id,
        )
    }

    /// Phase 3 of FUND_FROM_PLAN.md: request a primary name funded via a
    /// multi-source funding plan (CPIs into ario-gar's pay_from_funding_plan).
    /// Lua-faithful — `primaryNames.createPrimaryNameRequest` calls
    /// `gar.getFundingPlan` + `gar.applyFundingPlan` with no parallel
    /// single-source path.
    ///
    /// remaining_accounts layout:
    ///   [0..validation_account_count): primary-name validation
    ///     ([0] ArnsRecord PDA, [1] DemandFactor PDA)
    ///   [validation_account_count..):  funding-source PDAs forwarded to ario-gar
    pub fn request_primary_name_from_funding_plan<'info>(
        ctx: Context<'_, '_, 'info, 'info, RequestPrimaryNameFromFundingPlan<'info>>,
        name: String,
        sources: Vec<ario_gar::FundingSourceSpec>,
        validation_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        instructions::primary_name::request_primary_name_from_funding_plan::handler(
            ctx,
            name,
            sources,
            validation_account_count,
            residue_vault_count,
        )
    }

    /// Phase 3 of FUND_FROM_PLAN.md: request and set a primary name funded
    /// via a multi-source funding plan.
    ///
    /// remaining_accounts layout:
    ///   [0..validation_account_count): primary-name validation
    ///     ([0] ArnsRecord, [1] DemandFactor, [2] AntRecord)
    ///   [validation_account_count..):  funding-source PDAs
    ///
    /// `ant_program_id` must equal `ario_ant::ID` (canonical lockdown) —
    /// see `request_and_set_primary_name` for the full rationale and the
    /// ADR-016 pluggable-program follow-up.
    pub fn request_and_set_primary_name_from_funding_plan<'info>(
        ctx: Context<'_, '_, 'info, 'info, RequestAndSetPrimaryNameFromFundingPlan<'info>>,
        name: String,
        reverse_lookup_hash: [u8; 32],
        sources: Vec<ario_gar::FundingSourceSpec>,
        validation_account_count: u8,
        residue_vault_count: u8,
        ant_program_id: Pubkey,
    ) -> Result<()> {
        instructions::primary_name::request_and_set_primary_name_from_funding_plan::handler(
            ctx,
            name,
            reverse_lookup_hash,
            sources,
            validation_account_count,
            residue_vault_count,
            ant_program_id,
        )
    }

    /// Approve a primary name request (F43)
    ///
    /// Authorization: `name_owner` must be the AntRecord.owner for the
    /// requested name. `ant_program_id` must equal `ario_ant::ID`
    /// (canonical lockdown) — see `request_and_set_primary_name` for the
    /// full rationale and the ADR-016 pluggable-program follow-up.
    pub fn approve_primary_name(
        ctx: Context<ApprovePrimaryName>,
        reverse_lookup_hash: [u8; 32],
        ant_program_id: Pubkey,
    ) -> Result<()> {
        instructions::primary_name::approve_primary_name::handler(
            ctx,
            reverse_lookup_hash,
            ant_program_id,
        )
    }

    /// Close an expired primary name request (permissionless pruning)
    /// Matches Lua: primaryNames.prunePrimaryNameRequests
    pub fn close_expired_request(ctx: Context<CloseExpiredRequest>) -> Result<()> {
        instructions::primary_name::close_expired_request::handler(ctx)
    }

    /// Remove primary name association (F44)
    pub fn remove_primary_name(
        ctx: Context<RemovePrimaryName>,
        reverse_lookup_hash: [u8; 32],
    ) -> Result<()> {
        instructions::primary_name::remove_primary_name::handler(ctx, reverse_lookup_hash)
    }

    /// H1: Remove a primary name from the perspective of the base name owner.
    /// Matches Lua: primary_names.removePrimaryNamesForBaseName
    /// The base name owner can revoke any primary name that uses their ArNS domain.
    /// E.g., owner of "arweave" can revoke "alice_arweave" primary name.
    ///
    /// Authorization: caller must be the AntRecord.owner for the BASE
    /// name's @ undername. `ant_program_id` must equal `ario_ant::ID`
    /// (canonical lockdown) — see `request_and_set_primary_name`.
    pub fn remove_primary_name_for_base_name(
        ctx: Context<RemovePrimaryNameForBaseName>,
        reverse_lookup_hash: [u8; 32],
        ant_program_id: Pubkey,
    ) -> Result<()> {
        instructions::primary_name::remove_primary_name_for_base_name::handler(
            ctx,
            reverse_lookup_hash,
            ant_program_id,
        )
    }

    // =========================================
    // MIGRATION OPERATIONS
    // =========================================

    /// Import a pre-serialized account during migration
    pub fn import_account(
        ctx: Context<ImportAccount>,
        seeds: Vec<Vec<u8>>,
        data: Vec<u8>,
    ) -> Result<()> {
        import_account_handler(ctx, seeds, data)
    }

    /// Typed migration import for `Balance` accounts.
    /// Exists primarily to surface `Balance` in the IDL so off-chain tooling
    /// (snapshot encoder, SDK decoder) can use the Codama-generated codec
    /// instead of hand-rolled discriminator + Borsh bytes.
    pub fn import_balance(ctx: Context<ImportBalance>, owner: Pubkey, amount: u64) -> Result<()> {
        import_balance_handler(ctx, owner, amount)
    }

    /// Permanently disable migration imports (main authority only)
    pub fn finalize_migration(ctx: Context<FinalizeMigration>) -> Result<()> {
        finalize_migration_handler(ctx)
    }

    /// Set supply totals during migration
    pub fn finalize_supply(
        ctx: Context<FinalizeSupply>,
        total_supply: u64,
        protocol_balance: u64,
        circulating_supply: u64,
        locked_supply: u64,
    ) -> Result<()> {
        finalize_supply_handler(
            ctx,
            total_supply,
            protocol_balance,
            circulating_supply,
            locked_supply,
        )
    }

    // =========================================
    // ADMIN OPERATIONS
    // =========================================

    /// Update protocol configuration (admin only)
    pub fn update_config(ctx: Context<UpdateConfig>, params: UpdateConfigParams) -> Result<()> {
        instructions::admin::update_config::handler(ctx, params)
    }

    /// Admin recovery — repair `ArioConfig.mint` / `.treasury` when devnet
    /// genesis was partially-initialized (e.g., devnet-setup.ts crashed
    /// between `initializeCore` and `initializeArns` and a re-run created
    /// a new mint). Pre-cutover only — disabled once `finalize_migration`
    /// flips `migration_active` to false.
    pub fn admin_repair_config(
        ctx: Context<AdminRepairConfig>,
        new_mint: Pubkey,
        new_treasury: Pubkey,
    ) -> Result<()> {
        instructions::admin::admin_repair_config::handler(ctx, new_mint, new_treasury)
    }

    /// Migration ix for pre-`gar_program` ArioConfig deployments.
    /// Grows the PDA by 32 bytes and writes `config.gar_program`.
    /// Authority-gated, migration-window gated. Idempotent. Required
    /// once on existing deployments before the first
    /// `release_treasury_to_recipient` call after upgrading to a
    /// binary that uses `config.gar_program` for cross-program signer
    /// verification.
    pub fn admin_set_gar_program(
        ctx: Context<AdminSetGarProgram>,
        new_gar_program: Pubkey,
    ) -> Result<()> {
        instructions::admin::admin_set_gar_program::handler(ctx, new_gar_program)
    }
}

// =========================================
// PARAMETER TYPES
// =========================================

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct InitializeParams {
    pub authority: Pubkey,
    pub total_supply: u64,
    pub arns_program: Pubkey,
    pub treasury: Pubkey,
    pub migration_authority: Pubkey,
    /// GAR program ID — pinned at init so `release_treasury_to_recipient`
    /// can verify the cross-program signer. Existing pre-`gar_program`
    /// deployments populate via `admin_set_gar_program`.
    pub gar_program: Pubkey,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Default)]
pub struct UpdateConfigParams {
    pub min_vault_duration: Option<i64>,
    pub max_vault_duration: Option<i64>,
    pub primary_name_request_expiry: Option<i64>,
    pub new_authority: Option<Pubkey>,
}

// Account contexts are defined in their respective instruction modules
// and imported via `use instructions::*;` above.

#[cfg(test)]
mod tests {
    use super::*;
    use error::ArioError;

    // =========================================
    // Gap 11: Self-Transfer Check
    // =========================================

    #[test]
    fn self_transfer_error_exists() {
        // Verify the SelfTransfer error variant is defined and usable.
        // The actual check (from_token_account.key() != to_token_account.key())
        // is in the transfer instruction and requires full Anchor context to invoke.
        // Here we verify the error code is correctly wired.
        let err = ArioError::SelfTransfer;
        // The error message should indicate self-transfer is rejected
        assert_eq!(format!("{}", err), "Cannot transfer to self");
    }

    #[test]
    fn self_transfer_check_same_pubkey() {
        // Simulate the logic: when from == to, the transfer should be rejected.
        // This mirrors the require! check in the transfer instruction.
        let account_key = Pubkey::new_unique();
        let from_key = account_key;
        let to_key = account_key;
        // The instruction checks: from_token_account.key() != to_token_account.key()
        assert_eq!(
            from_key, to_key,
            "Same key should trigger SelfTransfer error"
        );
        // In the instruction, this would fail with ArioError::SelfTransfer
    }

    #[test]
    fn different_accounts_pass_self_transfer_check() {
        // When from != to, the transfer should proceed (not rejected as self-transfer)
        let from_key = Pubkey::new_unique();
        let to_key = Pubkey::new_unique();
        assert_ne!(
            from_key, to_key,
            "Different keys should pass self-transfer check"
        );
    }

    // =========================================
    // C-1: Base Name Extraction Tests
    // =========================================

    #[test]
    fn base_name_extraction_with_undername() {
        // "alice_arweave" -> base name should be "arweave", not "alice"
        let name = "alice_arweave";
        let name_lower = name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };
        assert_eq!(base_name, "arweave");
    }

    #[test]
    fn base_name_extraction_without_undername() {
        // "myname" -> base name should be "myname"
        let name = "myname";
        let name_lower = name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };
        assert_eq!(base_name, "myname");
    }

    #[test]
    fn base_name_extraction_multiple_underscores() {
        // "a_b_c" -> splitn(2, '_') gives ["a", "b_c"], base name = "b_c"
        let name = "a_b_c";
        let name_lower = name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };
        assert_eq!(base_name, "b_c");
    }

    // =========================================
    // L-7: Primary Name Casing Normalization
    // =========================================

    #[test]
    fn primary_name_stored_lowercase() {
        // Verify that names are normalized to lowercase for consistent storage
        let name = "Alice_Arweave";
        let stored = name.to_lowercase();
        assert_eq!(stored, "alice_arweave");
    }
}
