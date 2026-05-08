//! Integration tests for `ario-ant-escrow`.
//!
//! Test architecture mirrors `programs/ario-arns/tests/integration.rs`:
//! - `program_test()` loads both `ario_ant_escrow` (entry processor) and
//!   the real `mpl_core.so` BPF program (so TransferV1 CPIs land on actual
//!   logic instead of stubs).
//! - Each test mints a fresh Metaplex Core asset via raw CreateV1 wire
//!   bytes — no `mpl-core` Rust crate dependency, identical to what the
//!   existing program tests do.
//! - Helpers (`mint_test_ant`, `transfer_test_ant`) are vendored from the
//!   ario-arns suite so this file stays self-contained.
//!
//! RUNNING:
//!   1. `anchor build` (or `./build-sbf.sh`) so `target/deploy/ario_ant_escrow.so`
//!      is fresh — `solana-program-test` will load a stale .so otherwise.
//!   2. Copy the mpl-core fixture into BPF_OUT_DIR so `add_program("mpl_core")`
//!      can find it:
//!        cp programs/ario-ant-escrow/tests/fixtures/mpl_core.so target/deploy/
//!   3. Run with the fixture path exported:
//!        BPF_OUT_DIR=$(pwd)/contracts/target/deploy \
//!          cargo test -p ario-ant-escrow

use anchor_lang::{prelude::*, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction as Ix},
    program_pack::Pack,
    signature::{Keypair, Signer},
    system_instruction,
    transaction::Transaction,
};

use ario_ant_escrow::error::EscrowError;
use ario_ant_escrow::state::{
    derive_initial_nonce, EscrowAnt, EscrowToken, ARWEAVE_PUBKEY_LEN, ASSET_TYPE_ANT,
    ASSET_TYPE_TOKEN, ASSET_TYPE_VAULT, ESCROW_ANT_SEED, ESCROW_TOKEN_SEED, ESCROW_VAULT_SEED,
    ESCROW_VERSION_V1, ETHEREUM_PUBKEY_LEN, MPL_CORE_PROGRAM_ID, PROTOCOL_ARWEAVE,
    PROTOCOL_ETHEREUM,
};
use ario_ant_escrow::{
    EscrowCancelledEvent, EscrowClaimedEvent, EscrowDepositedEvent, EscrowRecipientUpdatedEvent,
};

// =========================================
// Test scaffolding (pattern from ario-arns)
// =========================================

fn anchor_processor(
    program_id: &Pubkey,
    accounts: &[anchor_lang::prelude::AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[anchor_lang::prelude::AccountInfo] = std::mem::transmute(accounts);
        ario_ant_escrow::entry(program_id, accounts, data)
    }
}

#[allow(unused_macros)]
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

/// Skip an integration test when the BPF artifacts that
/// `solana-program-test` needs are missing. Returns `true` if the test
/// should bail out early. We check for `ario_ant_escrow.so` (built by
/// `cargo build-sbf` / `./build-sbf.sh`) and `mpl_core.so` (the fixture
/// from `tests/fixtures/`) in either `BPF_OUT_DIR` or the workspace's
/// canonical `target/deploy` path.
///
/// The integration suite is the source of truth for Phase 1 acceptance,
/// but it can't run on dev machines that don't have Solana platform-tools
/// installed (cargo 1.79 + cargo-build-sbf). Unit tests in
/// `state::tests` and `mpl_core_cpi::tests` cover the pure-Rust portions
/// without that dependency.
fn skip_if_no_bpf_artifacts() -> bool {
    let dirs: Vec<std::path::PathBuf> = std::env::var("BPF_OUT_DIR")
        .ok()
        .map(std::path::PathBuf::from)
        .into_iter()
        .chain(std::iter::once(std::path::PathBuf::from(
            "../../target/deploy",
        )))
        .collect();

    let needed = ["ario_ant_escrow.so", "mpl_core.so"];
    let missing: Vec<&str> = needed
        .iter()
        .copied()
        .filter(|f| !dirs.iter().any(|d| d.join(f).exists()))
        .collect();

    if !missing.is_empty() {
        eprintln!(
            "[ario-ant-escrow] skipping integration test — missing BPF artifact(s): {:?}\n\
             To run these locally:\n\
               1. ./build-sbf.sh                   (in contracts/)\n\
               2. cp programs/ario-ant-escrow/tests/fixtures/mpl_core.so target/deploy/\n\
               3. BPF_OUT_DIR=$(pwd)/target/deploy cargo test -p ario-ant-escrow",
            missing
        );
        true
    } else {
        false
    }
}

fn program_test() -> ProgramTest {
    let mut pt = ProgramTest::new(
        "ario_ant_escrow",
        ario_ant_escrow::ID,
        processor!(anchor_processor),
    );
    pt.set_compute_max_units(400_000);
    // Real mpl-core BPF program. Fixture lives at
    // programs/ario-ant-escrow/tests/fixtures/mpl_core.so; tests must be
    // run with BPF_OUT_DIR pointing at a directory containing it (see
    // module-level RUNNING note).
    pt.add_program("mpl_core", MPL_CORE_PROGRAM_ID, None);
    pt
}

async fn airdrop(ctx: &mut ProgramTestContext, to: &Pubkey, lamports: u64) {
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            to,
            lamports,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

/// Mint a real Metaplex Core asset. Owner = `owner.pubkey()`. Uses raw
/// CreateV1 wire bytes (kinobi-validated), same encoding as
/// programs/ario-arns/tests/integration.rs::mint_test_ant.
async fn mint_test_ant(ctx: &mut ProgramTestContext, asset_keypair: &Keypair, owner: &Keypair) {
    let name = b"escrow-test-ant";
    let uri = b"ar://escrow-test";

    let mut data = Vec::<u8>::new();
    data.push(0); // CreateV1 discriminator
    data.push(0); // dataState = AccountState::Uncompressed
    data.extend_from_slice(&(name.len() as u32).to_le_bytes());
    data.extend_from_slice(name);
    data.extend_from_slice(&(uri.len() as u32).to_le_bytes());
    data.extend_from_slice(uri);
    data.push(1); // plugins Option = Some
    data.extend_from_slice(&1u32.to_le_bytes()); // plugins vec len = 1
    data.push(6); // Plugin::Attributes
    data.extend_from_slice(&0u32.to_le_bytes()); // attribute_list len = 0
    data.push(1); // plugin authority Option = Some
    data.push(1); // BasePluginAuthority::Owner

    let placeholder = MPL_CORE_PROGRAM_ID;
    let metas = vec![
        AccountMeta::new(asset_keypair.pubkey(), true), // 0 asset (signer, writable)
        AccountMeta::new_readonly(placeholder, false),  // 1 collection (None)
        AccountMeta::new_readonly(owner.pubkey(), true), // 2 authority (signer)
        AccountMeta::new(ctx.payer.pubkey(), true),     // 3 payer (signer, writable)
        AccountMeta::new_readonly(owner.pubkey(), false), // 4 owner (explicit; None defaults to payer, not authority)
        // 5 updateAuthority — explicit owner. ADR-013 mints AR.IO ANTs with
        // `Owner == UpdateAuthority`, and PR-5's deposit handler relies on
        // the depositor being the current UA so it can sign UpdateV1 to
        // transfer UA into escrow custody alongside Owner.
        AccountMeta::new_readonly(owner.pubkey(), false),
        AccountMeta::new_readonly(solana_sdk::system_program::ID, false), // 6 system_program
        AccountMeta::new_readonly(placeholder, false),                    // 7 logWrapper (None)
    ];

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    // Distinct signers: ctx.payer (rent), owner (authority), asset_keypair (account).
    // Order doesn't matter for solana-sdk; dedup by pubkey is implicit.
    let signers: Vec<&Keypair> = if owner.pubkey() == ctx.payer.pubkey() {
        vec![&ctx.payer, asset_keypair]
    } else {
        vec![&ctx.payer, owner, asset_keypair]
    };
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: MPL_CORE_PROGRAM_ID,
            accounts: metas,
            data,
        }],
        Some(&ctx.payer.pubkey()),
        &signers,
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("mint_test_ant: CreateV1 failed (is mpl_core.so loaded? BPF_OUT_DIR set?)");
}

fn escrow_pda(ant_mint: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ESCROW_ANT_SEED, ant_mint.as_ref()], &ario_ant_escrow::ID)
}

// =========================================
// Helpers — build escrow ix transactions
// =========================================

async fn deposit_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    depositor: &Keypair,
    protocol: u8,
    pubkey: Vec<u8>,
) -> std::result::Result<(), BanksClientError> {
    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::DepositAnt {
        escrow,
        ant_asset: asset,
        depositor: depositor.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::DepositAnt {
        recipient_protocol: protocol,
        recipient_pubkey: pubkey,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

async fn cancel_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    depositor: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::CancelDeposit {
        escrow,
        ant_asset: asset,
        depositor: depositor.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::CancelDeposit {}.data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

async fn update_recipient_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    depositor: &Keypair,
    new_protocol: u8,
    new_pubkey: Vec<u8>,
) -> std::result::Result<(), BanksClientError> {
    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::UpdateRecipient {
        escrow,
        depositor: depositor.pubkey(),
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::UpdateRecipient {
        new_protocol,
        new_pubkey,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

async fn fetch_escrow(ctx: &mut ProgramTestContext, asset: Pubkey) -> EscrowAnt {
    let (escrow, _) = escrow_pda(&asset);
    let raw = ctx
        .banks_client
        .get_account(escrow)
        .await
        .unwrap()
        .expect("escrow account missing");
    EscrowAnt::try_deserialize(&mut &raw.data[..]).expect("decode EscrowAnt")
}

async fn read_asset_owner(ctx: &mut ProgramTestContext, asset: Pubkey) -> Pubkey {
    let raw = ctx
        .banks_client
        .get_account(asset)
        .await
        .unwrap()
        .expect("asset account missing");
    // Metaplex Core AssetV1: byte 0 = key, bytes 1..33 = owner.
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&raw.data[1..33]);
    Pubkey::from(buf)
}

/// Read the asset's UpdateAuthority. Layout after Owner (bytes 33+):
/// byte 33 = `BaseUpdateAuthority` enum tag (0=None, 1=Address, 2=Collection).
/// For Address / Collection variants, bytes 34..66 = the 32-byte pubkey.
async fn read_asset_update_authority(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
) -> Option<Pubkey> {
    let raw = ctx
        .banks_client
        .get_account(asset)
        .await
        .unwrap()
        .expect("asset account missing");
    let tag = raw.data.get(33).copied().unwrap_or(0);
    match tag {
        1 | 2 => {
            let mut buf = [0u8; 32];
            buf.copy_from_slice(&raw.data[34..66]);
            Some(Pubkey::from(buf))
        }
        _ => None,
    }
}

fn arweave_pubkey_fixture(seed: u8) -> Vec<u8> {
    // Deterministic but non-trivial 512-byte blob. The escrow program does
    // not parse it during deposit/cancel/update — that's the verifier's
    // job in Phase 2 — so any 512-byte payload exercises the storage path.
    (0..ARWEAVE_PUBKEY_LEN as u32)
        .map(|i| ((i as u32).wrapping_add(seed as u32) & 0xFF) as u8)
        .collect()
}

fn ethereum_address_fixture(seed: u8) -> Vec<u8> {
    let mut v = vec![0u8; ETHEREUM_PUBKEY_LEN];
    for (i, b) in v.iter_mut().enumerate() {
        *b = (i as u8).wrapping_add(seed);
    }
    v
}

// =========================================
// Phase 1 — deposit_ant
// =========================================

#[tokio::test]
async fn test_deposit_ant_arweave() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    let pubkey = arweave_pubkey_fixture(0xAB);
    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        pubkey.clone(),
    )
    .await
    .expect("deposit_ant should succeed");

    // EscrowAnt populated correctly.
    let escrow = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(escrow.version, ESCROW_VERSION_V1);
    assert_eq!(escrow.depositor, depositor.pubkey());
    assert_eq!(escrow.ant_mint, asset_kp.pubkey());
    assert_eq!(escrow.recipient_protocol, PROTOCOL_ARWEAVE);
    assert_eq!(escrow.recipient_pubkey_len as usize, ARWEAVE_PUBKEY_LEN);
    assert_eq!(&escrow.recipient_pubkey[..ARWEAVE_PUBKEY_LEN], &pubkey[..]);

    // Initial nonce derivation is reproducible from public state.
    let expected_nonce =
        derive_initial_nonce(escrow.deposit_slot, &escrow.ant_mint, &escrow.depositor);
    assert_eq!(escrow.nonce, expected_nonce);

    // Asset ownership transferred to escrow PDA.
    let (expected_pda, _) = escrow_pda(&asset_kp.pubkey());
    let actual_owner = read_asset_owner(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(
        actual_owner, expected_pda,
        "ANT should be owned by escrow PDA"
    );
}

#[tokio::test]
async fn test_deposit_ant_ethereum() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    let addr = ethereum_address_fixture(0xCD);
    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        addr.clone(),
    )
    .await
    .expect("deposit_ant ethereum should succeed");

    let escrow = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(escrow.recipient_protocol, PROTOCOL_ETHEREUM);
    assert_eq!(escrow.recipient_pubkey_len as usize, ETHEREUM_PUBKEY_LEN);
    // Active prefix matches; tail must be zero-padded.
    assert_eq!(&escrow.recipient_pubkey[..ETHEREUM_PUBKEY_LEN], &addr[..]);
    assert!(escrow.recipient_pubkey[ETHEREUM_PUBKEY_LEN..]
        .iter()
        .all(|&b| b == 0));
}

#[tokio::test]
async fn test_deposit_invalid_protocol() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    // protocol=2 with otherwise-valid 512-byte pubkey — handler sees
    // (validated_protocol_and_len) returns None and the InvalidRecipientProtocol
    // branch fires (because protocol > 1).
    let result = deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        2,
        arweave_pubkey_fixture(0),
    )
    .await;
    assert_anchor_error!(result, EscrowError::InvalidRecipientProtocol);
}

