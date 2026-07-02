//! Cross-program integration tests for the **active-vault re-lock** claim
//! path (ADR-0027: direct CPI into ario-core, restoring what ADR-022
//! disabled).
//!
//! Why this lives in its own file (not `integration.rs`):
//!
//! - The re-lock path CPIs into `ario_core::vaulted_transfer` /
//!   `create_vault`, so a working `ario_core` must be deployed in the test
//!   runtime. Solana txs are atomic — if the CPI fails because ario-core
//!   isn't loaded, the whole claim reverts and the happy path is untestable.
//! - Adding ario-core to the existing `program_test()` would force every
//!   escrow-isolation test to also pay the core-init cost. Splitting keeps
//!   both runtimes minimal.
//!
//! What's covered:
//!
//! - Re-lock happy paths (Arweave-attested + Ethereum): claim a still-locked
//!   30-day vault escrow; assert a native ario-core `Vault` is created for
//!   the claimant with the escrow's exact original unlock time,
//!   non-revocable, and that the payer pass-through nets zero.
//! - `payer == claimant` (the `create_vault` CPI branch — `vaulted_transfer`
//!   rejects sender == recipient).
//! - Sub-`min_vault_duration` liquid fallback + the exact `>=` boundary.
//! - N claims in one tx → N distinct vaults (regression for the ADR-022
//!   sibling-reuse hole; now structurally impossible, pinned anyway).
//! - Wrong (future) vault PDA fails inside the CPI and reverts the claim.
//!
//! The old revocable-sibling rejection test is structurally obsolete: the
//! claim path no longer accepts any client-supplied `vaulted_transfer` —
//! the CPI hardwires `revocable = false` (ADR-021), asserted in the happy
//! paths via `vault.revocable == false && vault.controller == None`.
//!
//! RUNNING: `./scripts/test-integration.sh ario-ant-escrow` — it rebuilds
//! all workspace `.so` files (including `ario_core.so`) into
//! `target/test-fixtures`, sets `BPF_OUT_DIR`, and passes
//! `--features unsafe-allow-test-attestor-pubkey`.

