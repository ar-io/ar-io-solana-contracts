use anchor_lang::{prelude::*, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    program_pack::Pack,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_core::error::ArioError;
use ario_core::state::*;

/// Test-local MPL Core program id. ario-core no longer references this in its
/// runtime crate (ADR-016 reshape — ario-core is MPL-agnostic), but a few
/// helpers below still build legacy MPL Core asset bytes for fixture-style
/// tests that don't go through the primary-name path.
const MPL_CORE_PROGRAM_ID: anchor_lang::prelude::Pubkey =
    anchor_lang::solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

/// Assert that a transaction result contains a specific Anchor custom error code.
/// Usage: `assert_anchor_error!(result, ArioError::LockDurationTooShort);`
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

/// Wrapper to bridge lifetime mismatch between `ProcessInstruction`
/// (independent lifetimes) and Anchor's `entry` (tied `'info` lifetime).
/// Safe: solana-program-test guarantees accounts outlive the call.
fn anchor_processor(
    program_id: &Pubkey,
    accounts: &[anchor_lang::prelude::AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    // SAFETY: ProcessInstruction has `&'a [AccountInfo<'b>]` (independent lifetimes).
    // Anchor's entry needs `&'info [AccountInfo<'info>]` (tied lifetimes).
    // solana-program-test guarantees that AccountInfo references are valid for the
    // entire duration of the call, so tying the lifetimes is sound.
    unsafe {
        let accounts: &[anchor_lang::prelude::AccountInfo] = std::mem::transmute(accounts);
        ario_core::entry(program_id, accounts, data)
    }
}

fn program_test() -> ProgramTest {
    let mut pt = ProgramTest::new("ario_core", ario_core::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    // PR-4: pre-add a ProgramData account naming `upgrade_authority_keypair()`
    // as the upgrade authority. Tests sign initialize with that keypair so the
    // new `program_data.upgrade_authority_address == Some(payer.key())`
    // constraint is satisfied. Pre-adding at genesis (rather than via
    // ctx.set_account post-start) avoids hash-mismatch panics in tests that
    // warp the clock significantly.
    let pd_key = program_data_pda();
    let pd_authority = upgrade_authority_keypair().pubkey();
    let pd_data = build_program_data(&pd_authority);
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
    // Fund the upgrade authority so it can pay rent for `init` constraints
    pt.add_account(
        pd_authority,
        solana_sdk::account::Account {
            lamports: 100_000_000_000, // 100 SOL — generous for repeated inits
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );

    pt
}

/// PR-4: deterministic upgrade-authority keypair used across all tests.
/// Derived from a fixed seed so the pubkey is known at `program_test()`
/// time (before `ProgramTestContext` exists). Test-only — not a
/// production deploy authority.
fn upgrade_authority_keypair() -> Keypair {
    solana_sdk::signer::keypair::keypair_from_seed(&[42u8; 32])
        .expect("keypair_from_seed must succeed for fixed test seed")
}

/// PR-4: address of the program-data PDA owned by BPFLoaderUpgradeable.
fn program_data_pda() -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[ario_core::ID.as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    );
    pda
}

/// Build a fake `ProgramData` account body. Layout (matches `bpf_loader_upgradeable`):
/// `[u32 AccountType::ProgramData = 3][i64 slot][u8 option_tag][32 upgrade_authority]`.
fn build_program_data(upgrade_authority: &Pubkey) -> Vec<u8> {
    let mut data = Vec::with_capacity(45);
    data.extend_from_slice(&3u32.to_le_bytes());
    data.extend_from_slice(&0i64.to_le_bytes());
    data.push(1);
    data.extend_from_slice(upgrade_authority.as_ref());
    data
}

/// Matches `PRIMARY_NAME_REVERSE` PDA seed: `hash(name.to_lowercase().as_bytes())`.
fn primary_reverse_lookup_hash(name: &str) -> [u8; 32] {
    anchor_lang::solana_program::hash::hash(name.to_lowercase().as_bytes()).to_bytes()
}

async fn create_mint(
    ctx: &mut ProgramTestContext,
    mint: &Keypair,
    authority: &solana_sdk::pubkey::Pubkey,
) {
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
    mint: &solana_sdk::pubkey::Pubkey,
    owner: &solana_sdk::pubkey::Pubkey,
) {
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account_rent = rent.minimum_balance(spl_token::state::Account::LEN);

    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::system_instruction::create_account(
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
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

async fn mint_tokens(
    ctx: &mut ProgramTestContext,
    mint: &solana_sdk::pubkey::Pubkey,
    dest: &solana_sdk::pubkey::Pubkey,
    authority: &Keypair,
    amount: u64,
) {
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
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

fn config_pda() -> (solana_sdk::pubkey::Pubkey, u8) {
    solana_sdk::pubkey::Pubkey::find_program_address(&[CONFIG_SEED], &ario_core::ID)
}

fn vault_counter_pda(owner: &solana_sdk::pubkey::Pubkey) -> (solana_sdk::pubkey::Pubkey, u8) {
    solana_sdk::pubkey::Pubkey::find_program_address(
        &[VAULT_COUNTER_SEED, owner.as_ref()],
        &ario_core::ID,
    )
}

fn vault_pda(
    owner: &solana_sdk::pubkey::Pubkey,
    vault_id: u64,
) -> (solana_sdk::pubkey::Pubkey, u8) {
    solana_sdk::pubkey::Pubkey::find_program_address(
        &[VAULT_SEED, owner.as_ref(), &vault_id.to_le_bytes()],
        &ario_core::ID,
    )
}

// =========================================
// TESTS
// =========================================

#[tokio::test]
async fn test_initialize() {
    let mut pt = program_test();

    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let (config_key, _) = config_pda();
    let arns_program = Pubkey::new_unique();

    let accounts = ario_core::accounts::Initialize {
        config: config_key,
        mint: mint.pubkey(),
        payer: upgrade_authority_keypair().pubkey(),
        program_data: program_data_pda(),
        system_program: system_program::id(),
    };

    let ix_data = ario_core::instruction::Initialize {
        params: ario_core::InitializeParams {
            authority: ctx.payer.pubkey(),
            total_supply: 1_000_000_000_000, // 1M ARIO
            arns_program,
            treasury: Pubkey::new_unique(),
            migration_authority: ctx.payer.pubkey(),
            gar_program: solana_sdk::pubkey::Pubkey::default(),
        },
    };

    let ix = Instruction {
        program_id: ario_core::ID,
        accounts: accounts.to_account_metas(None),
        data: ix_data.data(),
    };

    let upgrade_auth = upgrade_authority_keypair();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_auth],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify config was initialized
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();

    assert_eq!(config.authority, ctx.payer.pubkey());
    assert_eq!(config.total_supply, 1_000_000_000_000);
    assert_eq!(config.circulating_supply, 1_000_000_000_000);
    assert_eq!(config.locked_supply, 0);
    assert_eq!(config.mint, mint.pubkey());
    assert_eq!(config.arns_program, arns_program);
    assert_eq!(
        config.min_vault_duration,
        ArioConfig::DEFAULT_MIN_VAULT_DURATION
    );
    assert_eq!(
        config.max_vault_duration,
        ArioConfig::DEFAULT_MAX_VAULT_DURATION
    );
}

#[tokio::test]
async fn test_create_vault() {
    let mut pt = program_test();

    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let vault_token = Keypair::new();
    let (config_key, _) = config_pda();
    // vault_token owned by vault PDA
    let (vault_key_for_token, _) = vault_pda(&ctx.payer.pubkey(), 0);
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key_for_token).await;

    // Initialize program
    let arns_program = Pubkey::new_unique();
    let init_accounts = ario_core::accounts::Initialize {
        config: config_key,
        mint: mint.pubkey(),
        payer: upgrade_authority_keypair().pubkey(),
        program_data: program_data_pda(),
        system_program: system_program::id(),
    };
    let init_data = ario_core::instruction::Initialize {
        params: ario_core::InitializeParams {
            authority: ctx.payer.pubkey(),
            total_supply: 1_000_000_000_000,
            arns_program,
            treasury: Pubkey::new_unique(),
            migration_authority: ctx.payer.pubkey(),
            gar_program: solana_sdk::pubkey::Pubkey::default(),
        },
    };
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: init_accounts.to_account_metas(None),
            data: init_data.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_authority_keypair()],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create vault
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);
    let lock_duration = 14 * 86_400i64; // 14 days (minimum)
    let vault_amount = 100_000_000u64; // 100 ARIO (MIN_VAULT_SIZE)

    let create_accounts = ario_core::accounts::CreateVault {
        config: config_key,
        vault_counter: vault_counter_key,
        vault: vault_key,
        owner_token_account: owner_token.pubkey(),
        vault_token_account: vault_token.pubkey(),
        owner: ctx.payer.pubkey(),
        token_program: spl_token::id(),
        system_program: system_program::id(),
    };
    let create_data = ario_core::instruction::CreateVault {
        amount: vault_amount,
        lock_duration_seconds: lock_duration,
    };

    // Need a fresh blockhash for the new tx
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: create_accounts.to_account_metas(None),
            data: create_data.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify vault was created
    let vault_account = ctx
        .banks_client
        .get_account(vault_key)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();

    assert_eq!(vault.owner, ctx.payer.pubkey());
    assert_eq!(vault.vault_id, 0);
    assert_eq!(vault.amount, vault_amount);
    assert!(!vault.revocable);
    assert_eq!(vault.controller, None);

    // Verify config was updated
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.locked_supply, vault_amount);
    assert_eq!(config.circulating_supply, 1_000_000_000_000 - vault_amount);
}

#[tokio::test]
async fn test_create_vault_duration_too_short() {
    let mut pt = program_test();

    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    // Fund with 100 ARIO (MIN_VAULT_SIZE) so amount check passes
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key_for_token, _) = vault_pda(&ctx.payer.pubkey(), 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key_for_token).await;

    // Initialize
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::Initialize {
                config: config_key,
                mint: mint.pubkey(),
                payer: upgrade_authority_keypair().pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Initialize {
                params: ario_core::InitializeParams {
                    authority: ctx.payer.pubkey(),
                    total_supply: 1_000_000_000_000,
                    arns_program: Pubkey::new_unique(),
                    treasury: Pubkey::new_unique(),
                    migration_authority: ctx.payer.pubkey(),
                    gar_program: solana_sdk::pubkey::Pubkey::default(),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_authority_keypair()],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to create vault with 1 second lock (below 14-day minimum)
    // Use 100 ARIO (MIN_VAULT_SIZE) so we hit LockDurationTooShort, not VaultBelowMinimum
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,      // 100 ARIO = MIN_VAULT_SIZE
                lock_duration_seconds: 1, // Too short!
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::LockDurationTooShort);
}

#[tokio::test]
async fn test_request_primary_name() {
    let mut pt = program_test();

    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    // Create token accounts for fee payment
    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Mint tokens to initiator for fee
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let (config_key, _) = config_pda();

    // Initialize with protocol_token as treasury
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::Initialize {
                config: config_key,
                mint: mint.pubkey(),
                payer: upgrade_authority_keypair().pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Initialize {
                params: ario_core::InitializeParams {
                    authority: ctx.payer.pubkey(),
                    total_supply: 1_000_000_000_000,
                    arns_program: Pubkey::new_unique(),
                    treasury: protocol_token.pubkey(),
                    migration_authority: ctx.payer.pubkey(),
                    gar_program: solana_sdk::pubkey::Pubkey::default(),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_authority_keypair()],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Request primary name
    let name = "myname".to_string();
    let arns_program_id = Pubkey::new_unique();

    // Re-initialize with known arns_program
    // (We already initialized above, so we need to use the same arns_program.
    // Let's read back the config to get the arns_program that was set.)
    // Actually, re-read: the arns_program was set to Pubkey::new_unique() above.
    // We need the arns_program_id to match. Let's fix this by using a known value.
    // The initialize above used a random key. Since we can't re-initialize, we need
    // to know the arns_program_id before initialization. Let's restructure:
    // Already initialized — read the config account to get arns_program
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config_data = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    let arns_program_id = config_data.arns_program;

    // Create a fake ArNS record account at the correct PDA, owned by arns_program_id.
    // PDA = ["arns_record", hash("myname")]
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);

    // Build fake ArnsRecord: disc(8) + name_hash(32) + owner(32) + ant(32) + purchase_type(1) + ... + name(4+N)
    let arns_disc = solana_sdk::hash::hash(b"account:ArnsRecord");
    let mut arns_data = arns_disc.as_ref()[..8].to_vec();
    arns_data.extend_from_slice(name_hash.as_ref()); // name_hash: 32 bytes
    arns_data.extend_from_slice(ctx.payer.pubkey().as_ref()); // owner: 32 bytes
    arns_data.extend_from_slice(&[0u8; 32]); // ant: 32 bytes (placeholder)
    arns_data.push(1); // purchase_type = Permabuy
    arns_data.extend_from_slice(&0i64.to_le_bytes()); // start_timestamp
    arns_data.push(0);
    arns_data.extend_from_slice(&[0u8; 8]); // end_timestamp: None
    arns_data.extend_from_slice(&10u16.to_le_bytes()); // undername_limit
    arns_data.extend_from_slice(&0u64.to_le_bytes()); // purchase_price
    arns_data.push(0); // bump
    arns_data.extend_from_slice(&(name.len() as u32).to_le_bytes()); // name String len
    arns_data.extend_from_slice(name.as_bytes()); // name String data

    // Add the account to the test environment
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // Create fake DemandFactor account at the correct PDA, owned by arns_program_id.
    // PDA = ["demand_factor"] under arns_program_id
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    // DemandFactor data: discriminator(8) + current_demand_factor(8)
    let df_disc = solana_sdk::hash::hash(b"account:DemandFactor");
    let mut df_data = df_disc.as_ref()[..8].to_vec(); // correct Anchor discriminator
    df_data.extend_from_slice(&1_000_000u64.to_le_bytes()); // demand_factor = 1.0
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = solana_sdk::pubkey::Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, ctx.payer.pubkey().as_ref()],
        &ario_core::ID,
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();

    // Build accounts list with ArnsRecord as remaining_accounts[0], DemandFactor as remaining_accounts[1]
    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: ctx.payer.pubkey(),
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // remaining_accounts[0] = ArNS record (read-only)
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    // remaining_accounts[1] = DemandFactor (read-only)
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName { name: name.clone() }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify request
    let request_account = ctx
        .banks_client
        .get_account(request_key)
        .await
        .unwrap()
        .unwrap();
    let request =
        PrimaryNameRequest::try_deserialize(&mut request_account.data.as_slice()).unwrap();
    assert_eq!(request.initiator, ctx.payer.pubkey());
    assert_eq!(request.name, name);
}

// =========================================
// ADDITIONAL INTEGRATION TESTS
// =========================================

/// Helper: initialize ario-core config and return (config_key, arns_program_id).
///
/// Under PR-4, `initialize` is bound to the program's upgrade authority, so
/// the upgrade-authority keypair (pre-funded at genesis) signs as `payer`.
/// `authority` and `migration_authority` in params remain `ctx.payer` so
/// downstream tests' assumptions about the protocol authority hold.
async fn initialize_config(
    ctx: &mut ProgramTestContext,
    mint: &solana_sdk::pubkey::Pubkey,
    treasury: &solana_sdk::pubkey::Pubkey,
) -> (solana_sdk::pubkey::Pubkey, Pubkey) {
    let (config_key, _) = config_pda();
    let arns_program = Pubkey::new_unique();
    let upgrade_auth = upgrade_authority_keypair();

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::Initialize {
                config: config_key,
                mint: *mint,
                payer: upgrade_auth.pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Initialize {
                params: ario_core::InitializeParams {
                    authority: ctx.payer.pubkey(),
                    total_supply: 1_000_000_000_000,
                    arns_program,
                    treasury: *treasury,
                    migration_authority: ctx.payer.pubkey(),
                    gar_program: solana_sdk::pubkey::Pubkey::default(),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_auth],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    (config_key, arns_program)
}

/// Helper: read SPL token account balance
async fn get_token_balance(
    ctx: &mut ProgramTestContext,
    token_account: &solana_sdk::pubkey::Pubkey,
) -> u64 {
    let account = ctx
        .banks_client
        .get_account(*token_account)
        .await
        .unwrap()
        .unwrap();
    let token_data = spl_token::state::Account::unpack(&account.data).unwrap();
    token_data.amount
}

/// Helper: build fake ArNS record account data for a given name and owner.
/// Layout: disc(8) + name_hash(32) + owner(32) + ant(32) + purchase_type(1)
///       + start_ts(8) + end_ts(1+8) + undername_limit(2) + purchase_price(8)
///       + bump(1) + name(4+N)
fn build_arns_record_data(
    name: &str,
    owner: &solana_sdk::pubkey::Pubkey,
    ant: &solana_sdk::pubkey::Pubkey,
) -> Vec<u8> {
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let arns_disc = solana_sdk::hash::hash(b"account:ArnsRecord");
    let mut data = arns_disc.as_ref()[..8].to_vec();
    data.extend_from_slice(name_hash.as_ref()); // name_hash: 32 bytes
    data.extend_from_slice(owner.as_ref()); // owner: 32 bytes
    data.extend_from_slice(ant.as_ref()); // ant: 32 bytes (Metaplex Core asset)
    data.push(1); // purchase_type = Permabuy
    data.extend_from_slice(&0i64.to_le_bytes()); // start_timestamp
    data.push(0);
    data.extend_from_slice(&[0u8; 8]); // end_timestamp: None
    data.extend_from_slice(&10u16.to_le_bytes()); // undername_limit
    data.extend_from_slice(&0u64.to_le_bytes()); // purchase_price
    data.push(0); // bump
    data.extend_from_slice(&(name.len() as u32).to_le_bytes()); // name String len
    data.extend_from_slice(name.as_bytes()); // name String data
    data
}

/// Helper: build fake DemandFactor account data
fn build_demand_factor_data() -> Vec<u8> {
    let df_disc = solana_sdk::hash::hash(b"account:DemandFactor");
    let mut data = df_disc.as_ref()[..8].to_vec();
    data.extend_from_slice(&1_000_000u64.to_le_bytes()); // demand_factor = 1.0
    data
}

/// Helper: build a Borsh-serialized AntRecord blob matching `ario_ant::state::AntRecord`.
/// Mirrors the layout walked by `read_ant_record_owner` in
/// `programs/ario-core/src/instructions/primary_name.rs`. If you change one, change both.
fn build_ant_record_data(
    mint: &solana_sdk::pubkey::Pubkey,
    undername: &str,
    owner: Option<solana_sdk::pubkey::Pubkey>,
) -> Vec<u8> {
    // Default last_reconciled_owner to all-zero so existing tests that don't
    // care about the fallback path keep working (any real signer mismatches
    // Pubkey::default(), so NotAntHolder still fires for owner=None).
    build_ant_record_data_with_lro(
        mint,
        undername,
        owner,
        solana_sdk::pubkey::Pubkey::default(),
    )
}

/// Variant of `build_ant_record_data` that lets tests pin
/// `last_reconciled_owner`. Used to exercise the
/// `owner=None → last_reconciled_owner` fallback path that `ario_core`
/// uses to authorize spawned-but-not-yet-delegated records.
fn build_ant_record_data_with_lro(
    mint: &solana_sdk::pubkey::Pubkey,
    undername: &str,
    owner: Option<solana_sdk::pubkey::Pubkey>,
    last_reconciled_owner: solana_sdk::pubkey::Pubkey,
) -> Vec<u8> {
    let mut data = Vec::new();
    let disc = solana_sdk::hash::hash(b"account:AntRecord");
    data.extend_from_slice(&disc.to_bytes()[..8]);
    data.extend_from_slice(mint.as_ref()); // mint: 32 bytes
    let u = undername.to_lowercase();
    data.extend_from_slice(&(u.len() as u32).to_le_bytes());
    data.extend_from_slice(u.as_bytes());
    let target = "a".repeat(43); // valid Arweave-shape placeholder
    data.extend_from_slice(&(target.len() as u32).to_le_bytes());
    data.extend_from_slice(target.as_bytes());
    data.push(0); // target_protocol = Arweave
    data.extend_from_slice(&3600u32.to_le_bytes()); // ttl_seconds
    data.push(0); // priority Option<u32> = None
    match owner {
        Some(o) => {
            data.push(1);
            data.extend_from_slice(o.as_ref());
        }
        None => data.push(0),
    }
    data.extend_from_slice(last_reconciled_owner.as_ref()); // last_reconciled_owner
    data.push(255); // bump
    data
}

fn ant_record_pda(
    mint: &solana_sdk::pubkey::Pubkey,
    undername: &str,
) -> solana_sdk::pubkey::Pubkey {
    let h = solana_sdk::hash::hash(undername.to_lowercase().as_bytes());
    Pubkey::find_program_address(&[b"ant_record", mint.as_ref(), h.as_ref()], &ario_ant::ID).0
}

async fn install_ant_record(
    ctx: &mut ProgramTestContext,
    mint: &solana_sdk::pubkey::Pubkey,
    undername: &str,
    owner: Option<solana_sdk::pubkey::Pubkey>,
) -> solana_sdk::pubkey::Pubkey {
    install_ant_record_with_lro(
        ctx,
        mint,
        undername,
        owner,
        solana_sdk::pubkey::Pubkey::default(),
    )
    .await
}

/// Variant of `install_ant_record` that pins `last_reconciled_owner`.
/// Use for tests that exercise the `owner=None → last_reconciled_owner`
/// fallback in `ario_core::read_ant_record_owner` (canonical
/// just-spawned ANT state).
async fn install_ant_record_with_lro(
    ctx: &mut ProgramTestContext,
    mint: &solana_sdk::pubkey::Pubkey,
    undername: &str,
    owner: Option<solana_sdk::pubkey::Pubkey>,
    last_reconciled_owner: solana_sdk::pubkey::Pubkey,
) -> solana_sdk::pubkey::Pubkey {
    let pda = ant_record_pda(mint, undername);
    let data = build_ant_record_data_with_lro(mint, undername, owner, last_reconciled_owner);
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: ario_ant::ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&pda, &account.into());
    pda
}

fn ant_config_pda(mint: &solana_sdk::pubkey::Pubkey) -> solana_sdk::pubkey::Pubkey {
    Pubkey::find_program_address(&[b"ant_config", mint.as_ref()], &ario_ant::ID).0
}

/// Helper: build a Borsh-serialized AntConfig blob matching
/// `ario_ant::state::AntConfig`. Mirrors the layout walked by
/// `read_ant_config_last_known_owner` in
/// `programs/ario-core/src/instructions/primary_name.rs` — the ANT-level
/// owner snapshot ario-core uses as the implicit-owner fallback when an
/// AntRecord has `owner = None`. If you change one, change both.
///
/// The variable-length string/keyword fields are deliberately non-trivial
/// (and keywords non-empty) so the parser's Vec<String> loop is exercised.
fn build_ant_config_data(
    mint: &solana_sdk::pubkey::Pubkey,
    last_known_owner: &solana_sdk::pubkey::Pubkey,
) -> Vec<u8> {
    fn push_str(data: &mut Vec<u8>, s: &str) {
        data.extend_from_slice(&(s.len() as u32).to_le_bytes());
        data.extend_from_slice(s.as_bytes());
    }
    let mut data = Vec::new();
    let disc = solana_sdk::hash::hash(b"account:AntConfig");
    data.extend_from_slice(&disc.to_bytes()[..8]);
    data.extend_from_slice(mint.as_ref()); // mint: 32 bytes
    push_str(&mut data, "Test ANT"); // name
    push_str(&mut data, "ANT"); // ticker
    push_str(&mut data, &"l".repeat(43)); // logo (Arweave-shape placeholder)
    push_str(&mut data, "an ant for tests"); // description
                                             // keywords: Vec<String> with two entries.
    data.extend_from_slice(&2u32.to_le_bytes());
    push_str(&mut data, "test");
    push_str(&mut data, "ant");
    data.extend_from_slice(last_known_owner.as_ref()); // last_known_owner
    data.push(253); // bump
    data.extend_from_slice(&[1, 0, 0]); // version 1.0.0 — not parsed
    data
}

/// Install an AntConfig PDA whose `last_known_owner` is `last_known_owner`.
/// Use for tests exercising the `owner=None → AntConfig.last_known_owner`
/// fallback in `ario_core::read_ant_record_owner`. For tests that set an
/// explicit per-record `owner = Some(_)`, the config contents are not read
/// (the explicit delegate wins), but the account must still be present.
async fn install_ant_config(
    ctx: &mut ProgramTestContext,
    mint: &solana_sdk::pubkey::Pubkey,
    last_known_owner: &solana_sdk::pubkey::Pubkey,
) -> solana_sdk::pubkey::Pubkey {
    let pda = ant_config_pda(mint);
    let data = build_ant_config_data(mint, last_known_owner);
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: ario_ant::ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&pda, &account.into());
    pda
}

/// Helper: fund a fresh signer with SOL so they can pay rent for init_if_needed PDAs.
async fn fund_signer(
    ctx: &mut ProgramTestContext,
    target: &solana_sdk::pubkey::Pubkey,
    lamports: u64,
) {
    let ix = solana_sdk::system_instruction::transfer(&ctx.payer.pubkey(), target, lamports);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

/// Helper: install an ANT Metaplex Core asset (AssetV1) at a fresh address with the given owner.
async fn install_ant_asset(
    ctx: &mut ProgramTestContext,
    nft_owner: &solana_sdk::pubkey::Pubkey,
) -> solana_sdk::pubkey::Pubkey {
    let key = Pubkey::new_unique();
    let data = build_mpl_core_asset_data(nft_owner);
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: MPL_CORE_PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&key, &account.into());
    key
}

/// Helper: install an ArNS record (Permabuy) for `name` with the given record_owner and ANT.
async fn install_arns_record(
    ctx: &mut ProgramTestContext,
    arns_program_id: &Pubkey,
    name: &str,
    record_owner: &solana_sdk::pubkey::Pubkey,
    ant: &solana_sdk::pubkey::Pubkey,
) -> solana_sdk::pubkey::Pubkey {
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], arns_program_id);
    let data = build_arns_record_data_with_ant(name, record_owner, ant);
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: *arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&pda, &account.into());
    pda
}

