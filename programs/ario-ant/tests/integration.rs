use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas};
use serde::Deserialize;
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
use ario_ant::{InitializeAntParams, SetRecordMetadataParams, SetRecordParams};

/// Metaplex Core program ID
const MPL_CORE_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

/// Assert that a transaction result contains a specific Anchor custom error code.
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
            Err(e) => panic!(
                "Expected custom error {} (code {}), got: {:?}",
                stringify!($error),
                expected_code,
                e
            ),
            Ok(()) => panic!(
                "Expected error {} (code {}), but instruction succeeded",
                stringify!($error),
                expected_code
            ),
        }
    };
}

// =========================================
// HELPERS
// =========================================

/// Bridge Anchor's tied lifetimes to ProcessInstruction's independent lifetimes.
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

/// A valid 43-character Arweave target for tests.
fn test_arweave_id() -> String {
    "a".repeat(43)
}

/// Create a fake Metaplex Core asset account data with the given owner.
fn create_fake_asset_data(owner: &Pubkey) -> Vec<u8> {
    let mut data = vec![0u8; 33];
    data[0] = 1; // AssetV1 key
    data[1..33].copy_from_slice(owner.as_ref());
    data
}

/// Build a ProgramTest with a fake Metaplex Core asset pre-added.
/// Returns (ProgramTest, asset_keypair).
fn program_test_with_asset(owner: &Pubkey) -> (ProgramTest, Keypair) {
    let mut pt = ProgramTest::new("ario_ant", ario_ant::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let asset = Keypair::new();
    let data = create_fake_asset_data(owner);
    let rent = solana_sdk::rent::Rent::default();

    pt.add_account(
        asset.pubkey(),
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(data.len()),
            data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    (pt, asset)
}

/// Build a ProgramTest with a fake asset and a funded second user.
/// Returns (ProgramTest, asset_keypair, second_user_keypair).
fn program_test_with_asset_and_user(owner: &Pubkey) -> (ProgramTest, Keypair, Keypair) {
    let mut pt = ProgramTest::new("ario_ant", ario_ant::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let asset = Keypair::new();
    let data = create_fake_asset_data(owner);
    let rent = solana_sdk::rent::Rent::default();

    pt.add_account(
        asset.pubkey(),
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(data.len()),
            data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let second_user = Keypair::new();
    pt.add_account(
        second_user.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000, // 10 SOL
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    (pt, asset, second_user)
}

// PDA helpers

fn config_pda(asset: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ANT_CONFIG_SEED, asset.as_ref()], &ario_ant::ID)
}

fn controllers_pda(asset: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ANT_CONTROLLERS_SEED, asset.as_ref()], &ario_ant::ID)
}

fn record_pda(asset: &Pubkey, undername: &str) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ANT_RECORD_SEED, asset.as_ref(), &hash_undername(undername)],
        &ario_ant::ID,
    )
}

/// Send an initialize instruction and return the processing result.
async fn send_initialize(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    owner: &Keypair,
    params: InitializeAntParams,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);
    let (root_record_key, _) = record_pda(asset, "@");

    let accounts = ario_ant::accounts::InitializeAnt {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        root_record: root_record_key,
        owner: owner.pubkey(),
        system_program: system_program::ID,
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::Initialize { params }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&owner.pubkey()), &[owner], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a set_record instruction and return the processing result.
async fn send_set_record(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    params: SetRecordParams,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);
    let (record_key, _) = record_pda(asset, &params.undername);

    let accounts = ario_ant::accounts::SetRecord {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        record: record_key,
        caller: caller.pubkey(),
        system_program: system_program::ID,
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::SetRecord { params }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a remove_record instruction and return the processing result.
///
/// `record_metadata` is `Option<Account<…>>` on the handler — when no metadata
/// has been set for `undername`, that PDA simply doesn't exist and the helper
/// must pass `None`. Probing with `get_account` keeps test setup short
/// (one less mandatory `set_record_metadata` per test) without papering over
/// real handler errors.
async fn send_remove_record(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    undername: &str,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);
    let (record_key, _) = record_pda(asset, undername);
    let (record_metadata_key, _) = record_metadata_pda(asset, undername);

    let metadata_exists = ctx
        .banks_client
        .get_account(record_metadata_key)
        .await
        .ok()
        .flatten()
        .is_some();

    let accounts = ario_ant::accounts::RemoveRecord {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        record: record_key,
        record_metadata: if metadata_exists {
            Some(record_metadata_key)
        } else {
            None
        },
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::RemoveRecord {}.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// PDA helper for an `AclConfig` head.
fn acl_config_pda(user: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ACL_CONFIG_SEED, user.as_ref()], &ario_ant::ID)
}

/// PDA helper for an `AclPage` at a specific index.
fn acl_page_pda(user: &Pubkey, page_idx: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ACL_PAGE_SEED, user.as_ref(), &page_idx.to_le_bytes()],
        &ario_ant::ID,
    )
}

/// Mirror of the SDK preflight: ensure `user` has an `AclConfig` head
/// and an `AclPage(page_idx=0)` allocated. Idempotent — checks each
/// account first and only emits the init ix when missing.
///
/// Returns the (`config_key`, `page_key`, `init_ixs`) tuple. Tests
/// prepend `init_ixs` to whichever instruction needs the ACL state.
async fn ensure_acl_state(
    ctx: &mut ProgramTestContext,
    user: &Pubkey,
    payer: &Pubkey,
) -> (Pubkey, Pubkey, Vec<Instruction>) {
    let (config_key, _) = acl_config_pda(user);
    let (page_key, _) = acl_page_pda(user, 0);

    let mut ixs = Vec::new();

    let config_exists = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .is_some();
    if !config_exists {
        let accounts = ario_ant::accounts::RegisterAclConfig {
            acl_config: config_key,
            payer: *payer,
            system_program: system_program::ID,
        };
        ixs.push(Instruction {
            program_id: ario_ant::ID,
            accounts: accounts.to_account_metas(None),
            data: ario_ant::instruction::RegisterAclConfig { user: *user }.data(),
        });
    }

    let page_exists = ctx
        .banks_client
        .get_account(page_key)
        .await
        .unwrap()
        .is_some();
    if !page_exists {
        let accounts = ario_ant::accounts::AddAclPage {
            acl_config: config_key,
            acl_page: page_key,
            payer: *payer,
            system_program: system_program::ID,
        };
        ixs.push(Instruction {
            program_id: ario_ant::ID,
            accounts: accounts.to_account_metas(None),
            data: ario_ant::instruction::AddAclPage {}.data(),
        });
    }

    (config_key, page_key, ixs)
}

/// Locate the `AclPage` (and its index) currently holding `(asset, role)`
/// for `user`. Returns `None` if no live entry exists. Tests use this
/// before `remove_controller` to mirror the SDK preflight that picks
/// the right page out of `[0..page_count)`.
async fn find_acl_entry_page(
    ctx: &mut ProgramTestContext,
    user: &Pubkey,
    asset: &Pubkey,
    role: AclRole,
) -> Option<(Pubkey, u64)> {
    let (config_key, _) = acl_config_pda(user);
    let cfg_account = ctx.banks_client.get_account(config_key).await.unwrap()?;
    let cfg = AclConfig::try_deserialize(&mut cfg_account.data.as_slice()).ok()?;

    for idx in 0..cfg.page_count {
        let (page_key, _) = acl_page_pda(user, idx);
        let page_account = ctx.banks_client.get_account(page_key).await.unwrap();
        let Some(account) = page_account else {
            continue;
        };
        let Ok(page) = AclPage::try_deserialize(&mut account.data.as_slice()) else {
            continue;
        };
        if page.position_of(asset, role as u8).is_some() {
            return Some((page_key, idx));
        }
    }
    None
}

/// Send an add_controller instruction and return the processing result.
///
/// Bundles the SDK preflight inline: ensures the controller's
/// `AclConfig` + `AclPage(0)` exist, then includes the `AddController`
/// ix with the ACL accounts wired up. The contract is responsible for
/// the inline `record_controller` ACL write.
async fn send_add_controller(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    controller: Pubkey,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let (acl_cfg_key, acl_page_key, mut ixs) =
        ensure_acl_state(ctx, &controller, &caller.pubkey()).await;

    let accounts = ario_ant::accounts::AddController {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        controller_acl_config: acl_cfg_key,
        controller_acl_page: acl_page_key,
        caller: caller.pubkey(),
        system_program: system_program::ID,
    };
    ixs.push(Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::AddController { controller }.data(),
    });

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a remove_controller instruction and return the processing result.
///
/// Looks up the `AclPage` currently holding the entry to remove. If no
/// live entry exists, bootstraps page 0 so the contract's account
/// loader doesn't preempt the handler's primary auth/existence checks
/// with `AccountNotInitialized`. This mirrors the production invariant
/// that any controller added through `add_controller` already has the
/// matching ACL state — the only path that hits the bootstrap branch
/// in tests is "negative" cases (unauthorized callers, nonexistent
/// controllers) where the handler is supposed to fail with a specific
/// `AntError`, not at account validation.
async fn send_remove_controller(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    controller: Pubkey,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let (acl_cfg_key, _) = acl_config_pda(&controller);
    let mut ixs: Vec<Instruction> = Vec::new();
    let acl_page_key = match find_acl_entry_page(ctx, &controller, asset, AclRole::Controller).await
    {
        Some((key, _idx)) => key,
        None => {
            // No live entry — bootstrap a placeholder ACL state so
            // account validation passes and the handler can surface
            // the real auth/existence error.
            let (_, page_key, prep_ixs) =
                ensure_acl_state(ctx, &controller, &caller.pubkey()).await;
            ixs.extend(prep_ixs);
            page_key
        }
    };

    let accounts = ario_ant::accounts::RemoveController {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        controller_acl_config: acl_cfg_key,
        controller_acl_page: acl_page_key,
        caller: caller.pubkey(),
    };
    ixs.push(Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::RemoveController { controller }.data(),
    });

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a set_ticker instruction and return the processing result.
async fn send_set_ticker(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    ticker: String,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let accounts = ario_ant::accounts::ManageMetadata {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::SetTicker { ticker }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a set_description instruction and return the processing result.
async fn send_set_description(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    description: String,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let accounts = ario_ant::accounts::ManageMetadata {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::SetDescription { description }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a set_keywords instruction and return the processing result.
async fn send_set_keywords(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    keywords: Vec<String>,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let accounts = ario_ant::accounts::ManageMetadata {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::SetKeywords { keywords }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a set_logo instruction and return the processing result.
async fn send_set_logo(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    logo: String,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let accounts = ario_ant::accounts::ManageMetadata {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::SetLogo { logo }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a transfer_record instruction and return the processing result.
async fn send_transfer_record(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    undername: &str,
    new_owner: Pubkey,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);
    let (record_key, _) = record_pda(asset, undername);

    let accounts = ario_ant::accounts::TransferRecord {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        record: record_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::TransferRecord { new_owner }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a set_name instruction and return the processing result.
async fn send_set_name(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    name: String,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let accounts = ario_ant::accounts::ManageMetadata {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::SetName { name }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a reconcile instruction and return the processing result.
async fn send_reconcile(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);

    let accounts = ario_ant::accounts::Reconcile {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::Reconcile {}.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a migrate_ant instruction and return the processing result.
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

/// Helper: initialize an ANT with default params. Returns nothing on success, panics on failure.
async fn initialize_default_ant(ctx: &mut ProgramTestContext, asset: &Pubkey, owner: &Keypair) {
    let params = InitializeAntParams {
        name: "Test ANT".to_string(),
        ticker: Some("TEST".to_string()),
        target: test_arweave_id(),
        target_protocol: None,
        logo: test_arweave_id(),
        description: "A test ANT".to_string(),
        keywords: vec!["test".to_string()],
    };
    send_initialize(ctx, asset, owner, params).await.unwrap();
}

/// Fetch and deserialize an AntConfig account.
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

/// Fetch and deserialize an AntControllers account.
async fn fetch_controllers(ctx: &mut ProgramTestContext, asset: &Pubkey) -> AntControllers {
    let (controllers_key, _) = controllers_pda(asset);
    let account = ctx
        .banks_client
        .get_account(controllers_key)
        .await
        .unwrap()
        .expect("AntControllers account not found");
    AntControllers::try_deserialize(&mut account.data.as_slice()).unwrap()
}

/// Fetch and deserialize an AntRecord account.
async fn fetch_record(ctx: &mut ProgramTestContext, asset: &Pubkey, undername: &str) -> AntRecord {
    let (record_key, _) = record_pda(asset, undername);
    let account = ctx
        .banks_client
        .get_account(record_key)
        .await
        .unwrap()
        .expect("AntRecord account not found");
    AntRecord::try_deserialize(&mut account.data.as_slice()).unwrap()
}

// =========================================
// TESTS
// =========================================

// 1. Initialize ANT — happy path
#[tokio::test]
async fn test_initialize_ant() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    // Fund the owner so they can pay for account creation
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "My ANT".to_string(),
        ticker: Some("MANT".to_string()),
        target: test_arweave_id(),
        target_protocol: None,
        logo: test_arweave_id(),
        description: "My first ANT".to_string(),
        keywords: vec!["cool".to_string(), "ant".to_string()],
    };
    send_initialize(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Verify config
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.name, "My ANT");
    assert_eq!(config.ticker, "MANT");
    assert_eq!(config.logo, test_arweave_id());
    assert_eq!(config.description, "My first ANT");
    assert_eq!(config.keywords, vec!["cool".to_string(), "ant".to_string()]);
    assert_eq!(config.last_known_owner, owner.pubkey());
    assert_eq!(config.mint, asset.pubkey());

    // Verify controllers (owner should be first controller)
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(controllers.controllers.len(), 1);
    assert_eq!(controllers.controllers[0], owner.pubkey());
    assert_eq!(controllers.mint, asset.pubkey());

    // Verify root @ record
    let root_record = fetch_record(&mut ctx, &asset.pubkey(), "@").await;
    assert_eq!(root_record.undername, "@");
    assert_eq!(root_record.target, test_arweave_id());
    assert_eq!(root_record.ttl_seconds, DEFAULT_TTL_SECONDS);
    assert_eq!(root_record.priority, Some(0));
    assert_eq!(root_record.owner, None);
    assert_eq!(root_record.last_reconciled_owner, owner.pubkey());
    assert_eq!(root_record.mint, asset.pubkey());
}

// 2. Initialize ANT — not NFT holder
#[tokio::test]
async fn test_initialize_ant_not_nft_holder() {
    let actual_owner = Keypair::new();
    let impostor = Keypair::new();
    // Asset is owned by actual_owner, but impostor tries to initialize
    let (mut pt, asset) = program_test_with_asset(&actual_owner.pubkey());
    pt.add_account(
        impostor.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "Stolen ANT".to_string(),
        ticker: None,
        target: test_arweave_id(),
        target_protocol: None,
        logo: String::new(),
        description: String::new(),
        keywords: vec![],
    };

    // The impostor passes themselves as `owner` signer — but the asset data says actual_owner
    let result = send_initialize(&mut ctx, &asset.pubkey(), &impostor, params).await;
    assert_anchor_error!(result, AntError::NotNftHolder);
}

// 3. Set record — happy path (new record)
#[tokio::test]
async fn test_set_record_new() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: "b".repeat(43),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 3600,
        priority: Some(1),
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Verify the record was created
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.undername, "blog");
    assert_eq!(record.target, "b".repeat(43));
    assert_eq!(record.ttl_seconds, 3600);
    assert_eq!(record.priority, Some(1));
    assert_eq!(record.owner, None);
    assert_eq!(record.mint, asset.pubkey());
}

// 4. Set record — unauthorized (non-owner, non-controller)
#[tokio::test]
async fn test_set_record_unauthorized() {
    let owner = Keypair::new();
    let (mut pt, asset, stranger) = program_test_with_asset_and_user(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let params = SetRecordParams {
        undername: "hack".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &stranger, params).await;
    assert_anchor_error!(result, AntError::OnlyOwnerOrControllerCanCreate);
}

// 5. Remove record — happy path
#[tokio::test]
async fn test_remove_record() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record first
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Verify it exists
    let (record_key, _) = record_pda(&asset.pubkey(), "blog");
    let account = ctx.banks_client.get_account(record_key).await.unwrap();
    assert!(account.is_some(), "Record should exist before removal");

    // Remove it
    send_remove_record(&mut ctx, &asset.pubkey(), &owner, "blog")
        .await
        .unwrap();

    // Verify it's gone (account closed)
    let account = ctx.banks_client.get_account(record_key).await.unwrap();
    assert!(account.is_none(), "Record account should be closed");
}

// 6. Remove root @ record — should fail
#[tokio::test]
async fn test_remove_root_record_fails() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_remove_record(&mut ctx, &asset.pubkey(), &owner, "@").await;
    assert_anchor_error!(result, AntError::CannotRemoveRootRecord);
}

// 7. Add controller — happy path
#[tokio::test]
async fn test_add_controller() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let new_controller = Pubkey::new_unique();
    send_add_controller(&mut ctx, &asset.pubkey(), &owner, new_controller)
        .await
        .unwrap();

    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(controllers.controllers.len(), 2);
    assert!(controllers.controllers.contains(&owner.pubkey()));
    assert!(controllers.controllers.contains(&new_controller));
}

// 8. Add controller — max reached
#[tokio::test]
async fn test_add_controller_max_reached() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Owner is already controller #1. Add 3 more to reach MAX_CONTROLLERS (4).
    for i in 0..3 {
        let controller = Pubkey::new_unique();
        send_add_controller(&mut ctx, &asset.pubkey(), &owner, controller)
            .await
            .unwrap_or_else(|e| panic!("Failed to add controller {}: {:?}", i + 2, e));
    }

    // Verify we have 4
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(controllers.controllers.len(), 4);

    // Try to add the 11th — should fail
    let overflow_controller = Pubkey::new_unique();
    let result = send_add_controller(&mut ctx, &asset.pubkey(), &owner, overflow_controller).await;
    assert_anchor_error!(result, AntError::MaxControllersReached);
}

// 9. Remove controller — happy path
#[tokio::test]
async fn test_remove_controller() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let new_controller = Pubkey::new_unique();
    send_add_controller(&mut ctx, &asset.pubkey(), &owner, new_controller)
        .await
        .unwrap();

    // Verify 2 controllers
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(controllers.controllers.len(), 2);

    // Remove the added controller
    send_remove_controller(&mut ctx, &asset.pubkey(), &owner, new_controller)
        .await
        .unwrap();

    // Verify back to 1
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(controllers.controllers.len(), 1);
    assert_eq!(controllers.controllers[0], owner.pubkey());
}

// 10. Set name — happy path
#[tokio::test]
async fn test_set_name() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Verify initial name
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.name, "Test ANT");

    // Change name
    send_set_name(&mut ctx, &asset.pubkey(), &owner, "New Name".to_string())
        .await
        .unwrap();

    // Verify updated
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.name, "New Name");
}

// 11. Reconcile on ownership change
#[tokio::test]
async fn test_reconcile_on_ownership_change() {
    let owner = Keypair::new();
    let new_owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        new_owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Add a controller so we can verify it gets cleared
    let controller = Pubkey::new_unique();
    send_add_controller(&mut ctx, &asset.pubkey(), &owner, controller)
        .await
        .unwrap();

    // Verify 2 controllers (owner + controller)
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(controllers.controllers.len(), 2);

    // Simulate NFT ownership transfer: update the asset data to change the owner
    let new_asset_data = create_fake_asset_data(&new_owner.pubkey());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let updated_asset_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(new_asset_data.len()),
        data: new_asset_data,
        owner: MPL_CORE_PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&asset.pubkey(), &updated_asset_account.into());

    // Call reconcile (permissionless — anyone can call it)
    send_reconcile(&mut ctx, &asset.pubkey(), &new_owner)
        .await
        .unwrap();

    // Verify: config.last_known_owner = new_owner, controllers cleared
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.last_known_owner, new_owner.pubkey());

    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert!(
        controllers.controllers.is_empty(),
        "Controllers should be cleared after ownership change"
    );
}

