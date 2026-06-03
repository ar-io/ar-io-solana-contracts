//! Integration tests for `#[event]` emission across the ario-ant program
//! (PR-3 of EVENT_EMISSION_IMPLEMENTATION_PLAN).
//!
//! BPF-only (the PR-0 helpers rely on `sol_log_data`, which
//! `solana-program-test` 2.1.0 does not stub under native dispatch).
//! Each test starts with `bpf_required!()` and skips cleanly when
//! `BPF_OUT_DIR` is unset.
//!
//! We avoid pulling in the `tests/sync_attributes.rs` MPL Core fixture
//! requirement here — the `transfer` ix CPIs into MPL Core and lives in
//! its own file (`tests/transfer_event.rs`) alongside the existing CPI
//! coverage. Everything in this file works against the fake asset blob
//! built by `create_fake_asset_data`.

use anchor_lang::{AccountDeserialize, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_ant::state::*;
use ario_ant::{
    AclEntryAddedEvent, AclEntryRemovedEvent, AntMetadataUpdatedEvent, AntReconciledEvent,
    ControllerAddedEvent, ControllerRemovedEvent, InitializeAntParams, RecordMetadataPrunedEvent,
    RecordMetadataRemovedEvent, RecordMetadataUpdatedEvent, RecordRemovedEvent, RecordSetEvent,
    RecordTransferredEvent, SetRecordMetadataParams, SetRecordParams, ACL_ROLE_CONTROLLER,
    ACL_ROLE_OWNER, ANT_METADATA_FIELD_DESCRIPTION, ANT_METADATA_FIELD_KEYWORDS,
    ANT_METADATA_FIELD_LOGO, ANT_METADATA_FIELD_NAME, ANT_METADATA_FIELD_TICKER,
    RECORD_METADATA_FIELD_ALL,
};
use ario_test_utils::{bpf_required, expect_event};

/// Metaplex Core program ID.
const MPL_CORE_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

/// Bridge Anchor's tied lifetimes to ProcessInstruction's independent
/// lifetimes. Standard pattern across the ario-ant test files.
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

fn create_fake_asset_data(owner: &Pubkey) -> Vec<u8> {
    let mut data = vec![0u8; 33];
    data[0] = 1; // AssetV1 key
    data[1..33].copy_from_slice(owner.as_ref());
    data
}

/// Build a `ProgramTest` with a fake MPL Core asset and an optional
/// pre-funded payer. Mirrors `program_test_with_asset` in
/// `integration.rs`.
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

fn fund(pt: &mut ProgramTest, who: &Pubkey, lamports: u64) {
    pt.add_account(
        *who,
        solana_sdk::account::Account {
            lamports,
            data: vec![],
            owner: solana_sdk::system_program::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
}

// ---------- PDA helpers ----------

fn config_pda(asset: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[ANT_CONFIG_SEED, asset.as_ref()], &ario_ant::ID).0
}
fn controllers_pda(asset: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[ANT_CONTROLLERS_SEED, asset.as_ref()], &ario_ant::ID).0
}
fn record_pda(asset: &Pubkey, undername: &str) -> Pubkey {
    Pubkey::find_program_address(
        &[ANT_RECORD_SEED, asset.as_ref(), &hash_undername(undername)],
        &ario_ant::ID,
    )
    .0
}
fn record_metadata_pda(asset: &Pubkey, undername: &str) -> Pubkey {
    Pubkey::find_program_address(
        &[
            ANT_RECORD_META_SEED,
            asset.as_ref(),
            &hash_undername(undername),
        ],
        &ario_ant::ID,
    )
    .0
}
fn acl_config_pda(user: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[ACL_CONFIG_SEED, user.as_ref()], &ario_ant::ID).0
}
fn acl_page_pda(user: &Pubkey, page_idx: u64) -> Pubkey {
    Pubkey::find_program_address(
        &[ACL_PAGE_SEED, user.as_ref(), &page_idx.to_le_bytes()],
        &ario_ant::ID,
    )
    .0
}

// ---------- Instruction senders ----------

async fn process_with_logs(
    ctx: &mut ProgramTestContext,
    ixs: &[Instruction],
    payer: &Keypair,
) -> Vec<String> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(ixs, Some(&payer.pubkey()), &[payer], blockhash);
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .expect("transaction should land");
    assert!(
        result.result.is_ok(),
        "tx must succeed: {:?}",
        result.result
    );
    result.metadata.expect("metadata").log_messages
}