use anchor_lang::{prelude::*, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction as Ix,
    program_pack::Pack,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

use ario_ant_escrow::state::{EscrowToken, ESCROW_VAULT_SEED, PROTOCOL_ARWEAVE, PROTOCOL_ETHEREUM};
use ario_core::state::{
    ArioConfig, Vault, VaultCounter, VaultCreatedEvent, CONFIG_SEED, VAULT_COUNTER_SEED, VAULT_SEED,
};

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
/// `ario_core` (loaded as a real BPF .so so the re-lock CPI can actually
/// create vaults). Pre-adds the BPFLoaderUpgradeable `ProgramData` PDA +
/// funds the upgrade authority so `Initialize` works.
fn program_test_with_core() -> ProgramTest {
    let mut pt = ProgramTest::new(
        "ario_ant_escrow",
        ario_ant_escrow::ID,
        processor!(anchor_processor_escrow),
    );
    pt.set_compute_max_units(800_000);

    // Real ario-core BPF — needed for the re-lock CPI target.
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
/// `treasury`, `migration_authority` are placeholders — the re-lock CPIs
/// only read `config.mint` and the vault duration bounds.
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
            gar_program: solana_sdk::pubkey::Pubkey::default(),
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
    config_pda: Pubkey,
}

/// Set up mint + initialized ario-core + a funded depositor holding
/// `depositor_funding` tokens (enough for however many deposits the test
/// makes).
async fn setup_with_initialized_core(
    ctx: &mut ProgramTestContext,
    depositor_funding: u64,
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
        depositor_funding,
    )
    .await;

    CrossSetup {
        mint_kp,
        mint_authority,
        depositor,
        depositor_ata,
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

#[allow(clippy::too_many_arguments)]
async fn deposit_vault(
    ctx: &mut ProgramTestContext,
    setup: &CrossSetup,
    asset_id: [u8; 32],
    escrow_ata: Pubkey,
    amount: u64,
    lock_duration_seconds: i64,
    protocol: u8,
    pubkey: Vec<u8>,
) {
    let (escrow, _) = escrow_vault_pda(&setup.depositor.pubkey(), &asset_id);
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
        asset_id,
        amount,
        lock_duration_seconds,
        // Escrow rejects revocable deposits (ADR-021); re-locks are always
        // non-revocable.
        revocable: false,
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

/// Warp the program-test clock so `unix_timestamp >= target`. The settle
/// logic reads `Clock::get()` directly, so this is the only knob that
/// affects the re-lock / liquid-fallback / expired branch.
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

fn test_eth_secret_key() -> libsecp256k1::SecretKey {
    let sk_bytes: [u8; 32] = {
        let mut b = [0u8; 32];
        for (i, byte) in b.iter_mut().enumerate() {
            *byte = ((i as u8).wrapping_mul(17).wrapping_add(5)) | 1;
        }
        b
    };
    libsecp256k1::SecretKey::parse(&sk_bytes).unwrap()
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
// Re-lock account set derivation + claim ix builders
// =========================================

/// The six trailing optional accounts a still-locked claim must carry
/// (`ario_core_program` is a constant and filled in by the builders).
struct RelockAccounts {
    payer_token_account: Pubkey,
    ario_core_config: Pubkey,
    recipient_vault_counter: Pubkey,
    vault: Pubkey,
    vault_token_account: Pubkey,
}

/// Derive the claimant's `VaultCounter` PDA and the `Vault` PDA for
/// `vault_id` — the caller-side derivation contract (the SDK reads
/// `counter.next_id` and derives the same way).
fn derive_relock_pdas(claimant: &Pubkey, vault_id: u64) -> (Pubkey, Pubkey) {
    let (counter_pda, _) =
        Pubkey::find_program_address(&[VAULT_COUNTER_SEED, claimant.as_ref()], &ario_core::ID);
    let (vault_pda, _) = Pubkey::find_program_address(
        &[VAULT_SEED, claimant.as_ref(), &vault_id.to_le_bytes()],
        &ario_core::ID,
    );
    (counter_pda, vault_pda)
}

#[allow(clippy::too_many_arguments)]
fn build_claim_vault_ethereum_ix(
    escrow: Pubkey,
    escrow_token_account: Pubkey,
    claimant_token_account: Pubkey,
    claimant: Pubkey,
    depositor: Pubkey,
    payer: Pubkey,
    nonce: [u8; 32],
    signature: [u8; 65],
    relock: Option<&RelockAccounts>,
) -> Ix {
    let accounts = ario_ant_escrow::accounts::ClaimVaultEthereum {
        escrow,
        escrow_token_account,
        claimant_token_account,
        claimant,
        depositor,
        payer,
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
        payer_token_account: relock.map(|r| r.payer_token_account),
        ario_core_config: relock.map(|r| r.ario_core_config),
        recipient_vault_counter: relock.map(|r| r.recipient_vault_counter),
        vault: relock.map(|r| r.vault),
        vault_token_account: relock.map(|r| r.vault_token_account),
        ario_core_program: relock.map(|_| ario_core::ID),
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimVaultEthereum {
        message_nonce: nonce,
        signature,
    }
    .data();
    Ix {
        program_id: ario_ant_escrow::ID,
        accounts,
        data,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_claim_vault_arweave_ix(
    escrow: Pubkey,
    escrow_token_account: Pubkey,
    claimant_token_account: Pubkey,
    claimant: Pubkey,
    depositor: Pubkey,
    payer: Pubkey,
    nonce: [u8; 32],
    relock: Option<&RelockAccounts>,
) -> Ix {
    let accounts = ario_ant_escrow::accounts::ClaimVaultArweaveAttested {
        escrow,
        escrow_token_account,
        claimant_token_account,
        claimant,
        depositor,
        payer,
        instructions_sysvar: solana_sdk::sysvar::instructions::id(),
        token_program: spl_token::id(),
        system_program: solana_sdk::system_program::ID,
        payer_token_account: relock.map(|r| r.payer_token_account),
        ario_core_config: relock.map(|r| r.ario_core_config),
        recipient_vault_counter: relock.map(|r| r.recipient_vault_counter),
        vault: relock.map(|r| r.vault),
        vault_token_account: relock.map(|r| r.vault_token_account),
        ario_core_program: relock.map(|_| ario_core::ID),
    }
    .to_account_metas(None);
    let data = ario_ant_escrow::instruction::ClaimVaultArweaveAttested {
        message_nonce: nonce,
    }
    .data();
    Ix {
        program_id: ario_ant_escrow::ID,
        accounts,
        data,
    }
}

/// Assert the re-locked vault has exactly the shape the escrow promised:
/// owned by the claimant, full amount, the escrow's original unlock time,
/// non-revocable with no controller (ADR-021).
async fn assert_relocked_vault(
    ctx: &mut ProgramTestContext,
    vault_pda: Pubkey,
    claimant: &Pubkey,
    amount: u64,
    expected_end_timestamp: i64,
    expected_vault_id: u64,
) {
    let vault_raw = ctx
        .banks_client
        .get_account(vault_pda)
        .await
        .unwrap()
        .expect("re-locked Vault PDA must exist");
    let vault = Vault::try_deserialize(&mut vault_raw.data.as_slice()).expect("decode Vault");
    assert_eq!(vault.owner, *claimant, "vault must be owned by claimant");
    assert_eq!(vault.vault_id, expected_vault_id);
    assert_eq!(vault.amount, amount, "vault must hold the escrowed amount");
    assert_eq!(
        vault.end_timestamp, expected_end_timestamp,
        "vault must unlock at the escrow's original vault_end_timestamp"
    );
    assert!(
        !vault.revocable,
        "re-locked vault must be non-revocable (ADR-021)"
    );
    assert_eq!(
        vault.controller, None,
        "re-locked vault must have no controller (ADR-021)"
    );
}

// =========================================
// Tests
// =========================================

#[tokio::test]
async fn test_claim_vault_arweave_attested_active_relock_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let asset_id = [0xA1u8; 32];
    let setup = setup_with_initialized_core(&mut ctx, amount).await;
    let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &asset_id);
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
        asset_id,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
        PROTOCOL_ARWEAVE,
        modulus.to_vec(),
    )
    .await;

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_pda).await;

    // Separate payer and claimant — the common relayer shape, exercising
    // the `vaulted_transfer` CPI branch.
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

    // First-ever vault for this claimant → vault_id 0. The vault's token
    // account must be pre-created (owner = the not-yet-initialized vault
    // PDA), same as the SDK will do.
    let (counter_pda, new_vault_pda) = derive_relock_pdas(&claimant.pubkey(), 0);
    let new_vault_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &new_vault_ata,
        &setup.mint_kp.pubkey(),
        &new_vault_pda,
    )
    .await;

    // Build the canonical message exactly as the on-chain attested ix
    // reconstructs it (binds escrow.recipient_pubkey via the `recipient`
    // line — F-1), and sign with the test attestor key.
    let canonical = build_escrow_canonical(
        "vault",
        &asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &modulus,
    );
    use ed25519_dalek::Signer;
    let kp = test_attestor_keypair();
    let attest_sig: [u8; 64] = kp.sign(&canonical).to_bytes();
    let attestor_pubkey_bytes: [u8; 32] = kp.public.to_bytes();
    let ed25519_ix = build_ed25519_sigverify_ix(&attestor_pubkey_bytes, &attest_sig, &canonical);

    let relock = RelockAccounts {
        payer_token_account: payer_ata.pubkey(),
        ario_core_config: setup.config_pda,
        recipient_vault_counter: counter_pda,
        vault: new_vault_pda,
        vault_token_account: new_vault_ata.pubkey(),
    };
    let claim_ix = build_claim_vault_arweave_ix(
        escrow_pda,
        escrow_ata.pubkey(),
        claimant_ata.pubkey(),
        claimant.pubkey(),
        setup.depositor.pubkey(),
        payer.pubkey(),
        escrow_state.nonce,
        Some(&relock),
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            // Ed25519 sigverify MUST be at claim_ix - 1 (introspection).
            ed25519_ix,
            claim_ix,
        ],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .expect("banks tx");
    result
        .result
        .expect("active-vault attested claim should re-lock via direct CPI");

    // Escrow PDA closed.
    let escrow_acct = ctx.banks_client.get_account(escrow_pda).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after claim"
    );

    // New vault: claimant-owned, exact original unlock time, non-revocable.
    assert_relocked_vault(
        &mut ctx,
        new_vault_pda,
        &claimant.pubkey(),
        amount,
        escrow_state.vault_end_timestamp,
        0,
    )
    .await;

    // Counter advanced.
    let counter_raw = ctx
        .banks_client
        .get_account(counter_pda)
        .await
        .unwrap()
        .unwrap();
    let counter = VaultCounter::try_deserialize(&mut counter_raw.data.as_slice()).unwrap();
    assert_eq!(counter.next_id, 1, "counter must advance after first vault");

    // Tokens settled in the new vault's token account, not the claimant's
    // primary ATA — and the payer pass-through nets zero (no-skim).
    assert_eq!(
        get_token_balance(&mut ctx, &new_vault_ata.pubkey()).await,
        amount,
        "active claim must re-lock tokens into the new vault"
    );
    assert_eq!(
        get_token_balance(&mut ctx, &claimant_ata.pubkey()).await,
        0,
        "active claim must NOT deliver liquid tokens to the claimant"
    );
    assert_eq!(
        get_token_balance(&mut ctx, &payer_ata.pubkey()).await,
        0,
        "payer pass-through must net zero (no skim)"
    );

    // ario-core (BPF-dispatched) logs its VaultCreatedEvent.
    let logs = result.metadata.expect("metadata").log_messages;
    let event: VaultCreatedEvent =
        ario_test_utils::parse_event(&logs).expect("VaultCreatedEvent must be emitted by the CPI");
    assert_eq!(event.owner, claimant.pubkey());
    assert_eq!(event.amount, amount);
    assert_eq!(event.end_timestamp, escrow_state.vault_end_timestamp);
    assert!(!event.revocable);
}

#[tokio::test]
async fn test_claim_vault_ethereum_active_relock_happy_path() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let asset_id = [0xA2u8; 32];
    let setup = setup_with_initialized_core(&mut ctx, amount).await;
    let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_pda, &setup.mint_kp.pubkey()).await;

    let secret_key = test_eth_secret_key();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault(
        &mut ctx,
        &setup,
        asset_id,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
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

    let (counter_pda, new_vault_pda) = derive_relock_pdas(&claimant.pubkey(), 0);
    let new_vault_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &new_vault_ata,
        &setup.mint_kp.pubkey(),
        &new_vault_pda,
    )
    .await;

    let msg = build_escrow_canonical(
        "vault",
        &asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let relock = RelockAccounts {
        payer_token_account: payer_ata.pubkey(),
        ario_core_config: setup.config_pda,
        recipient_vault_counter: counter_pda,
        vault: new_vault_pda,
        vault_token_account: new_vault_ata.pubkey(),
    };
    let claim_ix = build_claim_vault_ethereum_ix(
        escrow_pda,
        escrow_ata.pubkey(),
        claimant_ata.pubkey(),
        claimant.pubkey(),
        setup.depositor.pubkey(),
        payer.pubkey(),
        escrow_state.nonce,
        signature,
        Some(&relock),
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            claim_ix,
        ],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("active-vault ethereum claim should re-lock via direct CPI");

    assert_relocked_vault(
        &mut ctx,
        new_vault_pda,
        &claimant.pubkey(),
        amount,
        escrow_state.vault_end_timestamp,
        0,
    )
    .await;
    assert_eq!(
        get_token_balance(&mut ctx, &new_vault_ata.pubkey()).await,
        amount
    );
    assert_eq!(get_token_balance(&mut ctx, &claimant_ata.pubkey()).await, 0);
    assert_eq!(get_token_balance(&mut ctx, &payer_ata.pubkey()).await, 0);
}

