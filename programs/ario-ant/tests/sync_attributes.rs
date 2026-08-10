//! Integration test for `ario_ant::sync_attributes` (Sprint 3 / ADR-016
//! reshape). Exercises the actual UpdatePluginV1 CPI against a real MPL
//! Core program loaded from the BPF fixture, so it covers everything the
//! unit tests in `programs/ario-ant/src/mpl_core_cpi.rs::tests` cannot:
//! the wire format reaching MPL, the Owner-authority signer requirement,
//! and the post-update on-chain attribute layout.
//!
//! REQUIRES:
//!   1. `cargo build-sbf --manifest-path programs/ario-ant/Cargo.toml`
//!      (or `anchor build`) so `target/deploy/ario_ant.so` is fresh.
//!      `solana-program-test` 2.1 with `BPF_OUT_DIR` set treats every
//!      program registered via `ProgramTest::new(...)` as "prefer BPF"
//!      and panics with `Program file data not available for ario_ant`
//!      if the .so is missing — even when a native processor is
//!      supplied.
//!   2. `cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/`
//!   3. `BPF_OUT_DIR="$(pwd)/target/deploy" cargo test -p ario-ant
//!      --test sync_attributes`
//!
//! Without step 2, `add_program("mpl_core", ...)` falls back to a
//! stale/empty .so and the inner CPI returns AccountNotExecutable.

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
    // Real MPL Core BPF — without this, the inner UpdatePluginV1 CPI
    // returns AccountNotExecutable. See module doc-comment for setup.
    pt.add_program("mpl_core", MPL_CORE_PROGRAM_ID, None);
    pt
}

/// Per-asset ario-ant `ant_authority` PDA (ADR-028) — new ANTs park their
/// UpdateAuthority here and the program signs UpdatePluginV1 with it.
fn ant_authority_pda(asset: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[ario_ant::state::ANT_AUTHORITY_SEED, asset.as_ref()],
        &ario_ant::ID,
    )
    .0
}

/// Mint a real Metaplex Core asset the way new (ADR-028) ANTs are minted:
/// Owner = `ctx.payer`, UpdateAuthority = the per-asset `ant_authority` PDA,
/// and the Attributes plugin authority = `UpdateAuthority` (variant 2) so the
/// program can sign UpdatePluginV1 with the PDA.
async fn mint_test_ant(ctx: &mut ProgramTestContext, asset: &Keypair) {
    mint_ant_inner(ctx, asset, /* program_controlled */ true).await;
}

/// Mint a legacy (pre-ADR-028) ANT: Owner == UpdateAuthority == `ctx.payer`
/// and the Attributes plugin authority = `Owner` (variant 1). Used to cover the
/// owner-signed fallback branch of `sync_attributes`.
async fn mint_legacy_test_ant(ctx: &mut ProgramTestContext, asset: &Keypair) {
    mint_ant_inner(ctx, asset, /* program_controlled */ false).await;
}