async fn initialize_ant(ctx: &mut ProgramTestContext, asset: &Pubkey, owner: &Keypair) {
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::InitializeAnt {
            asset: *asset,
            ant_config: config_pda(asset),
            ant_controllers: controllers_pda(asset),
            root_record: record_pda(asset, "@"),
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
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&owner.pubkey()), &[owner], blockhash);
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

fn set_record_ix(asset: &Pubkey, caller: &Pubkey, params: SetRecordParams) -> Instruction {
    let record = record_pda(asset, &params.undername);
    Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::SetRecord {
            asset: *asset,
            ant_config: config_pda(asset),
            ant_controllers: controllers_pda(asset),
            record,
            caller: *caller,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::SetRecord { params }.data(),
    }
}

fn manage_metadata_accounts(
    asset: &Pubkey,
    caller: &Pubkey,
) -> Vec<solana_sdk::instruction::AccountMeta> {
    ario_ant::accounts::ManageMetadata {
        asset: *asset,
        ant_config: config_pda(asset),
        ant_controllers: controllers_pda(asset),
        caller: *caller,
    }
    .to_account_metas(None)
}

fn ensure_acl_state_ixs(
    user: &Pubkey,
    payer: &Pubkey,
    config_exists: bool,
    page_exists: bool,
) -> Vec<Instruction> {
    let mut ixs = Vec::new();
    if !config_exists {
        ixs.push(Instruction {
            program_id: ario_ant::ID,
            accounts: ario_ant::accounts::RegisterAclConfig {
                acl_config: acl_config_pda(user),
                payer: *payer,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: ario_ant::instruction::RegisterAclConfig { user: *user }.data(),
        });
    }
    if !page_exists {
        ixs.push(Instruction {
            program_id: ario_ant::ID,
            accounts: ario_ant::accounts::AddAclPage {
                acl_config: acl_config_pda(user),
                acl_page: acl_page_pda(user, 0),
                payer: *payer,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: ario_ant::instruction::AddAclPage {}.data(),
        });
    }
    ixs
}

async fn account_exists(ctx: &mut ProgramTestContext, key: &Pubkey) -> bool {
    ctx.banks_client.get_account(*key).await.unwrap().is_some()
}

// ============================================================================
// TESTS
// ============================================================================

#[tokio::test]
async fn test_set_record_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let params = SetRecordParams {
        undername: "blog".to_string(),
        target: "b".repeat(43),
        target_protocol: PROTOCOL_ARWEAVE,
        ttl_seconds: 3600,
        priority: Some(7),
        record_owner: None,
    };
    let ix = set_record_ix(&asset.pubkey(), &owner.pubkey(), params);
    let logs = process_with_logs(&mut ctx, &[ix], &owner).await;

    let ev = expect_event!(&logs, RecordSetEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.caller, owner.pubkey());
    assert_eq!(ev.undername, "blog");
    assert_eq!(ev.target, "b".repeat(43));
    assert_eq!(ev.target_protocol, PROTOCOL_ARWEAVE);
    assert_eq!(ev.ttl_seconds, 3600);
    assert_eq!(ev.priority, 7);
}

#[tokio::test]
async fn test_remove_record_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record first.
    let create = set_record_ix(
        &asset.pubkey(),
        &owner.pubkey(),
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 3600,
            priority: None,
            record_owner: None,
        },
    );
    process_with_logs(&mut ctx, &[create], &owner).await;

    // Now remove. metadata isn't set, so pass None.
    let remove = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RemoveRecord {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            record: record_pda(&asset.pubkey(), "blog"),
            record_metadata: None,
            caller: owner.pubkey(),
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RemoveRecord {}.data(),
    };
    let logs = process_with_logs(&mut ctx, &[remove], &owner).await;

    let ev = expect_event!(&logs, RecordRemovedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.caller, owner.pubkey());
    assert_eq!(ev.undername, "blog");
}