/// `payer == claimant` routes through the `create_vault` CPI branch
/// (`vaulted_transfer` rejects sender == recipient with `SelfTransfer`)
/// and must produce an identically-shaped vault. The claimant's single
/// token account serves as both `claimant_token_account` and
/// `payer_token_account`.
#[tokio::test]
async fn test_claim_vault_active_payer_equals_claimant() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let asset_id = [0xA3u8; 32];
    let setup = setup_with_initialized_core(&mut ctx, amount).await;
    let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_pda, &setup.mint_kp.pubkey()).await;

    let secret_key = test_eth_secret_key();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault(
        &mut ctx,
        &setup,
        asset_id,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await;

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_pda).await;

    let claimant = Keypair::new();
    let claimant_ata = Keypair::new();
    airdrop(&mut ctx, &claimant.pubkey(), 5_000_000_000).await;
    create_token_account(
        &mut ctx,
        &claimant_ata,
        &setup.mint_kp.pubkey(),
        &claimant.pubkey(),
    )
    .await;

    let (counter_pda, new_vault_pda) = derive_relock_pdas(&claimant.pubkey(), 0);
    let new_vault_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &new_vault_ata,
        &setup.mint_kp.pubkey(),
        &new_vault_pda,
    )
    .await;

    let msg = build_escrow_canonical(
        "vault",
        &asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let relock = RelockAccounts {
        payer_token_account: claimant_ata.pubkey(),
        ario_core_config: setup.config_pda,
        recipient_vault_counter: counter_pda,
        vault: new_vault_pda,
        vault_token_account: new_vault_ata.pubkey(),
    };
    let claim_ix = build_claim_vault_ethereum_ix(
        escrow_pda,
        escrow_ata.pubkey(),
        claimant_ata.pubkey(),
        claimant.pubkey(),
        setup.depositor.pubkey(),
        claimant.pubkey(), // payer == claimant
        escrow_state.nonce,
        signature,
        Some(&relock),
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            claim_ix,
        ],
        Some(&claimant.pubkey()),
        &[&claimant],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("payer==claimant claim should re-lock via the create_vault branch");

    assert_relocked_vault(
        &mut ctx,
        new_vault_pda,
        &claimant.pubkey(),
        amount,
        escrow_state.vault_end_timestamp,
        0,
    )
    .await;
    assert_eq!(
        get_token_balance(&mut ctx, &new_vault_ata.pubkey()).await,
        amount
    );
    // The pass-through account (== claimant's ATA) nets zero.
    assert_eq!(get_token_balance(&mut ctx, &claimant_ata.pubkey()).await, 0);
}

