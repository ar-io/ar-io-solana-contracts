//! Cross-program integration tests for the **active-vault claim** path.
//!
//! Why this lives in its own file (not `integration.rs`):
//!
//! - The active-claim path requires a working `ario_core` deployed in the
//!   test runtime so the sibling `vaulted_transfer` ix can actually execute.
//!   Solana txs are atomic — if `vaulted_transfer` fails because ario-core
//!   isn't loaded, the whole tx (including `claim_vault_*`) rolls back, so
//!   the introspection guard never gets to confirm the happy path.
//! - Adding ario-core to the existing `program_test()` would force every
//!   one of the 41 escrow-isolation tests to also pay the core-init cost.
//!   Splitting keeps both runtimes minimal.
//!
//! What's covered:
//!
//! - `test_claim_vault_arweave_active_happy_path` — deposit a 30-day vault
//!   escrow, build `[vaulted_transfer, claim_vault_arweave]` in one tx,
//!   submit, assert: escrow PDA closed, claimant's `Vault` + `VaultCounter`
//!   PDAs created with the correct `unlock_at` and `revocable`.
//! - `test_claim_vault_ethereum_active_happy_path` — same shape, ECDSA.
//!
//! These complement the `integration.rs` tests that already cover the
//! expired happy path and the active-without-sibling negative path.
//!
//! RUNNING:
//!   1. `anchor build` so `ario_ant_escrow.so`, `ario_core.so`, and
//!      `mpl_core.so` are all in `target/deploy/`.
//!   2. `cp programs/ario-ant-escrow/tests/fixtures/mpl_core.so target/deploy/`
//!   3. `BPF_OUT_DIR=$(pwd)/contracts/target/deploy cargo test \
//!         -p ario-ant-escrow --test cross_program_vault_claim`

