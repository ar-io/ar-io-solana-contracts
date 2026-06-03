//! Integration test for the wrapped `transfer` instruction's
//! `AntTransferredEvent` emission (PR-3 of EVENT_EMISSION_IMPLEMENTATION_PLAN).
//!
//! Lives in its own file alongside `sync_attributes.rs` because both
//! CPI into MPL Core, which requires the BPF fixture:
//!
//!   1. `cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/`
//!   2. `BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-ant`
//!
//! Without those, `add_program("mpl_core", ...)` falls back to a stale
//! .so and the inner CPI fails with AccountNotExecutable. The
//! `bpf_required!()` guard skips this test cleanly when `BPF_OUT_DIR`
//! is unset (matching the rest of the event suite).

use anchor_lang::{InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_ant::state::*;
use ario_ant::{AntTransferredEvent, InitializeAntParams};
use ario_test_utils::{bpf_required, expect_event};

const MPL_CORE_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

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

fn program_test() -> ProgramTest {
    let mut pt = ProgramTest::new("ario_ant", ario_ant::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);
    // Real MPL Core BPF — without this, the inner transferV1 CPI fails
    // with AccountNotExecutable. Mirrors `sync_attributes.rs::program_test`.
    pt.add_program("mpl_core", MPL_CORE_PROGRAM_ID, None);
    pt
}

/// Mint a real MPL Core asset (no plugins) — the wrapped `transfer`
/// path doesn't touch the Attributes plugin, so we keep the wire
/// payload minimal.
async fn mint_test_ant(ctx: &mut ProgramTestContext, asset: &Keypair) {
    let name = b"xfer-test";
    let uri = b"ar://x";
    let mut data = Vec::<u8>::new();
    data.push(0); // CreateV1 discriminator
    data.push(0); // AccountState::Uncompressed
    data.extend_from_slice(&(name.len() as u32).to_le_bytes());
    data.extend_from_slice(name);
    data.extend_from_slice(&(uri.len() as u32).to_le_bytes());
    data.extend_from_slice(uri);
    data.push(0); // plugins None

    let placeholder = MPL_CORE_PROGRAM_ID;
    let metas = vec![
        AccountMeta::new(asset.pubkey(), true),
        AccountMeta::new_readonly(placeholder, false), // collection (None)
        AccountMeta::new_readonly(ctx.payer.pubkey(), true), // authority
        AccountMeta::new(ctx.payer.pubkey(), true),    // payer
        AccountMeta::new_readonly(placeholder, false), // owner (None → authority)
        AccountMeta::new_readonly(placeholder, false), // updateAuthority (None)
        AccountMeta::new_readonly(system_program::ID, false),
        AccountMeta::new_readonly(placeholder, false), // logWrapper (None)
    ];
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: MPL_CORE_PROGRAM_ID,
            accounts: metas,
            data,
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, asset],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("mint_test_ant: CreateV1 failed (is mpl_core.so loaded? BPF_OUT_DIR set?)");
}