/// A still-locked claim whose remainder is UNDER `min_vault_duration`
/// delivers liquid immediately — even with the full re-lock account set
/// passed. No vault is created (ADR-0027's bounded early-liquidity window).
#[tokio::test]
async fn test_claim_vault_near_expiry_liquid_fallback() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let asset_id = [0xA4u8; 32];
    let setup = setup_with_initialized_core(&mut ctx, amount).await;
    let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_pda, &setup.mint_kp.pubkey()).await;

    let secret_key = test_eth_secret_key();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault(
        &mut ctx,
        &setup,
        asset_id,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await;

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_pda).await;

    // Warp so remaining = min_vault_duration - 1h (~13d23h): still locked,
    // but under the re-lock floor.
    warp_clock_to(
        &mut ctx,
        escrow_state.vault_end_timestamp - ArioConfig::DEFAULT_MIN_VAULT_DURATION + 3_600,
    )
    .await;

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

    let (counter_pda, new_vault_pda) = derive_relock_pdas(&claimant.pubkey(), 0);
    let new_vault_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &new_vault_ata,
        &setup.mint_kp.pubkey(),
        &new_vault_pda,
    )
    .await;

    let msg = build_escrow_canonical(
        "vault",
        &asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let relock = RelockAccounts {
        payer_token_account: payer_ata.pubkey(),
        ario_core_config: setup.config_pda,
        recipient_vault_counter: counter_pda,
        vault: new_vault_pda,
        vault_token_account: new_vault_ata.pubkey(),
    };
    let claim_ix = build_claim_vault_ethereum_ix(
        escrow_pda,
        escrow_ata.pubkey(),
        claimant_ata.pubkey(),
        claimant.pubkey(),
        setup.depositor.pubkey(),
        payer.pubkey(),
        escrow_state.nonce,
        signature,
        Some(&relock),
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            claim_ix,
        ],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("sub-minimum-remainder claim should deliver liquid");

    // Liquid delivery, no vault created, counter never initialized.
    assert_eq!(
        get_token_balance(&mut ctx, &claimant_ata.pubkey()).await,
        amount,
        "sub-minimum remainder must deliver liquid to the claimant"
    );
    assert_eq!(get_token_balance(&mut ctx, &payer_ata.pubkey()).await, 0);
    assert!(
        ctx.banks_client
            .get_account(new_vault_pda)
            .await
            .unwrap()
            .is_none(),
        "no vault must be created on the liquid fallback"
    );
    assert!(
        ctx.banks_client
            .get_account(counter_pda)
            .await
            .unwrap()
            .is_none(),
        "no vault counter must be created on the liquid fallback"
    );
    let escrow_acct = ctx.banks_client.get_account(escrow_pda).await.unwrap();
    assert!(
        escrow_acct.is_none() || escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA should be closed after claim"
    );
}