#[tokio::test]
async fn test_transfer_record_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create a record. No record_owner.
    let create = set_record_ix(
        &asset.pubkey(),
        &owner.pubkey(),
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 3600,
            priority: None,
            record_owner: None,
        },
    );
    process_with_logs(&mut ctx, &[create], &owner).await;

    let new_owner = Pubkey::new_unique();
    let xfer = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::TransferRecord {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            record: record_pda(&asset.pubkey(), "blog"),
            caller: owner.pubkey(),
        }
        .to_account_metas(None),
        data: ario_ant::instruction::TransferRecord { new_owner }.data(),
    };
    let logs = process_with_logs(&mut ctx, &[xfer], &owner).await;

    let ev = expect_event!(&logs, RecordTransferredEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.caller, owner.pubkey());
    assert_eq!(ev.undername, "blog");
    // Record had no prior owner.
    assert_eq!(ev.previous_owner, None);
    assert_eq!(ev.new_owner, new_owner);
}

#[tokio::test]
async fn test_add_controller_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    let controller = Pubkey::new_unique();
    let acl_cfg_exists = account_exists(&mut ctx, &acl_config_pda(&controller)).await;
    let acl_page_exists = account_exists(&mut ctx, &acl_page_pda(&controller, 0)).await;
    let mut ixs = ensure_acl_state_ixs(
        &controller,
        &owner.pubkey(),
        acl_cfg_exists,
        acl_page_exists,
    );

    ixs.push(Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::AddController {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            controller_acl_config: acl_config_pda(&controller),
            controller_acl_page: acl_page_pda(&controller, 0),
            caller: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::AddController { controller }.data(),
    });

    let logs = process_with_logs(&mut ctx, &ixs, &owner).await;

    let ev = expect_event!(&logs, ControllerAddedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.owner, owner.pubkey());
    assert_eq!(ev.controller, controller);
    // Note: `add_controller` writes the ACL entry inline (not via
    // `record_acl_controller_handler`), so it does NOT emit an
    // `AclEntryAddedEvent`. The dedicated `record_acl_controller`
    // ix is the canonical event source — covered by
    // `test_record_acl_controller_emits_event`.
}

#[tokio::test]
async fn test_remove_controller_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Add the controller first (consume its events; we only care about
    // the remove path here).
    let controller = Pubkey::new_unique();
    let mut ixs = ensure_acl_state_ixs(&controller, &owner.pubkey(), false, false);
    ixs.push(Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::AddController {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            controller_acl_config: acl_config_pda(&controller),
            controller_acl_page: acl_page_pda(&controller, 0),
            caller: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::AddController { controller }.data(),
    });
    process_with_logs(&mut ctx, &ixs, &owner).await;

    // Now remove.
    let rm = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RemoveController {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            controller_acl_config: acl_config_pda(&controller),
            controller_acl_page: acl_page_pda(&controller, 0),
            caller: owner.pubkey(),
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RemoveController { controller }.data(),
    };
    let logs = process_with_logs(&mut ctx, &[rm], &owner).await;

    let ev = expect_event!(&logs, ControllerRemovedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.owner, owner.pubkey());
    assert_eq!(ev.controller, controller);
    // Note: `remove_controller` writes the ACL diff inline; the
    // dedicated `remove_acl_controller` ix is the canonical
    // `AclEntryRemovedEvent` source.
}