async fn mint_ant_inner(ctx: &mut ProgramTestContext, asset: &Keypair, program_controlled: bool) {
    let name = b"sync-test-ant";
    let uri = b"ar://sync";
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
    data.extend_from_slice(&0u32.to_le_bytes()); // empty attribute list
    data.push(1); // authority Option = Some
                  // plugin Authority: UpdateAuthority (2) for program-controlled ANTs, Owner
                  // (1) for legacy ANTs.
    data.push(if program_controlled { 2 } else { 1 });

    let placeholder = MPL_CORE_PROGRAM_ID;
    // updateAuthority slot: the ant_authority PDA for program-controlled ANTs;
    // None (→ defaults to authority = payer) for legacy ANTs.
    let ua_meta = if program_controlled {
        AccountMeta::new_readonly(ant_authority_pda(&asset.pubkey()), false)
    } else {
        AccountMeta::new_readonly(placeholder, false) // updateAuthority (None)
    };
    let metas = vec![
        AccountMeta::new(asset.pubkey(), true),
        AccountMeta::new_readonly(placeholder, false), // collection (None)
        AccountMeta::new_readonly(ctx.payer.pubkey(), true), // authority
        AccountMeta::new(ctx.payer.pubkey(), true),    // payer
        // owner MUST be explicit: MPL Core defaults owner to updateAuthority
        // when None, so with UA = the ant_authority PDA an omitted owner would
        // make the PDA the owner. Pin it to the user.
        AccountMeta::new_readonly(ctx.payer.pubkey(), false), // owner
        ua_meta,                                              // updateAuthority
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

/// Inject a fake `ArnsRecord` PDA owned by ario_arns::ID, pointing at
/// `ant_mint`. The Borsh layout mirrors
/// `programs/ario-arns/src/state/mod.rs::ArnsRecord` — keep in lock-step.
async fn inject_arns_record(
    ctx: &mut ProgramTestContext,
    name: &str,
    ant_mint: &Pubkey,
    purchase_type: u8, // 0 = Lease, 1 = Permabuy
    undername_limit: u16,
) -> Pubkey {
    let pda = arns_record_pda(name);

    let mut data = Vec::<u8>::new();
    let disc = anchor_lang::solana_program::hash::hash(b"account:ArnsRecord");
    data.extend_from_slice(&disc.to_bytes()[..8]); // discriminator
    let name_hash = anchor_lang::solana_program::hash::hash(name.to_lowercase().as_bytes());
    data.extend_from_slice(name_hash.as_ref()); // name_hash
    data.extend_from_slice(&[0u8; 32]); // owner (unused on read)
    data.extend_from_slice(ant_mint.as_ref()); // ant
    data.push(purchase_type); // purchase_type
    data.extend_from_slice(&0i64.to_le_bytes()); // start_timestamp
    data.push(0); // end_timestamp = None
    data.extend_from_slice(&undername_limit.to_le_bytes());
    data.extend_from_slice(&0u64.to_le_bytes()); // purchase_price
    data.push(0); // bump
    data.extend_from_slice(&(name.len() as u32).to_le_bytes());
    data.extend_from_slice(name.as_bytes());
    // version: SchemaVersion (3 bytes), byte-end field after the variable-length
    // `name` (ADR-020). Required or `try_deserialize` hits Borsh EOF.
    data.extend_from_slice(&[1, 0, 0]);

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

/// Build a `sync_attributes` instruction. `payer` is the permissionless caller
/// (for program-controlled ANTs) or the owner (for legacy ANTs).
fn build_sync_ix(asset: &Pubkey, payer: &Pubkey, arns_record: &Pubkey, name: &str) -> Instruction {
    let metas = ario_ant::accounts::SyncAttributes {
        asset: *asset,
        payer: *payer,
        ant_authority: ant_authority_pda(asset),
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

/// Scan the asset blob for an Attributes plugin (variant byte = 6) and
/// confirm `(key, expected_value)` is present. Mirrors the on-chain
/// `read_existing_attribute` scan logic. Panics on miss.
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
    panic!(
        "expected attribute {}={} not found in asset blob",
        key, expected_value
    );
}

#[tokio::test]
async fn sync_attributes_populates_three_traits() {
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_test_ant(&mut ctx, &asset).await;

    let name = "myname";
    let arns_record = inject_arns_record(&mut ctx, name, &asset.pubkey(), 1, 10).await;

    let payer_pk = ctx.payer.pubkey();
    let ix = build_sync_ix(&asset.pubkey(), &payer_pk, &arns_record, name);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer_pk), &[&ctx.payer], blockhash);
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .expect("sync_attributes should land");
    assert!(
        result.result.is_ok(),
        "sync_attributes should succeed for asset owner: {:?}",
        result.result
    );

    let asset_after = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_present(&asset_after.data, "ArNS Name", "myname");
    assert_attribute_present(&asset_after.data, "Type", "permabuy");
    assert_attribute_present(&asset_after.data, "Undername Limit", "10");

    // PR-3 event coverage: AttributesSyncedEvent { mint, name }.
    let logs = result.metadata.expect("metadata").log_messages;
    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_ant::AttributesSyncedEvent);
    assert_eq!(ev.mint, asset.pubkey());
    assert_eq!(ev.name, "myname");
}

#[tokio::test]
async fn sync_attributes_rejects_record_pointing_at_other_asset() {
    let mut ctx = program_test().start_with_context().await;

    let asset_a = Keypair::new();
    let asset_b = Keypair::new();
    mint_test_ant(&mut ctx, &asset_a).await;
    mint_test_ant(&mut ctx, &asset_b).await;

    // Record claims `asset_a`; we'll call sync_attributes against `asset_b`.
    // Without the `record.ant == asset.key()` check, the attacker could
    // overwrite asset_b's traits with values from a record they don't own.
    let name = "victim";
    let arns_record = inject_arns_record(&mut ctx, name, &asset_a.pubkey(), 1, 10).await;

    let payer_pk = ctx.payer.pubkey();
    let ix = build_sync_ix(&asset_b.pubkey(), &payer_pk, &arns_record, name);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer_pk), &[&ctx.payer], blockhash);
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
                "expected InvalidArnsRecord (code {}), got {}",
                expected, code
            );
        }
        other => panic!(
            "expected InvalidArnsRecord (code {}), got {:?}",
            expected, other
        ),
    }
}