/// Boundary: `remaining == min_vault_duration` exactly must RE-LOCK (the
/// branch is `>=`). Pins the boundary the same way
/// `test_claim_vault_ethereum_at_unlock_boundary` pins the expired `>=`.
#[tokio::test]
async fn test_claim_vault_exact_min_boundary_relocks() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let asset_id = [0xA5u8; 32];
    let setup = setup_with_initialized_core(&mut ctx, amount).await;
    let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_pda, &setup.mint_kp.pubkey()).await;

    let secret_key = test_eth_secret_key();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault(
        &mut ctx,
        &setup,
        asset_id,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
        PROTOCOL_ETHEREUM,
        eth_addr.to_vec(),
    )
    .await;

    let escrow_state = fetch_escrow_token(&mut ctx, escrow_pda).await;

    // Warp so remaining == min_vault_duration exactly.
    warp_clock_to(
        &mut ctx,
        escrow_state.vault_end_timestamp - ArioConfig::DEFAULT_MIN_VAULT_DURATION,
    )
    .await;

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

    let (counter_pda, new_vault_pda) = derive_relock_pdas(&claimant.pubkey(), 0);
    let new_vault_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &new_vault_ata,
        &setup.mint_kp.pubkey(),
        &new_vault_pda,
    )
    .await;

    let msg = build_escrow_canonical(
        "vault",
        &asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let relock = RelockAccounts {
        payer_token_account: payer_ata.pubkey(),
        ario_core_config: setup.config_pda,
        recipient_vault_counter: counter_pda,
        vault: new_vault_pda,
        vault_token_account: new_vault_ata.pubkey(),
    };
    let claim_ix = build_claim_vault_ethereum_ix(
        escrow_pda,
        escrow_ata.pubkey(),
        claimant_ata.pubkey(),
        claimant.pubkey(),
        setup.depositor.pubkey(),
        payer.pubkey(),
        escrow_state.nonce,
        signature,
        Some(&relock),
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            claim_ix,
        ],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("remaining == min_vault_duration must take the re-lock branch (>= semantics)");

    assert_relocked_vault(
        &mut ctx,
        new_vault_pda,
        &claimant.pubkey(),
        amount,
        escrow_state.vault_end_timestamp,
        0,
    )
    .await;
    assert_eq!(
        get_token_balance(&mut ctx, &claimant_ata.pubkey()).await,
        0,
        "boundary claim must re-lock, not deliver liquid"
    );
}

