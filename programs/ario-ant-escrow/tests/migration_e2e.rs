//! Migration E2E tests — simulates four EscrowAnt schema versions.
//!
//! Run with:
//!   cargo test -p ario-ant-escrow --features migration-test --test migration_e2e
//!
//! This suite is compiled only when the `migration-test` feature is active.
//! Under that feature:
//!   - `EscrowAnt` gains three sentinel fields carved from `_reserved`:
//!       `field_1: u64`  (schema v1.1.0, default 1000)
//!       `field_2: u32`  (schema v1.2.0, default 42)
//!       `field_3: bool` (schema v1.3.0, default true)
//!   - `_reserved` shrinks from [u8; 30] to [u8; 17] (30 - 8 - 4 - 1 = 17).
//!   - `ESCROW_ANT_VERSION` is bumped to 1.3.0.
//!   - `schema_migration::migrate_escrow_ant_version` carries matching arms.
//!
//! Since the test fields are carved from reserved bytes, the total account
//! SIZE stays at 661 — no realloc is needed. Each test creates an account
//! whose borsh content matches a historical layout, injects it directly into
//! ProgramTestContext, then calls `migrate_escrow_ant` and verifies both the
//! final field values and version number.
//!
//! These tests use a native processor and do NOT require `BPF_OUT_DIR`.

#![cfg(feature = "migration-test")]

use anchor_lang::{prelude::*, AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use ario_ant_escrow::error::EscrowError;
use ario_ant_escrow::state::*;

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
        ario_ant_escrow::entry(program_id, accounts, data)
    }
}

fn account_discriminator(name: &str) -> [u8; 8] {
    let input = format!("account:{name}");
    solana_sdk::hash::hash(input.as_bytes()).to_bytes()[..8]
        .try_into()
        .expect("hash slice to [u8;8]")
}

fn escrow_ant_pda(ant_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ESCROW_ANT_SEED, ant_mint.as_ref()], &ario_ant_escrow::ID)
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

// =========================================
// OLD-SCHEMA LAYOUT BUILDERS
// =========================================
//
// EscrowAnt has a fixed-size layout (no Vecs/Strings). All versions
// occupy exactly EscrowAnt::SIZE = 661 bytes. The test fields occupy
// the same byte range as the last 13 bytes of the original 30-byte
// `_reserved` field. Building raw bytes manually avoids depending on
// layout structs that might drift.

/// Shared field values for all test layouts.
struct EscrowAntFields {
    version: [u8; 3],
    bump: u8,
    depositor: Pubkey,
    ant_mint: Pubkey,
    recipient_protocol: u8,
    recipient_pubkey_len: u16,
    recipient_pubkey: [u8; RECIPIENT_PUBKEY_MAX_LEN],
    nonce: [u8; 32],
    deposit_slot: u64,
}

fn default_fields(bump: u8, depositor: &Pubkey, ant_mint: &Pubkey) -> EscrowAntFields {
    let nonce = derive_initial_nonce(42, ant_mint, depositor);
    EscrowAntFields {
        version: [1, 0, 0],
        bump,
        depositor: *depositor,
        ant_mint: *ant_mint,
        recipient_protocol: PROTOCOL_ARWEAVE,
        recipient_pubkey_len: ARWEAVE_PUBKEY_LEN as u16,
        recipient_pubkey: [0xAA; RECIPIENT_PUBKEY_MAX_LEN],
        nonce,
        deposit_slot: 42,
    }
}

/// Serialize the common EscrowAnt fields (everything before _reserved)
/// and return the byte vector including the discriminator prefix.
fn serialize_common(fields: &EscrowAntFields) -> Vec<u8> {
    let disc = account_discriminator("EscrowAnt");
    let mut data = disc.to_vec();
    data.extend_from_slice(&fields.version);
    data.push(fields.bump);
    data.extend_from_slice(fields.depositor.as_ref());
    data.extend_from_slice(fields.ant_mint.as_ref());
    data.push(fields.recipient_protocol);
    data.extend_from_slice(&fields.recipient_pubkey_len.to_le_bytes());
    data.extend_from_slice(&fields.recipient_pubkey);
    data.extend_from_slice(&fields.nonce);
    data.extend_from_slice(&fields.deposit_slot.to_le_bytes());
    data
}