fn config_pda(asset: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[ANT_CONFIG_SEED, asset.as_ref()], &ario_ant::ID).0
}
fn controllers_pda(asset: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[ANT_CONTROLLERS_SEED, asset.as_ref()], &ario_ant::ID).0
}
fn root_record_pda(asset: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[ANT_RECORD_SEED, asset.as_ref(), &hash_undername("@")],
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

#[tokio::test]
async fn transfer_emits_ant_transferred_event() {
    bpf_required!();

    let mut ctx = program_test().start_with_context().await;

    // Mint owned by ctx.payer.
    let asset = Keypair::new();
    mint_test_ant(&mut ctx, &asset).await;
    let payer_pk = ctx.payer.pubkey();
    let payer = ctx.payer.insecure_clone();

    // Initialize the ANT's on-chain state (config + controllers + @).
    let init_ix = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::InitializeAnt {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            root_record: root_record_pda(&asset.pubkey()),
            owner: payer_pk,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::Initialize {
            params: InitializeAntParams {
                name: "Transferable".to_string(),
                ticker: Some("XFER".to_string()),
                target: "a".repeat(43),
                target_protocol: None,
                logo: "a".repeat(43),
                description: String::new(),
                keywords: vec![],
            },
        }
        .data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[init_ix], Some(&payer_pk), &[&payer], blockhash);
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Fund a fresh `new_owner` so they can hold rent and pay ACL setup
    // gas. The wrapped `transfer` ix expects `new_owner_acl_config` to
    // exist — we register it through the permissionless config init.
    let new_owner = Keypair::new();
    let fund_ix =
        solana_sdk::system_instruction::transfer(&payer_pk, &new_owner.pubkey(), 2_000_000_000);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[fund_ix], Some(&payer_pk), &[&payer], blockhash);
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Stand up both owners' AclConfig + AclPage(0). The current owner
    // also needs an ACL entry for the (asset, Owner) pair so the wrapper
    // can `swap_remove` it during transfer; mirrors the post-mint
    // SDK preflight behavior.
    let setup_ixs = vec![
        Instruction {
            program_id: ario_ant::ID,
            accounts: ario_ant::accounts::RegisterAclConfig {
                acl_config: acl_config_pda(&payer_pk),
                payer: payer_pk,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: ario_ant::instruction::RegisterAclConfig { user: payer_pk }.data(),
        },
        Instruction {
            program_id: ario_ant::ID,
            accounts: ario_ant::accounts::AddAclPage {
                acl_config: acl_config_pda(&payer_pk),
                acl_page: acl_page_pda(&payer_pk, 0),
                payer: payer_pk,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: ario_ant::instruction::AddAclPage {}.data(),
        },
        Instruction {
            program_id: ario_ant::ID,
            accounts: ario_ant::accounts::RecordAclOwner {
                asset: asset.pubkey(),
                ant_config: config_pda(&asset.pubkey()),
                acl_config: acl_config_pda(&payer_pk),
                acl_page: acl_page_pda(&payer_pk, 0),
                payer: payer_pk,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: ario_ant::instruction::RecordAclOwner {}.data(),
        },
        Instruction {
            program_id: ario_ant::ID,
            accounts: ario_ant::accounts::RegisterAclConfig {
                acl_config: acl_config_pda(&new_owner.pubkey()),
                payer: payer_pk,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: ario_ant::instruction::RegisterAclConfig {
                user: new_owner.pubkey(),
            }
            .data(),
        },
        Instruction {
            program_id: ario_ant::ID,
            accounts: ario_ant::accounts::AddAclPage {
                acl_config: acl_config_pda(&new_owner.pubkey()),
                acl_page: acl_page_pda(&new_owner.pubkey(), 0),
                payer: payer_pk,
                system_program: system_program::ID,
            }
            .to_account_metas(None),
            data: ario_ant::instruction::AddAclPage {}.data(),
        },
    ];
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&setup_ixs, Some(&payer_pk), &[&payer], blockhash);
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // The actual wrapped transfer.
    let xfer_ix = Instruction {
        program_id: ario_ant::ID,
        accounts: ario_ant::accounts::Transfer {
            asset: asset.pubkey(),
            ant_config: config_pda(&asset.pubkey()),
            ant_controllers: controllers_pda(&asset.pubkey()),
            caller: payer_pk,
            new_owner: new_owner.pubkey(),
            new_owner_acl_config: acl_config_pda(&new_owner.pubkey()),
            new_owner_acl_page: acl_page_pda(&new_owner.pubkey(), 0),
            old_owner_acl_config: acl_config_pda(&payer_pk),
            old_owner_acl_page: acl_page_pda(&payer_pk, 0),
            mpl_core_program: MPL_CORE_PROGRAM_ID,
            system_program: system_program::ID,
        }
        .to_account_metas(None),
        data: ario_ant::instruction::Transfer {}.data(),
    };
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[xfer_ix], Some(&payer_pk), &[&payer], blockhash);
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .expect("wrapped transfer should land");
    assert!(
        result.result.is_ok(),
        "wrapped transfer must succeed: {:?}",
        result.result
    );
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, AntTransferredEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.from, payer_pk);
    assert_eq!(ev.to, new_owner.pubkey());
}