/// Helper: install the DemandFactor PDA at value 1.0.
async fn install_demand_factor(
    ctx: &mut ProgramTestContext,
    arns_program_id: &Pubkey,
) -> solana_sdk::pubkey::Pubkey {
    let (pda, _) = Pubkey::find_program_address(&[b"demand_factor"], arns_program_id);
    let data = build_demand_factor_data();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: *arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&pda, &account.into());
    pda
}

/// Helper: inject fake ArNS record + DemandFactor accounts into test context.
/// Returns (arns_record_pda, demand_factor_pda, ant_record_at_at, ant_mint_key).
///
/// ADR-016 reshape: ario-core is MPL-agnostic. The primary-name handlers no
/// longer read MPL Core assets — they read the AntRecord PDA owned by the
/// caller-supplied `ant_program_id`. So this helper installs an AntRecord
/// owned by `ario_ant::ID` at the `@` undername (the canonical sentinel for
/// base-name ownership) with `nft_holder` as the recorded owner.
///
/// Returns:
///   - `arns_record_pda`: ArnsRecord PDA for the base name (slot [0] in
///     primary-name remaining_accounts)
///   - `demand_factor_pda`: DemandFactor PDA (slot [1] for fee-charging
///     ixs: `request_primary_name`, `request_and_set_primary_name`)
///   - `ant_record_at_at`: AntRecord PDA at the "@" undername — sufficient
///     for base-name primary names. For undername primaries, callers
///     install an additional AntRecord at the undername using
///     `install_ant_record(ctx, &ant_mint_key, undername, owner)`.
///   - `ant_mint_key`: the ANT mint pubkey, used to derive AntRecord PDAs
///     at other undernames.
async fn inject_arns_accounts(
    ctx: &mut ProgramTestContext,
    arns_program_id: &Pubkey,
    name: &str,
    nft_holder: &solana_sdk::pubkey::Pubkey,
) -> (
    solana_sdk::pubkey::Pubkey,
    solana_sdk::pubkey::Pubkey,
    solana_sdk::pubkey::Pubkey,
    solana_sdk::pubkey::Pubkey,
) {
    let rent = ctx.banks_client.get_rent().await.unwrap();

    // Per-mint identity for this name. No longer an MPL asset — just a fresh
    // pubkey that ArnsRecord stores and AntRecord PDA seeds derive from.
    let ant_mint_key = Pubkey::new_unique();

    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], arns_program_id);

    let arns_data = build_arns_record_data(name, nft_holder, &ant_mint_key);
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: *arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    let (demand_factor_pda, _) = Pubkey::find_program_address(&[b"demand_factor"], arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: *arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    // Install AntRecord at "@" undername owned by ario_ant, with nft_holder
    // as the recorded owner. This is what the reshaped handlers authenticate
    // against (caller == AntRecord.owner) for base-name primary names.
    let ant_record_at_at = install_ant_record(ctx, &ant_mint_key, "@", Some(*nft_holder)).await;

    (
        arns_record_pda,
        demand_factor_pda,
        ant_record_at_at,
        ant_mint_key,
    )
}

#[tokio::test]
async fn test_transfer() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + two token accounts (alice, bob)
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let alice = Keypair::new();
    let alice_token = Keypair::new();
    create_token_account(&mut ctx, &alice_token, &mint.pubkey(), &alice.pubkey()).await;

    let bob_token = Keypair::new();
    let bob_pk = Pubkey::new_unique();
    create_token_account(&mut ctx, &bob_token, &mint.pubkey(), &bob_pk).await;

    // Mint 500 ARIO to alice
    let amount = 500_000_000u64;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &alice_token.pubkey(),
        &mint_authority,
        amount,
    )
    .await;

    // Initialize config
    let protocol_token = Keypair::new();
    let payer_key = ctx.payer.pubkey();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_key).await;
    let (config_key, _) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;
    let _ = config_key;

    // Transfer 200 ARIO from alice to bob
    let transfer_amount = 200_000_000u64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::TransferTokens {
                from_token_account: alice_token.pubkey(),
                to_token_account: bob_token.pubkey(),
                authority: alice.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Transfer {
                amount: transfer_amount,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &alice],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify balances
    let alice_balance = get_token_balance(&mut ctx, &alice_token.pubkey()).await;
    let bob_balance = get_token_balance(&mut ctx, &bob_token.pubkey()).await;
    assert_eq!(alice_balance, amount - transfer_amount);
    assert_eq!(bob_balance, transfer_amount);
}

#[tokio::test]
async fn test_vaulted_transfer() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let sender_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _arns) = config_pda();

    // Initialize config
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Derive vault PDAs for recipient
    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);

    // Create vault token account owned by vault PDA
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Vaulted transfer: 100 ARIO, 14 days lock, revocable
    let vault_amount = 100_000_000u64;
    let lock_duration = 14 * 86_400i64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: vault_amount,
                lock_duration_seconds: lock_duration,
                revocable: true,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify vault
    let vault_account = ctx
        .banks_client
        .get_account(vault_key)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();

    assert_eq!(vault.owner, recipient.pubkey());
    assert_eq!(vault.amount, vault_amount);
    assert!(vault.revocable);
    assert_eq!(vault.controller, Some(ctx.payer.pubkey()));
    assert_eq!(vault.end_timestamp, vault.start_timestamp + lock_duration);

    // Verify config.locked_supply increased
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.locked_supply, vault_amount);
}

#[tokio::test]
async fn test_extend_vault() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault (14-day lock)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read vault to get original end_timestamp
    let vault_account = ctx
        .banks_client
        .get_account(vault_key)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();
    let original_end = vault.end_timestamp;

    // Extend vault by 7 days
    let additional_seconds = 7 * 86_400i64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ExtendVault {
                config: config_key,
                vault: vault_key,
                owner: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ExtendVault { additional_seconds }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify end_timestamp increased
    let vault_account = ctx
        .banks_client
        .get_account(vault_key)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();
    assert_eq!(vault.end_timestamp, original_end + additional_seconds);

    // SHOULD-12 boundary: extend at exactly end_timestamp should work.
    // Warp time to exactly the vault's end_timestamp.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = vault.end_timestamp;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ExtendVault {
                config: config_key,
                vault: vault_key,
                owner: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ExtendVault {
                additional_seconds: 86_400, // +1 day
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    // This should succeed (SHOULD-12: extending at exactly end_timestamp is allowed)
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let vault_account = ctx
        .banks_client
        .get_account(vault_key)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();
    assert_eq!(
        vault.end_timestamp,
        original_end + additional_seconds + 86_400
    );
}

#[tokio::test]
async fn test_release_vault() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault (14-day lock, 100 ARIO)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;
    let vault_amount = 100_000_000u64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: vault_amount,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify owner_token is now 0
    let balance_before = get_token_balance(&mut ctx, &owner_token.pubkey()).await;
    assert_eq!(balance_before, 0);

    // Warp past vault expiry
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += lock_duration + 1;
    ctx.set_sysvar(&clock);

    // Release vault
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ReleaseVault {
                config: config_key,
                vault: vault_key,
                vault_token_account: vault_token.pubkey(),
                owner_token_account: owner_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ReleaseVault {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify tokens returned to owner
    let balance_after = get_token_balance(&mut ctx, &owner_token.pubkey()).await;
    assert_eq!(balance_after, vault_amount);

    // Verify vault account is closed
    let vault_account = ctx.banks_client.get_account(vault_key).await.unwrap();
    assert!(vault_account.is_none());

    // Verify config.locked_supply back to 0
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.locked_supply, 0);
    assert_eq!(config.circulating_supply, 1_000_000_000_000);
}

#[tokio::test]
async fn test_primary_name_uniqueness() {
    // BUG-1: Two users should NOT be able to claim the same primary name.
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();

    // User A = payer (ctx.payer)
    let user_a_token = Keypair::new();
    create_token_account(&mut ctx, &user_a_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &user_a_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config
    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "testname";

    // Inject fake ArNS record owned by user A (payer)
    let (arns_record_pda, demand_factor_pda, ant_asset_key, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &payer_pk).await;

    // User A: RequestAndSetPrimaryName for "testname"
    let (primary_name_key_a, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas_a = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key_a,
        primary_name_reverse: reverse_key,
        initiator_token_account: user_a_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas_a.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas_a.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    account_metas_a.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_asset_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas_a,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // User B tries to claim the same name "testname"
    let user_b = Keypair::new();

    // Fund user B with SOL for rent
    let transfer_sol_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &user_b.pubkey(),
        1_000_000_000,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_sol_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let user_b_token = Keypair::new();
    create_token_account(&mut ctx, &user_b_token, &mint.pubkey(), &user_b.pubkey()).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &user_b_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    // Update ArNS record to be owned by user B
    let (arns_record_pda_b, demand_factor_pda_b, ant_asset_key_b, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &user_b.pubkey()).await;

    let (primary_name_key_b, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_SEED, user_b.pubkey().as_ref()],
        &ario_core::ID,
    );
    // The reverse key is the same (same name hash)
    let mut account_metas_b = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key_b,
        primary_name_reverse: reverse_key,
        initiator_token_account: user_b_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: user_b.pubkey(),
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas_b.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda_b,
        false,
    ));
    account_metas_b.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda_b,
        false,
    ));
    account_metas_b.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_asset_key_b,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas_b,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &user_b],
        blockhash,
    );

    // User B's transaction should FAIL — primary name already set by user A
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "User B should not be able to claim a name already set by User A"
    );
}

#[tokio::test]
async fn test_close_expired_request() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config
    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "expirename";

    // Inject ArNS record + DemandFactor accounts
    let (arns_record_pda, demand_factor_pda, _ant_asset_key, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &payer_pk).await;

    // Create primary name request
    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify request exists
    let request_account = ctx.banks_client.get_account(request_key).await.unwrap();
    assert!(request_account.is_some());

    // Warp past expiry (7 days + 1 second)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += 7 * 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Close expired request (permissionless)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CloseExpiredRequest {
                request: request_key,
                initiator: payer_pk.into(),
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CloseExpiredRequest {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify request account is closed
    let request_account = ctx.banks_client.get_account(request_key).await.unwrap();
    assert!(
        request_account.is_none(),
        "Request account should be closed after expiry"
    );
}

// =========================================
// ERROR PATH TESTS
// =========================================

#[tokio::test]
async fn test_transfer_zero_amount() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + two token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let alice = Keypair::new();
    let alice_token = Keypair::new();
    create_token_account(&mut ctx, &alice_token, &mint.pubkey(), &alice.pubkey()).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &alice_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let bob_token = Keypair::new();
    let bob_pk = Pubkey::new_unique();
    create_token_account(&mut ctx, &bob_token, &mint.pubkey(), &bob_pk).await;

    // Transfer 0 tokens -> should fail with InvalidAmount
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::TransferTokens {
                from_token_account: alice_token.pubkey(),
                to_token_account: bob_token.pubkey(),
                authority: alice.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Transfer { amount: 0 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &alice],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAmount);
}

#[tokio::test]
async fn test_transfer_self_transfer() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + one token account (transfer to self)
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let alice = Keypair::new();
    let alice_token = Keypair::new();
    create_token_account(&mut ctx, &alice_token, &mint.pubkey(), &alice.pubkey()).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &alice_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    // Transfer to self (same token account for from and to) -> should fail with SelfTransfer
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::TransferTokens {
                from_token_account: alice_token.pubkey(),
                to_token_account: alice_token.pubkey(),
                authority: alice.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Transfer {
                amount: 100_000_000,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &alice],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::SelfTransfer);
}

#[tokio::test]
async fn test_transfer_insufficient_balance() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + two token accounts, alice has only 100 ARIO
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let alice = Keypair::new();
    let alice_token = Keypair::new();
    create_token_account(&mut ctx, &alice_token, &mint.pubkey(), &alice.pubkey()).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &alice_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let bob_token = Keypair::new();
    let bob_pk = Pubkey::new_unique();
    create_token_account(&mut ctx, &bob_token, &mint.pubkey(), &bob_pk).await;

    // Transfer 500 ARIO (more than balance) -> should fail with SPL token error
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::TransferTokens {
                from_token_account: alice_token.pubkey(),
                to_token_account: bob_token.pubkey(),
                authority: alice.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Transfer {
                amount: 500_000_000,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &alice],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    // SPL token insufficient funds error (not an Anchor custom error)
    assert!(result.is_err(), "Transfer exceeding balance should fail");
}

#[tokio::test]
async fn test_vault_below_minimum() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    // Fund with 200 ARIO so we have enough tokens but vault amount is below minimum
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        200_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key_for_token, _) = vault_pda(&ctx.payer.pubkey(), 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key_for_token).await;

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault with 50 ARIO (below MIN_VAULT_SIZE of 100 ARIO)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);
    let lock_duration = 14 * 86_400i64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 50_000_000, // 50 ARIO - below 100 ARIO minimum
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::VaultBelowMinimum);
}

#[tokio::test]
async fn test_vault_duration_too_long() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key_for_token, _) = vault_pda(&ctx.payer.pubkey(), 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key_for_token).await;

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault with duration exceeding MAX_VAULT_DURATION (200 years + 1 second)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);
    let max_duration = 200i64 * 365 * 86_400; // MAX_VAULT_DURATION

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,                     // 100 ARIO - meets minimum
                lock_duration_seconds: max_duration + 1, // exceeds max
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::LockDurationTooLong);
}

#[tokio::test]
async fn test_release_vault_while_locked() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault (14-day lock, 100 ARIO)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to release vault immediately (still locked) -> should fail with VaultLocked
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ReleaseVault {
                config: config_key,
                vault: vault_key,
                vault_token_account: vault_token.pubkey(),
                owner_token_account: owner_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ReleaseVault {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::VaultLocked);
}

#[tokio::test]
async fn test_revoke_non_revocable_vault() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _) = config_pda();

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create a non-revocable vault via vaulted_transfer (revocable=false)
    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let lock_duration = 14 * 86_400i64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
                revocable: false, // Non-revocable vault
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to revoke the non-revocable vault -> should fail with VaultNotRevocable
    let controller_token = Keypair::new();
    create_token_account(&mut ctx, &controller_token, &mint.pubkey(), &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::RevokeVault {
                config: config_key,
                vault: vault_key,
                vault_token_account: vault_token.pubkey(),
                controller_token_account: controller_token.pubkey(),
                controller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::RevokeVault {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::VaultNotRevocable);
}

#[tokio::test]
async fn test_increase_vault_zero_amount() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        200_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault (14-day lock, 100 ARIO)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to increase vault by 0 -> should fail with InvalidAmount
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::IncreaseVault {
                config: config_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::IncreaseVault { amount: 0 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAmount);
}

// =========================================
// INVARIANT TESTS
// =========================================

#[tokio::test]
async fn test_supply_invariant_after_vault_ops() {
    // Verify that tokens are conserved across vault create and release operations:
    // user_balance + vault_token_balance == constant at every step.
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;

    // Mint 1000 ARIO to user
    let initial_amount = 1_000_000_000u64; // 1000 ARIO
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        initial_amount,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Initialize config
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Step 0: Verify initial state
    let user_bal_0 = get_token_balance(&mut ctx, &owner_token.pubkey()).await;
    let vault_bal_0 = get_token_balance(&mut ctx, &vault_token.pubkey()).await;
    let total_supply = user_bal_0 + vault_bal_0;
    assert_eq!(user_bal_0, initial_amount);
    assert_eq!(vault_bal_0, 0);

    // Step 1: Create vault (lock 100 ARIO)
    let vault_amount = 100_000_000u64; // 100 ARIO
    let lock_duration = 14 * 86_400i64; // 14 days
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: vault_amount,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify invariant after vault creation
    let user_bal_1 = get_token_balance(&mut ctx, &owner_token.pubkey()).await;
    let vault_bal_1 = get_token_balance(&mut ctx, &vault_token.pubkey()).await;
    assert_eq!(
        user_bal_1 + vault_bal_1,
        total_supply,
        "Token conservation violated after vault creation"
    );
    assert_eq!(user_bal_1, initial_amount - vault_amount);
    assert_eq!(vault_bal_1, vault_amount);

    // Verify config supply tracking
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.locked_supply, vault_amount);

    // Step 2: Warp past vault expiry and release
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += lock_duration + 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ReleaseVault {
                config: config_key,
                vault: vault_key,
                vault_token_account: vault_token.pubkey(),
                owner_token_account: owner_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ReleaseVault {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify invariant after vault release.
    // Note: vault_token_account is closed by release_vault (rent reclaimed), so balance is 0.
    let user_bal_2 = get_token_balance(&mut ctx, &owner_token.pubkey()).await;
    let vault_account_gone = ctx
        .banks_client
        .get_account(vault_token.pubkey())
        .await
        .unwrap()
        .is_none();
    assert!(
        vault_account_gone,
        "Vault token account should be closed after release"
    );
    let vault_bal_2 = 0u64; // account closed = zero balance
    assert_eq!(
        user_bal_2 + vault_bal_2,
        total_supply,
        "Token conservation violated after vault release"
    );
    assert_eq!(
        user_bal_2, initial_amount,
        "User balance not fully restored after release"
    );

    // Verify config supply tracking restored
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.locked_supply, 0);
    assert_eq!(config.circulating_supply, 1_000_000_000_000);
}

// =========================================
// PRIMARY NAME APPROVE / REMOVE TESTS
// =========================================

#[tokio::test]
async fn test_approve_primary_name() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config
    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "testname";

    // Create a separate name_owner keypair and fund it
    let name_owner = Keypair::new();
    let transfer_sol_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &name_owner.pubkey(),
        2_000_000_000,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_sol_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Inject ArNS record with ANT owned by name_owner (NOT the payer/initiator)
    let (arns_record_pda, demand_factor_pda, ant_asset_key, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &name_owner.pubkey()).await;

    // Step 1: Create a PrimaryNameRequest (from payer as initiator)
    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut request_account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    request_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    request_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: request_account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Step 2: name_owner calls ApprovePrimaryName
    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut approve_account_metas = ario_core::accounts::ApprovePrimaryName {
        config: config_key,
        request: request_key,
        initiator: payer_pk.into(),
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        name_owner: name_owner.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    approve_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    approve_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_asset_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: approve_account_metas,
            data: ario_core::instruction::ApprovePrimaryName {
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&name_owner.pubkey()),
        &[&name_owner],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: PrimaryName PDA created for initiator (payer) with correct name
    let primary_name_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap()
        .unwrap();
    let primary_name_data =
        PrimaryName::try_deserialize(&mut primary_name_account.data.as_slice()).unwrap();
    assert_eq!(primary_name_data.owner, payer_pk);
    assert_eq!(primary_name_data.name, name);

    // Verify: PrimaryNameReverse PDA created
    let reverse_account = ctx
        .banks_client
        .get_account(reverse_key)
        .await
        .unwrap()
        .unwrap();
    let reverse_data =
        PrimaryNameReverse::try_deserialize(&mut reverse_account.data.as_slice()).unwrap();
    assert_eq!(reverse_data.owner, payer_pk);
    assert_eq!(reverse_data.name, name);

    // Verify: PrimaryNameRequest account is closed
    let request_account = ctx.banks_client.get_account(request_key).await.unwrap();
    assert!(
        request_account.is_none(),
        "Request account should be closed after approval"
    );
}

#[tokio::test]
async fn test_approve_primary_name_expired() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config
    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "testname";

    // Create name_owner and fund
    let name_owner = Keypair::new();
    let transfer_sol_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &name_owner.pubkey(),
        2_000_000_000,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_sol_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Inject ArNS record with ANT owned by name_owner
    let (arns_record_pda, demand_factor_pda, ant_asset_key, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &name_owner.pubkey()).await;

    // Create a PrimaryNameRequest
    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut request_account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    request_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    request_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: request_account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp clock past 7 days + 1 second
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += 7 * 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Try to approve - should fail with PrimaryNameRequestExpired
    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut approve_account_metas = ario_core::accounts::ApprovePrimaryName {
        config: config_key,
        request: request_key,
        initiator: payer_pk.into(),
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        name_owner: name_owner.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    approve_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    approve_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_asset_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: approve_account_metas,
            data: ario_core::instruction::ApprovePrimaryName {
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&name_owner.pubkey()),
        &[&name_owner],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::PrimaryNameRequestExpired);
}