#[tokio::test]
async fn test_deposit_invalid_pubkey_len() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    // Arweave protocol but 256-byte pubkey — handler returns
    // InvalidRecipientPubkeyLength because protocol byte is in {0,1}.
    let too_short: Vec<u8> = vec![1u8; 256];
    let result = deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        too_short,
    )
    .await;
    assert_anchor_error!(result, EscrowError::InvalidRecipientPubkeyLength);
}

#[tokio::test]
async fn test_deposit_non_owner_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let real_owner = Keypair::new();
    let imposter = Keypair::new();
    airdrop(&mut ctx, &real_owner.pubkey(), 5_000_000_000).await;
    airdrop(&mut ctx, &imposter.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &real_owner).await;

    // Imposter tries to deposit an ANT they don't own. Handler's
    // read_mpl_core_owner check fires before the CPI ever runs.
    let result = deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &imposter,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await;
    assert_anchor_error!(result, EscrowError::NotAntOwner);
}

#[tokio::test]
async fn test_deposit_duplicate_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    // First deposit succeeds.
    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("first deposit");

    // Second deposit fails because the PDA already exists. We get a system
    // program "account already in use" error rather than a custom Anchor
    // error — assert it's an error of any kind.
    let result = deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(1),
    )
    .await;
    match result {
        Err(solana_program_test::BanksClientError::TransactionError(
            solana_sdk::transaction::TransactionError::InstructionError(_, _),
        )) => { /* expected: system program rejects because PDA already exists */ }
        Err(e) => panic!("duplicate deposit: expected InstructionError, got: {:?}", e),
        Ok(()) => panic!("duplicate deposit should have failed"),
    }
}

// =========================================
// Phase 1 — cancel_deposit
// =========================================

#[tokio::test]
async fn test_cancel_deposit() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("deposit");

    cancel_tx(&mut ctx, asset_kp.pubkey(), &depositor)
        .await
        .expect("cancel");

    // Escrow PDA closed (rent returned to depositor).
    let (escrow_addr, _) = escrow_pda(&asset_kp.pubkey());
    let escrow_acct = ctx.banks_client.get_account(escrow_addr).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after cancel"
    );

    // ANT returned to depositor.
    let owner_after = read_asset_owner(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(owner_after, depositor.pubkey());
}

#[tokio::test]
async fn test_cancel_non_depositor_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let attacker = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    airdrop(&mut ctx, &attacker.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("deposit");

    let result = cancel_tx(&mut ctx, asset_kp.pubkey(), &attacker).await;
    assert_anchor_error!(result, EscrowError::NotDepositor);
}

// =========================================
// Phase 1 — update_recipient
// =========================================

#[tokio::test]
async fn test_update_recipient() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0xAA),
    )
    .await
    .expect("deposit");

    let nonce_before = fetch_escrow(&mut ctx, asset_kp.pubkey()).await.nonce;

    // Switch protocols (Arweave → Ethereum) and identity.
    let new_addr = ethereum_address_fixture(0x55);
    update_recipient_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        new_addr.clone(),
    )
    .await
    .expect("update_recipient");

    let after = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(after.recipient_protocol, PROTOCOL_ETHEREUM);
    assert_eq!(after.recipient_pubkey_len as usize, ETHEREUM_PUBKEY_LEN);
    assert_eq!(
        &after.recipient_pubkey[..ETHEREUM_PUBKEY_LEN],
        &new_addr[..]
    );
    // Tail must be zeroed — was previously 512 random Arweave bytes.
    assert!(
        after.recipient_pubkey[ETHEREUM_PUBKEY_LEN..]
            .iter()
            .all(|&b| b == 0),
        "old Arweave key bytes leaked past the new active prefix"
    );
    // Nonce rotated (depended on old_nonce).
    assert_ne!(after.nonce, nonce_before, "nonce did not rotate");
}

#[tokio::test]
async fn test_update_recipient_non_depositor_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let attacker = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    airdrop(&mut ctx, &attacker.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("deposit");

    let result = update_recipient_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &attacker,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0x99),
    )
    .await;
    assert_anchor_error!(result, EscrowError::NotDepositor);
}

#[tokio::test]
async fn test_update_recipient_invalid_protocol_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("deposit");

    let result = update_recipient_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        7, // unknown protocol
        vec![0u8; 20],
    )
    .await;
    assert_anchor_error!(result, EscrowError::InvalidRecipientProtocol);
}

// =========================================
// CU baseline measurement
// =========================================

/// Build a deposit_ant transaction without submitting (for CU simulation).
async fn build_deposit_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    depositor: &Keypair,
    protocol: u8,
    pubkey: Vec<u8>,
) -> Transaction {
    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::DepositAnt {
        escrow,
        ant_asset: asset,
        depositor: depositor.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::DepositAnt {
        recipient_protocol: protocol,
        recipient_pubkey: pubkey,
    }
    .data();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    )
}

async fn build_cancel_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    depositor: &Keypair,
) -> Transaction {
    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::CancelDeposit {
        escrow,
        ant_asset: asset,
        depositor: depositor.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::CancelDeposit {}.data();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    )
}

async fn build_update_recipient_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    depositor: &Keypair,
    new_protocol: u8,
    new_pubkey: Vec<u8>,
) -> Transaction {
    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::UpdateRecipient {
        escrow,
        depositor: depositor.pubkey(),
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::UpdateRecipient {
        new_protocol,
        new_pubkey,
    }
    .data();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    )
}

/// Simulate a transaction and return the CU consumed.
async fn simulate_cu(ctx: &mut ProgramTestContext, tx: Transaction) -> u64 {
    let result = ctx
        .banks_client
        .simulate_transaction(tx)
        .await
        .expect("simulate_transaction failed");
    if let Some(ref err) = result.result {
        err.as_ref().expect("simulated tx returned error");
    }
    result
        .simulation_details
        .expect("simulation_details missing")
        .units_consumed
}

#[tokio::test]
async fn measure_cu_deposit_ant() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    let tx = build_deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await;
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] deposit_ant: {}", cu);
    assert!(cu < 200_000, "deposit_ant CU ({}) exceeds 200K budget", cu);
}

#[tokio::test]
async fn measure_cu_cancel_deposit() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("setup deposit");

    let tx = build_cancel_tx(&mut ctx, asset_kp.pubkey(), &depositor).await;
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] cancel_deposit: {}", cu);
    assert!(
        cu < 200_000,
        "cancel_deposit CU ({}) exceeds 200K budget",
        cu
    );
}

#[tokio::test]
async fn measure_cu_update_recipient() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("setup deposit");

    let tx = build_update_recipient_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0x55),
    )
    .await;
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] update_recipient: {}", cu);
    assert!(
        cu < 200_000,
        "update_recipient CU ({}) exceeds 200K budget",
        cu
    );
}

// =========================================
// Claim helpers (crypto signing + tx build)
// =========================================
//
// Legacy RSA-PSS signing helpers (`shared_rsa_keypair`, `sign_arweave`)
// were removed when the on-chain `claim_*_arweave` instructions were
// removed in favor of the off-chain attestor (ADR-017). All Arweave-
// claim tests now use the deterministic test attestor seed `[1u8; 32]`
// to construct Ed25519Program ixs against the program's compiled-in
// `ATTESTOR_PUBKEY`. See `test_attestor_keypair()` below.

/// Build the canonical message bytes matching `canonical.rs::build_ant_escrow_claim_message`.
///
/// `recipient_pubkey_active` is the recipient identity bytes — for Arweave
/// it's the 512-byte RSA modulus, for Ethereum it's the 20-byte address.
/// Used to derive the `recipient: <43-char base64url>` line that binds the
/// canonical message to a specific recipient identity (closes F-1).
fn build_test_canonical(
    ant_mint: &Pubkey,
    claimant: &Pubkey,
    nonce: &[u8; 32],
    recipient_pubkey_active: &[u8],
) -> Vec<u8> {
    let ant_b58 = ant_mint.to_string();
    let claimant_b58 = claimant.to_string();
    let nonce_hex: String = nonce.iter().map(|b| format!("{:02x}", b)).collect();
    let recipient_id =
        ario_ant_escrow::canonical::derive_recipient_id_b64url(recipient_pubkey_active);

    let network = if cfg!(feature = "network-devnet") {
        "solana-devnet"
    } else {
        "solana-mainnet"
    };

    format!(
        "ar.io ant-escrow claim\n\
         network: {}\n\
         recipient: {}\n\
         ant: {}\n\
         claimant: {}\n\
         nonce: {}",
        network, recipient_id, ant_b58, claimant_b58, nonce_hex
    )
    .into_bytes()
}

/// Produce an EIP-191 personal_sign hash and ECDSA signature using libsecp256k1.
fn sign_ethereum(
    canonical_message: &[u8],
    secret_key: &libsecp256k1::SecretKey,
) -> ([u8; 65], [u8; 20]) {
    use anchor_lang::solana_program::keccak::hash as keccak256;

    // EIP-191 wrapping
    let len_str = canonical_message.len().to_string();
    let mut to_hash = Vec::new();
    to_hash.extend_from_slice(b"\x19Ethereum Signed Message:\n");
    to_hash.extend_from_slice(len_str.as_bytes());
    to_hash.extend_from_slice(canonical_message);
    let msg_hash = keccak256(&to_hash).to_bytes();

    let msg = libsecp256k1::Message::parse(&msg_hash);
    let (sig, recovery_id) = libsecp256k1::sign(&msg, secret_key);
    let mut out = [0u8; 65];
    out[..64].copy_from_slice(&sig.serialize());
    out[64] = recovery_id.serialize();

    // Derive address
    let pubkey = libsecp256k1::PublicKey::from_secret_key(secret_key);
    let pk_bytes = pubkey.serialize(); // 65 bytes: 0x04 || X || Y
    let pk_hash = keccak256(&pk_bytes[1..]).to_bytes();
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&pk_hash[12..32]);

    (out, addr)
}

async fn build_claim_ethereum_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    claimant: &Keypair,
    depositor_pubkey: Pubkey,
    payer: &Keypair,
    nonce: [u8; 32],
    signature: [u8; 65],
) -> Transaction {
    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::ClaimAntEthereum {
        escrow,
        ant_asset: asset,
        claimant: claimant.pubkey(),
        depositor: depositor_pubkey,
        payer: payer.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimAntEthereum {
        message_nonce: nonce,
        signature,
    }
    .data();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    )
}

// =========================================
// Claim CU measurements + MEV-resistance test
// =========================================

