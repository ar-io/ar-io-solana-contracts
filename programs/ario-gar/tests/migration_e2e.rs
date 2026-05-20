//! Migration E2E tests — simulates four GatewaySettings schema versions.
//!
//! Run with:
//!   cargo test -p ario-gar --features migration-test --test migration_e2e
//!
//! This suite is compiled only when the `migration-test` feature is active.
//! Under that feature:
//!   - `GatewaySettings` gains three sentinel fields appended after `version`:
//!       `field_1: u64`  (schema v1.1.0, default 1000)
//!       `field_2: u32`  (schema v1.2.0, default 42)
//!       `field_3: bool` (schema v1.3.0, default true)
//!   - `GATEWAY_SETTINGS_VERSION` is bumped to 1.3.0.
//!   - `schema_migration::migrate_gateway_settings_version` carries matching arms.
//!
//! Each test creates an account whose borsh content matches one of the four
//! layouts, injects it directly into ProgramTestContext (mirroring what a
//! live on-chain upgrade produces), then calls `migrate_gateway_settings` and
//! verifies both the final field values and version number.

#![cfg(feature = "migration-test")]

use anchor_lang::{
    prelude::*, AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_gar::error::GarError;
use ario_gar::state::*;

// =========================================
// CONSTANTS
// =========================================

/// On-wire size of GatewaySettings at schema v1.0.0 (no test fields).
const SETTINGS_V100_SIZE: usize = 8  // discriminator
    + 32  // authority
    + 32  // mint
    + 8   // min_operator_stake
    + 8   // min_delegate_stake
    + 8   // withdrawal_period
    + 8   // max_expedited_withdrawal_penalty
    + 8   // min_expedited_withdrawal_penalty
    + 8   // min_expedited_withdrawal_amount
    + 4   // max_delegates_per_gateway
    + 1   // migration_active
    + 32  // migration_authority
    + 32  // stake_token_account
    + 32  // protocol_token_account
    + 32  // arns_program_id
    + 8   // total_staked
    + 8   // total_delegated
    + 8   // total_withdrawn
    + 1   // bump
    + 3; // version (SchemaVersion)

/// On-wire size at v1.1.0 (+ field_1: u64 = +8).
const SETTINGS_V110_SIZE: usize = SETTINGS_V100_SIZE + 8;
/// On-wire size at v1.2.0 (+ field_2: u32 = +4).
const SETTINGS_V120_SIZE: usize = SETTINGS_V110_SIZE + 4;
/// On-wire size at v1.3.0 (+ field_3: bool = +1) = current SIZE.
const SETTINGS_V130_SIZE: usize = SETTINGS_V120_SIZE + 1;

// =========================================
// ASSERT SIZE CONSISTENCY
// =========================================

#[test]
fn test_size_constants_consistent_with_current() {
    assert_eq!(GatewaySettings::SIZE, SETTINGS_V130_SIZE);
    assert_eq!(SETTINGS_V110_SIZE - SETTINGS_V100_SIZE, 8); // u64
    assert_eq!(SETTINGS_V120_SIZE - SETTINGS_V110_SIZE, 4); // u32
    assert_eq!(SETTINGS_V130_SIZE - SETTINGS_V120_SIZE, 1); // bool
}

// =========================================
// OLD-SCHEMA LAYOUT STRUCTS
// =========================================

/// GatewaySettings layout at schema v1.0.0 — no test sentinel fields.
#[derive(AnchorSerialize)]
struct SettingsV100Layout {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub min_operator_stake: u64,
    pub min_delegate_stake: u64,
    pub withdrawal_period: i64,
    pub max_expedited_withdrawal_penalty: u64,
    pub min_expedited_withdrawal_penalty: u64,
    pub min_expedited_withdrawal_amount: u64,
    pub max_delegates_per_gateway: u32,
    pub migration_active: bool,
    pub migration_authority: Pubkey,
    pub stake_token_account: Pubkey,
    pub protocol_token_account: Pubkey,
    pub arns_program_id: Pubkey,
    pub total_staked: u64,
    pub total_delegated: u64,
    pub total_withdrawn: u64,
    pub bump: u8,
    pub version: [u8; 3],
}

/// GatewaySettings layout at schema v1.1.0 — adds field_1: u64.
#[derive(AnchorSerialize)]
struct SettingsV110Layout {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub min_operator_stake: u64,
    pub min_delegate_stake: u64,
    pub withdrawal_period: i64,
    pub max_expedited_withdrawal_penalty: u64,
    pub min_expedited_withdrawal_penalty: u64,
    pub min_expedited_withdrawal_amount: u64,
    pub max_delegates_per_gateway: u32,
    pub migration_active: bool,
    pub migration_authority: Pubkey,
    pub stake_token_account: Pubkey,
    pub protocol_token_account: Pubkey,
    pub arns_program_id: Pubkey,
    pub total_staked: u64,
    pub total_delegated: u64,
    pub total_withdrawn: u64,
    pub bump: u8,
    pub version: [u8; 3],
    pub field_1: u64,
}

/// GatewaySettings layout at schema v1.2.0 — adds field_2: u32.
#[derive(AnchorSerialize)]
struct SettingsV120Layout {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub min_operator_stake: u64,
    pub min_delegate_stake: u64,
    pub withdrawal_period: i64,
    pub max_expedited_withdrawal_penalty: u64,
    pub min_expedited_withdrawal_penalty: u64,
    pub min_expedited_withdrawal_amount: u64,
    pub max_delegates_per_gateway: u32,
    pub migration_active: bool,
    pub migration_authority: Pubkey,
    pub stake_token_account: Pubkey,
    pub protocol_token_account: Pubkey,
    pub arns_program_id: Pubkey,
    pub total_staked: u64,
    pub total_delegated: u64,
    pub total_withdrawn: u64,
    pub bump: u8,
    pub version: [u8; 3],
    pub field_1: u64,
    pub field_2: u32,
}

// =========================================
// HELPERS
// =========================================

fn anchor_processor(
    program_id: &Pubkey,
    accounts: &[solana_sdk::account_info::AccountInfo],
    data: &[u8],
) -> solana_sdk::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[solana_sdk::account_info::AccountInfo] = std::mem::transmute(accounts);
        ario_gar::entry(program_id, accounts, data)
    }
}

