//! Integration test for `ario_ant::clear_attributes` (the recovery
//! complement of `sync_attributes`).
//!
//! `clear_attributes` exists to close the stale-trait gap left by
//! `release_name` and `reassign_name`: both produce ANT assets whose
//! Attributes plugin no longer matches any live ArnsRecord, and
//! `sync_attributes` cannot fix them (its `record.ant == asset.key()`
//! check fails). `clear_attributes` is the asset-side recovery path —
//! the asset owner clears the ArNS-related traits while preserving the
//! `ANT Program` routing trait.
//!
//! These tests exercise the actual UpdatePluginV1 CPI against a real
//! MPL Core program loaded from the BPF fixture, mirroring the setup
//! `sync_attributes.rs` uses.
//!
//! REQUIRES:
//!   1. `cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/`
//!   2. `BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-ant
//!      --test clear_attributes`

use anchor_lang::{InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_ant::error::AntError;

/// Metaplex Core program ID (matches `ario_ant::MPL_CORE_PROGRAM_ID`).
const MPL_CORE_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

/// Bridge Anchor's tied lifetimes to ProcessInstruction's independent
/// lifetimes. Same trick the other tests use.
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
    pt.add_program("mpl_core", MPL_CORE_PROGRAM_ID, None);
    pt
}

/// Mint a real Metaplex Core asset with an empty Attributes plugin
/// (authority = Owner). Lifted verbatim from sync_attributes test.
async fn mint_test_ant(ctx: &mut ProgramTestContext, asset: &Keypair) {
    mint_test_ant_with_attributes(ctx, asset, &[]).await;
}

/// Mint a real Metaplex Core asset with pre-populated Attributes plugin
/// entries (authority = Owner). Used to plant `ANT Program` at mint time
/// so we can verify clear preserves it through a real UpdatePluginV1 CPI.
async fn mint_test_ant_with_attributes(
    ctx: &mut ProgramTestContext,
    asset: &Keypair,
    attributes: &[(&str, &str)],
) {
    let name = b"clear-test-ant";
    let uri = b"ar://clear";
    let mut data = Vec::<u8>::new();
    data.push(0); // CreateV1 discriminator
    data.push(0); // AccountState::Uncompressed
    data.extend_from_slice(&(name.len() as u32).to_le_bytes());
    data.extend_from_slice(name);
    data.extend_from_slice(&(uri.len() as u32).to_le_bytes());
    data.extend_from_slice(uri);
    data.push(1); // plugins Some
    data.extend_from_slice(&1u32.to_le_bytes()); // 1 plugin
    data.push(6); // Plugin::Attributes
    data.extend_from_slice(&(attributes.len() as u32).to_le_bytes());
    for (k, v) in attributes {
        data.extend_from_slice(&(k.len() as u32).to_le_bytes());
        data.extend_from_slice(k.as_bytes());
        data.extend_from_slice(&(v.len() as u32).to_le_bytes());
        data.extend_from_slice(v.as_bytes());
    }
    data.push(1); // authority Option = Some
    data.push(1); // BasePluginAuthority::Owner

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

/// Compute the canonical ArnsRecord PDA for `name` under ario_arns::ID.
fn arns_record_pda(name: &str) -> Pubkey {
    let h = anchor_lang::solana_program::hash::hash(name.to_lowercase().as_bytes());
    Pubkey::find_program_address(&[b"arns_record", h.as_ref()], &ario_arns::ID).0
}

/// Inject a fake `ArnsRecord` PDA (mirrors sync_attributes test). Used
/// here only to set up "ANT had a name" preconditions before clearing.
async fn inject_arns_record(
    ctx: &mut ProgramTestContext,
    name: &str,
    ant_mint: &Pubkey,
    purchase_type: u8,
    undername_limit: u16,
) -> Pubkey {
    let pda = arns_record_pda(name);

    let mut data = Vec::<u8>::new();
    let disc = anchor_lang::solana_program::hash::hash(b"account:ArnsRecord");
    data.extend_from_slice(&disc.to_bytes()[..8]);
    let name_hash = anchor_lang::solana_program::hash::hash(name.to_lowercase().as_bytes());
    data.extend_from_slice(name_hash.as_ref());
    data.extend_from_slice(&[0u8; 32]);
    data.extend_from_slice(ant_mint.as_ref());
    data.push(purchase_type);
    data.extend_from_slice(&0i64.to_le_bytes());
    data.push(0);
    data.extend_from_slice(&undername_limit.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes());
    data.push(0);
    data.extend_from_slice(&(name.len() as u32).to_le_bytes());
    data.extend_from_slice(name.as_bytes());

    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: ario_arns::ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&pda, &account.into());
    pda
}

