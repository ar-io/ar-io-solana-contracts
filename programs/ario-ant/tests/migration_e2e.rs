//! Migration E2E tests — simulates four AntConfig schema versions.
//!
//! Run with:
//!   cargo test -p ario-ant --features migration-test --test migration_e2e
//!
//! This suite is compiled only when the `migration-test` feature is active.
//! Under that feature:
//!   - `AntConfig` gains three sentinel fields appended after `version`:
//!       `field_1: u64`  (schema v1.1.0, default 1000)
//!       `field_2: u32`  (schema v1.2.0, default 42)
//!       `field_3: bool` (schema v1.3.0, default true)
//!   - `ANT_CONFIG_VERSION` is bumped to 1.3.0.
//!   - `schema_migration::migrate_config_version` carries matching arms.
//!
//! Each test creates an account whose borsh content matches one of the four
//! layouts, injects it directly into ProgramTestContext (mirroring what a
//! live on-chain upgrade produces), then calls `migrate_ant` and verifies
//! both the final field values and version number.
//!
//! Because migrate_ant uses a native processor these tests do NOT require
//! `BPF_OUT_DIR` or `mpl_core.so` — run with plain `cargo test`.

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

use ario_ant::error::AntError;
use ario_ant::state::*;

// =========================================
// CONSTANTS
// =========================================

const MPL_CORE_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

/// On-wire size of AntConfig at schema v1.0.0 (no test fields).
const ANT_CONFIG_V100_SIZE: usize = 760;
/// On-wire size of AntConfig at schema v1.1.0 (+ field_1: u64 = +8).
const ANT_CONFIG_V110_SIZE: usize = 768;
/// On-wire size of AntConfig at schema v1.2.0 (+ field_2: u32 = +4).
const ANT_CONFIG_V120_SIZE: usize = 772;
/// On-wire size of AntConfig at schema v1.3.0 (+ field_3: bool = +1) = current SIZE.
const ANT_CONFIG_V130_SIZE: usize = 773;

// =========================================
// ASSERT SIZE CONSISTENCY
// =========================================

#[test]
fn test_size_constants_consistent_with_current() {
    // V1.3.0 is the latest; AntConfig::SIZE must match.
    assert_eq!(AntConfig::SIZE, ANT_CONFIG_V130_SIZE);
    // Deltas between schema versions must be exactly the field sizes.
    assert_eq!(ANT_CONFIG_V110_SIZE - ANT_CONFIG_V100_SIZE, 8); // u64
    assert_eq!(ANT_CONFIG_V120_SIZE - ANT_CONFIG_V110_SIZE, 4); // u32
    assert_eq!(ANT_CONFIG_V130_SIZE - ANT_CONFIG_V120_SIZE, 1); // bool
}

// =========================================
// OLD-SCHEMA LAYOUT STRUCTS
// =========================================
//
// These are NOT Anchor `#[account]` types — they are pure borsh structs that
// match the on-wire layout of each historical schema version so that we can
// build correct raw account bytes in tests.  The Anchor discriminator
// (8 bytes) is prepended separately.

/// AntConfig layout at schema v1.0.0 — no test sentinel fields.
#[derive(AnchorSerialize)]
struct AntConfigV100Layout {
    pub mint: Pubkey,
    pub name: String,
    pub ticker: String,
    pub logo: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub last_known_owner: Pubkey,
    pub bump: u8,
    /// Serialised as three consecutive u8 bytes: [major, minor, patch].
    /// Matches the in-memory layout of `SchemaVersion`.
    pub version: [u8; 3],
}

/// AntConfig layout at schema v1.1.0 — adds `field_1: u64` after version.
#[derive(AnchorSerialize)]
struct AntConfigV110Layout {
    pub mint: Pubkey,
    pub name: String,
    pub ticker: String,
    pub logo: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub last_known_owner: Pubkey,
    pub bump: u8,
    pub version: [u8; 3],
    pub field_1: u64,
}

/// AntConfig layout at schema v1.2.0 — adds `field_2: u32` after field_1.
#[derive(AnchorSerialize)]
struct AntConfigV120Layout {
    pub mint: Pubkey,
    pub name: String,
    pub ticker: String,
    pub logo: String,
    pub description: String,
    pub keywords: Vec<String>,
    pub last_known_owner: Pubkey,
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
        ario_ant::entry(program_id, accounts, data)
    }
}

fn test_arweave_id() -> String {
    "a".repeat(43)
}