// 12. Controller can set record
#[tokio::test]
async fn test_controller_can_set_record() {
    let owner = Keypair::new();
    let controller_kp = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        controller_kp.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Add controller
    send_add_controller(&mut ctx, &asset.pubkey(), &owner, controller_kp.pubkey())
        .await
        .unwrap();

    // Controller creates a new record
    let params = SetRecordParams {
        undername: "docs".to_string(),
        target: "c".repeat(43),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 1800,
        priority: Some(2),
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &controller_kp, params)
        .await
        .unwrap();

    // Verify the record was created
    let record = fetch_record(&mut ctx, &asset.pubkey(), "docs").await;
    assert_eq!(record.undername, "docs");
    assert_eq!(record.target, "c".repeat(43));
    assert_eq!(record.ttl_seconds, 1800);
    assert_eq!(record.priority, Some(2));
    assert_eq!(record.mint, asset.pubkey());
}

// 13. Set ticker — happy path
#[tokio::test]
async fn test_set_ticker() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Verify initial ticker
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.ticker, "TEST");

    // Change ticker
    send_set_ticker(&mut ctx, &asset.pubkey(), &owner, "NEWTK".to_string())
        .await
        .unwrap();

    // Verify updated
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.ticker, "NEWTK");
}

// 14. Set description — happy path
#[tokio::test]
async fn test_set_description() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Change description
    send_set_description(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        "Updated description".to_string(),
    )
    .await
    .unwrap();

    // Verify updated
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.description, "Updated description");
}

// 15. Set keywords — happy path
#[tokio::test]
async fn test_set_keywords() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Change keywords
    send_set_keywords(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        vec![
            "keyword1".to_string(),
            "keyword2".to_string(),
            "keyword3".to_string(),
        ],
    )
    .await
    .unwrap();

    // Verify updated
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(
        config.keywords,
        vec![
            "keyword1".to_string(),
            "keyword2".to_string(),
            "keyword3".to_string(),
        ]
    );
}

// 16. Set logo — happy path
#[tokio::test]
async fn test_set_logo() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let new_logo = "b".repeat(43);

    // Change logo
    send_set_logo(&mut ctx, &asset.pubkey(), &owner, new_logo.clone())
        .await
        .unwrap();

    // Verify updated
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.logo, new_logo);
}

// 17. Transfer record — happy path
#[tokio::test]
async fn test_transfer_record_happy() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record with record_owner = owner
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: Some(owner.pubkey()),
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Verify initial record owner
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.owner, Some(owner.pubkey()));

    // Transfer record to new_owner
    let new_owner = Pubkey::new_unique();
    send_transfer_record(&mut ctx, &asset.pubkey(), &owner, "blog", new_owner)
        .await
        .unwrap();

    // Verify record owner updated
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.owner, Some(new_owner));
}

// 18. Transfer record — unauthorized
#[tokio::test]
async fn test_transfer_record_unauthorized() {
    let owner = Keypair::new();
    let (mut pt, asset, stranger) = program_test_with_asset_and_user(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record with record_owner = owner
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: Some(owner.pubkey()),
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Stranger tries to transfer the record
    let new_owner = Pubkey::new_unique();
    let result =
        send_transfer_record(&mut ctx, &asset.pubkey(), &stranger, "blog", new_owner).await;
    assert_anchor_error!(result, AntError::UnauthorizedRecordAccess);
}

// 19. Transfer record — record has no owner
#[tokio::test]
async fn test_transfer_record_no_owner() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record WITHOUT record_owner
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Owner can transfer (assign) record ownership even when record has no owner
    let new_owner = Pubkey::new_unique();
    send_transfer_record(&mut ctx, &asset.pubkey(), &owner, "blog", new_owner)
        .await
        .unwrap();
}

// =========================================
// NEW TESTS
// =========================================

// 20. Set metadata — unauthorized (non-owner, non-controller tries all 4 metadata setters)
#[tokio::test]
async fn test_set_metadata_unauthorized() {
    let owner = Keypair::new();
    let (mut pt, asset, stranger) = program_test_with_asset_and_user(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Verify initial state is correct before attempting unauthorized changes
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.ticker, "TEST");
    assert_eq!(config.description, "A test ANT");
    assert_eq!(config.keywords, vec!["test".to_string()]);
    assert_eq!(config.logo, test_arweave_id());

    // Stranger tries set_ticker — should fail with Unauthorized
    let result = send_set_ticker(&mut ctx, &asset.pubkey(), &stranger, "HACKED".to_string()).await;
    assert_anchor_error!(result, AntError::Unauthorized);

    // Stranger tries set_description — should fail with Unauthorized
    let result = send_set_description(
        &mut ctx,
        &asset.pubkey(),
        &stranger,
        "Hacked description".to_string(),
    )
    .await;
    assert_anchor_error!(result, AntError::Unauthorized);

    // Stranger tries set_keywords — should fail with Unauthorized
    let result = send_set_keywords(
        &mut ctx,
        &asset.pubkey(),
        &stranger,
        vec!["hacked".to_string()],
    )
    .await;
    assert_anchor_error!(result, AntError::Unauthorized);

    // Stranger tries set_logo — should fail with Unauthorized
    let result = send_set_logo(&mut ctx, &asset.pubkey(), &stranger, "b".repeat(43)).await;
    assert_anchor_error!(result, AntError::Unauthorized);

    // Verify nothing changed
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.ticker, "TEST");
    assert_eq!(config.description, "A test ANT");
    assert_eq!(config.keywords, vec!["test".to_string()]);
    assert_eq!(config.logo, test_arweave_id());
}

// 21. Record owner reconciliation on NFT transfer (H-7)
//     Stale record.owner is cleared when set_record detects NFT ownership change.
#[tokio::test]
async fn test_record_owner_reconciliation() {
    let owner = Keypair::new();
    let new_owner = Keypair::new();
    let record_delegate = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        new_owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        record_delegate.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record with a record-level owner (delegate)
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: Some(1),
        record_owner: Some(record_delegate.pubkey()),
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Verify the record has a record-level owner
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.owner, Some(record_delegate.pubkey()));
    assert_eq!(record.last_reconciled_owner, owner.pubkey());

    // Simulate NFT ownership transfer: update the asset data to change the owner
    let new_asset_data = create_fake_asset_data(&new_owner.pubkey());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let updated_asset_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(new_asset_data.len()),
        data: new_asset_data,
        owner: MPL_CORE_PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&asset.pubkey(), &updated_asset_account.into());

    // New owner calls set_record on the same undername — should trigger H-7 reconciliation
    // which clears the stale record.owner because last_reconciled_owner != config.last_known_owner
    let updated_params = SetRecordParams {
        undername: "blog".to_string(),
        target: "c".repeat(43),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 1800,
        priority: Some(2),
        record_owner: None, // not re-assigning
    };
    send_set_record(&mut ctx, &asset.pubkey(), &new_owner, updated_params)
        .await
        .unwrap();

    // Verify: record.owner was cleared by H-7 reconciliation
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(
        record.owner, None,
        "Record owner should be cleared after NFT transfer (H-7 reconciliation)"
    );
    assert_eq!(record.last_reconciled_owner, new_owner.pubkey());
    // Verify the record data was actually updated
    assert_eq!(record.target, "c".repeat(43));
    assert_eq!(record.ttl_seconds, 1800);

    // Verify config was also updated
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.last_known_owner, new_owner.pubkey());

    // Controllers should be cleared too
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert!(
        controllers.controllers.is_empty(),
        "Controllers should be cleared after ownership change"
    );
}