use anchor_lang::{prelude::*, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction as Ix,
    program_pack::Pack,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use ario_ant_escrow::error::EscrowError;
use ario_ant_escrow::state::{EscrowToken, ESCROW_VAULT_SEED, PROTOCOL_ARWEAVE, PROTOCOL_ETHEREUM};
use ario_core::state::{Vault, VaultCounter, CONFIG_SEED, VAULT_COUNTER_SEED, VAULT_SEED};

// =========================================
// Test scaffolding
// =========================================

fn anchor_processor_escrow(
    program_id: &Pubkey,
    accounts: &[anchor_lang::prelude::AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[anchor_lang::prelude::AccountInfo] = std::mem::transmute(accounts);
        ario_ant_escrow::entry(program_id, accounts, data)
    }
}

/// Deterministic upgrade-authority keypair for ario-core's `Initialize`,
/// matching the convention in `programs/ario-core/tests/integration.rs`.
fn upgrade_authority_keypair() -> Keypair {
    solana_sdk::signer::keypair::keypair_from_seed(&[42u8; 32])
        .expect("keypair_from_seed must succeed")
}

fn ario_core_program_data_pda() -> Pubkey {
    Pubkey::find_program_address(
        &[ario_core::ID.as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    )
    .0
}

/// Build a fake `ProgramData` account body owned by `bpf_loader_upgradeable`,
/// naming `upgrade_authority_keypair()` as the upgrade authority. ario-core's
/// `Initialize` checks `program_data.upgrade_authority_address ==
/// Some(payer.key())` so the payer of `Initialize` must be that keypair.
fn build_program_data(upgrade_authority: &Pubkey) -> Vec<u8> {
    let mut data = Vec::with_capacity(45);
    data.extend_from_slice(&3u32.to_le_bytes()); // AccountType::ProgramData
    data.extend_from_slice(&0i64.to_le_bytes()); // slot
    data.push(1); // option_tag = Some
    data.extend_from_slice(upgrade_authority.as_ref());
    data
}

/// `program_test()` with both `ario_ant_escrow` (entry processor) and
/// `ario_core` (loaded as a real BPF .so so its `vaulted_transfer` handler
/// can actually create vaults). Pre-adds the BPFLoaderUpgradeable
/// `ProgramData` PDA + funds the upgrade authority so `Initialize` works.
fn program_test_with_core() -> ProgramTest {
    let mut pt = ProgramTest::new(
        "ario_ant_escrow",
        ario_ant_escrow::ID,
        processor!(anchor_processor_escrow),
    );
    pt.set_compute_max_units(800_000);

    // Real ario-core BPF — needed for the sibling vaulted_transfer ix.
    pt.add_program("ario_core", ario_core::ID, None);

    // BPFLoaderUpgradeable plumbing for ario_core's Initialize.
    let pd_authority = upgrade_authority_keypair().pubkey();
    let pd_data = build_program_data(&pd_authority);
    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        ario_core_program_data_pda(),
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(pd_data.len()),
            data: pd_data,
            owner: solana_sdk::bpf_loader_upgradeable::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        pd_authority,
        solana_sdk::account::Account {
            lamports: 100_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );

    pt
}

// =========================================
// SPL token + ario-core init helpers
// =========================================

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

async fn create_mint(ctx: &mut ProgramTestContext, mint: &Keypair, authority: &Pubkey) {
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let mint_rent = rent.minimum_balance(spl_token::state::Mint::LEN);
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::system_instruction::create_account(
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
                6,
            )
            .unwrap(),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, mint],
        ctx.last_blockhash,
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
    let acct_rent = rent.minimum_balance(spl_token::state::Account::LEN);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::system_instruction::create_account(
                &ctx.payer.pubkey(),
                &account.pubkey(),
                acct_rent,
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

async fn mint_tokens(
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

async fn get_token_balance(ctx: &mut ProgramTestContext, account: &Pubkey) -> u64 {
    let raw = ctx
        .banks_client
        .get_account(*account)
        .await
        .unwrap()
        .unwrap();
    spl_token::state::Account::unpack(&raw.data).unwrap().amount
}

/// Initialize ario-core's config bound to `mint`. `arns_program`,
/// `treasury`, `migration_authority` are placeholders — none of them are
/// touched by `vaulted_transfer`, which only reads `config.mint`.
async fn initialize_ario_core(ctx: &mut ProgramTestContext, mint: &Pubkey) -> Pubkey {
    let (config_key, _) = Pubkey::find_program_address(&[CONFIG_SEED], &ario_core::ID);
    let upgrade_auth = upgrade_authority_keypair();

    let accounts = ario_core::accounts::Initialize {
        config: config_key,
        mint: *mint,
        payer: upgrade_auth.pubkey(),
        program_data: ario_core_program_data_pda(),
        system_program: solana_sdk::system_program::ID,
    };
    let data = ario_core::instruction::Initialize {
        params: ario_core::InitializeParams {
            authority: ctx.payer.pubkey(),
            total_supply: 1_000_000_000_000,
            arns_program: Pubkey::new_unique(),
            treasury: Pubkey::new_unique(),
            migration_authority: ctx.payer.pubkey(),
        },
    };
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: ario_core::ID,
            accounts: accounts.to_account_metas(None),
            data: data.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_auth],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    config_key
}

// =========================================
// Escrow setup helpers (mirror integration.rs but mint is the ario-core mint)
// =========================================

#[allow(dead_code)]
struct CrossSetup {
    mint_kp: Keypair,
    mint_authority: Keypair,
    depositor: Keypair,
    depositor_ata: Keypair,
    asset_id: [u8; 32],
    config_pda: Pubkey,
}

async fn setup_with_initialized_core(
    ctx: &mut ProgramTestContext,
    deposit_amount: u64,
) -> CrossSetup {
    let mint_authority = Keypair::new();
    let mint_kp = Keypair::new();
    let depositor = Keypair::new();
    let depositor_ata = Keypair::new();

    airdrop(ctx, &depositor.pubkey(), 10_000_000_000).await;
    airdrop(ctx, &mint_authority.pubkey(), 1_000_000_000).await;

    create_mint(ctx, &mint_kp, &mint_authority.pubkey()).await;
    let config_pda = initialize_ario_core(ctx, &mint_kp.pubkey()).await;

    create_token_account(ctx, &depositor_ata, &mint_kp.pubkey(), &depositor.pubkey()).await;
    mint_tokens(
        ctx,
        &mint_kp.pubkey(),
        &depositor_ata.pubkey(),
        &mint_authority,
        deposit_amount,
    )
    .await;

    let asset_id = [0xA1u8; 32];

    CrossSetup {
        mint_kp,
        mint_authority,
        depositor,
        depositor_ata,
        asset_id,
        config_pda,
    }
}

fn escrow_vault_pda(depositor: &Pubkey, asset_id: &[u8; 32]) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ESCROW_VAULT_SEED, depositor.as_ref(), asset_id],
        &ario_ant_escrow::ID,
    )
}