#[tokio::test]
async fn measure_cu_claim_ethereum() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    // Generate Ethereum keypair.
    let sk_bytes: [u8; 32] = {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((i as u8).wrapping_mul(7).wrapping_add(13)) | 1;
        }
        b
    };
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("setup deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;

    let msg = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let tx = build_claim_ethereum_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &depositor,
        escrow_state.nonce,
        signature,
    )
    .await;

    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] claim_ant_ethereum: {}", cu);
    assert!(
        cu < 200_000,
        "claim_ant_ethereum CU ({}) exceeds 200K budget",
        cu
    );
}

/// MEV-resistance: a third-party payer (not the claimant) submits the claim.
/// The ANT must go to the claimant, not the payer.
#[tokio::test]
async fn test_claim_payer_is_not_claimant() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    let third_party_payer = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    airdrop(&mut ctx, &third_party_payer.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    // Ethereum claim — simpler setup than Arweave.
    let sk_bytes = [42u8; 32];
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    let msg = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    // Payer is third_party_payer, NOT claimant.
    let tx = build_claim_ethereum_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &third_party_payer,
        escrow_state.nonce,
        signature,
    )
    .await;

    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("claim should succeed");

    // ANT must be owned by claimant, NOT by the third_party_payer.
    let owner = read_asset_owner(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(
        owner,
        claimant.pubkey(),
        "ANT should go to claimant, not payer (MEV resistance)"
    );

    // Escrow should be closed.
    let (escrow_addr, _) = escrow_pda(&asset_kp.pubkey());
    let escrow_acct = ctx.banks_client.get_account(escrow_addr).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow should be closed after claim"
    );
}

// =========================================================================
// Token / Vault Escrow Tests
// =========================================================================

// ---- SPL token helpers (pattern from ario-arns integration.rs) ----

async fn create_spl_mint(ctx: &mut ProgramTestContext, mint: &Keypair, authority: &Pubkey) {
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let mint_rent = rent.minimum_balance(spl_token::state::Mint::LEN);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            system_instruction::create_account(
                &ctx.payer.pubkey(),
                &mint.pubkey(),
                mint_rent,
                spl_token::state::Mint::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_mint(
                &spl_token::id(),
                &mint.pubkey(),
                authority,
                None,
                6, // ARIO decimals
            )
            .unwrap(),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, mint],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

async fn create_token_account(
    ctx: &mut ProgramTestContext,
    account: &Keypair,
    mint: &Pubkey,
    owner: &Pubkey,
) {
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account_rent = rent.minimum_balance(spl_token::state::Account::LEN);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            system_instruction::create_account(
                &ctx.payer.pubkey(),
                &account.pubkey(),
                account_rent,
                spl_token::state::Account::LEN as u64,
                &spl_token::id(),
            ),
            spl_token::instruction::initialize_account(
                &spl_token::id(),
                &account.pubkey(),
                mint,
                owner,
            )
            .unwrap(),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, account],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

async fn mint_spl_tokens(
    ctx: &mut ProgramTestContext,
    mint: &Pubkey,
    dest: &Pubkey,
    authority: &Keypair,
    amount: u64,
) {
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[spl_token::instruction::mint_to(
            &spl_token::id(),
            mint,
            dest,
            &authority.pubkey(),
            &[],
            amount,
        )
        .unwrap()],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

async fn get_token_balance(ctx: &mut ProgramTestContext, token_account: &Pubkey) -> u64 {
    use solana_sdk::program_pack::Pack;
    let raw = ctx
        .banks_client
        .get_account(*token_account)
        .await
        .unwrap()
        .expect("token account missing");
    let account = spl_token::state::Account::unpack(&raw.data).unwrap();
    account.amount
}

// ---- PDA helpers for token/vault escrows ----

fn escrow_token_pda(depositor: &Pubkey, asset_id: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ESCROW_TOKEN_SEED, depositor.as_ref(), asset_id],
        &ario_ant_escrow::ID,
    )
}

fn escrow_vault_pda(depositor: &Pubkey, asset_id: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ESCROW_VAULT_SEED, depositor.as_ref(), asset_id],
        &ario_ant_escrow::ID,
    )
}

// ---- escrow canonical message builder (test-side mirror of canonical.rs) ----

fn build_escrow_canonical(
    asset_type: &str,
    asset_id: &[u8; 32],
    amount: u64,
    claimant: &Pubkey,
    nonce: &[u8; 32],
    recipient_pubkey_active: &[u8],
) -> Vec<u8> {
    let asset_hex: String = asset_id.iter().map(|b| format!("{:02x}", b)).collect();
    let claimant_b58 = claimant.to_string();
    let nonce_hex: String = nonce.iter().map(|b| format!("{:02x}", b)).collect();
    let recipient_id =
        ario_ant_escrow::canonical::derive_recipient_id_b64url(recipient_pubkey_active);
    let network = if cfg!(feature = "network-devnet") {
        "solana-devnet"
    } else {
        "solana-mainnet"
    };

    format!(
        "ar.io escrow claim\nnetwork: {}\nrecipient: {}\ntype: {}\nasset: {}\namount: {}\nclaimant: {}\nnonce: {}",
        network, recipient_id, asset_type, asset_hex, amount, claimant_b58, nonce_hex
    )
    .into_bytes()
}

// ---- Fetch helpers ----

async fn fetch_escrow_token(ctx: &mut ProgramTestContext, escrow_addr: Pubkey) -> EscrowToken {
    let raw = ctx
        .banks_client
        .get_account(escrow_addr)
        .await
        .unwrap()
        .expect("escrow token account missing");
    EscrowToken::try_deserialize(&mut &raw.data[..]).expect("decode EscrowToken")
}

// ---- Token scaffolding: set up a mint + depositor ATA + escrow ATA ----

#[allow(dead_code)]
struct TokenSetup {
    mint_kp: Keypair,
    mint_authority: Keypair,
    depositor: Keypair,
    depositor_ata: Keypair,
    asset_id: [u8; 32],
}

async fn setup_token_escrow(ctx: &mut ProgramTestContext, deposit_amount: u64) -> TokenSetup {
    let mint_authority = Keypair::new();
    let mint_kp = Keypair::new();
    let depositor = Keypair::new();
    let depositor_ata = Keypair::new();

    airdrop(ctx, &depositor.pubkey(), 10_000_000_000).await;
    airdrop(ctx, &mint_authority.pubkey(), 1_000_000_000).await;

    // Create SPL mint (6 decimals).
    create_spl_mint(ctx, &mint_kp, &mint_authority.pubkey()).await;

    // Create depositor's token account.
    create_token_account(ctx, &depositor_ata, &mint_kp.pubkey(), &depositor.pubkey()).await;

    // Mint tokens to depositor.
    mint_spl_tokens(
        ctx,
        &mint_kp.pubkey(),
        &depositor_ata.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let asset_id = {
        let mut id = [0u8; 32];
        id[..8].copy_from_slice(b"test-id\0");
        id[8..16].copy_from_slice(&depositor.pubkey().to_bytes()[..8]);
        id
    };

    TokenSetup {
        mint_kp,
        mint_authority,
        depositor,
        depositor_ata,
        asset_id,
    }
}

/// Create the escrow PDA's token account (must exist before deposit_tokens).
async fn create_escrow_token_account(
    ctx: &mut ProgramTestContext,
    escrow_pda: &Pubkey,
    mint: &Pubkey,
) -> Keypair {
    let escrow_ata = Keypair::new();
    create_token_account(ctx, &escrow_ata, mint, escrow_pda).await;
    escrow_ata
}

// ---- Transaction builders ----

async fn deposit_tokens_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    amount: u64,
    protocol: u8,
    pubkey: Vec<u8>,
) -> std::result::Result<(), BanksClientError> {
    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::DepositTokens {
        escrow,
        depositor_token_account: setup.depositor_ata.pubkey(),
        escrow_token_account: escrow_ata_pubkey,
        ario_mint: setup.mint_kp.pubkey(),
        depositor: setup.depositor.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::DepositTokens {
        asset_id: setup.asset_id,
        amount,
        recipient_protocol: protocol,
        recipient_pubkey: pubkey,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&setup.depositor.pubkey()),
        &[&setup.depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

async fn cancel_token_deposit_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    depositor_override: Option<&Keypair>,
) -> std::result::Result<(), BanksClientError> {
    let depositor = depositor_override.unwrap_or(&setup.depositor);
    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::CancelTokenDeposit {
        escrow,
        escrow_token_account: escrow_ata_pubkey,
        depositor_token_account: setup.depositor_ata.pubkey(),
        depositor: depositor.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::CancelTokenDeposit {}.data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

async fn update_token_recipient_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    depositor_override: Option<&Keypair>,
    new_protocol: u8,
    new_pubkey: Vec<u8>,
) -> std::result::Result<(), BanksClientError> {
    let depositor = depositor_override.unwrap_or(&setup.depositor);
    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::UpdateTokenRecipient {
        escrow,
        depositor: depositor.pubkey(),
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::UpdateTokenRecipient {
        new_protocol,
        new_pubkey,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

async fn claim_tokens_ethereum_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    claimant: &Keypair,
    claimant_ata_pubkey: Pubkey,
    payer: &Keypair,
    nonce: [u8; 32],
    signature: [u8; 65],
) -> std::result::Result<(), BanksClientError> {
    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::ClaimTokensEthereum {
        escrow,
        escrow_token_account: escrow_ata_pubkey,
        claimant_token_account: claimant_ata_pubkey,
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: payer.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimTokensEthereum {
        message_nonce: nonce,
        signature,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

// ---- Vault deposit helper ----

async fn deposit_vault_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    amount: u64,
    lock_duration_seconds: i64,
    revocable: bool,
    protocol: u8,
    pubkey: Vec<u8>,
) -> std::result::Result<(), BanksClientError> {
    let (escrow, _bump) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::DepositVault {
        escrow,
        depositor_token_account: setup.depositor_ata.pubkey(),
        escrow_token_account: escrow_ata_pubkey,
        ario_mint: setup.mint_kp.pubkey(),
        depositor: setup.depositor.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::DepositVault {
        asset_id: setup.asset_id,
        amount,
        lock_duration_seconds,
        revocable,
        recipient_protocol: protocol,
        recipient_pubkey: pubkey,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&setup.depositor.pubkey()),
        &[&setup.depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

// ---- Build-only (for CU measurement) ----

async fn build_deposit_tokens_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    amount: u64,
    protocol: u8,
    pubkey: Vec<u8>,
) -> Transaction {
    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::DepositTokens {
        escrow,
        depositor_token_account: setup.depositor_ata.pubkey(),
        escrow_token_account: escrow_ata_pubkey,
        ario_mint: setup.mint_kp.pubkey(),
        depositor: setup.depositor.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::DepositTokens {
        asset_id: setup.asset_id,
        amount,
        recipient_protocol: protocol,
        recipient_pubkey: pubkey,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&setup.depositor.pubkey()),
        &[&setup.depositor],
        blockhash,
    )
}

async fn build_cancel_token_deposit_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
) -> Transaction {
    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::CancelTokenDeposit {
        escrow,
        escrow_token_account: escrow_ata_pubkey,
        depositor_token_account: setup.depositor_ata.pubkey(),
        depositor: setup.depositor.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::CancelTokenDeposit {}.data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&setup.depositor.pubkey()),
        &[&setup.depositor],
        blockhash,
    )
}

async fn build_claim_tokens_ethereum_tx_sim(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    claimant: &Keypair,
    claimant_ata_pubkey: Pubkey,
    payer: &Keypair,
    nonce: [u8; 32],
    signature: [u8; 65],
) -> Transaction {
    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::ClaimTokensEthereum {
        escrow,
        escrow_token_account: escrow_ata_pubkey,
        claimant_token_account: claimant_ata_pubkey,
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: payer.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimTokensEthereum {
        message_nonce: nonce,
        signature,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    )
}

// =========================================
// Token Escrow — deposit_tokens
// =========================================

#[tokio::test]
async fn test_deposit_tokens_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64; // 1000 ARIO
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let eth_addr = ethereum_address_fixture(0xBB);
    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        eth_addr.clone(),
    )
    .await
    .expect("deposit_tokens should succeed");

    // Verify PDA state.
    let escrow = fetch_escrow_token(&mut ctx, escrow_addr).await;
    assert_eq!(escrow.version, 1);
    assert_eq!(escrow.depositor, setup.depositor.pubkey());
    assert_eq!(escrow.asset_type, ASSET_TYPE_TOKEN);
    assert_eq!(escrow.amount, amount);
    assert_eq!(escrow.ario_mint, setup.mint_kp.pubkey());
    assert_eq!(escrow.asset_id, setup.asset_id);
    assert_eq!(escrow.recipient_protocol, PROTOCOL_ETHEREUM);
    assert_eq!(escrow.recipient_pubkey_len as usize, ETHEREUM_PUBKEY_LEN);
    assert_eq!(
        &escrow.recipient_pubkey[..ETHEREUM_PUBKEY_LEN],
        &eth_addr[..]
    );
    // Tail zeroed.
    assert!(escrow.recipient_pubkey[ETHEREUM_PUBKEY_LEN..]
        .iter()
        .all(|&b| b == 0));
    // Vault fields default.
    assert_eq!(escrow.vault_end_timestamp, 0);
    assert!(!escrow.vault_revocable);

    // Nonce is reproducible.
    let expected_nonce = derive_initial_nonce(
        escrow.deposit_slot,
        &Pubkey::new_from_array(setup.asset_id),
        &setup.depositor.pubkey(),
    );
    assert_eq!(escrow.nonce, expected_nonce);

    // Token balances: depositor should be 0, escrow should hold `amount`.
    let depositor_bal = get_token_balance(&mut ctx, &setup.depositor_ata.pubkey()).await;
    assert_eq!(
        depositor_bal, 0,
        "depositor should have 0 tokens after deposit"
    );
    let escrow_bal = get_token_balance(&mut ctx, &escrow_ata.pubkey()).await;
    assert_eq!(
        escrow_bal, amount,
        "escrow ATA should hold deposited tokens"
    );
}

#[tokio::test]
async fn test_deposit_tokens_zero_amount_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let setup = setup_token_escrow(&mut ctx, 1_000_000_000).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let result = deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        0, // zero amount
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xCC),
    )
    .await;
    assert_anchor_error!(result, EscrowError::AmountZero);
}

#[tokio::test]
async fn test_deposit_tokens_arweave_protocol() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64; // 500 ARIO
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let ar_pubkey = arweave_pubkey_fixture(0xDD);
    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ARWEAVE,
        ar_pubkey.clone(),
    )
    .await
    .expect("deposit_tokens arweave should succeed");

    let escrow = fetch_escrow_token(&mut ctx, escrow_addr).await;
    assert_eq!(escrow.recipient_protocol, PROTOCOL_ARWEAVE);
    assert_eq!(escrow.recipient_pubkey_len as usize, ARWEAVE_PUBKEY_LEN);
    assert_eq!(
        &escrow.recipient_pubkey[..ARWEAVE_PUBKEY_LEN],
        &ar_pubkey[..]
    );
}

// =========================================
// Token Escrow — cancel_token_deposit
// =========================================

#[tokio::test]
async fn test_cancel_token_deposit() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await
    .expect("deposit");

    // Cancel
    cancel_token_deposit_tx(&mut ctx, &setup, escrow_ata.pubkey(), None)
        .await
        .expect("cancel");

    // Escrow PDA closed.
    let escrow_acct = ctx.banks_client.get_account(escrow_addr).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after cancel"
    );

    // Tokens returned to depositor.
    let depositor_bal = get_token_balance(&mut ctx, &setup.depositor_ata.pubkey()).await;
    assert_eq!(
        depositor_bal, amount,
        "tokens should be returned to depositor"
    );

    // Escrow token account closed (will fail to fetch).
    let escrow_ata_acct = ctx
        .banks_client
        .get_account(escrow_ata.pubkey())
        .await
        .unwrap();
    assert!(
        escrow_ata_acct.is_none(),
        "escrow token account should be closed after cancel"
    );
}