/// Fund `who` with 1 SOL so they can pay fees as a standalone signer.
async fn fund(ctx: &mut ProgramTestContext, who: &Pubkey) {
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund = solana_sdk::system_instruction::transfer(&ctx.payer.pubkey(), who, 1_000_000_000);
    let tx = Transaction::new_signed_with_payer(
        &[fund],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

#[tokio::test]
async fn sync_attributes_permissionless_for_program_controlled() {
    // ADR-028: a program-controlled ANT (UA = ant_authority PDA) can be synced
    // by anyone — the program signs the CPI with the PDA and the ArnsRecord
    // checks guarantee only canonical traits get written. A non-owner intruder
    // succeeds.
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_test_ant(&mut ctx, &asset).await; // owner = ctx.payer, UA = ant_authority PDA

    let name = "permissionless";
    let arns_record = inject_arns_record(&mut ctx, name, &asset.pubkey(), 1, 10).await;

    let intruder = Keypair::new();
    fund(&mut ctx, &intruder.pubkey()).await;

    let ix = build_sync_ix(&asset.pubkey(), &intruder.pubkey(), &arns_record, name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&intruder.pubkey()),
        &[&intruder],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("permissionless sync of a program-controlled ANT should succeed");

    let asset_after = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_present(&asset_after.data, "ArNS Name", "permissionless");
}

#[tokio::test]
async fn sync_attributes_legacy_requires_owner() {
    // Legacy ANTs (plugin authority = Owner) keep the pre-ADR-028 rule: only the
    // current owner may sync. A non-owner is rejected with NotNftHolder; the
    // owner succeeds.
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_legacy_test_ant(&mut ctx, &asset).await; // owner == UA == ctx.payer

    let name = "ownertest";
    let arns_record = inject_arns_record(&mut ctx, name, &asset.pubkey(), 0, 10).await;

    // Non-owner intruder is rejected.
    let intruder = Keypair::new();
    fund(&mut ctx, &intruder.pubkey()).await;
    let ix = build_sync_ix(&asset.pubkey(), &intruder.pubkey(), &arns_record, name);
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
                "expected NotNftHolder (code {}), got {}",
                expected, code
            );
        }
        other => panic!("expected NotNftHolder (code {}), got {:?}", expected, other),
    }

    // The owner (ctx.payer) succeeds via the legacy owner-signed path.
    let owner_pk = ctx.payer.pubkey();
    let ix = build_sync_ix(&asset.pubkey(), &owner_pk, &arns_record, name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&owner_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("owner-signed sync of a legacy ANT should succeed");
}

/// Read the asset-level UpdateAuthority from a Metaplex Core AssetV1 blob.
/// byte 0 = key (1), bytes 1..33 = owner, byte 33 = UA enum variant
/// (0=None, 1=Address(Pubkey), 2=Collection(Pubkey)).
fn read_update_authority(blob: &[u8]) -> Option<Pubkey> {
    if blob.len() < 34 || blob[0] != 1 {
        return None;
    }
    match blob[33] {
        1 if blob.len() >= 66 => Some(Pubkey::new_from_array(blob[34..66].try_into().unwrap())),
        _ => None,
    }
}

/// Read the asset Owner (bytes 1..33) from a Metaplex Core AssetV1 blob.
fn read_owner(blob: &[u8]) -> Pubkey {
    Pubkey::new_from_array(blob[1..33].try_into().unwrap())
}

/// Build an `adopt_authority` instruction (owner-signed).
fn build_adopt_ix(asset: &Pubkey, payer: &Pubkey, owner: &Pubkey) -> Instruction {
    let metas = ario_ant::accounts::AdoptAuthority {
        asset: *asset,
        payer: *payer,
        owner: *owner,
        ant_authority: ant_authority_pda(asset),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: system_program::ID,
    }
    .to_account_metas(None);
    Instruction {
        program_id: ario_ant::ID,
        accounts: metas,
        data: ario_ant::instruction::AdoptAuthority {}.data(),
    }
}