// 22. Migrate ANT version — already at latest version should fail
//     Because initialize sets version = ANT_CONFIG_VERSION (1.0.0), calling
//     migrate_ant on a freshly initialized ANT should immediately fail with
//     AlreadyLatestVersion.  We also test that an account at an unknown old
//     version (0.0.0) returns UnknownSchemaVersion (no production migration
//     path exists below 1.0.0).  Multi-step migration success is covered by
//     the dedicated `migration_e2e` integration test suite compiled with
//     `--features migration-test`.
#[tokio::test]
async fn test_migrate_ant_version() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Verify the ANT is at the latest version
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.version, ANT_CONFIG_VERSION);

    // Try to migrate — should fail because already at latest version
    let result = send_migrate_ant(&mut ctx, &asset.pubkey(), &owner).await;
    assert_anchor_error!(result, AntError::AlreadyLatestVersion);

    // Now test with an account whose version is below any known migration arm.
    // Construct an asset and a pre-existing AntConfig at version 0.0.0 +
    // a minimal AntControllers so the transaction can reach the handler.
    let asset2 = Keypair::new();
    let asset2_data = create_fake_asset_data(&owner.pubkey());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    ctx.set_account(
        &asset2.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(asset2_data.len()),
            data: asset2_data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Build a version-0.0.0 AntConfig directly using try_serialize, then
    // overwrite the version bytes so Anchor's discriminator stays valid but
    // the stored SchemaVersion reads as 0.0.0 (below the current 1.0.0).
    let (config2_key, config2_bump) = config_pda(&asset2.pubkey());
    let v0_config = AntConfig {
        mint: asset2.pubkey(),
        name: "Old ANT".to_string(),
        ticker: "OLD".to_string(),
        logo: test_arweave_id(),
        description: String::new(),
        keywords: vec![],
        last_known_owner: owner.pubkey(),
        bump: config2_bump,
        version: SchemaVersion::new(0, 0, 0),
    };
    let mut v0_data = Vec::new();
    v0_config.try_serialize(&mut v0_data).unwrap();
    v0_data.resize(AntConfig::SIZE, 0);

    ctx.set_account(
        &config2_key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(AntConfig::SIZE),
            data: v0_data,
            owner: ario_ant::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Also create a minimal AntControllers for asset2 so AntMigration can load it.
    let (controllers2_key, controllers2_bump) = controllers_pda(&asset2.pubkey());
    let v0_controllers = AntControllers {
        mint: asset2.pubkey(),
        controllers: vec![],
        bump: controllers2_bump,
        version: SchemaVersion::new(1, 0, 0),
    };
    let mut ctrl_data = Vec::new();
    v0_controllers.try_serialize(&mut ctrl_data).unwrap();
    ctrl_data.resize(AntControllers::SIZE, 0);

    ctx.set_account(
        &controllers2_key,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(AntControllers::SIZE),
            data: ctrl_data,
            owner: ario_ant::ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Verify version is 0.0.0
    let config2 = fetch_config(&mut ctx, &asset2.pubkey()).await;
    assert_eq!(
        config2.version,
        SchemaVersion::new(0, 0, 0),
        "Pre-created ANT should have version 0.0.0"
    );

    // Migrate the 0.0.0 account — the bootstrap arm in `migrate_config_version`
    // stamps it at the post-#53 baseline 1.0.0. This models the pre-#53 upgrade
    // path: accounts created before versioning existed get the version field
    // zero-filled by realloc, then this migration brings them to 1.0.0.
    let result = send_migrate_ant(&mut ctx, &asset2.pubkey(), &owner).await;
    assert!(
        result.is_ok(),
        "Bootstrap migration from 0.0.0 should succeed, got: {:?}",
        result
    );
    let config2_after = fetch_config(&mut ctx, &asset2.pubkey()).await;
    assert_eq!(
        config2_after.version,
        SchemaVersion::new(1, 0, 0),
        "Bootstrap migration should bring account to 1.0.0"
    );
}

// =========================================
// COVERAGE GAP TESTS
// =========================================

// --- A. Initialize error paths ---

// 23. Initialize with ticker too long
#[tokio::test]
async fn test_initialize_ticker_too_long() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "Test".to_string(),
        ticker: Some("A".repeat(17)), // MAX_TICKER_LENGTH = 16
        target: test_arweave_id(),
        target_protocol: None,
        logo: test_arweave_id(),
        description: String::new(),
        keywords: vec![],
    };
    let result = send_initialize(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::TickerTooLong);
}

// 24. Initialize with invalid target
#[tokio::test]
async fn test_initialize_invalid_target() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "Test".to_string(),
        ticker: None,
        target: "short".to_string(), // Not 43 chars
        target_protocol: None,
        logo: String::new(),
        description: String::new(),
        keywords: vec![],
    };
    let result = send_initialize(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidTarget);
}

// 25. Initialize with description too long
#[tokio::test]
async fn test_initialize_description_too_long() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "Test".to_string(),
        ticker: None,
        target: test_arweave_id(),
        target_protocol: None,
        logo: String::new(),
        description: "x".repeat(129), // exceeds MAX_DESCRIPTION_LENGTH = 128
        keywords: vec![],
    };
    let result = send_initialize(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::DescriptionTooLong);
}

// 26. Initialize with name too long
#[tokio::test]
async fn test_initialize_name_too_long() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "a".repeat(62), // MAX_NAME_LENGTH = 61
        ticker: None,
        target: test_arweave_id(),
        target_protocol: None,
        logo: String::new(),
        description: String::new(),
        keywords: vec![],
    };
    let result = send_initialize(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::NameTooLong);
}

// 27. Initialize with invalid logo
#[tokio::test]
async fn test_initialize_invalid_logo() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "Test".to_string(),
        ticker: None,
        target: test_arweave_id(),
        target_protocol: None,
        logo: "not-a-valid-arweave-id".to_string(), // Wrong length
        description: String::new(),
        keywords: vec![],
    };
    let result = send_initialize(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidLogo);
}

// 28. Initialize with empty name
#[tokio::test]
async fn test_initialize_name_empty() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: String::new(),
        ticker: None,
        target: test_arweave_id(),
        target_protocol: None,
        logo: String::new(),
        description: String::new(),
        keywords: vec![],
    };
    let result = send_initialize(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::NameEmpty);
}

// 29. Initialize with invalid keywords
#[tokio::test]
async fn test_initialize_invalid_keywords() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "Test".to_string(),
        ticker: None,
        target: test_arweave_id(),
        target_protocol: None,
        logo: String::new(),
        description: String::new(),
        keywords: vec!["valid".to_string(), "has space".to_string()], // invalid keyword
    };
    let result = send_initialize(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidKeyword);
}

// 30. Initialize with empty ticker (Some(""))
#[tokio::test]
async fn test_initialize_ticker_empty() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let params = InitializeAntParams {
        name: "Test".to_string(),
        ticker: Some(String::new()), // empty ticker
        target: test_arweave_id(),
        target_protocol: None,
        logo: String::new(),
        description: String::new(),
        keywords: vec![],
    };
    let result = send_initialize(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::TickerTooLong);
}

// --- B. set_record validation errors ---

// 31. Set record with invalid target
#[tokio::test]
async fn test_set_record_invalid_target() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: "too-short".to_string(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidTarget);
}

// 32. Set record metadata with invalid logo (must be a valid Arweave TX ID)
#[tokio::test]
async fn test_set_record_invalid_logo() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;
    // Metadata-level validations (logo, description, display_name, keywords)
    // live on `set_record_metadata`, not `set_record` — and require a record
    // to already exist for the undername.
    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 900,
            priority: None,
            record_owner: None,
        },
    )
    .await
    .unwrap();

    let params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: None,
        record_logo: Some("not-a-valid-arweave-id".to_string()),
        record_description: None,
        record_keywords: None,
    };
    let result = send_set_record_metadata(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidLogo);
}

// 33. Set record metadata with description too long (>256 chars)
#[tokio::test]
async fn test_set_record_description_too_long() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;
    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 900,
            priority: None,
            record_owner: None,
        },
    )
    .await
    .unwrap();

    let params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: None,
        record_logo: None,
        record_description: Some("x".repeat(257)),
        record_keywords: None,
    };
    let result = send_set_record_metadata(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::DescriptionTooLong);
}

// 34. Set record with invalid undername
#[tokio::test]
async fn test_set_record_invalid_undername() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let params = SetRecordParams {
        undername: "-invalid".to_string(), // starts with dash
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidUndername);
}

// 35. Set record with invalid TTL
#[tokio::test]
async fn test_set_record_invalid_ttl() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 10, // Below MIN_TTL_SECONDS (60)
        priority: None,
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidTtl);
}

// 36. Set record metadata with display name too long (>61 chars)
#[tokio::test]
async fn test_set_record_display_name_too_long() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;
    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 900,
            priority: None,
            record_owner: None,
        },
    )
    .await
    .unwrap();

    let params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: Some("x".repeat(62)),
        record_logo: None,
        record_description: None,
        record_keywords: None,
    };
    let result = send_set_record_metadata(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::NameTooLong);
}

// 37. Set record metadata with invalid keywords (>8 entries)
#[tokio::test]
async fn test_set_record_invalid_keywords() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;
    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 900,
            priority: None,
            record_owner: None,
        },
    )
    .await
    .unwrap();

    let params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: None,
        record_logo: None,
        record_description: None,
        // MAX_KEYWORDS = 3; 4 → InvalidKeyword
        record_keywords: Some((0..4).map(|i| format!("kw{}", i)).collect()),
    };
    let result = send_set_record_metadata(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidKeyword);
}

// --- B2. IPFS multi-protocol tests ---

// Initialize an ANT with IPFS target
#[tokio::test]
async fn test_initialize_with_ipfs_target() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let ipfs_cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
    let params = InitializeAntParams {
        name: "IPFS ANT".to_string(),
        ticker: Some("IPFS".to_string()),
        target: ipfs_cid.to_string(),
        target_protocol: Some(PROTOCOL_IPFS),
        logo: test_arweave_id(), // logos stay Arweave-only
        description: String::new(),
        keywords: vec![],
    };
    send_initialize(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    let record = fetch_record(&mut ctx, &asset.pubkey(), "@").await;
    assert_eq!(record.target, ipfs_cid);
    assert_eq!(record.target_protocol, PROTOCOL_IPFS);
}

// Set an undername record with IPFS target
#[tokio::test]
async fn test_set_record_ipfs_target() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let ipfs_cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
    let params = SetRecordParams {
        undername: "docs".to_string(),
        target: ipfs_cid.to_string(),
        target_protocol: PROTOCOL_IPFS,
        ttl_seconds: 3600,
        priority: Some(1),
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    let record = fetch_record(&mut ctx, &asset.pubkey(), "docs").await;
    assert_eq!(record.target, ipfs_cid);
    assert_eq!(record.target_protocol, PROTOCOL_IPFS);
    assert_eq!(record.ttl_seconds, 3600);
}

// Mixed protocols: root=Arweave, undername=IPFS on the same ANT
#[tokio::test]
async fn test_mixed_protocol_records() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // Initialize with Arweave root
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Add IPFS undername
    let ipfs_cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: ipfs_cid.to_string(),
        target_protocol: PROTOCOL_IPFS,
        ttl_seconds: 900,
        priority: Some(1),
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Verify root is Arweave
    let root = fetch_record(&mut ctx, &asset.pubkey(), "@").await;
    assert_eq!(root.target_protocol, PROTOCOL_ARWEAVE);
    assert_eq!(root.target, test_arweave_id());

    // Verify undername is IPFS
    let blog = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(blog.target_protocol, PROTOCOL_IPFS);
    assert_eq!(blog.target, ipfs_cid);
}

// set_record overwrites target_protocol on an existing record (Arweave → IPFS → Arweave).
// Verifies the update path clears the old protocol/target correctly and doesn't leave
// stale bytes in the on-chain account.
#[tokio::test]
async fn test_set_record_protocol_switch_arweave_ipfs_arweave() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Step 1: create an Arweave record at undername "docs"
    let arweave_id = test_arweave_id();
    let arweave_params = SetRecordParams {
        undername: "docs".to_string(),
        target: arweave_id.to_string(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 3600,
        priority: Some(5),
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, arweave_params)
        .await
        .unwrap();
    let r = fetch_record(&mut ctx, &asset.pubkey(), "docs").await;
    assert_eq!(r.target_protocol, PROTOCOL_ARWEAVE);
    assert_eq!(r.target, arweave_id);

    // Step 2: switch the same undername to IPFS. Target becomes a CID; priority changes.
    let ipfs_cid = "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi";
    let ipfs_params = SetRecordParams {
        undername: "docs".to_string(),
        target: ipfs_cid.to_string(),
        target_protocol: PROTOCOL_IPFS,
        ttl_seconds: 900,
        priority: Some(10),
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, ipfs_params)
        .await
        .unwrap();
    let r = fetch_record(&mut ctx, &asset.pubkey(), "docs").await;
    assert_eq!(r.target_protocol, PROTOCOL_IPFS);
    assert_eq!(r.target, ipfs_cid);
    assert_eq!(r.ttl_seconds, 900);
    assert_eq!(r.priority, Some(10));

    // Step 3: switch back to Arweave with a different TX id, confirming no stale IPFS state
    let arweave_id_2 = "-k7t8xMoB8hW482609Z9F4bTFMC3MnuW8bTvTyT8pFI";
    let back_params = SetRecordParams {
        undername: "docs".to_string(),
        target: arweave_id_2.to_string(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 1800,
        priority: Some(3),
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, back_params)
        .await
        .unwrap();
    let r = fetch_record(&mut ctx, &asset.pubkey(), "docs").await;
    assert_eq!(r.target_protocol, PROTOCOL_ARWEAVE);
    assert_eq!(r.target, arweave_id_2);
    assert_eq!(r.ttl_seconds, 1800);
    assert_eq!(r.priority, Some(3));
}

// Accept CIDv0 (Qm prefix, 46 chars) with IPFS protocol. Gateways serve both
// CIDv0 and CIDv1; rejecting CIDv0 was a usability tax for no security gain.
#[tokio::test]
async fn test_set_record_ipfs_accepts_cidv0() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let cidv0 = "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbdG";
    let params = SetRecordParams {
        undername: "legacy".to_string(),
        target: cidv0.to_string(),
        target_protocol: PROTOCOL_IPFS,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    let r = fetch_record(&mut ctx, &asset.pubkey(), "legacy").await;
    assert_eq!(r.target_protocol, PROTOCOL_IPFS);
    assert_eq!(r.target, cidv0);
}

// Reject malformed CIDv0 (right prefix, wrong length).
#[tokio::test]
async fn test_set_record_ipfs_rejects_short_cidv0() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // 45 chars — not a valid CIDv0 length
    let params = SetRecordParams {
        undername: "bad".to_string(),
        target: "QmYwAPJzv5CZsnA625s3Xf2nemtYgPpHdWEz79ojWnPbd".to_string(),
        target_protocol: PROTOCOL_IPFS,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidTarget);
}