/// Drives every metadata setter and asserts each emits with the right
/// `field` discriminator. One tx per setter so logs stay clean.
#[tokio::test]
async fn test_metadata_setters_emit_events() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // set_name
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: manage_metadata_accounts(&asset.pubkey(), &owner.pubkey()),
        data: ario_ant::instruction::SetName {
            name: "New Name".to_string(),
        }
        .data(),
    };
    let logs = process_with_logs(&mut ctx, &[ix], &owner).await;
    let ev = expect_event!(&logs, AntMetadataUpdatedEvent);
    assert_eq!(ev.field, ANT_METADATA_FIELD_NAME);
    assert_eq!(ev.caller, owner.pubkey());
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.new_value, "New Name");

    // set_ticker
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: manage_metadata_accounts(&asset.pubkey(), &owner.pubkey()),
        data: ario_ant::instruction::SetTicker {
            ticker: "TICK".to_string(),
        }
        .data(),
    };
    let logs = process_with_logs(&mut ctx, &[ix], &owner).await;
    let ev = expect_event!(&logs, AntMetadataUpdatedEvent);
    assert_eq!(ev.field, ANT_METADATA_FIELD_TICKER);
    assert_eq!(ev.new_value, "TICK");

    // set_description
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: manage_metadata_accounts(&asset.pubkey(), &owner.pubkey()),
        data: ario_ant::instruction::SetDescription {
            description: "new description".to_string(),
        }
        .data(),
    };
    let logs = process_with_logs(&mut ctx, &[ix], &owner).await;
    let ev = expect_event!(&logs, AntMetadataUpdatedEvent);
    assert_eq!(ev.field, ANT_METADATA_FIELD_DESCRIPTION);
    assert_eq!(ev.new_value, "new description");

    // set_keywords (joined with comma in the event payload)
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: manage_metadata_accounts(&asset.pubkey(), &owner.pubkey()),
        data: ario_ant::instruction::SetKeywords {
            keywords: vec!["solana".to_string(), "ant".to_string()],
        }
        .data(),
    };
    let logs = process_with_logs(&mut ctx, &[ix], &owner).await;
    let ev = expect_event!(&logs, AntMetadataUpdatedEvent);
    assert_eq!(ev.field, ANT_METADATA_FIELD_KEYWORDS);
    assert_eq!(ev.new_value, "solana,ant");

    // set_logo
    let logo_value = "c".repeat(43);
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: manage_metadata_accounts(&asset.pubkey(), &owner.pubkey()),
        data: ario_ant::instruction::SetLogo {
            logo: logo_value.clone(),
        }
        .data(),
    };
    let logs = process_with_logs(&mut ctx, &[ix], &owner).await;
    let ev = expect_event!(&logs, AntMetadataUpdatedEvent);
    assert_eq!(ev.field, ANT_METADATA_FIELD_LOGO);
    assert_eq!(ev.new_value, logo_value);
}

#[tokio::test]
async fn test_reconcile_emits_event_only_on_ownership_change() {
    bpf_required!();
    let owner = Keypair::new();
    let new_owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    fund(&mut pt, &stranger.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Reconcile with no ownership change — must NOT emit.
    let noop_ix = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::Reconcile {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            caller: stranger.pubkey(),
        }
        .to_account_metas(None),
        data: ario_ant::instruction::Reconcile {}.data(),
    };
    let logs = process_with_logs(&mut ctx, &[noop_ix.clone()], &stranger).await;
    ario_test_utils::assert_no_event!(&logs, AntReconciledEvent);

    // Simulate an out-of-band MPL Core transfer by overwriting the
    // asset's owner field.
    let new_data = create_fake_asset_data(&new_owner.pubkey());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    ctx.set_account(
        &asset.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(new_data.len()),
            data: new_data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Advance the slot so banks-client returns a fresh blockhash —
    // otherwise the second `reconcile` tx is `AlreadyProcessed`.
    ctx.warp_to_slot(ctx.banks_client.get_root_slot().await.unwrap() + 100)
        .unwrap();

    // Reconcile picks it up — must emit, and `controllers_cleared = true`.
    let logs = process_with_logs(&mut ctx, &[noop_ix], &stranger).await;
    let ev = expect_event!(&logs, AntReconciledEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.previous_owner, owner.pubkey());
    assert_eq!(ev.new_owner, new_owner.pubkey());
    assert!(ev.controllers_cleared);
}

#[tokio::test]
async fn test_set_record_metadata_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Need a core record before metadata can attach.
    let create = set_record_ix(
        &asset.pubkey(),
        &owner.pubkey(),
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 3600,
            priority: None,
            record_owner: None,
        },
    );
    process_with_logs(&mut ctx, &[create], &owner).await;

    let params = SetRecordMetadataParams {
        undername: "blog".to_string(),
        display_name: Some("My Blog".to_string()),
        record_logo: None,
        record_description: None,
        record_keywords: None,
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::SetRecordMetadata {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            record: record_pda(&asset.pubkey(), "blog"),
            record_metadata: record_metadata_pda(&asset.pubkey(), "blog"),
            caller: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::SetRecordMetadata { params }.data(),
    };
    let logs = process_with_logs(&mut ctx, &[ix], &owner).await;

    let ev = expect_event!(&logs, RecordMetadataUpdatedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.caller, owner.pubkey());
    assert_eq!(ev.undername, "blog");
    assert_eq!(ev.field, RECORD_METADATA_FIELD_ALL);
}