fn set_escrow_v100(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    depositor: &Pubkey,
    ant_mint: &Pubkey,
) {
    let fields = default_fields(bump, depositor, ant_mint);
    let mut data = serialize_common(&fields);
    // 30 bytes of _reserved, all zeros
    data.extend_from_slice(&[0u8; 30]);
    assert_eq!(data.len(), EscrowAnt::SIZE);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(EscrowAnt::SIZE),
            data,
            owner: ario_ant_escrow::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_escrow_v110(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    depositor: &Pubkey,
    ant_mint: &Pubkey,
    field_1_value: u64,
) {
    let mut fields = default_fields(bump, depositor, ant_mint);
    fields.version = [1, 1, 0];
    let mut data = serialize_common(&fields);
    // 17 bytes _reserved (zeros) + field_1 + 4 bytes zero (field_2) + 1 byte zero (field_3)
    data.extend_from_slice(&[0u8; 17]);
    data.extend_from_slice(&field_1_value.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.push(0u8);
    assert_eq!(data.len(), EscrowAnt::SIZE);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(EscrowAnt::SIZE),
            data,
            owner: ario_ant_escrow::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_escrow_v120(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    depositor: &Pubkey,
    ant_mint: &Pubkey,
    field_1_value: u64,
    field_2_value: u32,
) {
    let mut fields = default_fields(bump, depositor, ant_mint);
    fields.version = [1, 2, 0];
    let mut data = serialize_common(&fields);
    data.extend_from_slice(&[0u8; 17]);
    data.extend_from_slice(&field_1_value.to_le_bytes());
    data.extend_from_slice(&field_2_value.to_le_bytes());
    data.push(0u8);
    assert_eq!(data.len(), EscrowAnt::SIZE);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(EscrowAnt::SIZE),
            data,
            owner: ario_ant_escrow::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

fn set_escrow_v130(
    ctx: &mut ProgramTestContext,
    key: &Pubkey,
    bump: u8,
    depositor: &Pubkey,
    ant_mint: &Pubkey,
    field_1_value: u64,
    field_2_value: u32,
    field_3_value: bool,
) {
    let mut fields = default_fields(bump, depositor, ant_mint);
    fields.version = [1, 3, 0];
    let mut data = serialize_common(&fields);
    data.extend_from_slice(&[0u8; 17]);
    data.extend_from_slice(&field_1_value.to_le_bytes());
    data.extend_from_slice(&field_2_value.to_le_bytes());
    data.push(field_3_value as u8);
    assert_eq!(data.len(), EscrowAnt::SIZE);

    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(EscrowAnt::SIZE),
            data,
            owner: ario_ant_escrow::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

// =========================================
// FETCH + ERROR HELPERS
// =========================================

async fn fetch_escrow_ant(ctx: &mut ProgramTestContext, ant_mint: &Pubkey) -> EscrowAnt {
    let (escrow_key, _) = escrow_ant_pda(ant_mint);
    let account = ctx
        .banks_client
        .get_account(escrow_key)
        .await
        .unwrap()
        .expect("EscrowAnt account not found");
    EscrowAnt::try_deserialize(&mut account.data.as_slice()).unwrap()
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

async fn send_migrate_escrow_ant(
    ctx: &mut ProgramTestContext,
    ant_mint: &Pubkey,
    payer: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (escrow_key, _) = escrow_ant_pda(ant_mint);

    let accounts = ario_ant_escrow::accounts::MigrateEscrowAnt {
        ant_mint: *ant_mint,
        escrow: escrow_key,
    };
    let ix = Instruction {
        program_id: ario_ant_escrow::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant_escrow::instruction::MigrateEscrowAnt {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

// =========================================
// TEST SETUP
// =========================================

async fn setup() -> (ProgramTestContext, Keypair, Pubkey, Pubkey, u8) {
    let payer = Keypair::new();
    let ant_mint = Keypair::new();

    let pt = ProgramTest::new(
        "ario_ant_escrow",
        ario_ant_escrow::ID,
        processor!(anchor_processor),
    );

    let mut ctx = pt.start_with_context().await;
    set_sol_account(&mut ctx, &payer.pubkey(), 10_000_000_000);

    let (escrow_key, escrow_bump) = escrow_ant_pda(&ant_mint.pubkey());

    // The `ant_mint` account needs to exist (MigrateEscrowAnt references it
    // as an unchecked AccountInfo for PDA derivation). A zero-data system
    // account suffices.
    set_sol_account(&mut ctx, &ant_mint.pubkey(), 1_000_000);

    (ctx, payer, ant_mint.pubkey(), escrow_key, escrow_bump)
}

// =========================================
// SIZE CONSISTENCY
// =========================================

#[test]
fn test_escrow_ant_size_unchanged() {
    assert_eq!(EscrowAnt::SIZE, ESCROW_ANT_ON_CHAIN_SIZE);
    assert_eq!(EscrowAnt::SIZE, 661);
}

// =========================================
// v1.0.0 -> v1.3.0 (full migration)
// =========================================

#[tokio::test]
async fn test_migrate_schema_1_to_schema_4() {
    let (mut ctx, payer, ant_mint, escrow_key, escrow_bump) = setup().await;

    set_escrow_v100(
        &mut ctx,
        &escrow_key,
        escrow_bump,
        &payer.pubkey(),
        &ant_mint,
    );

    let before = fetch_escrow_ant(&mut ctx, &ant_mint).await;
    assert_eq!(before.version, SchemaVersion::new(1, 0, 0));
    assert_eq!(before.field_1, 0, "field_1 is zero before migration");
    assert_eq!(before.field_2, 0, "field_2 is zero before migration");
    assert!(!before.field_3, "field_3 is false before migration");

    send_migrate_escrow_ant(&mut ctx, &ant_mint, &payer)
        .await
        .unwrap();

    let after = fetch_escrow_ant(&mut ctx, &ant_mint).await;
    assert_eq!(after.version, ESCROW_ANT_VERSION);
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 1000, "1.0.0->1.1.0 arm sets field_1 = 1000");
    assert_eq!(after.field_2, 42, "1.1.0->1.2.0 arm sets field_2 = 42");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    // Existing fields preserved.
    assert_eq!(after.depositor, payer.pubkey());
    assert_eq!(after.ant_mint, ant_mint);
    assert_eq!(after.recipient_protocol, PROTOCOL_ARWEAVE);

    // Second call must return AlreadyLatestVersion.
    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_escrow_ant(&mut ctx, &ant_mint, &payer2).await;
    assert_anchor_error!(result, EscrowError::AlreadyLatestVersion);
}

// =========================================
// v1.1.0 -> v1.3.0 (skip first arm)
// =========================================

#[tokio::test]
async fn test_migrate_schema_2_to_schema_4() {
    let (mut ctx, payer, ant_mint, escrow_key, escrow_bump) = setup().await;

    set_escrow_v110(
        &mut ctx,
        &escrow_key,
        escrow_bump,
        &payer.pubkey(),
        &ant_mint,
        500,
    );

    let before = fetch_escrow_ant(&mut ctx, &ant_mint).await;
    assert_eq!(before.version, SchemaVersion::new(1, 1, 0));
    assert_eq!(before.field_1, 500, "pre-migration field_1 should be 500");

    send_migrate_escrow_ant(&mut ctx, &ant_mint, &payer)
        .await
        .unwrap();

    let after = fetch_escrow_ant(&mut ctx, &ant_mint).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(
        after.field_1, 500,
        "field_1 must NOT be overwritten - arm 1.0.0->1.1.0 is skipped"
    );
    assert_eq!(after.field_2, 42, "1.1.0->1.2.0 arm sets field_2 = 42");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_escrow_ant(&mut ctx, &ant_mint, &payer2).await;
    assert_anchor_error!(result, EscrowError::AlreadyLatestVersion);
}

// =========================================
// v1.2.0 -> v1.3.0 (only last arm)
// =========================================

#[tokio::test]
async fn test_migrate_schema_3_to_schema_4() {
    let (mut ctx, payer, ant_mint, escrow_key, escrow_bump) = setup().await;

    set_escrow_v120(
        &mut ctx,
        &escrow_key,
        escrow_bump,
        &payer.pubkey(),
        &ant_mint,
        777,
        99,
    );

    let before = fetch_escrow_ant(&mut ctx, &ant_mint).await;
    assert_eq!(before.version, SchemaVersion::new(1, 2, 0));
    assert_eq!(before.field_1, 777);
    assert_eq!(before.field_2, 99);
    assert!(!before.field_3, "field_3 is false before migration");

    send_migrate_escrow_ant(&mut ctx, &ant_mint, &payer)
        .await
        .unwrap();

    let after = fetch_escrow_ant(&mut ctx, &ant_mint).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 777, "field_1 preserved");
    assert_eq!(after.field_2, 99, "field_2 preserved");
    assert!(after.field_3, "1.2.0->1.3.0 arm sets field_3 = true");

    let payer2 = Keypair::new();
    set_sol_account(&mut ctx, &payer2.pubkey(), 10_000_000_000);
    let result = send_migrate_escrow_ant(&mut ctx, &ant_mint, &payer2).await;
    assert_anchor_error!(result, EscrowError::AlreadyLatestVersion);
}

// =========================================
// v1.3.0 (already latest — no-op)
// =========================================

#[tokio::test]
async fn test_migrate_schema_4_already_latest() {
    let (mut ctx, payer, ant_mint, escrow_key, escrow_bump) = setup().await;

    set_escrow_v130(
        &mut ctx,
        &escrow_key,
        escrow_bump,
        &payer.pubkey(),
        &ant_mint,
        1000,
        42,
        true,
    );

    let before = fetch_escrow_ant(&mut ctx, &ant_mint).await;
    assert_eq!(before.version, SchemaVersion::new(1, 3, 0));

    let result = send_migrate_escrow_ant(&mut ctx, &ant_mint, &payer).await;
    assert_anchor_error!(result, EscrowError::AlreadyLatestVersion);

    let after = fetch_escrow_ant(&mut ctx, &ant_mint).await;
    assert_eq!(after.version, SchemaVersion::new(1, 3, 0));
    assert_eq!(after.field_1, 1000);
    assert_eq!(after.field_2, 42);
    assert!(after.field_3);
}