async fn create_escrow_token_account(
    ctx: &mut ProgramTestContext,
    escrow_pda: &Pubkey,
    mint: &Pubkey,
) -> Keypair {
    let escrow_ata = Keypair::new();
    create_token_account(ctx, &escrow_ata, mint, escrow_pda).await;
    escrow_ata
}

async fn deposit_vault(
    ctx: &mut ProgramTestContext,
    setup: &CrossSetup,
    escrow_ata: Pubkey,
    amount: u64,
    lock_duration_seconds: i64,
    revocable: bool,
    protocol: u8,
    pubkey: Vec<u8>,
) {
    let (escrow, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let accounts = ario_ant_escrow::accounts::DepositVault {
        escrow,
        depositor_token_account: setup.depositor_ata.pubkey(),
        escrow_token_account: escrow_ata,
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
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("deposit_vault");
}

async fn fetch_escrow_token(ctx: &mut ProgramTestContext, escrow: Pubkey) -> EscrowToken {
    let raw = ctx.banks_client.get_account(escrow).await.unwrap().unwrap();
    EscrowToken::try_deserialize(&mut raw.data.as_slice()).expect("decode EscrowToken")
}

// =========================================
// Canonical message + signature helpers (vendored from integration.rs)
// =========================================

fn build_escrow_canonical(
    asset_type: &str,
    asset_id: &[u8; 32],
    amount: u64,
    claimant: &Pubkey,
    nonce: &[u8; 32],
    recipient_pubkey_active: &[u8],
) -> Vec<u8> {
    let asset_hex: String = asset_id.iter().map(|b| format!("{:02x}", b)).collect();
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
        network, recipient_id, asset_type, asset_hex, amount, claimant.to_string(), nonce_hex
    )
    .into_bytes()
}

/// Deterministic test attestor seed used by all `claim_*_attested` tests.
/// MUST match `programs/ario-ant-escrow/src/state.rs::ATTESTOR_PUBKEY`.
const TEST_ATTESTOR_SEED: [u8; 32] = [1u8; 32];

fn test_attestor_keypair() -> ed25519_dalek::Keypair {
    let secret = ed25519_dalek::SecretKey::from_bytes(&TEST_ATTESTOR_SEED).unwrap();
    let public: ed25519_dalek::PublicKey = (&secret).into();
    ed25519_dalek::Keypair { secret, public }
}

/// Build the Solana `Ed25519Program` native sigverify ix with all data
/// inline. Layout reference: agave/programs/ed25519-program/src/lib.rs.
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

fn sign_ethereum(
    canonical_message: &[u8],
    secret_key: &libsecp256k1::SecretKey,
) -> ([u8; 65], [u8; 20]) {
    use anchor_lang::solana_program::keccak::hash as keccak256;
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

    let pubkey = libsecp256k1::PublicKey::from_secret_key(secret_key);
    let pk_bytes = pubkey.serialize();
    let pk_hash = keccak256(&pk_bytes[1..]).to_bytes();
    let mut addr = [0u8; 20];
    addr.copy_from_slice(&pk_hash[12..]);
    (out, addr)
}

fn skip_if_no_bpf_artifacts() -> bool {
    let bpf_dir = std::env::var("BPF_OUT_DIR").ok();
    if bpf_dir.is_none() {
        eprintln!("skipping: BPF_OUT_DIR not set");
        return true;
    }
    let dir = std::path::PathBuf::from(bpf_dir.unwrap());
    for needed in ["ario_ant_escrow.so", "ario_core.so"] {
        if !dir.join(needed).exists() {
            eprintln!("skipping: missing {} in BPF_OUT_DIR", needed);
            return true;
        }
    }
    false
}

// =========================================
// Sibling vaulted_transfer ix builder (mirror what the SDK auto-bundles)
// =========================================

#[allow(clippy::too_many_arguments)]
fn build_vaulted_transfer_ix(
    config_pda: Pubkey,
    recipient: Pubkey,
    recipient_vault_counter: Pubkey,
    new_vault: Pubkey,
    sender_token_account: Pubkey,
    vault_token_account: Pubkey,
    sender: Pubkey,
    amount: u64,
    lock_duration_seconds: i64,
    revocable: bool,
) -> Ix {
    let accounts = ario_core::accounts::VaultedTransfer {
        config: config_pda,
        recipient_vault_counter,
        vault: new_vault,
        sender_token_account,
        vault_token_account,
        recipient,
        sender,
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let data = ario_core::instruction::VaultedTransfer {
        amount,
        lock_duration_seconds,
        revocable,
    }
    .data();
    Ix {
        program_id: ario_core::ID,
        accounts,
        data,
    }
}

// =========================================
// Tests
// =========================================

#[tokio::test]
async fn test_claim_vault_arweave_attested_active_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let setup = setup_with_initialized_core(&mut ctx, amount).await;
    let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_pda, &setup.mint_kp.pubkey()).await;

    // Stable Arweave-shaped recipient bytes. Real claims go through the
    // attestor service which signs over the canonical (built from the
    // user's actual modulus); here we stand in with a fixed value so the
    // canonical reconstructed on-chain matches what we sign over below.
    let modulus = [0xAAu8; 512];
    deposit_vault(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
        true, // revocable — exercises the revocable=true branch of introspection
        PROTOCOL_ARWEAVE,
        modulus.to_vec(),
    )
    .await;

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_pda).await;
    // Vault is still active (no clock warp). Active-claim path requires the
    // sibling vaulted_transfer ix in the same tx.

    // Use a separate `payer` keypair from `claimant` — ario-core's
    // `vaulted_transfer` rejects sender == recipient with `SelfTransfer`.
    // Real-world usage will typically have a third-party payer (cranker /
    // service) anyway; this test exercises that shape.
    let claimant = Keypair::new();
    let payer = Keypair::new();
    let claimant_ata = Keypair::new();
    let payer_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 1_000_000_000).await;
    airdrop(&mut ctx, &payer.pubkey(), 5_000_000_000).await;
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
        &payer.pubkey(),
    )
    .await;

    // Compute remaining lock and derive new vault PDA + ATA for the claimant.
    let clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    let remaining = escrow_state.vault_end_timestamp - clock.unix_timestamp;
    assert!(remaining > 0, "vault must still be active for this test");

    let (counter_pda, _) = Pubkey::find_program_address(
        &[VAULT_COUNTER_SEED, claimant.pubkey().as_ref()],
        &ario_core::ID,
    );
    // First-ever vault for this claimant, so vault_id = 0.
    let new_vault_id: u64 = 0;
    let (new_vault_pda, _) = Pubkey::find_program_address(
        &[
            VAULT_SEED,
            claimant.pubkey().as_ref(),
            &new_vault_id.to_le_bytes(),
        ],
        &ario_core::ID,
    );
    let new_vault_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &new_vault_ata,
        &setup.mint_kp.pubkey(),
        &new_vault_pda,
    )
    .await;

    let vaulted_ix = build_vaulted_transfer_ix(
        setup.config_pda,
        claimant.pubkey(),
        counter_pda,
        new_vault_pda,
        payer_ata.pubkey(),
        new_vault_ata.pubkey(),
        payer.pubkey(), // sender = payer (separate from claimant)
        amount,
        remaining,
        true, // matches escrow.vault_revocable
    );

    // Build the canonical message exactly as the on-chain attested ix
    // will reconstruct it (binds escrow.recipient_pubkey via the
    // `recipient` line — F-1).
    let canonical = build_escrow_canonical(
        "vault",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &modulus,
    );

    // Sign with the test attestor's Ed25519 key (matches ATTESTOR_PUBKEY
    // baked into the program with the unsafe-allow-test-attestor-pubkey
    // feature on for the test build).
    use ed25519_dalek::Signer;
    let kp = test_attestor_keypair();
    let attest_sig: [u8; 64] = kp.sign(&canonical).to_bytes();
    let attestor_pubkey_bytes: [u8; 32] = kp.public.to_bytes();

    let ed25519_ix = build_ed25519_sigverify_ix(&attestor_pubkey_bytes, &attest_sig, &canonical);

    // Build attested claim ix.
    let claim_accounts = ario_ant_escrow::accounts::ClaimVaultArweaveAttested {
        escrow: escrow_pda,
        escrow_token_account: escrow_ata.pubkey(),
        claimant_token_account: claimant_ata.pubkey(),
        payer_token_account: payer_ata.pubkey(),
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: payer.pubkey(),
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let claim_data = ario_ant_escrow::instruction::ClaimVaultArweaveAttested {
        message_nonce: escrow_state.nonce,
    }
    .data();
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts: claim_accounts,
        data: claim_data,
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            // Order matters:
            // - Ed25519 sigverify MUST be at claim_ix - 1 (introspection requirement).
            // - Claim must run before vaulted_transfer so payer_ata gets funded
            //   from the escrow before vaulted_transfer pulls from it.
            ed25519_ix,
            claim_ix,
            vaulted_ix,
        ],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.expect(
        "active-vault attested claim should succeed when sibling vaulted_transfer is included",
    );

    // Escrow PDA closed.
    let escrow_acct = ctx.banks_client.get_account(escrow_pda).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after claim"
    );

    // New Vault PDA created on the claimant with the right shape.
    let vault_raw = ctx
        .banks_client
        .get_account(new_vault_pda)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_raw.data.as_slice()).expect("decode Vault");
    assert_eq!(
        vault.owner,
        claimant.pubkey(),
        "new vault must be owned by claimant"
    );
    assert_eq!(
        vault.amount, amount,
        "new vault must hold the escrowed amount"
    );
    assert!(
        vault.revocable,
        "new vault must inherit escrow.vault_revocable=true"
    );

    // Counter advanced.
    let counter_raw = ctx
        .banks_client
        .get_account(counter_pda)
        .await
        .unwrap()
        .unwrap();
    let counter = VaultCounter::try_deserialize(&mut counter_raw.data.as_slice()).unwrap();
    assert_eq!(
        counter.next_id, 1,
        "counter must advance to 1 after first vault"
    );

    // Tokens settled in the new vault's ATA, not the claimant's primary ATA.
    let new_vault_bal = get_token_balance(&mut ctx, &new_vault_ata.pubkey()).await;
    assert_eq!(
        new_vault_bal, amount,
        "active claim must re-lock tokens into new vault"
    );
    let claimant_primary_bal = get_token_balance(&mut ctx, &claimant_ata.pubkey()).await;
    assert_eq!(
        claimant_primary_bal, 0,
        "active claim must NOT deliver to claimant's primary ATA"
    );
}