// Reject Arweave TX ID passed as IPFS protocol
#[tokio::test]
async fn test_set_record_arweave_id_as_ipfs_rejected() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let params = SetRecordParams {
        undername: "wrong".to_string(),
        target: test_arweave_id(), // Arweave ID but claiming IPFS protocol
        target_protocol: PROTOCOL_IPFS,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidTarget);
}

// Reject unknown protocol
#[tokio::test]
async fn test_set_record_unknown_protocol_rejected() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let params = SetRecordParams {
        undername: "future".to_string(),
        target: "bafybeigdyrzt5sfp7udm7hu76uh7y26nf3efuylqabf3oclgtqy55fbzdi".to_string(),
        target_protocol: 99, // unsupported
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::InvalidTarget);
}

// --- C. transfer_record edge cases ---

// 38. Transfer record to same owner (self-transfer)
#[tokio::test]
async fn test_transfer_record_to_self() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record with record_owner = owner
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: Some(owner.pubkey()),
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Try to transfer to same owner — should fail
    let result =
        send_transfer_record(&mut ctx, &asset.pubkey(), &owner, "blog", owner.pubkey()).await;
    assert_anchor_error!(result, AntError::RecordTransferToSelf);
}

// --- D. Reconcile no-change branch ---

// 39. Reconcile when ownership has not changed
#[tokio::test]
async fn test_reconcile_no_ownership_change() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Add a controller
    let controller = Pubkey::new_unique();
    send_add_controller(&mut ctx, &asset.pubkey(), &owner, controller)
        .await
        .unwrap();

    // Verify 2 controllers
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(controllers.controllers.len(), 2);

    // Call reconcile — ownership hasn't changed, should be a no-op
    send_reconcile(&mut ctx, &asset.pubkey(), &owner)
        .await
        .unwrap();

    // Verify controllers are preserved (not cleared)
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(
        controllers.controllers.len(),
        2,
        "Controllers should not be cleared when ownership has not changed"
    );

    // Verify config unchanged
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.last_known_owner, owner.pubkey());
}

// --- E. remove_record unauthorized ---

// 40. Non-owner/non-controller tries to remove a record
#[tokio::test]
async fn test_remove_record_unauthorized() {
    let owner = Keypair::new();
    let (mut pt, asset, stranger) = program_test_with_asset_and_user(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Stranger tries to remove it
    let result = send_remove_record(&mut ctx, &asset.pubkey(), &stranger, "blog").await;
    assert_anchor_error!(result, AntError::Unauthorized);
}

// --- F. add_controller errors ---

// 41. Unauthorized caller tries to add controller
#[tokio::test]
async fn test_add_controller_unauthorized() {
    let owner = Keypair::new();
    let (mut pt, asset, stranger) = program_test_with_asset_and_user(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let new_controller = Pubkey::new_unique();
    let result = send_add_controller(&mut ctx, &asset.pubkey(), &stranger, new_controller).await;
    assert_anchor_error!(result, AntError::Unauthorized);
}

// 42. Add controller that already exists
#[tokio::test]
async fn test_add_controller_already_exists() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Add the owner as a controller (owner is auto-added during init, so this should fail)
    let result = send_add_controller(&mut ctx, &asset.pubkey(), &owner, owner.pubkey()).await;
    assert_anchor_error!(result, AntError::ControllerAlreadyExists);
}

// --- G. remove_controller unauthorized ---

// 43. Non-owner tries to remove controller
#[tokio::test]
async fn test_remove_controller_unauthorized() {
    let owner = Keypair::new();
    let (mut pt, asset, stranger) = program_test_with_asset_and_user(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Stranger tries to remove the owner as controller
    let result = send_remove_controller(&mut ctx, &asset.pubkey(), &stranger, owner.pubkey()).await;
    assert_anchor_error!(result, AntError::Unauthorized);
}

// --- H. set_name errors ---

// 44. Non-owner/non-controller tries to set name
#[tokio::test]
async fn test_set_name_unauthorized() {
    let owner = Keypair::new();
    let (mut pt, asset, stranger) = program_test_with_asset_and_user(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_set_name(&mut ctx, &asset.pubkey(), &stranger, "Hacked".to_string()).await;
    assert_anchor_error!(result, AntError::Unauthorized);
}

// 45. Set name empty
#[tokio::test]
async fn test_set_name_empty() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_set_name(&mut ctx, &asset.pubkey(), &owner, String::new()).await;
    assert_anchor_error!(result, AntError::NameEmpty);
}

// 46. Set name too long
#[tokio::test]
async fn test_set_name_too_long() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_set_name(&mut ctx, &asset.pubkey(), &owner, "a".repeat(62)).await;
    assert_anchor_error!(result, AntError::NameTooLong);
}

// --- I. set_description too long ---

// 47. Set description too long
#[tokio::test]
async fn test_set_description_too_long() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_set_description(&mut ctx, &asset.pubkey(), &owner, "x".repeat(513)).await;
    assert_anchor_error!(result, AntError::DescriptionTooLong);
}

// --- Additional edge cases ---

// 48. Set ticker empty
#[tokio::test]
async fn test_set_ticker_empty() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_set_ticker(&mut ctx, &asset.pubkey(), &owner, String::new()).await;
    assert_anchor_error!(result, AntError::TickerEmpty);
}

// 49. Set ticker too long
#[tokio::test]
async fn test_set_ticker_too_long() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_set_ticker(&mut ctx, &asset.pubkey(), &owner, "A".repeat(17)).await;
    assert_anchor_error!(result, AntError::TickerTooLong);
}

// 50. Set logo invalid
#[tokio::test]
async fn test_set_logo_invalid() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_set_logo(&mut ctx, &asset.pubkey(), &owner, "bad".to_string()).await;
    assert_anchor_error!(result, AntError::InvalidLogo);
}

// 51. Set keywords invalid
#[tokio::test]
async fn test_set_keywords_invalid() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_set_keywords(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        vec!["has space".to_string()],
    )
    .await;
    assert_anchor_error!(result, AntError::InvalidKeyword);
}

// 52. Remove controller that doesn't exist
#[tokio::test]
async fn test_remove_controller_not_found() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let nonexistent = Pubkey::new_unique();
    let result = send_remove_controller(&mut ctx, &asset.pubkey(), &owner, nonexistent).await;
    assert_anchor_error!(result, AntError::ControllerNotFound);
}

// 53. Set record — @ record priority change rejected
#[tokio::test]
async fn test_set_record_root_priority_change_rejected() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Try to change @ record priority to non-zero
    let params = SetRecordParams {
        undername: "@".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: Some(5), // @ must be 0 or None
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert_anchor_error!(result, AntError::CannotChangePriorityOfRoot);
}

// 54. Record owner can update their own record but not set priority
#[tokio::test]
async fn test_record_owner_cannot_set_priority() {
    let owner = Keypair::new();
    let record_owner_kp = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        record_owner_kp.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record with record_owner set
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: Some(1),
        record_owner: Some(record_owner_kp.pubkey()),
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Record owner tries to update with a priority change — should fail
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: "b".repeat(43),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: Some(5), // Cannot set priority as record owner (not ANT owner/controller)
        record_owner: None,
    };
    let result = send_set_record(&mut ctx, &asset.pubkey(), &record_owner_kp, params).await;
    assert_anchor_error!(result, AntError::PriorityRequiresOwnerOrController);
}

// 55. Record owner can update their record (tx_id and ttl, no priority)
#[tokio::test]
async fn test_record_owner_can_update_record() {
    let owner = Keypair::new();
    let record_owner_kp = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        record_owner_kp.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record with record_owner set
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: Some(1),
        record_owner: Some(record_owner_kp.pubkey()),
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Record owner updates the record (no priority change)
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: "b".repeat(43),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 1800,
        priority: None, // No priority change
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &record_owner_kp, params)
        .await
        .unwrap();

    // Verify record was updated
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.target, "b".repeat(43));
    assert_eq!(record.ttl_seconds, 1800);
    // Priority should remain unchanged (record owner cannot change it)
    assert_eq!(record.priority, Some(1));
    // Record owner should still be set (only owner/controllers can change record_owner)
    assert_eq!(record.owner, Some(record_owner_kp.pubkey()));
}

// 56. Add controller as existing owner (owner is already in controller list)
#[tokio::test]
async fn test_add_controller_owner_already_controller() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Owner is already added as controller during initialize.
    // Trying to add owner again should fail.
    let result = send_add_controller(&mut ctx, &asset.pubkey(), &owner, owner.pubkey()).await;
    assert_anchor_error!(result, AntError::ControllerAlreadyExists);
}

// =========================================
// MIGRATION TESTS
// =========================================

/// Build a fake BPF Upgradeable ProgramData account with the given upgrade authority.
/// Layout: account_type(4 LE u32 = 3) + slot(8 LE i64) + option<Pubkey>(1 + 32)
fn build_program_data(upgrade_authority: &Pubkey) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(&3u32.to_le_bytes()); // AccountType::ProgramData = 3
    data.extend_from_slice(&0i64.to_le_bytes()); // slot = 0
    data.push(1); // Some(upgrade_authority)
    data.extend_from_slice(upgrade_authority.as_ref());
    data
}

fn migration_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ANT_MIGRATION_CONFIG_SEED], &ario_ant::ID)
}

fn program_data_pda() -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[ario_ant::ID.as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    );
    pda
}

/// Helper: create a ProgramTest with the BPF Upgradeable ProgramData pre-added.
fn program_test_with_program_data(upgrade_authority: &Pubkey) -> ProgramTest {
    let mut pt = ProgramTest::new("ario_ant", ario_ant::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let pd_key = program_data_pda();
    let pd_data = build_program_data(upgrade_authority);
    let rent = solana_sdk::rent::Rent::default();

    pt.add_account(
        pd_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(pd_data.len()),
            data: pd_data,
            owner: solana_sdk::bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: 0,
        },
    );

    pt
}