/// Regression for the ADR-022 reuse hole: N active claims in ONE tx (same
/// claimant, identical amounts — the old attack construction) must produce
/// N distinct, fully-funded vaults. With the direct CPI this is structural
/// (each claim performs its own re-lock); this pins it forever.
#[tokio::test]
async fn test_n_claims_produce_n_vaults() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let asset_ids = [[0xB1u8; 32], [0xB2u8; 32]];
    // Fund the depositor for both deposits.
    let setup = setup_with_initialized_core(&mut ctx, amount * asset_ids.len() as u64).await;

    let secret_key = test_eth_secret_key();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    let mut escrows = Vec::new();
    for asset_id in asset_ids {
        let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &asset_id);
        let escrow_ata =
            create_escrow_token_account(&mut ctx, &escrow_pda, &setup.mint_kp.pubkey()).await;
        deposit_vault(
            &mut ctx,
            &setup,
            asset_id,
            escrow_ata.pubkey(),
            amount,
            lock_duration,
            PROTOCOL_ETHEREUM,
            eth_addr.to_vec(),
        )
        .await;
        let state = fetch_escrow_token(&mut ctx, escrow_pda).await;
        escrows.push((asset_id, escrow_pda, escrow_ata, state));
    }

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

    // Derive vault ids 0..N up front (the counter advances once per claim
    // as the tx's instructions execute sequentially).
    let mut vault_pdas = Vec::new();
    let mut vault_atas = Vec::new();
    for id in 0..asset_ids.len() as u64 {
        let (_, vault_pda) = derive_relock_pdas(&claimant.pubkey(), id);
        let vault_ata = Keypair::new();
        create_token_account(&mut ctx, &vault_ata, &setup.mint_kp.pubkey(), &vault_pda).await;
        vault_pdas.push(vault_pda);
        vault_atas.push(vault_ata);
    }
    let (counter_pda, _) = derive_relock_pdas(&claimant.pubkey(), 0);

    let mut ixs =
        vec![solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(800_000)];
    for (i, (asset_id, escrow_pda, escrow_ata, state)) in escrows.iter().enumerate() {
        let msg = build_escrow_canonical(
            "vault",
            asset_id,
            amount,
            &claimant.pubkey(),
            &state.nonce,
            &eth_addr,
        );
        let (signature, _) = sign_ethereum(&msg, &secret_key);
        let relock = RelockAccounts {
            payer_token_account: payer_ata.pubkey(),
            ario_core_config: setup.config_pda,
            recipient_vault_counter: counter_pda,
            vault: vault_pdas[i],
            vault_token_account: vault_atas[i].pubkey(),
        };
        ixs.push(build_claim_vault_ethereum_ix(
            *escrow_pda,
            escrow_ata.pubkey(),
            claimant_ata.pubkey(),
            claimant.pubkey(),
            setup.depositor.pubkey(),
            payer.pubkey(),
            state.nonce,
            signature,
            Some(&relock),
        ));
    }

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&ixs, Some(&payer.pubkey()), &[&payer], blockhash);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("N active claims in one tx must each re-lock independently");

    // N distinct vaults, each fully funded.
    for (i, vault_pda) in vault_pdas.iter().enumerate() {
        assert_relocked_vault(
            &mut ctx,
            *vault_pda,
            &claimant.pubkey(),
            amount,
            escrows[i].3.vault_end_timestamp,
            i as u64,
        )
        .await;
        assert_eq!(
            get_token_balance(&mut ctx, &vault_atas[i].pubkey()).await,
            amount,
            "each claim must fund its own vault in full"
        );
    }
    let counter_raw = ctx
        .banks_client
        .get_account(counter_pda)
        .await
        .unwrap()
        .unwrap();
    let counter = VaultCounter::try_deserialize(&mut counter_raw.data.as_slice()).unwrap();
    assert_eq!(counter.next_id, asset_ids.len() as u64);
    // The shared pass-through account nets zero across all claims.
    assert_eq!(get_token_balance(&mut ctx, &payer_ata.pubkey()).await, 0);
    assert_eq!(get_token_balance(&mut ctx, &claimant_ata.pubkey()).await, 0);
}