/// Build a `sync_attributes` instruction (used in setup-then-clear tests).
fn build_sync_ix(
    asset: &Pubkey,
    payer: &Pubkey,
    authority: &Pubkey,
    arns_record: &Pubkey,
    name: &str,
) -> Instruction {
    let metas = ario_ant::accounts::SyncAttributes {
        asset: *asset,
        payer: *payer,
        authority: *authority,
        arns_record: *arns_record,
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: system_program::ID,
    }
    .to_account_metas(None);
    Instruction {
        program_id: ario_ant::ID,
        accounts: metas,
        data: ario_ant::instruction::SyncAttributes {
            name: name.to_string(),
        }
        .data(),
    }
}

/// Build a `clear_attributes` instruction.
fn build_clear_ix(asset: &Pubkey, payer: &Pubkey, authority: &Pubkey) -> Instruction {
    let metas = ario_ant::accounts::ClearAttributes {
        asset: *asset,
        payer: *payer,
        authority: *authority,
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: system_program::ID,
    }
    .to_account_metas(None);
    Instruction {
        program_id: ario_ant::ID,
        accounts: metas,
        data: ario_ant::instruction::ClearAttributes {}.data(),
    }
}

/// Scan the asset blob for an Attributes plugin and confirm `key` is NOT
/// present. Used to verify clear actually removed traits.
fn assert_attribute_absent(blob: &[u8], key: &str) {
    let key_bytes = key.as_bytes();
    let mut i = 1usize;
    while i + 5 <= blob.len() {
        if blob[i] == 6 {
            let mut p = i + 1;
            if p + 4 > blob.len() {
                i += 1;
                continue;
            }
            let count = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
            p += 4;
            if count > 64 {
                i += 1;
                continue;
            }
            let mut found = false;
            let mut bad = false;
            for _ in 0..count {
                if p + 4 > blob.len() {
                    bad = true;
                    break;
                }
                let kl = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
                p += 4;
                if kl > 256 || p + kl > blob.len() {
                    bad = true;
                    break;
                }
                let k = &blob[p..p + kl];
                p += kl;
                if p + 4 > blob.len() {
                    bad = true;
                    break;
                }
                let vl = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
                p += 4;
                if vl > 1024 || p + vl > blob.len() {
                    bad = true;
                    break;
                }
                p += vl;
                if k == key_bytes {
                    found = true;
                }
            }
            if !bad {
                if found {
                    panic!("attribute {} unexpectedly present in asset blob", key);
                }
                return;
            }
        }
        i += 1;
    }
    // Plugin chain didn't include an Attributes plugin at all — also a
    // valid "absent" outcome.
}