/// Send an initialize_migration instruction.
async fn send_initialize_migration(
    ctx: &mut ProgramTestContext,
    payer: &Keypair,
    authority: Pubkey,
    migration_authority: Pubkey,
) -> std::result::Result<(), BanksClientError> {
    let (migration_config_key, _) = migration_config_pda();
    let pd_key = program_data_pda();

    let accounts = ario_ant::accounts::InitializeAntMigration {
        migration_config: migration_config_key,
        payer: payer.pubkey(),
        program_data: pd_key,
        system_program: system_program::ID,
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::InitializeMigration {
            params: ario_ant::InitializeAntMigrationParams {
                authority,
                migration_authority,
            },
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[payer], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send an import_account instruction.
async fn send_import_account(
    ctx: &mut ProgramTestContext,
    authority: &Keypair,
    payer: &Keypair,
    account: Pubkey,
    seeds: Vec<Vec<u8>>,
    data: Vec<u8>,
) -> std::result::Result<(), BanksClientError> {
    let (migration_config_key, _) = migration_config_pda();

    let accounts = ario_ant::accounts::ImportAccount {
        migration_config: migration_config_key,
        authority: authority.pubkey(),
        payer: payer.pubkey(),
        account: account,
        system_program: system_program::ID,
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::ImportAccount { seeds, data }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let signers: Vec<&Keypair> = if authority.pubkey() == payer.pubkey() {
        vec![payer]
    } else {
        vec![authority, payer]
    };
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &signers, blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a finalize_migration instruction.
async fn send_finalize_migration(
    ctx: &mut ProgramTestContext,
    authority: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (migration_config_key, _) = migration_config_pda();

    let accounts = ario_ant::accounts::FinalizeMigration {
        migration_config: migration_config_key,
        authority: authority.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::FinalizeMigration {}.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&authority.pubkey()),
        &[authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// Build a serialized AntConfig for import testing.
fn build_serialized_ant_config(mint: &Pubkey, owner: &Pubkey, bump: u8) -> Vec<u8> {
    let config = AntConfig {
        mint: *mint,
        name: "Imported ANT".to_string(),
        ticker: "IMP".to_string(),
        logo: test_arweave_id(),
        description: "An imported ANT".to_string(),
        keywords: vec![],
        last_known_owner: *owner,
        bump,
        version: ANT_CONFIG_VERSION,
    };
    let mut data = Vec::new();
    config.try_serialize(&mut data).unwrap();
    data.resize(AntConfig::SIZE, 0);
    data
}

fn build_serialized_ant_config_named(
    mint: &Pubkey,
    owner: &Pubkey,
    bump: u8,
    name: &str,
    ticker: &str,
) -> Vec<u8> {
    let config = AntConfig {
        mint: *mint,
        name: name.to_string(),
        ticker: ticker.to_string(),
        logo: test_arweave_id(),
        description: "E2E migration import".to_string(),
        keywords: vec![],
        last_known_owner: *owner,
        bump,
        version: ANT_CONFIG_VERSION,
    };
    let mut data = Vec::new();
    config.try_serialize(&mut data).unwrap();
    data.resize(AntConfig::SIZE, 0);
    data
}

fn build_serialized_ant_controllers(mint: &Pubkey, controllers: &[Pubkey], bump: u8) -> Vec<u8> {
    let ac = AntControllers {
        mint: *mint,
        controllers: controllers.to_vec(),
        bump,
        version: ANT_CONTROLLERS_VERSION,
    };
    let mut data = Vec::new();
    ac.try_serialize(&mut data).unwrap();
    data.resize(AntControllers::SIZE, 0);
    data
}

fn build_serialized_ant_record(
    mint: &Pubkey,
    undername: &str,
    target: &str,
    ttl_seconds: u32,
    last_reconciled_owner: &Pubkey,
    bump: u8,
) -> Vec<u8> {
    let record = AntRecord {
        mint: *mint,
        undername: undername.to_string(),
        target: target.to_string(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds,
        priority: None,
        owner: None,
        last_reconciled_owner: *last_reconciled_owner,
        bump,
        version: ANT_RECORD_VERSION,
    };
    let mut data = Vec::new();
    record.try_serialize(&mut data).unwrap();
    data.resize(AntRecord::SIZE, 0);
    data
}

fn build_serialized_ant_record_metadata(
    mint: &Pubkey,
    undername: &str,
    display_name: Option<String>,
    record_logo: Option<String>,
    record_description: Option<String>,
    record_keywords: Option<Vec<String>>,
    bump: u8,
) -> Vec<u8> {
    let meta = AntRecordMetadata {
        mint: *mint,
        undername_hash: hash_undername(undername),
        display_name,
        record_logo,
        record_description,
        record_keywords,
        bump,
        version: ANT_RECORD_METADATA_VERSION,
    };
    let mut data = Vec::new();
    meta.try_serialize(&mut data).unwrap();
    data.resize(AntRecordMetadata::SIZE, 0);
    data
}

async fn fetch_record_metadata_by_mint(
    ctx: &mut ProgramTestContext,
    mint: &Pubkey,
    undername: &str,
) -> AntRecordMetadata {
    let h = hash_undername(undername);
    let (pda, _) = Pubkey::find_program_address(
        &[ANT_RECORD_META_SEED, mint.as_ref(), h.as_ref()],
        &ario_ant::ID,
    );
    let account = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("record metadata account");
    AntRecordMetadata::try_deserialize(&mut account.data.as_slice()).unwrap()
}

#[derive(Deserialize)]
struct AntImportE2eFixture {
    expected: AntImportE2eExpected,
}

#[derive(Deserialize)]
struct AntImportE2eExpected {
    config_name: String,
    ticker: String,
    record_undername: String,
    ttl_seconds: u32,
}

/// Golden path with **synthesized** full `AntRecord` borsh (see `tests/fixtures/ant-import-e2e.json`
/// for expected strings only). For raw AO snapshot input → Borsh roundtrip,
/// run `yarn test:ant-fixture` in `migration/import` (uses
/// `tests/fixtures/ant-state-sample.json`).
///
/// `ProgramTest` matches BPF behavior on Surfpool; full stack: `./scripts/e2e-ant-import-surfpool.sh`.
#[tokio::test]
async fn test_e2e_migration_import_full_ant_state() {
    let fixture: AntImportE2eFixture =
        serde_json::from_str(include_str!("fixtures/ant-import-e2e.json"))
            .expect("parse tests/fixtures/ant-import-e2e.json");
    let exp = fixture.expected;

    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let owner = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        owner.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    let mint = Pubkey::new_unique();

    let (config_pda, config_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, mint.as_ref()], &ario_ant::ID);
    let (controllers_pda, ctrl_bump) =
        Pubkey::find_program_address(&[ANT_CONTROLLERS_SEED, mint.as_ref()], &ario_ant::ID);
    let under_hash = hash_undername(&exp.record_undername);
    let (record_pda, rec_bump) = Pubkey::find_program_address(
        &[ANT_RECORD_SEED, mint.as_ref(), under_hash.as_ref()],
        &ario_ant::ID,
    );

    let config_data = build_serialized_ant_config_named(
        &mint,
        &owner.pubkey(),
        config_bump,
        &exp.config_name,
        &exp.ticker,
    );
    let ctrl_data = build_serialized_ant_controllers(&mint, &[owner.pubkey()], ctrl_bump);
    let record_data = build_serialized_ant_record(
        &mint,
        &exp.record_undername,
        &test_arweave_id(),
        exp.ttl_seconds,
        &owner.pubkey(),
        rec_bump,
    );

    send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        config_pda,
        vec![ANT_CONFIG_SEED.to_vec(), mint.to_bytes().to_vec()],
        config_data,
    )
    .await
    .unwrap();

    send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        controllers_pda,
        vec![ANT_CONTROLLERS_SEED.to_vec(), mint.to_bytes().to_vec()],
        ctrl_data,
    )
    .await
    .unwrap();

    send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        record_pda,
        vec![
            ANT_RECORD_SEED.to_vec(),
            mint.to_bytes().to_vec(),
            under_hash.to_vec(),
        ],
        record_data,
    )
    .await
    .unwrap();

    let cfg = fetch_config_by_mint(&mut ctx, &mint).await;
    assert_eq!(cfg.name, exp.config_name);
    assert_eq!(cfg.ticker, exp.ticker);
    assert_eq!(cfg.mint, mint);
    assert_eq!(cfg.last_known_owner, owner.pubkey());

    let ctr = fetch_controllers_by_mint(&mut ctx, &mint).await;
    assert_eq!(ctr.mint, mint);
    assert_eq!(ctr.controllers, vec![owner.pubkey()]);

    let rec = fetch_record_by_mint(&mut ctx, &mint, &exp.record_undername).await;
    assert_eq!(rec.undername, exp.record_undername);
    assert_eq!(rec.ttl_seconds, exp.ttl_seconds);
    assert_eq!(rec.mint, mint);
    assert_eq!(rec.last_reconciled_owner, owner.pubkey());
}

async fn fetch_config_by_mint(ctx: &mut ProgramTestContext, mint: &Pubkey) -> AntConfig {
    let (pda, _) = Pubkey::find_program_address(&[ANT_CONFIG_SEED, mint.as_ref()], &ario_ant::ID);
    let account = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("config account");
    AntConfig::try_deserialize(&mut account.data.as_slice()).unwrap()
}

async fn fetch_controllers_by_mint(ctx: &mut ProgramTestContext, mint: &Pubkey) -> AntControllers {
    let (pda, _) =
        Pubkey::find_program_address(&[ANT_CONTROLLERS_SEED, mint.as_ref()], &ario_ant::ID);
    let account = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("controllers account");
    AntControllers::try_deserialize(&mut account.data.as_slice()).unwrap()
}

async fn fetch_record_by_mint(
    ctx: &mut ProgramTestContext,
    mint: &Pubkey,
    undername: &str,
) -> AntRecord {
    let h = hash_undername(undername);
    let (pda, _) =
        Pubkey::find_program_address(&[ANT_RECORD_SEED, mint.as_ref(), h.as_ref()], &ario_ant::ID);
    let account = ctx
        .banks_client
        .get_account(pda)
        .await
        .unwrap()
        .expect("record account");
    AntRecord::try_deserialize(&mut account.data.as_slice()).unwrap()
}

// Real-snapshot Borsh E2E (`tests/fixtures/ant-import-sample.json`) was removed
// when ANT serialization moved out of the snapshot exporter. The end-to-end
// roundtrip now lives in `migration/import` (`yarn test:ant-fixture`):
//   raw `tests/fixtures/ant-state-sample.json`
//     → `transformAntState(...)`
//     → `buildImportAccountIx(...)`
//     → `BorshAccountsCoder.decode(...)` (validates the contract sees the
//        same fields as the raw fixture)
// In-process Rust E2E with synthesized bytes is still covered by
// `test_e2e_migration_import_full_ant_state` above.

// 57. Initialize migration — happy path
#[tokio::test]
async fn test_initialize_migration() {
    let upgrade_auth = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let authority = Pubkey::new_unique();
    let migration_authority = Pubkey::new_unique();

    send_initialize_migration(&mut ctx, &upgrade_auth, authority, migration_authority)
        .await
        .unwrap();

    // Verify migration config was created
    let (mig_key, _) = migration_config_pda();
    let account = ctx
        .banks_client
        .get_account(mig_key)
        .await
        .unwrap()
        .expect("Migration config should exist");
    let mig_config =
        ario_ant::state::AntMigrationConfig::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(mig_config.authority, authority);
    assert_eq!(mig_config.migration_authority, migration_authority);
    assert!(mig_config.migration_active);
}

// 58. Initialize migration — not upgrade authority
#[tokio::test]
async fn test_initialize_migration_not_upgrade_authority() {
    let upgrade_auth = Keypair::new();
    let impostor = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        impostor.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let result = send_initialize_migration(
        &mut ctx,
        &impostor,
        Pubkey::new_unique(),
        Pubkey::new_unique(),
    )
    .await;
    assert_anchor_error!(result, AntError::Unauthorized);
}

// 59. Import account — happy path (import an AntConfig)
#[tokio::test]
async fn test_import_account_happy_path() {
    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let authority = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // Initialize migration
    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        authority.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    // Build seeds for an AntConfig PDA
    let fake_mint = Pubkey::new_unique();
    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), fake_mint.to_bytes().to_vec()];
    let (expected_pda, expected_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, fake_mint.as_ref()], &ario_ant::ID);

    let data = build_serialized_ant_config(&fake_mint, &upgrade_auth.pubkey(), expected_bump);

    // Import the account
    send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        expected_pda,
        seeds,
        data,
    )
    .await
    .unwrap();

    // Verify the imported account
    let account = ctx
        .banks_client
        .get_account(expected_pda)
        .await
        .unwrap()
        .expect("Imported account should exist");
    assert_eq!(account.owner, ario_ant::ID);
    let config = AntConfig::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(config.name, "Imported ANT");
    assert_eq!(config.ticker, "IMP");
    assert_eq!(config.mint, fake_mint);
}

// 60. Import account — wrong migration authority
#[tokio::test]
async fn test_import_account_wrong_authority() {
    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let impostor = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        impostor.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // Initialize migration
    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        upgrade_auth.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    // Try import with wrong authority
    let fake_mint = Pubkey::new_unique();
    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), fake_mint.to_bytes().to_vec()];
    let (expected_pda, expected_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, fake_mint.as_ref()], &ario_ant::ID);
    let data = build_serialized_ant_config(&fake_mint, &upgrade_auth.pubkey(), expected_bump);

    let result = send_import_account(
        &mut ctx,
        &impostor, // wrong authority
        &impostor,
        expected_pda,
        seeds,
        data,
    )
    .await;
    assert_anchor_error!(result, AntError::Unauthorized);
}

// 60b. Import account — lamport-grief defense
//
// Solana security checklist #12: an attacker can predict the migration PDA
// (deterministic from program ID + seeds) and pre-fund it with 1 lamport
// before the migration authority's tx lands. A naive
// `system_program::create_account` rejects pre-funded accounts with
// AccountAlreadyInUse. The migration handler now uses deficit-Transfer +
// Allocate + Assign, which tolerates pre-existing lamports.
#[tokio::test]
async fn test_import_account_lamport_grief_defense() {
    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let authority = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    // ATTACK: pre-fund the predicted PDA before migration authority's tx.
    let fake_mint = Pubkey::new_unique();
    let (expected_pda, expected_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, fake_mint.as_ref()], &ario_ant::ID);
    pt.add_account(
        expected_pda,
        solana_sdk::account::Account {
            lamports: 1,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        authority.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), fake_mint.to_bytes().to_vec()];
    let data = build_serialized_ant_config(&fake_mint, &upgrade_auth.pubkey(), expected_bump);

    // Defense in place: import succeeds despite the pre-funded PDA.
    send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        expected_pda,
        seeds,
        data,
    )
    .await
    .unwrap();

    let account = ctx
        .banks_client
        .get_account(expected_pda)
        .await
        .unwrap()
        .expect("Imported account should exist");
    assert_eq!(account.owner, ario_ant::ID);
    let config = AntConfig::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(config.mint, fake_mint);
}

// 61. Import account — invalid discriminator rejected
#[tokio::test]
async fn test_import_account_invalid_discriminator() {
    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        upgrade_auth.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    // Build data with a bogus discriminator
    let fake_mint = Pubkey::new_unique();
    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), fake_mint.to_bytes().to_vec()];
    let (expected_pda, _) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, fake_mint.as_ref()], &ario_ant::ID);

    let mut bad_data = vec![0u8; AntConfig::SIZE];
    // First 8 bytes are all zeros — not a valid discriminator
    let result = send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        expected_pda,
        seeds,
        bad_data,
    )
    .await;
    assert_anchor_error!(result, AntError::InvalidAccountData);
}

// 62. Finalize migration — happy path
#[tokio::test]
async fn test_finalize_migration() {
    let upgrade_auth = Keypair::new();
    let authority = Keypair::new();
    let migration_auth = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        authority.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        authority.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    // Verify migration is active
    let (mig_key, _) = migration_config_pda();
    let account = ctx
        .banks_client
        .get_account(mig_key)
        .await
        .unwrap()
        .unwrap();
    let mig_config =
        ario_ant::state::AntMigrationConfig::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert!(mig_config.migration_active);

    // Finalize
    send_finalize_migration(&mut ctx, &authority).await.unwrap();

    // Verify migration is inactive
    let account = ctx
        .banks_client
        .get_account(mig_key)
        .await
        .unwrap()
        .unwrap();
    let mig_config =
        ario_ant::state::AntMigrationConfig::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert!(!mig_config.migration_active);
}

// 63. Finalize migration — wrong authority
#[tokio::test]
async fn test_finalize_migration_wrong_authority() {
    let upgrade_auth = Keypair::new();
    let authority = Keypair::new();
    let impostor = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        impostor.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        authority.pubkey(),
        Pubkey::new_unique(),
    )
    .await
    .unwrap();

    let result = send_finalize_migration(&mut ctx, &impostor).await;
    assert_anchor_error!(result, AntError::Unauthorized);
}

// 64. Import after finalization — should fail
#[tokio::test]
async fn test_import_after_finalize_fails() {
    let upgrade_auth = Keypair::new();
    let authority = Keypair::new();
    let migration_auth = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        authority.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        authority.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    // Finalize first
    send_finalize_migration(&mut ctx, &authority).await.unwrap();

    // Try to import after finalization
    let fake_mint = Pubkey::new_unique();
    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), fake_mint.to_bytes().to_vec()];
    let (expected_pda, expected_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, fake_mint.as_ref()], &ario_ant::ID);
    let data = build_serialized_ant_config(&fake_mint, &upgrade_auth.pubkey(), expected_bump);

    let result = send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        expected_pda,
        seeds,
        data,
    )
    .await;
    assert_anchor_error!(result, AntError::MigrationInactive);
}

// 65. Double finalize — should fail
#[tokio::test]
async fn test_double_finalize_fails() {
    let upgrade_auth = Keypair::new();
    let authority = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        authority.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        authority.pubkey(),
        Pubkey::new_unique(),
    )
    .await
    .unwrap();

    // First finalize succeeds
    send_finalize_migration(&mut ctx, &authority).await.unwrap();

    // Advance slot so the second tx gets a fresh blockhash (avoids dedup)
    let slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(slot + 2).unwrap();

    // Second finalize fails (already finalized)
    let result = send_finalize_migration(&mut ctx, &authority).await;
    assert_anchor_error!(result, AntError::MigrationAlreadyFinalized);
}