/// Passing the WRONG (future) vault PDA fails inside ario-core's seed
/// validation and atomically reverts the claim — pinning the caller retry
/// contract (re-derive from `counter.next_id` and resubmit).
#[tokio::test]
async fn test_claim_vault_active_wrong_vault_pda_fails() {
    if skip_if_no_bpf_artifacts() {
        return;
    }
    let mut ctx = program_test_with_core().start_with_context().await;

    let amount = 500_000_000u64;
    let lock_duration = 30 * 86_400i64;
    let asset_id = [0xA6u8; 32];
    let setup = setup_with_initialized_core(&mut ctx, amount).await;
    let (escrow_pda, _) = escrow_vault_pda(&setup.depositor.pubkey(), &asset_id);
    let escrow_ata =
        create_escrow_token_account(&mut ctx, &escrow_pda, &setup.mint_kp.pubkey()).await;

    let secret_key = test_eth_secret_key();
    let (_, eth_addr) = sign_ethereum(b"dummy", &secret_key);

    deposit_vault(
        &mut ctx,
        &setup,
        asset_id,
        escrow_ata.pubkey(),
        amount,
        lock_duration,
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

    // Wrong: vault PDA for id 1 while the counter's next_id is 0.
    let (counter_pda, wrong_vault_pda) = derive_relock_pdas(&claimant.pubkey(), 1);
    let wrong_vault_ata = Keypair::new();
    create_token_account(
        &mut ctx,
        &wrong_vault_ata,
        &setup.mint_kp.pubkey(),
        &wrong_vault_pda,
    )
    .await;

    let msg = build_escrow_canonical(
        "vault",
        &asset_id,
        amount,
        &claimant.pubkey(),
        &escrow_state.nonce,
        &eth_addr,
    );
    let (signature, _) = sign_ethereum(&msg, &secret_key);

    let relock = RelockAccounts {
        payer_token_account: payer_ata.pubkey(),
        ario_core_config: setup.config_pda,
        recipient_vault_counter: counter_pda,
        vault: wrong_vault_pda,
        vault_token_account: wrong_vault_ata.pubkey(),
    };
    let claim_ix = build_claim_vault_ethereum_ix(
        escrow_pda,
        escrow_ata.pubkey(),
        claimant_ata.pubkey(),
        claimant.pubkey(),
        setup.depositor.pubkey(),
        payer.pubkey(),
        escrow_state.nonce,
        signature,
        Some(&relock),
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::compute_budget::ComputeBudgetInstruction::set_compute_unit_limit(400_000),
            claim_ix,
        ],
        Some(&payer.pubkey()),
        &[&payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect_err("wrong vault PDA must fail ario-core's seed validation");

    // Atomic revert: escrow untouched, no vault created, pass-through undone.
    let escrow_acct = ctx.banks_client.get_account(escrow_pda).await.unwrap();
    assert!(
        escrow_acct.is_some() && !escrow_acct.as_ref().unwrap().data.is_empty(),
        "escrow PDA must survive the reverted claim"
    );
    assert_eq!(
        get_token_balance(&mut ctx, &escrow_ata.pubkey()).await,
        amount,
        "escrow tokens must be untouched"
    );
    assert_eq!(get_token_balance(&mut ctx, &payer_ata.pubkey()).await, 0);
    assert!(ctx
        .banks_client
        .get_account(wrong_vault_pda)
        .await
        .unwrap()
        .is_none());
}