#[tokio::test]
async fn test_cancel_token_non_depositor_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await
    .expect("deposit");

    // Attacker tries to cancel.
    let attacker = Keypair::new();
    airdrop(&mut ctx, &attacker.pubkey(), 5_000_000_000).await;
    let result =
        cancel_token_deposit_tx(&mut ctx, &setup, escrow_ata.pubkey(), Some(&attacker)).await;
    assert_anchor_error!(result, EscrowError::NotDepositor);
}

// =========================================
// Token Escrow — update_token_recipient
// =========================================

#[tokio::test]
async fn test_update_token_recipient() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0xAA),
    )
    .await
    .expect("deposit");

    let nonce_before = fetch_escrow_token(&mut ctx, escrow_addr).await.nonce;

    // Switch Arweave -> Ethereum.
    let new_addr = ethereum_address_fixture(0x55);
    update_token_recipient_tx(&mut ctx, &setup, None, PROTOCOL_ETHEREUM, new_addr.clone())
        .await
        .expect("update_token_recipient");

    let after = fetch_escrow_token(&mut ctx, escrow_addr).await;
    assert_eq!(after.recipient_protocol, PROTOCOL_ETHEREUM);
    assert_eq!(after.recipient_pubkey_len as usize, ETHEREUM_PUBKEY_LEN);
    assert_eq!(
        &after.recipient_pubkey[..ETHEREUM_PUBKEY_LEN],
        &new_addr[..]
    );
    // Tail zeroed (was previously 512 Arweave bytes).
    assert!(
        after.recipient_pubkey[ETHEREUM_PUBKEY_LEN..]
            .iter()
            .all(|&b| b == 0),
        "old Arweave key bytes leaked past the new active prefix"
    );
    // Nonce rotated.
    assert_ne!(after.nonce, nonce_before, "nonce did not rotate");
}

// =========================================
// Token Escrow — claim_tokens_ethereum
// =========================================

#[tokio::test]
async fn test_claim_tokens_ethereum_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64; // 1000 ARIO
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    // Generate Ethereum keypair.
    let sk_bytes: [u8; 32] = {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((i as u8).wrapping_mul(7).wrapping_add(13)) | 1;
        }
        b
    };
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;

    // Create claimant and their token account.
    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    // Build escrow canonical message and sign.
    let msg = build_escrow_canonical(
        "token",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    claim_tokens_ethereum_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        escrow_state.nonce,
        signature,
    )
    .await
    .expect("claim_tokens_ethereum should succeed");

    // Verify claimant received the tokens.
    let claimant_bal = get_token_balance(&mut ctx, &claimant_ata.pubkey()).await;
    assert_eq!(claimant_bal, amount, "claimant should have received tokens");

    // Escrow PDA closed.
    let escrow_acct = ctx.banks_client.get_account(escrow_addr).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after claim"
    );
}

#[tokio::test]
async fn test_claim_tokens_protocol_mismatch_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    // Deposit for ARWEAVE protocol. Real modulus content is irrelevant
    // for this test — we never claim through the Arweave path here, we
    // just exercise the protocol-mismatch guard on `claim_tokens_ethereum`.
    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("deposit with arweave protocol");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;

    // Now try to claim via Ethereum path -- should fail with ProtocolMismatch.
    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    // Fabricate an Ethereum signature (content doesn't matter -- protocol check fires first).
    let fake_sig = [0u8; 65];
    let result = claim_tokens_ethereum_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        escrow_state.nonce,
        fake_sig,
    )
    .await;
    assert_anchor_error!(result, EscrowError::ProtocolMismatch);
}

// =========================================
// Vault Escrow — deposit_vault
// =========================================

#[tokio::test]
async fn test_deposit_vault_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64; // 500 ARIO (above 100 ARIO minimum)
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let lock_duration = 30 * 86_400i64; // 30 days
    let eth_addr = ethereum_address_fixture(0xEE);

    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
        true, // revocable
        PROTOCOL_ETHEREUM,
        eth_addr.clone(),
    )
    .await
    .expect("deposit_vault should succeed");

    let escrow = fetch_escrow_token(&mut ctx, escrow_addr).await;
    assert_eq!(escrow.version, 1);
    assert_eq!(escrow.depositor, setup.depositor.pubkey());
    assert_eq!(escrow.asset_type, ASSET_TYPE_VAULT);
    assert_eq!(escrow.amount, amount);
    assert_eq!(escrow.recipient_protocol, PROTOCOL_ETHEREUM);
    assert!(escrow.vault_revocable);
    // vault_end_timestamp should be clock.unix_timestamp + lock_duration.
    // We can't know the exact clock time, but it should be > 0 and in the future.
    assert!(
        escrow.vault_end_timestamp > 0,
        "vault_end_timestamp should be set"
    );

    // Token balances.
    let depositor_bal = get_token_balance(&mut ctx, &setup.depositor_ata.pubkey()).await;
    assert_eq!(depositor_bal, 0);
    let escrow_bal = get_token_balance(&mut ctx, &escrow_ata.pubkey()).await;
    assert_eq!(escrow_bal, amount);
}

#[tokio::test]
async fn test_deposit_vault_duration_too_short_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let result = deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        86_400, // 1 day -- below 14-day minimum
        false,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await;
    assert_anchor_error!(result, EscrowError::VaultDurationTooShort);
}

#[tokio::test]
async fn test_deposit_vault_amount_too_low_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 50_000_000u64; // 50 ARIO -- below 100 ARIO minimum
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let result = deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400, // 30 days (valid duration)
        false,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xBB),
    )
    .await;
    assert_anchor_error!(result, EscrowError::VaultAmountBelowMinimum);
}

#[tokio::test]
async fn test_deposit_vault_zero_amount_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let setup = setup_token_escrow(&mut ctx, 1_000_000_000).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let result = deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        0, // zero amount
        30 * 86_400,
        false,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xCC),
    )
    .await;
    assert_anchor_error!(result, EscrowError::AmountZero);
}

// =========================================
// Vault Escrow — cancel_vault_deposit + update_vault_recipient
// =========================================
//
// The vault-side cancel/update handlers are byte-equivalent to their token
// counterparts except for the PDA seed (`ESCROW_VAULT_SEED` vs
// `ESCROW_TOKEN_SEED`) and a different on-chain Anchor discriminator.
// Coverage parity matters because a Codama regen could silently drift the
// vault-variant accounts struct independently of the token variant.

async fn cancel_vault_deposit_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    depositor_override: Option<&Keypair>,
) -> std::result::Result<(), BanksClientError> {
    let depositor = depositor_override.unwrap_or(&setup.depositor);
    let (escrow, _bump) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::CancelVaultDeposit {
        escrow,
        escrow_token_account: escrow_ata_pubkey,
        depositor_token_account: setup.depositor_ata.pubkey(),
        depositor: depositor.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::CancelVaultDeposit {}.data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

async fn update_vault_recipient_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    depositor_override: Option<&Keypair>,
    new_protocol: u8,
    new_pubkey: Vec<u8>,
) -> std::result::Result<(), BanksClientError> {
    let depositor = depositor_override.unwrap_or(&setup.depositor);
    let (escrow, _bump) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::UpdateVaultRecipient {
        escrow,
        depositor: depositor.pubkey(),
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::UpdateVaultRecipient {
        new_protocol,
        new_pubkey,
    }
    .data();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn test_cancel_vault_deposit() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400,
        true, // revocable — irrelevant for cancel by depositor, but exercises the path
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await
    .expect("deposit_vault");

    cancel_vault_deposit_tx(&mut ctx, &setup, escrow_ata.pubkey(), None)
        .await
        .expect("cancel_vault_deposit should succeed for the depositor");

    // Escrow PDA closed.
    let escrow_acct = ctx.banks_client.get_account(escrow_addr).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after vault cancel"
    );
    // Tokens returned to depositor.
    let depositor_bal = get_token_balance(&mut ctx, &setup.depositor_ata.pubkey()).await;
    assert_eq!(depositor_bal, amount);
    // Escrow token account closed.
    let escrow_ata_acct = ctx
        .banks_client
        .get_account(escrow_ata.pubkey())
        .await
        .unwrap();
    assert!(
        escrow_ata_acct.is_none(),
        "escrow token account should be closed"
    );
}

#[tokio::test]
async fn test_cancel_vault_non_depositor_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400,
        false,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await
    .expect("deposit_vault");

    let attacker = Keypair::new();
    airdrop(&mut ctx, &attacker.pubkey(), 5_000_000_000).await;
    let result =
        cancel_vault_deposit_tx(&mut ctx, &setup, escrow_ata.pubkey(), Some(&attacker)).await;
    assert_anchor_error!(result, EscrowError::NotDepositor);
}