#[tokio::test]
async fn test_remove_record_metadata_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Stand up a record + metadata.
    let create = set_record_ix(
        &asset.pubkey(),
        &owner.pubkey(),
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 3600,
            priority: None,
            record_owner: None,
        },
    );
    process_with_logs(&mut ctx, &[create], &owner).await;
    let set_meta = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::SetRecordMetadata {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            record: record_pda(&asset.pubkey(), "blog"),
            record_metadata: record_metadata_pda(&asset.pubkey(), "blog"),
            caller: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::SetRecordMetadata {
            params: SetRecordMetadataParams {
                undername: "blog".to_string(),
                display_name: Some("dn".to_string()),
                record_logo: None,
                record_description: None,
                record_keywords: None,
            },
        }
        .data(),
    };
    process_with_logs(&mut ctx, &[set_meta], &owner).await;

    // Now remove metadata.
    let rm = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RemoveRecordMetadata {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            record_metadata: record_metadata_pda(&asset.pubkey(), "blog"),
            caller: owner.pubkey(),
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RemoveRecordMetadata {
            undername: "blog".to_string(),
        }
        .data(),
    };
    let logs = process_with_logs(&mut ctx, &[rm], &owner).await;

    let ev = expect_event!(&logs, RecordMetadataRemovedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.caller, owner.pubkey());
    assert_eq!(ev.undername, "blog");
}

#[tokio::test]
async fn test_close_orphaned_record_metadata_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    fund(&mut pt, &stranger.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Create record + metadata.
    let create = set_record_ix(
        &asset.pubkey(),
        &owner.pubkey(),
        SetRecordParams {
            undername: "blog".to_string(),
            target: test_arweave_id(),
            target_protocol: PROTOCOL_ARWEAVE,
            ttl_seconds: 3600,
            priority: None,
            record_owner: None,
        },
    );
    process_with_logs(&mut ctx, &[create], &owner).await;
    let set_meta = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::SetRecordMetadata {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            record: record_pda(&asset.pubkey(), "blog"),
            record_metadata: record_metadata_pda(&asset.pubkey(), "blog"),
            caller: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::SetRecordMetadata {
            params: SetRecordMetadataParams {
                undername: "blog".to_string(),
                display_name: Some("dn".to_string()),
                record_logo: None,
                record_description: None,
                record_keywords: None,
            },
        }
        .data(),
    };
    process_with_logs(&mut ctx, &[set_meta], &owner).await;

    // Forgetfully remove the record without closing the metadata
    // sibling — leaves the metadata as an orphan.
    let rm_record = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RemoveRecord {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            record: record_pda(&asset.pubkey(), "blog"),
            record_metadata: None,
            caller: owner.pubkey(),
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RemoveRecord {}.data(),
    };
    process_with_logs(&mut ctx, &[rm_record], &owner).await;

    // Stranger reclaims rent permissionlessly.
    let prune = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::CloseOrphanedRecordMetadata {
            asset: asset.pubkey(),
            record: record_pda(&asset.pubkey(), "blog"),
            record_metadata: record_metadata_pda(&asset.pubkey(), "blog"),
            caller: stranger.pubkey(),
        }
        .to_account_metas(None),
        data: ario_ant::instruction::CloseOrphanedRecordMetadata {
            undername: "blog".to_string(),
        }
        .data(),
    };
    let logs = process_with_logs(&mut ctx, &[prune], &stranger).await;

    let ev = expect_event!(&logs, RecordMetadataPrunedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.undername, "blog");
    assert_eq!(ev.pruner, stranger.pubkey());
}

#[tokio::test]
async fn test_record_acl_owner_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    fund(&mut pt, &stranger.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Ensure owner has an AclConfig + first page.
    let mut ixs = ensure_acl_state_ixs(&owner.pubkey(), &stranger.pubkey(), false, false);
    ixs.push(Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RecordAclOwner {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            acl_config: acl_config_pda(&owner.pubkey()),
            acl_page: acl_page_pda(&owner.pubkey(), 0),
            payer: stranger.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RecordAclOwner {}.data(),
    });
    let logs = process_with_logs(&mut ctx, &ixs, &stranger).await;

    let ev = expect_event!(&logs, AclEntryAddedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.address, owner.pubkey());
    assert_eq!(ev.role, ACL_ROLE_OWNER);
}