// 66. Import re-import (idempotent overwrite)
#[tokio::test]
async fn test_import_idempotent_overwrite() {
    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        upgrade_auth.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    let fake_mint = Pubkey::new_unique();
    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), fake_mint.to_bytes().to_vec()];
    let (expected_pda, expected_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, fake_mint.as_ref()], &ario_ant::ID);
    let data = build_serialized_ant_config(&fake_mint, &upgrade_auth.pubkey(), expected_bump);

    // First import
    send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        expected_pda,
        seeds.clone(),
        data.clone(),
    )
    .await
    .unwrap();

    // Verify first import
    let config = fetch_config(&mut ctx, &fake_mint).await;
    assert_eq!(config.name, "Imported ANT");

    // Re-import with updated data (idempotent overwrite)
    let updated_config = AntConfig {
        mint: fake_mint,
        name: "Updated Import".to_string(),
        ticker: "UPD".to_string(),
        logo: test_arweave_id(),
        description: String::new(),
        keywords: vec![],
        last_known_owner: upgrade_auth.pubkey(),
        bump: expected_bump,
        version: ANT_CONFIG_VERSION,
    };
    let mut updated_data = Vec::new();
    updated_config.try_serialize(&mut updated_data).unwrap();
    updated_data.resize(AntConfig::SIZE, 0);

    send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        expected_pda,
        seeds,
        updated_data,
    )
    .await
    .unwrap();

    // Verify overwrite
    let config = fetch_config(&mut ctx, &fake_mint).await;
    assert_eq!(config.name, "Updated Import");
    assert_eq!(config.ticker, "UPD");
}

// 67. Import with wrong PDA — should fail
#[tokio::test]
async fn test_import_wrong_pda_fails() {
    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        upgrade_auth.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    // Seeds for mint_a but account address for mint_b
    let mint_a = Pubkey::new_unique();
    let mint_b = Pubkey::new_unique();
    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), mint_a.to_bytes().to_vec()];
    let (wrong_pda, wrong_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, mint_b.as_ref()], &ario_ant::ID);
    let (correct_pda, _) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, mint_a.as_ref()], &ario_ant::ID);

    let data = build_serialized_ant_config(&mint_a, &upgrade_auth.pubkey(), wrong_bump);

    // Pass correct seeds but wrong account address — should fail PDA mismatch
    let result = send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        wrong_pda, // doesn't match seeds for mint_a
        seeds,
        data,
    )
    .await;
    assert_anchor_error!(result, AntError::InvalidPda);
}

// 68. Default logo matches Lua constant
#[tokio::test]
async fn test_default_logo_matches_lua() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // Initialize with empty logo to get default
    let params = InitializeAntParams {
        name: "Test".to_string(),
        ticker: None,
        target: test_arweave_id(),
        target_protocol: None,
        logo: String::new(), // triggers default
        description: String::new(),
        keywords: vec![],
    };
    send_initialize(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    // Must match Lua: constants.DEFAULT_ANT_LOGO = "AnYvLJTWcG9lr2Ll5MwYWZR2o5uTE39WbpYB0zCxwKM"
    assert_eq!(
        config.logo, "AnYvLJTWcG9lr2Ll5MwYWZR2o5uTE39WbpYB0zCxwKM",
        "Default logo must match Lua DEFAULT_ANT_LOGO constant"
    );
}

// 69. Controller can add another controller (Lua parity: controllers have full management rights)
#[tokio::test]
async fn test_controller_can_add_controller() {
    let owner = Keypair::new();
    let controller_kp = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        controller_kp.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Owner adds controller
    send_add_controller(&mut ctx, &asset.pubkey(), &owner, controller_kp.pubkey())
        .await
        .unwrap();

    // Controller adds another controller (Lua parity: controllers can manage controllers)
    let third_controller = Pubkey::new_unique();
    send_add_controller(&mut ctx, &asset.pubkey(), &controller_kp, third_controller)
        .await
        .unwrap();

    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert_eq!(controllers.controllers.len(), 3);
    assert!(controllers.controllers.contains(&owner.pubkey()));
    assert!(controllers.controllers.contains(&controller_kp.pubkey()));
    assert!(controllers.controllers.contains(&third_controller));
}

// 70. Record owner can transfer record to another address (Lua parity)
#[tokio::test]
async fn test_record_owner_can_transfer_record() {
    let owner = Keypair::new();
    let record_owner_kp = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        record_owner_kp.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create record with record_owner
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: Some(record_owner_kp.pubkey()),
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Record owner transfers to new address
    let new_record_owner = Pubkey::new_unique();
    send_transfer_record(
        &mut ctx,
        &asset.pubkey(),
        &record_owner_kp,
        "blog",
        new_record_owner,
    )
    .await
    .unwrap();

    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.owner, Some(new_record_owner));
}

// 71. Reconcile by third party (permissionless — Solana-specific feature)
#[tokio::test]
async fn test_reconcile_by_stranger() {
    let owner = Keypair::new();
    let new_owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        stranger.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let controller = Pubkey::new_unique();
    send_add_controller(&mut ctx, &asset.pubkey(), &owner, controller)
        .await
        .unwrap();

    // Simulate NFT transfer
    let new_asset_data = create_fake_asset_data(&new_owner.pubkey());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    ctx.set_account(
        &asset.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(new_asset_data.len()),
            data: new_asset_data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Stranger triggers reconciliation (permissionless)
    send_reconcile(&mut ctx, &asset.pubkey(), &stranger)
        .await
        .unwrap();

    // Verify cleanup happened
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.last_known_owner, new_owner.pubkey());
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert!(controllers.controllers.is_empty());
}

// 72. Multiple records on same ANT
#[tokio::test]
async fn test_multiple_records() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create 3 records with different priorities
    for (name, prio) in [("blog", 1u32), ("docs", 2), ("api", 3)] {
        let params = SetRecordParams {
            undername: name.to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 900,
            priority: Some(prio),
            record_owner: None,
        };
        send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
            .await
            .unwrap();
    }

    // Verify all 4 records exist (@ + 3 new)
    let root = fetch_record(&mut ctx, &asset.pubkey(), "@").await;
    assert_eq!(root.priority, Some(0));

    let blog = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(blog.priority, Some(1));

    let docs = fetch_record(&mut ctx, &asset.pubkey(), "docs").await;
    assert_eq!(docs.priority, Some(2));

    let api = fetch_record(&mut ctx, &asset.pubkey(), "api").await;
    assert_eq!(api.priority, Some(3));
}

// 73. Undername case insensitivity (Lua parity: name:lower() before storage)
#[tokio::test]
async fn test_undername_case_insensitive() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create record with uppercase undername
    let params = SetRecordParams {
        undername: "Blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: Some(1),
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Fetch using lowercase — should find the same PDA (case insensitive hash)
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.undername, "blog"); // stored lowercase
    assert_eq!(record.priority, Some(1));

    // Update via mixed case — same PDA, should update not create
    let params = SetRecordParams {
        undername: "BLOG".to_string(),
        target: "b".repeat(43),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 1800,
        priority: None,
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.target, "b".repeat(43));
    assert_eq!(record.ttl_seconds, 1800);
}

// 74. Import after migration deadline — MigrationExpired
//     Uses set_sysvar to warp the clock past MIGRATION_DEADLINE.
#[tokio::test]
async fn test_import_after_migration_deadline() {
    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // Initialize migration
    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        upgrade_auth.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    // Warp clock past MIGRATION_DEADLINE
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = ario_ant::MIGRATION_DEADLINE + 1;
    ctx.set_sysvar(&clock);

    // Try to import — should fail with MigrationExpired
    let fake_mint = Pubkey::new_unique();
    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), fake_mint.to_bytes().to_vec()];
    let (expected_pda, expected_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, fake_mint.as_ref()], &ario_ant::ID);
    let data = build_serialized_ant_config(&fake_mint, &upgrade_auth.pubkey(), expected_bump);

    let result = send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        expected_pda,
        seeds,
        data,
    )
    .await;

    assert_anchor_error!(result, AntError::MigrationExpired);
}

// 75. Import with wrong data size — InvalidAccountData
//     Valid discriminator but data truncated to wrong length.
#[tokio::test]
async fn test_import_account_wrong_data_size() {
    let upgrade_auth = Keypair::new();
    let migration_auth = Keypair::new();
    let mut pt = program_test_with_program_data(&upgrade_auth.pubkey());
    pt.add_account(
        upgrade_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        migration_auth.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // Initialize migration
    send_initialize_migration(
        &mut ctx,
        &upgrade_auth,
        upgrade_auth.pubkey(),
        migration_auth.pubkey(),
    )
    .await
    .unwrap();

    // Build data with valid AntConfig discriminator but wrong size (truncated)
    let fake_mint = Pubkey::new_unique();
    let seeds: Vec<Vec<u8>> = vec![ANT_CONFIG_SEED.to_vec(), fake_mint.to_bytes().to_vec()];
    let (expected_pda, expected_bump) =
        Pubkey::find_program_address(&[ANT_CONFIG_SEED, fake_mint.as_ref()], &ario_ant::ID);

    let full_data = build_serialized_ant_config(&fake_mint, &upgrade_auth.pubkey(), expected_bump);
    // Keep discriminator (8 bytes) + some data, but truncate to wrong total size
    let truncated_data = full_data[..64].to_vec();

    let result = send_import_account(
        &mut ctx,
        &migration_auth,
        &migration_auth,
        expected_pda,
        seeds,
        truncated_data,
    )
    .await;

    assert_anchor_error!(result, AntError::InvalidAccountData);
}

// 76. Transfer record triggers H-7 reconciliation on NFT ownership change
//     Separate code path from set_record's H-7 reconciliation.
#[tokio::test]
async fn test_transfer_record_h7_reconciliation() {
    let owner = Keypair::new();
    let new_owner = Keypair::new();
    let record_delegate = Keypair::new();
    let new_delegate = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        new_owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        record_delegate.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record with a record-level owner (delegate)
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: Some(1),
        record_owner: Some(record_delegate.pubkey()),
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    // Verify: record has a record-level owner
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record.owner, Some(record_delegate.pubkey()));
    assert_eq!(record.last_reconciled_owner, owner.pubkey());

    // Simulate NFT ownership transfer
    let new_asset_data = create_fake_asset_data(&new_owner.pubkey());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    ctx.set_account(
        &asset.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(new_asset_data.len()),
            data: new_asset_data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // New owner calls transfer_record — should trigger H-7 reconciliation first,
    // clearing stale record_delegate, then assign new_delegate as record owner
    send_transfer_record(
        &mut ctx,
        &asset.pubkey(),
        &new_owner,
        "blog",
        new_delegate.pubkey(),
    )
    .await
    .unwrap();

    // Verify: record owner is now new_delegate (not the old stale record_delegate)
    let record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(
        record.owner,
        Some(new_delegate.pubkey()),
        "New owner should have been able to re-assign record ownership after H-7 reconciliation"
    );
    assert_eq!(record.last_reconciled_owner, new_owner.pubkey());

    // Verify config was also updated
    let config = fetch_config(&mut ctx, &asset.pubkey()).await;
    assert_eq!(config.last_known_owner, new_owner.pubkey());

    // Controllers should be cleared too
    let controllers = fetch_controllers(&mut ctx, &asset.pubkey()).await;
    assert!(
        controllers.controllers.is_empty(),
        "Controllers should be cleared after ownership change"
    );
}

// =========================================
// SECURITY: init_if_needed double-call (SetRecord)
// =========================================

/// Calling set_record twice on the same undername must update (not reinitialize)
/// the existing record.
#[tokio::test]
async fn test_set_record_init_if_needed_preserves_state() {
    let (pt, asset) = program_test_with_asset(&Pubkey::new_unique());
    let mut ctx = pt.start_with_context().await;
    let owner = ctx.payer.insecure_clone();

    let data = create_fake_asset_data(&owner.pubkey());
    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        &asset.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(data.len()),
            data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let target_1 = test_arweave_id();
    let target_2 = "b".repeat(43);

    // First call creates the record via init_if_needed
    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordParams {
            undername: "blog".to_string(),
            target: target_1.clone(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 3600,
            priority: Some(1),
            record_owner: None,
        },
    )
    .await
    .unwrap();

    let record_1 = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(record_1.target, target_1);
    assert_eq!(record_1.ttl_seconds, 3600);

    // Second call must update in-place, not reset
    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordParams {
            undername: "blog".to_string(),
            target: target_2.clone(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 7200,
            priority: Some(2),
            record_owner: None,
        },
    )
    .await
    .unwrap();

    let record_2 = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
    assert_eq!(
        record_2.target, target_2,
        "target must be updated on second call"
    );
    assert_eq!(
        record_2.ttl_seconds, 7200,
        "ttl must be updated on second call"
    );
}

// =========================================
// SECURITY: CU consumption assertion
// =========================================

/// Verify that InitializeAnt stays within its CU budget.
#[tokio::test]
async fn test_initialize_cu_consumption() {
    let (pt, asset) = program_test_with_asset(&Pubkey::new_unique());
    let mut ctx = pt.start_with_context().await;
    let owner = ctx.payer.insecure_clone();

    let data = create_fake_asset_data(&owner.pubkey());
    let rent = solana_sdk::rent::Rent::default();
    ctx.set_account(
        &asset.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(data.len()),
            data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    let (config_key, _) = config_pda(&asset.pubkey());
    let (controllers_key, _) = controllers_pda(&asset.pubkey());
    let (root_record_key, _) = record_pda(&asset.pubkey(), "@");

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::InitializeAnt {
            asset: asset.pubkey(),
            ant_config: config_key,
            ant_controllers: controllers_key,
            root_record: root_record_key,
            owner: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::Initialize {
            params: InitializeAntParams {
                name: "Test ANT".to_string(),
                ticker: Some("TEST".to_string()),
                target: test_arweave_id(),
                target_protocol: None,
                logo: test_arweave_id(),
                description: "A test ANT".to_string(),
                keywords: vec!["test".to_string()],
            },
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&owner.pubkey()), &[&owner], blockhash);
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "Initialize should succeed");
    let metadata = result.metadata.expect("metadata must be present");
    // Measured in BPF mode (BPF_OUT_DIR set): ~45K–60K CU across runs. Native
    // dispatch reports ~800. Threshold sized for BPF + ~1.5× headroom.
    assert!(
        metadata.compute_units_consumed < 90_000,
        "InitializeAnt used {} CU, expected < 90_000",
        metadata.compute_units_consumed
    );
}

// =========================================
// SET / REMOVE RECORD METADATA (separate PDA)
// =========================================

/// Helper: build a SetRecordParams with no metadata (the new shape).
fn basic_set_record_params(undername: &str) -> SetRecordParams {
    SetRecordParams {
        undername: undername.to_string(),
        target: test_arweave_id(),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    }
}

#[tokio::test]
async fn test_set_record_metadata_creates_pda() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        basic_set_record_params("blog"),
    )
    .await
    .unwrap();

    assert!(fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
        .await
        .is_none());

    let params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: Some("My Blog".to_string()),
        record_logo: Some(test_arweave_id()),
        record_description: Some("A blog about stuff".to_string()),
        record_keywords: Some(vec!["personal".to_string(), "tech".to_string()]),
    };
    send_set_record_metadata(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    let meta = fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
        .await
        .expect("metadata PDA should exist after set_record_metadata");
    assert_eq!(meta.mint, asset.pubkey());
    assert_eq!(meta.undername_hash, hash_undername("blog"));
    assert_eq!(meta.display_name, Some("My Blog".to_string()));
    assert!(meta.record_logo.is_some());
    assert_eq!(
        meta.record_description,
        Some("A blog about stuff".to_string())
    );
    assert_eq!(
        meta.record_keywords,
        Some(vec!["personal".to_string(), "tech".to_string()])
    );
}