#[tokio::test]
async fn test_update_vault_recipient() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400,
        true,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0xAA),
    )
    .await
    .expect("deposit_vault");

    let nonce_before = fetch_escrow_token(&mut ctx, escrow_addr).await.nonce;

    let new_addr = ethereum_address_fixture(0x77);
    update_vault_recipient_tx(&mut ctx, &setup, None, PROTOCOL_ETHEREUM, new_addr.clone())
        .await
        .expect("update_vault_recipient");

    let after = fetch_escrow_token(&mut ctx, escrow_addr).await;
    assert_eq!(after.recipient_protocol, PROTOCOL_ETHEREUM);
    assert_eq!(after.recipient_pubkey_len as usize, ETHEREUM_PUBKEY_LEN);
    assert_eq!(
        &after.recipient_pubkey[..ETHEREUM_PUBKEY_LEN],
        &new_addr[..]
    );
    assert!(
        after.recipient_pubkey[ETHEREUM_PUBKEY_LEN..]
            .iter()
            .all(|&b| b == 0),
        "old Arweave key bytes leaked past the new active prefix"
    );
    assert_ne!(after.nonce, nonce_before, "nonce did not rotate");
    // Vault-specific fields untouched by recipient update.
    assert_eq!(after.amount, amount);
    assert!(after.vault_revocable);
    assert!(after.vault_end_timestamp > 0);
}

#[tokio::test]
async fn test_update_vault_recipient_non_depositor_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400,
        false,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await
    .expect("deposit_vault");

    let attacker = Keypair::new();
    airdrop(&mut ctx, &attacker.pubkey(), 5_000_000_000).await;
    let result = update_vault_recipient_tx(
        &mut ctx,
        &setup,
        Some(&attacker),
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xBB),
    )
    .await;
    assert_anchor_error!(result, EscrowError::NotDepositor);
}

// =========================================
// Vault Escrow — claim_vault_{arweave,ethereum}
// =========================================
//
// These tests cover two of the three claim paths the on-chain handler
// implements:
//
//  - **Expired-vault path** (happy): the on-chain handler routes the SPL
//    transfer straight to `claimant_token_account` and does NOT consult
//    `vault_introspect`. We warp the test clock past `vault_end_timestamp`
//    and assert the claim succeeds without any sibling instruction.
//
//  - **Active-vault path** (negative): the on-chain handler MUST find a
//    matching `ario_core::vaulted_transfer` sibling in the same tx via
//    `vault_introspect::verify_vaulted_transfer_in_tx`. We submit the
//    claim while the vault is still active without that sibling and
//    assert the on-chain handler rejects with
//    `MissingVaultedTransferInstruction`.
//
// The **active-vault happy** path (claim + sibling vaulted_transfer in
// the same tx, tokens re-locked into a new vault for the claimant) is
// not exercised here — it requires `ario_core` to be deployed and
// initialised inside this program-test runtime, which is a substantial
// new test-infra expansion. End-to-end coverage of that path lives in
// the SDK localnet suite (`sdk/src/solana/escrow-tokens.localnet.test.ts`)
// where Surfpool already has all programs deployed.

async fn claim_vault_ethereum_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    claimant: &Keypair,
    claimant_ata_pubkey: Pubkey,
    payer: &Keypair,
    payer_ata_pubkey: Pubkey,
    nonce: [u8; 32],
    signature: [u8; 65],
    extra_ixs: Vec<Ix>,
) -> std::result::Result<(), BanksClientError> {
    let (escrow, _bump) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::ClaimVaultEthereum {
        escrow,
        escrow_token_account: escrow_ata_pubkey,
        claimant_token_account: claimant_ata_pubkey,
        payer_token_account: payer_ata_pubkey,
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: payer.pubkey(),
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimVaultEthereum {
        message_nonce: nonce,
        signature,
    }
    .data();

    let mut ixs =
        vec![solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000)];
    ixs.extend(extra_ixs);
    ixs.push(Ix {
        program_id: ario_ant_escrow::ID,
        accounts,
        data,
    });

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[payer], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

/// Warp the program-test clock so `unix_timestamp >= target`. The escrow
/// claim handler reads `Clock::get()` directly, so this is the only knob
/// that affects the expired-vs-active branch.
async fn warp_clock_to(ctx: &mut ProgramTestContext, target: i64) {
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    if clock.unix_timestamp < target {
        clock.unix_timestamp = target;
        ctx.set_sysvar(&clock);
    }
}

#[tokio::test]
async fn test_claim_vault_ethereum_expired_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let sk_bytes: [u8; 32] = {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((i as u8).wrapping_mul(11).wrapping_add(7)) | 1;
        }
        b
    };
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400,
        false,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("deposit_vault should succeed");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;
    warp_clock_to(&mut ctx, escrow_state.vault_end_timestamp + 1).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    let payer_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;
    create_token_account(
        &mut ctx,
        &payer_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let msg = build_escrow_canonical(
        "vault",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    claim_vault_ethereum_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        payer_ata.pubkey(),
        escrow_state.nonce,
        signature,
        vec![],
    )
    .await
    .expect("expired-vault ethereum claim should succeed without sibling");

    let claimant_bal = get_token_balance(&mut ctx, &claimant_ata.pubkey()).await;
    assert_eq!(claimant_bal, amount);
}

#[tokio::test]
async fn test_claim_vault_ethereum_active_without_sibling_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let sk_bytes: [u8; 32] = {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((i as u8).wrapping_mul(13).wrapping_add(3)) | 1;
        }
        b
    };
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400,
        false,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("deposit_vault should succeed");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    let payer_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;
    create_token_account(
        &mut ctx,
        &payer_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let msg = build_escrow_canonical(
        "vault",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let result = claim_vault_ethereum_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        payer_ata.pubkey(),
        escrow_state.nonce,
        signature,
        vec![],
    )
    .await;
    assert_anchor_error!(result, EscrowError::MissingVaultedTransferInstruction);
}

// =========================================
// CU measurements — token escrow
// =========================================

#[tokio::test]
async fn measure_cu_deposit_tokens() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let tx = build_deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await;
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] deposit_tokens: {}", cu);
    assert!(
        cu < 200_000,
        "deposit_tokens CU ({}) exceeds 200K budget",
        cu
    );
}

#[tokio::test]
async fn measure_cu_cancel_token_deposit() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await
    .expect("setup deposit");

    let tx = build_cancel_token_deposit_tx(&mut ctx, &setup, escrow_ata.pubkey()).await;
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] cancel_token_deposit: {}", cu);
    assert!(
        cu < 200_000,
        "cancel_token_deposit CU ({}) exceeds 200K budget",
        cu
    );
}

#[tokio::test]
async fn measure_cu_claim_tokens_ethereum() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let sk_bytes: [u8; 32] = {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((i as u8).wrapping_mul(7).wrapping_add(13)) | 1;
        }
        b
    };
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("setup deposit");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let msg = build_escrow_canonical(
        "token",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let tx = build_claim_tokens_ethereum_tx_sim(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        escrow_state.nonce,
        signature,
    )
    .await;
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] claim_tokens_ethereum: {}", cu);
    assert!(
        cu < 200_000,
        "claim_tokens_ethereum CU ({}) exceeds 200K budget",
        cu
    );
}

// =========================================
// Token Escrow — MEV resistance (payer != claimant)
// =========================================

#[tokio::test]
async fn test_claim_tokens_payer_is_not_claimant() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let sk_bytes = [42u8; 32];
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    let third_party_payer = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 1_000_000_000).await;
    airdrop(&mut ctx, &third_party_payer.pubkey(), 5_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let msg = build_escrow_canonical(
        "token",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    // Payer is third_party_payer, NOT claimant.
    claim_tokens_ethereum_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &third_party_payer,
        escrow_state.nonce,
        signature,
    )
    .await
    .expect("claim should succeed");

    // Tokens must go to claimant, not the payer.
    let claimant_bal = get_token_balance(&mut ctx, &claimant_ata.pubkey()).await;
    assert_eq!(
        claimant_bal, amount,
        "tokens should go to claimant, not payer (MEV resistance)"
    );
}

// =========================================
// Token Escrow — nonce mismatch
// =========================================

#[tokio::test]
async fn test_claim_tokens_nonce_mismatch_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let sk_bytes = [42u8; 32];
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("deposit");

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    // Use a wrong nonce.
    let bad_nonce = [0xFFu8; 32];
    let msg = build_escrow_canonical(
        "token",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &bad_nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let result = claim_tokens_ethereum_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        bad_nonce,
        signature,
    )
    .await;
    assert_anchor_error!(result, EscrowError::NonceMismatch);
}

// =========================================
// ANT Escrow — UpdateAuthority transfer
// =========================================

#[tokio::test]
async fn test_claim_transfers_update_authority_to_claimant() {
    // Audit L23: post-claim, the ANT's UpdateAuthority must belong to the
    // claimant. Without the deposit-side UpdateV1 and the matching claim-side
    // UpdateV1, UA would stay with the depositor — who could then UpdateV1
    // the metadata URI on an asset they no longer own.
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    // Sanity: pre-deposit, UA belongs to the depositor.
    let pre_ua = read_asset_update_authority(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(
        pre_ua,
        Some(depositor.pubkey()),
        "depositor must hold UA before deposit"
    );

    // Use Ethereum claim path (simpler signature setup than RSA).
    let sk_bytes = [42u8; 32];
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("deposit");

    // After deposit: UA belongs to the escrow PDA (custodied).
    let post_deposit_ua = read_asset_update_authority(&mut ctx, asset_kp.pubkey()).await;
    let (escrow_addr, _) = escrow_pda(&asset_kp.pubkey());
    assert_eq!(
        post_deposit_ua,
        Some(escrow_addr),
        "escrow PDA must hold UA between deposit and claim"
    );

    // Claim. After this, UA should be the claimant.
    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    let msg = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);
    let tx = build_claim_ethereum_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &depositor, // payer
        escrow_state.nonce,
        signature,
    )
    .await;
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("claim should succeed");

    let post_claim_owner = read_asset_owner(&mut ctx, asset_kp.pubkey()).await;
    let post_claim_ua = read_asset_update_authority(&mut ctx, asset_kp.pubkey()).await;

    assert_eq!(
        post_claim_owner,
        claimant.pubkey(),
        "Owner must be claimant"
    );
    assert_eq!(
        post_claim_ua,
        Some(claimant.pubkey()),
        "UpdateAuthority must be claimant — depositor must not retain UA \
         (audit L23: enables post-claim metadata URI rewrite)"
    );
    assert_ne!(
        post_claim_ua,
        Some(depositor.pubkey()),
        "regression: depositor still holds UA — would enable post-claim URI rewrite"
    );
}

#[tokio::test]
async fn test_cancel_returns_update_authority_to_depositor() {
    // Mirror of test_claim_transfers_update_authority_to_claimant for the
    // cancel path: deposit → cancel must restore UA to the depositor.
    // Otherwise the depositor would lose the ability to update their own
    // asset's metadata URI after a cancelled deposit.
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0xCD),
    )
    .await
    .expect("deposit");

    cancel_tx(&mut ctx, asset_kp.pubkey(), &depositor)
        .await
        .expect("cancel should succeed");

    let post_cancel_owner = read_asset_owner(&mut ctx, asset_kp.pubkey()).await;
    let post_cancel_ua = read_asset_update_authority(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(
        post_cancel_owner,
        depositor.pubkey(),
        "Owner returns to depositor"
    );
    assert_eq!(
        post_cancel_ua,
        Some(depositor.pubkey()),
        "UpdateAuthority returns to depositor — must not stay at the closed escrow PDA"
    );
}

// =========================================
// ANT Escrow — claim_ant_arweave happy path
// =========================================