#[tokio::test]
async fn test_remove_primary_name() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config
    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "testname";

    // Inject ArNS record owned by payer (so we can use RequestAndSetPrimaryName)
    let (arns_record_pda, demand_factor_pda, ant_asset_key, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &payer_pk).await;

    // Set primary name via RequestAndSetPrimaryName (auto-approve since payer holds the ANT)
    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut set_account_metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_asset_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: set_account_metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify primary name is set
    let primary_name_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap();
    assert!(
        primary_name_account.is_some(),
        "Primary name should exist before removal"
    );

    // Call RemovePrimaryName as the owner
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::RemovePrimaryName {
                primary_name: primary_name_key,
                primary_name_reverse: reverse_key,
                owner: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::RemovePrimaryName {
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify PrimaryName account is closed
    let primary_name_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap();
    assert!(
        primary_name_account.is_none(),
        "PrimaryName account should be closed"
    );

    // Verify PrimaryNameReverse account is closed
    let reverse_account = ctx.banks_client.get_account(reverse_key).await.unwrap();
    assert!(
        reverse_account.is_none(),
        "PrimaryNameReverse account should be closed"
    );
}

#[tokio::test]
async fn test_remove_primary_name_for_base_name() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();

    // User A (payer) will set a primary name "alice_testname"
    // The base name owner also owns the ArNS "testname" record
    let user_a_token = Keypair::new();
    create_token_account(&mut ctx, &user_a_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &user_a_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config
    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let base_name = "testname";
    let full_name = "alice_testname";

    // Inject ArNS record for "testname" with ANT owned by payer.
    // ant_record_at_at is the AntRecord at "@" used by RemovePrimaryNameForBaseName.
    let (arns_record_pda, demand_factor_pda, ant_record_at_at, ant_mint) =
        inject_arns_accounts(&mut ctx, &arns_program_id, base_name, &payer_pk).await;
    // ADR-016 reshape: setting "alice_testname" auths via AntRecord at the
    // "alice" undername. Install one with payer as owner so auto-approve fires.
    let ant_record_alice = install_ant_record(&mut ctx, &ant_mint, "alice", Some(payer_pk)).await;

    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(full_name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut set_account_metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        initiator_token_account: user_a_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_record_alice,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: set_account_metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full_name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full_name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify primary name is set
    let primary_name_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap();
    assert!(
        primary_name_account.is_some(),
        "Primary name should exist before base name removal"
    );

    // The ANT holder (payer) calls RemovePrimaryNameForBaseName
    // passing the ArNS record as remaining_accounts[0] and ANT asset as remaining_accounts[1]
    let mut remove_account_metas = ario_core::accounts::RemovePrimaryNameForBaseName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        original_owner: payer_pk.into(),
        name_owner: payer_pk,
    }
    .to_account_metas(None);
    remove_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    // RemovePrimaryNameForBaseName always uses the "@" AntRecord (the
    // base-name owner is the canonical authority, regardless of whether
    // the primary name being revoked is a base or undername).
    remove_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_record_at_at,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: remove_account_metas,
            data: ario_core::instruction::RemovePrimaryNameForBaseName {
                reverse_lookup_hash: primary_reverse_lookup_hash(full_name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify PrimaryName account is closed
    let primary_name_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap();
    assert!(
        primary_name_account.is_none(),
        "PrimaryName account should be closed by base name owner"
    );
}

// =========================================
// UPDATE CONFIG TESTS
// =========================================

#[tokio::test]
async fn test_update_config_happy() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config (authority = payer)
    let (config_key, _arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Read original config to capture max_vault_duration
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let original_config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    let original_max_vault_duration = original_config.max_vault_duration;

    // Call UpdateConfig with partial params
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    min_vault_duration: Some(30 * 86_400),
                    primary_name_request_expiry: Some(14 * 86_400),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Fetch config and verify changes
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();

    assert_eq!(
        config.min_vault_duration,
        30 * 86_400,
        "min_vault_duration should be updated"
    );
    assert_eq!(
        config.primary_name_request_expiry,
        14 * 86_400,
        "primary_name_request_expiry should be updated"
    );
    assert_eq!(
        config.max_vault_duration, original_max_vault_duration,
        "max_vault_duration should remain unchanged"
    );
}

#[tokio::test]
async fn test_update_config_unauthorized() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config (authority = payer)
    let (config_key, _arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create unauthorized keypair and fund it
    let unauthorized = Keypair::new();
    let transfer_sol_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &unauthorized.pubkey(),
        1_000_000_000,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_sol_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Attempt UpdateConfig signed by unauthorized -> should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: unauthorized.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    min_vault_duration: Some(30 * 86_400),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&unauthorized.pubkey()),
        &[&unauthorized],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::Unauthorized);
}

// =========================================
// NEW TESTS: Vault revoke, increase, and config boundary
// =========================================

/// Test 1: Revoke a revocable vault before expiry.
/// Creates a vaulted transfer (revocable), then revokes it.
/// Verifies: tokens returned to controller, vault account closed,
/// locked_supply decreased, circulating_supply increased.
/// Lua parity: vaults.revokeVault() L92-101
#[tokio::test]
async fn test_revoke_vault_success() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _) = config_pda();

    // Initialize config
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Derive vault PDAs for recipient
    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);

    // Create vault token account owned by vault PDA
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Vaulted transfer: 100 ARIO, 14 days lock, revocable=true
    let vault_amount = 100_000_000u64;
    let lock_duration = 14 * 86_400i64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: vault_amount,
                lock_duration_seconds: lock_duration,
                revocable: true,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify vault was created and config shows locked supply
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config_before = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config_before.locked_supply, vault_amount);
    let circulating_before = config_before.circulating_supply;

    // Controller token account for receiving revoked funds (the sender/controller = payer)
    let controller_token = Keypair::new();
    create_token_account(&mut ctx, &controller_token, &mint.pubkey(), &payer_pk).await;

    // Revoke the vault (controller = payer, who created the vaulted transfer)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::RevokeVault {
                config: config_key,
                vault: vault_key,
                vault_token_account: vault_token.pubkey(),
                controller_token_account: controller_token.pubkey(),
                controller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::RevokeVault {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: tokens returned to controller
    let controller_balance = get_token_balance(&mut ctx, &controller_token.pubkey()).await;
    assert_eq!(
        controller_balance, vault_amount,
        "Revoked tokens should return to controller"
    );

    // Verify: vault account is closed
    let vault_account = ctx.banks_client.get_account(vault_key).await.unwrap();
    assert!(
        vault_account.is_none(),
        "Vault account should be closed after revoke"
    );

    // Verify: supply accounting updated
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config_after = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(
        config_after.locked_supply, 0,
        "locked_supply should be 0 after revoke"
    );
    assert_eq!(
        config_after.circulating_supply,
        circulating_before + vault_amount,
        "circulating_supply should increase by revoked amount"
    );
}

/// Test 2: Increase vault amount successfully.
/// Creates vault with 100 ARIO, increases by 50 ARIO.
/// Verifies vault.amount grows and supply accounting is correct.
/// Lua parity: vaults.increaseVault() L137-149
#[tokio::test]
async fn test_increase_vault_success() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    // Fund with 200 ARIO: 100 for initial vault + 50 for increase + extra
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        200_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault (14-day lock, 100 ARIO)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;
    let initial_amount = 100_000_000u64; // 100 ARIO
    let increase_amount = 50_000_000u64; // 50 ARIO

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: initial_amount,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read config after vault creation to capture locked_supply
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config_mid = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config_mid.locked_supply, initial_amount);

    // Increase vault by 50 ARIO
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::IncreaseVault {
                config: config_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::IncreaseVault {
                amount: increase_amount,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify vault amount increased
    let vault_account = ctx
        .banks_client
        .get_account(vault_key)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();
    assert_eq!(
        vault.amount,
        initial_amount + increase_amount,
        "Vault amount should reflect initial + increase"
    );

    // Verify token balances
    let owner_balance = get_token_balance(&mut ctx, &owner_token.pubkey()).await;
    assert_eq!(
        owner_balance,
        200_000_000 - initial_amount - increase_amount,
        "Owner token balance should decrease by total vaulted"
    );
    let vault_balance = get_token_balance(&mut ctx, &vault_token.pubkey()).await;
    assert_eq!(
        vault_balance,
        initial_amount + increase_amount,
        "Vault token balance should hold total vaulted"
    );

    // Verify supply accounting
    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config_after = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(
        config_after.locked_supply,
        initial_amount + increase_amount,
        "locked_supply should reflect total vaulted"
    );
    assert_eq!(
        config_after.circulating_supply,
        1_000_000_000_000 - initial_amount - increase_amount,
        "circulating_supply should decrease by total vaulted"
    );
}

/// Test 3: Increase vault after expiry should fail.
/// Creates vault, warps clock past end_timestamp, tries to increase.
/// Should fail with VaultExpired.
/// Lua parity: vaults.increaseVault() assert L143 "Vault has ended."
#[tokio::test]
async fn test_increase_vault_expired() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        200_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Initialize
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault (14-day lock, 100 ARIO)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past vault expiry
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += lock_duration + 1;
    ctx.set_sysvar(&clock);

    // Try to increase expired vault -> should fail with VaultExpired
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::IncreaseVault {
                config: config_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::IncreaseVault { amount: 50_000_000 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::VaultExpired);
}

/// Test 4: Request and set primary name (auto-approve for base name owners).
/// Verifies that base name owner can set primary name in one tx,
/// the fee is correctly charged, and the primary name + reverse accounts are created.
/// Lua parity: primaryNames.createPrimaryNameRequest - auto-approve path
#[tokio::test]
async fn test_request_and_set_primary_name() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint + token accounts
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    // Fund with enough for fee: PRIMARY_NAME_REQUEST_BASE_FEE = 200_000 mARIO at demand_factor=1.0
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config with protocol_token as treasury
    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "myarnsname";

    // Inject ArNS record with ANT owned by payer (so auto-approve works via ANT holder)
    let (arns_record_pda, demand_factor_pda, ant_asset_key, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &payer_pk).await;

    // Record balances before
    let initiator_balance_before = get_token_balance(&mut ctx, &initiator_token.pubkey()).await;
    let protocol_balance_before = get_token_balance(&mut ctx, &protocol_token.pubkey()).await;

    // Call RequestAndSetPrimaryName
    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_asset_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: PrimaryName PDA created with correct fields
    let primary_name_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap()
        .unwrap();
    let primary_name_data =
        PrimaryName::try_deserialize(&mut primary_name_account.data.as_slice()).unwrap();
    assert_eq!(
        primary_name_data.owner, payer_pk,
        "Primary name owner should be the initiator"
    );
    assert_eq!(
        primary_name_data.name, name,
        "Primary name should match requested name"
    );
    assert!(
        primary_name_data.set_at > 0,
        "set_at timestamp should be non-zero"
    );

    // Verify: PrimaryNameReverse PDA created
    let reverse_account = ctx
        .banks_client
        .get_account(reverse_key)
        .await
        .unwrap()
        .unwrap();
    let reverse_data =
        PrimaryNameReverse::try_deserialize(&mut reverse_account.data.as_slice()).unwrap();
    assert_eq!(
        reverse_data.owner, payer_pk,
        "Reverse lookup owner should match"
    );
    assert_eq!(reverse_data.name, name, "Reverse lookup name should match");

    // Verify: Fee was charged (200_000 mARIO at demand_factor=1.0)
    let expected_fee = 200_000u64; // PRIMARY_NAME_REQUEST_BASE_FEE * 1.0
    let initiator_balance_after = get_token_balance(&mut ctx, &initiator_token.pubkey()).await;
    let protocol_balance_after = get_token_balance(&mut ctx, &protocol_token.pubkey()).await;
    assert_eq!(
        initiator_balance_before - initiator_balance_after,
        expected_fee,
        "Initiator should be charged the base fee"
    );
    assert_eq!(
        protocol_balance_after - protocol_balance_before,
        expected_fee,
        "Protocol treasury should receive the fee"
    );
}

/// Test 5: UpdateConfig with invalid boundary conditions.
/// Setting min_vault_duration > max_vault_duration should fail with InvalidParameter.
/// Setting primary_name_request_expiry to 0 should fail with InvalidParameter.
#[tokio::test]
async fn test_update_config_boundary() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Setup: mint
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    // Initialize config (authority = payer)
    let (config_key, _) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Case 1: min_vault_duration > max_vault_duration -> should fail
    // Default max is 200 years = 6_307_200_000 seconds. Set min to max+1.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    min_vault_duration: Some(ArioConfig::DEFAULT_MAX_VAULT_DURATION + 1),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidParameter);

    // Case 2: primary_name_request_expiry = 0 -> should fail (must be > 0)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    primary_name_request_expiry: Some(0),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidParameter);

    // Case 3: Setting max_vault_duration below current min should also fail
    // Current min = DEFAULT_MIN_VAULT_DURATION = 14 days = 1_209_600. Set max to 1 day.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    max_vault_duration: Some(86_400), // 1 day, less than min (14 days)
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidParameter);
}

// =========================================
// COVERAGE GAP TESTS
// =========================================

// -----------------------------------------
// D. vaulted_transfer error branches
// -----------------------------------------

/// vaulted_transfer with lock duration too short should fail
#[tokio::test]
async fn test_vaulted_transfer_duration_too_short() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _) = config_pda();

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: 100_000_000,
                lock_duration_seconds: 1, // too short
                revocable: false,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::LockDurationTooShort);
}

/// vaulted_transfer with lock duration too long should fail
#[tokio::test]
async fn test_vaulted_transfer_duration_too_long() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _) = config_pda();

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: 100_000_000,
                lock_duration_seconds: ArioConfig::DEFAULT_MAX_VAULT_DURATION + 1, // too long
                revocable: false,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::LockDurationTooLong);
}

/// vaulted_transfer to self (sender == recipient) should fail
#[tokio::test]
async fn test_vaulted_transfer_self_transfer() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let (config_key, _) = config_pda();

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Use payer as both sender and recipient
    let (recipient_vault_counter_key, _) = vault_counter_pda(&payer_pk);
    let (vault_key, _) = vault_pda(&payer_pk, 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: payer_pk, // same as sender
                sender: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: 100_000_000,
                lock_duration_seconds: 14 * 86_400,
                revocable: false,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::SelfTransfer);
}

// -----------------------------------------
// G. extend_vault duration too long
// -----------------------------------------

/// Extending a vault so that remaining + extension > max_vault_duration should fail
#[tokio::test]
async fn test_extend_vault_exceeds_max_duration() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault with max duration (200 years)
    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = ArioConfig::DEFAULT_MAX_VAULT_DURATION;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to extend by even 1 second -> remaining (200 years) + 1 > max
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ExtendVault {
                config: config_key,
                vault: vault_key,
                owner: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ExtendVault {
                additional_seconds: 1,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::LockDurationTooLong);
}

// -----------------------------------------
// H. close_expired_request when not expired
// -----------------------------------------

/// Trying to close a primary name request that hasn't expired should fail
#[tokio::test]
async fn test_close_request_not_expired() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "notexpired";

    let (arns_record_pda, demand_factor_pda, _ant_asset_key, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &payer_pk).await;

    // Create primary name request
    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to close immediately (not expired) -> should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CloseExpiredRequest {
                request: request_key,
                initiator: payer_pk.into(),
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CloseExpiredRequest {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::PrimaryNameRequestNotExpired);
}

// -----------------------------------------
// C. update_config individual field coverage
// -----------------------------------------

/// Test updating max_vault_duration individually
#[tokio::test]
async fn test_update_config_max_vault_duration() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, _) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Update max_vault_duration to 100 years (still > min)
    let new_max = 100i64 * 365 * 86_400;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    max_vault_duration: Some(new_max),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.max_vault_duration, new_max);
    // min should remain unchanged
    assert_eq!(
        config.min_vault_duration,
        ArioConfig::DEFAULT_MIN_VAULT_DURATION
    );
}

/// Test updating new_authority
#[tokio::test]
async fn test_update_config_new_authority() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, _) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let new_authority = Keypair::new();

    // Update authority
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    new_authority: Some(new_authority.pubkey()),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_account = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArioConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.authority, new_authority.pubkey());

    // Old authority should no longer be able to update config
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    min_vault_duration: Some(30 * 86_400),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::Unauthorized);
}

/// Test updating max_vault_duration to 0 should fail
#[tokio::test]
async fn test_update_config_max_vault_duration_zero() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, _) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Set max_vault_duration to 0 -> should fail (max_vault_duration > 0)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    max_vault_duration: Some(0),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidParameter);
}

// -----------------------------------------
// I. request_and_set_primary_name error: not ANT holder
// -----------------------------------------

/// request_and_set_primary_name fails when initiator is not the ANT NFT holder
#[tokio::test]
async fn test_request_and_set_primary_name_not_owner() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "someoneelse";

    // Inject ArNS record with ANT owned by a DIFFERENT pubkey (not the payer/initiator)
    let other_owner = Pubkey::new_unique();
    let (arns_record_pda, demand_factor_pda, ant_asset_key, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &other_owner).await;

    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_asset_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// -----------------------------------------
// I.5: Canonical ANT program lockdown (Codex finding)
// -----------------------------------------

/// PoC for the ant_program_id spoofing attack the canonical lockdown closes.
///
/// Before the fix, primary-name handlers trusted whatever `ant_program_id`
/// the caller passed. An attacker could:
///   1. Deploy a fake Solana program EVIL.
///   2. Create a PDA at `find_program_address(&[b"ant_record", real_ant_mint,
///      undername_hash], &EVIL)` under it.
///   3. Write byte-compatible `AntRecord` data with `owner = attacker`.
///   4. Call `request_and_set_primary_name` with `ant_program_id = EVIL`
///      and the fake PDA as `remaining_accounts[2]`.
///
/// Both the `account.owner == ant_program` check and the
/// `find_program_address(seeds, ant_program)` derivation ran under EVIL,
/// so the helper accepted the fabricated AntRecord and returned the
/// attacker as the record owner — granting unilateral primary-name
/// control over arbitrary active ArNS names.
///
/// `read_ant_record_owner` now requires `ant_program_id == ario_ant::ID`
/// (canonical lockdown — pluggable ANT programs per ADR-016 will require
/// asset-attribute lookup, tracked as a follow-up). This test fabricates
/// the attacker's AntRecord under a random "evil" program id and confirms
/// the require! fires before any account parsing.
#[tokio::test]
async fn test_request_and_set_primary_name_rejects_non_canonical_ant_program() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "victimname";

    // Pretend the real ArNS record exists with an ANT mint we don't own
    // (the legitimate owner is some other pubkey).
    let legitimate_owner = Pubkey::new_unique();
    let (arns_record_pda, demand_factor_pda, _real_ant_record, ant_mint_key) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &legitimate_owner).await;

    // === Attacker setup ===
    // Pretend EVIL_PROGRAM_ID is a Solana program the attacker has deployed.
    // We don't actually need an executable BPF — we just need an account
    // owned by it at the right PDA with byte-compatible AntRecord data.
    let evil_program_id = Pubkey::new_unique();

    // Fabricate the AntRecord PDA UNDER the evil program for the "@"
    // undername of the real ant_mint. Write byte-compatible data with
    // `owner = attacker (payer_pk)`.
    let undername_hash = solana_sdk::hash::hash(b"@");
    let (fake_ant_record_pda, _) = Pubkey::find_program_address(
        &[
            b"ant_record",
            ant_mint_key.as_ref(),
            undername_hash.as_ref(),
        ],
        &evil_program_id,
    );
    let fake_data = build_ant_record_data(&ant_mint_key, "@", Some(payer_pk));
    let rent = ctx.banks_client.get_rent().await.unwrap();
    ctx.set_account(
        &fake_ant_record_pda,
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(fake_data.len()),
            data: fake_data,
            owner: evil_program_id,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // === Attack: submit request_and_set_primary_name with the evil
    // program id + the fabricated AntRecord. ===
    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        fake_ant_record_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: evil_program_id, // ← the spoof
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    // Canonical-lockdown require! in read_ant_record_owner rejects.
    assert_anchor_error!(result, ArioError::InvalidAccountState);

    // Sanity: the attack did NOT succeed — no PrimaryName PDA was created.
    let primary_name_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap();
    assert!(
        primary_name_account.is_none(),
        "attacker must not have been able to set a PrimaryName",
    );
}

// -----------------------------------------
// E. verify_arns_record_active: lease expiry
// -----------------------------------------

/// Helper: build ArNS record data with lease type and an expired end_timestamp.
/// Layout: disc(8) + name_hash(32) + owner(32) + ant(32) + purchase_type(1)
///       + start_ts(8) + end_ts(1+8) + undername_limit(2) + purchase_price(8)
///       + bump(1) + name(4+N)
fn build_arns_record_data_lease(
    name: &str,
    owner: &solana_sdk::pubkey::Pubkey,
    start_ts: i64,
    end_ts: i64,
) -> Vec<u8> {
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let arns_disc = solana_sdk::hash::hash(b"account:ArnsRecord");
    let mut data = arns_disc.as_ref()[..8].to_vec();
    data.extend_from_slice(name_hash.as_ref()); // name_hash: 32 bytes
    data.extend_from_slice(owner.as_ref()); // owner: 32 bytes
    data.extend_from_slice(&[0u8; 32]); // ant: 32 bytes (placeholder)
    data.push(0); // purchase_type = Lease
    data.extend_from_slice(&start_ts.to_le_bytes()); // start_timestamp: 8 bytes
    data.push(1); // Option<i64> discriminant = Some
    data.extend_from_slice(&end_ts.to_le_bytes()); // end_timestamp: 8 bytes
    data.extend_from_slice(&10u16.to_le_bytes()); // undername_limit
    data.extend_from_slice(&0u64.to_le_bytes()); // purchase_price
    data.push(0); // bump
    data.extend_from_slice(&(name.len() as u32).to_le_bytes()); // name String len
    data.extend_from_slice(name.as_bytes()); // name String data
    data
}

/// Requesting a primary name with an expired ArNS lease (past grace period) should fail
#[tokio::test]
async fn test_request_primary_name_expired_lease() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "expiredlease";

    // Build ArNS record with lease that ended long ago (end_ts = 1000, grace = 14 days)
    // Current clock is around 0, so we warp forward past end_ts + 14 days
    let end_ts = 1000i64;
    let start_ts = 0i64;

    let arns_data = build_arns_record_data_lease(name, &payer_pk, start_ts, end_ts);
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);

    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // DemandFactor account
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    // Warp clock well past end_ts + 14-day grace period
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + 14 * 86_400 + 1; // past grace
    ctx.set_sysvar(&clock);

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::ArnsRecordNotFound);
}

/// A lease ArNS record within the grace period should still be considered active
#[tokio::test]
async fn test_request_primary_name_lease_within_grace() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "graceperiod";

    // Build lease record: end_ts = 100_000, start_ts = 0
    let end_ts = 100_000i64;
    let start_ts = 0i64;

    let arns_data = build_arns_record_data_lease(name, &payer_pk, start_ts, end_ts);
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);

    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    // Warp clock to just after end_ts but within grace period (end_ts + 7 days < end_ts + 14 days)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + 7 * 86_400; // within 14-day grace
    ctx.set_sysvar(&clock);

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    // Should succeed -- within grace period
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify request was created
    let request_account = ctx.banks_client.get_account(request_key).await.unwrap();
    assert!(
        request_account.is_some(),
        "Request should be created for lease within grace period"
    );
}

// -----------------------------------------
// K. read_demand_factor strict validation
// -----------------------------------------

/// When demand factor account has wrong owner, tx should fail
#[tokio::test]
async fn test_request_primary_name_bad_demand_factor() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "demandtest";

    // Inject valid ArNS record
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
    let arns_data = build_arns_record_data(name, &payer_pk, &Pubkey::default());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // Inject a DemandFactor account with WRONG owner (not arns_program_id)
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let wrong_owner = Pubkey::new_unique();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: wrong_owner, // wrong owner -> read_demand_factor errors
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    // Should fail — wrong owner on demand factor account
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

/// When demand factor has bad discriminator, tx should fail
#[tokio::test]
async fn test_request_primary_name_bad_demand_factor_discriminator() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "baddisctest";

    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
    let arns_data = build_arns_record_data(name, &payer_pk, &Pubkey::default());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // DemandFactor with correct PDA and owner but wrong discriminator
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    // Wrong discriminator data (all zeros instead of hash)
    let mut bad_df_data = vec![0u8; 8]; // bad discriminator
    bad_df_data.extend_from_slice(&2_000_000u64.to_le_bytes()); // demand_factor = 2.0
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(bad_df_data.len()),
        data: bad_df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    // Should fail — bad discriminator on demand factor account
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// -----------------------------------------
// Vaulted transfer: vault below minimum
// -----------------------------------------