#[tokio::test]
async fn test_claim_vault_ethereum_active_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let setup = setup_with_initialized_core(&mut ctx, amount).await;
    let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &setup.asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_pda, &setup.mint_kp.pubkey()).await;

    let sk_bytes: [u8; 32] = {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((i as u8).wrapping_mul(17).wrapping_add(5)) | 1;
        }
        b
    };
    let secret_key = libsecp256k1::SecretKey::parse(&sk_bytes).unwrap();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault(
        &mut ctx,
        &setup,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
        false, // non-revocable — covers the revocable=false branch
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await;

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_pda).await;

    let claimant = Keypair::new();
    let payer = Keypair::new();
    let claimant_ata = Keypair::new();
    let payer_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 1_000_000_000).await;
    airdrop(&mut ctx, &payer.pubkey(), 5_000_000_000).await;
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
        &payer.pubkey(),
    )
    .await;

    let clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    let remaining = escrow_state.vault_end_timestamp - clock.unix_timestamp;

    let (counter_pda, _) = Pubkey::find_program_address(
        &[VAULT_COUNTER_SEED, claimant.pubkey().as_ref()],
        &ario_core::ID,
    );
    let new_vault_id: u64 = 0;
    let (new_vault_pda, _) = Pubkey::find_program_address(
        &[
            VAULT_SEED,
            claimant.pubkey().as_ref(),
            &new_vault_id.to_le_bytes(),
        ],
        &ario_core::ID,
    );
    let new_vault_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &new_vault_ata,
        &setup.mint_kp.pubkey(),
        &new_vault_pda,
    )
    .await;

    let vaulted_ix = build_vaulted_transfer_ix(
        setup.config_pda,
        claimant.pubkey(),
        counter_pda,
        new_vault_pda,
        payer_ata.pubkey(),
        new_vault_ata.pubkey(),
        payer.pubkey(),
        amount,
        remaining,
        false,
    );

    let msg = build_escrow_canonical(
        "vault",
        &setup.asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let claim_accounts = ario_ant_escrow::accounts::ClaimVaultEthereum {
        escrow: escrow_pda,
        escrow_token_account: escrow_ata.pubkey(),
        claimant_token_account: claimant_ata.pubkey(),
        payer_token_account: payer_ata.pubkey(),
        claimant: claimant.pubkey(),
        depositor: setup.depositor.pubkey(),
        payer: payer.pubkey(),
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
    }
    .to_account_metas(None);
    let claim_data = ario_ant_escrow::instruction::ClaimVaultEthereum {
        message_nonce: escrow_state.nonce,
        signature,
    }
    .data();
    let claim_ix = Ix {
        program_id: ario_ant_escrow::ID,
        accounts: claim_accounts,
        data: claim_data,
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(800_000),
            // Order matters: claim must run FIRST to fund payer_ata from escrow,
            // then vaulted_transfer pulls from payer_ata into the new vault.
            // Reversing causes vaulted_transfer to fail with `insufficient funds`.
            claim_ix,
            vaulted_ix,
        ],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.expect(
        "active-vault ethereum claim should succeed when sibling vaulted_transfer is included",
    );

    let vault_raw = ctx
        .banks_client
        .get_account(new_vault_pda)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_raw.data.as_slice()).unwrap();
    assert_eq!(vault.owner, claimant.pubkey());
    assert_eq!(vault.amount, amount);
    assert!(
        !vault.revocable,
        "new vault must inherit escrow.vault_revocable=false"
    );

    let new_vault_bal = get_token_balance(&mut ctx, &new_vault_ata.pubkey()).await;
    assert_eq!(new_vault_bal, amount);

    // Sanity: silence unused warnings if EscrowError import drifts.
    let _ = EscrowError::MissingVaultedTransferInstruction;
    let _: Pubkey = setup.mint_authority.pubkey();
}