#[tokio::test]
async fn test_set_record_metadata_without_record_fails() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // No "blog" core record exists. Anchor's seeds constraint on the
    // `record` account fails to load the non-existent AntRecord PDA.
    let params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: Some("Should Fail".to_string()),
        record_logo: None,
        record_description: None,
        record_keywords: None,
    };
    let result = send_set_record_metadata(&mut ctx, &asset.pubkey(), &owner, params).await;
    assert!(
        result.is_err(),
        "set_record_metadata without an existing core record must fail"
    );
}

#[tokio::test]
async fn test_set_record_metadata_updates_existing() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        basic_set_record_params("blog"),
    )
    .await
    .unwrap();

    send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: Some("v1".to_string()),
            record_logo: None,
            record_description: Some("first".to_string()),
            record_keywords: None,
        },
    )
    .await
    .unwrap();

    // Second write — overwrites everything, including clearing the description.
    send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: Some("v2".to_string()),
            record_logo: Some(test_arweave_id()),
            record_description: None,
            record_keywords: Some(vec!["updated".to_string()]),
        },
    )
    .await
    .unwrap();

    let meta = fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
        .await
        .expect("metadata PDA should still exist");
    assert_eq!(meta.display_name, Some("v2".to_string()));
    assert!(meta.record_logo.is_some());
    assert_eq!(meta.record_description, None);
    assert_eq!(meta.record_keywords, Some(vec!["updated".to_string()]));
}

#[tokio::test]
async fn test_set_record_metadata_permission_check() {
    let owner = Keypair::new();
    let (mut pt, asset, stranger) = program_test_with_asset_and_user(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        basic_set_record_params("blog"),
    )
    .await
    .unwrap();

    let params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: Some("Hacked".to_string()),
        record_logo: None,
        record_description: None,
        record_keywords: None,
    };
    let result = send_set_record_metadata(&mut ctx, &asset.pubkey(), &stranger, params).await;
    assert_anchor_error!(result, AntError::UnauthorizedRecordAccess);

    assert!(fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
        .await
        .is_none());
}

#[tokio::test]
async fn test_remove_record_metadata_closes_pda() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        basic_set_record_params("blog"),
    )
    .await
    .unwrap();
    send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: Some("My Blog".to_string()),
            record_logo: None,
            record_description: None,
            record_keywords: None,
        },
    )
    .await
    .unwrap();

    let before = ctx
        .banks_client
        .get_account(owner.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;

    send_remove_record_metadata(&mut ctx, &asset.pubkey(), &owner, "blog")
        .await
        .unwrap();

    assert!(
        fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
            .await
            .is_none(),
        "metadata PDA should be closed"
    );

    let after = ctx
        .banks_client
        .get_account(owner.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert!(
        after > before,
        "rent should be returned to caller (before={}, after={})",
        before,
        after
    );

    // Core record itself remains untouched
    let _record = fetch_record(&mut ctx, &asset.pubkey(), "blog").await;
}

#[tokio::test]
async fn test_set_record_metadata_validates_fields() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;
    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        basic_set_record_params("blog"),
    )
    .await
    .unwrap();

    let r = send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: Some("a".repeat(MAX_NAME_LENGTH + 1)),
            record_logo: None,
            record_description: None,
            record_keywords: None,
        },
    )
    .await;
    assert_anchor_error!(r, AntError::NameTooLong);

    let r = send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: None,
            record_logo: Some("bad-logo".to_string()),
            record_description: None,
            record_keywords: None,
        },
    )
    .await;
    assert_anchor_error!(r, AntError::InvalidLogo);

    let r = send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: None,
            record_logo: None,
            record_description: Some("x".repeat(MAX_DESCRIPTION_LENGTH + 1)),
            record_keywords: None,
        },
    )
    .await;
    assert_anchor_error!(r, AntError::DescriptionTooLong);

    let r = send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: None,
            record_logo: None,
            record_description: None,
            record_keywords: Some(vec!["has space".to_string()]),
        },
    )
    .await;
    assert_anchor_error!(r, AntError::InvalidKeyword);
}

#[tokio::test]
async fn test_remove_record_orphans_metadata_pda() {
    // Demonstrates: remove_record closes AntRecord but leaves AntRecordMetadata
    // behind. After my refactor split AntRecord into core + metadata PDAs,
    // removeRecord no longer cleans up its sibling metadata PDA — rent leak.
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    pt.add_account(
        owner.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        basic_set_record_params("blog"),
    )
    .await
    .unwrap();
    send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: Some("My Blog".to_string()),
            record_logo: None,
            record_description: None,
            record_keywords: None,
        },
    )
    .await
    .unwrap();

    // Sanity: metadata exists
    assert!(fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
        .await
        .is_some());

    // Now remove the core record only
    send_remove_record(&mut ctx, &asset.pubkey(), &owner, "blog")
        .await
        .unwrap();

    // After my refactor this assertion should pass — but it WILL FAIL with
    // current code because remove_record doesn't touch the metadata PDA.
    assert!(
        fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
            .await
            .is_none(),
        "metadata PDA should be closed when its sibling AntRecord is removed"
    );
}
fn record_metadata_pda(asset: &Pubkey, undername: &str) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            ANT_RECORD_META_SEED,
            asset.as_ref(),
            &hash_undername(undername),
        ],
        &ario_ant::ID,
    )
}