/// Compute the Anchor account discriminator for a named account type.
fn account_discriminator(name: &str) -> [u8; 8] {
    let input = format!("account:{name}");
    solana_sdk::hash::hash(input.as_bytes()).to_bytes()[..8]
        .try_into()
        .expect("hash slice to [u8;8]")
}

fn config_pda(asset: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ANT_CONFIG_SEED, asset.as_ref()], &ario_ant::ID)
}

fn controllers_pda(asset: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ANT_CONTROLLERS_SEED, asset.as_ref()], &ario_ant::ID)
}

/// Build a fake Metaplex Core asset data blob (33 bytes: key + owner pubkey).
fn fake_asset_data(owner: &Pubkey) -> Vec<u8> {
    let mut d = vec![0u8; 33];
    d[0] = 1; // AssetV1 key
    d[1..33].copy_from_slice(owner.as_ref());
    d
}

/// Inject a funded SOL account.
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

/// Inject a fake MPL Core asset account.
fn set_asset_account(ctx: &mut ProgramTestContext, key: &Pubkey, owner: &Pubkey) {
    let data = fake_asset_data(owner);
    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(data.len()),
            data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

/// Inject an AntControllers PDA at v1.0.0 (no migration needed).
fn set_controllers_account(ctx: &mut ProgramTestContext, key: &Pubkey, mint: &Pubkey, bump: u8) {
    let ctrl = AntControllers {
        mint: *mint,
        controllers: vec![],
        bump,
        version: SchemaVersion::new(1, 0, 0),
    };
    let mut data = Vec::new();
    ctrl.try_serialize(&mut data).unwrap();
    data.resize(AntControllers::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(AntControllers::SIZE),
            data,
            owner: ario_ant::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

/// Inject an AntConfig PDA whose raw bytes were serialised from a V100 layout
/// struct (no sentinel fields).  The account is allocated at `ANT_CONFIG_V100_SIZE`
/// bytes; `migrate_ant`'s realloc will grow it to `AntConfig::SIZE`.
fn set_config_v100(ctx: &mut ProgramTestContext, key: &Pubkey, mint: &Pubkey, bump: u8) {
    let layout = AntConfigV100Layout {
        mint: *mint,
        name: "Migration ANT".to_string(),
        ticker: "MIG".to_string(),
        logo: test_arweave_id(),
        description: String::new(),
        keywords: vec![],
        last_known_owner: Pubkey::default(),
        bump,
        version: [1, 0, 0],
    };
    let disc = account_discriminator("AntConfig");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(ANT_CONFIG_V100_SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(AntConfig::SIZE), // fund for grown size
            data,
            owner: ario_ant::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

/// Inject an AntConfig PDA at the V110 layout (field_1 pre-populated with
/// `field_1_value` so we can verify the migration arm does NOT overwrite it).
fn set_config_v110(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    mint: &Pubkey,
    bump: u8,
    field_1_value: u64,
) {
    let layout = AntConfigV110Layout {
        mint: *mint,
        name: "Migration ANT".to_string(),
        ticker: "MIG".to_string(),
        logo: test_arweave_id(),
        description: String::new(),
        keywords: vec![],
        last_known_owner: Pubkey::default(),
        bump,
        version: [1, 1, 0],
        field_1: field_1_value,
    };
    let disc = account_discriminator("AntConfig");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(ANT_CONFIG_V110_SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(AntConfig::SIZE),
            data,
            owner: ario_ant::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

/// Inject an AntConfig PDA at the V120 layout.
fn set_config_v120(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    mint: &Pubkey,
    bump: u8,
    field_1_value: u64,
    field_2_value: u32,
) {
    let layout = AntConfigV120Layout {
        mint: *mint,
        name: "Migration ANT".to_string(),
        ticker: "MIG".to_string(),
        logo: test_arweave_id(),
        description: String::new(),
        keywords: vec![],
        last_known_owner: Pubkey::default(),
        bump,
        version: [1, 2, 0],
        field_1: field_1_value,
        field_2: field_2_value,
    };
    let disc = account_discriminator("AntConfig");
    let mut data = disc.to_vec();
    layout.serialize(&mut data).unwrap();
    data.resize(ANT_CONFIG_V120_SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(AntConfig::SIZE),
            data,
            owner: ario_ant::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

/// Inject an AntConfig PDA at the full V130 layout (current `AntConfig`).
fn set_config_v130(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    mint: &Pubkey,
    bump: u8,
    field_1_value: u64,
    field_2_value: u32,
    field_3_value: bool,
) {
    let config = AntConfig {
        mint: *mint,
        name: "Migration ANT".to_string(),
        ticker: "MIG".to_string(),
        logo: test_arweave_id(),
        description: String::new(),
        keywords: vec![],
        last_known_owner: Pubkey::default(),
        bump,
        version: SchemaVersion::new(1, 3, 0),
        field_1: field_1_value,
        field_2: field_2_value,
        field_3: field_3_value,
    };
    let mut data = Vec::new();
    config.try_serialize(&mut data).unwrap();
    data.resize(AntConfig::SIZE, 0);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(AntConfig::SIZE),
            data,
            owner: ario_ant::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

/// Read and deserialise an AntConfig PDA from the bank.
async fn fetch_config(ctx: &mut ProgramTestContext, asset: &Pubkey) -> AntConfig {
    let (config_key, _) = config_pda(asset);
    let account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .expect("AntConfig account not found");
    AntConfig::try_deserialize(&mut account.data.as_slice()).unwrap()
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

/// Build and send a `migrate_ant` instruction, returning the raw result.
async fn send_migrate_ant(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    payer: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let accounts = ario_ant::accounts::AntMigration {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        payer: payer.pubkey(),
        system_program: system_program::ID,
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::MigrateAnt {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

// =========================================
// TEST SETUP HELPER
// =========================================

/// Start a ProgramTestContext with a funded payer, a fake asset, and the
/// derived config + controllers PDA keys pre-computed.
///
/// Returns `(ctx, payer, asset_pubkey, config_pda, controllers_pda)`.
async fn setup() -> (ProgramTestContext, Keypair, Pubkey, Pubkey, Pubkey) {
    let payer = Keypair::new();
    let asset = Keypair::new();

    let mut pt = ProgramTest::new("ario_ant", ario_ant::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let asset_data = fake_asset_data(&payer.pubkey());
    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        asset.pubkey(),
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(asset_data.len()),
            data: asset_data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let mut ctx = pt.start_with_context().await;

    // Fund the payer
    set_sol_account(&mut ctx, &payer.pubkey(), 10_000_000_000);

    let (config_key, _) = config_pda(&asset.pubkey());
    let (controllers_key, _) = controllers_pda(&asset.pubkey());

    (ctx, payer, asset.pubkey(), config_key, controllers_key)
}

// =========================================
// SCHEMA 1 → SCHEMA 4  (v1.0.0 → v1.3.0)
// =========================================
//
// Simulate deploying an ANT at schema v1.0.0, then upgrading the program
// directly to v1.3.0.  A single `migrate_ant` call must traverse all three
// intermediate steps and populate every sentinel field with its defined
// default.

#[tokio::test]
async fn test_migrate_schema_1_to_schema_4() {
    let (mut ctx, payer, asset, config_key, controllers_key) = setup().await;

    let (_, config_bump) = config_pda(&asset);
    let (_, ctrl_bump) = controllers_pda(&asset);

    // Inject a v1.0.0 AntConfig (no sentinel fields in the stored bytes).
    set_config_v100(&mut ctx, &config_key, &asset, config_bump);
    // Inject AntControllers at current version (no migration needed for it).
    set_controllers_account(&mut ctx, &controllers_key, &asset, ctrl_bump);

    // Verify starting version.
    let before = fetch_config(&mut ctx, &asset).await;
    assert_eq!(before.version, SchemaVersion::new(1, 0, 0));
    assert_eq!(before.field_1, 0, "field_1 is zero-padded before migration");
    assert_eq!(before.field_2, 0, "field_2 is zero-padded before migration");
    assert!(!before.field_3, "field_3 is zero-padded before migration");

    // Run the migration.
    send_migrate_ant(&mut ctx, &asset, &payer).await.unwrap();

    // Verify all three steps were applied.
    let after = fetch_config(&mut ctx, &asset).await;
    assert_eq!(
        after.version, ANT_CONFIG_VERSION,
        "should reach latest version"
    );
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 1000, "1.0.0→1.1.0 arm sets field_1 = 1000");
    assert_eq!(after.field_2, 42, "1.1.0→1.2.0 arm sets field_2 = 42");
    assert!(after.field_3, "1.2.0→1.3.0 arm sets field_3 = true");

    // Existing fields should be intact.
    assert_eq!(after.name, "Migration ANT");
    assert_eq!(after.ticker, "MIG");
    assert_eq!(after.mint, asset);

    // A second migrate_ant must return AlreadyLatestVersion.
    // Use a fresh payer so the tx signature differs (avoids bank dedup).
    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_ant(&mut ctx, &asset, &payer2).await;
    assert_anchor_error!(result, AntError::AlreadyLatestVersion);
}

// =========================================
// SCHEMA 2 → SCHEMA 4  (v1.1.0 → v1.3.0)
// =========================================
//
// An ANT that was already upgraded to v1.1.0 (field_1 present, set to a
// non-default value) gets migrated to v1.3.0.  field_1 must be PRESERVED —
// the 1.0.0→1.1.0 arm never fires for this account.

#[tokio::test]
async fn test_migrate_schema_2_to_schema_4() {
    let (mut ctx, payer, asset, config_key, controllers_key) = setup().await;

    let (_, config_bump) = config_pda(&asset);
    let (_, ctrl_bump) = controllers_pda(&asset);

    // field_1 stored as 500 in the old account — must survive migration.
    set_config_v110(&mut ctx, &config_key, &asset, config_bump, 500);
    set_controllers_account(&mut ctx, &controllers_key, &asset, ctrl_bump);

    let before = fetch_config(&mut ctx, &asset).await;
    assert_eq!(before.version, SchemaVersion::new(1, 1, 0));
    assert_eq!(before.field_1, 500, "pre-migration field_1 should be 500");

    send_migrate_ant(&mut ctx, &asset, &payer).await.unwrap();

    let after = fetch_config(&mut ctx, &asset).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(
        after.field_1, 500,
        "field_1 must NOT be overwritten — arm 1.0.0→1.1.0 is skipped"
    );
    assert_eq!(after.field_2, 42, "1.1.0→1.2.0 arm sets field_2 = 42");
    assert!(after.field_3, "1.2.0→1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_ant(&mut ctx, &asset, &payer2).await;
    assert_anchor_error!(result, AntError::AlreadyLatestVersion);
}

// =========================================
// SCHEMA 3 → SCHEMA 4  (v1.2.0 → v1.3.0)
// =========================================
//
// An ANT at v1.2.0 (field_1 and field_2 both present with non-default
// values) needs only the final step.  Only field_3 changes.

#[tokio::test]
async fn test_migrate_schema_3_to_schema_4() {
    let (mut ctx, payer, asset, config_key, controllers_key) = setup().await;

    let (_, config_bump) = config_pda(&asset);
    let (_, ctrl_bump) = controllers_pda(&asset);

    // Distinct non-default values to verify neither is altered.
    set_config_v120(&mut ctx, &config_key, &asset, config_bump, 777, 99);
    set_controllers_account(&mut ctx, &controllers_key, &asset, ctrl_bump);

    let before = fetch_config(&mut ctx, &asset).await;
    assert_eq!(before.version, SchemaVersion::new(1, 2, 0));
    assert_eq!(before.field_1, 777);
    assert_eq!(before.field_2, 99);
    assert!(!before.field_3, "field_3 is zero-padded before migration");

    send_migrate_ant(&mut ctx, &asset, &payer).await.unwrap();

    let after = fetch_config(&mut ctx, &asset).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 777, "field_1 preserved");
    assert_eq!(after.field_2, 99, "field_2 preserved");
    assert!(after.field_3, "1.2.0→1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_ant(&mut ctx, &asset, &payer2).await;
    assert_anchor_error!(result, AntError::AlreadyLatestVersion);
}

// =========================================
// SCHEMA 4 (already latest — no-op)
// =========================================
//
// An ANT already at v1.3.0 must return AlreadyLatestVersion immediately
// without mutating any field.

#[tokio::test]
async fn test_migrate_schema_4_already_latest() {
    let (mut ctx, payer, asset, config_key, controllers_key) = setup().await;

    let (_, config_bump) = config_pda(&asset);
    let (_, ctrl_bump) = controllers_pda(&asset);

    // Inject a fully-migrated v1.3.0 account.
    set_config_v130(&mut ctx, &config_key, &asset, config_bump, 1000, 42, true);
    set_controllers_account(&mut ctx, &controllers_key, &asset, ctrl_bump);

    let before = fetch_config(&mut ctx, &asset).await;
    assert_eq!(before.version, SchemaVersion::new(1, 3, 0));

    // migrate_ant should short-circuit with AlreadyLatestVersion.
    let result = send_migrate_ant(&mut ctx, &asset, &payer).await;
    assert_anchor_error!(result, AntError::AlreadyLatestVersion);

    // Confirm no fields were touched.
    let after = fetch_config(&mut ctx, &asset).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 1000);
    assert_eq!(after.field_2, 42);
    assert!(after.field_3);
}