// =========================================
// claim_ant_arweave_attested — Ed25519 attestation path
// =========================================
//
// Deterministic test attestor: secret seed [1u8; 32]. The matching
// public key is baked into ario_ant_escrow::state::ATTESTOR_PUBKEY
// (base58: AKnL4NNf3DGWZJS6cPknBuEGnVsV4A4m5tgebLHaRSZ9). Tests sign
// with this seed; the on-chain ix verifies via instruction
// introspection of the Ed25519Program native sigverify ix.

const TEST_ATTESTOR_SEED: [u8; 32] = [1u8; 32];

/// Construct an `ed25519_dalek::Keypair` from `TEST_ATTESTOR_SEED`.
/// We rebuild it per-test since `Keypair` isn't `Clone`.
fn test_attestor_keypair() -> ed25519_dalek::Keypair {
    let secret = ed25519_dalek::SecretKey::from_bytes(&TEST_ATTESTOR_SEED).expect("32-byte secret");
    let public: ed25519_dalek::PublicKey = (&secret).into();
    ed25519_dalek::Keypair { secret, public }
}

/// Build a Solana `Ed25519Program` sigverify instruction with all data
/// inline (pubkey + sig + message in the ix's own data buffer).
///
/// Mirrors `solana-sdk`'s `ed25519_instruction::new_ed25519_instruction_raw`,
/// which is `#[cfg(test)]`-only inside that crate so we can't call it.
/// Layout reference: agave/programs/ed25519-program/src/lib.rs.
fn build_ed25519_sigverify_ix(pubkey: &[u8; 32], signature: &[u8; 64], message: &[u8]) -> Ix {
    const HEADER_LEN: usize = 16;
    const PK_OFFSET: u16 = HEADER_LEN as u16;
    const SIG_OFFSET: u16 = PK_OFFSET + 32;
    const MSG_OFFSET: u16 = SIG_OFFSET + 64;
    const SAME_IX: u16 = 0xFFFF;

    let mut data = Vec::with_capacity(HEADER_LEN + 32 + 64 + message.len());
    data.push(1u8); // num_signatures
    data.push(0u8); // padding
    data.extend_from_slice(&SIG_OFFSET.to_le_bytes());
    data.extend_from_slice(&SAME_IX.to_le_bytes());
    data.extend_from_slice(&PK_OFFSET.to_le_bytes());
    data.extend_from_slice(&SAME_IX.to_le_bytes());
    data.extend_from_slice(&MSG_OFFSET.to_le_bytes());
    data.extend_from_slice(&(message.len() as u16).to_le_bytes());
    data.extend_from_slice(&SAME_IX.to_le_bytes());
    data.extend_from_slice(pubkey);
    data.extend_from_slice(signature);
    data.extend_from_slice(message);

    Ix {
        program_id: solana_sdk::ed25519_program::ID,
        accounts: vec![],
        data,
    }
}

/// Build a transaction containing:
///   1. Ed25519Program sigverify ix (verifies attestor sig over canonical message)
///   2. claim_ant_arweave_attested ix
async fn build_claim_arweave_attested_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    claimant: &Keypair,
    depositor_pubkey: Pubkey,
    payer: &Keypair,
    nonce: [u8; 32],
    canonical_message: &[u8],
    ed25519_signature: [u8; 64],
) -> Transaction {
    let attestor_pubkey_bytes: [u8; 32] = ario_ant_escrow::state::ATTESTOR_PUBKEY.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(
        &attestor_pubkey_bytes,
        &ed25519_signature,
        canonical_message,
    );

    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::ClaimAntArweaveAttested {
        escrow,
        ant_asset: asset,
        claimant: claimant.pubkey(),
        depositor: depositor_pubkey,
        payer: payer.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimAntArweaveAttested {
        message_nonce: nonce,
    }
    .data();
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts,
        data,
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        // ORDER MATTERS: Ed25519Program ix must immediately precede the
        // claim ix. The introspection helper rejects any other layout.
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            ed25519_ix,
            claim_ix,
        ],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    )
}

#[tokio::test]
async fn test_claim_ant_arweave_attested_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    // Use any 512-byte recipient pubkey for the deposit — the attested
    // claim path consults `recipient_pubkey_active` to derive the
    // canonical message's `recipient` field — closing F-1, which is
    // why the attestor's canonical (built from client-supplied modulus)
    // and the on-chain canonical (built from escrow state) diverge if
    // the attacker tries to substitute their own modulus.
    let modulus_bytes = [0xAAu8; 512];
    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        modulus_bytes.to_vec(),
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;

    // Build canonical message + sign with attestor's Ed25519 key.
    let canonical = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &modulus_bytes,
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig = kp.sign(&canonical);
    let sig_bytes: [u8; 64] = sig.to_bytes();

    let tx = build_claim_arweave_attested_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &depositor,
        escrow_state.nonce,
        &canonical,
        sig_bytes,
    )
    .await;
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("claim_ant_arweave_attested should succeed");

    // Post-claim assertions: claimant owns the ANT, escrow PDA is closed,
    // UpdateAuthority transferred.
    let post_owner = read_asset_owner(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(
        post_owner,
        claimant.pubkey(),
        "claimant must own ANT after attested claim"
    );
    let (escrow_addr, _) = escrow_pda(&asset_kp.pubkey());
    let escrow_acct = ctx.banks_client.get_account(escrow_addr).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow should be closed after attested claim"
    );
    let post_ua = read_asset_update_authority(&mut ctx, asset_kp.pubkey()).await;
    assert_eq!(
        post_ua,
        Some(claimant.pubkey()),
        "UpdateAuthority must transfer to claimant after attested claim"
    );
}

#[tokio::test]
async fn test_claim_ant_arweave_attested_rejects_wrong_attestor() {
    // A signature from a key OTHER than ATTESTOR_PUBKEY must not pass.
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    let canonical = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &[0xAAu8; 512],
    );

    // Sign with a DIFFERENT seed — pubkey will not match ATTESTOR_PUBKEY.
    let bogus_seed = [9u8; 32];
    let secret = ed25519_dalek::SecretKey::from_bytes(&bogus_seed).unwrap();
    let public: ed25519_dalek::PublicKey = (&secret).into();
    let kp = ed25519_dalek::Keypair { secret, public };
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    // Build the Ed25519Program ix with the WRONG pubkey (matches our sig)
    // so the sigverify itself succeeds but our on-chain check rejects.
    let wrong_pubkey_bytes: [u8; 32] = kp.public.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(&wrong_pubkey_bytes, &sig_bytes, &canonical);

    let (escrow, _bump) = escrow_pda(&asset_kp.pubkey());
    let claim_accounts = ario_ant_escrow::accounts::ClaimAntArweaveAttested {
        escrow,
        ant_asset: asset_kp.pubkey(),
        claimant: claimant.pubkey(),
        depositor: depositor.pubkey(),
        payer: depositor.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts: claim_accounts,
        data: ario_ant_escrow::instruction::ClaimAntArweaveAttested {
            message_nonce: escrow_state.nonce,
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            ed25519_ix,
            claim_ix,
        ],
        Some(&depositor.pubkey()),
        &[&depositor],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "claim with wrong attestor pubkey must fail"
    );
}

#[tokio::test]
async fn test_claim_ant_arweave_attested_rejects_missing_sigverify_ix() {
    // Without the preceding Ed25519Program ix, introspection fails.
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    let (escrow, _bump) = escrow_pda(&asset_kp.pubkey());
    let claim_accounts = ario_ant_escrow::accounts::ClaimAntArweaveAttested {
        escrow,
        ant_asset: asset_kp.pubkey(),
        claimant: claimant.pubkey(),
        depositor: depositor.pubkey(),
        payer: depositor.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);

    // Build tx WITHOUT the Ed25519Program ix
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            Ix {
                program_id: ario_ant_escrow::ID,
                accounts: claim_accounts,
                data: ario_ant_escrow::instruction::ClaimAntArweaveAttested {
                    message_nonce: escrow_state.nonce,
                }
                .data(),
            },
        ],
        Some(&depositor.pubkey()),
        &[&depositor],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(result.is_err(), "claim without sigverify ix must fail");
}

#[tokio::test]
async fn test_claim_ant_arweave_attested_rejects_message_mismatch() {
    // Attestor signs message A, but tx-time canonical (from escrow state)
    // resolves to message B. On-chain introspection must reject.
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;

    // Sign over canonical for a DIFFERENT claimant
    let other_claimant = Keypair::new();
    let canonical_wrong = build_test_canonical(
        &asset_kp.pubkey(),
        &other_claimant.pubkey(), // wrong claimant
        &escrow_state.nonce,
        &[0xAAu8; 512],
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical_wrong).to_bytes();

    // Submit with the actual claimant (who is NOT the one signed over).
    // The Ed25519Program ix has canonical_wrong, but the claim ix
    // reconstructs canonical with `claimant`, mismatch.
    let tx = build_claim_arweave_attested_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &depositor,
        escrow_state.nonce,
        &canonical_wrong,
        sig_bytes,
    )
    .await;
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "claim with attested message != reconstructed canonical must fail"
    );
}

/// F-1 regression: an attestation built from a *different* RSA
/// modulus than the one stored at deposit time must be rejected.
///
/// Attack scenario this guards against:
///   1. Alice deposits an ANT into escrow with `recipient_pubkey = M_alice`.
///   2. Eve generates her own RSA-4096 key (M_eve, K_eve).
///   3. Eve gets the attestor to sign the canonical message it built
///      from her client-supplied modulus M_eve.
///   4. Eve submits the attestation on-chain pointing at her own
///      claimant.
///
/// Pre-fix: the on-chain canonical did not include the modulus, so
/// Eve's attestation verified successfully. Asset transfers to Eve.
///
/// Post-fix: the on-chain canonical includes
/// `recipient: base64url(sha256(escrow.recipient_pubkey_active()))`,
/// while the attestor's canonical hashes M_eve. The two byte strings
/// diverge, the Ed25519 introspection fails. Test asserts this.
#[tokio::test]
async fn test_claim_ant_arweave_attested_rejects_wrong_modulus_in_canonical() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    // Alice's modulus stored on-chain.
    let alice_modulus = [0xAAu8; 512];
    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        alice_modulus.to_vec(),
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;

    // Eve's modulus — different bytes. She constructs an "attestation"
    // for *her* canonical (built with M_eve) and signs it with the
    // valid attestor key.
    let eve_modulus = [0xEEu8; 512];
    let attacker_canonical = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(), // attacker controls claimant
        &escrow_state.nonce,
        &eve_modulus, // ← attacker's modulus, NOT what's on-chain
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&attacker_canonical).to_bytes();

    // Submit the (Ed25519(attacker_canonical), claim_ant_arweave_attested)
    // tx. The on-chain code reconstructs canonical from
    // `escrow.recipient_pubkey_active()` (= alice_modulus), which
    // hashes to a different `recipient` value than attacker_canonical
    // contains. AttestationMessageMismatch.
    let tx = build_claim_arweave_attested_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &depositor,
        escrow_state.nonce,
        &attacker_canonical,
        sig_bytes,
    )
    .await;
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "F-1: attested claim with attacker-supplied modulus in canonical must fail"
    );
}

/// F-5 regression: the Ed25519Program ix must be at position
/// `claim_ix_index - 1` exactly. An ed25519 ix elsewhere in the tx is
/// not enough — even if it cryptographically verified the right
/// (pubkey, message) tuple. This test crafts
/// `[ed25519, no-op, claim]` and asserts the claim is rejected.
#[tokio::test]
async fn test_claim_ant_arweave_attested_rejects_sigverify_not_immediately_preceding() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    let modulus_bytes = [0xAAu8; 512];
    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        modulus_bytes.to_vec(),
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    let canonical = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &modulus_bytes,
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    let attestor_pubkey_bytes: [u8; 32] = ario_ant_escrow::state::ATTESTOR_PUBKEY.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(&attestor_pubkey_bytes, &sig_bytes, &canonical);

    // ComputeBudget ix in the middle as a "no-op spacer" between
    // the Ed25519 ix and the claim ix. The claim's introspection
    // looks at index claim_idx-1 = ComputeBudget, NOT Ed25519. Reject.
    let spacer = solana_sdk::compute_budget::ComputeBudgetInstruction::request_heap_frame(0x10000);

    let (escrow, _bump) = escrow_pda(&asset_kp.pubkey());
    let accounts = ario_ant_escrow::accounts::ClaimAntArweaveAttested {
        escrow,
        ant_asset: asset_kp.pubkey(),
        claimant: claimant.pubkey(),
        depositor: depositor.pubkey(),
        payer: depositor.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts,
        data: ario_ant_escrow::instruction::ClaimAntArweaveAttested {
            message_nonce: escrow_state.nonce,
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ed25519_ix, spacer, claim_ix],
        Some(&depositor.pubkey()),
        &[&depositor],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "claim must reject when Ed25519Program ix is not at claim_ix - 1"
    );
}