/// Send a set_record_metadata instruction and return the processing result.
async fn send_set_record_metadata(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    params: SetRecordMetadataParams,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);
    let (record_key, _) = record_pda(asset, &params.undername);
    let (record_metadata_key, _) = record_metadata_pda(asset, &params.undername);

    let accounts = ario_ant::accounts::SetRecordMetadata {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        record: record_key,
        record_metadata: record_metadata_key,
        caller: caller.pubkey(),
        system_program: system_program::ID,
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::SetRecordMetadata { params }.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Send a remove_record_metadata instruction and return the processing result.
async fn send_remove_record_metadata(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    undername: &str,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);
    let (record_metadata_key, _) = record_metadata_pda(asset, undername);

    let accounts = ario_ant::accounts::RemoveRecordMetadata {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        record_metadata: record_metadata_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::RemoveRecordMetadata {
            undername: undername.to_string(),
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Fetch and deserialize an AntRecordMetadata account, if it exists.
async fn fetch_record_metadata_opt(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    undername: &str,
) -> Option<AntRecordMetadata> {
    let (key, _) = record_metadata_pda(asset, undername);
    let account = ctx.banks_client.get_account(key).await.unwrap()?;
    Some(AntRecordMetadata::try_deserialize(&mut account.data.as_slice()).unwrap())
}

// -----------------------------------------
// NEW-1 (security audit 2026-04-30) — orphan cleanup
// `RemoveRecord.record_metadata` is `Option<Account<…>>`. A forgetful caller
// can close the record without closing the sibling metadata PDA, leaving
// rent permanently locked at the orphan. The permissionless
// `close_orphaned_record_metadata` instruction recovers that rent.
// -----------------------------------------

/// Remove the core record but intentionally pass `None` for `record_metadata`,
/// even when one exists. Simulates the forgetful caller from NEW-1.
async fn send_remove_record_omit_metadata(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    undername: &str,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let (controllers_key, _) = controllers_pda(asset);
    let (record_key, _) = record_pda(asset, undername);

    let accounts = ario_ant::accounts::RemoveRecord {
        asset: *asset,
        ant_config: config_key,
        ant_controllers: controllers_key,
        record: record_key,
        record_metadata: None,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::RemoveRecord {}.data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

async fn send_close_orphaned_record_metadata(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    undername: &str,
) -> std::result::Result<(), BanksClientError> {
    let (record_key, _) = record_pda(asset, undername);
    let (meta_key, _) = record_metadata_pda(asset, undername);

    let accounts = ario_ant::accounts::CloseOrphanedRecordMetadata {
        asset: *asset,
        record: record_key,
        record_metadata: meta_key,
        caller: caller.pubkey(),
    };

    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::CloseOrphanedRecordMetadata {
            undername: undername.to_string(),
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn test_close_orphaned_record_metadata_after_forgetful_remove() {
    let owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    for who in [owner.pubkey(), stranger.pubkey()] {
        pt.add_account(
            who,
            solana_sdk::account::Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        basic_set_record_params("blog"),
    )
    .await
    .unwrap();
    send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: Some("My Blog".to_string()),
            record_logo: None,
            record_description: None,
            record_keywords: None,
        },
    )
    .await
    .unwrap();

    // Forgetful owner removes the record but omits the metadata PDA.
    send_remove_record_omit_metadata(&mut ctx, &asset.pubkey(), &owner, "blog")
        .await
        .unwrap();

    // Record is gone; metadata is now an orphan.
    let (record_key, _) = record_pda(&asset.pubkey(), "blog");
    assert!(
        ctx.banks_client
            .get_account(record_key)
            .await
            .unwrap()
            .is_none(),
        "core record should be closed"
    );
    assert!(
        fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
            .await
            .is_some(),
        "metadata PDA should be orphaned (forgetful caller)"
    );

    // Permissionless cleanup by a stranger — rent flows to them.
    let stranger_balance_before = ctx
        .banks_client
        .get_account(stranger.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;

    send_close_orphaned_record_metadata(&mut ctx, &asset.pubkey(), &stranger, "blog")
        .await
        .expect("orphan cleanup must succeed");

    assert!(
        fetch_record_metadata_opt(&mut ctx, &asset.pubkey(), "blog")
            .await
            .is_none(),
        "orphaned metadata PDA should now be closed"
    );

    let stranger_balance_after = ctx
        .banks_client
        .get_account(stranger.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert!(
        stranger_balance_after > stranger_balance_before,
        "rent reward should flow to caller (got delta {})",
        stranger_balance_after.saturating_sub(stranger_balance_before),
    );
}

#[tokio::test]
async fn test_close_orphaned_record_metadata_rejects_when_record_alive() {
    let owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    for who in [owner.pubkey(), stranger.pubkey()] {
        pt.add_account(
            who,
            solana_sdk::account::Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    send_set_record(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        basic_set_record_params("blog"),
    )
    .await
    .unwrap();
    send_set_record_metadata(
        &mut ctx,
        &asset.pubkey(),
        &owner,
        SetRecordMetadataParams {
            undername: "blog".to_string(),
            display_name: Some("My Blog".to_string()),
            record_logo: None,
            record_description: None,
            record_keywords: None,
        },
    )
    .await
    .unwrap();

    // Record is still live. Cleanup must refuse.
    let result =
        send_close_orphaned_record_metadata(&mut ctx, &asset.pubkey(), &stranger, "blog").await;
    assert_anchor_error!(result, AntError::RecordStillExists);
}

// =========================================================================
// RENT RECLAIM — user-callable close ixs + admin orphan cleanup
// =========================================================================
// Coverage:
//   - close_ant_record (happy + non-owner rejected)
//   - close_ant_record_metadata_for_owner (non-owner rejected)
//   - close_ant_controllers (happy + non-owner rejected)
//   - close_ant_config (happy + non-owner rejected)
//   - admin_close_orphaned_ant_state (asset-still-alive rejected)
//   - End-to-end: owner closes records → controllers → config

/// Fund a fresh keypair as a system-owned account so it can pay tx fees.
/// Matches the pattern used by `test_initialize_ant`.
fn fund_user(pt: &mut ProgramTest, who: &Pubkey) {
    pt.add_account(
        *who,
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
}

async fn send_close_ant_record(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    undername: &str,
) -> std::result::Result<(), BanksClientError> {
    let (record_key, _) = record_pda(asset, undername);
    let accounts = ario_ant::accounts::CloseAntRecord {
        asset: *asset,
        record: record_key,
        caller: caller.pubkey(),
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::CloseAntRecord {
            _undername: undername.to_string(),
        }
        .data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

async fn send_close_ant_record_metadata_for_owner(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
    undername: &str,
) -> std::result::Result<(), BanksClientError> {
    let (meta_key, _) = record_metadata_pda(asset, undername);
    let accounts = ario_ant::accounts::CloseAntRecordMetadataForOwner {
        asset: *asset,
        record_metadata: meta_key,
        caller: caller.pubkey(),
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::CloseAntRecordMetadataForOwner {
            _undername: undername.to_string(),
        }
        .data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

async fn send_close_ant_controllers(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (controllers_key, _) = controllers_pda(asset);
    let accounts = ario_ant::accounts::CloseAntControllers {
        asset: *asset,
        controllers: controllers_key,
        caller: caller.pubkey(),
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::CloseAntControllers {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

async fn send_close_ant_config(
    ctx: &mut ProgramTestContext,
    asset: &Pubkey,
    caller: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (config_key, _) = config_pda(asset);
    let accounts = ario_ant::accounts::CloseAntConfig {
        asset: *asset,
        config: config_key,
        caller: caller.pubkey(),
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::CloseAntConfig {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix], Some(&caller.pubkey()), &[caller], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn test_close_ant_record_succeeds_for_owner() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund_user(&mut pt, &owner.pubkey());
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Add a non-@ record so closing it doesn't fight @-record protection
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: 0,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    let (record_key, _) = record_pda(&asset.pubkey(), "blog");
    let rent_before = ctx
        .banks_client
        .get_account(record_key)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let owner_lamports_before = ctx
        .banks_client
        .get_account(owner.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;

    send_close_ant_record(&mut ctx, &asset.pubkey(), &owner, "blog")
        .await
        .unwrap();

    // PDA is closed (data empty + System-owned)
    let acct = ctx.banks_client.get_account(record_key).await.unwrap();
    assert!(acct.is_none() || acct.unwrap().data.is_empty());

    // Refund went to owner (modulo tx fees the owner also paid as payer)
    let owner_lamports_after = ctx
        .banks_client
        .get_account(owner.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert!(
        owner_lamports_after > owner_lamports_before - rent_before / 2,
        "owner should net-positive after closing record (refund={}, before={}, after={})",
        rent_before,
        owner_lamports_before,
        owner_lamports_after,
    );
}

#[tokio::test]
async fn test_close_ant_record_rejects_non_owner() {
    let owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    for who in [owner.pubkey(), stranger.pubkey()] {
        pt.add_account(
            who,
            solana_sdk::account::Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: 0,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();

    let result = send_close_ant_record(&mut ctx, &asset.pubkey(), &stranger, "blog").await;
    assert_anchor_error!(result, AntError::NotNftHolder);
}

#[tokio::test]
async fn test_close_ant_controllers_succeeds_for_owner() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund_user(&mut pt, &owner.pubkey());
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let (controllers_key, _) = controllers_pda(&asset.pubkey());
    let exists_before = ctx
        .banks_client
        .get_account(controllers_key)
        .await
        .unwrap()
        .is_some();
    assert!(exists_before);

    send_close_ant_controllers(&mut ctx, &asset.pubkey(), &owner)
        .await
        .unwrap();

    let after = ctx.banks_client.get_account(controllers_key).await.unwrap();
    assert!(after.is_none() || after.unwrap().data.is_empty());
}

#[tokio::test]
async fn test_close_ant_controllers_rejects_non_owner() {
    let owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    for who in [owner.pubkey(), stranger.pubkey()] {
        pt.add_account(
            who,
            solana_sdk::account::Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_close_ant_controllers(&mut ctx, &asset.pubkey(), &stranger).await;
    assert_anchor_error!(result, AntError::NotNftHolder);
}

#[tokio::test]
async fn test_close_ant_config_succeeds_for_owner() {
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund_user(&mut pt, &owner.pubkey());
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let (config_key, _) = config_pda(&asset.pubkey());
    assert!(ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .is_some());

    send_close_ant_config(&mut ctx, &asset.pubkey(), &owner)
        .await
        .unwrap();

    let after = ctx.banks_client.get_account(config_key).await.unwrap();
    assert!(after.is_none() || after.unwrap().data.is_empty());
}

#[tokio::test]
async fn test_close_ant_config_rejects_non_owner() {
    let owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    for who in [owner.pubkey(), stranger.pubkey()] {
        pt.add_account(
            who,
            solana_sdk::account::Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let result = send_close_ant_config(&mut ctx, &asset.pubkey(), &stranger).await;
    assert_anchor_error!(result, AntError::NotNftHolder);
}

#[tokio::test]
async fn test_close_ant_record_metadata_rejects_non_owner() {
    let owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    for who in [owner.pubkey(), stranger.pubkey()] {
        pt.add_account(
            who,
            solana_sdk::account::Account {
                lamports: 10_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Add a record + metadata so there's something to attempt to close
    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: test_arweave_id(),
        target_protocol: 0,
        ttl_seconds: 900,
        priority: None,
        record_owner: None,
    };
    send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
        .await
        .unwrap();
    let meta_params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: Some("Blog".to_string()),
        record_logo: None,
        record_description: None,
        record_keywords: None,
    };
    send_set_record_metadata(&mut ctx, &asset.pubkey(), &owner, meta_params)
        .await
        .unwrap();

    let result =
        send_close_ant_record_metadata_for_owner(&mut ctx, &asset.pubkey(), &stranger, "blog")
            .await;
    assert_anchor_error!(result, AntError::NotNftHolder);
}

#[tokio::test]
async fn test_close_all_then_records_order_independent() {
    // End-to-end: owner closes records → controllers → config, in that
    // order. Verifies each ix independently doesn't block the next.
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund_user(&mut pt, &owner.pubkey());
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // 2 records
    for undername in &["blog", "docs"] {
        let params = SetRecordParams {
            undername: undername.to_string(),
            target: test_arweave_id(),
            target_protocol: 0,
            ttl_seconds: 900,
            priority: None,
            record_owner: None,
        };
        send_set_record(&mut ctx, &asset.pubkey(), &owner, params)
            .await
            .unwrap();
    }

    // Close records first
    for undername in &["blog", "docs"] {
        send_close_ant_record(&mut ctx, &asset.pubkey(), &owner, undername)
            .await
            .unwrap();
    }

    // Then controllers (no record dependency)
    send_close_ant_controllers(&mut ctx, &asset.pubkey(), &owner)
        .await
        .unwrap();

    // Then config (no controllers dependency)
    send_close_ant_config(&mut ctx, &asset.pubkey(), &owner)
        .await
        .unwrap();

    // All four PDAs closed
    for key in &[
        config_pda(&asset.pubkey()).0,
        controllers_pda(&asset.pubkey()).0,
        record_pda(&asset.pubkey(), "blog").0,
        record_pda(&asset.pubkey(), "docs").0,
    ] {
        let acct = ctx.banks_client.get_account(*key).await.unwrap();
        assert!(acct.is_none() || acct.unwrap().data.is_empty());
    }
}

/// Pre-populate an `AntMigrationConfig` PDA with `authority` set to the
/// given pubkey. Used by `admin_close_orphaned_ant_state` tests so the
/// ix's `has_one = authority` gate can be verified end-to-end without
/// running through `initialize_migration`.
fn seed_migration_config(pt: &mut ProgramTest, authority: &Pubkey) -> Pubkey {
    use anchor_lang::{AnchorSerialize, Discriminator};
    let (pda, bump) = migration_config_pda();
    let cfg = ario_ant::state::AntMigrationConfig {
        authority: *authority,
        migration_authority: Pubkey::new_unique(),
        migration_active: true,
        bump,
        version: ario_ant::state::ANT_MIGRATION_CONFIG_VERSION,
    };
    let mut data = ario_ant::state::AntMigrationConfig::DISCRIMINATOR.to_vec();
    cfg.serialize(&mut data).unwrap();
    data.resize(ario_ant::state::AntMigrationConfig::SIZE, 0);
    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        pda,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(data.len()),
            data,
            owner: ario_ant::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pda
}

#[tokio::test]
async fn test_admin_close_orphaned_rejects_when_asset_alive() {
    // The asset has NOT been burned — admin_close_orphaned must refuse
    // with AssetStillExists (after passing the new authority gate).
    let owner = Keypair::new();
    let authority = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund_user(&mut pt, &owner.pubkey());
    fund_user(&mut pt, &authority.pubkey());
    let migration_config = seed_migration_config(&mut pt, &authority.pubkey());
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let (config_key, _) = config_pda(&asset.pubkey());
    let (controllers_key, _) = controllers_pda(&asset.pubkey());
    let accounts = ario_ant::accounts::AdminCloseOrphanedAntState {
        asset: asset.pubkey(),
        migration_config,
        config: config_key,
        controllers: controllers_key,
        authority: authority.pubkey(),
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::AdminCloseOrphanedAntState {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&authority.pubkey()),
        &[&authority],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, AntError::AssetStillExists);
}

/// Audit fix: admin_close_orphaned_ant_state previously had no authority
/// gate — any signer could route per-ANT rent to themselves. The
/// `has_one = authority @ Unauthorized` constraint added in this fix
/// must reject non-authority signers BEFORE the AssetStillExists check.
/// Without this regression test, the original bug would silently
/// re-enter on any refactor.
#[tokio::test]
async fn test_admin_close_orphaned_rejects_non_authority() {
    let owner = Keypair::new();
    let real_authority = Keypair::new();
    let attacker = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund_user(&mut pt, &owner.pubkey());
    fund_user(&mut pt, &real_authority.pubkey());
    fund_user(&mut pt, &attacker.pubkey());
    let migration_config = seed_migration_config(&mut pt, &real_authority.pubkey());
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let (config_key, _) = config_pda(&asset.pubkey());
    let (controllers_key, _) = controllers_pda(&asset.pubkey());
    let accounts = ario_ant::accounts::AdminCloseOrphanedAntState {
        asset: asset.pubkey(),
        migration_config,
        config: config_key,
        controllers: controllers_key,
        authority: attacker.pubkey(), // ← NOT real_authority
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::AdminCloseOrphanedAntState {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&attacker.pubkey()),
        &[&attacker],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, AntError::Unauthorized);
}

/// Helper: rewrite `asset` to the post-burn state (System-owned, empty)
/// so `admin_close_orphaned_ant_state` passes the AssetStillExists check
/// and reaches the remaining_accounts close loop.
async fn set_asset_post_burn(ctx: &mut ProgramTestContext, asset: &Pubkey) {
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let post_burn = solana_sdk::account::Account {
        lamports: rent.minimum_balance(0),
        data: vec![],
        owner: solana_sdk::system_program::ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(asset, &post_burn.into());
}

/// Helper: seed a minimal `AntRecord`-shaped account for `mint` at
/// `addr`. Only the 8-byte discriminator + 32-byte `mint` field matter —
/// the close loop reads those raw and never borsh-deserializes the rest.
async fn seed_raw_ant_record(ctx: &mut ProgramTestContext, addr: &Pubkey, mint: &Pubkey) {
    use anchor_lang::Discriminator;
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let mut data = ario_ant::state::AntRecord::DISCRIMINATOR.to_vec();
    data.extend_from_slice(mint.as_ref());
    data.resize(ario_ant::state::AntRecord::SIZE, 0);
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: ario_ant::ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(addr, &account.into());
}

/// Audit follow-up (Codex finding #3): the remaining_accounts close loop
/// closes AntRecord / AntRecordMetadata by discriminator. Even though the
/// ix is now authority-gated, the migration authority must not be able to
/// (accidentally) close a record belonging to a DIFFERENT asset and
/// refund its rent here. Each record's stored `mint` must equal
/// `asset.key()`, else OrphanRecordAssetMismatch — the whole tx reverts.
#[tokio::test]
async fn test_admin_close_orphaned_rejects_foreign_record() {
    let owner = Keypair::new();
    let authority = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund_user(&mut pt, &owner.pubkey());
    fund_user(&mut pt, &authority.pubkey());
    let migration_config = seed_migration_config(&mut pt, &authority.pubkey());
    let mut ctx = pt.start_with_context().await;
    // Real config + controllers for `asset`, then transition asset to
    // post-burn so the handler reaches the close loop.
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;
    set_asset_post_burn(&mut ctx, &asset.pubkey()).await;

    // A record for a DIFFERENT asset, wrongly included in the cleanup.
    let foreign_mint = Pubkey::new_unique();
    let (foreign_record, _) = record_pda(&foreign_mint, "sub");
    seed_raw_ant_record(&mut ctx, &foreign_record, &foreign_mint).await;

    let (config_key, _) = config_pda(&asset.pubkey());
    let (controllers_key, _) = controllers_pda(&asset.pubkey());
    let accounts = ario_ant::accounts::AdminCloseOrphanedAntState {
        asset: asset.pubkey(),
        migration_config,
        config: config_key,
        controllers: controllers_key,
        authority: authority.pubkey(),
    };
    let mut metas = accounts.to_account_metas(None);
    metas.push(solana_sdk::instruction::AccountMeta::new(
        foreign_record,
        false,
    ));
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: metas,
        data: ario_ant::instruction::AdminCloseOrphanedAntState {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&authority.pubkey()),
        &[&authority],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, AntError::OrphanRecordAssetMismatch);

    // The foreign record must be untouched (tx reverted), not drained.
    let still_there = ctx.banks_client.get_account(foreign_record).await.unwrap();
    assert!(still_there.is_some(), "foreign record must NOT be closed");
}

/// Happy-path counterpart: a record whose `mint == asset` is a legitimate
/// orphan and IS closed (rent refunded to the authority). Guards against
/// the mint-binding check being too strict / wrong byte offset, which no
/// existing test would otherwise catch.
#[tokio::test]
async fn test_admin_close_orphaned_closes_matching_record() {
    let owner = Keypair::new();
    let authority = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund_user(&mut pt, &owner.pubkey());
    fund_user(&mut pt, &authority.pubkey());
    let migration_config = seed_migration_config(&mut pt, &authority.pubkey());
    let mut ctx = pt.start_with_context().await;
    initialize_default_ant(&mut ctx, &asset.pubkey(), &owner).await;
    set_asset_post_burn(&mut ctx, &asset.pubkey()).await;

    // A record that genuinely belongs to `asset` (mint == asset).
    let (orphan_record, _) = record_pda(&asset.pubkey(), "sub");
    seed_raw_ant_record(&mut ctx, &orphan_record, &asset.pubkey()).await;

    let (config_key, _) = config_pda(&asset.pubkey());
    let (controllers_key, _) = controllers_pda(&asset.pubkey());
    let accounts = ario_ant::accounts::AdminCloseOrphanedAntState {
        asset: asset.pubkey(),
        migration_config,
        config: config_key,
        controllers: controllers_key,
        authority: authority.pubkey(),
    };
    let mut metas = accounts.to_account_metas(None);
    metas.push(solana_sdk::instruction::AccountMeta::new(
        orphan_record,
        false,
    ));
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: metas,
        data: ario_ant::instruction::AdminCloseOrphanedAntState {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&authority.pubkey()),
        &[&authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // The matching record was closed (drained to 0 lamports → GC'd).
    let closed = ctx.banks_client.get_account(orphan_record).await.unwrap();
    assert!(closed.is_none(), "matching record should have been closed");
}