/// Regression: the permissionless owner-record path must reject a Metaplex
/// Core asset that ario-ant never initialized as an ANT. Without the
/// `ant_config` PDA constraint on `RecordAclOwner`, an attacker could record
/// arbitrary Core assets owned by a victim into the victim's ACL, spoofing
/// the reverse index. Here the asset is owned by `victim` but has no
/// `AntConfig`, so the record must fail and the ACL must stay empty.
#[tokio::test]
async fn test_record_acl_owner_rejects_non_ant_asset() {
    bpf_required!();
    let victim = Keypair::new();
    let attacker = Keypair::new();
    // Asset owned by the victim but deliberately NOT initialized as an ANT —
    // note the absence of an `initialize_ant` call below.
    let (mut pt, asset) = program_test_with_asset(&victim.pubkey());
    fund(&mut pt, &victim.pubkey(), 10_000_000_000);
    fund(&mut pt, &attacker.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;

    // The asset's AntConfig PDA must not exist (no ANT initialization).
    assert!(
        !account_exists(&mut ctx, &config_pda(&asset.pubkey())).await,
        "precondition: non-ANT asset has no AntConfig"
    );

    // Stand up the victim's ACL config + page first (permissionless). This
    // tx MUST succeed so the only thing left that can fail below is the
    // record guard itself — not an unrelated abort in ACL setup.
    let setup = ensure_acl_state_ixs(&victim.pubkey(), &attacker.pubkey(), false, false);
    process_with_logs(&mut ctx, &setup, &attacker).await;

    // Now attempt the spoof on its own: record the non-ANT asset as owned.
    // Single-instruction tx, so any failure is unambiguously this ix.
    let record = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RecordAclOwner {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            acl_config: acl_config_pda(&victim.pubkey()),
            acl_page: acl_page_pda(&victim.pubkey(), 0),
            payer: attacker.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RecordAclOwner {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[record],
        Some(&attacker.pubkey()),
        &[&attacker],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .expect("transaction should land");

    // Anchor evaluates the `ant_config` account constraint during `Accounts`
    // deserialization, before the handler body runs. A non-existent AntConfig
    // PDA (owner = system program, 0 lamports) fails `Account::try_from` with
    // `AccountNotInitialized` (3012) on instruction 0. Pin to that exact
    // failure so this regression tracks the new guard, not any other abort.
    assert_eq!(
        result
            .result
            .expect_err("recording a non-ANT asset must fail"),
        solana_sdk::transaction::TransactionError::InstructionError(
            0,
            solana_sdk::instruction::InstructionError::Custom(
                anchor_lang::error::ErrorCode::AccountNotInitialized as u32,
            ),
        ),
        "expected AccountNotInitialized (3012) on the missing ant_config PDA"
    );

    // The guard fired before any ACL mutation: the page exists (from setup)
    // but holds no entries — the spoof asset was never recorded.
    let page_acct = ctx
        .banks_client
        .get_account(acl_page_pda(&victim.pubkey(), 0))
        .await
        .unwrap()
        .expect("acl page should exist from setup");
    let page = AclPage::try_deserialize(&mut page_acct.data.as_slice()).unwrap();
    assert!(
        page.entries.is_empty(),
        "spoof asset must not be recorded — page must stay empty, got {:?}",
        page.entries
    );
}

#[tokio::test]
async fn test_remove_acl_owner_emits_event() {
    bpf_required!();
    let original_owner = Keypair::new();
    let new_owner = Keypair::new();
    let stranger = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&original_owner.pubkey());
    fund(&mut pt, &original_owner.pubkey(), 10_000_000_000);
    fund(&mut pt, &stranger.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &original_owner).await;

    // Record the owner ACL entry first.
    let mut ixs = ensure_acl_state_ixs(&original_owner.pubkey(), &stranger.pubkey(), false, false);
    ixs.push(Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RecordAclOwner {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            acl_config: acl_config_pda(&original_owner.pubkey()),
            acl_page: acl_page_pda(&original_owner.pubkey(), 0),
            payer: stranger.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RecordAclOwner {}.data(),
    });
    process_with_logs(&mut ctx, &ixs, &stranger).await;

    // Simulate transfer (asset blob owner field flips to new_owner).
    let new_data = create_fake_asset_data(&new_owner.pubkey());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    ctx.set_account(
        &asset.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(new_data.len()),
            data: new_data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Now remove_acl_owner — verifies owner != acl_config.user.
    let rm = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RecordAclOwner {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            acl_config: acl_config_pda(&original_owner.pubkey()),
            acl_page: acl_page_pda(&original_owner.pubkey(), 0),
            payer: stranger.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RemoveAclOwner {}.data(),
    };
    let logs = process_with_logs(&mut ctx, &[rm], &stranger).await;

    let ev = expect_event!(&logs, AclEntryRemovedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.address, original_owner.pubkey());
    assert_eq!(ev.role, ACL_ROLE_OWNER);
}

#[tokio::test]
async fn test_record_acl_controller_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Owner is in the controllers list by default (initialize installs
    // them). RecordAclController validates membership against
    // AntControllers, so we can target the owner directly.
    let mut ixs = ensure_acl_state_ixs(&owner.pubkey(), &owner.pubkey(), false, false);
    ixs.push(Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RecordAclController {
            asset: asset.pubkey(),
            ant_controllers: controllers_pda(&asset.pubkey()),
            acl_config: acl_config_pda(&owner.pubkey()),
            acl_page: acl_page_pda(&owner.pubkey(), 0),
            payer: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RecordAclController {}.data(),
    });
    let logs = process_with_logs(&mut ctx, &ixs, &owner).await;

    let ev = expect_event!(&logs, AclEntryAddedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.address, owner.pubkey());
    assert_eq!(ev.role, ACL_ROLE_CONTROLLER);
}

#[tokio::test]
async fn test_remove_acl_controller_emits_event() {
    bpf_required!();
    let owner = Keypair::new();
    let (mut pt, asset) = program_test_with_asset(&owner.pubkey());
    fund(&mut pt, &owner.pubkey(), 10_000_000_000);
    let mut ctx = pt.start_with_context().await;
    initialize_ant(&mut ctx, &asset.pubkey(), &owner).await;

    // Stage a stale-ACL scenario:
    //   1. record_acl_controller writes an entry against the owner
    //      (who initialize() inserted into AntControllers).
    //   2. Simulate an out-of-band MPL Core transfer to a new owner.
    //   3. reconcile clears AntControllers — now the owner is no
    //      longer a controller, but their ACL entry still says they
    //      are. That's the stale state remove_acl_controller fixes.
    let mut ixs = ensure_acl_state_ixs(&owner.pubkey(), &owner.pubkey(), false, false);
    ixs.push(Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RecordAclController {
            asset: asset.pubkey(),
            ant_controllers: controllers_pda(&asset.pubkey()),
            acl_config: acl_config_pda(&owner.pubkey()),
            acl_page: acl_page_pda(&owner.pubkey(), 0),
            payer: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RecordAclController {}.data(),
    });
    process_with_logs(&mut ctx, &ixs, &owner).await;

    // Out-of-band ownership change.
    let new_owner = Keypair::new();
    let new_data = create_fake_asset_data(&new_owner.pubkey());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    ctx.set_account(
        &asset.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(new_data.len()),
            data: new_data,
            owner: MPL_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Reconcile clears AntControllers, leaving the owner's ACL entry
    // stale.
    let reconcile = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::Reconcile {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            caller: owner.pubkey(),
        }
        .to_account_metas(None),
        data: ario_ant::instruction::Reconcile {}.data(),
    };
    process_with_logs(&mut ctx, &[reconcile], &owner).await;

    // remove_acl_controller succeeds (user is no longer in AntControllers).
    let rm = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::RecordAclController {
            asset: asset.pubkey(),
            ant_controllers: controllers_pda(&asset.pubkey()),
            acl_config: acl_config_pda(&owner.pubkey()),
            acl_page: acl_page_pda(&owner.pubkey(), 0),
            payer: owner.pubkey(),
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::RemoveAclController {}.data(),
    };
    let logs = process_with_logs(&mut ctx, &[rm], &owner).await;

    let ev = expect_event!(&logs, AclEntryRemovedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.address, owner.pubkey());
    assert_eq!(ev.role, ACL_ROLE_CONTROLLER);
}