/// Same scanner but asserts the key IS present with `expected_value`.
fn assert_attribute_present(blob: &[u8], key: &str, expected_value: &str) {
    let key_bytes = key.as_bytes();
    let mut i = 1usize;
    while i + 5 <= blob.len() {
        if blob[i] == 6 {
            let mut p = i + 1;
            if p + 4 > blob.len() {
                i += 1;
                continue;
            }
            let count = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
            p += 4;
            if count > 64 {
                i += 1;
                continue;
            }
            let mut ok = false;
            let mut bad = false;
            for _ in 0..count {
                if p + 4 > blob.len() {
                    bad = true;
                    break;
                }
                let kl = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
                p += 4;
                if kl > 256 || p + kl > blob.len() {
                    bad = true;
                    break;
                }
                let k = &blob[p..p + kl];
                p += kl;
                if p + 4 > blob.len() {
                    bad = true;
                    break;
                }
                let vl = u32::from_le_bytes(blob[p..p + 4].try_into().unwrap()) as usize;
                p += 4;
                if vl > 1024 || p + vl > blob.len() {
                    bad = true;
                    break;
                }
                let v = &blob[p..p + vl];
                p += vl;
                if k == key_bytes && v == expected_value.as_bytes() {
                    ok = true;
                }
            }
            if !bad && ok {
                return;
            }
        }
        i += 1;
    }
    panic!("expected attribute {}={} not found", key, expected_value);
}

// =========================================
// Tests
// =========================================