#[tokio::test]
async fn adopt_authority_makes_legacy_ant_program_controlled() {
    // A legacy ANT (owner == UA, plugin authority = Owner) is adopted by its
    // owner: UA moves to the ant_authority PDA and, because the plugin authority
    // is re-pointed to UpdateAuthority, sync becomes permissionless afterward.
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_legacy_test_ant(&mut ctx, &asset).await; // owner == UA == ctx.payer
    let ant_authority = ant_authority_pda(&asset.pubkey());

    // Pre-adopt: UA is the owner (ctx.payer), not the PDA.
    let pre = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read_update_authority(&pre.data), Some(ctx.payer.pubkey()));

    let owner_pk = ctx.payer.pubkey();
    let ix = build_adopt_ix(&asset.pubkey(), &owner_pk, &owner_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&owner_pk), &[&ctx.payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("adopt_authority should succeed for the owner");

    // Post-adopt: UA is the ant_authority PDA.
    let post = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        read_update_authority(&post.data),
        Some(ant_authority),
        "adopt must move UA to the ant_authority PDA"
    );
    // Custody invariant: Owner is unchanged — adoption only rotates UA, never
    // the NFT owner.
    assert_eq!(
        read_owner(&post.data),
        ctx.payer.pubkey(),
        "adopt must NOT change the asset Owner"
    );

    // And a permissionless (non-owner) sync now works.
    let name = "adopted";
    let arns_record = inject_arns_record(&mut ctx, name, &asset.pubkey(), 1, 10).await;
    let intruder = Keypair::new();
    fund(&mut ctx, &intruder.pubkey()).await;
    let ix = build_sync_ix(&asset.pubkey(), &intruder.pubkey(), &arns_record, name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&intruder.pubkey()),
        &[&intruder],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("permissionless sync should work after adopt");

    let asset_after = ctx
        .banks_client
        .get_account(asset.pubkey())
        .await
        .unwrap()
        .unwrap();
    assert_attribute_present(&asset_after.data, "ArNS Name", "adopted");
}

#[tokio::test]
async fn adopt_authority_rejects_non_owner() {
    // Only the current owner may adopt. A non-owner attempt is rejected with
    // NotNftHolder (owner-controlled migration contract).
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_legacy_test_ant(&mut ctx, &asset).await; // owner == UA == ctx.payer

    let intruder = Keypair::new();
    fund(&mut ctx, &intruder.pubkey()).await;

    // Intruder passes itself as `owner` and signs — the handler's
    // `owner == nft_owner` check rejects it.
    let ix = build_adopt_ix(&asset.pubkey(), &intruder.pubkey(), &intruder.pubkey());
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
                "expected NotNftHolder (code {}), got {}",
                expected, code
            );
        }
        other => panic!("expected NotNftHolder (code {}), got {:?}", expected, other),
    }
}

#[tokio::test]
async fn adopt_authority_rejects_already_program_controlled() {
    // Adopting an ANT whose UA is already the ant_authority PDA fails with
    // AlreadyProgramControlled.
    let mut ctx = program_test().start_with_context().await;

    let asset = Keypair::new();
    mint_test_ant(&mut ctx, &asset).await; // already program-controlled

    let owner_pk = ctx.payer.pubkey();
    let ix = build_adopt_ix(&asset.pubkey(), &owner_pk, &owner_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&owner_pk), &[&ctx.payer], blockhash);
    let result = ctx.banks_client.process_transaction(tx).await;

    let expected =
        anchor_lang::error::ERROR_CODE_OFFSET + AntError::AlreadyProgramControlled as u32;
    match result {
        Err(BanksClientError::TransactionError(
            solana_sdk::transaction::TransactionError::InstructionError(
                _,
                solana_sdk::instruction::InstructionError::Custom(code),
            ),
        )) => {
            assert_eq!(
                code, expected,
                "expected AlreadyProgramControlled (code {}), got {}",
                expected, code
            );
        }
        other => panic!(
            "expected AlreadyProgramControlled (code {}), got {:?}",
            expected, other
        ),
    }
}