#[tokio::test]
async fn measure_cu_claim_arweave_attested() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("setup deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    let canonical = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &[0xAAu8; 512],
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    let tx = build_claim_arweave_attested_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &depositor,
        escrow_state.nonce,
        &canonical,
        sig_bytes,
    )
    .await;
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] claim_ant_arweave_attested: {}", cu);
    assert!(
        cu < 100_000,
        "claim_ant_arweave_attested CU ({}) exceeds 100K target — Ed25519 introspection should be cheap",
        cu
    );
}

// =========================================
// Token Escrow — update_token_recipient non-depositor
// =========================================

#[tokio::test]
async fn test_update_token_recipient_non_depositor_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xAA),
    )
    .await
    .expect("deposit");

    let attacker = Keypair::new();
    airdrop(&mut ctx, &attacker.pubkey(), 5_000_000_000).await;
    let result = update_token_recipient_tx(
        &mut ctx,
        &setup,
        Some(&attacker),
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0xBB),
    )
    .await;
    assert_anchor_error!(result, EscrowError::NotDepositor);
}

// =========================================
// Vault Escrow — claim nonce mismatch
// =========================================

#[tokio::test]
async fn test_claim_vault_nonce_mismatch_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let sk_bytes = [42u8; 32];
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400,
        false,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("deposit_vault");

    // Warp past expiry so we hit the expired (liquid) path — no sibling ix needed.
    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;
    warp_clock_to(&mut ctx, escrow_state.vault_end_timestamp + 1).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    // Build a payer ATA (required by the accounts struct even on expired path).
    let payer_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &payer_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    // Use a wrong nonce.
    let bad_nonce = [0xFFu8; 32];
    let msg = build_escrow_canonical(
        "vault",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &bad_nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let result = claim_vault_ethereum_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        payer_ata.pubkey(),
        bad_nonce,
        signature,
        vec![],
    )
    .await;
    assert_anchor_error!(result, EscrowError::NonceMismatch);
}

// =========================================
// Vault Escrow — claim protocol mismatch
// =========================================

#[tokio::test]
async fn test_claim_vault_protocol_mismatch_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    // Deposit for ARWEAVE protocol. Real modulus content is irrelevant
    // — we only exercise the protocol-mismatch guard on the Ethereum
    // claim path below.
    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400,
        false,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("deposit_vault with arweave");

    // Warp past expiry.
    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;
    warp_clock_to(&mut ctx, escrow_state.vault_end_timestamp + 1).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let payer_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &payer_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    // Try to claim via Ethereum path — should fail with ProtocolMismatch.
    let fake_sig = [0u8; 65];
    let result = claim_vault_ethereum_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        payer_ata.pubkey(),
        escrow_state.nonce,
        fake_sig,
        vec![],
    )
    .await;
    assert_anchor_error!(result, EscrowError::ProtocolMismatch);
}

// =========================================
// claim_tokens_arweave_attested — Ed25519 attestation path
// =========================================

/// Build a transaction with Ed25519Program sigverify ix + claim_tokens_arweave_attested ix.
async fn claim_tokens_arweave_attested_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    claimant: &Keypair,
    claimant_ata_pubkey: Pubkey,
    payer: &Keypair,
    nonce: [u8; 32],
    canonical_message: &[u8],
    ed25519_signature: [u8; 64],
) -> std::result::Result<(), BanksClientError> {
    let attestor_pubkey_bytes: [u8; 32] = ario_ant_escrow::state::ATTESTOR_PUBKEY.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(
        &attestor_pubkey_bytes,
        &ed25519_signature,
        canonical_message,
    );

    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::ClaimTokensArweaveAttested {
        escrow,
        escrow_token_account: escrow_ata_pubkey,
        claimant_token_account: claimant_ata_pubkey,
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: payer.pubkey(),
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimTokensArweaveAttested {
        message_nonce: nonce,
    }
    .data();
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts,
        data,
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        // ORDER MATTERS: Ed25519Program ix must be at idx-1 of claim ix.
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            ed25519_ix,
            claim_ix,
        ],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn test_claim_tokens_arweave_attested_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 2_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512], // any 512-byte recipient pubkey; attested path doesn't consult it
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let canonical = build_escrow_canonical(
        "token",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &[0xAAu8; 512],
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    claim_tokens_arweave_attested_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        escrow_state.nonce,
        &canonical,
        sig_bytes,
    )
    .await
    .expect("claim_tokens_arweave_attested should succeed");

    let claimant_bal = get_token_balance(&mut ctx, &claimant_ata.pubkey()).await;
    assert_eq!(claimant_bal, amount, "claimant should have received tokens");

    let escrow_acct = ctx.banks_client.get_account(escrow_addr).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after attested claim"
    );
}

#[tokio::test]
async fn test_claim_tokens_arweave_attested_rejects_wrong_attestor() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 2_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;
    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("deposit");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;
    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let canonical = build_escrow_canonical(
        "token",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &[0xAAu8; 512],
    );

    // Sign with a different seed
    let bogus_seed = [9u8; 32];
    let secret = ed25519_dalek::SecretKey::from_bytes(&bogus_seed).unwrap();
    let public: ed25519_dalek::PublicKey = (&secret).into();
    let kp = ed25519_dalek::Keypair { secret, public };
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    let wrong_pubkey_bytes: [u8; 32] = kp.public.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(&wrong_pubkey_bytes, &sig_bytes, &canonical);

    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let claim_accounts = ario_ant_escrow::accounts::ClaimTokensArweaveAttested {
        escrow,
        escrow_token_account: escrow_ata.pubkey(),
        claimant_token_account: claimant_ata.pubkey(),
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: claimant.pubkey(),
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts: claim_accounts,
        data: ario_ant_escrow::instruction::ClaimTokensArweaveAttested {
            message_nonce: escrow_state.nonce,
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            ed25519_ix,
            claim_ix,
        ],
        Some(&claimant.pubkey()),
        &[&claimant],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "claim with wrong attestor pubkey must fail"
    );
}

#[tokio::test]
async fn measure_cu_claim_tokens_arweave_attested() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 2_000_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;
    deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("setup deposit");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;
    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let canonical = build_escrow_canonical(
        "token",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &[0xAAu8; 512],
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    let attestor_pubkey_bytes: [u8; 32] = ario_ant_escrow::state::ATTESTOR_PUBKEY.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(&attestor_pubkey_bytes, &sig_bytes, &canonical);
    let (escrow, _bump) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let claim_accounts = ario_ant_escrow::accounts::ClaimTokensArweaveAttested {
        escrow,
        escrow_token_account: escrow_ata.pubkey(),
        claimant_token_account: claimant_ata.pubkey(),
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: claimant.pubkey(),
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts: claim_accounts,
        data: ario_ant_escrow::instruction::ClaimTokensArweaveAttested {
            message_nonce: escrow_state.nonce,
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            ed25519_ix,
            claim_ix,
        ],
        Some(&claimant.pubkey()),
        &[&claimant],
        blockhash,
    );
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] claim_tokens_arweave_attested: {}", cu);
    assert!(
        cu < 100_000,
        "claim_tokens_arweave_attested CU ({}) exceeds 100K target",
        cu
    );
}

// =========================================
// claim_vault_arweave_attested — Ed25519 attestation path (expired)
// =========================================
//
// The expired path is structurally simpler (direct SPL transfer to
// claimant ATA, no sibling vaulted_transfer required). It exercises
// the Ed25519 introspection in isolation. The active-vault path is
// covered by claim_vault_arweave's existing tests; the only logic
// that differs in _attested is the verification step which is tested
// here and in test_claim_ant_arweave_attested_*.

async fn claim_vault_arweave_attested_tx(
    ctx: &mut ProgramTestContext,
    setup: &TokenSetup,
    escrow_ata_pubkey: Pubkey,
    claimant: &Keypair,
    claimant_ata_pubkey: Pubkey,
    payer: &Keypair,
    payer_ata_pubkey: Pubkey,
    nonce: [u8; 32],
    canonical_message: &[u8],
    ed25519_signature: [u8; 64],
    extra_ixs: Vec<Ix>,
) -> std::result::Result<(), BanksClientError> {
    let attestor_pubkey_bytes: [u8; 32] = ario_ant_escrow::state::ATTESTOR_PUBKEY.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(
        &attestor_pubkey_bytes,
        &ed25519_signature,
        canonical_message,
    );

    let (escrow, _bump) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::ClaimVaultArweaveAttested {
        escrow,
        escrow_token_account: escrow_ata_pubkey,
        claimant_token_account: claimant_ata_pubkey,
        payer_token_account: payer_ata_pubkey,
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: payer.pubkey(),
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimVaultArweaveAttested {
        message_nonce: nonce,
    }
    .data();

    let mut ixs = vec![
        solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
        ed25519_ix,
    ];
    ixs.push(Ix {
        program_id: ario_ant_escrow::ID,
        accounts,
        data,
    });
    ixs.extend(extra_ixs);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[payer], blockhash);
    ctx.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn test_claim_vault_arweave_attested_expired_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let lock_duration = 30 * 86_400i64;
    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
        false,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("deposit_vault should succeed");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;
    warp_clock_to(&mut ctx, escrow_state.vault_end_timestamp + 1).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    let payer_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;
    create_token_account(
        &mut ctx,
        &payer_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let canonical = build_escrow_canonical(
        "vault",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &[0xAAu8; 512],
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    claim_vault_arweave_attested_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        &claimant,
        claimant_ata.pubkey(),
        &claimant,
        payer_ata.pubkey(),
        escrow_state.nonce,
        &canonical,
        sig_bytes,
        vec![], // expired path: no sibling vaulted_transfer required
    )
    .await
    .expect("expired-vault attested claim should succeed");

    let claimant_bal = get_token_balance(&mut ctx, &claimant_ata.pubkey()).await;
    assert_eq!(
        claimant_bal, amount,
        "expired-vault attested claim must deliver tokens to claimant"
    );

    let escrow_acct = ctx.banks_client.get_account(escrow_addr).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after expired-vault attested claim"
    );
}

#[tokio::test]
async fn measure_cu_claim_vault_arweave_attested_expired() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64;
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;
    deposit_vault_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        30 * 86_400i64,
        false,
        PROTOCOL_ARWEAVE,
        vec![0xAAu8; 512],
    )
    .await
    .expect("setup deposit");

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_addr).await;
    warp_clock_to(&mut ctx, escrow_state.vault_end_timestamp + 1).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    let payer_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 2_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;
    create_token_account(
        &mut ctx,
        &payer_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let canonical = build_escrow_canonical(
        "vault",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &[0xAAu8; 512],
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    let attestor_pubkey_bytes: [u8; 32] = ario_ant_escrow::state::ATTESTOR_PUBKEY.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(&attestor_pubkey_bytes, &sig_bytes, &canonical);
    let (escrow, _bump) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::ClaimVaultArweaveAttested {
        escrow,
        escrow_token_account: escrow_ata.pubkey(),
        claimant_token_account: claimant_ata.pubkey(),
        payer_token_account: payer_ata.pubkey(),
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: claimant.pubkey(),
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts,
        data: ario_ant_escrow::instruction::ClaimVaultArweaveAttested {
            message_nonce: escrow_state.nonce,
        }
        .data(),
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            ed25519_ix,
            claim_ix,
        ],
        Some(&claimant.pubkey()),
        &[&claimant],
        blockhash,
    );
    let cu = simulate_cu(&mut ctx, tx).await;
    println!("[cu] claim_vault_arweave_attested (expired): {}", cu);
    assert!(
        cu < 100_000,
        "claim_vault_arweave_attested CU ({}) exceeds 100K target",
        cu
    );
}
// ============================================================================
// PR-4: Event emission tests
//
// These tests submit each kind of escrow instruction via
// `process_transaction_with_metadata` (so `Program data:` log lines are
// captured), then assert the unified event payload via
// `ario-test-utils::expect_event!`.
//
// All tests start with `ario_test_utils::bpf_required!()` because
// solana-program-test 2.1.0 only routes `sol_log_data` to log_messages
// under BPF dispatch.
//
// The 15 emit sites collapse onto 4 event shapes via `asset_type` /
// `claim_protocol` discriminator fields, so we cover at least one path
// per (event-shape, asset_type) combination plus both claim_protocol
// values for the claim shape — enough to catch any regression in the
// unified-event wiring without re-running the full 15-call matrix.
// Wire-level coverage of every single instruction lives in the
// existing localnet/SDK e2e suites.
// ============================================================================