#[tokio::test]
async fn clear_attributes_removes_arns_traits_after_release_scenario() {
    // Scenario: ANT had a name; sync populated traits; record was then
    // closed (release_name on ario-arns side). Without the record,
    // sync_attributes cannot run. clear_attributes is the recovery.
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_test_ant(&mut ctx, &asset).await;

    // Step 1: populate traits via sync (simulates a prior buy_name).
    let name = "released";
    let arns_record = inject_arns_record(&mut ctx, name, &asset.pubkey(), 1, 25).await;
    let payer_pk = ctx.payer.pubkey();
    let sync_ix = build_sync_ix(&asset.pubkey(), &payer_pk, &payer_pk, &arns_record, name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[sync_ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("sync setup");

    // Sanity: sync wrote the traits.
    let asset_after_sync = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_present(&asset_after_sync.data, "ArNS Name", "released");
    assert_attribute_present(&asset_after_sync.data, "Type", "permabuy");
    assert_attribute_present(&asset_after_sync.data, "Undername Limit", "25");

    // Step 2: simulate release — wipe the ArnsRecord PDA. (In production
    // this happens via ario-arns::release_name, which closes the PDA.)
    let empty = solana_sdk::account::Account {
        lamports: 0,
        data: vec![],
        owner: solana_sdk::system_program::ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record, &empty.into());
    // (The asset_data still carries the post-sync traits — that's the
    // stale-trait scenario.)

    // Step 3: clear_attributes succeeds without needing the record.
    let clear_ix = build_clear_ix(&asset.pubkey(), &payer_pk, &payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[clear_ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("clear should succeed");

    // The three name-bound traits are gone.
    let asset_after_clear = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_absent(&asset_after_clear.data, "ArNS Name");
    assert_attribute_absent(&asset_after_clear.data, "Type");
    assert_attribute_absent(&asset_after_clear.data, "Undername Limit");
}

#[tokio::test]
async fn clear_attributes_preserves_ant_program_trait_when_present() {
    // The ANT Program trait is asset-bound (BD-100 routing key). It
    // MUST survive clear_attributes — otherwise a custom-program ANT
    // loses its routing on cleanup and silently reverts to canonical
    // resolution.
    //
    // This test plants the trait at CreateV1 time via
    // mint_test_ant_with_attributes, then syncs ArNS traits on top,
    // then clears — verifying the routing key survives the whole
    // round-trip through a real UpdatePluginV1 CPI.
    let mut ctx = program_test().start_with_context().await;

    let custom_program = "CustomANTProgramXXXXXXXXXXXXXXXXXXXXXXXXXXX";
    let asset = Keypair::new();
    mint_test_ant_with_attributes(&mut ctx, &asset, &[("ANT Program", custom_program)]).await;

    // Sanity: the trait was planted at mint.
    let asset_data = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_present(&asset_data.data, "ANT Program", custom_program);

    // Sync to add name-bound traits alongside ANT Program.
    let name = "preservetest";
    let arns_record = inject_arns_record(&mut ctx, name, &asset.pubkey(), 0, 5).await;
    let payer_pk = ctx.payer.pubkey();
    let sync_ix = build_sync_ix(&asset.pubkey(), &payer_pk, &payer_pk, &arns_record, name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[sync_ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Sanity: sync preserved ANT Program and added ArNS traits.
    let asset_after_sync = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_present(&asset_after_sync.data, "ANT Program", custom_program);
    assert_attribute_present(&asset_after_sync.data, "ArNS Name", "preservetest");

    // Now clear — this must remove ArNS traits but keep ANT Program.
    let clear_ix = build_clear_ix(&asset.pubkey(), &payer_pk, &payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[clear_ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let asset_after = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_absent(&asset_after.data, "ArNS Name");
    assert_attribute_absent(&asset_after.data, "Type");
    assert_attribute_absent(&asset_after.data, "Undername Limit");
    // The critical assertion: ANT Program survived the clear.
    assert_attribute_present(&asset_after.data, "ANT Program", custom_program);
}

#[tokio::test]
async fn clear_attributes_rejects_non_owner_authority() {
    // Same authority rule as sync_attributes: only the asset's current
    // MPL Core owner can sign UpdatePluginV1.
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_test_ant(&mut ctx, &asset).await; // owner = ctx.payer

    let intruder = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &intruder.pubkey(),
        1_000_000_000,
    );
    let tx = Transaction::new_signed_with_payer(
        &[fund],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let ix = build_clear_ix(&asset.pubkey(), &intruder.pubkey(), &intruder.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&intruder.pubkey()),
        &[&intruder],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;

    let expected = anchor_lang::error::ERROR_CODE_OFFSET + AntError::NotNftHolder as u32;
    match result {
        Err(BanksClientError::TransactionError(
            solana_sdk::transaction::TransactionError::InstructionError(
                _,
                solana_sdk::instruction::InstructionError::Custom(code),
            ),
        )) => {
            assert_eq!(
                code, expected,
                "expected NotNftHolder ({}), got {}",
                expected, code
            );
        }
        other => panic!("expected NotNftHolder ({}), got {:?}", expected, other),
    }
}

#[tokio::test]
async fn sync_attributes_fails_after_release_then_clear_recovers() {
    // Pins the architectural contract: after a name is released, sync
    // fails (the ArnsRecord PDA is closed — owner is system program,
    // not ario_arns). clear_attributes is the recovery path.
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_test_ant(&mut ctx, &asset).await;

    // Populate traits via sync.
    let name = "releasedname";
    let arns_record = inject_arns_record(&mut ctx, name, &asset.pubkey(), 1, 10).await;
    let payer_pk = ctx.payer.pubkey();
    let sync_ix = build_sync_ix(&asset.pubkey(), &payer_pk, &payer_pk, &arns_record, name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[sync_ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("sync setup");

    // Simulate release — close the ArnsRecord PDA.
    let empty = solana_sdk::account::Account {
        lamports: 0,
        data: vec![],
        owner: solana_sdk::system_program::ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record, &empty.into());

    // sync_attributes must fail — record owner is system program.
    let sync_ix2 = build_sync_ix(&asset.pubkey(), &payer_pk, &payer_pk, &arns_record, name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[sync_ix2], Some(&payer_pk), &[&ctx.payer], blockhash);
    let result = ctx.banks_client.process_transaction(tx).await;
    let expected = anchor_lang::error::ERROR_CODE_OFFSET + AntError::InvalidArnsRecord as u32;
    match result {
        Err(BanksClientError::TransactionError(
            solana_sdk::transaction::TransactionError::InstructionError(
                _,
                solana_sdk::instruction::InstructionError::Custom(code),
            ),
        )) => {
            assert_eq!(
                code, expected,
                "expected InvalidArnsRecord ({}), got {}",
                expected, code
            );
        }
        other => panic!("expected InvalidArnsRecord ({}), got {:?}", expected, other),
    }

    // clear_attributes recovers — no record needed.
    let clear_ix = build_clear_ix(&asset.pubkey(), &payer_pk, &payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[clear_ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("clear should recover");

    let asset_after = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_absent(&asset_after.data, "ArNS Name");
    assert_attribute_absent(&asset_after.data, "Type");
    assert_attribute_absent(&asset_after.data, "Undername Limit");
}

#[tokio::test]
async fn sync_attributes_fails_after_reassign_clear_recovers_old_asset() {
    // After reassign_name, the ArnsRecord points at the NEW asset.
    // sync_attributes against the OLD asset fails (record.ant !=
    // asset.key()). clear_attributes recovers the OLD asset's stale
    // traits.
    let mut ctx = program_test().start_with_context().await;

    let old_asset = Keypair::new();
    mint_test_ant(&mut ctx, &old_asset).await;

    // Populate traits on old_asset.
    let name = "reassigned";
    let arns_record = inject_arns_record(&mut ctx, name, &old_asset.pubkey(), 1, 20).await;
    let payer_pk = ctx.payer.pubkey();
    let sync_ix = build_sync_ix(
        &old_asset.pubkey(),
        &payer_pk,
        &payer_pk,
        &arns_record,
        name,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[sync_ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("sync setup");

    // Simulate reassign — update the record to point at a new asset.
    let new_asset = Keypair::new();
    mint_test_ant(&mut ctx, &new_asset).await;
    // Re-inject the record with the new ANT mint.
    inject_arns_record(&mut ctx, name, &new_asset.pubkey(), 1, 20).await;

    // sync_attributes against old_asset must fail — record.ant is now new_asset.
    let sync_ix2 = build_sync_ix(
        &old_asset.pubkey(),
        &payer_pk,
        &payer_pk,
        &arns_record,
        name,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[sync_ix2], Some(&payer_pk), &[&ctx.payer], blockhash);
    let result = ctx.banks_client.process_transaction(tx).await;
    let expected = anchor_lang::error::ERROR_CODE_OFFSET + AntError::InvalidArnsRecord as u32;
    match result {
        Err(BanksClientError::TransactionError(
            solana_sdk::transaction::TransactionError::InstructionError(
                _,
                solana_sdk::instruction::InstructionError::Custom(code),
            ),
        )) => {
            assert_eq!(
                code, expected,
                "expected InvalidArnsRecord ({}), got {}",
                expected, code
            );
        }
        other => panic!("expected InvalidArnsRecord ({}), got {:?}", expected, other),
    }

    // clear_attributes recovers the old asset.
    let clear_ix = build_clear_ix(&old_asset.pubkey(), &payer_pk, &payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[clear_ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("clear should recover old asset");

    let asset_after = ctx
        .banks_client
        .get_account(old_asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_absent(&asset_after.data, "ArNS Name");
    assert_attribute_absent(&asset_after.data, "Type");
    assert_attribute_absent(&asset_after.data, "Undername Limit");
}

#[tokio::test]
async fn clear_attributes_idempotent_on_already_clean_asset() {
    // Calling clear twice in a row should succeed both times. Useful
    // sanity check that the ix doesn't depend on prior state.
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_test_ant(&mut ctx, &asset).await;

    let payer_pk = ctx.payer.pubkey();

    // First clear (asset starts with empty plugin) — should succeed.
    let ix1 = build_clear_ix(&asset.pubkey(), &payer_pk, &payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix1], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("first clear");

    // Second clear — also succeeds.
    let ix2 = build_clear_ix(&asset.pubkey(), &payer_pk, &payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix2], Some(&payer_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("second clear");
}