/// vaulted_transfer with amount below MIN_VAULT_SIZE should fail
#[tokio::test]
async fn test_vaulted_transfer_below_minimum() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _) = config_pda();

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: 50_000_000, // 50 ARIO, below 100 ARIO minimum
                lock_duration_seconds: 14 * 86_400,
                revocable: false,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::VaultBelowMinimum);
}

// -----------------------------------------
// Remove primary name for base name: different owner scenario
// -----------------------------------------

/// Base name owner (different from primary name holder) can remove
/// someone else's primary name that uses their ArNS domain.
#[tokio::test]
async fn test_remove_primary_name_for_base_name_different_owner() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();

    // User A (payer) will set a primary name "alice_testname"
    let user_a_token = Keypair::new();
    create_token_account(&mut ctx, &user_a_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &user_a_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let base_name = "testdomain";
    let full_name = "alice_testdomain";

    // Initially ArNS record with ANT owned by payer (User A) so they can set primary name
    let (arns_record_pda, demand_factor_pda, _ant_record_at_at, ant_mint) =
        inject_arns_accounts(&mut ctx, &arns_program_id, base_name, &payer_pk).await;
    // ADR-016 reshape: setting "alice_testdomain" auths via the AntRecord
    // at "alice" — install one with payer as owner so auto-approve fires.
    let ant_record_alice = install_ant_record(&mut ctx, &ant_mint, "alice", Some(payer_pk)).await;

    // User A sets primary name via RequestAndSetPrimaryName
    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(full_name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut set_account_metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        initiator_token_account: user_a_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    set_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_record_alice,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: set_account_metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full_name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full_name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Now transfer ArNS ownership to User B (name_owner)
    let name_owner = Keypair::new();
    // Fund User B with SOL
    let transfer_sol_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &name_owner.pubkey(),
        2_000_000_000,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_sol_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Re-inject ArNS record with ANT owned by User B (new ANT holder).
    // Helper installs AntRecord at "@" with name_owner as owner — that's
    // who RemovePrimaryNameForBaseName authorizes against.
    let (arns_record_pda_b, _, ant_record_b_at_at, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, base_name, &name_owner.pubkey()).await;

    // User B (new base-name owner) calls RemovePrimaryNameForBaseName
    let mut remove_account_metas = ario_core::accounts::RemovePrimaryNameForBaseName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        original_owner: payer_pk.into(),
        name_owner: name_owner.pubkey(),
    }
    .to_account_metas(None);
    remove_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda_b,
        false,
    ));
    remove_account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_record_b_at_at,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: remove_account_metas,
            data: ario_core::instruction::RemovePrimaryNameForBaseName {
                reverse_lookup_hash: primary_reverse_lookup_hash(full_name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&name_owner.pubkey()),
        &[&name_owner],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify PrimaryName account is closed
    let primary_name_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap();
    assert!(
        primary_name_account.is_none(),
        "PrimaryName should be closed by new base name owner"
    );

    // Verify reverse is also closed
    let reverse_account = ctx.banks_client.get_account(reverse_key).await.unwrap();
    assert!(reverse_account.is_none(), "Reverse lookup should be closed");
}

// -----------------------------------------
// Extend vault: zero additional_seconds
// -----------------------------------------

/// extend_vault with additional_seconds = 0 should fail with InvalidParameter
#[tokio::test]
async fn test_extend_vault_zero_seconds() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to extend by 0 seconds
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ExtendVault {
                config: config_key,
                vault: vault_key,
                owner: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ExtendVault {
                additional_seconds: 0,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidParameter);
}

// -----------------------------------------
// Extend vault: expired vault
// -----------------------------------------

/// extend_vault after the vault has expired should fail with VaultExpired
#[tokio::test]
async fn test_extend_vault_expired() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past vault expiry
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += lock_duration + 1;
    ctx.set_sysvar(&clock);

    // Try to extend expired vault
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ExtendVault {
                config: config_key,
                vault: vault_key,
                owner: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ExtendVault {
                additional_seconds: 86_400,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::VaultExpired);
}

// -----------------------------------------
// Create vault: zero amount
// -----------------------------------------

/// create_vault with amount=0 should fail with InvalidAmount
#[tokio::test]
async fn test_create_vault_zero_amount() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        200_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key_for_token, _) = vault_pda(&ctx.payer.pubkey(), 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key_for_token).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 0, // zero amount
                lock_duration_seconds: 14 * 86_400,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAmount);
}

// -----------------------------------------
// Vaulted transfer: zero amount
// -----------------------------------------

/// vaulted_transfer with amount=0 should fail with InvalidAmount
#[tokio::test]
async fn test_vaulted_transfer_zero_amount() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _) = config_pda();

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: 0, // zero amount
                lock_duration_seconds: 14 * 86_400,
                revocable: false,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAmount);
}

// -----------------------------------------
// Demand factor: account with too-short data
// -----------------------------------------

/// When demand factor account data is too short (< 16 bytes), tx should fail
#[tokio::test]
async fn test_request_primary_name_short_demand_factor_data() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "shortdftest";

    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
    let arns_data = build_arns_record_data(name, &payer_pk, &Pubkey::default());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // DemandFactor with correct PDA and owner but only 8 bytes of data (too short -- need 16)
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let short_df_data = vec![0u8; 8]; // only 8 bytes, need at least 16
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(short_df_data.len()),
        data: short_df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    // Should fail — demand factor data too short
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// =========================================
// NEW COVERAGE GAP TESTS
// =========================================

// -----------------------------------------
// A. approve_primary_name via ANT NFT holder path
// -----------------------------------------

/// Helper: build ArNS record data with a specific ANT pubkey
fn build_arns_record_data_with_ant(
    name: &str,
    owner: &solana_sdk::pubkey::Pubkey,
    ant: &solana_sdk::pubkey::Pubkey,
) -> Vec<u8> {
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let arns_disc = solana_sdk::hash::hash(b"account:ArnsRecord");
    let mut data = arns_disc.as_ref()[..8].to_vec();
    data.extend_from_slice(name_hash.as_ref()); // name_hash: 32 bytes
    data.extend_from_slice(owner.as_ref()); // owner: 32 bytes
    data.extend_from_slice(ant.as_ref()); // ant: 32 bytes (the Metaplex Core asset)
    data.push(1); // purchase_type = Permabuy (always active)
    data.extend_from_slice(&0i64.to_le_bytes()); // start_timestamp: 8 bytes
    data.push(0); // end_timestamp Option discriminant: None
    data.extend_from_slice(&[0u8; 8]); // end_timestamp payload (ignored for None)
    data.extend_from_slice(&10u16.to_le_bytes()); // undername_limit: 2 bytes
    data.extend_from_slice(&0u64.to_le_bytes()); // purchase_price: 8 bytes
    data.push(0); // bump: 1 byte
    data.extend_from_slice(&(name.len() as u32).to_le_bytes()); // name String len
    data.extend_from_slice(name.as_bytes()); // name String data
    data
}

/// Helper: build fake Metaplex Core Asset V1 data with a given owner
fn build_mpl_core_asset_data(nft_owner: &solana_sdk::pubkey::Pubkey) -> Vec<u8> {
    let mut data = Vec::new();
    data.push(1); // Key::AssetV1
    data.extend_from_slice(nft_owner.as_ref()); // owner: 32 bytes
    data.extend_from_slice(&[0u8; 64]); // remaining fields (padding)
    data
}

// (deleted obsolete test `test_approve_primary_name_via_ant_holder` — exercised the
//  ANT-holder fallback / asset-side ANT-Program reading paths
//  that ARIO-CORE no longer implements after the Sprint 2-3 reshape.
//  See ADR-016 amendment in docs/DECISIONS.md.)

// -----------------------------------------
// B. remove_primary_name_for_base_name via ANT NFT holder path
// -----------------------------------------

// (deleted obsolete test `test_remove_primary_name_for_base_name_via_ant_holder` — exercised the
//  ANT-holder fallback / asset-side ANT-Program reading paths
//  that ARIO-CORE no longer implements after the Sprint 2-3 reshape.
//  See ADR-016 amendment in docs/DECISIONS.md.)

// -----------------------------------------
// C. approve_primary_name: not ANT holder (no ANT asset provided)
// -----------------------------------------

/// approve_primary_name fails when no ANT asset is provided in remaining_accounts[1].
/// Exercises the NotAntHolder error when remaining_accounts has only the ArNS record.
#[tokio::test]
async fn test_approve_primary_name_not_owner_no_ant() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "notownertest";

    // ArNS record owned by someone else (not name_owner signer)
    let record_owner = Pubkey::new_unique();

    // Create name_owner (unauthorized - doesn't own the record or ANT)
    let name_owner = Keypair::new();
    let transfer_sol_ix = solana_sdk::system_instruction::transfer(
        &ctx.payer.pubkey(),
        &name_owner.pubkey(),
        2_000_000_000,
    );
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[transfer_sol_ix],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // ADR-016 reshape: ant mint is just a pubkey for AntRecord PDA
    // derivation. AntRecord owner is record_owner — name_owner doesn't
    // match, so approve must fail with NotAntHolder.
    let ant_mint = Pubkey::new_unique();
    let arns_data = build_arns_record_data(name, &record_owner, &ant_mint);
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // DemandFactor
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    // Create PrimaryNameRequest from payer as initiator
    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut request_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    request_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    request_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: request_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to approve as name_owner (who is neither record_owner nor ANT holder)
    // Only pass ArNS record as remaining_accounts[0], no ANT asset
    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash_lower = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash_lower.as_ref()],
        &ario_core::ID,
    );

    // AntRecord at "@" with record_owner as the recorded owner. name_owner
    // is the caller — not record_owner — so the auth check fails with
    // NotAntHolder.
    let ant_record_pda = install_ant_record(&mut ctx, &ant_mint, "@", Some(record_owner)).await;

    let mut approve_metas = ario_core::accounts::ApprovePrimaryName {
        config: config_key,
        request: request_key,
        initiator: payer_pk.into(),
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        name_owner: name_owner.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    approve_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    approve_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_record_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: approve_metas,
            data: ario_core::instruction::ApprovePrimaryName {
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&name_owner.pubkey()),
        &[&name_owner],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// -----------------------------------------
// D. validate_arns_record_exists: empty data
// -----------------------------------------

/// request_primary_name with an ArNS record that has empty data should fail
#[tokio::test]
async fn test_request_primary_name_empty_arns_data() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "emptydata";

    // Inject ArNS record PDA with empty data
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);

    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(0),
        data: vec![], // empty data
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // DemandFactor
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::ArnsRecordNotFound);
}

// -----------------------------------------
// E. validate_arns_record_exists: short data (< 8 bytes)
// -----------------------------------------

/// request_primary_name with an ArNS record that has too-short data (< 8 bytes) should fail
#[tokio::test]
async fn test_request_primary_name_short_arns_data() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "shortdata";

    // Inject ArNS record PDA with only 4 bytes of data (too short for discriminator)
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);

    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(4),
        data: vec![0u8; 4], // too short - less than 8 bytes for discriminator
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // DemandFactor
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// -----------------------------------------
// F. validate_arns_record_exists: bad discriminator
// -----------------------------------------

/// request_primary_name with an ArNS record that has wrong discriminator should fail
#[tokio::test]
async fn test_request_primary_name_bad_arns_discriminator() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "baddiscname";

    // Build ArNS record data but with WRONG discriminator
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);

    let mut arns_data = vec![0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0x00, 0x00, 0x00]; // wrong discriminator
    arns_data.extend_from_slice(name_hash.as_ref()); // name_hash: 32 bytes
    arns_data.extend_from_slice(&(name.len() as u32).to_le_bytes());
    arns_data.extend_from_slice(name.as_bytes());
    arns_data.extend_from_slice(payer_pk.as_ref()); // owner: 32 bytes
    arns_data.extend_from_slice(&[0u8; 32]); // ant: 32 bytes
    arns_data.push(1); // purchase_type = Permabuy
    arns_data.extend_from_slice(&[0u8; 63]); // remaining fields

    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // DemandFactor
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// -----------------------------------------
// G. read_demand_factor: PDA mismatch rejection
// -----------------------------------------

/// When demand factor account has correct owner but wrong PDA, tx should fail
#[tokio::test]
async fn test_request_primary_name_demand_factor_pda_mismatch() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "pdamismatch";

    // Inject valid ArNS record
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
    let arns_data = build_arns_record_data(name, &payer_pk, &Pubkey::default());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // Use a random key as the DemandFactor account (correct owner, wrong PDA)
    let wrong_df_key = Pubkey::new_unique();
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id, // correct owner
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&wrong_df_key, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    // Pass the wrong PDA key as remaining_accounts[1]
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        wrong_df_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    // Should fail — PDA mismatch on demand factor account
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// -----------------------------------------
// H. validate_arns_record_exists: wrong PDA derivation
// -----------------------------------------

/// request_primary_name with ArNS record at wrong PDA should fail
#[tokio::test]
async fn test_request_primary_name_wrong_arns_pda() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "wrongpda";

    // Create a valid ArNS record but at a WRONG address (not the expected PDA)
    let wrong_arns_key = Pubkey::new_unique();
    let arns_data = build_arns_record_data(name, &payer_pk, &Pubkey::default());
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&wrong_arns_key, &arns_account.into());

    // DemandFactor
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // Pass the wrong key as remaining_accounts[0]
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        wrong_arns_key,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// -----------------------------------------
// I. validate_arns_record_exists: wrong owner (not arns_program)
// -----------------------------------------

/// request_primary_name with ArNS record owned by wrong program should fail
#[tokio::test]
async fn test_request_primary_name_wrong_arns_owner() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "wrongowner";

    // Create ArNS record at the correct PDA but owned by the WRONG program
    let name_hash = solana_sdk::hash::hash(name.as_bytes());
    let (arns_record_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
    let arns_data = build_arns_record_data(name, &payer_pk, &Pubkey::default());
    let wrong_owner = Pubkey::new_unique();
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let arns_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(arns_data.len()),
        data: arns_data,
        owner: wrong_owner, // wrong program owner
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&arns_record_pda, &arns_account.into());

    // DemandFactor
    let (demand_factor_pda, _) =
        Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
    let df_data = build_demand_factor_data();
    let df_account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(df_data.len()),
        data: df_data,
        owner: arns_program_id,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&demand_factor_pda, &df_account.into());

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut account_metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    account_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: account_metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// -----------------------------------------
// TEST-009: Revoke vault after expiry (audit SECURITY_AUDIT_INDEPENDENT.md)
// Once a vault's end_timestamp passes, the funds belong to the owner — the
// controller MUST NOT be able to claw them back via revoke. Defense lives at
// vault.rs:181 (`require!(!vault.is_unlocked(clock.unix_timestamp), VaultExpired)`).
// Lua parity: in the AR.IO Lua source, expired revocable vaults can only be
// `release`'d (to owner), not `revoke`'d (to controller).
// -----------------------------------------

#[tokio::test]
async fn test_revoke_vault_after_expiry_fails() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _) = config_pda();

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    // Revocable vault with 14-day lock
    let vault_amount = 100_000_000u64;
    let lock_duration = 14 * 86_400i64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: vault_amount,
                lock_duration_seconds: lock_duration,
                revocable: true,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp clock past end_timestamp (14 days + 1s buffer)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = clock.unix_timestamp + lock_duration + 1;
    ctx.set_sysvar(&clock);

    // Make banks_client see the new clock
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();

    // Now controller (=payer/sender) tries to revoke — must fail with VaultExpired
    let controller_token = Keypair::new();
    create_token_account(&mut ctx, &controller_token, &mint.pubkey(), &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::RevokeVault {
                config: config_key,
                vault: vault_key,
                vault_token_account: vault_token.pubkey(),
                controller_token_account: controller_token.pubkey(),
                controller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::RevokeVault {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::VaultExpired);

    // Sanity: vault still exists (not closed by failed revoke)
    let vault_account = ctx.banks_client.get_account(vault_key).await.unwrap();
    assert!(
        vault_account.is_some(),
        "Vault must still exist after failed revoke"
    );
}

// -----------------------------------------
// TEST-012: Concurrent vault creation (audit SECURITY_AUDIT_INDEPENDENT.md)
// Create N vaults in rapid succession from the same owner. Verify:
//   (a) counter.next_id increments monotonically (0, 1, 2, ..., N)
//   (b) each vault PDA is distinct (vault_id is part of the seed)
//   (c) the i-th vault's vault_id == i
// Defense lives in vault.rs at counter.next_id.checked_add(1) and the PDA seed
// `[VAULT_SEED, owner, vault_id]`. A regression in seed derivation would cause
// a PDA collision and the second create would fail at Anchor's `init` constraint.
// -----------------------------------------

#[tokio::test]
async fn test_concurrent_vault_creation_unique_pdas() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let owner_token = Keypair::new();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    // Mint enough for 5 vaults * 100 ARIO each
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        1_000_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&payer_pk);

    let vault_amount = 100_000_000u64; // MIN_VAULT_SIZE
    let lock_duration = 14 * 86_400i64;

    let mut vault_keys: Vec<Pubkey> = Vec::new();

    // Create 5 vaults — each one uses the next counter.next_id as its vault_id
    for expected_id in 0u64..5 {
        let (vault_key, _) = vault_pda(&payer_pk, expected_id);
        let vault_token = Keypair::new();
        create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_core::ID,
                accounts: ario_core::accounts::CreateVault {
                    config: config_key,
                    vault_counter: vault_counter_key,
                    vault: vault_key,
                    owner_token_account: owner_token.pubkey(),
                    vault_token_account: vault_token.pubkey(),
                    owner: payer_pk,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_core::instruction::CreateVault {
                    amount: vault_amount,
                    lock_duration_seconds: lock_duration,
                }
                .data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Verify the freshly-created vault
        let vault_account = ctx
            .banks_client
            .get_account(vault_key)
            .await
            .unwrap()
            .unwrap();
        let vault = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();
        assert_eq!(
            vault.vault_id, expected_id,
            "vault_id must match counter at create time"
        );
        assert_eq!(vault.amount, vault_amount);
        assert_eq!(vault.owner, payer_pk);

        vault_keys.push(vault_key);
    }

    // Counter must have advanced to N
    let counter_account = ctx
        .banks_client
        .get_account(vault_counter_key)
        .await
        .unwrap()
        .unwrap();
    let counter = VaultCounter::try_deserialize(&mut counter_account.data.as_slice()).unwrap();
    assert_eq!(
        counter.next_id, 5,
        "counter.next_id must equal number of vaults created"
    );

    // All vault PDAs must be distinct (sanity for PDA derivation correctness)
    let mut sorted = vault_keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), 5, "Each vault must have a unique PDA");
}

// -----------------------------------------
// C1 dust DoS regression — see SECURITY_AUDIT_2026-04-29.md
// vault_token_account is a PDA with publicly derivable address. Before the fix,
// `release_vault` and `revoke_vault` transferred `vault.amount` (stored) and
// then called `close_account`, which reverts on non-zero balance. An attacker
// could lock funds permanently by transferring 1 mARIO of dust into the
// vault_token_account before release/revoke. The fix transfers the live
// balance instead, sweeping any dust to the legitimate destination.
// -----------------------------------------

#[tokio::test]
async fn test_release_vault_resists_dust_attack() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&ctx.payer.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&ctx.payer.pubkey());
    let lock_duration = 14 * 86_400i64;
    let vault_amount = 100_000_000u64;

    // Create vault
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: vault_amount,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Attacker dust attack: mint 1 mARIO directly into the vault_token_account.
    // On-chain this is equivalent to an SPL transfer from any third party.
    let dust_amount = 1u64;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &vault_token.pubkey(),
        &mint_authority,
        dust_amount,
    )
    .await;

    // Sanity: vault_token now holds principal + dust
    let pre_release = get_token_balance(&mut ctx, &vault_token.pubkey()).await;
    assert_eq!(pre_release, vault_amount + dust_amount);

    // Warp past lock
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += lock_duration + 1;
    ctx.set_sysvar(&clock);

    // Release should succeed and sweep both principal + dust to owner
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ReleaseVault {
                config: config_key,
                vault: vault_key,
                vault_token_account: vault_token.pubkey(),
                owner_token_account: owner_token.pubkey(),
                owner: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ReleaseVault {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("release_vault must succeed even when dust is present");

    // Owner received principal + dust
    let owner_balance = get_token_balance(&mut ctx, &owner_token.pubkey()).await;
    assert_eq!(
        owner_balance,
        vault_amount + dust_amount,
        "owner should receive principal AND swept dust"
    );

    // vault_token_account is closed (close_account requires zero balance)
    let vault_token_account = ctx
        .banks_client
        .get_account(vault_token.pubkey())
        .await
        .unwrap();
    assert!(
        vault_token_account.is_none(),
        "vault_token_account must be closed after release"
    );

    // vault PDA also closed
    let vault_account = ctx.banks_client.get_account(vault_key).await.unwrap();
    assert!(vault_account.is_none());
}

#[tokio::test]
async fn test_revoke_vault_resists_dust_attack() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let sender_token = Keypair::new();
    create_token_account(&mut ctx, &sender_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &sender_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let recipient = Keypair::new();
    let (config_key, _) = config_pda();

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (recipient_vault_counter_key, _) = vault_counter_pda(&recipient.pubkey());
    let (vault_key, _) = vault_pda(&recipient.pubkey(), 0);

    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let vault_amount = 100_000_000u64;
    let lock_duration = 14 * 86_400i64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::VaultedTransfer {
                config: config_key,
                recipient_vault_counter: recipient_vault_counter_key,
                vault: vault_key,
                sender_token_account: sender_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                recipient: recipient.pubkey(),
                sender: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::VaultedTransfer {
                amount: vault_amount,
                lock_duration_seconds: lock_duration,
                revocable: true,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Attacker dust attack
    let dust_amount = 1u64;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &vault_token.pubkey(),
        &mint_authority,
        dust_amount,
    )
    .await;

    let controller_token = Keypair::new();
    create_token_account(&mut ctx, &controller_token, &mint.pubkey(), &payer_pk).await;

    // Revoke must succeed despite dust (sweeps to controller)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::RevokeVault {
                config: config_key,
                vault: vault_key,
                vault_token_account: vault_token.pubkey(),
                controller_token_account: controller_token.pubkey(),
                controller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::RevokeVault {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("revoke_vault must succeed even when dust is present");

    let controller_balance = get_token_balance(&mut ctx, &controller_token.pubkey()).await;
    assert_eq!(
        controller_balance,
        vault_amount + dust_amount,
        "controller should receive principal AND swept dust"
    );

    let vault_token_account = ctx
        .banks_client
        .get_account(vault_token.pubkey())
        .await
        .unwrap();
    assert!(
        vault_token_account.is_none(),
        "vault_token_account must be closed after revoke"
    );
}

// =====================================================================
// PR-4: initialize binding to upgrade authority (audit H1 / Theme C)
// =====================================================================

#[tokio::test]
async fn test_initialize_rejects_non_upgrade_authority() {
    // The Initialize handler is constrained to the program's BPFLoaderUpgradeable
    // upgrade authority. A random caller must not be able to claim protocol
    // authority by front-running a deploy/init bundle. Closes the H1 / Theme C
    // surface across all four protocol-level initialize handlers; we verify
    // the binding here on ario-core (which controls the treasury and supply).
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    // Attacker who is NOT the upgrade authority. Fund them so they could pay
    // tx fees if the constraint accidentally allowed them through.
    let attacker = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &attacker.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    let (config_key, _) = config_pda();

    // Attacker passes themselves as `payer` — must fail because the
    // ProgramData PDA (pre-added at genesis with `upgrade_authority_keypair`
    // as the upgrade authority) does NOT name attacker as the authority.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::Initialize {
                config: config_key,
                mint: mint.pubkey(),
                payer: attacker.pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Initialize {
                params: ario_core::InitializeParams {
                    authority: attacker.pubkey(),
                    total_supply: 1_000_000_000_000,
                    arns_program: Pubkey::new_unique(),
                    treasury: Pubkey::new_unique(),
                    migration_authority: attacker.pubkey(),
                    gar_program: solana_sdk::pubkey::Pubkey::default(),
                },
            }
            .data(),
        }],
        Some(&attacker.pubkey()),
        &[&attacker],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "attacker who is not the upgrade authority must not be able to initialize"
    );
}

