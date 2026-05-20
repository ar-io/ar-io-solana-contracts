//! Migration E2E tests — simulates four ArnsConfig schema versions.
//!
//! Run with:
//!   cargo test -p ario-arns --features migration-test --test migration_e2e
//!
//! This suite is compiled only when the `migration-test` feature is active.
//! Under that feature:
//!   - `ArnsConfig` gains three sentinel fields appended after `version`:
//!       `field_1: u64`  (schema v1.1.0, default 1000)
//!       `field_2: u32`  (schema v1.2.0, default 42)
//!       `field_3: bool` (schema v1.3.0, default true)
//!   - `ARNS_CONFIG_VERSION` is bumped to 1.3.0.
//!   - `schema_migration::migrate_arns_config_version` carries matching arms.
//!
//! Each test creates an account whose borsh content matches one of the four
//! layouts, injects it directly into ProgramTestContext (mirroring what a
//! live on-chain upgrade produces), then calls `migrate_arns_config` and
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

use ario_arns::error::ArnsError;
use ario_arns::state::*;

// =========================================
// CONSTANTS
// =========================================

/// On-wire size of ArnsConfig at schema v1.0.0 (no test fields).
const ARNS_CONFIG_V100_SIZE: usize = 8  // discriminator
    + 32  // authority
    + 32  // mint
    + 32  // treasury
    + 8   // grace_period_seconds
    + 8   // return_auction_duration_seconds
    + 1   // max_lease_length_years
    + 8   // total_names_registered
    + 8   // next_records_prune_timestamp
    + 8   // next_returned_names_prune_timestamp
    + 1   // migration_active
    + 32  // migration_authority
    + 1   // bump
    + 3; // version (SchemaVersion)

/// On-wire size at v1.1.0 (+ field_1: u64 = +8).
const ARNS_CONFIG_V110_SIZE: usize = ARNS_CONFIG_V100_SIZE + 8;
/// On-wire size at v1.2.0 (+ field_2: u32 = +4).
const ARNS_CONFIG_V120_SIZE: usize = ARNS_CONFIG_V110_SIZE + 4;
/// On-wire size at v1.3.0 (+ field_3: bool = +1) = current SIZE.
const ARNS_CONFIG_V130_SIZE: usize = ARNS_CONFIG_V120_SIZE + 1;

// =========================================
// ASSERT SIZE CONSISTENCY
// =========================================

#[test]
fn test_size_constants_consistent_with_current() {
    assert_eq!(ArnsConfig::SIZE, ARNS_CONFIG_V130_SIZE);
    assert_eq!(ARNS_CONFIG_V110_SIZE - ARNS_CONFIG_V100_SIZE, 8); // u64
    assert_eq!(ARNS_CONFIG_V120_SIZE - ARNS_CONFIG_V110_SIZE, 4); // u32
    assert_eq!(ARNS_CONFIG_V130_SIZE - ARNS_CONFIG_V120_SIZE, 1); // bool
}

// =========================================
// OLD-SCHEMA LAYOUT STRUCTS
// =========================================

/// ArnsConfig layout at schema v1.0.0 — no test sentinel fields.
#[derive(AnchorSerialize)]
struct ArnsConfigV100Layout {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub treasury: Pubkey,
    pub grace_period_seconds: i64,
    pub return_auction_duration_seconds: i64,
    pub max_lease_length_years: u8,
    pub total_names_registered: u64,
    pub next_records_prune_timestamp: i64,
    pub next_returned_names_prune_timestamp: i64,
    pub migration_active: bool,
    pub migration_authority: Pubkey,
    pub bump: u8,
    pub version: [u8; 3],
}

/// ArnsConfig layout at schema v1.1.0 — adds field_1: u64.
#[derive(AnchorSerialize)]
struct ArnsConfigV110Layout {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub treasury: Pubkey,
    pub grace_period_seconds: i64,
    pub return_auction_duration_seconds: i64,
    pub max_lease_length_years: u8,
    pub total_names_registered: u64,
    pub next_records_prune_timestamp: i64,
    pub next_returned_names_prune_timestamp: i64,
    pub migration_active: bool,
    pub migration_authority: Pubkey,
    pub bump: u8,
    pub version: [u8; 3],
    pub field_1: u64,
}

/// ArnsConfig layout at schema v1.2.0 — adds field_2: u32.
#[derive(AnchorSerialize)]
struct ArnsConfigV120Layout {
    pub authority: Pubkey,
    pub mint: Pubkey,
    pub treasury: Pubkey,
    pub grace_period_seconds: i64,
    pub return_auction_duration_seconds: i64,
    pub max_lease_length_years: u8,
    pub total_names_registered: u64,
    pub next_records_prune_timestamp: i64,
    pub next_returned_names_prune_timestamp: i64,
    pub migration_active: bool,
    pub migration_authority: Pubkey,
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
        ario_arns::entry(program_id, accounts, data)
    }
}