/// Build a `deposit_ant` transaction without submitting — lets the test
/// capture log_messages via `process_transaction_with_metadata`.
async fn build_deposit_ant_tx(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    depositor: &Keypair,
    protocol: u8,
    pubkey: Vec<u8>,
) -> Transaction {
    let (escrow, _bump) = escrow_pda(&asset);
    let accounts = ario_ant_escrow::accounts::DepositAnt {
        escrow,
        ant_asset: asset,
        depositor: depositor.pubkey(),
        mpl_core_program: MPL_CORE_PROGRAM_ID,
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::DepositAnt {
        recipient_protocol: protocol,
        recipient_pubkey: pubkey,
    }
    .data();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&depositor.pubkey()),
        &[depositor],
        blockhash,
    )
}

#[tokio::test]
async fn test_deposit_ant_emits_deposited_event_arweave() {
    ario_test_utils::bpf_required!();
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    let pubkey = arweave_pubkey_fixture(0xAB);
    let tx = build_deposit_ant_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        pubkey.clone(),
    )
    .await;

    let meta = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(meta.result.is_ok(), "deposit_ant should succeed");
    let logs = meta.metadata.expect("metadata").log_messages;

    let ev = ario_test_utils::expect_event!(&logs, EscrowDepositedEvent);
    let (expected_escrow, _) = escrow_pda(&asset_kp.pubkey());
    assert_eq!(ev.escrow, expected_escrow);
    assert_eq!(ev.depositor, depositor.pubkey());
    assert_eq!(ev.asset_id, asset_kp.pubkey());
    assert_eq!(ev.asset_type, ASSET_TYPE_ANT);
    assert_eq!(ev.amount, 0, "ANT deposits carry zero amount");
    assert_eq!(ev.recipient_protocol, PROTOCOL_ARWEAVE);
    // Arweave: hashed-modulus form, 32 bytes; first 32 bytes match
    // sha256(modulus); trailing 32 bytes zero-padded.
    assert_eq!(ev.recipient_pubkey_len, 32);
    let expected_hash = anchor_lang::solana_program::hash::hash(&pubkey).to_bytes();
    assert_eq!(&ev.recipient_pubkey[..32], &expected_hash[..]);
    assert!(ev.recipient_pubkey[32..].iter().all(|&b| b == 0));
    assert!(ev.timestamp > 0);
}

#[tokio::test]
async fn test_deposit_ant_emits_deposited_event_ethereum_recipient_pubkey_len() {
    ario_test_utils::bpf_required!();
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    let addr = ethereum_address_fixture(0xCD);
    let tx = build_deposit_ant_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        addr.clone(),
    )
    .await;

    let meta = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(meta.result.is_ok(), "deposit_ant ethereum should succeed");
    let logs = meta.metadata.expect("metadata").log_messages;

    let ev = ario_test_utils::expect_event!(&logs, EscrowDepositedEvent);
    assert_eq!(ev.asset_type, ASSET_TYPE_ANT);
    assert_eq!(ev.recipient_protocol, PROTOCOL_ETHEREUM);
    // Ethereum address fits directly: 20 bytes, rest zero.
    assert_eq!(ev.recipient_pubkey_len, 20);
    assert_eq!(&ev.recipient_pubkey[..20], &addr[..]);
    assert!(ev.recipient_pubkey[20..].iter().all(|&b| b == 0));
}

#[tokio::test]
async fn test_cancel_deposit_emits_cancelled_event() {
    ario_test_utils::bpf_required!();
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("setup deposit");

    let tx = build_cancel_tx(&mut ctx, asset_kp.pubkey(), &depositor).await;
    let meta = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(meta.result.is_ok(), "cancel_deposit should succeed");
    let logs = meta.metadata.expect("metadata").log_messages;

    let ev = ario_test_utils::expect_event!(&logs, EscrowCancelledEvent);
    let (expected_escrow, _) = escrow_pda(&asset_kp.pubkey());
    assert_eq!(ev.escrow, expected_escrow);
    assert_eq!(ev.depositor, depositor.pubkey());
    assert_eq!(ev.asset_id, asset_kp.pubkey());
    assert_eq!(ev.asset_type, ASSET_TYPE_ANT);
    assert!(ev.timestamp > 0);
}

#[tokio::test]
async fn test_update_recipient_emits_recipient_updated_event() {
    ario_test_utils::bpf_required!();
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        arweave_pubkey_fixture(0),
    )
    .await
    .expect("setup deposit");

    let tx = build_update_recipient_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        ethereum_address_fixture(0x55),
    )
    .await;
    let meta = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(meta.result.is_ok(), "update_recipient should succeed");
    let logs = meta.metadata.expect("metadata").log_messages;

    let ev = ario_test_utils::expect_event!(&logs, EscrowRecipientUpdatedEvent);
    let (expected_escrow, _) = escrow_pda(&asset_kp.pubkey());
    assert_eq!(ev.escrow, expected_escrow);
    assert_eq!(ev.depositor, depositor.pubkey());
    assert_eq!(ev.asset_type, ASSET_TYPE_ANT);
    assert!(ev.timestamp > 0);
}

#[tokio::test]
async fn test_claim_ant_arweave_attested_emits_claimed_event() {
    ario_test_utils::bpf_required!();
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    let modulus = [0xAAu8; 512];
    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ARWEAVE,
        modulus.to_vec(),
    )
    .await
    .expect("setup deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    let canonical = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &modulus,
    );
    let kp = test_attestor_keypair();
    use ed25519_dalek::Signer;
    let sig_bytes: [u8; 64] = kp.sign(&canonical).to_bytes();

    let tx = build_claim_arweave_attested_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &depositor,
        escrow_state.nonce,
        &canonical,
        sig_bytes,
    )
    .await;

    let meta = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(
        meta.result.is_ok(),
        "claim_ant_arweave_attested should succeed"
    );
    let logs = meta.metadata.expect("metadata").log_messages;

    let ev = ario_test_utils::expect_event!(&logs, EscrowClaimedEvent);
    let (expected_escrow, _) = escrow_pda(&asset_kp.pubkey());
    assert_eq!(ev.escrow, expected_escrow);
    assert_eq!(ev.claimer, claimant.pubkey());
    assert_eq!(ev.asset_id, asset_kp.pubkey());
    assert_eq!(ev.asset_type, ASSET_TYPE_ANT);
    assert_eq!(ev.amount, 0, "ANT claims carry zero amount");
    assert_eq!(ev.claim_protocol, PROTOCOL_ARWEAVE);
    assert!(ev.timestamp > 0);
}

#[tokio::test]
async fn test_claim_ant_ethereum_emits_claimed_event() {
    ario_test_utils::bpf_required!();
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let depositor = Keypair::new();
    let claimant = Keypair::new();
    airdrop(&mut ctx, &depositor.pubkey(), 5_000_000_000).await;
    let asset_kp = Keypair::new();
    mint_test_ant(&mut ctx, &asset_kp, &depositor).await;

    let sk_bytes: [u8; 32] = {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((i as u8).wrapping_mul(13).wrapping_add(7)) | 1;
        }
        b
    };
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &depositor,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await
    .expect("setup deposit");

    let escrow_state = fetch_escrow(&mut ctx, asset_kp.pubkey()).await;
    let msg = build_test_canonical(
        &asset_kp.pubkey(),
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let tx = build_claim_ethereum_tx(
        &mut ctx,
        asset_kp.pubkey(),
        &claimant,
        depositor.pubkey(),
        &depositor,
        escrow_state.nonce,
        signature,
    )
    .await;

    let meta = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(meta.result.is_ok(), "claim_ant_ethereum should succeed");
    let logs = meta.metadata.expect("metadata").log_messages;

    let ev = ario_test_utils::expect_event!(&logs, EscrowClaimedEvent);
    assert_eq!(ev.claimer, claimant.pubkey());
    assert_eq!(ev.asset_id, asset_kp.pubkey());
    assert_eq!(ev.asset_type, ASSET_TYPE_ANT);
    assert_eq!(ev.amount, 0);
    assert_eq!(
        ev.claim_protocol, PROTOCOL_ETHEREUM,
        "ethereum claim must record claim_protocol = 1"
    );
}

#[tokio::test]
async fn test_deposit_tokens_emits_deposited_event() {
    ario_test_utils::bpf_required!();
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 1_000_000_000u64; // 1000 ARIO
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_token_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let eth_addr = ethereum_address_fixture(0x77);
    let tx = build_deposit_tokens_tx(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        PROTOCOL_ETHEREUM,
        eth_addr.clone(),
    )
    .await;

    let meta = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(meta.result.is_ok(), "deposit_tokens should succeed");
    let logs = meta.metadata.expect("metadata").log_messages;

    let ev = ario_test_utils::expect_event!(&logs, EscrowDepositedEvent);
    assert_eq!(ev.escrow, escrow_addr);
    assert_eq!(ev.depositor, setup.depositor.pubkey());
    // Token escrows: asset_id = ARIO mint (not the client-supplied 32-byte id).
    assert_eq!(ev.asset_id, setup.mint_kp.pubkey());
    assert_eq!(ev.asset_type, ASSET_TYPE_TOKEN);
    assert_eq!(ev.amount, amount, "Token deposits record the mARIO amount");
    assert_eq!(ev.recipient_protocol, PROTOCOL_ETHEREUM);
    assert_eq!(ev.recipient_pubkey_len, 20);
    assert_eq!(&ev.recipient_pubkey[..20], &eth_addr[..]);
}

#[tokio::test]
async fn test_deposit_vault_emits_deposited_event() {
    ario_test_utils::bpf_required!();
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test().start_with_context().await;
    let amount = 500_000_000u64; // 500 ARIO (above 100 ARIO minimum)
    let setup = setup_token_escrow(&mut ctx, amount).await;
    let (escrow_addr, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_addr, &setup.mint_kp.pubkey()).await;

    let lock_duration = 30 * 86_400i64;
    let eth_addr = ethereum_address_fixture(0x91);

    let accounts = ario_ant_escrow::accounts::DepositVault {
        escrow: escrow_addr,
        depositor_token_account: setup.depositor_ata.pubkey(),
        escrow_token_account: escrow_ata.pubkey(),
        ario_mint: setup.mint_kp.pubkey(),
        depositor: setup.depositor.pubkey(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::DepositVault {
        asset_id: setup.asset_id,
        amount,
        lock_duration_seconds: lock_duration,
        revocable: true,
        recipient_protocol: PROTOCOL_ETHEREUM,
        recipient_pubkey: eth_addr.clone(),
    }
    .data();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_ant_escrow::ID,
            accounts,
            data,
        }],
        Some(&setup.depositor.pubkey()),
        &[&setup.depositor],
        blockhash,
    );

    let meta = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(meta.result.is_ok(), "deposit_vault should succeed");
    let logs = meta.metadata.expect("metadata").log_messages;

    let ev = ario_test_utils::expect_event!(&logs, EscrowDepositedEvent);
    assert_eq!(ev.escrow, escrow_addr);
    assert_eq!(ev.depositor, setup.depositor.pubkey());
    // Vault escrows: asset_id = the 32-byte client id, decoded as a Pubkey.
    assert_eq!(ev.asset_id, Pubkey::new_from_array(setup.asset_id));
    assert_eq!(ev.asset_type, ASSET_TYPE_VAULT);
    assert_eq!(ev.amount, amount);
    assert_eq!(ev.recipient_protocol, PROTOCOL_ETHEREUM);
    assert_eq!(ev.recipient_pubkey_len, 20);
}