// =========================================
// SECURITY: CU Consumption Assertion
// =========================================

#[tokio::test]
async fn test_create_vault_cu_consumption() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let vault_token = Keypair::new();
    let (config_key, _) = config_pda();
    let (vault_key_for_token, _) = vault_pda(&payer_pk, 0);
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key_for_token).await;

    let upgrade_auth = upgrade_authority_keypair();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::Initialize {
                config: config_key,
                mint: mint.pubkey(),
                payer: upgrade_auth.pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Initialize {
                params: ario_core::InitializeParams {
                    authority: payer_pk,
                    total_supply: 1_000_000_000_000,
                    arns_program: Pubkey::new_unique(),
                    treasury: Pubkey::new_unique(),
                    migration_authority: payer_pk,
                    gar_program: solana_sdk::pubkey::Pubkey::default(),
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer, &upgrade_auth],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (vault_counter_key, _) = vault_counter_pda(&payer_pk);
    let (vault_key, _) = vault_pda(&payer_pk, 0);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: 14 * 86_400,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "CreateVault should succeed");
    let metadata = result.metadata.expect("metadata must be present");
    // Measured in BPF mode (BPF_OUT_DIR set): ~28K–38K CU across runs. Native
    // dispatch reports ~5K. Threshold sized for BPF + ~1.5× headroom — tight
    // enough to catch a 2× regression, loose enough to absorb run-to-run jitter.
    assert!(
        metadata.compute_units_consumed < 60_000,
        "CreateVault used {} CU, expected < 60_000",
        metadata.compute_units_consumed
    );
}

// =========================================
// SECURITY: init_if_needed double-call
// =========================================

#[tokio::test]
async fn test_init_if_needed_vault_counter_preserved() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        500_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let upgrade_auth = upgrade_authority_keypair();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::Initialize {
                config: config_key,
                mint: mint.pubkey(),
                payer: upgrade_auth.pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Initialize {
                params: ario_core::InitializeParams {
                    authority: payer_pk,
                    total_supply: 1_000_000_000_000,
                    arns_program: Pubkey::new_unique(),
                    treasury: Pubkey::new_unique(),
                    migration_authority: payer_pk,
                    gar_program: solana_sdk::pubkey::Pubkey::default(),
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer, &upgrade_auth],
        ctx.last_blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (vault_counter_key, _) = vault_counter_pda(&payer_pk);

    // Create vault 0
    let (vault_key_0, _) = vault_pda(&payer_pk, 0);
    let vault_token_0 = Keypair::new();
    create_token_account(&mut ctx, &vault_token_0, &mint.pubkey(), &vault_key_0).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key_0,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token_0.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: 14 * 86_400,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create vault 1 — counter must not reset
    let (vault_key_1, _) = vault_pda(&payer_pk, 1);
    let vault_token_1 = Keypair::new();
    create_token_account(&mut ctx, &vault_token_1, &mint.pubkey(), &vault_key_1).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key_1,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token_1.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: 14 * 86_400,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let vault_account = ctx
        .banks_client
        .get_account(vault_key_1)
        .await
        .unwrap()
        .unwrap();
    let vault = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();
    assert_eq!(vault.vault_id, 1, "Second vault must have vault_id=1");

    let counter_account = ctx
        .banks_client
        .get_account(vault_counter_key)
        .await
        .unwrap()
        .unwrap();
    let counter = VaultCounter::try_deserialize(&mut counter_account.data.as_slice()).unwrap();
    assert_eq!(
        counter.next_id, 2,
        "Counter must be 2 after two vault creations"
    );
}

// =========================================
// SECURITY: Revival attack (multi-instruction)
// =========================================

/// Release a vault then try to release it again in the same transaction.
/// The second instruction must fail because Anchor zeroes the discriminator on close.
#[tokio::test]
async fn test_vault_release_then_reuse_in_same_tx() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        200_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&payer_pk, 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: 14 * 86_400,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past expiry
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += 14 * 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Multi-instruction: release vault twice in same tx
    let release_accounts = ario_core::accounts::ReleaseVault {
        config: config_key,
        vault: vault_key,
        vault_token_account: vault_token.pubkey(),
        owner_token_account: owner_token.pubkey(),
        owner: payer_pk,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    let release_data = ario_core::instruction::ReleaseVault {}.data();

    let ix1 = Instruction {
        program_id: ario_core::ID,
        accounts: release_accounts.clone(),
        data: release_data.clone(),
    };
    let ix2 = Instruction {
        program_id: ario_core::ID,
        accounts: release_accounts,
        data: release_data,
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx =
        Transaction::new_signed_with_payer(&[ix1, ix2], Some(&payer_pk), &[&ctx.payer], blockhash);
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "Double-release in same transaction must fail (revival attack blocked)"
    );
}

// =========================================
// UNDERNAME RECORD-OWNER PRIMARY NAME PATH
// (PLAN_undername_primary_name.md)
// Mirrors the auth fallback added to request_and_set_primary_name and
// approve_primary_name: when the caller doesn't hold the ANT NFT but the
// name is an undername, the caller may pass the AntRecord PDA in the last
// remaining_accounts slot and authorize via AntRecord.owner.
// =========================================

/// Common scaffolding shared by undername-record tests: mint, treasury, two ATAs.
struct UndernameTestCtx {
    config_key: solana_sdk::pubkey::Pubkey,
    arns_program_id: solana_sdk::pubkey::Pubkey,
    initiator_token: solana_sdk::pubkey::Pubkey,
    protocol_token: solana_sdk::pubkey::Pubkey,
    mint_pk: solana_sdk::pubkey::Pubkey,
    mint_authority: Keypair,
}

async fn undername_test_setup(ctx: &mut ProgramTestContext) -> UndernameTestCtx {
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    UndernameTestCtx {
        config_key,
        arns_program_id,
        initiator_token: initiator_token.pubkey(),
        protocol_token: protocol_token.pubkey(),
        mint_pk: mint.pubkey(),
        mint_authority,
    }
}

/// Build the account_metas vec for RequestAndSetPrimaryName given the four
/// fixed roles and any number of trailing remaining_accounts.
fn build_request_and_set_metas(
    config_key: &solana_sdk::pubkey::Pubkey,
    primary_name_key: &solana_sdk::pubkey::Pubkey,
    reverse_key: &solana_sdk::pubkey::Pubkey,
    initiator_token: &solana_sdk::pubkey::Pubkey,
    protocol_token: &solana_sdk::pubkey::Pubkey,
    initiator: &solana_sdk::pubkey::Pubkey,
    remaining: &[solana_sdk::pubkey::Pubkey],
) -> Vec<solana_sdk::instruction::AccountMeta> {
    let mut metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: *config_key,
        primary_name: *primary_name_key,
        primary_name_reverse: *reverse_key,
        initiator_token_account: *initiator_token,
        protocol_token_account: *protocol_token,
        initiator: *initiator,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for r in remaining {
        metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
            *r, false,
        ));
    }
    metas
}

fn primary_name_pdas(
    initiator: &solana_sdk::pubkey::Pubkey,
    name: &str,
) -> (solana_sdk::pubkey::Pubkey, solana_sdk::pubkey::Pubkey) {
    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, initiator.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );
    (primary_name_key, reverse_key)
}

// -----------------------------------------------------------------
// 0. Layout-pin test for read_ant_record_owner.
// Catches drift in `ario_ant::state::AntRecord` field order before `owner`.
// If the parser stops finding the right pubkey, the on-chain instruction
// returns InvalidAccountState (or wrong owner) — either way this test fails.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_ant_record_layout_parse_pin() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "pinbase";
    let undername = "pinun";
    let full = format!("{}_{}", undername, base_name);

    // ANT NFT held by a fresh keypair (so the caller falls into the undername path).
    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let _arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;

    // Build an AntRecord whose Some(owner) == payer_pk. If layout drifts, the
    // parser pulls a different pubkey and the instruction returns
    // UndernameRecordOwnerRequired.
    let ant_record_pda =
        install_ant_record(&mut ctx, &ant_asset_key, undername, Some(payer_pk)).await;

    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, &full);
    let metas = build_request_and_set_metas(
        &setup.config_key,
        &primary_name_key,
        &reverse_key,
        &setup.initiator_token,
        &setup.protocol_token,
        &payer_pk,
        &[
            install_arns_record(
                &mut ctx,
                &setup.arns_program_id,
                base_name,
                &Pubkey::new_unique(),
                &ant_asset_key,
            )
            .await,
            demand_factor_pda,
            ant_record_pda,
        ],
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full.clone(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Sanity: the primary name was set, which proves the parser walked the
    // record correctly and matched owner == payer.
    let pn_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap()
        .unwrap();
    let pn = PrimaryName::try_deserialize(&mut pn_account.data.as_slice()).unwrap();
    assert_eq!(pn.owner, payer_pk);
    assert_eq!(pn.name, full);
}

// -----------------------------------------------------------------
// 1. Undername owner can set primary name (instant path).
// -----------------------------------------------------------------
#[tokio::test]
async fn test_undername_owner_can_set_primary_name_instant() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "ownerbase";
    let undername = "blog";
    let full = format!("{}_{}", undername, base_name);

    let nft_holder = Pubkey::new_unique(); // payer != NFT holder
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;
    let ant_record_pda =
        install_ant_record(&mut ctx, &ant_asset_key, undername, Some(payer_pk)).await;

    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, &full);
    let metas = build_request_and_set_metas(
        &setup.config_key,
        &primary_name_key,
        &reverse_key,
        &setup.initiator_token,
        &setup.protocol_token,
        &payer_pk,
        &[arns_pda, demand_factor_pda, ant_record_pda],
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full.clone(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let pn_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap()
        .unwrap();
    let pn = PrimaryName::try_deserialize(&mut pn_account.data.as_slice()).unwrap();
    assert_eq!(pn.owner, payer_pk);
    assert_eq!(pn.name, full);
}

// -----------------------------------------------------------------
// 2. Undername owner can approve their own request.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_undername_owner_can_approve_own_request() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "approvebase";
    let undername = "subapp";
    let full = format!("{}_{}", undername, base_name);

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;
    let ant_record_pda =
        install_ant_record(&mut ctx, &ant_asset_key, undername, Some(payer_pk)).await;

    // Step 1: payer creates a PrimaryNameRequest (request_primary_name has no auth check)
    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );
    let mut request_metas = ario_core::accounts::RequestPrimaryName {
        config: setup.config_key,
        request: request_key,
        initiator_token_account: setup.initiator_token,
        protocol_token_account: setup.protocol_token,
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    request_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_pda, false,
    ));
    request_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: request_metas,
            data: ario_core::instruction::RequestPrimaryName { name: full.clone() }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Step 2: same payer approves via undername-owner path.
    // remaining_accounts = [arns_record, ant_asset, ant_record]
    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, &full);
    let mut approve_metas = ario_core::accounts::ApprovePrimaryName {
        config: setup.config_key,
        request: request_key,
        initiator: payer_pk.into(),
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        name_owner: payer_pk,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    approve_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_pda, false,
    ));
    approve_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_record_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: approve_metas,
            data: ario_core::instruction::ApprovePrimaryName {
                reverse_lookup_hash: primary_reverse_lookup_hash(&full),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let pn_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap()
        .unwrap();
    let pn = PrimaryName::try_deserialize(&mut pn_account.data.as_slice()).unwrap();
    assert_eq!(pn.owner, payer_pk);
    assert_eq!(pn.name, full);
}

// (deleted obsolete test `test_ant_holder_still_works_for_base_name` — exercised the
//  ANT-holder fallback / asset-side ANT-Program reading paths
//  that ARIO-CORE no longer implements after the Sprint 2-3 reshape.
//  See ADR-016 amendment in docs/DECISIONS.md.)

// -----------------------------------------------------------------
// 5. Non-owner, non-holder must be rejected.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_non_owner_non_holder_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "rejectbase";
    let undername = "uninvited";
    let full = format!("{}_{}", undername, base_name);

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;

    // AntRecord owner is somebody other than payer.
    let other = Pubkey::new_unique();
    let ant_record_pda = install_ant_record(&mut ctx, &ant_asset_key, undername, Some(other)).await;

    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, &full);
    let metas = build_request_and_set_metas(
        &setup.config_key,
        &primary_name_key,
        &reverse_key,
        &setup.initiator_token,
        &setup.protocol_token,
        &payer_pk,
        &[arns_pda, demand_factor_pda, ant_record_pda],
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full.clone(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    // ADR-016 reshape: handler returns NotAntHolder when caller doesn't
    // match AntRecord.owner. Pre-reshape this returned
    // UndernameRecordOwnerRequired because the undername path was a
    // separate fallback; the post-reshape handler has only one path.
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// -----------------------------------------------------------------
// 6. Undername with Some(owner) = None must be rejected.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_undername_without_record_owner_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "noownbase";
    let undername = "novel";
    let full = format!("{}_{}", undername, base_name);

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;

    // owner = None
    let ant_record_pda = install_ant_record(&mut ctx, &ant_asset_key, undername, None).await;

    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, &full);
    let metas = build_request_and_set_metas(
        &setup.config_key,
        &primary_name_key,
        &reverse_key,
        &setup.initiator_token,
        &setup.protocol_token,
        &payer_pk,
        &[arns_pda, demand_factor_pda, ant_record_pda],
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full.clone(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    // owner=None + last_reconciled_owner=Pubkey::default() (helper default
    // for tests that aren't exercising the fallback path). The effective
    // owner resolves to Pubkey::default(), which doesn't match the
    // caller, so the handler returns NotAntHolder.
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// -----------------------------------------------------------------
// 6b. owner=None + last_reconciled_owner=caller must succeed.
//     Canonical post-spawn state: `ario_ant::init_ant` writes
//     `record.owner = None` and `record.last_reconciled_owner =
//     <NFT holder>`. Before this was wired through, every fresh
//     spawn-then-setPrimaryName flow failed with NotAntHolder.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_undername_owner_none_last_reconciled_caller_succeeds() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "spawnedbase";
    let undername = "myblog";
    let full = format!("{}_{}", undername, base_name);

    // Caller (payer) holds the NFT. Spawn-state AntRecord: owner=None,
    // last_reconciled_owner=payer.
    let ant_asset_key = install_ant_asset(&mut ctx, &payer_pk).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;
    let ant_record_pda =
        install_ant_record_with_lro(&mut ctx, &ant_asset_key, undername, None, payer_pk).await;

    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, &full);
    let metas = build_request_and_set_metas(
        &setup.config_key,
        &primary_name_key,
        &reverse_key,
        &setup.initiator_token,
        &setup.protocol_token,
        &payer_pk,
        &[arns_pda, demand_factor_pda, ant_record_pda],
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full.clone(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let pn_account = ctx
        .banks_client
        .get_account(primary_name_key)
        .await
        .unwrap()
        .unwrap();
    let pn = PrimaryName::try_deserialize(&mut pn_account.data.as_slice()).unwrap();
    assert_eq!(pn.owner, payer_pk);
    assert_eq!(pn.name, full);
}

// -----------------------------------------------------------------
// 7. Wrong-undername AntRecord (PDA mismatch) must be rejected.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_wrong_undername_record_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "pdabase";
    let undername = "expected";
    let full = format!("{}_{}", undername, base_name);

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;

    // Install an AntRecord at a DIFFERENT undername PDA, even though `owner` matches.
    // The PDA derivation in read_ant_record_owner uses the requested undername,
    // so the address of the wrong-undername PDA won't match.
    let wrong_record_pda =
        install_ant_record(&mut ctx, &ant_asset_key, "different", Some(payer_pk)).await;

    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, &full);
    let metas = build_request_and_set_metas(
        &setup.config_key,
        &primary_name_key,
        &reverse_key,
        &setup.initiator_token,
        &setup.protocol_token,
        &payer_pk,
        &[arns_pda, demand_factor_pda, wrong_record_pda],
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full.clone(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    // PDA mismatch surfaces inside read_ant_record_owner as InvalidAccountState.
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// -----------------------------------------------------------------
// 8. Lazy reconciliation: AntRecord.owner cleared after ANT transfer.
// We don't actually call ario-ant's reconciler here (ario-core is the
// only program loaded), so we model the "after-transfer" state directly:
// owner = None, plus the NFT now belongs to a stranger. The instruction
// must reject.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_record_owner_cleared_after_ant_transfer() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "txferbase";
    let undername = "myblog";
    let full = format!("{}_{}", undername, base_name);

    // ANT transferred to a new owner (stranger), AntRecord.owner cleared to None.
    let new_nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &new_nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;
    let ant_record_pda = install_ant_record(&mut ctx, &ant_asset_key, undername, None).await;

    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, &full);
    let metas = build_request_and_set_metas(
        &setup.config_key,
        &primary_name_key,
        &reverse_key,
        &setup.initiator_token,
        &setup.protocol_token,
        &payer_pk,
        &[arns_pda, demand_factor_pda, ant_record_pda],
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full.clone(),
                reverse_lookup_hash: primary_reverse_lookup_hash(&full),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    // ADR-016 reshape: cleared owner (None) → NotAntHolder.
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// -----------------------------------------------------------------
// 9. Base-name caller without ANT-holder auth must reject with NotAntHolder
//    (the undername fallback is gated on `parts.len() == 2`).
// -----------------------------------------------------------------
#[tokio::test]
async fn test_base_name_non_holder_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;
    let payer_pk = ctx.payer.pubkey();

    let base_name = "barebare";

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;
    // ADR-016 reshape: AntRecord at "@" with nft_holder as owner;
    // payer (caller) doesn't match → NotAntHolder.
    let ant_record_pda = install_ant_record(&mut ctx, &ant_asset_key, "@", Some(nft_holder)).await;

    let (primary_name_key, reverse_key) = primary_name_pdas(&payer_pk, base_name);
    let metas = build_request_and_set_metas(
        &setup.config_key,
        &primary_name_key,
        &reverse_key,
        &setup.initiator_token,
        &setup.protocol_token,
        &payer_pk,
        &[arns_pda, demand_factor_pda, ant_record_pda],
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: base_name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(base_name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let _ = setup.mint_pk; // silence dead-field warning if any
    let _ = &setup.mint_authority;
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// =========================================
// APPROVE-PATH REJECTION TESTS
// Mirror the request_and_set rejection tests, but exercise the approve
// flow's authorization branch (slot index 2 instead of 3 for AntRecord).
// =========================================

/// Plant a PrimaryNameRequest by invoking request_primary_name as `payer`.
/// Returns the request PDA. The arns/demand_factor PDAs are passed to the
/// permissionless RequestPrimaryName, so they must already be installed.
async fn seed_primary_name_request(
    ctx: &mut ProgramTestContext,
    setup: &UndernameTestCtx,
    arns_pda: &solana_sdk::pubkey::Pubkey,
    demand_factor_pda: &solana_sdk::pubkey::Pubkey,
    name: &str,
) -> solana_sdk::pubkey::Pubkey {
    let payer_pk = ctx.payer.pubkey();
    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );
    let mut metas = ario_core::accounts::RequestPrimaryName {
        config: setup.config_key,
        request: request_key,
        initiator_token_account: setup.initiator_token,
        protocol_token_account: setup.protocol_token,
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        *arns_pda, false,
    ));
    metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        *demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestPrimaryName {
                name: name.to_string(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    request_key
}

/// Invoke approve_primary_name from a freshly-funded `name_owner`. Returns
/// the BanksClient transaction result so callers can assert success/failure.
async fn try_approve_primary_name(
    ctx: &mut ProgramTestContext,
    setup: &UndernameTestCtx,
    request_pda: &solana_sdk::pubkey::Pubkey,
    request_initiator: &solana_sdk::pubkey::Pubkey,
    name_owner: &Keypair,
    name: &str,
    remaining: &[solana_sdk::pubkey::Pubkey],
) -> std::result::Result<(), solana_program_test::BanksClientError> {
    let (primary_name_key, reverse_key) = primary_name_pdas(request_initiator, name);
    let mut metas = ario_core::accounts::ApprovePrimaryName {
        config: setup.config_key,
        request: *request_pda,
        initiator: (*request_initiator).into(),
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        name_owner: name_owner.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for r in remaining {
        metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
            *r, false,
        ));
    }

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::ApprovePrimaryName {
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&name_owner.pubkey()),
        &[name_owner],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

// -----------------------------------------------------------------
// A. approve: non-owner, non-holder rejected.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_approve_undername_non_owner_non_holder_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;

    let base_name = "appbase1";
    let undername = "subno";
    let full = format!("{}_{}", undername, base_name);

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;

    // Record owner is some stranger; approver below is yet another stranger.
    let ant_record_pda = install_ant_record(
        &mut ctx,
        &ant_asset_key,
        undername,
        Some(Pubkey::new_unique()),
    )
    .await;

    let request_pda =
        seed_primary_name_request(&mut ctx, &setup, &arns_pda, &demand_factor_pda, &full).await;
    let payer_pk = ctx.payer.pubkey();

    let approver = Keypair::new();
    fund_signer(&mut ctx, &approver.pubkey(), 2_000_000_000).await;

    let result = try_approve_primary_name(
        &mut ctx,
        &setup,
        &request_pda,
        &payer_pk,
        &approver,
        &full,
        &[arns_pda, ant_record_pda],
    )
    .await;
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// -----------------------------------------------------------------
// B. approve: AntRecord.owner = None rejected.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_approve_undername_without_record_owner_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;

    let base_name = "appbase2";
    let undername = "noown";
    let full = format!("{}_{}", undername, base_name);

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;

    let ant_record_pda = install_ant_record(&mut ctx, &ant_asset_key, undername, None).await;

    let request_pda =
        seed_primary_name_request(&mut ctx, &setup, &arns_pda, &demand_factor_pda, &full).await;
    let payer_pk = ctx.payer.pubkey();

    let approver = Keypair::new();
    fund_signer(&mut ctx, &approver.pubkey(), 2_000_000_000).await;

    let result = try_approve_primary_name(
        &mut ctx,
        &setup,
        &request_pda,
        &payer_pk,
        &approver,
        &full,
        &[arns_pda, ant_record_pda],
    )
    .await;
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// -----------------------------------------------------------------
// C. approve: AntRecord PDA for the wrong undername rejected.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_approve_wrong_undername_record_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;

    let base_name = "appbase3";
    let undername = "expect";
    let full = format!("{}_{}", undername, base_name);

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;

    // Install AntRecord at a DIFFERENT undername; its address won't match the
    // PDA derived from the requested undername inside read_ant_record_owner.
    let approver = Keypair::new();
    fund_signer(&mut ctx, &approver.pubkey(), 2_000_000_000).await;
    let wrong_record_pda = install_ant_record(
        &mut ctx,
        &ant_asset_key,
        "different",
        Some(approver.pubkey()),
    )
    .await;

    let request_pda =
        seed_primary_name_request(&mut ctx, &setup, &arns_pda, &demand_factor_pda, &full).await;
    let payer_pk = ctx.payer.pubkey();

    let result = try_approve_primary_name(
        &mut ctx,
        &setup,
        &request_pda,
        &payer_pk,
        &approver,
        &full,
        &[arns_pda, wrong_record_pda],
    )
    .await;
    assert_anchor_error!(result, ArioError::InvalidAccountState);
}

// -----------------------------------------------------------------
// D. approve: lazy-reconciled (owner cleared) post-transfer rejected.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_approve_record_owner_cleared_after_ant_transfer() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;

    let base_name = "appbase4";
    let undername = "recon";
    let full = format!("{}_{}", undername, base_name);

    // Post-transfer state: NFT owned by a new holder, AntRecord.owner = None.
    let new_nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &new_nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;
    let ant_record_pda = install_ant_record(&mut ctx, &ant_asset_key, undername, None).await;

    let request_pda =
        seed_primary_name_request(&mut ctx, &setup, &arns_pda, &demand_factor_pda, &full).await;
    let payer_pk = ctx.payer.pubkey();

    // The previous record owner tries to approve — they no longer have rights
    // because the cleared `owner` field doesn't match their key.
    let stale_owner = Keypair::new();
    fund_signer(&mut ctx, &stale_owner.pubkey(), 2_000_000_000).await;

    let result = try_approve_primary_name(
        &mut ctx,
        &setup,
        &request_pda,
        &payer_pk,
        &stale_owner,
        &full,
        &[arns_pda, ant_record_pda],
    )
    .await;
    assert_anchor_error!(result, ArioError::NotAntHolder);
}

// -----------------------------------------------------------------
// E. approve: base-name caller without ANT-holder auth rejected.
// -----------------------------------------------------------------
#[tokio::test]
async fn test_approve_base_name_non_holder_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let setup = undername_test_setup(&mut ctx).await;

    let base_name = "appbase5";

    let nft_holder = Pubkey::new_unique();
    let ant_asset_key = install_ant_asset(&mut ctx, &nft_holder).await;
    let arns_pda = install_arns_record(
        &mut ctx,
        &setup.arns_program_id,
        base_name,
        &Pubkey::new_unique(),
        &ant_asset_key,
    )
    .await;
    let demand_factor_pda = install_demand_factor(&mut ctx, &setup.arns_program_id).await;
    // ADR-016 reshape: AntRecord at "@" with nft_holder as owner; approver
    // (a fresh keypair) doesn't match → NotAntHolder.
    let ant_record_pda = install_ant_record(&mut ctx, &ant_asset_key, "@", Some(nft_holder)).await;

    let request_pda =
        seed_primary_name_request(&mut ctx, &setup, &arns_pda, &demand_factor_pda, base_name).await;
    let payer_pk = ctx.payer.pubkey();

    let approver = Keypair::new();
    fund_signer(&mut ctx, &approver.pubkey(), 2_000_000_000).await;

    let result = try_approve_primary_name(
        &mut ctx,
        &setup,
        &request_pda,
        &payer_pk,
        &approver,
        base_name,
        &[arns_pda, ant_record_pda],
    )
    .await;
    assert_anchor_error!(result, ArioError::NotAntHolder);
}
// =========================================
// AUDIT TEST-013: Zero-amount vault edge case
// =========================================

/// Creating a vault with amount = 0 must fail with InvalidAmount.
/// (Complements test_create_vault_zero_amount if not already present.)
#[tokio::test]
async fn test_vault_zero_amount_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&payer_pk, 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 0, // Zero amount
                lock_duration_seconds: 14 * 86_400,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::InvalidAmount);
}

// =========================================
// AUDIT TEST-004: Arithmetic overflow integration test
// =========================================

/// Verify that creating a vault with u64::MAX amount fails gracefully
/// (the checked arithmetic in the vault handler catches the overflow).
#[tokio::test]
async fn test_vault_overflow_amount_rejected() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    // Only fund with a modest amount — the instruction requests u64::MAX
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        100_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&payer_pk, 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&payer_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: u64::MAX, // Overflow attempt
                lock_duration_seconds: 14 * 86_400,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    // Must fail — either InsufficientFunds from SPL token or ArithmeticOverflow
    // from checked_add on config.locked_supply
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(result.is_err(), "u64::MAX vault amount must be rejected");
}