fn account_discriminator(name: &str) -> [u8; 8] {
    let input = format!("account:{name}");
    solana_sdk::hash::hash(input.as_bytes()).to_bytes()[..8]
        .try_into()
        .expect("hash slice to [u8;8]")
}

fn settings_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SETTINGS_SEED], &ario_gar::ID)
}

fn set_sol_account(ctx: &mut ProgramTestContext, key: &Pubkey, lamports: u64) {
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn make_v100_layout(bump: u8) -> SettingsV100Layout {
    SettingsV100Layout {
        authority: Pubkey::new_unique(),
        mint: Pubkey::new_unique(),
        min_operator_stake: 20_000_000_000,
        min_delegate_stake: 10_000_000,
        withdrawal_period: 30 * 86_400,
        max_expedited_withdrawal_penalty: 500_000,
        min_expedited_withdrawal_penalty: 100_000,
        min_expedited_withdrawal_amount: 1_000_000,
        max_delegates_per_gateway: 10_000,
        migration_active: false,
        migration_authority: Pubkey::default(),
        stake_token_account: Pubkey::new_unique(),
        protocol_token_account: Pubkey::new_unique(),
        arns_program_id: Pubkey::new_unique(),
        total_staked: 100_000_000_000,
        total_delegated: 50_000_000_000,
        total_withdrawn: 10_000_000_000,
        bump,
        version: [1, 0, 0],
    }
}

fn set_settings_v100(ctx: &mut ProgramTestContext, key: &Pubkey, bump: u8) {
    let layout = make_v100_layout(bump);
    let disc = account_discriminator("GatewaySettings");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(GatewaySettings::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(GatewaySettings::SIZE),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_settings_v110(ctx: &mut ProgramTestContext, key: &Pubkey, bump: u8, field_1_value: u64) {
    let base = make_v100_layout(bump);
    let layout = SettingsV110Layout {
        authority: base.authority,
        mint: base.mint,
        min_operator_stake: base.min_operator_stake,
        min_delegate_stake: base.min_delegate_stake,
        withdrawal_period: base.withdrawal_period,
        max_expedited_withdrawal_penalty: base.max_expedited_withdrawal_penalty,
        min_expedited_withdrawal_penalty: base.min_expedited_withdrawal_penalty,
        min_expedited_withdrawal_amount: base.min_expedited_withdrawal_amount,
        max_delegates_per_gateway: base.max_delegates_per_gateway,
        migration_active: base.migration_active,
        migration_authority: base.migration_authority,
        stake_token_account: base.stake_token_account,
        protocol_token_account: base.protocol_token_account,
        arns_program_id: base.arns_program_id,
        total_staked: base.total_staked,
        total_delegated: base.total_delegated,
        total_withdrawn: base.total_withdrawn,
        bump: base.bump,
        version: [1, 1, 0],
        field_1: field_1_value,
    };
    let disc = account_discriminator("GatewaySettings");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(GatewaySettings::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(GatewaySettings::SIZE),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_settings_v120(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    field_1_value: u64,
    field_2_value: u32,
) {
    let base = make_v100_layout(bump);
    let layout = SettingsV120Layout {
        authority: base.authority,
        mint: base.mint,
        min_operator_stake: base.min_operator_stake,
        min_delegate_stake: base.min_delegate_stake,
        withdrawal_period: base.withdrawal_period,
        max_expedited_withdrawal_penalty: base.max_expedited_withdrawal_penalty,
        min_expedited_withdrawal_penalty: base.min_expedited_withdrawal_penalty,
        min_expedited_withdrawal_amount: base.min_expedited_withdrawal_amount,
        max_delegates_per_gateway: base.max_delegates_per_gateway,
        migration_active: base.migration_active,
        migration_authority: base.migration_authority,
        stake_token_account: base.stake_token_account,
        protocol_token_account: base.protocol_token_account,
        arns_program_id: base.arns_program_id,
        total_staked: base.total_staked,
        total_delegated: base.total_delegated,
        total_withdrawn: base.total_withdrawn,
        bump: base.bump,
        version: [1, 2, 0],
        field_1: field_1_value,
        field_2: field_2_value,
    };
    let disc = account_discriminator("GatewaySettings");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(GatewaySettings::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(GatewaySettings::SIZE),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_settings_v130(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    field_1_value: u64,
    field_2_value: u32,
    field_3_value: bool,
) {
    let base = make_v100_layout(bump);
    let settings = GatewaySettings {
        authority: base.authority,
        mint: base.mint,
        min_operator_stake: base.min_operator_stake,
        min_delegate_stake: base.min_delegate_stake as u64,
        withdrawal_period: base.withdrawal_period,
        max_expedited_withdrawal_penalty: base.max_expedited_withdrawal_penalty,
        min_expedited_withdrawal_penalty: base.min_expedited_withdrawal_penalty,
        min_expedited_withdrawal_amount: base.min_expedited_withdrawal_amount,
        max_delegates_per_gateway: base.max_delegates_per_gateway,
        migration_active: base.migration_active,
        migration_authority: base.migration_authority,
        stake_token_account: base.stake_token_account,
        protocol_token_account: base.protocol_token_account,
        arns_program_id: base.arns_program_id,
        total_staked: base.total_staked,
        total_delegated: base.total_delegated,
        total_withdrawn: base.total_withdrawn,
        bump: base.bump,
        version: SchemaVersion::new(1, 3, 0),
        field_1: field_1_value,
        field_2: field_2_value,
        field_3: field_3_value,
    };
    let mut data = Vec::new();
    settings.try_serialize(&mut data).unwrap();
    data.resize(GatewaySettings::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(GatewaySettings::SIZE),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

async fn fetch_settings(ctx: &mut ProgramTestContext) -> GatewaySettings {
    let (settings_key, _) = settings_pda();
    let account = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .expect("GatewaySettings account not found");
    GatewaySettings::try_deserialize(&mut account.data.as_slice()).unwrap()
}

macro_rules! assert_anchor_error {
    ($result:expr, $error:expr) => {
        let expected_code = anchor_lang::error::ERROR_CODE_OFFSET + $error as u32;
        match $result {
            Err(solana_program_test::BanksClientError::TransactionError(
                solana_sdk::transaction::TransactionError::InstructionError(
                    _,
                    solana_sdk::instruction::InstructionError::Custom(code),
                ),
            )) => {
                assert_eq!(
                    code,
                    expected_code,
                    "Expected error {} (code {}), got code {}",
                    stringify!($error),
                    expected_code,
                    code
                );
            }
            other => panic!(
                "Expected custom error {} (code {}), got: {:?}",
                stringify!($error),
                expected_code,
                other
            ),
        }
    };
}

async fn send_migrate_gateway_settings(
    ctx: &mut ProgramTestContext,
    payer: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (settings_key, _) = settings_pda();

    let accounts = ario_gar::accounts::MigrateGatewaySettings {
        settings: settings_key,
        payer: payer.pubkey(),
        system_program: system_program::ID,
    };
    let ix = Instruction {
        program_id: ario_gar::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_gar::instruction::MigrateGatewaySettings {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

// =========================================
// TEST SETUP HELPER
// =========================================

async fn setup() -> (ProgramTestContext, Keypair, Pubkey, u8) {
    let payer = Keypair::new();

    let mut pt = ProgramTest::new("ario_gar", ario_gar::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let mut ctx = pt.start_with_context().await;
    set_sol_account(&mut ctx, &payer.pubkey(), 10_000_000_000);

    let (settings_key, settings_bump) = settings_pda();
    (ctx, payer, settings_key, settings_bump)
}

// =========================================
// v1.0.0 -> v1.3.0 (full migration)
// =========================================

#[tokio::test]
async fn test_migrate_v100_to_v130() {
    let (mut ctx, payer, settings_key, settings_bump) = setup().await;

    set_settings_v100(&mut ctx, &settings_key, settings_bump);

    let before = fetch_settings(&mut ctx).await;
    assert_eq!(before.version, SchemaVersion::new(1, 0, 0));
    assert_eq!(before.field_1, 0, "field_1 is zero-padded before migration");
    assert_eq!(before.field_2, 0, "field_2 is zero-padded before migration");
    assert!(!before.field_3, "field_3 is zero-padded before migration");

    send_migrate_gateway_settings(&mut ctx, &payer)
        .await
        .unwrap();

    let after = fetch_settings(&mut ctx).await;
    assert_eq!(after.version, GATEWAY_SETTINGS_VERSION);
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 1000, "1.0.0->1.1.0 arm sets field_1 = 1000");
    assert_eq!(after.field_2, 42, "1.1.0->1.2.0 arm sets field_2 = 42");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_gateway_settings(&mut ctx, &payer2).await;
    assert_anchor_error!(result, GarError::AlreadyLatestVersion);
}

// =========================================
// v1.1.0 -> v1.3.0 (skip first arm)
// =========================================

#[tokio::test]
async fn test_migrate_v110_to_v130() {
    let (mut ctx, payer, settings_key, settings_bump) = setup().await;

    set_settings_v110(&mut ctx, &settings_key, settings_bump, 500);

    let before = fetch_settings(&mut ctx).await;
    assert_eq!(before.version, SchemaVersion::new(1, 1, 0));
    assert_eq!(before.field_1, 500, "pre-migration field_1 should be 500");

    send_migrate_gateway_settings(&mut ctx, &payer)
        .await
        .unwrap();

    let after = fetch_settings(&mut ctx).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(
        after.field_1, 500,
        "field_1 must NOT be overwritten - arm 1.0.0->1.1.0 is skipped"
    );
    assert_eq!(after.field_2, 42, "1.1.0->1.2.0 arm sets field_2 = 42");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_gateway_settings(&mut ctx, &payer2).await;
    assert_anchor_error!(result, GarError::AlreadyLatestVersion);
}

// =========================================
// v1.2.0 -> v1.3.0 (only last arm)
// =========================================

#[tokio::test]
async fn test_migrate_v120_to_v130() {
    let (mut ctx, payer, settings_key, settings_bump) = setup().await;

    set_settings_v120(&mut ctx, &settings_key, settings_bump, 777, 99);

    let before = fetch_settings(&mut ctx).await;
    assert_eq!(before.version, SchemaVersion::new(1, 2, 0));
    assert_eq!(before.field_1, 777);
    assert_eq!(before.field_2, 99);
    assert!(!before.field_3, "field_3 is zero-padded before migration");

    send_migrate_gateway_settings(&mut ctx, &payer)
        .await
        .unwrap();

    let after = fetch_settings(&mut ctx).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 777, "field_1 preserved");
    assert_eq!(after.field_2, 99, "field_2 preserved");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_gateway_settings(&mut ctx, &payer2).await;
    assert_anchor_error!(result, GarError::AlreadyLatestVersion);
}

// =========================================
// v1.3.0 (already latest - no-op)
// =========================================

#[tokio::test]
async fn test_migrate_v130_already_latest() {
    let (mut ctx, payer, settings_key, settings_bump) = setup().await;

    set_settings_v130(&mut ctx, &settings_key, settings_bump, 1000, 42, true);

    let before = fetch_settings(&mut ctx).await;
    assert_eq!(before.version, SchemaVersion::new(1, 3, 0));

    let result = send_migrate_gateway_settings(&mut ctx, &payer).await;
    assert_anchor_error!(result, GarError::AlreadyLatestVersion);

    let after = fetch_settings(&mut ctx).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 1000);
    assert_eq!(after.field_2, 42);
    assert!(after.field_3);
}