fn account_discriminator(name: &str) -> [u8; 8] {
    let input = format!("account:{name}");
    solana_sdk::hash::hash(input.as_bytes()).to_bytes()[..8]
        .try_into()
        .expect("hash slice to [u8;8]")
}

fn config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ARNS_CONFIG_SEED], &ario_arns::ID)
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

fn make_v100_layout(bump: u8, authority: &Pubkey) -> ArnsConfigV100Layout {
    ArnsConfigV100Layout {
        authority: *authority,
        mint: Pubkey::new_unique(),
        treasury: Pubkey::new_unique(),
        grace_period_seconds: GRACE_PERIOD_SECONDS,
        return_auction_duration_seconds: RETURNED_NAME_DURATION_SECONDS,
        max_lease_length_years: MAX_LEASE_LENGTH_YEARS,
        total_names_registered: 0,
        next_records_prune_timestamp: 0,
        next_returned_names_prune_timestamp: 0,
        migration_active: false,
        migration_authority: Pubkey::default(),
        bump,
        version: [1, 0, 0],
    }
}

fn set_config_v100(ctx: &mut ProgramTestContext, key: &Pubkey, bump: u8, authority: &Pubkey) {
    let layout = make_v100_layout(bump, authority);
    let disc = account_discriminator("ArnsConfig");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(ArnsConfig::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(ArnsConfig::SIZE),
            data,
            owner: ario_arns::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_config_v110(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    authority: &Pubkey,
    field_1_value: u64,
) {
    let base = make_v100_layout(bump, authority);
    let layout = ArnsConfigV110Layout {
        authority: base.authority,
        mint: base.mint,
        treasury: base.treasury,
        grace_period_seconds: base.grace_period_seconds,
        return_auction_duration_seconds: base.return_auction_duration_seconds,
        max_lease_length_years: base.max_lease_length_years,
        total_names_registered: base.total_names_registered,
        next_records_prune_timestamp: base.next_records_prune_timestamp,
        next_returned_names_prune_timestamp: base.next_returned_names_prune_timestamp,
        migration_active: base.migration_active,
        migration_authority: base.migration_authority,
        bump: base.bump,
        version: [1, 1, 0],
        field_1: field_1_value,
    };
    let disc = account_discriminator("ArnsConfig");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(ArnsConfig::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(ArnsConfig::SIZE),
            data,
            owner: ario_arns::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_config_v120(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    authority: &Pubkey,
    field_1_value: u64,
    field_2_value: u32,
) {
    let base = make_v100_layout(bump, authority);
    let layout = ArnsConfigV120Layout {
        authority: base.authority,
        mint: base.mint,
        treasury: base.treasury,
        grace_period_seconds: base.grace_period_seconds,
        return_auction_duration_seconds: base.return_auction_duration_seconds,
        max_lease_length_years: base.max_lease_length_years,
        total_names_registered: base.total_names_registered,
        next_records_prune_timestamp: base.next_records_prune_timestamp,
        next_returned_names_prune_timestamp: base.next_returned_names_prune_timestamp,
        migration_active: base.migration_active,
        migration_authority: base.migration_authority,
        bump: base.bump,
        version: [1, 2, 0],
        field_1: field_1_value,
        field_2: field_2_value,
    };
    let disc = account_discriminator("ArnsConfig");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(ArnsConfig::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(ArnsConfig::SIZE),
            data,
            owner: ario_arns::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_config_v130(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    authority: &Pubkey,
    field_1_value: u64,
    field_2_value: u32,
    field_3_value: bool,
) {
    let config = ArnsConfig {
        authority: *authority,
        mint: Pubkey::new_unique(),
        treasury: Pubkey::new_unique(),
        grace_period_seconds: GRACE_PERIOD_SECONDS,
        return_auction_duration_seconds: RETURNED_NAME_DURATION_SECONDS,
        max_lease_length_years: MAX_LEASE_LENGTH_YEARS,
        total_names_registered: 0,
        next_records_prune_timestamp: 0,
        next_returned_names_prune_timestamp: 0,
        migration_active: false,
        migration_authority: Pubkey::default(),
        bump,
        version: SchemaVersion::new(1, 3, 0),
        field_1: field_1_value,
        field_2: field_2_value,
        field_3: field_3_value,
    };
    let mut data = Vec::new();
    config.try_serialize(&mut data).unwrap();
    data.resize(ArnsConfig::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(ArnsConfig::SIZE),
            data,
            owner: ario_arns::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

async fn fetch_config(ctx: &mut ProgramTestContext) -> ArnsConfig {
    let (config_key, _) = config_pda();
    let account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .expect("ArnsConfig account not found");
    ArnsConfig::try_deserialize(&mut account.data.as_slice()).unwrap()
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

async fn send_migrate_arns_config(
    ctx: &mut ProgramTestContext,
    payer: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda();

    let accounts = ario_arns::accounts::MigrateArnsConfig {
        config: config_key,
        payer: payer.pubkey(),
        system_program: system_program::ID,
    };
    let ix = Instruction {
        program_id: ario_arns::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_arns::instruction::MigrateArnsConfig {}.data(),
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

    let mut pt = ProgramTest::new("ario_arns", ario_arns::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let mut ctx = pt.start_with_context().await;
    set_sol_account(&mut ctx, &payer.pubkey(), 10_000_000_000);

    let (config_key, config_bump) = config_pda();
    (ctx, payer, config_key, config_bump)
}

// =========================================
// v1.0.0 -> v1.3.0 (full migration)
// =========================================

#[tokio::test]
async fn test_migrate_schema_1_to_schema_4() {
    let (mut ctx, payer, config_key, config_bump) = setup().await;

    set_config_v100(&mut ctx, &config_key, config_bump, &payer.pubkey());

    let before = fetch_config(&mut ctx).await;
    assert_eq!(before.version, SchemaVersion::new(1, 0, 0));
    assert_eq!(before.field_1, 0, "field_1 is zero-padded before migration");
    assert_eq!(before.field_2, 0, "field_2 is zero-padded before migration");
    assert!(!before.field_3, "field_3 is zero-padded before migration");

    send_migrate_arns_config(&mut ctx, &payer).await.unwrap();

    let after = fetch_config(&mut ctx).await;
    assert_eq!(after.version, ARNS_CONFIG_VERSION);
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 1000, "1.0.0->1.1.0 arm sets field_1 = 1000");
    assert_eq!(after.field_2, 42, "1.1.0->1.2.0 arm sets field_2 = 42");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_arns_config(&mut ctx, &payer2).await;
    assert_anchor_error!(result, ArnsError::AlreadyLatestVersion);
}

// =========================================
// v1.1.0 -> v1.3.0 (skip first arm)
// =========================================

#[tokio::test]
async fn test_migrate_schema_2_to_schema_4() {
    let (mut ctx, payer, config_key, config_bump) = setup().await;

    set_config_v110(&mut ctx, &config_key, config_bump, &payer.pubkey(), 500);

    let before = fetch_config(&mut ctx).await;
    assert_eq!(before.version, SchemaVersion::new(1, 1, 0));
    assert_eq!(before.field_1, 500, "pre-migration field_1 should be 500");

    send_migrate_arns_config(&mut ctx, &payer).await.unwrap();

    let after = fetch_config(&mut ctx).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(
        after.field_1, 500,
        "field_1 must NOT be overwritten - arm 1.0.0->1.1.0 is skipped"
    );
    assert_eq!(after.field_2, 42, "1.1.0->1.2.0 arm sets field_2 = 42");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_arns_config(&mut ctx, &payer2).await;
    assert_anchor_error!(result, ArnsError::AlreadyLatestVersion);
}

// =========================================
// v1.2.0 -> v1.3.0 (only last arm)
// =========================================

#[tokio::test]
async fn test_migrate_schema_3_to_schema_4() {
    let (mut ctx, payer, config_key, config_bump) = setup().await;

    set_config_v120(&mut ctx, &config_key, config_bump, &payer.pubkey(), 777, 99);

    let before = fetch_config(&mut ctx).await;
    assert_eq!(before.version, SchemaVersion::new(1, 2, 0));
    assert_eq!(before.field_1, 777);
    assert_eq!(before.field_2, 99);
    assert!(!before.field_3, "field_3 is zero-padded before migration");

    send_migrate_arns_config(&mut ctx, &payer).await.unwrap();

    let after = fetch_config(&mut ctx).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 777, "field_1 preserved");
    assert_eq!(after.field_2, 99, "field_2 preserved");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_arns_config(&mut ctx, &payer2).await;
    assert_anchor_error!(result, ArnsError::AlreadyLatestVersion);
}

// =========================================
// v1.3.0 (already latest - no-op)
// =========================================

#[tokio::test]
async fn test_migrate_schema_4_already_latest() {
    let (mut ctx, payer, config_key, config_bump) = setup().await;

    set_config_v130(
        &mut ctx,
        &config_key,
        config_bump,
        &payer.pubkey(),
        1000,
        42,
        true,
    );

    let before = fetch_config(&mut ctx).await;
    assert_eq!(before.version, SchemaVersion::new(1, 3, 0));

    let result = send_migrate_arns_config(&mut ctx, &payer).await;
    assert_anchor_error!(result, ArnsError::AlreadyLatestVersion);

    let after = fetch_config(&mut ctx).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 1000);
    assert_eq!(after.field_2, 42);
    assert!(after.field_3);
}