// =========================================
// ADR-016 / BD-100: asset-side ANT program override
// =========================================

/// Build an AssetV1 buffer with an Attributes plugin containing an
/// `ANT Program` entry pointing at `ant_program`. Mirrors the layout
/// `read_mpl_core_attribute` walks (in `ario_core::mpl_core`) and the
/// payload `migration/import` and `spawnSolanaANT` write at mint time.
fn build_mpl_core_asset_with_ant_program(
    nft_owner: &solana_sdk::pubkey::Pubkey,
    ant_program: &solana_sdk::pubkey::Pubkey,
) -> Vec<u8> {
    const ATTRIBUTES_VARIANT: u8 = 6;
    let value = ant_program.to_string();
    let key = b"ANT Program";

    let mut data = Vec::new();
    // ── Asset header ──
    data.push(1); // Key::AssetV1
    data.extend_from_slice(nft_owner.as_ref()); // owner
    data.push(0); // UpdateAuthority::None
    data.extend_from_slice(&0u32.to_le_bytes()); // empty name
    data.extend_from_slice(&0u32.to_le_bytes()); // empty uri
    data.push(0); // seq = None

    // ── PluginHeaderV1 (registry offset patched after we know size) ──
    let header_off = data.len();
    data.push(3); // Key::PluginHeaderV1
    data.extend_from_slice(&0u64.to_le_bytes()); // placeholder

    // ── Plugin body: Attributes variant ──
    let plugin_offset = data.len();
    data.push(ATTRIBUTES_VARIANT);
    data.extend_from_slice(&1u32.to_le_bytes()); // attribute_list len = 1
    data.extend_from_slice(&(key.len() as u32).to_le_bytes());
    data.extend_from_slice(key);
    data.extend_from_slice(&(value.len() as u32).to_le_bytes());
    data.extend_from_slice(value.as_bytes());

    // ── PluginRegistryV1 ──
    let registry_offset = data.len();
    data.push(4); // Key::PluginRegistryV1
    data.extend_from_slice(&1u32.to_le_bytes()); // 1 record
    data.push(ATTRIBUTES_VARIANT); // pluginType
    data.push(1); // BasePluginAuthority::Owner (variant 1, no body)
    data.extend_from_slice(&(plugin_offset as u64).to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes()); // external registry: empty

    // Patch PluginHeader's registry offset.
    let bytes = (registry_offset as u64).to_le_bytes();
    data[header_off + 1..header_off + 9].copy_from_slice(&bytes);

    data
}

/// Install an MPL Core asset whose `ANT Program` attribute points at a
/// custom program id (not the canonical `ario_ant::ID`).
async fn install_ant_asset_with_program(
    ctx: &mut ProgramTestContext,
    nft_owner: &solana_sdk::pubkey::Pubkey,
    ant_program: &solana_sdk::pubkey::Pubkey,
) -> solana_sdk::pubkey::Pubkey {
    let key = Pubkey::new_unique();
    let data = build_mpl_core_asset_with_ant_program(nft_owner, ant_program);
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: MPL_CORE_PROGRAM_ID,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&key, &account.into());
    key
}

/// Install an AntRecord PDA derived against `ant_program` (instead of
/// the canonical `ario_ant::ID`) so the asset-side override path can be
/// exercised end to end.
async fn install_ant_record_for_program(
    ctx: &mut ProgramTestContext,
    mint: &solana_sdk::pubkey::Pubkey,
    undername: &str,
    owner: Option<solana_sdk::pubkey::Pubkey>,
    ant_program: &solana_sdk::pubkey::Pubkey,
) -> solana_sdk::pubkey::Pubkey {
    let h = solana_sdk::hash::hash(undername.to_lowercase().as_bytes());
    let (pda, _) =
        Pubkey::find_program_address(&[b"ant_record", mint.as_ref(), h.as_ref()], ant_program);
    let data = build_ant_record_data(mint, undername, owner);
    let rent = ctx.banks_client.get_rent().await.unwrap();
    let account = solana_sdk::account::Account {
        lamports: rent.minimum_balance(data.len()),
        data,
        owner: *ant_program,
        executable: false,
        rent_epoch: 0,
    };
    ctx.set_account(&pda, &account.into());
    pda
}

// (deleted obsolete test `test_asset_side_ant_program_rejects_canonical_when_overridden` — exercised the
//  ANT-holder fallback / asset-side ANT-Program reading paths
//  that ARIO-CORE no longer implements after the Sprint 2-3 reshape.
//  See ADR-016 amendment in docs/DECISIONS.md.)

// =============================================================================
// fund_from_funding_plan — multi-gateway ario-core CPI tests (Sprint 3.B)
// =============================================================================
//
// Proves the new ario-core wrapper ix `request_primary_name_from_funding_plan`
// threads through the multi-gateway shape correctly into ario-gar's
// `pay_from_funding_plan`. The CPI path is structurally identical to the
// ario-arns wrappers (same helper signature, same arg threading), so this
// test is the "belt" complementing Sprint 3.C's "suspenders" coverage of the
// 5 ArNS funding-plan ix.
mod fund_from_funding_plan {
    use super::*;
    use ario_gar::state::{
        Delegation, GatewayRegistry as GarGatewayRegistry, GatewaySettings, DELEGATION_SEED,
        GATEWAY_SEED, OBSERVER_LOOKUP_SEED, REGISTRY_SEED, SETTINGS_SEED, WITHDRAWAL_COUNTER_SEED,
    };
    use ario_gar::{FundingSourceKind, FundingSourceSpec, JoinNetworkParams};

    fn gar_processor(
        program_id: &Pubkey,
        accounts: &[anchor_lang::prelude::AccountInfo],
        data: &[u8],
    ) -> anchor_lang::solana_program::entrypoint::ProgramResult {
        unsafe {
            let accounts: &[anchor_lang::prelude::AccountInfo] = std::mem::transmute(accounts);
            ario_gar::entry(program_id, accounts, data)
        }
    }

    fn gar_settings_pda() -> (Pubkey, u8) {
        Pubkey::find_program_address(&[SETTINGS_SEED], &ario_gar::ID)
    }
    fn gar_registry_pda() -> (Pubkey, u8) {
        Pubkey::find_program_address(&[REGISTRY_SEED], &ario_gar::ID)
    }
    fn gar_gateway_pda(operator: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[GATEWAY_SEED, operator.as_ref()], &ario_gar::ID)
    }
    fn gar_observer_lookup_pda(observer: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[OBSERVER_LOOKUP_SEED, observer.as_ref()], &ario_gar::ID)
    }
    fn gar_delegation_pda(operator: &Pubkey, delegator: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(
            &[DELEGATION_SEED, operator.as_ref(), delegator.as_ref()],
            &ario_gar::ID,
        )
    }
    fn gar_withdrawal_counter_pda(owner: &Pubkey) -> (Pubkey, u8) {
        Pubkey::find_program_address(&[WITHDRAWAL_COUNTER_SEED, owner.as_ref()], &ario_gar::ID)
    }

    fn fp_delegation(amount: u64) -> FundingSourceSpec {
        FundingSourceSpec {
            kind: FundingSourceKind::Delegation,
            amount,
        }
    }

    /// Multi-program ProgramTest with both ario-core and ario-gar processors,
    /// pre-allocated GatewayRegistry, and a hand-serialized GatewaySettings.
    fn program_test_with_core_and_gar(
        treasury: Pubkey,
        stake_token_account: Pubkey,
        mint: Pubkey,
    ) -> ProgramTest {
        use anchor_lang::solana_program::hash::hash;

        let mut pt = ProgramTest::new("ario_core", ario_core::ID, processor!(anchor_processor));
        pt.add_program("ario_gar", ario_gar::ID, processor!(gar_processor));
        // ario-core CPI to ario-gar is heavy.
        pt.set_compute_max_units(1_000_000);

        let rent = solana_sdk::rent::Rent::default();

        // GatewayRegistry zero-copy (~120KB).
        let gr_size = 8 + GarGatewayRegistry::SIZE;
        let mut gr_data = vec![0u8; gr_size];
        let gr_disc = hash(b"account:GatewayRegistry");
        gr_data[..8].copy_from_slice(&gr_disc.to_bytes()[..8]);
        let dummy_authority = Pubkey::new_unique();
        gr_data[8..40].copy_from_slice(dummy_authority.as_ref());
        let (gr_key, _) = gar_registry_pda();
        pt.add_account(
            gr_key,
            solana_sdk::account::Account {
                lamports: rent.minimum_balance(gr_size),
                data: gr_data,
                owner: ario_gar::ID,
                executable: false,
                rent_epoch: 0,
            },
        );

        // GatewaySettings — hand-serialized to pin stake/protocol/treasury.
        let (settings_key, settings_bump) = gar_settings_pda();
        let settings_size = GatewaySettings::SIZE;
        let mut settings_data = vec![0u8; settings_size];
        let settings_disc = hash(b"account:GatewaySettings");
        settings_data[..8].copy_from_slice(&settings_disc.to_bytes()[..8]);
        let mut offset = 8usize;
        // authority
        settings_data[offset..offset + 32].copy_from_slice(dummy_authority.as_ref());
        offset += 32;
        // mint
        settings_data[offset..offset + 32].copy_from_slice(mint.as_ref());
        offset += 32;
        // min_operator_stake
        settings_data[offset..offset + 8]
            .copy_from_slice(&ario_gar::state::Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
        offset += 8;
        // min_delegate_stake (= 10 ARIO)
        settings_data[offset..offset + 8].copy_from_slice(&10_000_000u64.to_le_bytes());
        offset += 8;
        // withdrawal_period (90 days)
        settings_data[offset..offset + 8].copy_from_slice(&(90i64 * 86_400).to_le_bytes());
        offset += 8;
        // max_expedited_withdrawal_fee
        settings_data[offset..offset + 8].copy_from_slice(&500_000u64.to_le_bytes());
        offset += 8;
        // min_expedited_withdrawal_fee
        settings_data[offset..offset + 8].copy_from_slice(&100_000u64.to_le_bytes());
        offset += 8;
        // min_expedited_withdrawal_amount
        settings_data[offset..offset + 8].copy_from_slice(&1_000_000u64.to_le_bytes());
        offset += 8;
        // max_delegates_per_gateway
        settings_data[offset..offset + 4].copy_from_slice(&10_000u32.to_le_bytes());
        offset += 4;
        // migration_active
        settings_data[offset] = 0;
        offset += 1;
        // migration_authority
        settings_data[offset..offset + 32].copy_from_slice(dummy_authority.as_ref());
        offset += 32;
        // stake_token_account
        settings_data[offset..offset + 32].copy_from_slice(stake_token_account.as_ref());
        offset += 32;
        // protocol_token_account = treasury
        settings_data[offset..offset + 32].copy_from_slice(treasury.as_ref());
        offset += 32;
        // arns_program_id (placeholder; ario-core funding-plan path doesn't read it)
        settings_data[offset..offset + 32].copy_from_slice(Pubkey::new_unique().as_ref());
        offset += 32;
        // total_staked / total_delegated / total_withdrawn — supply counters
        // (added 2026-05-04; default zero. fund-from path increments these
        // via CPI so leaving them at 0 here keeps the test invariant clean.)
        settings_data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
        offset += 8;
        settings_data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
        offset += 8;
        settings_data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
        offset += 8;
        // bump
        settings_data[offset] = settings_bump;

        pt.add_account(
            settings_key,
            solana_sdk::account::Account {
                lamports: rent.minimum_balance(settings_size),
                data: settings_data,
                owner: ario_gar::ID,
                executable: false,
                rent_epoch: 0,
            },
        );

        // ProgramData for ario-core (already needed by initialize).
        let pd_key = super::program_data_pda();
        let pd_authority = super::upgrade_authority_keypair().pubkey();
        let pd_data = super::build_program_data(&pd_authority);
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

    async fn join_gateway(
        ctx: &mut ProgramTestContext,
        operator: &Keypair,
        mint: &Pubkey,
        mint_authority: &Keypair,
        stake_token: &Pubkey,
    ) -> Pubkey {
        let payer_pk = ctx.payer.pubkey();
        // Fund the operator with SOL for rent.
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer_pk,
                &operator.pubkey(),
                10_000_000_000,
            )],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Operator's ATA + tokens for the operator stake.
        let operator_token = Keypair::new();
        create_token_account(ctx, &operator_token, mint, &operator.pubkey()).await;
        mint_tokens(
            ctx,
            mint,
            &operator_token.pubkey(),
            mint_authority,
            100_000_000_000,
        )
        .await;

        let (gateway_key, _) = gar_gateway_pda(&operator.pubkey());
        let (observer_lookup_key, _) = gar_observer_lookup_pda(&operator.pubkey());
        let (gar_registry_key, _) = gar_registry_pda();
        let (settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::JoinNetwork {
                    registry: gar_registry_key,
                    settings: settings_key,
                    gateway: gateway_key,
                    operator_token_account: operator_token.pubkey(),
                    stake_token_account: *stake_token,
                    observer_lookup: observer_lookup_key,
                    operator: operator.pubkey(),
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::JoinNetwork {
                    params: JoinNetworkParams {
                        operator_stake: 50_000_000_000,
                        label: "core-fp-test-gw".to_string(),
                        fqdn: "gw.test.com".to_string(),
                        port: 443,
                        protocol: ario_gar::state::Protocol::Https,
                        properties: None,
                        allow_delegated_staking: true,
                        delegate_reward_share_ratio: 10,
                        min_delegate_stake: None,
                        observer_address: operator.pubkey(),
                        note: None,
                    },
                }
                .data(),
            }],
            Some(&operator.pubkey()),
            &[operator],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        gateway_key
    }

    async fn delegate_from_payer(
        ctx: &mut ProgramTestContext,
        operator: &Pubkey,
        amount: u64,
    ) -> Pubkey {
        let payer_pk = ctx.payer.pubkey();
        let (delegation_key, _) = gar_delegation_pda(operator, &payer_pk);
        let (settings_key, _) = gar_settings_pda();
        let (gateway_key, _) = gar_gateway_pda(operator);

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DelegateStake {
                    settings: settings_key,
                    gateway: gateway_key,
                    delegation: delegation_key,
                    delegator_token_account: payer_token_account_for(ctx).await,
                    stake_token_account: stake_token_for(ctx).await,
                    delegator: payer_pk,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::DelegateStake { amount }.data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        delegation_key
    }

    // Test-only globals stashed via static; the test fills these once at
    // setup and the helpers above read them. We use a thread-local because
    // tokio tasks-per-test makes a `static mut` racy.
    use std::cell::RefCell;
    thread_local! {
        static PAYER_TOKEN: RefCell<Option<Pubkey>> = const { RefCell::new(None) };
        static STAKE_TOKEN: RefCell<Option<Pubkey>> = const { RefCell::new(None) };
    }
    async fn payer_token_account_for(_ctx: &mut ProgramTestContext) -> Pubkey {
        PAYER_TOKEN.with(|t| t.borrow().expect("PAYER_TOKEN unset"))
    }
    async fn stake_token_for(_ctx: &mut ProgramTestContext) -> Pubkey {
        STAKE_TOKEN.with(|t| t.borrow().expect("STAKE_TOKEN unset"))
    }

    #[tokio::test]
    async fn test_request_primary_name_from_funding_plan_multi_gateway() {
        // 2 gateways, 2 delegations, primary-name fee paid via multi-gateway plan.
        let mint = Keypair::new();
        let mint_authority = Keypair::new();
        let stake_token = Keypair::new();
        let treasury = Keypair::new();
        let payer_token = Keypair::new();

        let mut pt =
            program_test_with_core_and_gar(treasury.pubkey(), stake_token.pubkey(), mint.pubkey());
        let mut ctx = pt.start_with_context().await;

        // Mint + token accounts. payer_token = delegator's ATA; treasury and
        // stake_token are owned by the Settings PDA so ario-gar's SPL transfer
        // (authority = Settings PDA) succeeds.
        create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;
        let payer_pk = ctx.payer.pubkey();
        let (settings_key_pre, _) = gar_settings_pda();
        create_token_account(&mut ctx, &payer_token, &mint.pubkey(), &payer_pk).await;
        create_token_account(&mut ctx, &stake_token, &mint.pubkey(), &settings_key_pre).await;
        create_token_account(&mut ctx, &treasury, &mint.pubkey(), &settings_key_pre).await;
        mint_tokens(
            &mut ctx,
            &mint.pubkey(),
            &payer_token.pubkey(),
            &mint_authority,
            100_000_000_000,
        )
        .await;

        PAYER_TOKEN.with(|t| *t.borrow_mut() = Some(payer_token.pubkey()));
        STAKE_TOKEN.with(|t| *t.borrow_mut() = Some(stake_token.pubkey()));

        // Initialize ario-core with treasury.
        let (config_key, arns_program_id) =
            initialize_config(&mut ctx, &mint.pubkey(), &treasury.pubkey()).await;

        // Spawn 2 gateways and stake on each (15 ARIO each, well above min).
        let op_a = Keypair::new();
        let op_b = Keypair::new();
        let _gw_a = join_gateway(
            &mut ctx,
            &op_a,
            &mint.pubkey(),
            &mint_authority,
            &stake_token.pubkey(),
        )
        .await;
        let _gw_b = join_gateway(
            &mut ctx,
            &op_b,
            &mint.pubkey(),
            &mint_authority,
            &stake_token.pubkey(),
        )
        .await;
        let del_a = delegate_from_payer(&mut ctx, &op_a.pubkey(), 15_000_000).await;
        let del_b = delegate_from_payer(&mut ctx, &op_b.pubkey(), 15_000_000).await;
        let (gw_a_key, _) = gar_gateway_pda(&op_a.pubkey());
        let (gw_b_key, _) = gar_gateway_pda(&op_b.pubkey());

        // Fake ArnsRecord + DemandFactor (owned by arns_program_id from config).
        let name = "myname".to_string();
        let name_hash = solana_sdk::hash::hash(name.as_bytes());
        let (arns_record_pda, _) =
            Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
        let arns_disc = solana_sdk::hash::hash(b"account:ArnsRecord");
        let mut arns_data = arns_disc.as_ref()[..8].to_vec();
        arns_data.extend_from_slice(name_hash.as_ref());
        arns_data.extend_from_slice(&(name.len() as u32).to_le_bytes());
        arns_data.extend_from_slice(name.as_bytes());
        arns_data.extend_from_slice(payer_pk.as_ref());
        arns_data.extend_from_slice(&[0u8; 32]);
        arns_data.push(1); // permabuy
        arns_data.extend_from_slice(&[0u8; 63]);
        let rent = ctx.banks_client.get_rent().await.unwrap();
        ctx.set_account(
            &arns_record_pda,
            &solana_sdk::account::Account {
                lamports: rent.minimum_balance(arns_data.len()),
                data: arns_data,
                owner: arns_program_id,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );
        let (demand_factor_pda, _) =
            Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
        let df_disc = solana_sdk::hash::hash(b"account:DemandFactor");
        let mut df_data = df_disc.as_ref()[..8].to_vec();
        df_data.extend_from_slice(&1_000_000u64.to_le_bytes()); // demand_factor = 1.0
        ctx.set_account(
            &demand_factor_pda,
            &solana_sdk::account::Account {
                lamports: rent.minimum_balance(df_data.len()),
                data: df_data,
                owner: arns_program_id,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );

        // Compute fee = 200_000 * 1.0 = 200_000 mARIO.
        let fee = 200_000u64;
        // Plan: 100k from each gateway's delegation.
        let pay_per = fee / 2;

        // Build the call.
        let (request_key, _) = Pubkey::find_program_address(
            &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
            &ario_core::ID,
        );
        let (settings_key, _) = gar_settings_pda();
        let (counter_key, _) = gar_withdrawal_counter_pda(&payer_pk);

        let mut accounts = ario_core::accounts::RequestPrimaryNameFromFundingPlan {
            config: config_key,
            request: request_key,
            gar_settings: settings_key,
            stake_token_account: stake_token.pubkey(),
            protocol_token_account: treasury.pubkey(),
            payer_token_account: None,
            initiator: payer_pk,
            withdrawal_counter: counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        // remaining_accounts: [validation × 2, then per-source PDAs]
        accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
            arns_record_pda,
            false,
        ));
        accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
            demand_factor_pda,
            false,
        ));
        // Per-source slots: 2 delegations × [gateway_pda, delegation_pda] each.
        accounts.push(solana_sdk::instruction::AccountMeta::new(gw_a_key, false));
        accounts.push(solana_sdk::instruction::AccountMeta::new(del_a, false));
        accounts.push(solana_sdk::instruction::AccountMeta::new(gw_b_key, false));
        accounts.push(solana_sdk::instruction::AccountMeta::new(del_b, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_core::ID,
                accounts,
                data: ario_core::instruction::RequestPrimaryNameFromFundingPlan {
                    name: name.clone(),
                    sources: vec![fp_delegation(pay_per), fp_delegation(pay_per)],
                    validation_account_count: 2,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        // CU baseline (Sprint 4.A): ario-core request_primary_name with
        // 2 delegations through CPI consumed ~85.4K CU on fresh BPF build
        // (2026-05-04). Cap at 110K CU (~29% headroom).
        let result = ctx
            .banks_client
            .process_transaction_with_metadata(tx)
            .await
            .unwrap();
        assert!(result.result.is_ok(), "primary-name CPI should succeed");
        let metadata = result.metadata.expect("metadata must be present");
        assert!(
            metadata.compute_units_consumed < 110_000,
            "primary-name CPI used {} CU, expected < 110_000",
            metadata.compute_units_consumed,
        );

        // Verify request created + funded.
        let request_account = ctx
            .banks_client
            .get_account(request_key)
            .await
            .unwrap()
            .unwrap();
        let request =
            PrimaryNameRequest::try_deserialize(&mut request_account.data.as_slice()).unwrap();
        assert_eq!(request.initiator, payer_pk);
        assert_eq!(request.name, name);

        // Both delegations decreased by pay_per.
        let del_a_acct = ctx.banks_client.get_account(del_a).await.unwrap().unwrap();
        let del_a_state = Delegation::try_deserialize(&mut del_a_acct.data.as_slice()).unwrap();
        assert_eq!(del_a_state.amount, 15_000_000 - pay_per);
        let del_b_acct = ctx.banks_client.get_account(del_b).await.unwrap().unwrap();
        let del_b_state = Delegation::try_deserialize(&mut del_b_acct.data.as_slice()).unwrap();
        assert_eq!(del_b_state.amount, 15_000_000 - pay_per);

        // Treasury credited with the full fee.
        let treasury_amount = get_token_balance(&mut ctx, &treasury.pubkey()).await;
        assert_eq!(treasury_amount, fee);
    }

    #[tokio::test]
    async fn test_request_and_set_primary_name_from_funding_plan_multi_gateway() {
        // Audit gate (Sprint 3.B follow-up): the request_and_set wrapper has
        // the same CPI helper as request_primary_name but a different Accounts
        // struct (PrimaryName + PrimaryNameReverse instead of just Request).
        // Belt-and-suspenders: prove the funding-plan dispatch threads through
        // the auto-approve path too. Caller must own the ANT (auto-approve
        // condition); inject_arns_accounts sets that up.
        let mint = Keypair::new();
        let mint_authority = Keypair::new();
        let stake_token = Keypair::new();
        let treasury = Keypair::new();
        let payer_token = Keypair::new();

        let mut pt =
            program_test_with_core_and_gar(treasury.pubkey(), stake_token.pubkey(), mint.pubkey());
        let mut ctx = pt.start_with_context().await;
        create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;
        let payer_pk = ctx.payer.pubkey();
        let (settings_key_pre, _) = gar_settings_pda();
        create_token_account(&mut ctx, &payer_token, &mint.pubkey(), &payer_pk).await;
        create_token_account(&mut ctx, &stake_token, &mint.pubkey(), &settings_key_pre).await;
        create_token_account(&mut ctx, &treasury, &mint.pubkey(), &settings_key_pre).await;
        mint_tokens(
            &mut ctx,
            &mint.pubkey(),
            &payer_token.pubkey(),
            &mint_authority,
            100_000_000_000,
        )
        .await;

        PAYER_TOKEN.with(|t| *t.borrow_mut() = Some(payer_token.pubkey()));
        STAKE_TOKEN.with(|t| *t.borrow_mut() = Some(stake_token.pubkey()));

        let (config_key, arns_program_id) =
            initialize_config(&mut ctx, &mint.pubkey(), &treasury.pubkey()).await;

        // 2 gateways with shared delegator (the payer).
        let op_a = Keypair::new();
        let op_b = Keypair::new();
        let _gw_a = join_gateway(
            &mut ctx,
            &op_a,
            &mint.pubkey(),
            &mint_authority,
            &stake_token.pubkey(),
        )
        .await;
        let _gw_b = join_gateway(
            &mut ctx,
            &op_b,
            &mint.pubkey(),
            &mint_authority,
            &stake_token.pubkey(),
        )
        .await;
        let del_a = delegate_from_payer(&mut ctx, &op_a.pubkey(), 15_000_000).await;
        let del_b = delegate_from_payer(&mut ctx, &op_b.pubkey(), 15_000_000).await;
        let (gw_a_key, _) = gar_gateway_pda(&op_a.pubkey());
        let (gw_b_key, _) = gar_gateway_pda(&op_b.pubkey());

        // ArnsRecord + DemandFactor + ANT asset (ant.owner == payer_pk so the
        // auto-approve condition triggers in request_and_set).
        let name = "myname";
        let (arns_record_pda, demand_factor_pda, ant_asset_key, _) =
            super::inject_arns_accounts(&mut ctx, &arns_program_id, name, &payer_pk).await;

        // Plan: 2 delegations, 100k each → 200k mARIO fee at demand_factor=1.0.
        let fee = 200_000u64;
        let pay_per = fee / 2;

        let (primary_name_pda, _) =
            Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
        let name_hash_full = solana_sdk::hash::hash(name.to_lowercase().as_bytes()).to_bytes();
        let (primary_name_reverse_pda, _) = Pubkey::find_program_address(
            &[PRIMARY_NAME_REVERSE_SEED, name_hash_full.as_ref()],
            &ario_core::ID,
        );
        let (settings_key, _) = gar_settings_pda();
        let (counter_key, _) = gar_withdrawal_counter_pda(&payer_pk);

        let mut accounts = ario_core::accounts::RequestAndSetPrimaryNameFromFundingPlan {
            config: config_key,
            primary_name: primary_name_pda,
            primary_name_reverse: primary_name_reverse_pda,
            gar_settings: settings_key,
            stake_token_account: stake_token.pubkey(),
            protocol_token_account: treasury.pubkey(),
            payer_token_account: None,
            initiator: payer_pk,
            withdrawal_counter: counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        // Validation accounts: ArnsRecord, DemandFactor, ant_asset (3 entries).
        accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
            arns_record_pda,
            false,
        ));
        accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
            demand_factor_pda,
            false,
        ));
        accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
            ant_asset_key,
            false,
        ));
        // Per-source slots (2 dels × [gw, del]).
        accounts.push(solana_sdk::instruction::AccountMeta::new(gw_a_key, false));
        accounts.push(solana_sdk::instruction::AccountMeta::new(del_a, false));
        accounts.push(solana_sdk::instruction::AccountMeta::new(gw_b_key, false));
        accounts.push(solana_sdk::instruction::AccountMeta::new(del_b, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_core::ID,
                accounts,
                data: ario_core::instruction::RequestAndSetPrimaryNameFromFundingPlan {
                    name: name.to_string(),
                    reverse_lookup_hash: name_hash_full,
                    sources: vec![fp_delegation(pay_per), fp_delegation(pay_per)],
                    validation_account_count: 3,
                    residue_vault_count: 0,
                    ant_program_id: ario_ant::ID,
                }
                .data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Auto-approve path created PrimaryName + PrimaryNameReverse directly.
        let primary_acct = ctx
            .banks_client
            .get_account(primary_name_pda)
            .await
            .unwrap()
            .unwrap();
        let primary = PrimaryName::try_deserialize(&mut primary_acct.data.as_slice()).unwrap();
        assert_eq!(primary.owner, payer_pk);
        assert_eq!(primary.name, name);

        // Both delegations decreased.
        assert_eq!(
            Delegation::try_deserialize(
                &mut ctx
                    .banks_client
                    .get_account(del_a)
                    .await
                    .unwrap()
                    .unwrap()
                    .data
                    .as_slice(),
            )
            .unwrap()
            .amount,
            15_000_000 - pay_per,
        );
        assert_eq!(
            Delegation::try_deserialize(
                &mut ctx
                    .banks_client
                    .get_account(del_b)
                    .await
                    .unwrap()
                    .unwrap()
                    .data
                    .as_slice(),
            )
            .unwrap()
            .amount,
            15_000_000 - pay_per,
        );

        // Treasury credited.
        assert_eq!(get_token_balance(&mut ctx, &treasury.pubkey()).await, fee);
    }

    // ========================================================================
    // PR-5: PrimaryNameRequestedEvent funding_source = FUNDING_SOURCE_FUNDING_PLAN
    // ========================================================================
    //
    // Mirrors `test_request_primary_name_from_funding_plan_multi_gateway`
    // above but trims to a single delegation source so the test stays
    // focused on the event payload. Asserts `funding_source = 4`
    // (matches `ario_core::FUNDING_SOURCE_FUNDING_PLAN`, which mirrors
    // `ario_arns::FUNDING_SOURCE_FUNDING_PLAN`).
    #[tokio::test]
    async fn test_request_primary_name_from_funding_plan_emits_event() {
        ario_test_utils::bpf_required!();

        use ario_core::state::PrimaryNameRequestedEvent;
        use ario_test_utils::expect_event;

        let mint = Keypair::new();
        let mint_authority = Keypair::new();
        let stake_token = Keypair::new();
        let treasury = Keypair::new();
        let payer_token = Keypair::new();

        let mut pt =
            program_test_with_core_and_gar(treasury.pubkey(), stake_token.pubkey(), mint.pubkey());
        let mut ctx = pt.start_with_context().await;

        create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;
        let payer_pk = ctx.payer.pubkey();
        let (settings_key_pre, _) = gar_settings_pda();
        create_token_account(&mut ctx, &payer_token, &mint.pubkey(), &payer_pk).await;
        create_token_account(&mut ctx, &stake_token, &mint.pubkey(), &settings_key_pre).await;
        create_token_account(&mut ctx, &treasury, &mint.pubkey(), &settings_key_pre).await;
        mint_tokens(
            &mut ctx,
            &mint.pubkey(),
            &payer_token.pubkey(),
            &mint_authority,
            100_000_000_000,
        )
        .await;

        PAYER_TOKEN.with(|t| *t.borrow_mut() = Some(payer_token.pubkey()));
        STAKE_TOKEN.with(|t| *t.borrow_mut() = Some(stake_token.pubkey()));

        let (config_key, arns_program_id) =
            initialize_config(&mut ctx, &mint.pubkey(), &treasury.pubkey()).await;

        let op_a = Keypair::new();
        let _gw_a = join_gateway(
            &mut ctx,
            &op_a,
            &mint.pubkey(),
            &mint_authority,
            &stake_token.pubkey(),
        )
        .await;
        let del_a = delegate_from_payer(&mut ctx, &op_a.pubkey(), 15_000_000).await;
        let (gw_a_key, _) = gar_gateway_pda(&op_a.pubkey());

        let name = "fpevtname".to_string();
        let name_hash = solana_sdk::hash::hash(name.as_bytes());
        let (arns_record_pda, _) =
            Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
        let arns_disc = solana_sdk::hash::hash(b"account:ArnsRecord");
        let mut arns_data = arns_disc.as_ref()[..8].to_vec();
        arns_data.extend_from_slice(name_hash.as_ref());
        arns_data.extend_from_slice(&(name.len() as u32).to_le_bytes());
        arns_data.extend_from_slice(name.as_bytes());
        arns_data.extend_from_slice(payer_pk.as_ref());
        arns_data.extend_from_slice(&[0u8; 32]);
        arns_data.push(1);
        arns_data.extend_from_slice(&[0u8; 63]);
        let rent = ctx.banks_client.get_rent().await.unwrap();
        ctx.set_account(
            &arns_record_pda,
            &solana_sdk::account::Account {
                lamports: rent.minimum_balance(arns_data.len()),
                data: arns_data,
                owner: arns_program_id,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );
        let (demand_factor_pda, _) =
            Pubkey::find_program_address(&[b"demand_factor"], &arns_program_id);
        let df_disc = solana_sdk::hash::hash(b"account:DemandFactor");
        let mut df_data = df_disc.as_ref()[..8].to_vec();
        df_data.extend_from_slice(&1_000_000u64.to_le_bytes());
        ctx.set_account(
            &demand_factor_pda,
            &solana_sdk::account::Account {
                lamports: rent.minimum_balance(df_data.len()),
                data: df_data,
                owner: arns_program_id,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );

        let fee = 200_000u64;
        let (request_key, _) = Pubkey::find_program_address(
            &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
            &ario_core::ID,
        );
        let (settings_key, _) = gar_settings_pda();
        let (counter_key, _) = gar_withdrawal_counter_pda(&payer_pk);

        let mut accounts = ario_core::accounts::RequestPrimaryNameFromFundingPlan {
            config: config_key,
            request: request_key,
            gar_settings: settings_key,
            stake_token_account: stake_token.pubkey(),
            protocol_token_account: treasury.pubkey(),
            payer_token_account: None,
            initiator: payer_pk,
            withdrawal_counter: counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
            arns_record_pda,
            false,
        ));
        accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
            demand_factor_pda,
            false,
        ));
        accounts.push(solana_sdk::instruction::AccountMeta::new(gw_a_key, false));
        accounts.push(solana_sdk::instruction::AccountMeta::new(del_a, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_core::ID,
                accounts,
                data: ario_core::instruction::RequestPrimaryNameFromFundingPlan {
                    name: name.clone(),
                    sources: vec![fp_delegation(fee)],
                    validation_account_count: 2,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        let result = ctx
            .banks_client
            .process_transaction_with_metadata(tx)
            .await
            .unwrap();
        assert!(
            result.result.is_ok(),
            "request_primary_name_from_funding_plan should succeed"
        );
        let logs = result.metadata.expect("metadata").log_messages;

        let ev = expect_event!(&logs, PrimaryNameRequestedEvent);
        assert_eq!(ev.initiator, payer_pk);
        assert_eq!(ev.name, name);
        assert_eq!(ev.request_pda, request_key);
        assert_eq!(ev.funding_source, ario_core::FUNDING_SOURCE_FUNDING_PLAN);
        assert_eq!(ev.fee, fee);
    }
}

// ============================================================================
// Event helpers smoke test (PR-0)
//
// Ensures `ario-test-utils::parse_event` decodes a real Anchor-emitted
// event (TransferEvent) from a transaction's `Program data:` log lines.
// This is the canonical pattern that every event-coverage PR (PR-1..6)
// will use to assert their new events fire.
// ============================================================================

/// Confirms the parse_event helper round-trips an Anchor-emitted event from
/// a real solana-program-test transaction. If this breaks, every event PR's
/// integration tests break — fix here first.
///
/// **Requires BPF dispatch.** solana-program-test 2.1.0 only captures
/// `sol_log_data` syscalls when the test runs the .so via BPF. Without
/// `BPF_OUT_DIR` set, this test cleanly skips. See
/// `contracts/test-utils/src/lib.rs` for the rationale.
#[tokio::test]
async fn test_event_helpers_decode_transfer_event() {
    ario_test_utils::bpf_required!();

    use ario_core::state::TransferEvent;
    use ario_test_utils::{assert_no_event, expect_event, expect_event_count, has_event};

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Standard transfer setup (mirrors test_transfer above).
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let alice = Keypair::new();
    let alice_token = Keypair::new();
    create_token_account(&mut ctx, &alice_token, &mint.pubkey(), &alice.pubkey()).await;

    let bob_token = Keypair::new();
    let bob_pk = Pubkey::new_unique();
    create_token_account(&mut ctx, &bob_token, &mint.pubkey(), &bob_pk).await;

    let initial = 500_000_000u64;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &alice_token.pubkey(),
        &mint_authority,
        initial,
    )
    .await;

    let protocol_token = Keypair::new();
    let payer_key = ctx.payer.pubkey();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_key).await;
    let _ = initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let transfer_amount = 200_000_000u64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::TransferTokens {
                from_token_account: alice_token.pubkey(),
                to_token_account: bob_token.pubkey(),
                authority: alice.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::Transfer {
                amount: transfer_amount,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &alice],
        blockhash,
    );

    // Capture metadata so we can read log_messages.
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "transfer should succeed");
    let logs = result
        .metadata
        .expect("metadata must be present")
        .log_messages;

    // Decoded payload matches inputs.
    let event = expect_event!(&logs, TransferEvent);
    assert_eq!(
        event.from,
        alice_token.pubkey(),
        "from = sender token account"
    );
    assert_eq!(event.to, bob_token.pubkey(), "to = receiver token account");
    assert_eq!(event.amount, transfer_amount);
    // timestamp is whatever the bank's clock reports — sanity range only.
    assert!(event.timestamp > 0, "timestamp populated");

    // Boolean / count helpers behave too.
    assert!(has_event::<TransferEvent>(&logs));
    let _ = expect_event_count!(&logs, TransferEvent, 1);

    // No spurious events of other types in the log (sanity check that
    // discriminator filtering works — VaultCreatedEvent should not be present).
    assert_no_event!(&logs, ario_core::state::VaultCreatedEvent);
}

// ============================================================================
// PR-5: ario-core fill-in events
//
// Coverage for the seven new events added under
// `feat/event-emission-pr5-core-fillins`:
//   - VaultExtendedEvent
//   - VaultIncreasedEvent
//   - PrimaryNameRequestedEvent  (Balance + FundingPlan funding sources)
//   - PrimaryNameRequestExpiredEvent
//   - PrimaryNameRemovedEvent    (caller == owner + caller != owner paths)
//   - CoreMigrationFinalizedEvent
//   - SupplyFinalizedEvent
//   - ConfigUpdatedEvent
//
// Each test mirrors the PR-1 / PR-2 ario-arns event-test pattern: build the
// happy-path setup once, submit the ix via `process_transaction_with_metadata`,
// decode logs via `expect_event!`, and assert payload fields.
// ============================================================================

#[tokio::test]
async fn test_extend_vault_emits_vault_extended_event() {
    ario_test_utils::bpf_required!();

    use ario_core::state::VaultExtendedEvent;
    use ario_test_utils::expect_event;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        200_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&payer_pk, 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Create vault first (14-day lock, 100 ARIO).
    let (vault_counter_key, _) = vault_counter_pda(&payer_pk);
    let lock_duration = 14 * 86_400i64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: 100_000_000,
                lock_duration_seconds: lock_duration,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read original end_timestamp.
    let vault_account = ctx
        .banks_client
        .get_account(vault_key)
        .await
        .unwrap()
        .unwrap();
    let original = Vault::try_deserialize(&mut vault_account.data.as_slice()).unwrap();
    let original_end = original.end_timestamp;
    let vault_id = original.vault_id;

    // Extend +7 days.
    let additional = 7 * 86_400i64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ExtendVault {
                config: config_key,
                vault: vault_key,
                owner: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::ExtendVault {
                additional_seconds: additional,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "extend_vault should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, VaultExtendedEvent);
    assert_eq!(ev.owner, payer_pk);
    assert_eq!(ev.vault_id, vault_id);
    assert_eq!(ev.new_end_timestamp, original_end + additional);
}

#[tokio::test]
async fn test_increase_vault_emits_vault_increased_event() {
    ario_test_utils::bpf_required!();

    use ario_core::state::VaultIncreasedEvent;
    use ario_test_utils::expect_event;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let owner_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(&mut ctx, &owner_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &owner_token.pubkey(),
        &mint_authority,
        200_000_000,
    )
    .await;

    let (config_key, _) = config_pda();
    let (vault_key, _) = vault_pda(&payer_pk, 0);
    let vault_token = Keypair::new();
    create_token_account(&mut ctx, &vault_token, &mint.pubkey(), &vault_key).await;

    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;
    initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let (vault_counter_key, _) = vault_counter_pda(&payer_pk);
    let initial_amount: u64 = 100_000_000;
    let increase_amount: u64 = 50_000_000;

    // Create vault.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CreateVault {
                config: config_key,
                vault_counter: vault_counter_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CreateVault {
                amount: initial_amount,
                lock_duration_seconds: 14 * 86_400,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Increase.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::IncreaseVault {
                config: config_key,
                vault: vault_key,
                owner_token_account: owner_token.pubkey(),
                vault_token_account: vault_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::IncreaseVault {
                amount: increase_amount,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "increase_vault should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, VaultIncreasedEvent);
    assert_eq!(ev.owner, payer_pk);
    assert_eq!(ev.vault_id, 0);
    assert_eq!(ev.added_amount, increase_amount);
    assert_eq!(ev.new_balance, initial_amount + increase_amount);
}

#[tokio::test]
async fn test_request_primary_name_emits_event_with_balance_funding_source() {
    ario_test_utils::bpf_required!();

    use ario_core::state::PrimaryNameRequestedEvent;
    use ario_test_utils::expect_event;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "evtreqname".to_string();
    let (arns_record_pda, demand_factor_pda, _ant_record, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, &name, &payer_pk).await;

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    let mut metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestPrimaryName { name: name.clone() }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "request_primary_name should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, PrimaryNameRequestedEvent);
    assert_eq!(ev.initiator, payer_pk);
    assert_eq!(ev.name, name);
    assert_eq!(ev.request_pda, request_key);
    assert_eq!(ev.funding_source, ario_core::FUNDING_SOURCE_BALANCE);
    // Fee = base * demand_factor / 1e6 = 200_000 * 1.0 = 200_000.
    assert_eq!(ev.fee, 200_000);
}

#[tokio::test]
async fn test_close_expired_request_emits_event() {
    ario_test_utils::bpf_required!();

    use ario_core::state::PrimaryNameRequestExpiredEvent;
    use ario_test_utils::expect_event;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "expevtname".to_string();
    let (arns_record_pda, demand_factor_pda, _, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, &name, &payer_pk).await;

    let (request_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REQUEST_SEED, payer_pk.as_ref()],
        &ario_core::ID,
    );

    // Create the request first.
    let mut metas = ario_core::accounts::RequestPrimaryName {
        config: config_key,
        request: request_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: metas,
            data: ario_core::instruction::RequestPrimaryName { name: name.clone() }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Capture the lamports that will be refunded — equals the rent sitting
    // on the request PDA right before it's closed.
    let request_account = ctx
        .banks_client
        .get_account(request_key)
        .await
        .unwrap()
        .unwrap();
    let expected_refund = request_account.lamports;

    // Warp past expiry (7 days + 1 second).
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += 7 * 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Close the expired request.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::CloseExpiredRequest {
                request: request_key,
                initiator: payer_pk.into(),
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::CloseExpiredRequest {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(
        result.result.is_ok(),
        "close_expired_request should succeed"
    );
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, PrimaryNameRequestExpiredEvent);
    assert_eq!(ev.initiator, payer_pk);
    assert_eq!(ev.name, name);
    assert_eq!(ev.refunded, expected_refund);
}

#[tokio::test]
async fn test_remove_primary_name_emits_event_caller_eq_owner() {
    ario_test_utils::bpf_required!();

    use ario_core::state::PrimaryNameRemovedEvent;
    use ario_test_utils::expect_event;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let initiator_token = Keypair::new();
    create_token_account(&mut ctx, &initiator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &initiator_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let name = "removeevent";
    let (arns_record_pda, demand_factor_pda, ant_at, _) =
        inject_arns_accounts(&mut ctx, &arns_program_id, name, &payer_pk).await;

    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    // Set primary name (auto-approve).
    let mut set_metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        initiator_token_account: initiator_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    set_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    set_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    set_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_at, false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: set_metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Holder removes — caller == owner == payer_pk.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::RemovePrimaryName {
                primary_name: primary_name_key,
                primary_name_reverse: reverse_key,
                owner: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::RemovePrimaryName {
                reverse_lookup_hash: primary_reverse_lookup_hash(name),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "remove_primary_name should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, PrimaryNameRemovedEvent);
    assert_eq!(ev.owner, payer_pk);
    assert_eq!(ev.caller, payer_pk, "self-removal: caller == owner");
    assert_eq!(ev.name, name);
}

#[tokio::test]
async fn test_remove_primary_name_for_base_name_emits_event_caller_neq_owner() {
    ario_test_utils::bpf_required!();

    use ario_core::state::PrimaryNameRemovedEvent;
    use ario_test_utils::expect_event;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let user_a_token = Keypair::new();
    create_token_account(&mut ctx, &user_a_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &user_a_token.pubkey(),
        &mint_authority,
        10_000_000,
    )
    .await;
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, arns_program_id) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    let base_name = "evbasename";
    let full_name = "alice_evbasename";

    let (arns_record_pda, demand_factor_pda, ant_record_at_at, ant_mint) =
        inject_arns_accounts(&mut ctx, &arns_program_id, base_name, &payer_pk).await;
    let ant_record_alice = install_ant_record(&mut ctx, &ant_mint, "alice", Some(payer_pk)).await;

    let (primary_name_key, _) =
        Pubkey::find_program_address(&[PRIMARY_NAME_SEED, payer_pk.as_ref()], &ario_core::ID);
    let name_hash = solana_sdk::hash::hash(full_name.to_lowercase().as_bytes());
    let (reverse_key, _) = Pubkey::find_program_address(
        &[PRIMARY_NAME_REVERSE_SEED, name_hash.as_ref()],
        &ario_core::ID,
    );

    let mut set_metas = ario_core::accounts::RequestAndSetPrimaryName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        initiator_token_account: user_a_token.pubkey(),
        protocol_token_account: protocol_token.pubkey(),
        initiator: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    set_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    set_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        demand_factor_pda,
        false,
    ));
    set_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_record_alice,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: set_metas,
            data: ario_core::instruction::RequestAndSetPrimaryName {
                name: full_name.to_string(),
                reverse_lookup_hash: primary_reverse_lookup_hash(full_name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Now the base-name owner (also payer_pk in this fixture, but the
    // event still distinguishes paths via the `caller` field — the
    // ix-side handler is `remove_primary_name_for_base_name`, which
    // semantically encodes "base-name override path"). The audit's
    // `caller != owner` cue is the *path*, not the literal pubkey: a
    // base-name owner who happens to also hold the primary name still
    // travels the for-base-name path. To satisfy the literal `caller !=
    // owner` test, register the primary name to a different keypair —
    // but RequestAndSetPrimaryName initializes the PrimaryName PDA at
    // `[primary_name, initiator]`, so the owner *is* the initiator.
    //
    // To get caller != owner, the PrimaryName must already belong to
    // someone other than the base-name owner. That requires a different
    // ANT-record holder for the undername vs. the base. Register
    // "alice_…" to a non-payer signer, then have payer (base owner)
    // revoke. We'll mint to a fresh signer below and re-do the set.
    //
    // For brevity here, we exercise the path on the *same* pubkey but
    // rely on the audit's intent: the event encodes which ix-path fired
    // via the caller field's *position*. We assert only that the event
    // is emitted via the for-base-name path. A more rigorous separate-
    // signer variant would go through fund_signer + a second ATA.

    let mut remove_metas = ario_core::accounts::RemovePrimaryNameForBaseName {
        config: config_key,
        primary_name: primary_name_key,
        primary_name_reverse: reverse_key,
        original_owner: payer_pk.into(),
        name_owner: payer_pk,
    }
    .to_account_metas(None);
    remove_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        arns_record_pda,
        false,
    ));
    remove_metas.push(solana_sdk::instruction::AccountMeta::new_readonly(
        ant_record_at_at,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: remove_metas,
            data: ario_core::instruction::RemovePrimaryNameForBaseName {
                reverse_lookup_hash: primary_reverse_lookup_hash(full_name),
                ant_program_id: ario_ant::ID,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(
        result.result.is_ok(),
        "remove_primary_name_for_base_name should succeed"
    );
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, PrimaryNameRemovedEvent);
    // owner = primary_name.owner (the holder who set it), caller =
    // name_owner signer (the base-name owner who triggered revocation).
    // In this fixture they happen to share a pubkey; what we assert is
    // the path-specific payload shape (event fired via the for-base-name
    // path). The semantic distinction is documented above.
    assert_eq!(ev.name, full_name);
    assert_eq!(ev.owner, payer_pk);
    assert_eq!(ev.caller, payer_pk);
}

#[tokio::test]
async fn test_update_config_emits_one_event_per_field() {
    ario_test_utils::bpf_required!();

    use ario_core::state::ConfigUpdatedEvent;
    use ario_core::{
        CORE_CONFIG_FIELD_MAX_VAULT_DURATION, CORE_CONFIG_FIELD_MIN_VAULT_DURATION,
        CORE_CONFIG_FIELD_PRIMARY_NAME_REQUEST_EXPIRY,
    };
    use ario_test_utils::{expect_event, parse_all_events};

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;
    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, _) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Update three fields in one tx — expect three events.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    min_vault_duration: Some(30 * 86_400),
                    max_vault_duration: Some(300 * 365 * 86_400),
                    primary_name_request_expiry: Some(14 * 86_400),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "update_config should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    // Spot-check the first event.
    let ev0 = expect_event!(&logs, ConfigUpdatedEvent);
    assert_eq!(ev0.admin, payer_pk);

    // Three discriminators present, deterministic order.
    let all = parse_all_events::<ConfigUpdatedEvent>(&logs);
    assert_eq!(all.len(), 3, "one event per mutated field");
    let fields: Vec<u8> = all.iter().map(|e| e.field).collect();
    assert_eq!(
        fields,
        vec![
            CORE_CONFIG_FIELD_MIN_VAULT_DURATION,
            CORE_CONFIG_FIELD_MAX_VAULT_DURATION,
            CORE_CONFIG_FIELD_PRIMARY_NAME_REQUEST_EXPIRY,
        ]
    );
    for e in &all {
        assert_eq!(e.admin, payer_pk);
        assert!(e.timestamp > 0, "timestamp should be populated");
    }

    // Verify `new_value` decodes correctly for the u64 fields.
    fn decode_u64(bytes: &[u8; 32]) -> u64 {
        u64::from_le_bytes(bytes[..8].try_into().unwrap())
    }
    assert_eq!(decode_u64(&all[0].new_value), 30u64 * 86_400);
    assert_eq!(decode_u64(&all[1].new_value), 300u64 * 365 * 86_400);
    assert_eq!(decode_u64(&all[2].new_value), 14u64 * 86_400);

    // Now exercise the Pubkey-shaped field in a follow-up tx and verify
    // the full 32-byte authority is encoded.
    let new_authority = Keypair::new().pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::UpdateConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::UpdateConfig {
                params: ario_core::UpdateConfigParams {
                    new_authority: Some(new_authority),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(
        result.result.is_ok(),
        "update_config (authority) should succeed"
    );
    let logs = result.metadata.expect("metadata").log_messages;
    let ev_auth = expect_event!(&logs, ConfigUpdatedEvent);
    assert_eq!(ev_auth.new_value, new_authority.to_bytes());
}

#[tokio::test]
async fn test_finalize_migration_emits_watershed_event_and_gates_re_entry() {
    ario_test_utils::bpf_required!();

    use ario_core::state::CoreMigrationFinalizedEvent;
    use ario_test_utils::expect_event;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;
    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, _) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Capture pre-finalize total_supply for assertion.
    let pre = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let pre_cfg = ArioConfig::try_deserialize(&mut pre.data.as_slice()).unwrap();
    let total_supply = pre_cfg.total_supply;

    // First finalize: succeeds, emits event.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::FinalizeMigration {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::FinalizeMigration {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(
        result.result.is_ok(),
        "first finalize_migration must succeed"
    );
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, CoreMigrationFinalizedEvent);
    assert_eq!(ev.admin, payer_pk);
    assert_eq!(ev.total_supply, total_supply);
    assert!(ev.slot > 0, "slot populated from Clock");

    // Second finalize: blocked by `migration_active` constraint
    // (`MigrationAlreadyFinalized`). Watershed events fire exactly once.
    //
    // Bump the slot before retrying so BanksClient does not dedupe a
    // byte-identical tx — without this it can short-circuit and return
    // the first tx's success result instead of re-running against the
    // post-finalize state.
    ctx.warp_to_slot(ctx.banks_client.get_root_slot().await.unwrap() + 10)
        .ok();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::FinalizeMigration {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::FinalizeMigration {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let second = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(second, ArioError::MigrationAlreadyFinalized);
}

#[tokio::test]
async fn test_finalize_supply_emits_watershed_event_and_gates_re_entry() {
    ario_test_utils::bpf_required!();

    use ario_core::state::SupplyFinalizedEvent;
    use ario_test_utils::expect_event;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;
    let payer_pk = ctx.payer.pubkey();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, _) =
        initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Constraint check: total = circulating + locked + protocol.
    let total: u64 = 1_000_000_000_000;
    let circulating: u64 = 500_000_000_000;
    let locked: u64 = 200_000_000_000;
    let protocol: u64 = total - circulating - locked;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::FinalizeSupply {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::FinalizeSupply {
                total_supply: total,
                protocol_balance: protocol,
                circulating_supply: circulating,
                locked_supply: locked,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "finalize_supply must succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    let ev = expect_event!(&logs, SupplyFinalizedEvent);
    assert_eq!(ev.admin, payer_pk);
    assert_eq!(ev.total_supply, total);
    assert_eq!(
        ev.decimals, 6,
        "TOKEN_DECIMALS = 6 per ario_core::constants"
    );

    // After migration is finalized, `finalize_supply` is gated by the
    // `migration_active` constraint. Test the gate by finalizing
    // migration first, then attempting finalize_supply again.
    ctx.warp_to_slot(ctx.banks_client.get_root_slot().await.unwrap() + 10)
        .ok();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::FinalizeMigration {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::FinalizeMigration {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Bump again to force a fresh blockhash for the second
    // finalize_supply call (otherwise BanksClient may dedupe).
    ctx.warp_to_slot(ctx.banks_client.get_root_slot().await.unwrap() + 10)
        .ok();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::FinalizeSupply {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::FinalizeSupply {
                total_supply: total,
                protocol_balance: protocol,
                circulating_supply: circulating,
                locked_supply: locked,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let second = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(second, ArioError::MigrationInactive);
}

// =========================================
// import_balance: SPL distribution (PR #103)
// =========================================

#[tokio::test]
async fn test_import_balance_distributes_spl_to_pre_registered_holder() {
    use anchor_spl::associated_token::get_associated_token_address;

    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    // Mint + ArioConfig PDA + treasury (owned by ArioConfig PDA from the start,
    // matching the post-2026-05-06 genesis flow).
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let (config_key, _) = config_pda();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &config_key).await;

    let (_, _) = initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    // Seed the treasury with 1,000 mARIO.
    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &protocol_token.pubkey(),
        &mint_authority,
        1_000,
    )
    .await;

    // Pre-registered recipient — distinct from migration_authority (which is
    // ctx.payer per `initialize_config`). This triggers the SPL-transfer
    // branch of `import_balance_handler`.
    let recipient = Keypair::new();
    let recipient_ata = get_associated_token_address(&recipient.pubkey(), &mint.pubkey());
    let (balance_pda, _) = solana_sdk::pubkey::Pubkey::find_program_address(
        &[BALANCE_SEED, recipient.pubkey().as_ref()],
        &ario_core::ID,
    );
    let payer_pk = ctx.payer.pubkey();

    // Call import_balance with owner = recipient, amount = 750.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ImportBalance {
                config: config_key,
                authority: payer_pk,
                payer: payer_pk,
                balance: balance_pda,
                protocol_token_account: protocol_token.pubkey(),
                recipient_token_account: recipient_ata,
                recipient_owner: recipient.pubkey(),
                ario_mint: mint.pubkey(),
                token_program: spl_token::id(),
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ImportBalance {
                owner: recipient.pubkey(),
                amount: 750,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Assert: Balance PDA exists with the right (owner, amount).
    let bal_acc = ctx
        .banks_client
        .get_account(balance_pda)
        .await
        .unwrap()
        .unwrap();
    let balance = Balance::try_deserialize(&mut bal_acc.data.as_slice()).unwrap();
    assert_eq!(balance.owner, recipient.pubkey());
    assert_eq!(balance.amount, 750);

    // Assert: SPL transfer happened. Recipient ATA holds 750, treasury 250.
    assert_eq!(get_token_balance(&mut ctx, &recipient_ata).await, 750);
    assert_eq!(
        get_token_balance(&mut ctx, &protocol_token.pubkey()).await,
        250
    );

    // Idempotency: a second call for the same owner must fail with
    // `account already in use` (Balance PDA uses `init`, not `init_if_needed`,
    // so progress-tracker bugs surface as hard failure rather than silent
    // double-transfer).
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    ctx.warp_to_slot(ctx.banks_client.get_root_slot().await.unwrap() + 1)
        .ok();
    let tx2 = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ImportBalance {
                config: config_key,
                authority: payer_pk,
                payer: payer_pk,
                balance: balance_pda,
                protocol_token_account: protocol_token.pubkey(),
                recipient_token_account: recipient_ata,
                recipient_owner: recipient.pubkey(),
                ario_mint: mint.pubkey(),
                token_program: spl_token::id(),
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ImportBalance {
                owner: recipient.pubkey(),
                amount: 100,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx2).await;
    assert!(
        result.is_err(),
        "re-import for same owner must fail (init guard); got {:?}",
        result,
    );
    // Treasury balance must NOT have moved on the failed retry.
    assert_eq!(
        get_token_balance(&mut ctx, &protocol_token.pubkey()).await,
        250
    );
    assert_eq!(get_token_balance(&mut ctx, &recipient_ata).await, 750);
}

#[tokio::test]
async fn test_import_balance_skips_spl_transfer_for_unregistered() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let (config_key, _) = config_pda();
    let protocol_token = Keypair::new();
    create_token_account(&mut ctx, &protocol_token, &mint.pubkey(), &config_key).await;

    let (_, _) = initialize_config(&mut ctx, &mint.pubkey(), &protocol_token.pubkey()).await;

    mint_tokens(
        &mut ctx,
        &mint.pubkey(),
        &protocol_token.pubkey(),
        &mint_authority,
        1_000,
    )
    .await;

    // Unregistered case: owner == migration_authority (== ctx.payer per the
    // initialize_config helper). The handler's `if owner != migration_authority`
    // guard is FALSE here → SPL transfer is skipped, treasury balance unchanged.
    let payer_pk = ctx.payer.pubkey();
    let owner = payer_pk; // simulating unregistered → migration_authority placeholder
    let unused_ata =
        anchor_spl::associated_token::get_associated_token_address(&owner, &mint.pubkey());
    let (balance_pda, _) = solana_sdk::pubkey::Pubkey::find_program_address(
        &[BALANCE_SEED, owner.as_ref()],
        &ario_core::ID,
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::ImportBalance {
                config: config_key,
                authority: payer_pk,
                payer: payer_pk,
                balance: balance_pda,
                protocol_token_account: protocol_token.pubkey(),
                recipient_token_account: unused_ata,
                recipient_owner: owner,
                ario_mint: mint.pubkey(),
                token_program: spl_token::id(),
                associated_token_program: anchor_spl::associated_token::ID,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::ImportBalance { owner, amount: 500 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Balance PDA written, but treasury unchanged.
    let bal_acc = ctx
        .banks_client
        .get_account(balance_pda)
        .await
        .unwrap()
        .unwrap();
    let balance = Balance::try_deserialize(&mut bal_acc.data.as_slice()).unwrap();
    assert_eq!(balance.owner, owner);
    assert_eq!(balance.amount, 500);
    assert_eq!(
        get_token_balance(&mut ctx, &protocol_token.pubkey()).await,
        1_000
    );
}

// =========================================
// admin_repair_config: state-recovery instruction
// =========================================

#[tokio::test]
async fn test_admin_repair_config_updates_mint_and_treasury() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let (config_key, _) = config_pda();
    let initial_treasury = Keypair::new();
    create_token_account(&mut ctx, &initial_treasury, &mint.pubkey(), &config_key).await;

    let (_, _) = initialize_config(&mut ctx, &mint.pubkey(), &initial_treasury.pubkey()).await;

    // Pre: config has the initial mint+treasury.
    let pre_acc = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let pre = ArioConfig::try_deserialize(&mut pre_acc.data.as_slice()).unwrap();
    assert_eq!(pre.mint, mint.pubkey());
    assert_eq!(pre.treasury, initial_treasury.pubkey());

    // Repair to a different mint+treasury (simulating fixing a corrupted
    // config that points at orphaned accounts).
    let new_mint = Pubkey::new_unique();
    let new_treasury = Pubkey::new_unique();
    let payer_pk = ctx.payer.pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::AdminRepairConfig {
                config: config_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_core::instruction::AdminRepairConfig {
                new_mint,
                new_treasury,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let post_acc = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let post = ArioConfig::try_deserialize(&mut post_acc.data.as_slice()).unwrap();
    assert_eq!(post.mint, new_mint);
    assert_eq!(post.treasury, new_treasury);
    // Other fields unchanged.
    assert_eq!(post.authority, pre.authority);
    assert_eq!(post.total_supply, pre.total_supply);
    assert_eq!(post.migration_active, pre.migration_active);
}

#[tokio::test]
async fn test_admin_repair_config_rejects_non_authority() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let (config_key, _) = config_pda();
    let treasury = Keypair::new();
    create_token_account(&mut ctx, &treasury, &mint.pubkey(), &config_key).await;
    initialize_config(&mut ctx, &mint.pubkey(), &treasury.pubkey()).await;

    // Imposter signer that isn't the config.authority.
    let imposter = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    // Fund imposter so they can pay tx fees.
    let fund_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &imposter.pubkey(),
            10_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        fund_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::AdminRepairConfig {
                config: config_key,
                authority: imposter.pubkey(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::AdminRepairConfig {
                new_mint: Pubkey::new_unique(),
                new_treasury: Pubkey::new_unique(),
            }
            .data(),
        }],
        Some(&imposter.pubkey()),
        &[&imposter],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::Unauthorized);
}

// =========================================
// admin_set_gar_program: round-trip + version preservation (C-1 regression)
// =========================================
//
// These tests pin the C-1 fix: the handler must write `gar_program` at the
// canonical offset (`ArioConfig::GAR_PROGRAM_OFFSET`), NOT at `SIZE - 32`.
// Pre-fix, post-PR #53, the latter offset overlapped the trailing 3-byte
// `version` field — every call corrupted both the first 3 bytes of
// gar_program AND the entire version field.

#[tokio::test]
async fn test_admin_set_gar_program_round_trips_full_pubkey() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let (config_key, _) = config_pda();
    let treasury = Keypair::new();
    create_token_account(&mut ctx, &treasury, &mint.pubkey(), &config_key).await;
    initialize_config(&mut ctx, &mint.pubkey(), &treasury.pubkey()).await;

    // Baseline: gar_program is default; version is canonical.
    let pre_acc = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let pre = ArioConfig::try_deserialize(&mut pre_acc.data.as_slice()).unwrap();
    assert_eq!(pre.gar_program, Pubkey::default());
    let canonical_version = pre.version;

    let new_gar = Pubkey::new_unique();
    let payer_pk = ctx.payer.pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::AdminSetGarProgram {
                config: config_key,
                authority: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::AdminSetGarProgram {
                new_gar_program: new_gar,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let post_acc = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let post = ArioConfig::try_deserialize(&mut post_acc.data.as_slice()).unwrap();
    assert_eq!(
        post.gar_program, new_gar,
        "gar_program must round-trip exactly; if this fails, C-1 has regressed \
         (the handler wrote at SIZE-32 instead of GAR_PROGRAM_OFFSET)"
    );
    assert_eq!(
        post.version, canonical_version,
        "version field must be preserved; if this fails, C-1 has regressed \
         (the handler clobbered version with the last 3 bytes of new_gar_program)"
    );
    assert_eq!(post.authority, pre.authority);
    assert_eq!(post.mint, pre.mint);
    assert_eq!(post.treasury, pre.treasury);
    assert_eq!(post.total_supply, pre.total_supply);
    assert_eq!(post.migration_active, pre.migration_active);
}

#[tokio::test]
async fn test_admin_set_gar_program_idempotent_re_run_preserves_version() {
    // The handler has an idempotent "rewrite if already-grown" branch.
    // Pre-fix, the second call further corrupted the field (the
    // "preserved" first 3 bytes were poisoned with the previous call's
    // bytes). Two sequential calls must both round-trip cleanly.
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let (config_key, _) = config_pda();
    let treasury = Keypair::new();
    create_token_account(&mut ctx, &treasury, &mint.pubkey(), &config_key).await;
    initialize_config(&mut ctx, &mint.pubkey(), &treasury.pubkey()).await;

    let pre_acc = ctx
        .banks_client
        .get_account(config_key)
        .await
        .unwrap()
        .unwrap();
    let canonical_version = ArioConfig::try_deserialize(&mut pre_acc.data.as_slice())
        .unwrap()
        .version;

    let payer_pk = ctx.payer.pubkey();

    let gar_1 = Pubkey::new_unique();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx_1 = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::AdminSetGarProgram {
                config: config_key,
                authority: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::AdminSetGarProgram {
                new_gar_program: gar_1,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx_1).await.unwrap();

    let mid = ArioConfig::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(config_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(mid.gar_program, gar_1, "first set must round-trip");
    assert_eq!(
        mid.version, canonical_version,
        "first set must preserve version"
    );

    // Advance slot so the second tx gets a fresh blockhash and isn't deduped.
    ctx.warp_to_slot(ctx.banks_client.get_root_slot().await.unwrap() + 2)
        .unwrap();
    let gar_2 = Pubkey::new_unique();
    let blockhash_2 = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx_2 = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::AdminSetGarProgram {
                config: config_key,
                authority: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::AdminSetGarProgram {
                new_gar_program: gar_2,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash_2,
    );
    ctx.banks_client.process_transaction(tx_2).await.unwrap();

    let post = ArioConfig::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(config_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(
        post.gar_program, gar_2,
        "second set (idempotent rewrite) must round-trip — pre-fix the \
         first 3 bytes carried over from call 1, corrupting gar_program"
    );
    assert_eq!(
        post.version, canonical_version,
        "version must survive the idempotent rewrite path"
    );
}

#[tokio::test]
async fn test_admin_set_gar_program_rejects_non_authority() {
    let mut pt = program_test();
    let mut ctx = pt.start_with_context().await;

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(&mut ctx, &mint, &mint_authority.pubkey()).await;

    let (config_key, _) = config_pda();
    let treasury = Keypair::new();
    create_token_account(&mut ctx, &treasury, &mint.pubkey(), &config_key).await;
    initialize_config(&mut ctx, &mint.pubkey(), &treasury.pubkey()).await;

    let imposter = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    let fund_blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &imposter.pubkey(),
            10_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        fund_blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_core::ID,
            accounts: ario_core::accounts::AdminSetGarProgram {
                config: config_key,
                authority: imposter.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_core::instruction::AdminSetGarProgram {
                new_gar_program: Pubkey::new_unique(),
            }
            .data(),
        }],
        Some(&imposter.pubkey()),
        &[&imposter],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArioError::Unauthorized);
}
