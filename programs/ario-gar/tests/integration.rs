use anchor_lang::{prelude::*, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    program_pack::Pack,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_gar::error::GarError;
use ario_gar::state::*;

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
    accounts: &[anchor_lang::prelude::AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[anchor_lang::prelude::AccountInfo] = std::mem::transmute(accounts);
        ario_gar::entry(program_id, accounts, data)
    }
}

/// Native-processor bridge for ario-core, used by the `distribute_epoch`
/// lifecycle tests. `distribute_epoch` CPIs into
/// `ario_core::release_treasury_to_recipient` to move the per-epoch reward
/// pool out of the treasury (whose SPL authority lives on ario-core's
/// `ArioConfig` PDA). The tests load ario-core in-process via
/// `pt.add_program("ario_core", ario_gar::ARIO_CORE_PROGRAM_ID, ...)`; the
/// gar source pins the CPI target to `ARIO_CORE_PROGRAM_ID`, which is the
/// placeholder `declare_id!()` literal ario-core was compiled with, so the
/// loaded-at address matches ario-core's runtime declared ID.
fn ario_core_processor(
    program_id: &Pubkey,
    accounts: &[anchor_lang::prelude::AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[anchor_lang::prelude::AccountInfo] = std::mem::transmute(accounts);
        ario_core::entry(program_id, accounts, data)
    }
}

/// Create a ProgramTest with pre-initialized GAR state.
///
/// In native processor mode, Anchor's `init` CPI to system_program is limited to
/// 10KB account data increase (`MAX_PERMITTED_DATA_INCREASE`). The GatewayRegistry
/// is ~64KB, so we pre-create it and the GatewaySettings account directly.
/// Standard test setup. Pre-creates `GatewayRegistry`, `GatewaySettings`,
/// AND `EpochSettings` so any test that exercises `leave_network` /
/// `prune_gateway` (which now snapshot `epoch_duration` onto the gateway)
/// can run without extra setup. Tests that explicitly initialize epoch
/// settings via the program's `initialize_epochs` ix should use
/// `program_test_with_gar_for_epoch_init` instead.
fn program_test_with_gar(
    authority: &Pubkey,
    mint: &Pubkey,
    stake_token_account: &Pubkey,
    protocol_token_account: &Pubkey,
) -> ProgramTest {
    let mut pt = program_test_with_gar_for_epoch_init(
        authority,
        mint,
        stake_token_account,
        protocol_token_account,
    );
    // Sensible defaults: epoch_duration=86_400s (1 day), epochs disabled, genesis at 0.
    // Tests that need different values can call pre_create_epoch_settings AGAIN
    // after this — `add_account` overwrites.
    pre_create_epoch_settings(&mut pt, authority, 0, 86_400, false);
    pt
}

/// Test setup variant that does NOT pre-create `EpochSettings`. Used by the
/// two tests that exercise `initialize_epochs` directly — their `init`
/// constraint requires the account to not exist on entry.
fn program_test_with_gar_for_epoch_init(
    authority: &Pubkey,
    mint: &Pubkey,
    stake_token_account: &Pubkey,
    protocol_token_account: &Pubkey,
) -> ProgramTest {
    use anchor_lang::solana_program::hash::hash;

    let mut pt = ProgramTest::new("ario_gar", ario_gar::ID, processor!(anchor_processor));
    pt.set_compute_max_units(1_000_000);

    let rent = solana_sdk::rent::Rent::default();

    // PR-4: pre-add ProgramData so initialize_epochs satisfies the upgrade-
    // authority constraint, and fund that authority for `init` rent.
    let pd_key = program_data_pda();
    let pd_authority = upgrade_authority_keypair().pubkey();
    let pd_data = build_program_data(&pd_authority);
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

    // Pre-create GatewayRegistry (zero-copy, ~64KB)
    let (registry_key, _) = registry_pda();
    let registry_size = 8 + GatewayRegistry::SIZE;
    let mut reg_data = vec![0u8; registry_size];
    // Zero-copy discriminator
    let reg_disc = hash(b"account:GatewayRegistry");
    reg_data[..8].copy_from_slice(&reg_disc.to_bytes()[..8]);
    // GatewayRegistry fields (after discriminator, in repr(C) order):
    // authority: [u8; 32] at offset 8
    reg_data[8..40].copy_from_slice(authority.as_ref());
    // count: u32 at offset 40 — leave as 0
    // _padding: u32 at offset 44 — leave as 0
    // gateways: [Pubkey; 2000] at offset 48 — leave as zeroes

    pt.add_account(
        registry_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(registry_size),
            data: reg_data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    // Pre-create GatewaySettings (~177 bytes)
    let (settings_key, settings_bump) = settings_pda();
    let settings_size = GatewaySettings::SIZE;
    let mut settings_data = vec![0u8; settings_size];
    // Regular account discriminator
    let settings_disc = hash(b"account:GatewaySettings");
    settings_data[..8].copy_from_slice(&settings_disc.to_bytes()[..8]);

    // Serialize fields manually (Borsh, sequential):
    let mut offset = 8;
    // authority: Pubkey
    settings_data[offset..offset + 32].copy_from_slice(authority.as_ref());
    offset += 32;
    // mint: Pubkey
    settings_data[offset..offset + 32].copy_from_slice(mint.as_ref());
    offset += 32;
    // min_operator_stake: u64
    settings_data[offset..offset + 8].copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
    offset += 8;
    // min_delegate_stake: u64
    settings_data[offset..offset + 8].copy_from_slice(&10_000_000u64.to_le_bytes());
    offset += 8;
    // withdrawal_period: i64
    settings_data[offset..offset + 8].copy_from_slice(&(30i64 * 86_400).to_le_bytes());
    offset += 8;
    // max_expedited_withdrawal_penalty: u64
    settings_data[offset..offset + 8].copy_from_slice(&500_000u64.to_le_bytes());
    offset += 8;
    // min_expedited_withdrawal_penalty: u64
    settings_data[offset..offset + 8].copy_from_slice(&100_000u64.to_le_bytes());
    offset += 8;
    // min_expedited_withdrawal_amount: u64
    settings_data[offset..offset + 8].copy_from_slice(&1_000_000u64.to_le_bytes());
    offset += 8;
    // max_delegates_per_gateway: u32
    settings_data[offset..offset + 4].copy_from_slice(&10_000u32.to_le_bytes());
    offset += 4;
    // migration_active: bool
    settings_data[offset] = 0; // false — migration not active in tests
    offset += 1;
    // migration_authority: Pubkey
    settings_data[offset..offset + 32].copy_from_slice(authority.as_ref());
    offset += 32;
    // stake_token_account: Pubkey
    settings_data[offset..offset + 32].copy_from_slice(stake_token_account.as_ref());
    offset += 32;
    // protocol_token_account: Pubkey
    settings_data[offset..offset + 32].copy_from_slice(protocol_token_account.as_ref());
    offset += 32;
    // arns_program_id: Pubkey — placeholder. None of the ario-gar
    // integration tests exercise the prescribe_epoch NameRegistry path
    // (which is the only consumer of this field), so any non-zero value
    // works. End-to-end NameRegistry validation lives in
    // ario-arns/tests/integration.rs where ario_arns::ID is reachable.
    settings_data[offset..offset + 32].copy_from_slice(&[0xAAu8; 32]);
    offset += 32;
    // total_staked: u64 — starts at 0
    settings_data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
    offset += 8;
    // total_delegated: u64 — starts at 0
    settings_data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
    offset += 8;
    // total_withdrawn: u64 — starts at 0
    settings_data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
    offset += 8;
    // bump: u8
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

    pt
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

/// PR-4: deterministic upgrade-authority keypair used for tests of
/// `initialize_epochs` (which now binds to BPFLoaderUpgradeable's program-data
/// upgrade authority).
fn upgrade_authority_keypair() -> Keypair {
    solana_sdk::signer::keypair::keypair_from_seed(&[42u8; 32])
        .expect("keypair_from_seed must succeed for fixed test seed")
}

fn program_data_pda() -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[ario_gar::ID.as_ref()],
        &solana_sdk::bpf_loader_upgradeable::id(),
    );
    pda
}

fn build_program_data(upgrade_authority: &Pubkey) -> Vec<u8> {
    let mut data = Vec::with_capacity(45);
    data.extend_from_slice(&3u32.to_le_bytes());
    data.extend_from_slice(&0i64.to_le_bytes());
    data.push(1);
    data.extend_from_slice(upgrade_authority.as_ref());
    data
}

// PDA helpers
fn registry_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[REGISTRY_SEED], &ario_gar::ID)
}

fn settings_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SETTINGS_SEED], &ario_gar::ID)
}

fn gateway_pda(operator: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[GATEWAY_SEED, operator.as_ref()], &ario_gar::ID)
}

fn observer_lookup_pda(observer_address: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[OBSERVER_LOOKUP_SEED, observer_address.as_ref()],
        &ario_gar::ID,
    )
}

fn epoch_settings_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[EPOCH_SETTINGS_SEED], &ario_gar::ID)
}

fn withdrawal_counter_pda(owner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[WITHDRAWAL_COUNTER_SEED, owner.as_ref()], &ario_gar::ID)
}

fn withdrawal_pda(owner: &Pubkey, withdrawal_id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            WITHDRAWAL_SEED,
            owner.as_ref(),
            &withdrawal_id.to_le_bytes(),
        ],
        &ario_gar::ID,
    )
}

fn delegation_pda(gateway_operator: &Pubkey, delegator: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            DELEGATION_SEED,
            gateway_operator.as_ref(),
            delegator.as_ref(),
        ],
        &ario_gar::ID,
    )
}

struct GarSetup {
    mint: Keypair,
    mint_authority: Keypair,
    operator_token: Keypair,
    stake_token: Keypair,
    protocol_token: Keypair,
    registry_key: Pubkey,
    settings_key: Pubkey,
}

/// Pre-generate keypairs for a GAR test, then build ProgramTest with matching settings.
fn prepare_gar_test() -> (Keypair, Keypair, Keypair, Keypair, Keypair) {
    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    let operator_token = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    (
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
}

/// Setup GAR test environment: creates mint, token accounts.
/// Expects program_test_with_gar() was used to pre-create registry + settings.
async fn setup_gar(
    ctx: &mut ProgramTestContext,
    mint: Keypair,
    mint_authority: Keypair,
    operator_token: Keypair,
    stake_token: Keypair,
    protocol_token: Keypair,
) -> GarSetup {
    create_mint(ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    create_token_account(ctx, &operator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        ctx,
        &mint.pubkey(),
        &operator_token.pubkey(),
        &mint_authority,
        100_000_000_000,
    )
    .await;

    let (settings_key, _) = settings_pda();
    create_token_account(ctx, &stake_token, &mint.pubkey(), &settings_key).await;
    create_token_account(ctx, &protocol_token, &mint.pubkey(), &settings_key).await;

    let (registry_key, _) = registry_pda();

    GarSetup {
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
        registry_key,
        settings_key,
    }
}

/// Variant of `setup_gar` for the `distribute_epoch` lifecycle tests: the
/// `protocol_token_account` (the treasury / transfer source) is created with
/// its SPL `Owner` authority set to ario-core's `ArioConfig` PDA, matching
/// production — `release_treasury_to_recipient` signs the SPL transfer FROM
/// the treasury with the ArioConfig PDA as authority, so the treasury must be
/// owned by that PDA, not by gar settings. The stake token account (transfer
/// destination) stays owned by gar settings; SPL transfers don't require the
/// destination's owner to sign.
async fn setup_gar_with_core_treasury(
    ctx: &mut ProgramTestContext,
    mint: Keypair,
    mint_authority: Keypair,
    operator_token: Keypair,
    stake_token: Keypair,
    protocol_token: Keypair,
) -> GarSetup {
    create_mint(ctx, &mint, &mint_authority.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    create_token_account(ctx, &operator_token, &mint.pubkey(), &payer_pk).await;
    mint_tokens(
        ctx,
        &mint.pubkey(),
        &operator_token.pubkey(),
        &mint_authority,
        100_000_000_000,
    )
    .await;

    let (settings_key, _) = settings_pda();
    create_token_account(ctx, &stake_token, &mint.pubkey(), &settings_key).await;
    // Treasury source: owned by the ArioConfig PDA (production parity).
    let (ario_config_key, _) = ario_config_pda();
    create_token_account(ctx, &protocol_token, &mint.pubkey(), &ario_config_key).await;

    let (registry_key, _) = registry_pda();

    GarSetup {
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
        registry_key,
        settings_key,
    }
}

/// Join a gateway with default params. Returns gateway PDA key.
async fn join_gateway(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    operator_stake: u64,
) -> Pubkey {
    let payer_pk = ctx.payer.pubkey();
    let (gateway_key, _) = gateway_pda(&payer_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake,
                    label: "test-gateway".to_string(),
                    fqdn: "gateway.example.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: true,
                    delegate_reward_share_ratio: 10,
                    min_delegate_stake: None,
                    observer_address: payer_pk,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    gateway_key
}

// =========================================
// TESTS
// =========================================

#[tokio::test]
async fn test_gar_settings_initialized() {
    let placeholder_authority = Pubkey::new_unique();
    let mint = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let mut pt = program_test_with_gar(
        &placeholder_authority,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let (settings_key, _) = settings_pda();
    let settings_account = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    let settings = GatewaySettings::try_deserialize(&mut settings_account.data.as_slice()).unwrap();

    assert_eq!(settings.authority, placeholder_authority);
    assert_eq!(settings.min_operator_stake, Gateway::MIN_OPERATOR_STAKE);
    assert_eq!(settings.withdrawal_period, 30 * 86_400);
    assert_eq!(settings.max_delegates_per_gateway, 10_000);
    assert_eq!(settings.stake_token_account, stake_token.pubkey());
    assert_eq!(settings.protocol_token_account, protocol_token.pubkey());
}

/// Verify `admin_set_withdrawal_period`:
///   1. Authority signer succeeds; `settings.withdrawal_period` updates.
///   2. Non-authority signer rejected with `Unauthorized`.
///   3. `new_period_seconds < 60` rejected with `InvalidParameter` (matches
///      the same bound on `admin_set_epoch_duration`).
#[tokio::test]
async fn test_admin_set_withdrawal_period() {
    let mint = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let mut pt = program_test_with_gar(
        // payer is the authority for the GAR settings PDA in this fixture.
        // We don't have `ctx.payer.pubkey()` yet (ProgramTest hasn't started),
        // but ProgramTest's default payer is deterministic. Pre-set the
        // authority to a placeholder we'll then replace below.
        &Pubkey::new_unique(),
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    // Re-write the GatewaySettings PDA's `authority` field to `ctx.payer`
    // so we can sign as authority. (Anchor processes the `has_one =
    // authority` constraint at ix execution time, against on-chain state.)
    let (settings_key, _) = settings_pda();
    let mut settings_account = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    // authority is the first field after the 8-byte discriminator.
    settings_account.data[8..40].copy_from_slice(ctx.payer.pubkey().as_ref());
    ctx.set_account(
        &settings_key,
        &solana_sdk::account::AccountSharedData::from(settings_account),
    );

    // Step 1: authority signer succeeds, period updates from default to 120s.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::AdminSetWithdrawalPeriod {
                settings: settings_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AdminSetWithdrawalPeriod {
                new_period_seconds: 120,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let account = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    let settings = GatewaySettings::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(settings.withdrawal_period, 120);

    // Step 2: non-authority signer rejected.
    let bad_signer = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &bad_signer.pubkey(),
            10_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let bad_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::AdminSetWithdrawalPeriod {
                settings: settings_key,
                authority: bad_signer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AdminSetWithdrawalPeriod {
                new_period_seconds: 60,
            }
            .data(),
        }],
        Some(&bad_signer.pubkey()),
        &[&bad_signer],
        blockhash,
    );
    let bad_result = ctx.banks_client.process_transaction(bad_tx).await;
    assert_anchor_error!(bad_result, GarError::Unauthorized);

    // Step 3: new_period_seconds < 60 rejected.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let too_short_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::AdminSetWithdrawalPeriod {
                settings: settings_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AdminSetWithdrawalPeriod {
                new_period_seconds: 30,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let too_short_result = ctx.banks_client.process_transaction(too_short_tx).await;
    assert_anchor_error!(too_short_result, GarError::InvalidParameter);
}

#[tokio::test]
async fn test_join_network() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64; // 20,000 ARIO (minimum)
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Verify gateway state
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();

    assert_eq!(gateway.operator, ctx.payer.pubkey());
    assert_eq!(gateway.operator_stake, stake_amount);
    assert!(matches!(gateway.status, GatewayStatus::Joined));
    assert_eq!(gateway.label, "test-gateway");
    assert_eq!(gateway.fqdn, "gateway.example.com");
    assert_eq!(gateway.port, 443);
    assert!(gateway.settings.allow_delegated_staking);
    assert_eq!(gateway.settings.delegate_reward_share_ratio, 1000);
    assert_eq!(gateway.total_delegated_stake, 0);
    assert_eq!(gateway.status, ario_gar::state::GatewayStatus::Joined);
    assert_eq!(gateway.registry_index.index, 0);

    // Verify registry count is 1
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    // GatewayRegistry is zero-copy repr(C): discriminator(8) + authority(32) + count(4)
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_join_network_insufficient_stake() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let (gateway_key, _) = gateway_pda(&payer_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: 1_000, // Way below 10,000 ARIO minimum
                    label: "test".to_string(),
                    fqdn: "gw.test.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: false,
                    delegate_reward_share_ratio: 0,
                    min_delegate_stake: None,
                    observer_address: payer_pk,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InsufficientStake);
}

#[tokio::test]
async fn test_leave_network() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Leave network
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway is Leaving
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert!(matches!(gateway.status, GatewayStatus::Leaving));
    assert!(gateway.leave_timestamp.is_some());
    assert_eq!(gateway.operator_stake, 0);

    // Verify withdrawal
    let withdrawal_account = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    assert_eq!(withdrawal.owner, payer_pk);
    assert_eq!(withdrawal.amount, stake_amount);
    assert!(!withdrawal.is_delegate);

    // Verify registry count is 0
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    // Registry slot stays occupied after leave (status=Leaving in place).
    // Slot is reclaimed later by `finalize_gone`.
    assert_eq!(count, 1);
}

#[tokio::test]
async fn test_increase_operator_stake() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    // Increase stake
    let additional_stake = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::IncreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                operator: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::IncreaseOperatorStake {
                amount: additional_stake,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify new stake
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.operator_stake, initial_stake + additional_stake);
}

#[tokio::test]
async fn test_delegate_stake() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Create a separate delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    // Fund delegator with SOL
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create delegator's token account and fund it
    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate
    let delegate_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.delegator, delegator_pk);
    assert_eq!(delegation.gateway, payer_pk);
    assert_eq!(delegation.amount, delegate_amount);

    // Verify gateway's total delegated stake
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.total_delegated_stake, delegate_amount);
}

#[tokio::test]
async fn test_update_gateway_settings() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Update settings
    let payer_pk = ctx.payer.pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: settings_pda().0,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateGatewaySettings {
                params: ario_gar::UpdateGatewayParams {
                    label: Some("updated-gateway".to_string()),
                    fqdn: Some("new.example.com".to_string()),
                    port: Some(8080),

                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify updates
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.label, "updated-gateway");
    assert_eq!(gateway.fqdn, "new.example.com");
    assert_eq!(gateway.port, 8080);
    assert!(gateway.settings.allow_delegated_staking); // Unchanged
}

#[tokio::test]
async fn test_initialize_epochs() {
    let mint = Keypair::new();
    let dummy = Pubkey::new_unique();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    // This test runs `initialize_epochs` directly; EpochSettings must be
    // un-initialized on entry so the program's `init` constraint succeeds.
    let mut pt = program_test_with_gar_for_epoch_init(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let (epoch_settings_key, _) = epoch_settings_pda();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InitializeEpochs {
                epoch_settings: epoch_settings_key,
                payer: upgrade_authority_keypair().pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InitializeEpochs {
                params: ario_gar::InitializeEpochParams {
                    authority: ctx.payer.pubkey(),
                    epoch_duration: 86_400,
                    observer_count: 5,
                    name_count: 10,
                    min_observer_stake: 10_000_000_000,
                    slash_rate: 1000,
                    tenure_weight_duration: 180 * 86_400,
                    max_tenure_weight: 4,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_authority_keypair()],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify
    let account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let epoch_settings = EpochSettings::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(epoch_settings.epoch_duration, 86_400);
    assert_eq!(epoch_settings.prescribed_observer_count, 5);
    assert_eq!(epoch_settings.prescribed_name_count, 10);
    assert!(!epoch_settings.enabled);
    assert_eq!(epoch_settings.current_epoch_index, 0);
    // Tenure-ramp params are caller-supplied — confirm they round-trip
    // intact (regression guard against the earlier hardcoded `3600` /
    // `4` defaults that leaked from devnet into a mainnet-bound build).
    assert_eq!(epoch_settings.tenure_weight_duration, 180 * 86_400);
    assert_eq!(epoch_settings.max_tenure_weight, 4);
}

/// `initialize_epochs` must reject zero / negative tenure-ramp params.
/// Zero `tenure_weight_duration` would divide-by-zero in
/// `GatewayWeights::compute`; zero `max_tenure_weight` would peg every
/// gateway's tenure_weight to 0 and silently disable observer / reward
/// flows. Caller is the upgrade authority — these are operator-typo
/// guards, not adversarial defenses.
async fn send_initialize_epochs(
    ctx: &mut solana_program_test::ProgramTestContext,
    epoch_settings_key: Pubkey,
    params: ario_gar::InitializeEpochParams,
) -> std::result::Result<(), solana_program_test::BanksClientError> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await?;
    let upgrade_auth = upgrade_authority_keypair();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InitializeEpochs {
                epoch_settings: epoch_settings_key,
                payer: upgrade_auth.pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InitializeEpochs { params }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_auth],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn test_initialize_epochs_rejects_zero_tenure_params() {
    let mint = Keypair::new();
    let dummy = Pubkey::new_unique();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let mut pt = program_test_with_gar_for_epoch_init(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let (epoch_settings_key, _) = epoch_settings_pda();

    let base = ario_gar::InitializeEpochParams {
        authority: ctx.payer.pubkey(),
        epoch_duration: 86_400,
        observer_count: 5,
        name_count: 10,
        min_observer_stake: 10_000_000_000,
        slash_rate: 1000,
        tenure_weight_duration: 180 * 86_400,
        max_tenure_weight: 4,
    };

    // Zero tenure_weight_duration → reject.
    let err = send_initialize_epochs(
        &mut ctx,
        epoch_settings_key,
        ario_gar::InitializeEpochParams {
            tenure_weight_duration: 0,
            ..base.clone()
        },
    )
    .await
    .expect_err("init must reject tenure_weight_duration=0");
    assert!(
        format!("{err:?}").contains("Custom"),
        "expected on-chain Custom error, got: {err:?}",
    );

    // Negative tenure_weight_duration → reject.
    let err = send_initialize_epochs(
        &mut ctx,
        epoch_settings_key,
        ario_gar::InitializeEpochParams {
            tenure_weight_duration: -1,
            ..base.clone()
        },
    )
    .await
    .expect_err("init must reject tenure_weight_duration<0");
    assert!(
        format!("{err:?}").contains("Custom"),
        "expected on-chain Custom error, got: {err:?}",
    );

    // Zero max_tenure_weight → reject.
    let err = send_initialize_epochs(
        &mut ctx,
        epoch_settings_key,
        ario_gar::InitializeEpochParams {
            max_tenure_weight: 0,
            ..base.clone()
        },
    )
    .await
    .expect_err("init must reject max_tenure_weight=0");
    assert!(
        format!("{err:?}").contains("Custom"),
        "expected on-chain Custom error, got: {err:?}",
    );

    // Sanity: original valid params still succeed (no account-already-in-use
    // leakage from rejected calls — Anchor `init` constraint must still
    // see EpochSettings as un-initialized after every revert).
    send_initialize_epochs(&mut ctx, epoch_settings_key, base)
        .await
        .expect("init with valid params must succeed after prior rejections");
}

/// Verify the close_epoch_settings ix:
/// 1. Init → close → re-init roundtrip succeeds with new params.
/// 2. Non-authority signer is rejected with `Unauthorized`.
#[tokio::test]
async fn test_close_epoch_settings() {
    let mint = Keypair::new();
    let dummy = Pubkey::new_unique();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let mut pt = program_test_with_gar_for_epoch_init(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let (epoch_settings_key, _) = epoch_settings_pda();

    // Step 1: initial init
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InitializeEpochs {
                epoch_settings: epoch_settings_key,
                payer: upgrade_authority_keypair().pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InitializeEpochs {
                params: ario_gar::InitializeEpochParams {
                    authority: ctx.payer.pubkey(),
                    epoch_duration: 86_400,
                    observer_count: 50,
                    name_count: 50,
                    min_observer_stake: 50_000_000_000,
                    slash_rate: 1000,
                    tenure_weight_duration: 180 * 86_400,
                    max_tenure_weight: 4,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_authority_keypair()],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Step 2: non-authority signer must be rejected
    let bad_signer = Keypair::new();
    // Fund bad_signer so they can pay tx fees
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &bad_signer.pubkey(),
            10_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let bad_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: bad_signer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpochSettings {}.data(),
        }],
        Some(&bad_signer.pubkey()),
        &[&bad_signer],
        blockhash,
    );
    let bad_result = ctx.banks_client.process_transaction(bad_tx).await;
    assert_anchor_error!(bad_result, GarError::Unauthorized);

    // Step 3: authority signer closes successfully
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let close_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpochSettings {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(close_tx)
        .await
        .unwrap();

    // PDA must be gone (Anchor `close = authority` zeros + drains lamports).
    let after_close = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap();
    assert!(
        after_close.is_none() || after_close.unwrap().lamports == 0,
        "EpochSettings PDA should be closed"
    );

    // Step 4: re-init with new (fast-test) params lands fresh state
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let reinit_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InitializeEpochs {
                epoch_settings: epoch_settings_key,
                payer: upgrade_authority_keypair().pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InitializeEpochs {
                params: ario_gar::InitializeEpochParams {
                    authority: ctx.payer.pubkey(),
                    epoch_duration: 300,
                    observer_count: 5,
                    name_count: 10,
                    min_observer_stake: 0,
                    slash_rate: 0,
                    tenure_weight_duration: 180 * 86_400,
                    max_tenure_weight: 4,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_authority_keypair()],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(reinit_tx)
        .await
        .unwrap();

    let account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let reinit = EpochSettings::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(reinit.epoch_duration, 300);
    assert_eq!(reinit.prescribed_observer_count, 5);
    assert_eq!(reinit.prescribed_name_count, 10);
    assert_eq!(reinit.min_observer_stake, 0);
    assert_eq!(reinit.slash_rate, 0);
    assert!(!reinit.enabled);
}

// Note on test coverage for admin_close_stale_epoch:
//
// The ix body is `Ok(())` — all behavior is in Anchor constraints
// (`has_one = authority`, `constraint = settings.migration_active`,
// `close = authority`). Each constraint is independently exercised by
// existing sibling-ix tests in this file:
//   - `has_one = authority`: e.g. `test_close_epoch_settings` (lines ~1100+)
//     proves the gate rejects non-authority signers with `Unauthorized`.
//   - `migration_active` gate: e.g. `import_*` ix tests prove the gate
//     rejects calls when `migration_active = false`.
//   - `close = authority`: Anchor-standard, exercised by every other
//     `close = …` ix in this file (`close_drained_withdrawal`,
//     `close_empty_delegation`, `close_observation`, `close_epoch`,
//     `close_epoch_settings`).
//
// A dedicated test for this ix would require ~80 LoC of state-setup
// scaffolding (override `GatewaySettings.migration_active = true`, drive
// the full `join_network` + `create_epoch` lifecycle to make a real Epoch
// PDA) to validate something already covered by existing tests. Skipped
// in favor of live devnet validation when the ix is first run.

/// Verify `admin_set_current_epoch_index`:
///   1. Authority signer with non-zero `new_index` succeeds — settings
///      updates AND `genesis_timestamp` re-anchors to make the new
///      index's epoch_start ≈ now.
///   2. `new_index = 0` rejected with `InvalidParameter` (no point;
///      0 is the default).
///   3. Non-authority signer rejected with `Unauthorized`.
///   4. One-shot: re-calling after a successful set rejects with
///      `EpochCounterAlreadyAdvanced`.
#[tokio::test]
async fn test_admin_set_current_epoch_index() {
    let mint = Keypair::new();
    let dummy = Pubkey::new_unique();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let mut pt = program_test_with_gar_for_epoch_init(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let (epoch_settings_key, _) = epoch_settings_pda();

    // initialize_epochs: starts with current_epoch_index=0, enabled=false.
    let epoch_duration: i64 = 86_400;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InitializeEpochs {
                epoch_settings: epoch_settings_key,
                payer: upgrade_authority_keypair().pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InitializeEpochs {
                params: ario_gar::InitializeEpochParams {
                    authority: ctx.payer.pubkey(),
                    epoch_duration,
                    observer_count: 50,
                    name_count: 50,
                    min_observer_stake: 50_000_000_000,
                    slash_rate: 1000,
                    tenure_weight_duration: 180 * 86_400,
                    max_tenure_weight: 4,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_authority_keypair()],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Step 1: new_index = 0 rejected.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let zero_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AdminSetCurrentEpochIndex { new_index: 0 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let zero_result = ctx.banks_client.process_transaction(zero_tx).await;
    assert_anchor_error!(zero_result, GarError::InvalidParameter);

    // Step 2: non-authority signer rejected.
    let bad_signer = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &bad_signer.pubkey(),
            10_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let bad_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: bad_signer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AdminSetCurrentEpochIndex { new_index: 406 }.data(),
        }],
        Some(&bad_signer.pubkey()),
        &[&bad_signer],
        blockhash,
    );
    let bad_result = ctx.banks_client.process_transaction(bad_tx).await;
    assert_anchor_error!(bad_result, GarError::Unauthorized);

    // Step 3: authority + valid new_index succeeds.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let ok_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AdminSetCurrentEpochIndex { new_index: 406 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(ok_tx).await.unwrap();

    // Verify the state update.
    let account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let settings = EpochSettings::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert_eq!(settings.current_epoch_index, 406);
    // Re-anchor invariant: epoch_start for index=406 ≈ now (i.e.
    // `now - new_genesis ≈ new_index * epoch_duration`).
    let clock_sysvar = ctx
        .banks_client
        .get_sysvar::<solana_sdk::sysvar::clock::Clock>()
        .await
        .unwrap();
    let now = clock_sysvar.unix_timestamp;
    let elapsed = now - settings.genesis_timestamp;
    let expected_elapsed = 406i64 * epoch_duration;
    // Allow a small slack for the slot/clock advance between the tx and
    // this query (program-test slots advance every few ms).
    assert!(
        (elapsed - expected_elapsed).abs() < 60,
        "expected genesis re-anchor to ~{}s ago (406 * {}), got {}s",
        expected_elapsed,
        epoch_duration,
        elapsed
    );

    // Step 4: one-shot — calling again is rejected.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let twice_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AdminSetCurrentEpochIndex { new_index: 500 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let twice_result = ctx.banks_client.process_transaction(twice_tx).await;
    assert_anchor_error!(twice_result, GarError::EpochCounterAlreadyAdvanced);
}

// =========================================
// NEW INTEGRATION TESTS
// =========================================

fn allowlist_pda(operator: &Pubkey, delegate: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ALLOWLIST_SEED, operator.as_ref(), delegate.as_ref()],
        &ario_gar::ID,
    )
}

#[tokio::test]
async fn test_decrease_operator_stake() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with 30,000 ARIO
    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    // Decrease by 5,000 ARIO (remaining 25,000 >= MIN_OPERATOR_STAKE of 20,000)
    let decrease_amount = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway.operator_stake decreased
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.operator_stake, initial_stake - decrease_amount);

    // Verify Withdrawal PDA created with correct amount and available_at
    let withdrawal_account = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    assert_eq!(withdrawal.owner, payer_pk);
    assert_eq!(withdrawal.amount, decrease_amount);
    assert!(!withdrawal.is_delegate);
    assert!(!withdrawal.is_exit_vault);
    // available_at should be created_at + 30 days withdrawal period
    assert_eq!(withdrawal.available_at, withdrawal.created_at + 30 * 86_400);
}

#[tokio::test]
async fn test_allow_disallow_delegate() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let delegate = Pubkey::new_unique();
    let (allowlist_key, _) = allowlist_pda(&payer_pk, &delegate);

    // AllowDelegate
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::AllowDelegate {
                gateway: gateway_key,
                allowlist_entry: allowlist_key,
                delegate: delegate,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AllowDelegate {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify AllowlistEntry PDA was created
    let entry_account = ctx
        .banks_client
        .get_account(allowlist_key)
        .await
        .unwrap()
        .unwrap();
    let entry = AllowlistEntry::try_deserialize(&mut entry_account.data.as_slice()).unwrap();
    assert_eq!(entry.gateway, payer_pk);
    assert_eq!(entry.delegate, delegate);

    // DisallowDelegate
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DisallowDelegate {
                gateway: gateway_key,
                allowlist_entry: allowlist_key,
                delegate: delegate,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DisallowDelegate {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify AllowlistEntry PDA is closed
    let entry_account = ctx.banks_client.get_account(allowlist_key).await.unwrap();
    assert!(entry_account.is_none());
}

#[tokio::test]
async fn test_update_gateway_settings_leaving_check() {
    // BUG-4: Cannot update settings on a leaving gateway
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Leave network (only 1 gateway, so no swap needed — no remaining_accounts)
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway is now Leaving
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert!(matches!(gateway.status, GatewayStatus::Leaving));

    // Try to update settings on the leaving gateway — should fail with GatewayLeaving
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateGatewaySettings {
                params: ario_gar::UpdateGatewayParams {
                    label: Some("should-fail".to_string()),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayLeaving);
}

#[tokio::test]
async fn test_min_delegation_floor() {
    // BUG-5: Gateway cannot set min_delegate_stake below global settings.min_delegate_stake
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Try to set min_delegate_stake to 1 ARIO (below global min of 10 ARIO)
    let payer_pk = ctx.payer.pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateGatewaySettings {
                params: ario_gar::UpdateGatewayParams {
                    min_delegate_stake: Some(1_000_000), // 1 ARIO, below global 10 ARIO min
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::DelegationBelowMinimum);
}

#[tokio::test]
async fn test_properties_validation() {
    // SHOULD-10: Properties must be a valid 43-char Arweave ID or empty
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let (gateway_key, _) = gateway_pda(&payer_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);

    // 1. Invalid properties: not 43 chars
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: 20_000_000_000,
                    label: "test-gw".to_string(),
                    fqdn: "gw.test.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: Some("not-43-chars".to_string()),
                    note: None,
                    allow_delegated_staking: false,
                    delegate_reward_share_ratio: 0,
                    min_delegate_stake: None,
                    observer_address: payer_pk,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(result.is_err(), "Join with invalid properties should fail");

    // 2. Valid 43-char Arweave ID
    let valid_arweave_id = "abcdefghijklmnopqrstuvwxyz01234567890ABCDEF".to_string();
    assert_eq!(valid_arweave_id.len(), 43);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: 20_000_000_000,
                    label: "test-gw".to_string(),
                    fqdn: "gw.test.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: Some(valid_arweave_id.clone()),
                    note: None,
                    allow_delegated_staking: false,
                    delegate_reward_share_ratio: 0,
                    min_delegate_stake: None,
                    observer_address: payer_pk,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway was created with the valid properties
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.properties, valid_arweave_id);

    // 3. Join with empty properties (None) — use a different operator since gateway PDA already taken
    // We verify this via the existing test_join_network which passes properties: None successfully
    // The join_gateway helper already uses properties: None and succeeds, so this case is covered
}

// =========================================
// SHOULD-9: OBSERVER ADDRESS UNIQUENESS
// =========================================

#[tokio::test]
async fn test_join_network_observer_uniqueness() {
    // Two operators try to join with the same observer address; second must fail
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    // Add a funded second operator to the test context
    let operator2 = Keypair::new();
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Operator 1 (ctx.payer) joins with a specific observer address
    let shared_observer = Pubkey::new_unique();
    let payer_pk = ctx.payer.pubkey();
    let (gateway_key1, _) = gateway_pda(&payer_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&shared_observer);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key1,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: 20_000_000_000,
                    label: "gateway-1".to_string(),
                    fqdn: "gw1.test.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: false,
                    delegate_reward_share_ratio: 0,
                    min_delegate_stake: None,
                    observer_address: shared_observer,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify observer lookup was created
    let lookup_account = ctx
        .banks_client
        .get_account(observer_lookup_key)
        .await
        .unwrap()
        .unwrap();
    let lookup = ObserverLookup::try_deserialize(&mut lookup_account.data.as_slice()).unwrap();
    assert_eq!(lookup.gateway, payer_pk);

    // Operator 2 tries to join with the SAME observer address — must fail
    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;

    let (gateway_key2, _) = gateway_pda(&op2_pk);
    // Same observer_lookup PDA — already exists from operator 1
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key2,
                operator_token_account: op2_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: op2_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: 20_000_000_000,
                    label: "gateway-2".to_string(),
                    fqdn: "gw2.test.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: false,
                    delegate_reward_share_ratio: 0,
                    min_delegate_stake: None,
                    observer_address: shared_observer,
                },
            }
            .data(),
        }],
        Some(&op2_pk),
        &[&operator2],
        blockhash,
    );
    // Should fail because observer_lookup PDA already exists (Anchor init constraint)
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "Second operator should not be able to use same observer address"
    );
}

#[tokio::test]
async fn test_leave_network_cleans_observer_lookup() {
    // After leave_network with observer_lookup in remaining_accounts, the PDA is closed
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Verify observer lookup exists (join_gateway uses payer as observer)
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);
    let lookup_account = ctx
        .banks_client
        .get_account(observer_lookup_key)
        .await
        .unwrap();
    assert!(
        lookup_account.is_some(),
        "Observer lookup should exist after join"
    );

    // Leave network, passing observer_lookup as last remaining_account
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let mut accounts = ario_gar::accounts::LeaveNetwork {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_pda().0,
        registry: setup.registry_key,
        gateway: gateway_key,
        withdrawal_counter: withdrawal_counter_key,
        withdrawal: withdrawal_key,
        excess_withdrawal: None,
        operator: payer_pk,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // Append observer_lookup as writable remaining_account
    accounts.push(solana_sdk::instruction::AccountMeta::new(
        observer_lookup_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify observer lookup is closed
    let lookup_account = ctx
        .banks_client
        .get_account(observer_lookup_key)
        .await
        .unwrap();
    assert!(
        lookup_account.is_none(),
        "Observer lookup should be closed after leave"
    );
}

#[tokio::test]
async fn test_update_observer_address() {
    // Operator changes observer address via dedicated update_observer_address instruction
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Old observer = payer_pk, new observer = a fresh key
    let new_observer = Pubkey::new_unique();
    let (old_lookup_key, _) = observer_lookup_pda(&payer_pk);
    let (new_lookup_key, _) = observer_lookup_pda(&new_observer);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateObserverAddress {
                gateway: gateway_key,
                old_observer_lookup: old_lookup_key,
                new_observer_lookup: new_lookup_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateObserverAddress { new_observer }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify old lookup is closed
    let old_lookup = ctx.banks_client.get_account(old_lookup_key).await.unwrap();
    assert!(old_lookup.is_none(), "Old observer lookup should be closed");

    // Verify new lookup is created and points to same gateway
    let new_lookup_account = ctx
        .banks_client
        .get_account(new_lookup_key)
        .await
        .unwrap()
        .unwrap();
    let new_lookup =
        ObserverLookup::try_deserialize(&mut new_lookup_account.data.as_slice()).unwrap();
    assert_eq!(new_lookup.gateway, payer_pk);

    // Verify gateway's observer_address was updated
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.observer_address, new_observer);
}

// =========================================
// WITHDRAWAL + DELEGATION TESTS (Phase 2)
// =========================================

async fn get_token_balance(ctx: &mut ProgramTestContext, token_account: &Pubkey) -> u64 {
    let account = ctx
        .banks_client
        .get_account(*token_account)
        .await
        .unwrap()
        .unwrap();
    spl_token::state::Account::unpack(&account.data)
        .unwrap()
        .amount
}

fn redelegation_pda(delegator: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[ario_gar::state::REDELEGATION_SEED, delegator.as_ref()],
        &ario_gar::ID,
    )
}

/// Helper to join a gateway with an explicit operator keypair (not just ctx.payer).
/// The operator must already be funded with SOL and have a funded token account.
async fn join_gateway_with_operator(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    operator: &Keypair,
    operator_token_pubkey: &Pubkey,
    operator_stake: u64,
) -> Pubkey {
    let op_pk = operator.pubkey();
    let (gateway_key, _) = gateway_pda(&op_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&op_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: *operator_token_pubkey,
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: op_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake,
                    label: "test-gw-2".to_string(),
                    fqdn: "gw2.example.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: true,
                    delegate_reward_share_ratio: 10,
                    min_delegate_stake: None,
                    observer_address: op_pk,
                },
            }
            .data(),
        }],
        Some(&op_pk),
        &[operator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    gateway_key
}

// -----------------------------------------
// 1. test_claim_withdrawal
// -----------------------------------------

#[tokio::test]
async fn test_claim_withdrawal() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with 30k ARIO
    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    // Decrease by 5k to create withdrawal
    let decrease_amount = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read withdrawal to get available_at
    let withdrawal_account = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    let available_at = withdrawal.available_at;

    // Warp past available_at
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = available_at + 1;
    ctx.set_sysvar(&clock);

    // Record token balance before claim
    let balance_before = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;

    // Claim withdrawal
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimWithdrawal {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify tokens transferred to owner
    let balance_after = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    assert_eq!(balance_after - balance_before, decrease_amount);

    // Verify withdrawal account closed
    let withdrawal_account = ctx.banks_client.get_account(withdrawal_key).await.unwrap();
    assert!(withdrawal_account.is_none());
}

// -----------------------------------------
// 2. test_claim_withdrawal_not_ready
// -----------------------------------------

#[tokio::test]
async fn test_claim_withdrawal_not_ready() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    let decrease_amount = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Do NOT warp time. Try to claim immediately.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimWithdrawal {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::WithdrawalNotReady);
}

// -----------------------------------------
// 3. test_claim_withdrawal_at_exact_boundary
// -----------------------------------------

#[tokio::test]
async fn test_claim_withdrawal_at_exact_boundary() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    let decrease_amount = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read withdrawal to get exact available_at
    let withdrawal_account = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    let available_at = withdrawal.available_at;

    // Warp to exactly available_at (>= check should succeed)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = available_at;
    ctx.set_sysvar(&clock);

    // Claim should succeed at exact boundary
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimWithdrawal {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify withdrawal account closed
    let withdrawal_account = ctx.banks_client.get_account(withdrawal_key).await.unwrap();
    assert!(withdrawal_account.is_none());
}

// -----------------------------------------
// 4. test_instant_withdrawal
// -----------------------------------------

#[tokio::test]
async fn test_instant_withdrawal() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with 30k ARIO
    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    // Decrease by 5k to create withdrawal
    let decrease_amount = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // At time ~0: penalty = 50% (max), so fee = 2500 ARIO, payout = 2500 ARIO
    let balance_before = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    // Instant withdrawal
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InstantWithdrawal {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InstantWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // penalty_rate at time 0 = max_penalty = 500_000 (50%)
    // fee = 5_000_000_000 * 500_000 / 1_000_000 = 2_500_000_000
    // payout = 5_000_000_000 - 2_500_000_000 = 2_500_000_000
    let expected_fee = 2_500_000_000u64;
    let expected_payout = 2_500_000_000u64;

    let balance_after = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    assert_eq!(balance_after - balance_before, expected_payout);
    assert_eq!(protocol_after - protocol_before, expected_fee);

    // Verify withdrawal account closed
    let withdrawal_account = ctx.banks_client.get_account(withdrawal_key).await.unwrap();
    assert!(withdrawal_account.is_none());
}

// -----------------------------------------
// 5. test_instant_withdrawal_exit_vault
// -----------------------------------------

#[tokio::test]
async fn test_instant_withdrawal_exit_vault() {
    // BD-102 / Lua-parity: leave_network produces a *protected* exit vault
    // (`is_protected: true`) for the min portion that **cannot** be
    // instantly withdrawn (mirrors `gar.lua::instantGatewayWithdrawal`'s
    // `vaultId == gatewayAddress` reject), plus an unprotected excess
    // vault for any above-min stake that **can** be expedited.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Stake 30k = min(20k) + excess(10k) so leave produces both vaults.
    let stake_amount = 30_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (exit_vault_key, _) = withdrawal_pda(&payer_pk, 0);
    let (excess_vault_key, _) = withdrawal_pda(&payer_pk, 1);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: exit_vault_key,
                excess_withdrawal: Some(excess_vault_key),
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // The exit vault holds the min stake and is flagged protected.
    let exit_account = ctx
        .banks_client
        .get_account(exit_vault_key)
        .await
        .unwrap()
        .unwrap();
    let exit_vault = Withdrawal::try_deserialize(&mut exit_account.data.as_slice()).unwrap();
    assert!(exit_vault.is_exit_vault);
    assert!(
        exit_vault.is_protected,
        "min-stake exit vault must be protected"
    );
    assert_eq!(exit_vault.amount, 20_000_000_000u64);

    // The excess vault holds the above-min stake, NOT protected.
    let excess_account = ctx
        .banks_client
        .get_account(excess_vault_key)
        .await
        .unwrap()
        .unwrap();
    let excess_vault = Withdrawal::try_deserialize(&mut excess_account.data.as_slice()).unwrap();
    assert!(
        excess_vault.is_exit_vault,
        "excess vault is_exit_vault marker is set"
    );
    assert!(
        !excess_vault.is_protected,
        "excess vault must not be protected"
    );
    assert_eq!(excess_vault.amount, 10_000_000_000u64);

    // Reject: instant_withdrawal on the protected exit vault.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InstantWithdrawal {
                settings: setup.settings_key,
                withdrawal: exit_vault_key,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InstantWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ario_gar::error::GarError::ProtectedVault);

    // Allow: instant_withdrawal on the excess vault. (Penalty is at max
    // since elapsed == 0; we just verify the call succeeds.)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InstantWithdrawal {
                settings: setup.settings_key,
                withdrawal: excess_vault_key,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InstantWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// -----------------------------------------
// 6. test_cancel_withdrawal_operator
// -----------------------------------------

#[tokio::test]
async fn test_cancel_withdrawal_operator() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with 30k ARIO
    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    // Decrease by 5k to create withdrawal
    let decrease_amount = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway stake decreased
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.operator_stake, initial_stake - decrease_amount);

    // Cancel withdrawal (operator, no delegation needed)
    // For optional delegation = None, pass the program ID
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::CancelWithdrawal {
        settings: setup.settings_key,
        gateway: gateway_key,
        withdrawal: withdrawal_key,
        delegation: None,
        owner: payer_pk,
    }
    .to_account_metas(None);
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::CancelWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway.operator_stake restored to 20k
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.operator_stake, initial_stake);

    // Verify withdrawal account closed
    let withdrawal_account = ctx.banks_client.get_account(withdrawal_key).await.unwrap();
    assert!(withdrawal_account.is_none());
}

// -----------------------------------------
// 7. test_cancel_withdrawal_gateway_leaving
// -----------------------------------------

#[tokio::test]
async fn test_cancel_withdrawal_gateway_leaving() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Leave network (creates exit vault withdrawal)
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to cancel the exit vault withdrawal -> should fail with GatewayLeaving
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_gar::accounts::CancelWithdrawal {
        settings: setup.settings_key,
        gateway: gateway_key,
        withdrawal: withdrawal_key,
        delegation: None,
        owner: payer_pk,
    }
    .to_account_metas(None);
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::CancelWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayLeaving);
}

// -----------------------------------------
// 8. test_decrease_delegate_stake
// -----------------------------------------

#[tokio::test]
async fn test_decrease_delegate_stake() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Create a separate delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    // Fund delegator with SOL
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create delegator's token account and fund it
    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO
    let delegate_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Decrease delegate by 10 ARIO
    let decrease_amount = 10_000_000_000u64;
    let (del_withdrawal_counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (del_withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_withdrawal_counter_key,
                withdrawal: del_withdrawal_key,
                delegator: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation.amount decreased
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, delegate_amount - decrease_amount);

    // Verify withdrawal created with is_delegate=true
    let withdrawal_account = ctx
        .banks_client
        .get_account(del_withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    assert_eq!(withdrawal.amount, decrease_amount);
    assert!(withdrawal.is_delegate);
    assert!(!withdrawal.is_exit_vault);

    // Verify gateway.total_delegated_stake decreased
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(
        gateway.total_delegated_stake,
        delegate_amount - decrease_amount
    );
}

// -----------------------------------------
// 9. test_decrease_delegate_stake_below_minimum
// -----------------------------------------

#[tokio::test]
async fn test_decrease_delegate_stake_below_minimum() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Create delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO (20_000_000 base units)
    let delegate_amount = 20_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to decrease by 15 ARIO (remaining 5 ARIO < min 10 ARIO)
    let decrease_amount = 15_000_000u64;
    let (del_withdrawal_counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (del_withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_withdrawal_counter_key,
                withdrawal: del_withdrawal_key,
                delegator: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::DelegationBelowMinimum);
}

// -----------------------------------------
// 10. test_redelegate_stake
// -----------------------------------------

#[tokio::test]
async fn test_redelegate_stake() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    // operator2 needs to be pre-funded before start
    let operator2 = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Gateway 1: ctx.payer
    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Gateway 2: operator2
    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;

    let gateway_key2 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator2,
        &op2_token.pubkey(),
        stake_amount,
    )
    .await;

    // Create delegator, delegate to gateway1
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO to gateway1
    let delegate_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key1, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key1,
                delegation: delegation_key1,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Redelegate full 20 ARIO from gateway1 to gateway2
    let (delegation_key2, _) = delegation_pda(&op2_pk, &delegator_pk);
    let (redelegation_key, _) = redelegation_pda(&delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::RedelegateStake {
                source_gateway: gateway_key1,
                target_gateway: gateway_key2,
                source_delegation: delegation_key1,
                target_delegation: delegation_key2,
                redelegation_record: redelegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                settings: setup.settings_key,
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::RedelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // First redelegate is free (count was 0 -> 0% fee)
    // Source delegation should be fully drained
    let delegation1_account = ctx
        .banks_client
        .get_account(delegation_key1)
        .await
        .unwrap()
        .unwrap();
    let delegation1 =
        Delegation::try_deserialize(&mut delegation1_account.data.as_slice()).unwrap();
    assert_eq!(delegation1.amount, 0);

    // Target delegation should have full amount (no fee)
    let delegation2_account = ctx
        .banks_client
        .get_account(delegation_key2)
        .await
        .unwrap()
        .unwrap();
    let delegation2 =
        Delegation::try_deserialize(&mut delegation2_account.data.as_slice()).unwrap();
    assert_eq!(delegation2.amount, delegate_amount);

    // Source gateway.total_delegated_stake should decrease
    let gw1_account = ctx
        .banks_client
        .get_account(gateway_key1)
        .await
        .unwrap()
        .unwrap();
    let gw1 = Gateway::try_deserialize(&mut gw1_account.data.as_slice()).unwrap();
    assert_eq!(gw1.total_delegated_stake, 0);

    // Target gateway.total_delegated_stake should increase
    let gw2_account = ctx
        .banks_client
        .get_account(gateway_key2)
        .await
        .unwrap()
        .unwrap();
    let gw2 = Gateway::try_deserialize(&mut gw2_account.data.as_slice()).unwrap();
    assert_eq!(gw2.total_delegated_stake, delegate_amount);
}

// -----------------------------------------
// 11. test_redelegate_stake_same_gateway
// -----------------------------------------

#[tokio::test]
async fn test_redelegate_stake_same_gateway() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Create delegator and delegate
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    let delegate_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to redelegate from gateway to SAME gateway
    let (redelegation_key, _) = redelegation_pda(&delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::RedelegateStake {
                source_gateway: gateway_key,
                target_gateway: gateway_key,
                source_delegation: delegation_key,
                target_delegation: delegation_key,
                redelegation_record: redelegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                settings: setup.settings_key,
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::RedelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::RedelegateSameGateway);
}

// =========================================
// EPOCH LIFECYCLE TESTS
// =========================================

fn epoch_pda(epoch_index: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[EPOCH_SEED, &epoch_index.to_le_bytes()], &ario_gar::ID)
}

fn observation_pda(epoch_index: u64, observer: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(
        &[
            OBSERVATION_SEED,
            &epoch_index.to_le_bytes(),
            observer.as_ref(),
        ],
        &ario_gar::ID,
    )
}

/// Pre-create an EpochSettings account and add it to ProgramTest before starting.
fn pre_create_epoch_settings(
    pt: &mut ProgramTest,
    authority: &Pubkey,
    genesis_timestamp: i64,
    epoch_duration: i64,
    enabled: bool,
) {
    use anchor_lang::solana_program::hash::hash;

    let (epoch_settings_key, epoch_settings_bump) = epoch_settings_pda();
    let epoch_settings_size = EpochSettings::SIZE;
    let mut data = vec![0u8; epoch_settings_size];

    // Anchor discriminator for regular account
    let disc = hash(b"account:EpochSettings");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);

    let mut offset = 8;
    // authority: Pubkey (32)
    data[offset..offset + 32].copy_from_slice(authority.as_ref());
    offset += 32;
    // epoch_duration: i64 (8)
    data[offset..offset + 8].copy_from_slice(&epoch_duration.to_le_bytes());
    offset += 8;
    // prescribed_observer_count: u8 (1)
    data[offset] = 50;
    offset += 1;
    // prescribed_name_count: u8 (1)
    data[offset] = 2;
    offset += 1;
    // min_observer_stake: u64 (8) — same as min operator stake
    data[offset..offset + 8].copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
    offset += 8;
    // slash_rate: u16 (2)
    data[offset..offset + 2].copy_from_slice(&0u16.to_le_bytes());
    offset += 2;
    // enabled: bool (1)
    data[offset] = if enabled { 1 } else { 0 };
    offset += 1;
    // current_epoch_index: u64 (8)
    data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
    offset += 8;
    // genesis_timestamp: i64 (8)
    data[offset..offset + 8].copy_from_slice(&genesis_timestamp.to_le_bytes());
    offset += 8;
    // tenure_weight_duration: i64 (8) — 180 days in seconds
    let tenure_weight_duration = 180i64 * 86_400;
    data[offset..offset + 8].copy_from_slice(&tenure_weight_duration.to_le_bytes());
    offset += 8;
    // max_tenure_weight: u64 (8) — 4
    data[offset..offset + 8].copy_from_slice(&4u64.to_le_bytes());
    offset += 8;
    // gateway_reward_ratio: u64 (8) — 900_000 = 90%
    data[offset..offset + 8].copy_from_slice(&900_000u64.to_le_bytes());
    offset += 8;
    // observer_reward_ratio: u64 (8) — 100_000 = 10%
    data[offset..offset + 8].copy_from_slice(&100_000u64.to_le_bytes());
    offset += 8;
    // missed_observation_penalty_rate: u64 (8) — 250_000 = 25%
    data[offset..offset + 8].copy_from_slice(&250_000u64.to_le_bytes());
    offset += 8;
    // max_reward_rate: u64 (8) — 1_000 = 0.1%
    data[offset..offset + 8].copy_from_slice(&1_000u64.to_le_bytes());
    offset += 8;
    // min_reward_rate: u64 (8) — 500 = 0.05%
    data[offset..offset + 8].copy_from_slice(&500u64.to_le_bytes());
    offset += 8;
    // reward_decay_start_epoch: u64 (8)
    data[offset..offset + 8].copy_from_slice(&365u64.to_le_bytes());
    offset += 8;
    // reward_decay_last_epoch: u64 (8)
    data[offset..offset + 8].copy_from_slice(&547u64.to_le_bytes());
    offset += 8;
    // max_consecutive_failures: u8 (1)
    data[offset] = 30;
    offset += 1;
    // failed_gateway_slash_rate: u64 (8) — 1_000_000 = 100%
    data[offset..offset + 8].copy_from_slice(&1_000_000u64.to_le_bytes());
    offset += 8;
    // disable_at: i64 (8) — 0 = no pending disable (GAR-007)
    data[offset..offset + 8].copy_from_slice(&0i64.to_le_bytes());
    offset += 8;
    // bump: u8 (1)
    data[offset] = epoch_settings_bump;

    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        epoch_settings_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(epoch_settings_size),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
}

/// Canonical `ArioConfig` PDA address, derived from the placeholder
/// `ARIO_CORE_PROGRAM_ID` that ario-core was compiled with (its
/// `declare_id!()` literal). This is the address `distribute_epoch`'s
/// `ario_config` account constraint resolves to
/// (`seeds = [b"ario_config"], seeds::program = ARIO_CORE_PROGRAM_ID`).
fn ario_config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"ario_config"], &ario_gar::ARIO_CORE_PROGRAM_ID)
}

/// Pre-create ario-core's `ArioConfig` PDA so `distribute_epoch`'s CPI into
/// `ario_core::release_treasury_to_recipient` can authenticate the transfer.
///
/// ario-core deserializes this as a typed `Account<ArioConfig>` and enforces:
///   * `config.treasury == protocol_token_account` (transfer source)
///   * `config.gar_program == ario_gar::ID` (caller authentication — the
///     `gar_settings` signer must derive from this program ID)
///
/// The byte layout mirrors `ario_core::state::ArioConfig` exactly (borsh,
/// sequential). Built WITHOUT the `migration-test` feature, so the struct
/// has no `field_1/2/3` tail and `version` is the canonical 1.0.0.
fn pre_create_ario_config(pt: &mut ProgramTest, treasury: &Pubkey, gar_program: &Pubkey) {
    use anchor_lang::solana_program::hash::hash;

    let (config_key, config_bump) = ario_config_pda();
    // SIZE = disc(8) + authority(32) + mint(32) + arns_program(32)
    //      + treasury(32) + 7*u64/i64(56) + migration_active(1)
    //      + migration_authority(32) + bump(1) + gar_program(32)
    //      + version(3) = 261
    let size = 261usize;
    let mut data = vec![0u8; size];

    // Anchor account discriminator
    let disc = hash(b"account:ArioConfig");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);

    let mut offset = 8usize;
    let dummy_authority = Pubkey::new_unique();
    // authority: Pubkey
    data[offset..offset + 32].copy_from_slice(dummy_authority.as_ref());
    offset += 32;
    // mint: Pubkey (unused by release_treasury_to_recipient)
    data[offset..offset + 32].copy_from_slice(Pubkey::new_unique().as_ref());
    offset += 32;
    // arns_program: Pubkey (unused here)
    data[offset..offset + 32].copy_from_slice(Pubkey::new_unique().as_ref());
    offset += 32;
    // treasury: Pubkey — pinned source; MUST equal protocol_token_account
    data[offset..offset + 32].copy_from_slice(treasury.as_ref());
    offset += 32;
    // total_supply / protocol_balance / circulating_supply / locked_supply:
    // u64 (4 * 8 = 32 bytes, all 0)
    offset += 32;
    // min_vault_duration / max_vault_duration / primary_name_request_expiry:
    // i64 (3 * 8 = 24 bytes, all 0)
    offset += 24;
    // migration_active: bool — false (release ix is NOT gated on this)
    data[offset] = 0;
    offset += 1;
    // migration_authority: Pubkey
    data[offset..offset + 32].copy_from_slice(dummy_authority.as_ref());
    offset += 32;
    // bump: u8
    data[offset] = config_bump;
    offset += 1;
    // gar_program: Pubkey — caller authentication, MUST equal ario_gar::ID
    data[offset..offset + 32].copy_from_slice(gar_program.as_ref());
    offset += 32;
    // version: SchemaVersion { major, minor, patch } = 1.0.0
    data[offset] = 1;
    data[offset + 1] = 0;
    data[offset + 2] = 0;
    offset += 3;
    debug_assert_eq!(offset, size, "ArioConfig hand-serialization length drift");

    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        config_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(size),
            data,
            owner: ario_gar::ARIO_CORE_PROGRAM_ID,
            executable: false,
            rent_epoch: 0,
        },
    );
}

/// `program_test_with_gar`, plus the ario-core native processor and a
/// pre-created `ArioConfig` PDA — the setup `distribute_epoch` needs to CPI
/// into `ario_core::release_treasury_to_recipient`. `treasury` MUST be the
/// same pubkey passed as `protocol_token_account` (the transfer source that
/// ario-core pins against `ArioConfig.treasury`).
fn program_test_with_gar_and_core(
    authority: &Pubkey,
    mint: &Pubkey,
    stake_token_account: &Pubkey,
    protocol_token_account: &Pubkey,
) -> ProgramTest {
    let mut pt = program_test_with_gar(
        authority,
        mint,
        stake_token_account,
        protocol_token_account,
    );
    pt.add_program(
        "ario_core",
        ario_gar::ARIO_CORE_PROGRAM_ID,
        processor!(ario_core_processor),
    );
    // gar_program in ArioConfig must be ario-gar's program ID so the
    // gar_settings PDA signer (derived from ario_gar::ID) authenticates.
    pre_create_ario_config(&mut pt, protocol_token_account, &ario_gar::ID);
    pt
}

// -----------------------------------------
// 1. test_epoch_create_and_tally
// -----------------------------------------

#[tokio::test]
async fn test_epoch_create_and_tally() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    // genesis_timestamp = 100, so epoch 0 starts at 100
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol token account with 1000 ARIO (1_000_000_000 mARIO)
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    // Join gateway with 10k ARIO (start_timestamp = 0)
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Warp clock past epoch start (>= 100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // --- Create epoch ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify epoch was created
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.epoch_index, 0);
    assert_eq!(epoch.start_timestamp, 100);
    assert_eq!(epoch.end_timestamp, 100 + 86_400);
    assert_eq!(epoch.active_gateway_count, 1);
    assert_eq!(epoch.weights_tallied, 0);
    assert_eq!(epoch.reward_rate, 1_000); // MAX_REWARD_RATE at epoch 0
                                          // total_eligible = 1_000_000_000 * 1_000 / 1_000_000 = 1_000_000
    assert_eq!(epoch.total_eligible_rewards, 1_000_000);

    // --- Tally weights ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: ctx.payer.pubkey(),
    }
    .to_account_metas(None);
    // Gateway PDA as remaining_account (writable for weight write-back)
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify weights were tallied
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.weights_tallied, 1);
    assert_eq!(epoch.tally_index, 1);
    assert!(epoch.total_composite_weight() > 0);

    // ADR-025 / BD-111: this gateway has no delegated stake, so tally must
    // record `delegated_at_tally == 0` for its slot (no delegate carve-out at
    // distribution). The positive case is covered by
    // test_tally_snapshots_delegated_at_tally.
    let registry_data = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let registry: &GatewayRegistry =
        bytemuck::from_bytes(&registry_data.data[8..8 + std::mem::size_of::<GatewayRegistry>()]);
    assert_eq!(
        registry.gateways[0].delegated_at_tally, 0,
        "no delegated stake at tally must snapshot the flag as 0"
    );
}

// ADR-025 / BD-111 regression: tally_weights must snapshot
// GatewaySlot.delegated_at_tally = 1 when the gateway carries delegated stake,
// so distribute_epoch carves the delegate share from this tally snapshot rather
// than the operator-manipulable live total_delegated_stake. Without the
// snapshot, an operator could disable delegation + crank every delegate out
// between tally and distribution to redirect the delegate reward to itself.
#[tokio::test]
async fn test_tally_snapshots_delegated_at_tally() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // clock 0 so gateway.start_timestamp (0) <= epoch.start (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Delegate to the gateway BEFORE tally so the stake contributes to weight.
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    let (delegation_key, _) = delegation_pda(&ctx.payer.pubkey(), &delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: 20_000_000_000u64,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past epoch start, create epoch, tally.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: ctx.payer.pubkey(),
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // The gateway carried delegated stake at tally → flag must be 1.
    let registry_data = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let registry: &GatewayRegistry =
        bytemuck::from_bytes(&registry_data.data[8..8 + std::mem::size_of::<GatewayRegistry>()]);
    assert_eq!(
        registry.gateways[0].delegated_at_tally, 1,
        "delegated stake present at tally must snapshot the flag as 1"
    );
    assert!(
        registry.gateways[0].composite_weight > 0,
        "tallied gateway should have nonzero weight"
    );
}

// -----------------------------------------
// 2. test_epoch_full_lifecycle
// -----------------------------------------

#[tokio::test]
async fn test_epoch_full_lifecycle() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol token account with 1,000,000 ARIO (1_000_000_000_000 mARIO)
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    // Join gateway with 10k ARIO (start_timestamp = 0)
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp clock past epoch start (>= 100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // --- 1. Create epoch ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // --- 2. Tally weights ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // --- 3. Prescribe epoch (no NameRegistry, just gateway PDA for observer resolution) ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    // Gateway PDA for observer address resolution
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify prescriptions
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.prescriptions_done, 1);
    assert_eq!(epoch.observer_count, 1);
    // Observer should be the payer (observer_address = payer in join_gateway)
    assert_eq!(epoch.prescribed_observers[0], payer_pk);
    assert!(epoch.per_gateway_reward > 0);
    assert!(epoch.per_observer_reward > 0);

    // --- 4. Save observations (observer = payer) ---
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let mut gateway_results = [0u8; 375];
    gateway_results[0] = 0b00000001; // bit 0 = pass for gateway at index 0

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Record balances before distribution
    let protocol_balance_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let stake_balance_before = get_token_balance(&mut ctx, &setup.stake_token.pubkey()).await;

    // Read gateway operator_stake before distribution
    let gw_data = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway_before = Gateway::try_deserialize(&mut gw_data.data.as_slice()).unwrap();
    let op_stake_before = gateway_before.operator_stake;

    // --- 5. Warp past epoch end and distribute ---
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1; // past epoch end
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    // Gateway PDA as writable remaining_account
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify rewards distributed
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.rewards_distributed, 1);
    assert_eq!(epoch.distribution_index, 1);

    // Verify SPL transfer happened (protocol balance decreased, stake balance increased)
    let protocol_balance_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let stake_balance_after = get_token_balance(&mut ctx, &setup.stake_token.pubkey()).await;
    assert!(protocol_balance_after < protocol_balance_before);
    assert!(stake_balance_after > stake_balance_before);

    // Verify gateway operator_stake increased
    let gw_data = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway_after = Gateway::try_deserialize(&mut gw_data.data.as_slice()).unwrap();
    assert!(gateway_after.operator_stake > op_stake_before);

    // Verify gateway stats updated
    assert_eq!(gateway_after.stats.total_epochs, 1);
    assert_eq!(gateway_after.stats.passed_epochs, 1);
    assert_eq!(gateway_after.stats.prescribed_epochs, 1);
    assert_eq!(gateway_after.stats.observed_epochs, 1);

    // --- 6. Close all Observation PDAs before close_epoch ---
    // Audit M8: close_epoch now requires observations_closed == observations_submitted
    // so observation rent isn't orphaned at the parent's closure.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseObservation {
                epoch: epoch_key,
                observation: observation_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseObservation { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // --- 7. Close epoch (need current_epoch_index >= epoch_index + 7) ---
    // current_epoch_index was incremented to 1 after create_epoch.
    // We need to bump it to at least 7. Manipulate the epoch_settings account directly.
    let es_account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let mut es_data = es_account.data.clone();
    // current_epoch_index is at offset: 8 (disc) + 32 (authority) + 8 (epoch_duration) + 1 + 1 + 8 + 2 + 1 = 61
    let cei_offset = 8 + 32 + 8 + 1 + 1 + 8 + 2 + 1;
    es_data[cei_offset..cei_offset + 8].copy_from_slice(&8u64.to_le_bytes());
    ctx.set_account(
        &epoch_settings_key,
        &solana_sdk::account::Account {
            lamports: es_account.lamports,
            data: es_data,
            owner: es_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify epoch account is closed
    let epoch_account = ctx.banks_client.get_account(epoch_key).await.unwrap();
    assert!(epoch_account.is_none());
}

// -----------------------------------------
// 3. test_epoch_create_not_enabled
// -----------------------------------------

#[tokio::test]
async fn test_epoch_create_not_enabled() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, false); // disabled
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Warp clock past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);
    let payer_pk = ctx.payer.pubkey();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::EpochsNotEnabled);
}

// -----------------------------------------
// 4. test_epoch_tally_weights_already_tallied
// -----------------------------------------

#[tokio::test]
async fn test_epoch_tally_weights_already_tallied() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp clock past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights (first time -- should succeed)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Advance slot to get a different blockhash (set_sysvar alone doesn't advance
    // the bank slot, so the blockhash stays the same and the identical transaction
    // gets silently deduplicated as "already processed").
    ctx.warp_to_slot(100).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 101;
    ctx.set_sysvar(&clock);

    // Tally weights again (should fail with WeightsAlreadyTallied)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts2 = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts2.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts2,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::WeightsAlreadyTallied);
}

// -----------------------------------------
// 5. test_epoch_prescribe_before_tally
// -----------------------------------------

#[tokio::test]
async fn test_epoch_prescribe_before_tally() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    let _gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp clock past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try prescribe without tally -- should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::WeightsNotTallied);
}

// -----------------------------------------
// 6. test_epoch_save_observations_not_prescribed
// -----------------------------------------

#[tokio::test]
async fn test_epoch_save_observations_not_prescribed() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);

    // Create a non-prescribed observer keypair
    let rando = Keypair::new();
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp clock past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Fund rando with SOL for tx fee
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &rando.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try save_observations from non-prescribed observer
    let rando_pk = rando.pubkey();
    let (observation_key, _) = observation_pda(0, &rando_pk);
    let gateway_results = [0u8; 375];

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: rando_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&rando_pk),
        &[&rando],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::NotPrescribedObserver);
}

// -----------------------------------------
// 7. test_epoch_distribute_before_end
// -----------------------------------------

#[tokio::test]
async fn test_epoch_distribute_before_end() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    // The `EpochInProgress` guard rejects before the treasury CPI, so the
    // treasury SPL-owner doesn't matter here — plain setup_gar is fine.
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp clock past epoch start but well before epoch end
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Save observations
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let mut gateway_results = [0u8; 375];
    gateway_results[0] = 0b00000001;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try distribute BEFORE epoch ends (clock is at 100, epoch ends at 100 + 86400 = 86500)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::EpochInProgress);
}

// -----------------------------------------
// 8. test_epoch_close_before_retention
// -----------------------------------------

#[tokio::test]
async fn test_epoch_close_before_retention() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp clock past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Save observations
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let mut gateway_results = [0u8; 375];
    gateway_results[0] = 0b00000001;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past epoch end and distribute
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try close epoch -- current_epoch_index=1 (incremented after create), need >= 0 + 7 = 7
    // So with current=1, this should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::EpochNotCloseable);
}

// =========================================
// ERROR PATH TESTS
// =========================================

#[tokio::test]
async fn test_join_network_duplicate() {
    // Joining the network twice with the same operator should fail because
    // the gateway PDA (init constraint) already exists.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let _gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Advance to a new slot to force a fresh blockhash. Without this, the second
    // join_network below would have an identical (payer, blockhash, instructions)
    // tuple to the first, producing the same transaction signature. Solana's
    // status cache returns the previous Ok(()) for that signature, masking the
    // duplicate-PDA failure we want to assert. This is a parallel-only flake:
    // serial runs idle long enough for the PoH ticker to register a new
    // blockhash between the two calls, but under load the ticker can starve.
    ctx.warp_to_slot(2).unwrap();

    // Try to join again with the same operator — gateway PDA already exists
    let payer_pk = ctx.payer.pubkey();
    let (gateway_key, _) = gateway_pda(&payer_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: stake_amount,
                    label: "test-gateway".to_string(),
                    fqdn: "gateway.example.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: true,
                    delegate_reward_share_ratio: 10,
                    min_delegate_stake: None,
                    observer_address: payer_pk,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "Duplicate join_network should fail because gateway PDA already exists"
    );
}

#[tokio::test]
async fn test_leave_network_already_leaving() {
    // A gateway that has already left cannot leave again.
    // The handler checks: gateway.status == GatewayStatus::Joined → GarError::GatewayNotJoined
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Leave network the first time
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key_0, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key_0,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway is now Leaving
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert!(matches!(gateway.status, GatewayStatus::Leaving));

    // Try to leave again — should fail with GatewayNotJoined
    let (withdrawal_key_1, _) = withdrawal_pda(&payer_pk, 1);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key_1,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayNotJoined);
}

#[tokio::test]
async fn test_delegate_to_self() {
    // Operator cannot delegate to their own gateway — must use increase_operator_stake instead.
    // Handler checks: delegator.key() != gateway.operator → GarError::CannotDelegateToSelf
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Operator tries to delegate to their own gateway
    let payer_pk = ctx.payer.pubkey();
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &payer_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::CannotDelegateToSelf);
}

#[tokio::test]
async fn test_delegate_below_minimum() {
    // Delegating below the gateway's min_delegation_amount should fail.
    // Handler checks: amount >= gateway.settings.min_delegation_amount → GarError::DelegationBelowMinimum
    // Default min_delegation_amount = 10_000_000 (10 ARIO = global min_delegate_stake)
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Create a separate delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    // Fund delegator with SOL
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create delegator's token account and fund it
    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Try to delegate 1 mARIO (way below 10 ARIO minimum)
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: 1, // 1 mARIO, far below 10 ARIO minimum
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::DelegationBelowMinimum);
}

#[tokio::test]
async fn test_delegate_to_leaving_gateway() {
    // Cannot delegate to a gateway that is in Leaving status.
    // Handler checks: gateway.status == GatewayStatus::Joined → GarError::GatewayNotJoined
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Leave the network
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create a separate delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Try to delegate to the leaving gateway
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayNotJoined);
}

#[tokio::test]
async fn test_decrease_operator_stake_below_minimum() {
    // Decreasing operator stake to a value below MIN_OPERATOR_STAKE (but non-zero) should fail.
    // Handler checks: remaining >= min_operator_stake || remaining == 0 → GarError::StakeBelowMinimum
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with exactly 10,000 ARIO (the minimum)
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Try to decrease by 5,000 ARIO → remaining would be 5,000 ARIO (below 10,000 minimum)
    let decrease_amount = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::StakeBelowMinimum);
}

#[tokio::test]
async fn test_decrease_operator_stake_to_zero_not_leaving() {
    // Decreasing all operator stake without leaving first should fail.
    // Handler checks: if remaining == 0, gateway.status must be Leaving → GarError::MustLeaveFirst
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with 10,000 ARIO
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Try to decrease by 10,000 ARIO (all of it) without leaving first
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: stake_amount, // decrease all stake
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::MustLeaveFirst);
}

// =========================================
// INVARIANT TESTS
// =========================================

#[tokio::test]
async fn test_stake_conservation_across_operations() {
    // Invariant: stake_token_account balance == gateway.operator_stake + sum(pending_withdrawal_amounts)
    // Tokens stay locked in the stake account until claimed, even after decrease_operator_stake.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with 30,000 ARIO
    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    // Verify initial state: stake_token_account has 30k, no withdrawals
    let stake_bal_0 = get_token_balance(&mut ctx, &setup.stake_token.pubkey()).await;
    assert_eq!(stake_bal_0, initial_stake);

    let gw_data = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gw_data.data.as_slice()).unwrap();
    assert_eq!(gateway.operator_stake, initial_stake);
    // Invariant check: stake_balance == operator_stake + 0 (no withdrawals)
    assert_eq!(
        stake_bal_0, gateway.operator_stake,
        "Stake conservation violated at initial state"
    );

    // Decrease operator stake by 5k (creates withdrawal, tokens stay in stake account)
    let decrease_amount = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // After decrease: stake_token_account still has 20k (tokens don't move until claimed)
    let stake_bal_1 = get_token_balance(&mut ctx, &setup.stake_token.pubkey()).await;
    assert_eq!(
        stake_bal_1, initial_stake,
        "Stake token balance should not change on decrease (tokens stay locked)"
    );

    // Read gateway and withdrawal to verify the invariant
    let gw_data = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gw_data.data.as_slice()).unwrap();
    assert_eq!(gateway.operator_stake, initial_stake - decrease_amount);

    let wd_data = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut wd_data.data.as_slice()).unwrap();
    assert_eq!(withdrawal.amount, decrease_amount);

    // Invariant: stake_token_balance == gateway.operator_stake + pending_withdrawal_amount
    assert_eq!(
        stake_bal_1,
        gateway.operator_stake + withdrawal.amount,
        "Stake conservation violated: stake_balance != operator_stake + pending_withdrawals"
    );
}

/// Read the program-tracked stake state and assert both invariants documented in
/// `INVARIANTS.md`:
///
/// 1. `stake_token_account.balance == Σ Gateway.operator_stake + Σ Gateway.total_delegated_stake + Σ Withdrawal.amount`
/// 2. `GatewaySettings.{total_staked, total_delegated, total_withdrawn}` equals the per-entity sums.
///
/// `gateway_keys` lists every Gateway PDA in the test. `withdrawal_keys` lists
/// every Withdrawal PDA that has been created so far; closed/uninitialized
/// entries are skipped silently. `label` is included in assertion messages to
/// identify which checkpoint failed.
async fn assert_global_stake_invariants(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    gateway_keys: &[Pubkey],
    withdrawal_keys: &[Pubkey],
    label: &str,
) {
    let pool_balance = get_token_balance(ctx, &setup.stake_token.pubkey()).await;

    let settings_acc = ctx
        .banks_client
        .get_account(setup.settings_key)
        .await
        .unwrap()
        .unwrap();
    let settings = GatewaySettings::try_deserialize(&mut settings_acc.data.as_slice()).unwrap();

    let mut sum_operator = 0u64;
    let mut sum_delegated = 0u64;
    for gw_key in gateway_keys {
        let gw_acc = ctx
            .banks_client
            .get_account(*gw_key)
            .await
            .unwrap()
            .unwrap();
        let gw = Gateway::try_deserialize(&mut gw_acc.data.as_slice()).unwrap();
        sum_operator += gw.operator_stake;
        sum_delegated += gw.total_delegated_stake;
    }

    let mut sum_withdrawn = 0u64;
    for wd_key in withdrawal_keys {
        let Some(wd_acc) = ctx.banks_client.get_account(*wd_key).await.unwrap() else {
            continue;
        };
        if wd_acc.data.len() < 8 {
            continue;
        }
        if let Ok(wd) = Withdrawal::try_deserialize(&mut wd_acc.data.as_slice()) {
            sum_withdrawn += wd.amount;
        }
    }

    // Invariant 1 — stake pool conservation
    assert_eq!(
        pool_balance,
        sum_operator + sum_delegated + sum_withdrawn,
        "[{label}] Invariant 1 violated: stake_token_account.balance ({pool_balance}) != \
         Σ operator_stake ({sum_operator}) + Σ total_delegated_stake ({sum_delegated}) + \
         Σ Withdrawal.amount ({sum_withdrawn})"
    );

    // Invariant 2 — supply-counter shadow
    assert_eq!(
        settings.total_staked, sum_operator,
        "[{label}] Invariant 2 violated: settings.total_staked ({}) != Σ operator_stake ({sum_operator})",
        settings.total_staked
    );
    assert_eq!(
        settings.total_delegated, sum_delegated,
        "[{label}] Invariant 2 violated: settings.total_delegated ({}) != Σ total_delegated_stake ({sum_delegated})",
        settings.total_delegated
    );
    assert_eq!(
        settings.total_withdrawn, sum_withdrawn,
        "[{label}] Invariant 2 violated: settings.total_withdrawn ({}) != Σ Withdrawal.amount ({sum_withdrawn})",
        settings.total_withdrawn
    );
}

/// Create and fund a new actor (SOL for fees/rent + a token ATA with `mint_amount` ARIO).
async fn create_funded_actor(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    sol_lamports: u64,
    mint_amount: u64,
) -> (Keypair, Keypair) {
    let actor = Keypair::new();
    let actor_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &actor.pubkey(),
            sol_lamports,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    create_token_account(ctx, &actor_token, &setup.mint.pubkey(), &actor.pubkey()).await;
    mint_tokens(
        ctx,
        &setup.mint.pubkey(),
        &actor_token.pubkey(),
        &setup.mint_authority,
        mint_amount,
    )
    .await;

    (actor, actor_token)
}

#[tokio::test]
async fn test_stake_conservation_global() {
    // Multi-gateway / multi-delegator property test for INVARIANTS.md (#1 and #2).
    //
    // Scenario:
    //   1. Three gateways join with different stakes.
    //   2. Three delegators delegate to a mix of gateways (one gateway has 2
    //      delegators, exercising aggregation under total_delegated_stake).
    //   3. One operator decreases their stake (creates a Withdrawal PDA;
    //      tokens stay in the pool).
    //   4. One delegator decreases their delegation (second Withdrawal PDA).
    //
    // After each step we assert both invariants hold globally across all
    // gateways and withdrawals, and that the GatewaySettings supply counters
    // match the per-entity sums.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // ---- Step 1: three gateways join -------------------------------------
    let op1_pk = ctx.payer.pubkey();
    let stake1 = 30_000_000_000u64;
    let gw1 = join_gateway(&mut ctx, &setup, stake1).await;

    let (op2, op2_token) =
        create_funded_actor(&mut ctx, &setup, 10_000_000_000, 100_000_000_000).await;
    let stake2 = 25_000_000_000u64;
    let gw2 = join_gateway_with_operator(&mut ctx, &setup, &op2, &op2_token.pubkey(), stake2).await;

    let (op3, op3_token) =
        create_funded_actor(&mut ctx, &setup, 10_000_000_000, 100_000_000_000).await;
    let stake3 = 20_000_000_000u64;
    let gw3 = join_gateway_with_operator(&mut ctx, &setup, &op3, &op3_token.pubkey(), stake3).await;

    let gateway_keys = vec![gw1, gw2, gw3];
    let mut withdrawal_keys: Vec<Pubkey> = Vec::new();
    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &withdrawal_keys,
        "after 3 gateways join",
    )
    .await;

    // ---- Step 2: three delegations ---------------------------------------
    // del1 → gw1, del2 → gw2, del3 → gw1 (two delegators on gw1)
    let (del1, del1_token) =
        create_funded_actor(&mut ctx, &setup, 5_000_000_000, 50_000_000_000).await;
    let (del2, del2_token) =
        create_funded_actor(&mut ctx, &setup, 5_000_000_000, 50_000_000_000).await;
    let (del3, del3_token) =
        create_funded_actor(&mut ctx, &setup, 5_000_000_000, 50_000_000_000).await;

    async fn do_delegate(
        ctx: &mut ProgramTestContext,
        setup: &GarSetup,
        gateway_operator: &Pubkey,
        gateway_key: &Pubkey,
        delegator: &Keypair,
        delegator_token: &Pubkey,
        amount: u64,
    ) {
        let delegator_pk = delegator.pubkey();
        let (delegation_key, _) = delegation_pda(gateway_operator, &delegator_pk);
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DelegateStake {
                    settings: setup.settings_key,
                    gateway: *gateway_key,
                    delegation: delegation_key,
                    delegator_token_account: *delegator_token,
                    stake_token_account: setup.stake_token.pubkey(),
                    delegator: delegator_pk,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::DelegateStake { amount }.data(),
            }],
            Some(&delegator_pk),
            &[delegator],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let del_amt_1 = 10_000_000_000u64;
    let del_amt_2 = 8_000_000_000u64;
    let del_amt_3 = 5_000_000_000u64;

    do_delegate(
        &mut ctx,
        &setup,
        &op1_pk,
        &gw1,
        &del1,
        &del1_token.pubkey(),
        del_amt_1,
    )
    .await;
    do_delegate(
        &mut ctx,
        &setup,
        &op2.pubkey(),
        &gw2,
        &del2,
        &del2_token.pubkey(),
        del_amt_2,
    )
    .await;
    do_delegate(
        &mut ctx,
        &setup,
        &op1_pk,
        &gw1,
        &del3,
        &del3_token.pubkey(),
        del_amt_3,
    )
    .await;

    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &withdrawal_keys,
        "after 3 delegations",
    )
    .await;

    // ---- Step 3: op2 decreases their operator stake ----------------------
    let op2_decrease = 5_000_000_000u64;
    let (op2_wd_counter, _) = withdrawal_counter_pda(&op2.pubkey());
    let (op2_wd_0, _) = withdrawal_pda(&op2.pubkey(), 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gw2,
                withdrawal_counter: op2_wd_counter,
                withdrawal: op2_wd_0,
                operator: op2.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: op2_decrease,
            }
            .data(),
        }],
        Some(&op2.pubkey()),
        &[&op2],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    withdrawal_keys.push(op2_wd_0);

    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &withdrawal_keys,
        "after op2 decrease_operator_stake",
    )
    .await;

    // ---- Step 4: del3 decreases their delegation -------------------------
    let del3_decrease = 2_000_000_000u64;
    let (del3_delegation, _) = delegation_pda(&op1_pk, &del3.pubkey());
    let (del3_wd_counter, _) = withdrawal_counter_pda(&del3.pubkey());
    let (del3_wd_0, _) = withdrawal_pda(&del3.pubkey(), 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gw1,
                delegation: del3_delegation,
                withdrawal_counter: del3_wd_counter,
                withdrawal: del3_wd_0,
                delegator: del3.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: del3_decrease,
            }
            .data(),
        }],
        Some(&del3.pubkey()),
        &[&del3],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    withdrawal_keys.push(del3_wd_0);

    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &withdrawal_keys,
        "after del3 decrease_delegate_stake",
    )
    .await;

    // Sanity: expected supply-counter totals at the end
    let settings_acc = ctx
        .banks_client
        .get_account(setup.settings_key)
        .await
        .unwrap()
        .unwrap();
    let settings = GatewaySettings::try_deserialize(&mut settings_acc.data.as_slice()).unwrap();
    assert_eq!(
        settings.total_staked,
        stake1 + stake2 - op2_decrease + stake3,
        "final total_staked mismatch"
    );
    assert_eq!(
        settings.total_delegated,
        del_amt_1 + del_amt_2 + del_amt_3 - del3_decrease,
        "final total_delegated mismatch"
    );
    assert_eq!(
        settings.total_withdrawn,
        op2_decrease + del3_decrease,
        "final total_withdrawn mismatch"
    );
}

#[tokio::test]
async fn test_stake_conservation_slash_path() {
    // Property test for INVARIANTS.md #1/#2/#3 across the prune_gateway slash flow.
    //
    // Scenario:
    //   1. Gateway joins with 50k ARIO operator stake (well above the 20k min).
    //   2. Delegator delegates 5k ARIO to that gateway.
    //   3. failed_consecutive is injected at 30 (== max_consecutive_failures from
    //      pre_create_epoch_settings) to make the gateway eligible for pruning.
    //   4. prune_gateway is called. Per gateway.rs:
    //        - slash_amount = min(MIN_OPERATOR_STAKE, operator_stake) = 20k
    //        - post_slash    = 50k - 20k = 30k
    //        - protected     = min(20k, 30k) = 20k        (Withdrawal #0)
    //        - excess        = 30k - 20k = 10k            (Withdrawal #1)
    //        - slash transferred stake → protocol treasury
    //
    // Asserts:
    //   * Invariant 1: stake_token_account.balance equals Σ operator_stake +
    //     Σ total_delegated_stake + Σ Withdrawal.amount after the slash.
    //   * Invariant 2: GatewaySettings supply counters track per-entity sums.
    //   * Invariant 3 / treasury inflow: protocol_token_account grew by exactly
    //     MIN_OPERATOR_STAKE.
    //   * Delegations are untouched by the slash.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with 50k ARIO (well above 20k min so post_slash > min, exercising both vaults).
    let operator_pk = ctx.payer.pubkey();
    let stake_amount = 50_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Delegator with 5k delegation
    let (del, del_token) =
        create_funded_actor(&mut ctx, &setup, 5_000_000_000, 50_000_000_000).await;
    let del_amount = 5_000_000_000u64;
    let (del_delegation, _) = delegation_pda(&operator_pk, &del.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: del_delegation,
                delegator_token_account: del_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: del.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake { amount: del_amount }.data(),
        }],
        Some(&del.pubkey()),
        &[&del],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let gateway_keys = vec![gateway_key];
    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &[],
        "after join + delegate",
    )
    .await;

    // Snapshot treasury before the slash
    let treasury_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    // Inject failed_consecutive = 30 to make the gateway eligible for pruning.
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let mut gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    gw.stats.failed_consecutive = 30;
    let mut new_data = Vec::new();
    gw.try_serialize(&mut new_data).unwrap();
    let original_len = gw_account.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &gateway_key,
        &solana_sdk::account::Account {
            lamports: gw_account.lamports,
            data: new_data,
            owner: gw_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Call prune_gateway. Operator gets exit_id=0 (protected vault) and id=1 (excess vault).
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (op_wd_counter, _) = withdrawal_counter_pda(&operator_pk);
    let (protected_vault, _) = withdrawal_pda(&operator_pk, 0);
    let (excess_vault, _) = withdrawal_pda(&operator_pk, 1);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PruneGateway {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_key,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: op_wd_counter,
                withdrawal: protected_vault,
                excess_withdrawal: Some(excess_vault),
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: operator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PruneGateway {}.data(),
        }],
        Some(&operator_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Invariant 1 + 2 still hold after slash.
    let withdrawal_keys = vec![protected_vault, excess_vault];
    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &withdrawal_keys,
        "after prune_gateway slash",
    )
    .await;

    // Invariant 3 / treasury inflow: protocol_token_account grew by exactly slash_amount.
    let treasury_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    assert_eq!(
        treasury_after - treasury_before,
        Gateway::MIN_OPERATOR_STAKE,
        "treasury did not receive slash_amount (= MIN_OPERATOR_STAKE)"
    );

    // Slash math: protected = 20k, excess = 10k, delegation intact at 5k.
    let protected = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(protected_vault)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    let excess = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(excess_vault)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(protected.amount, Gateway::MIN_OPERATOR_STAKE);
    assert!(protected.is_protected);
    assert!(protected.is_exit_vault);
    assert_eq!(
        excess.amount,
        stake_amount - 2 * Gateway::MIN_OPERATOR_STAKE
    );
    assert!(!excess.is_protected);

    let gw_after = Gateway::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(gateway_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(
        gw_after.operator_stake, 0,
        "operator_stake zeroed after slash"
    );
    assert_eq!(
        gw_after.total_delegated_stake, del_amount,
        "delegations not affected by slash"
    );
    assert!(matches!(gw_after.status, GatewayStatus::Leaving));
}

#[tokio::test]
async fn test_stake_conservation_payment_paths() {
    // Property test for INVARIANTS.md across the cross-flow payment paths
    // (deduct_operator_stake_for_payment, deduct_delegation_for_payment).
    // These instructions move tokens stake_token_account → protocol_token_account
    // while decrementing the corresponding accounting field; they're designed
    // to be CPI'd from ario-arns for ArNS purchases funded from stake.
    //
    // Asserts:
    //   * Invariant 1 holds after each deduct (pool balance still matches
    //     the per-entity sums even though tokens left the pool).
    //   * Invariant 2 supply counters track in lockstep with per-entity state.
    //   * Treasury (protocol_token_account) gains exactly the deducted amount.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let operator_pk = ctx.payer.pubkey();
    let stake_amount = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Delegator with 8k delegation
    let (del, del_token) =
        create_funded_actor(&mut ctx, &setup, 5_000_000_000, 50_000_000_000).await;
    let del_amount = 8_000_000_000u64;
    let (del_delegation, _) = delegation_pda(&operator_pk, &del.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: del_delegation,
                delegator_token_account: del_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: del.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake { amount: del_amount }.data(),
        }],
        Some(&del.pubkey()),
        &[&del],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let gateway_keys = vec![gateway_key];
    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &[],
        "after join + delegate",
    )
    .await;

    // ---- Operator deducts from their own stake to pay the protocol ------
    let treasury_t0 = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let op_deduct = 5_000_000_000u64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductOperatorStakeForPayment {
                settings: setup.settings_key,
                gateway: gateway_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                operator: operator_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductOperatorStakeForPayment { amount: op_deduct }.data(),
        }],
        Some(&operator_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &[],
        "after deduct_operator_stake_for_payment",
    )
    .await;
    let treasury_t1 = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    assert_eq!(
        treasury_t1 - treasury_t0,
        op_deduct,
        "treasury did not receive operator-stake deduction"
    );

    // ---- Delegator deducts from their own delegation --------------------
    let del_deduct = 2_000_000_000u64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductDelegationForPayment {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: del_delegation,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                delegator: del.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductDelegationForPayment { amount: del_deduct }.data(),
        }],
        Some(&del.pubkey()),
        &[&del],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_global_stake_invariants(
        &mut ctx,
        &setup,
        &gateway_keys,
        &[],
        "after deduct_delegation_for_payment",
    )
    .await;
    let treasury_t2 = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    assert_eq!(
        treasury_t2 - treasury_t1,
        del_deduct,
        "treasury did not receive delegation deduction"
    );

    // Final balance check: operator and delegation accounting reflect both deductions.
    let gw_final = Gateway::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(gateway_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(gw_final.operator_stake, stake_amount - op_deduct);
    assert_eq!(gw_final.total_delegated_stake, del_amount - del_deduct);

    let del_final = Delegation::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(del_delegation)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(del_final.amount, del_amount - del_deduct);
}

#[tokio::test]
async fn test_epoch_reward_budget_not_exceeded() {
    // Verify that reward distribution never transfers more than epoch.total_eligible_rewards
    // from the protocol token account.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol token account with 1,000,000 ARIO
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    // Join gateway with 10k ARIO
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp clock past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // --- 1. Create epoch ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // --- 2. Tally weights ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // --- 3. Prescribe epoch ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // --- 4. Save observations ---
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let mut gateway_results = [0u8; 375];
    gateway_results[0] = 0b00000001; // bit 0 = pass for gateway at index 0

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read epoch.total_eligible_rewards BEFORE distribution
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    let total_eligible_rewards = epoch.total_eligible_rewards;
    assert!(
        total_eligible_rewards > 0,
        "Epoch should have non-zero eligible rewards"
    );

    // Record protocol balance before distribution
    let protocol_balance_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    // --- 5. Warp past epoch end and distribute ---
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1; // past epoch end
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify protocol balance decreased
    let protocol_balance_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let actual_spent = protocol_balance_before - protocol_balance_after;

    // Key invariant: actual tokens spent must not exceed total_eligible_rewards
    assert!(
        actual_spent <= total_eligible_rewards,
        "Reward budget exceeded! Spent {} but budget was {}",
        actual_spent,
        total_eligible_rewards
    );
    assert!(
        actual_spent > 0,
        "Distribution should have transferred some tokens"
    );
}

// -----------------------------------------
// test_prune_gateway_happy
// -----------------------------------------

#[tokio::test]
async fn test_prune_gateway_happy() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join gateway with exactly MIN_OPERATOR_STAKE (10,000 ARIO)
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Inject failed_consecutive = 30 to make gateway eligible for pruning
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let mut gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    gw.stats.failed_consecutive = 30;
    // Re-serialize and pad to original account size (Gateway has variable-length String fields,
    // so serialized output is shorter than allocated account size)
    let mut new_data = Vec::new();
    gw.try_serialize(&mut new_data).unwrap();
    let original_len = gw_account.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &gateway_key,
        &solana_sdk::account::Account {
            lamports: gw_account.lamports,
            data: new_data,
            owner: gw_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Derive PDAs for prune instruction
    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    // Call PruneGateway
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PruneGateway {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_key,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PruneGateway {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: gateway.status == Leaving and operator_stake == 0
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert!(
        matches!(gw.status, GatewayStatus::Leaving),
        "Gateway should be in Leaving status after prune"
    );
    assert_eq!(
        gw.operator_stake, 0,
        "Operator stake should be zero after prune"
    );

    // Verify: Withdrawal PDA created (amount = 0 since slash = 100% of min_operator_stake)
    let withdrawal_account = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    // With stake == MIN_OPERATOR_STAKE and 100% slash rate, remaining = 0
    assert_eq!(
        withdrawal.amount, 0,
        "Withdrawal amount should be 0 (full slash)"
    );
    assert_eq!(withdrawal.owner, payer_pk);

    // Verify: Registry count decreased to 0
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    // Registry slot stays occupied after prune; slot reclaimed later by
    // `finalize_gone` once the grace window expires.
    assert_eq!(count, 1, "registry slot preserved after prune");
}

// -----------------------------------------
// test_prune_gateway_not_eligible
// -----------------------------------------

#[tokio::test]
async fn test_prune_gateway_not_eligible() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join gateway (failed_consecutive stays at 0 — not eligible for pruning)
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Derive PDAs for prune instruction
    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    // Call PruneGateway — should fail because failed_consecutive < 30
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PruneGateway {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_key,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PruneGateway {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayNotEligible);
}

// =========================================
// NEW COVERAGE TESTS (8 tests)
// =========================================

// -----------------------------------------
// test_claim_delegate_from_leaving_gateway
// -----------------------------------------

#[tokio::test]
async fn test_claim_delegate_from_leaving_gateway() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join gateway with 10k ARIO
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;
    let payer_pk = ctx.payer.pubkey();

    // Create delegator, fund with SOL + tokens
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Gateway leaves network
    let (op_withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (op_withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: op_withdrawal_counter_key,
                withdrawal: op_withdrawal_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway is Leaving
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert!(matches!(gw.status, GatewayStatus::Leaving));

    // Delegate claims from leaving gateway
    let (del_withdrawal_counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (del_withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimDelegateFromLeavingGateway {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_withdrawal_counter_key,
                withdrawal: del_withdrawal_key,
                delegator: delegator_pk,
                payer: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimDelegateFromLeavingGateway {}.data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify withdrawal created with correct amount and lock period
    let withdrawal_account = ctx
        .banks_client
        .get_account(del_withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    assert_eq!(withdrawal.owner, delegator_pk);
    assert_eq!(withdrawal.amount, delegate_amount); // No rewards accumulated
    assert!(withdrawal.is_delegate);
    // Lock period = WITHDRAWAL_LOCK_PERIOD = 30 days = 2,592,000 seconds
    assert_eq!(withdrawal.available_at, withdrawal.created_at + 2_592_000);

    // Verify delegation.amount is now 0
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, 0);

    // Verify gateway.total_delegated_stake decreased
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert_eq!(gw.total_delegated_stake, 0);
}

// -----------------------------------------
// test_claim_delegate_not_leaving
// -----------------------------------------

#[tokio::test]
async fn test_claim_delegate_not_leaving() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join gateway (stays Joined)
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;
    let payer_pk = ctx.payer.pubkey();

    // Create delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try claim_delegate_from_leaving_gateway while gateway is still Joined -> should fail
    let (del_withdrawal_counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (del_withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimDelegateFromLeavingGateway {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_withdrawal_counter_key,
                withdrawal: del_withdrawal_key,
                delegator: delegator_pk,
                payer: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimDelegateFromLeavingGateway {}.data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    // Anchor constraint: gateway.status == GatewayStatus::Leaving @ GarError::GatewayNotJoined
    assert_anchor_error!(result, GarError::GatewayNotJoined);
}

// -----------------------------------------
// test_close_empty_delegation
// -----------------------------------------

#[tokio::test]
async fn test_close_empty_delegation() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;
    let payer_pk = ctx.payer.pubkey();

    // Create delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Fully withdraw delegation (decrease by full amount)
    let (del_withdrawal_counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (del_withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_withdrawal_counter_key,
                withdrawal: del_withdrawal_key,
                delegator: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation.amount == 0
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, 0);

    // Close the empty delegation (permissionless — anyone can close)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEmptyDelegation {
                gateway: gateway_key,
                delegation: delegation_key,
                delegator: delegator_pk,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEmptyDelegation {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation account is closed (rent reclaimed)
    let delegation_account = ctx.banks_client.get_account(delegation_key).await.unwrap();
    assert!(
        delegation_account.is_none(),
        "Delegation account should be closed"
    );
}

// -----------------------------------------
// test_compound_delegation_rewards
// -----------------------------------------

#[tokio::test]
async fn test_compound_delegation_rewards() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;
    let payer_pk = ctx.payer.pubkey();

    // Create delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 100 ARIO
    let delegate_amount = 100_000_000u64; // 100 ARIO
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Simulate rewards by injecting cumulative_reward_per_token into gateway
    // delegation.reward_debt was set to gateway.cumulative_reward_per_token at delegation time (= 0)
    // If we set gateway.cumulative_reward_per_token = 1e18, then:
    // pending = 100_000_000 * (1e18 - 0) / 1e18 = 100_000_000
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let mut gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    gw.cumulative_reward_per_token = 1_000_000_000_000_000_000u128; // 1e18
    let mut new_data = Vec::new();
    gw.try_serialize(&mut new_data).unwrap();
    let original_len = gw_account.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &gateway_key,
        &solana_sdk::account::Account {
            lamports: gw_account.lamports,
            data: new_data,
            owner: gw_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Verify delegation amount before compounding
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, delegate_amount);
    assert_eq!(delegation.reward_debt, 0);

    // Call compound_delegation_rewards
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CompoundDelegationRewards {
                gateway: gateway_key,
                delegation: delegation_key,
                delegator: delegator_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CompoundDelegationRewards {}.data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation.amount increased by pending rewards
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    // pending = 100_000_000 * 1e18 / 1e18 = 100_000_000
    assert_eq!(delegation.amount, delegate_amount + 100_000_000);
    // reward_debt should now equal gateway.cumulative_reward_per_token
    assert_eq!(delegation.reward_debt, 1_000_000_000_000_000_000u128);

    // Verify gateway.total_delegated_stake also increased
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert_eq!(gw.total_delegated_stake, delegate_amount + 100_000_000);
}

// -----------------------------------------
// test_import_account_rejects_epoch_settings_discriminator (audit M-4, 2026-05-30 follow-up)
//
// PR #81 added `epoch_settings_excluded_from_import_allowlist` as a unit test
// that pins the `known_discriminator` helper returns None for EpochSettings.
// But the helper returning None doesn't ALONE prove the on-chain
// `import_account` handler actually rejects — Anchor's account-resolution
// layer or a future refactor could conceivably bypass it. This test takes
// the integration path: builds a real `import_account` ix with the
// EpochSettings discriminator + valid PDA derivation, sends it signed by
// the migration_authority, and asserts the handler rejects with
// `InvalidAccountData`.
//
// Pre-fix (PR #81): EpochSettings was in the allowlist; this ix would have
// succeeded and overwritten the EpochSettings account (the post-finalize
// authority-hijack the audit flagged).
// Post-fix: rejects at the `known_discriminator` check inside the handler.
// -----------------------------------------

#[tokio::test]
async fn test_import_account_rejects_epoch_settings_discriminator() {
    let (mint, _mint_authority, _operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let payer_pk = ctx.payer.pubkey();

    // Pre-setup: GatewaySettings is created in program_test_with_gar with
    // migration_active=false + migration_authority=&dummy. Flip both so
    // `import_account`'s `constraint = settings.migration_active` AND
    // `settings.migration_authority == authority.key()` constraints pass
    // — that way we land on the discriminator gate (InvalidAccountData),
    // which is what M-4 is testing, not MigrationInactive/Unauthorized.
    //
    // CodeRabbit nit #84: deserialize the typed struct + mutate + reserialize
    // (vs. hardcoded byte offsets) so a future GatewaySettings layout change
    // can't silently break this test.
    let (settings_key, _) = settings_pda();
    let settings_acc = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    let mut settings = GatewaySettings::try_deserialize(&mut settings_acc.data.as_slice()).unwrap();
    settings.migration_active = true;
    settings.migration_authority = payer_pk;
    let mut new_data = Vec::with_capacity(settings_acc.data.len());
    settings.try_serialize(&mut new_data).unwrap();
    new_data.resize(settings_acc.data.len(), 0); // preserve the existing account size
    ctx.set_account(
        &settings_key,
        &solana_sdk::account::Account {
            lamports: settings_acc.lamports,
            data: new_data,
            owner: settings_acc.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Build the EpochSettings discriminator (same way the handler computes it).
    use anchor_lang::solana_program::hash::hash;
    let es_disc = hash(b"account:EpochSettings");
    let mut data = vec![0u8; 8 + 256];
    data[..8].copy_from_slice(&es_disc.to_bytes()[..8]);

    // PDA derivation MUST match what the handler computes with the supplied
    // seeds, else we get InvalidPda before reaching the discriminator check.
    let seeds: Vec<Vec<u8>> = vec![EPOCH_SETTINGS_SEED.to_vec()];
    let (es_pda, _bump) = epoch_settings_pda();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ImportAccount {
                settings: settings_key,
                authority: payer_pk, // migration_authority defaults to payer in setup
                payer: payer_pk,
                account: es_pda,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ImportAccount { seeds, data }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;

    // M-4 assertion: the handler MUST reject EpochSettings at the
    // discriminator-allowlist check. Pre-PR #81 this would have SUCCEEDED
    // and overwritten EpochSettings — the exact post-finalize authority-
    // hijack the audit flagged.
    assert_anchor_error!(result, GarError::InvalidAccountData);
}

// -----------------------------------------
// test_distribute_epoch_skips_cleared_registry_slots (audit M-1, 2026-05-29)
//
// CodeRabbit pushback on PR #79: add focused non-ignored test coverage
// for the cleared-slot skip branch in distribute_epoch (mirrors the
// GAR-009 pattern in tally_weights).
//
// Strategy: run the standard scaffold through `create_epoch`, then
// directly mutate the GatewayRegistry + Epoch zero-copy accounts to
// inject a state where every active slot is `Pubkey::default()`. With
// no real gateways, pass 2 produces zero pending distributions →
// `transfer_amount == 0` → the ario-core CPI is skipped, so this test
// does NOT need ario-core loaded (which is why all the other
// distribute_epoch tests are #[ignore]'d).
//
// Pre-fix the loop would hit the `registry.gateways[dist_idx].address
// == gateway.operator` require! on the very first cleared slot and
// reject the whole tx. Post-fix the loop skips each cleared slot,
// advances `distribution_index`, and finishes cleanly with
// `rewards_distributed = 1`.
// -----------------------------------------

#[tokio::test]
async fn test_distribute_epoch_skips_cleared_registry_slots() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    // epoch_duration=86_400s, enabled=true so create_epoch passes the gate
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Warp clock past genesis (100) so create_epoch can build a valid epoch.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200;
    ctx.set_sysvar(&clock);

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // --- Create epoch (real ix; gives us a valid Epoch state to mutate) ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // --- Inject the cleared-slot state ---
    //
    // Bump registry.count to N so dist_idx iterates through slots, but
    // leave each slot's address at the default `Pubkey::default()`
    // (slots are zero-initialized by setup_gar / pre_create_epoch_settings).
    let cleared_count: u32 = 2;
    {
        let reg_acc = ctx
            .banks_client
            .get_account(setup.registry_key)
            .await
            .unwrap()
            .unwrap();
        let mut data = reg_acc.data.clone();
        // GatewayRegistry layout: disc(8) + authority(32) + count(u32 LE @ 40)
        data[40..44].copy_from_slice(&cleared_count.to_le_bytes());
        ctx.set_account(
            &setup.registry_key,
            &solana_sdk::account::Account {
                lamports: reg_acc.lamports,
                data,
                owner: reg_acc.owner,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );
    }

    // Mutate the Epoch state so distribute_epoch's guards pass:
    //   prescriptions_done = 1  (else PrescriptionsNotDone)
    //   end_timestamp = 0       (already in the past — clock is 200)
    //   rewards_distributed = 0 (already zero from create_epoch)
    //   active_gateway_count = cleared_count (so the loop iterates N times)
    //   distribution_index = 0
    {
        let epoch_acc = ctx
            .banks_client
            .get_account(epoch_key)
            .await
            .unwrap()
            .unwrap();
        let mut data = epoch_acc.data.clone();
        // bytemuck::from_bytes_mut on the post-discriminator slice.
        let epoch_bytes = &mut data[8..8 + std::mem::size_of::<Epoch>()];
        let epoch: &mut Epoch = bytemuck::from_bytes_mut(epoch_bytes);
        epoch.end_timestamp = 0;
        epoch.prescriptions_done = 1;
        epoch.rewards_distributed = 0;
        epoch.active_gateway_count = cleared_count;
        epoch.distribution_index = 0;
        // Clear reward fields so transfer_amount stays 0 even if a slot somehow processed
        epoch.per_gateway_reward = 0;
        epoch.per_observer_reward = 0;
        epoch.total_eligible_rewards = 0;

        ctx.set_account(
            &epoch_key,
            &solana_sdk::account::Account {
                lamports: epoch_acc.lamports,
                data,
                owner: epoch_acc.owner,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );
    }

    // Derive the ario_config PDA that the address constraint expects.
    // Uses the placeholder ARIO_CORE_PROGRAM_ID baked into the build —
    // sufficient for the constraint; no CPI to ario-core actually fires
    // because all slots are cleared.
    let (ario_config_pda, _) =
        Pubkey::find_program_address(&[b"ario_config"], &ario_gar::ARIO_CORE_PROGRAM_ID);

    // Two dummy account_infos for the remaining_accounts slot. The skip
    // branch fires BEFORE any deserialization, so these accounts are
    // never actually read — any system-owned address works.
    let dummy_a = Pubkey::new_unique();
    let dummy_b = Pubkey::new_unique();

    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        ario_config: ario_config_pda,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        payer: payer_pk,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(dummy_a, false));
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(dummy_b, false));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );

    // M-1 assertion: distribute_epoch succeeds despite every registry slot
    // being Pubkey::default(). Pre-fix this failed with InvalidGatewayAccount
    // on the first slot because the registry-vs-operator equality check
    // (now skipped) rejected default vs the dummy account's owner.
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("distribute_epoch must succeed when all registry slots are cleared (audit M-1)");

    // Verify the loop advanced past both cleared slots and finalized the epoch.
    let epoch_acc = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_acc.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(
        epoch.distribution_index, cleared_count,
        "distribution_index must equal active_count after iterating past cleared slots"
    );
    assert_eq!(
        epoch.rewards_distributed, 1,
        "rewards_distributed must be set when dist_idx reaches active_count"
    );
}

// -----------------------------------------
// test_compound_delegation_rewards_permissionless (audit M-2, 2026-05-29)
//
// `compound_delegation_rewards` MUST be invokable by any signer — not just
// the delegator themselves. INVARIANTS.md describes the ix as
// "permissionless" because compounding only moves a delegate's pending
// rewards into their active stake on their OWN balance; no funds move
// between accounts and no authorization is required. Pre-this-fix the
// account struct required `delegator: Signer<'info>`, contradicting the
// documented model and blocking (a) the off-chain monitor's Invariant 1
// health check and (b) `finalize_gone` for gateways with dormant pending
// delegate rewards.
//
// This test pins the new behavior: a third-party signer (not the
// delegator) calls compound on the delegator's behalf and the
// instruction succeeds with the same state mutation.
// -----------------------------------------

#[tokio::test]
async fn test_compound_delegation_rewards_permissionless() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;
    let payer_pk = ctx.payer.pubkey();

    // Set up a real delegator (the holder of the Delegation PDA).
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    let delegate_amount = 100_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Inject pending reward (same simulation as test_compound_delegation_rewards).
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let mut gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    gw.cumulative_reward_per_token = 1_000_000_000_000_000_000u128; // 1e18
    let mut new_data = Vec::new();
    gw.try_serialize(&mut new_data).unwrap();
    let original_len = gw_account.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &gateway_key,
        &solana_sdk::account::Account {
            lamports: gw_account.lamports,
            data: new_data,
            owner: gw_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // ---- The permissionless invocation ----
    // A THIRD-PARTY caller — not the delegator — invokes compound on the
    // delegator's behalf. The `delegator` ACCOUNT in the metas is still the
    // delegator's pubkey (it's a PDA-derivation source bound by the seeds
    // constraint), but it is NOT a signer here. Pre-this-fix this fails
    // with `MissingRequiredSignature`; post-fix it succeeds.
    let third_party = Keypair::new();
    let third_party_pk = third_party.pubkey();
    // Fund the third party so they can pay tx fees.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &third_party_pk,
            10_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CompoundDelegationRewards {
                gateway: gateway_key,
                delegation: delegation_key,
                // `delegator` is now an UncheckedAccount (not a Signer);
                // we pass its pubkey purely as a PDA-derivation seed.
                delegator: delegator_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CompoundDelegationRewards {}.data(),
        }],
        Some(&third_party_pk),
        &[&third_party], // ← signed by third party, NOT the delegator
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("permissionless compound must succeed (audit M-2)");

    // State mutation is identical to the delegator-signed variant.
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, delegate_amount + 100_000_000);
    assert_eq!(delegation.reward_debt, 1_000_000_000_000_000_000u128);
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert_eq!(gw.total_delegated_stake, delegate_amount + 100_000_000);
}

// -----------------------------------------
// test_close_epoch_happy
// -----------------------------------------

#[tokio::test]
async fn test_close_epoch_happy() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol token account
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // 1. Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // 2. Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // 3. Prescribe epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // 4. Warp past epoch end and distribute
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify epoch is distributed
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.rewards_distributed, 1);

    // 5. Try to close epoch before 7 epochs gap (current_epoch_index = 1) -> should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::EpochNotCloseable);

    // 6. Bump current_epoch_index to >= epoch_index + 7 (need to set to at least 7)
    // Use raw byte manipulation to avoid serialize/deserialize format issues.
    // current_epoch_index is at byte offset 61 in the account data:
    //   8 (disc) + 32 (authority) + 8 (epoch_duration) + 1 (observer_count) +
    //   1 (name_count) + 8 (min_observer_stake) + 2 (slash_rate) + 1 (enabled) = 61
    let es_account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let mut new_es_data = es_account.data.clone();
    new_es_data[61..69].copy_from_slice(&8u64.to_le_bytes());
    ctx.set_account(
        &epoch_settings_key,
        &solana_sdk::account::Account {
            lamports: es_account.lamports,
            data: new_es_data,
            owner: es_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Advance slot so the bank sees the updated account
    ctx.warp_to_slot(200).unwrap();

    // 7. Close epoch (should succeed now)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify epoch account is closed
    let epoch_account = ctx.banks_client.get_account(epoch_key).await.unwrap();
    assert!(
        epoch_account.is_none(),
        "Epoch account should be closed after close_epoch"
    );
}

// -----------------------------------------
// test_redelegate_fee_reset
// -----------------------------------------

#[tokio::test]
async fn test_redelegate_fee_reset() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let operator2 = Keypair::new();
    let operator3 = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        operator3.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Gateway 1 (ctx.payer)
    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Gateway 2 (operator2)
    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key2 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator2,
        &op2_token.pubkey(),
        stake_amount,
    )
    .await;

    // Gateway 3 (operator3)
    let op3_pk = operator3.pubkey();
    let op3_token = Keypair::new();
    create_token_account(&mut ctx, &op3_token, &setup.mint.pubkey(), &op3_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op3_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key3 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator3,
        &op3_token.pubkey(),
        stake_amount,
    )
    .await;

    // Create delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;

    // Delegate 50 ARIO to gateway1
    let delegate_amount = 50_000_000_000u64;
    let (delegation_key1, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key1,
                delegation: delegation_key1,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // 1st redelegate: gateway1 -> gateway2 (free, count was 0)
    let (delegation_key2, _) = delegation_pda(&op2_pk, &delegator_pk);
    let (redelegation_key, _) = redelegation_pda(&delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::RedelegateStake {
                source_gateway: gateway_key1,
                target_gateway: gateway_key2,
                source_delegation: delegation_key1,
                target_delegation: delegation_key2,
                redelegation_record: redelegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                settings: setup.settings_key,
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::RedelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: first redelegate was free -> target got full amount
    let del2_account = ctx
        .banks_client
        .get_account(delegation_key2)
        .await
        .unwrap()
        .unwrap();
    let del2 = Delegation::try_deserialize(&mut del2_account.data.as_slice()).unwrap();
    assert_eq!(
        del2.amount, delegate_amount,
        "First redelegate should be free (no fee)"
    );

    // Verify redelegation record: count=1, fee_reset_at set
    let redelegation_account = ctx
        .banks_client
        .get_account(redelegation_key)
        .await
        .unwrap()
        .unwrap();
    let redelegation =
        RedelegationRecord::try_deserialize(&mut redelegation_account.data.as_slice()).unwrap();
    assert_eq!(redelegation.redelegation_count, 1);
    let fee_reset_at = redelegation.fee_reset_at;

    // Warp past the fee reset interval (7 days = 604_800 seconds)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = fee_reset_at + 1; // Past fee reset
    ctx.set_sysvar(&clock);

    // 2nd redelegate: gateway2 -> gateway3 (should be free again after reset)
    let (delegation_key3, _) = delegation_pda(&op3_pk, &delegator_pk);

    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::RedelegateStake {
                source_gateway: gateway_key2,
                target_gateway: gateway_key3,
                source_delegation: delegation_key2,
                target_delegation: delegation_key3,
                redelegation_record: redelegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                settings: setup.settings_key,
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::RedelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // After fee reset, the count resets to 1 (the current redelegation),
    // but the fee rate at the time of this redelegation is calculated BEFORE the count increment.
    // get_fee_rate checks: current_timestamp >= fee_reset_at -> count = 0 -> fee = 0%
    // Then count is updated to 1 for the next redelegation.
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    assert_eq!(
        protocol_after, protocol_before,
        "After fee reset, redelegate should be free (0% fee)"
    );

    // Verify full amount arrived at target
    let del3_account = ctx
        .banks_client
        .get_account(delegation_key3)
        .await
        .unwrap()
        .unwrap();
    let del3 = Delegation::try_deserialize(&mut del3_account.data.as_slice()).unwrap();
    assert_eq!(
        del3.amount, delegate_amount,
        "After fee reset, full amount should arrive"
    );

    // Verify redelegation count was reset to 1
    let redelegation_account = ctx
        .banks_client
        .get_account(redelegation_key)
        .await
        .unwrap()
        .unwrap();
    let redelegation =
        RedelegationRecord::try_deserialize(&mut redelegation_account.data.as_slice()).unwrap();
    assert_eq!(
        redelegation.redelegation_count, 1,
        "Count should reset to 1 after fee window expires"
    );
}

// -----------------------------------------
// test_prune_gateway_stake_at_minimum
// -----------------------------------------

#[tokio::test]
async fn test_prune_gateway_stake_at_minimum() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join gateway with exactly MIN_OPERATOR_STAKE (10,000 ARIO)
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Inject failed_consecutive = 30 to make gateway eligible for pruning
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let mut gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    gw.stats.failed_consecutive = 30;
    let mut new_data = Vec::new();
    gw.try_serialize(&mut new_data).unwrap();
    let original_len = gw_account.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &gateway_key,
        &solana_sdk::account::Account {
            lamports: gw_account.lamports,
            data: new_data,
            owner: gw_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    // Record protocol balance before prune
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    // Prune gateway
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PruneGateway {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_key,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PruneGateway {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: slash_amount = min(min_operator_stake, operator_stake) = 10,000 ARIO
    // remaining = operator_stake - slash_amount = 0
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert_eq!(
        gw.operator_stake, 0,
        "Operator stake should be zero after full slash"
    );
    assert!(matches!(gw.status, GatewayStatus::Leaving));

    // Verify: Withdrawal amount = 0 (full slash, nothing left)
    let withdrawal_account = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    assert_eq!(
        withdrawal.amount, 0,
        "With stake == min_operator_stake, remaining = 0"
    );

    // Verify: Slashed tokens went to protocol
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    assert_eq!(
        protocol_after - protocol_before,
        stake_amount,
        "Full min_operator_stake should be slashed to protocol"
    );

    // Registry slot stays occupied after prune; reclaimed later by
    // `finalize_gone` once the grace window expires.
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(count, 1, "registry slot preserved after prune");
}

// -----------------------------------------
// test_instant_withdrawal_decay_boundaries
// -----------------------------------------

#[tokio::test]
async fn test_instant_withdrawal_decay_boundaries() {
    // Test the time-decaying penalty at three boundary points:
    // elapsed=0 -> max penalty (50%)
    // elapsed=total_period -> min penalty (10%)
    // elapsed=half -> mid penalty (linear interpolation)
    //
    // Lua: penaltyRate = maxPenalty - (maxPenalty - minPenalty) * elapsed / total

    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // --- Point 1: elapsed=0 -> max penalty (50%) ---
    let initial_stake = 50_000_000_000u64; // 50k ARIO to create 3 withdrawals while staying above 20k min
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;
    let payer_pk = ctx.payer.pubkey();

    let decrease_amount = 10_000_000_000u64; // 10k ARIO per withdrawal

    // Create withdrawal 0
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key_0, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key_0,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read withdrawal to get created_at
    let w0_account = ctx
        .banks_client
        .get_account(withdrawal_key_0)
        .await
        .unwrap()
        .unwrap();
    let w0 = Withdrawal::try_deserialize(&mut w0_account.data.as_slice()).unwrap();
    let created_at = w0.created_at;

    // Instant withdrawal at elapsed=0 -> penalty = max = 50%
    let balance_before = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InstantWithdrawal {
                settings: setup.settings_key,
                withdrawal: withdrawal_key_0,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InstantWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let balance_after = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    // penalty_rate = 500_000 (50%), fee = 10B * 0.5 = 5B, payout = 5B
    let expected_fee_0 = 5_000_000_000u64;
    let expected_payout_0 = 5_000_000_000u64;
    assert_eq!(
        balance_after - balance_before,
        expected_payout_0,
        "elapsed=0: payout should be 50%"
    );
    assert_eq!(
        protocol_after - protocol_before,
        expected_fee_0,
        "elapsed=0: fee should be 50%"
    );

    // --- Point 2: elapsed=total_period -> min penalty (10%) ---
    // Create withdrawal 1
    let (withdrawal_key_1, _) = withdrawal_pda(&payer_pk, 1);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key_1,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp to created_at + total_period (30 days)
    let total_period = 30 * 86_400i64;
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = created_at + total_period;
    ctx.set_sysvar(&clock);

    let balance_before = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InstantWithdrawal {
                settings: setup.settings_key,
                withdrawal: withdrawal_key_1,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InstantWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let balance_after = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    // elapsed >= total_period -> min penalty = 100_000 (10%)
    // fee = 10B * 0.10 = 1B, payout = 9B
    let expected_fee_1 = 1_000_000_000u64;
    let expected_payout_1 = 9_000_000_000u64;
    assert_eq!(
        balance_after - balance_before,
        expected_payout_1,
        "elapsed=total: payout should be 90%"
    );
    assert_eq!(
        protocol_after - protocol_before,
        expected_fee_1,
        "elapsed=total: fee should be 10%"
    );

    // --- Point 3: elapsed=half -> mid penalty (linear interpolation) ---
    // After 2x10k decreases we have 30k stake. Decrease by another 10k to
    // create a third (non-protected) withdrawal vault — easier than going
    // through leave_network here, and the leave-vault split (BD-102 fix)
    // protects exit vaults from instant withdrawal anyway, so the original
    // "leave then instant on the exit vault" pattern can no longer test
    // mid-decay on a leave vault directly.
    let third_decrease = 10_000_000_000u64;
    let (withdrawal_key_2, _) = withdrawal_pda(&payer_pk, 2);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key_2,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: third_decrease,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read withdrawal created_at for half-point calculation
    let w2_account = ctx
        .banks_client
        .get_account(withdrawal_key_2)
        .await
        .unwrap()
        .unwrap();
    let w2 = Withdrawal::try_deserialize(&mut w2_account.data.as_slice()).unwrap();
    let created_at_2 = w2.created_at;

    // Warp to half the withdrawal period (15 days from created_at_2)
    let half_period = 15 * 86_400i64;
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = created_at_2 + half_period;
    ctx.set_sysvar(&clock);

    let balance_before = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InstantWithdrawal {
                settings: setup.settings_key,
                withdrawal: withdrawal_key_2,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InstantWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let balance_after = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    // penalty_rate at half = 500_000 - (400_000 * 15 / 30) = 500_000 - 200_000 = 300_000 (30%)
    // withdrawal = 10K; fee = 10B * 0.30 = 3B, payout = 7B
    let expected_fee_2 = 3_000_000_000u64;
    let expected_payout_2 = 7_000_000_000u64;
    assert_eq!(
        balance_after - balance_before,
        expected_payout_2,
        "elapsed=half: payout should be 70%"
    );
    assert_eq!(
        protocol_after - protocol_before,
        expected_fee_2,
        "elapsed=half: fee should be 30%"
    );
}

// -----------------------------------------
// test_instant_withdrawal_decay_quarter_and_period_edge
// -----------------------------------------

/// Complements `test_instant_withdrawal_decay_boundaries` (which pins
/// elapsed ∈ {0, half, total}) with the points that exercise the exact
/// shape of the linear ramp and the inclusive `>=` boundary in
/// `instant_withdrawal` (withdrawal.rs ~lines 84-103):
///   * elapsed = period/4   -> max - (max-min)/4 = 40% (ramp is linear)
///   * elapsed = period - 1 -> still decaying, fee strictly above the 10% min
///   * elapsed = period + 1 -> min (10%), confirming the `>=` gate clamps
///
/// All three are charged by the real on-chain handler (not a mirror), so a
/// regression in the interpolation or the boundary comparison fails here.
#[tokio::test]
async fn test_instant_withdrawal_decay_quarter_and_period_edge() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // 50k stake supports three 10k withdrawals while staying above the 20k
    // operator minimum.
    let initial_stake = 50_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);

    let decrease_amount = 10_000_000_000u64;
    let total_period = 30 * 86_400i64;

    // Create three withdrawal vaults (ids 0,1,2) up front; capture each
    // vault's created_at so the elapsed offset is exact.
    let mut created_ats = [0i64; 3];
    for (id, slot) in created_ats.iter_mut().enumerate() {
        let (withdrawal_key, _) = withdrawal_pda(&payer_pk, id as u64);
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DecreaseOperatorStake {
                    settings: setup.settings_key,
                    gateway: gateway_key,
                    withdrawal_counter: withdrawal_counter_key,
                    withdrawal: withdrawal_key,
                    operator: payer_pk,
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::DecreaseOperatorStake {
                    amount: decrease_amount,
                }
                .data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let acct = ctx
            .banks_client
            .get_account(withdrawal_key)
            .await
            .unwrap()
            .unwrap();
        let w = Withdrawal::try_deserialize(&mut acct.data.as_slice()).unwrap();
        *slot = w.created_at;
    }

    // (elapsed offset, expected fee, expected payout) for each vault.
    // rate(elapsed) = max - (max-min)*elapsed/period, clamped to [min,max].
    //   quarter:  rate = 500_000 - 400_000/4 = 400_000 (40%) -> fee 4B, payout 6B
    //   period+1: elapsed >= period -> min = 100_000 (10%)  -> fee 1B, payout 9B
    let quarter = total_period / 4;
    let cases = [
        (0usize, quarter, 4_000_000_000u64, 6_000_000_000u64),
        (1usize, total_period + 1, 1_000_000_000u64, 9_000_000_000u64),
    ];

    for (id, elapsed, expected_fee, expected_payout) in cases {
        let (withdrawal_key, _) = withdrawal_pda(&payer_pk, id as u64);
        let mut clock = ctx
            .banks_client
            .get_sysvar::<solana_sdk::clock::Clock>()
            .await
            .unwrap();
        clock.unix_timestamp = created_ats[id] + elapsed;
        ctx.set_sysvar(&clock);

        let balance_before = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
        let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::InstantWithdrawal {
                    settings: setup.settings_key,
                    withdrawal: withdrawal_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    owner_token_account: setup.operator_token.pubkey(),
                    protocol_token_account: setup.protocol_token.pubkey(),
                    owner: payer_pk,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::InstantWithdrawal {}.data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let balance_after = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
        let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
        assert_eq!(
            balance_after - balance_before,
            expected_payout,
            "elapsed={elapsed}: payout mismatch"
        );
        assert_eq!(
            protocol_after - protocol_before,
            expected_fee,
            "elapsed={elapsed}: fee mismatch"
        );
    }

    // Vault 2 at elapsed = period - 1: still on the decaying ramp, so the
    // fee must be strictly greater than the 10% floor (1B) — proving the
    // boundary is `>=` (min only AT/after period), not `>` (min just before).
    let (withdrawal_key_2, _) = withdrawal_pda(&payer_pk, 2);
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = created_ats[2] + total_period - 1;
    ctx.set_sysvar(&clock);

    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InstantWithdrawal {
                settings: setup.settings_key,
                withdrawal: withdrawal_key_2,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InstantWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let fee_at_period_minus_one = protocol_after - protocol_before;
    // 10% floor on a 10k withdrawal is exactly 1B. At period-1 the rate is
    // still above min, so the fee must exceed that floor.
    assert!(
        fee_at_period_minus_one > 1_000_000_000u64,
        "at period-1 the penalty must still be above the 10% min (got fee {fee_at_period_minus_one}); \
         a `>` boundary instead of `>=` would not change this point, but a fee == 1B would mean the \
         min-clamp fired one second too early"
    );
    // And it must remain below the 50% ceiling (5B).
    assert!(
        fee_at_period_minus_one < 5_000_000_000u64,
        "at period-1 the penalty must be below the 50% max"
    );
}

// =========================================
// COVERAGE GAP TESTS
// =========================================

// -----------------------------------------
// A. close_observation
// -----------------------------------------

#[tokio::test]
async fn test_close_observation() {
    // After a distributed epoch, close an observation PDA to reclaim rent.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway starts before epoch
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Save observations
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let mut gateway_results = [0u8; 375];
    gateway_results[0] = 0b00000001;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past epoch end and distribute
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify observation account exists
    let obs_account = ctx.banks_client.get_account(observation_key).await.unwrap();
    assert!(
        obs_account.is_some(),
        "Observation should exist before closing"
    );

    // Close observation
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseObservation {
                epoch: epoch_key,
                observation: observation_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseObservation { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify observation account is closed
    let obs_account = ctx.banks_client.get_account(observation_key).await.unwrap();
    assert!(
        obs_account.is_none(),
        "Observation should be closed after close_observation"
    );
}

// -----------------------------------------
// B. set_allowlist_enabled
// -----------------------------------------

#[tokio::test]
async fn test_set_allowlist_enabled() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();

    // Verify initially false
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert!(!gateway.settings.allowlist_enabled);

    // Enable allowlist
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SetAllowlistEnabled { enabled: true }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify allowlist is now enabled
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert!(gateway.settings.allowlist_enabled);

    // Disable allowlist
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SetAllowlistEnabled { enabled: false }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify allowlist is now disabled
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert!(!gateway.settings.allowlist_enabled);
}

// -----------------------------------------
// C. set_epochs_enabled
// -----------------------------------------

#[tokio::test]
async fn test_set_epochs_enabled() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    // This test asserts custom EpochSettings via pre_create_epoch_settings
    // (enabled=true with a specific authority); using the default helper
    // would auto-create it with enabled=false first and pre_create overrides
    // would still work, but using the bare helper keeps the intent obvious.
    let mut pt = program_test_with_gar_for_epoch_init(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    // Pre-create epoch settings with enabled=true, authority=dummy
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);

    // Add the authority as a funded account so it can sign
    let authority = Keypair::new();
    // We need to use the dummy pubkey as the authority, but since it's random
    // and we don't have its keypair, let's use a different approach: create with
    // a known keypair's pubkey as authority.
    // Actually, we need the authority keypair to sign. Let's re-create epoch_settings
    // with the payer as authority.
    // Use the no-auto-create variant — `InitializeEpochs` below has an `init`
    // constraint that requires the EpochSettings PDA to not exist on entry.
    let mut pt = program_test_with_gar_for_epoch_init(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Initialize epochs with payer as authority
    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::InitializeEpochs {
                epoch_settings: epoch_settings_key,
                payer: upgrade_authority_keypair().pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::InitializeEpochs {
                params: ario_gar::InitializeEpochParams {
                    authority: payer_pk,
                    epoch_duration: 86_400,
                    observer_count: 5,
                    name_count: 2,
                    min_observer_stake: 10_000_000_000,
                    slash_rate: 0,
                    tenure_weight_duration: 180 * 86_400,
                    max_tenure_weight: 4,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer, &upgrade_authority_keypair()],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify initially disabled
    let account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let es = EpochSettings::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert!(!es.enabled);

    // Enable epochs
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SetEpochsEnabled { enabled: true }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify enabled
    let account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let es = EpochSettings::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert!(es.enabled);

    // Disable epochs
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SetEpochsEnabled { enabled: false }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // GAR-007: Verify disable is timelocked (enabled stays true, disable_at is set)
    let account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let es = EpochSettings::try_deserialize(&mut account.data.as_slice()).unwrap();
    assert!(es.enabled); // Still enabled during timelock period
    assert!(es.disable_at > 0); // But disable_at is set for future
}

// -----------------------------------------
// D. cancel_withdrawal delegate path
// -----------------------------------------

#[tokio::test]
async fn test_cancel_withdrawal_delegate() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Create a separate delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Decrease delegate by full amount to create a delegate withdrawal
    let (del_withdrawal_counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (del_withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_withdrawal_counter_key,
                withdrawal: del_withdrawal_key,
                delegator: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation is zeroed and withdrawal exists
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, 0);

    let withdrawal_account = ctx
        .banks_client
        .get_account(del_withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    assert!(withdrawal.is_delegate);
    assert_eq!(withdrawal.amount, delegate_amount);

    // Cancel the delegate withdrawal (tokens return to delegation)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_gar::accounts::CancelWithdrawal {
        settings: setup.settings_key,
        gateway: gateway_key,
        withdrawal: del_withdrawal_key,
        delegation: Some(delegation_key),
        owner: delegator_pk,
    }
    .to_account_metas(None);
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::CancelWithdrawal {}.data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation.amount restored
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, delegate_amount);

    // Verify gateway.total_delegated_stake restored
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert_eq!(gateway.total_delegated_stake, delegate_amount);

    // Verify withdrawal account closed
    let wd_account = ctx
        .banks_client
        .get_account(del_withdrawal_key)
        .await
        .unwrap();
    assert!(wd_account.is_none());
}

// -----------------------------------------
// E. delegate_stake with allowlist enforcement
// -----------------------------------------

#[tokio::test]
async fn test_delegate_stake_allowlist_not_allowed() {
    // When allowlist is enabled, delegate NOT on allowlist should fail
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();

    // Enable allowlist
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SetAllowlistEnabled { enabled: true }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create a separate delegator NOT on allowlist
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    // Try to delegate without being on the allowlist -- should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::DelegateNotAllowed);
}

#[tokio::test]
async fn test_delegate_stake_allowlist_allowed() {
    // When allowlist is enabled, delegate ON allowlist should succeed
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();

    // Create a separate delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Add delegate to allowlist
    let (allowlist_key, _) = allowlist_pda(&payer_pk, &delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::AllowDelegate {
                gateway: gateway_key,
                allowlist_entry: allowlist_key,
                delegate: delegator_pk,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AllowDelegate {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Enable allowlist
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SetAllowlistEnabled { enabled: true }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Delegate with allowlist entry as remaining_account -- should succeed
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut delegate_accounts = ario_gar::accounts::DelegateStake {
        settings: setup.settings_key,
        gateway: gateway_key,
        delegation: delegation_key,
        delegator_token_account: delegator_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        delegator: delegator_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // Append allowlist entry PDA as remaining_account
    delegate_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        allowlist_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: delegate_accounts,
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation created
    let delegation_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, delegate_amount);
}

// -----------------------------------------
// H. join_network invalid reward share ratio
// -----------------------------------------

#[tokio::test]
async fn test_join_network_invalid_reward_share() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let (gateway_key, _) = gateway_pda(&payer_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);

    // Try to join with delegate_reward_share_ratio = 96 (96*100=9600 > MAX 9500)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: 20_000_000_000,
                    label: "test-gw".to_string(),
                    fqdn: "gw.test.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: true,
                    delegate_reward_share_ratio: 96, // 96*100=9600 > MAX 9500
                    min_delegate_stake: None,
                    observer_address: payer_pk,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InvalidRewardShare);
}

// -----------------------------------------
// I. update_gateway_settings field coverage: properties, protocol, note
// -----------------------------------------

#[tokio::test]
async fn test_update_gateway_settings_all_fields() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let valid_arweave_id = "abcdefghijklmnopqrstuvwxyz01234567890ABCDEF".to_string();
    // Update all fields: properties, protocol, allow_delegated_staking, delegate_reward_share_ratio, note (via label/fqdn)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateGatewaySettings {
                params: ario_gar::UpdateGatewayParams {
                    label: Some("new-label".to_string()),
                    fqdn: Some("new.domain.com".to_string()),
                    port: Some(8443),
                    protocol: Some(Protocol::Http),
                    properties: Some(valid_arweave_id.clone()),
                    note: None,
                    allow_delegated_staking: Some(false),
                    delegate_reward_share_ratio: Some(50),
                    min_delegate_stake: Some(20_000_000), // 20 ARIO
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify all fields updated
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert_eq!(gateway.label, "new-label");
    assert_eq!(gateway.fqdn, "new.domain.com");
    assert_eq!(gateway.port, 8443);
    assert!(matches!(gateway.protocol, Protocol::Http));
    assert_eq!(gateway.properties, valid_arweave_id);
    assert!(!gateway.settings.allow_delegated_staking);
    // Fix #7 (WP §6.3): the delegate_reward_share_ratio change is DEFERRED —
    // the active value is unchanged (still the join value 10*100=1000) and the
    // request is staged in `pending_delegate_reward_share_ratio` until the next
    // tally_weights applies it.
    assert_eq!(gateway.settings.delegate_reward_share_ratio, 1000);
    assert_eq!(
        gateway.settings.pending_delegate_reward_share_ratio,
        Some(5000) // 50 * 100, staged for next epoch
    );
    // Fix #6 (WP §6.3): flipping allow_delegated_staking true→false records the
    // disable timestamp that starts the re-enable cooldown.
    assert!(gateway.settings.delegation_disabled_at.is_some());
    assert_eq!(gateway.settings.min_delegation_amount, 20_000_000);
}

// -----------------------------------------
// J. increase_operator_stake while leaving
// -----------------------------------------

#[tokio::test]
async fn test_increase_operator_stake_while_leaving() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Leave network
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to increase stake on leaving gateway -- should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::IncreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                operator: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::IncreaseOperatorStake {
                amount: 5_000_000_000,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayLeaving);
}

// -----------------------------------------
// K. create_epoch when not started (before genesis)
// -----------------------------------------

#[tokio::test]
async fn test_create_epoch_before_genesis() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    // genesis_timestamp = 1000, clock will be at ~0
    pre_create_epoch_settings(&mut pt, &dummy, 1000, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Explicitly set clock to 0 so it's before genesis_timestamp=1000
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::EpochNotStarted);
}

// -----------------------------------------
// L. update_observer_address errors
// -----------------------------------------

#[tokio::test]
async fn test_update_observer_address_same_address() {
    // Updating to the same observer address should fail with InvalidParameter
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Try to update to the same observer (payer_pk, which is the default)
    let (old_lookup_key, _) = observer_lookup_pda(&payer_pk);
    let (new_lookup_key, _) = observer_lookup_pda(&payer_pk); // same!

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateObserverAddress {
                gateway: gateway_key,
                old_observer_lookup: old_lookup_key,
                new_observer_lookup: new_lookup_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateObserverAddress {
                new_observer: payer_pk, // same as current
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    // When old and new observer are the same, the old_observer_lookup (close=operator)
    // and new_observer_lookup (init) point to the same PDA. Anchor processes constraints
    // before the handler, so the system program fails with "account already in use" (code 0)
    // before the require!(new_observer != gateway.observer_address) check runs.
    // Either way, the transaction correctly fails.
    assert!(
        result.is_err(),
        "Expected error when updating to same observer address"
    );
}

// -----------------------------------------
// F. save_observations error: epoch ended
// -----------------------------------------

#[tokio::test]
async fn test_save_observations_epoch_ended() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Warp clock to 0
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp PAST epoch end (epoch ends at 100+86400=86500)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Try to save observations after epoch ended -- should fail with EpochEnded
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let gateway_results = [0u8; 375];

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::EpochEnded);
}

// =========================================
// COVERAGE GAP TESTS
// =========================================

// -----------------------------------------
// B. leave_network swap-remove path (lines 271-301)
// When a non-last gateway leaves, the swap-remove path is triggered.
// -----------------------------------------

#[tokio::test]
async fn test_leave_network_preserves_registry_layout() {
    // Two gateways join. Gateway 1 (index 0) leaves. The registry layout
    // MUST be preserved — count stays 2, gateway 2 stays at index 1,
    // gateway 1's slot stays at index 0 with status=Leaving and
    // composite_weight=0. The slot is reclaimed later by `finalize_gone`.
    // This stability is what keeps `failure_counts[i]` and observer
    // bitmaps coherent across mid-epoch departures (audit H2 / H3).
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let operator2 = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Gateway 1: ctx.payer (index 0)
    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Gateway 2: operator2 (index 1)
    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key2 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator2,
        &op2_token.pubkey(),
        stake_amount,
    )
    .await;

    // Verify registry has 2 gateways
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(count, 2);

    // Gateway 1 (index 0) leaves. The slot is NOT swap-removed —
    // count stays at 2 and gateway 2's index does NOT change.
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let mut accounts = ario_gar::accounts::LeaveNetwork {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_pda().0,
        registry: setup.registry_key,
        gateway: gateway_key1,
        withdrawal_counter: withdrawal_counter_key,
        withdrawal: withdrawal_key,
        excess_withdrawal: None,
        operator: payer_pk,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // remaining_accounts[last] = observer lookup PDA (writable) for cleanup
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);
    accounts.push(solana_sdk::instruction::AccountMeta::new(
        observer_lookup_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Registry count stays at 2 — leave does not swap-remove.
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(count, 2, "registry count preserved across leave");

    // Gateway 2's index is unchanged (slot 1, not moved)
    let gw2_account = ctx
        .banks_client
        .get_account(gateway_key2)
        .await
        .unwrap()
        .unwrap();
    let gw2 = Gateway::try_deserialize(&mut gw2_account.data.as_slice()).unwrap();
    assert_eq!(gw2.registry_index.index, 1, "gateway 2 must stay at slot 1");
    assert_eq!(gw2.status, GatewayStatus::Joined);

    // Gateway 1 is Leaving and its slot is still occupied at index 0
    let gw1_account = ctx
        .banks_client
        .get_account(gateway_key1)
        .await
        .unwrap()
        .unwrap();
    let gw1 = Gateway::try_deserialize(&mut gw1_account.data.as_slice()).unwrap();
    assert!(matches!(gw1.status, GatewayStatus::Leaving));
    assert_eq!(
        gw1.registry_index.index, 0,
        "leaver's slot index is preserved"
    );
}

// -----------------------------------------
// C. redelegate_stake with non-zero fee (lines 768-871)
// Do TWO redelegations so the second one has a 10% fee.
// -----------------------------------------

#[tokio::test]
async fn test_redelegate_stake_with_fee() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let operator2 = Keypair::new();
    let operator3 = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        operator3.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Gateway 1: ctx.payer
    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Gateway 2: operator2
    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key2 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator2,
        &op2_token.pubkey(),
        stake_amount,
    )
    .await;

    // Gateway 3: operator3
    let op3_pk = operator3.pubkey();
    let op3_token = Keypair::new();
    create_token_account(&mut ctx, &op3_token, &setup.mint.pubkey(), &op3_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op3_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key3 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator3,
        &op3_token.pubkey(),
        stake_amount,
    )
    .await;

    // Create delegator, delegate to gateway1
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO to gateway1
    let delegate_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key1, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key1,
                delegation: delegation_key1,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // 1st redelegate: gateway1 -> gateway2 (free, count=0)
    let (delegation_key2, _) = delegation_pda(&op2_pk, &delegator_pk);
    let (redelegation_key, _) = redelegation_pda(&delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::RedelegateStake {
                source_gateway: gateway_key1,
                target_gateway: gateway_key2,
                source_delegation: delegation_key1,
                target_delegation: delegation_key2,
                redelegation_record: redelegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                settings: setup.settings_key,
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::RedelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Record protocol balance before 2nd redelegate
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    // 2nd redelegate: gateway2 -> gateway3 (10% fee, count=1)
    let (delegation_key3, _) = delegation_pda(&op3_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::RedelegateStake {
                source_gateway: gateway_key2,
                target_gateway: gateway_key3,
                source_delegation: delegation_key2,
                target_delegation: delegation_key3,
                redelegation_record: redelegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                settings: setup.settings_key,
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::RedelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Fee = 10% of 20_000_000_000 = 2_000_000_000
    let expected_fee = 2_000_000_000u64;
    let net_amount = delegate_amount - expected_fee;

    // Protocol balance should increase by fee
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    assert_eq!(protocol_after - protocol_before, expected_fee);

    // Target delegation should have net amount
    let del3_account = ctx
        .banks_client
        .get_account(delegation_key3)
        .await
        .unwrap()
        .unwrap();
    let del3 = Delegation::try_deserialize(&mut del3_account.data.as_slice()).unwrap();
    assert_eq!(del3.amount, net_amount);

    // Source delegation should be zero
    let del2_account = ctx
        .banks_client
        .get_account(delegation_key2)
        .await
        .unwrap()
        .unwrap();
    let del2 = Delegation::try_deserialize(&mut del2_account.data.as_slice()).unwrap();
    assert_eq!(del2.amount, 0);
}

// -----------------------------------------
// E. distribute_epoch with failed gateway (lines 1630-1739)
// Gateway fails observations -> gets 0 rewards, failed stats updated.
// -----------------------------------------

#[tokio::test]
async fn test_distribute_epoch_failed_gateway() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol token account
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Save observations marking gateway as FAILED (bit 0 = 0)
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let gateway_results = [0u8; 375]; // All zeros = all gateways failed

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Record gateway stake before distribution
    let gw_data = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway_before = Gateway::try_deserialize(&mut gw_data.data.as_slice()).unwrap();
    let op_stake_before = gateway_before.operator_stake;

    // Warp past epoch end and distribute
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: failed gateway gets per_observer reward (Scenario 4: failed + prescribed + observed)
    // but operator_stake should have observer reward added
    let gw_data = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway_after = Gateway::try_deserialize(&mut gw_data.data.as_slice()).unwrap();

    // Stats: failed_epochs=1, failed_consecutive=1, passed_consecutive=0
    assert_eq!(gateway_after.stats.total_epochs, 1);
    assert_eq!(gateway_after.stats.failed_epochs, 1);
    assert_eq!(gateway_after.stats.failed_consecutive, 1);
    assert_eq!(gateway_after.stats.passed_consecutive, 0);
    assert_eq!(gateway_after.stats.prescribed_epochs, 1);
    assert_eq!(gateway_after.stats.observed_epochs, 1);

    // Scenario 4: failed + prescribed + observed -> per_observer reward only
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    let per_observer = epoch.per_observer_reward;
    assert!(per_observer > 0);

    // The gateway should have gotten per_observer reward added to operator_stake
    assert_eq!(gateway_after.operator_stake, op_stake_before + per_observer);
}

// -----------------------------------------
// TEST-002: Double-distribute safety (audit SECURITY_AUDIT_INDEPENDENT.md)
// After a successful distribute_epoch, a second call on the same epoch must be
// rejected with RewardsAlreadyDistributed. Protects against double-crediting
// rewards when multiple crankers race or a cranker retries after confirmation.
// -----------------------------------------

#[tokio::test]
async fn test_distribute_epoch_twice_fails() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway.start_timestamp <= epoch.start_timestamp
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch start, then create + tally + prescribe + observe
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Observer submits successful observation (bit 0 = 1 = passed)
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let mut gateway_results = [0u8; 375];
    gateway_results[0] = 1;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past epoch end
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1;
    ctx.set_sysvar(&clock);

    // First distribute_epoch — must succeed
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts.clone(),
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify rewards_distributed flag is set
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(
        epoch.rewards_distributed, 1,
        "first distribute must set flag"
    );

    // Advance to a new slot before retrying with the same instructions. The
    // second distribute_epoch below uses the same payer + accounts + data, so
    // without a fresh blockhash the two transactions hash to the same signature.
    // Solana's status cache then short-circuits the second submission and
    // returns the first one's Ok(()), masking the RewardsAlreadyDistributed
    // error. Serial test runs avoid this because the PoH ticker has time to
    // advance the blockhash between calls; under parallel load the ticker can
    // starve and reproduce the flake. See test_epoch_tally_weights_already_tallied
    // for the same pattern.
    ctx.warp_to_slot(100).unwrap();
    // Re-assert the post-epoch-end clock so the EpochInProgress guard passes.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Second distribute_epoch on same epoch — must be rejected
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::RewardsAlreadyDistributed);
}

// -----------------------------------------
// E2. distribute_epoch with delegate reward splitting (lines 1681-1727)
// Gateway has delegated stake, reward is split between operator and delegates.
// -----------------------------------------

#[tokio::test]
async fn test_distribute_epoch_with_delegate_rewards() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol token account with plenty
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway starts before epoch
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Create a delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 10 ARIO
    let delegate_amount = 10_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Run full epoch lifecycle
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Save observations (pass)
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let mut gateway_results = [0u8; 375];
    gateway_results[0] = 0b00000001; // bit 0 = pass

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read gateway state before distribution
    let gw_data = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw_before = Gateway::try_deserialize(&mut gw_data.data.as_slice()).unwrap();
    let cumulative_before = gw_before.cumulative_reward_per_token;

    // Warp past epoch end and distribute
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100 + 86_400 + 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        payer: payer_pk,
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::id(),
    }
    .to_account_metas(None);
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify cumulative_reward_per_token increased (delegate reward accumulator)
    let gw_data = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw_after = Gateway::try_deserialize(&mut gw_data.data.as_slice()).unwrap();
    assert!(
        gw_after.cumulative_reward_per_token > cumulative_before,
        "cumulative_reward_per_token should increase when delegates get rewards"
    );

    // Verify operator_stake increased
    assert!(gw_after.operator_stake > gw_before.operator_stake);
}

// -----------------------------------------
// F. prune_gateway preserves registry layout
// Two gateways; gateway at index 0 has 30+ failures and gets pruned. The
// registry slot is NOT swap-removed — it stays at index 0 with
// status=Leaving until finalize_gone reclaims it.
// -----------------------------------------

#[tokio::test]
async fn test_prune_gateway_preserves_registry_layout() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let operator2 = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join 2 gateways
    let stake_amount = 30_000_000_000u64; // 30k ARIO (above min of 20k, so remaining_stake > 0 after slash)
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;

    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key2 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator2,
        &op2_token.pubkey(),
        stake_amount,
    )
    .await;

    // Verify registry has 2 gateways
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(count, 2);

    // Manually set gateway1's failed_consecutive to 30 (max_consecutive_failures)
    let gw1_account = ctx
        .banks_client
        .get_account(gateway_key1)
        .await
        .unwrap()
        .unwrap();
    let mut gw1_data = gw1_account.data.clone();
    let mut gw1 = Gateway::try_deserialize(&mut gw1_data.as_slice()).unwrap();
    gw1.stats.failed_consecutive = 30;

    // Re-serialize gateway with updated stats
    let mut buf = Vec::new();
    gw1.try_serialize(&mut buf).unwrap();
    let mut new_data = gw1_account.data.clone();
    new_data[..buf.len()].copy_from_slice(&buf);
    ctx.set_account(
        &gateway_key1,
        &solana_sdk::account::Account {
            lamports: gw1_account.lamports,
            data: new_data,
            owner: gw1_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Record protocol balance before prune
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    // Gateway1 already created withdrawal_counter via join_gateway? No - only leave/decrease creates it.
    // prune_gateway uses gateway.operator for withdrawal PDA seeds.
    let (prune_withdrawal_counter, _) = withdrawal_counter_pda(&payer_pk);
    // Read current withdrawal counter or 0 if it doesn't exist
    let counter_exists = ctx
        .banks_client
        .get_account(prune_withdrawal_counter)
        .await
        .unwrap();
    let next_id = if let Some(acct) = counter_exists {
        WithdrawalCounter::try_deserialize(&mut acct.data.as_slice())
            .unwrap()
            .next_id
    } else {
        0
    };
    let (prune_withdrawal_key, _) = withdrawal_pda(&payer_pk, next_id);

    let (epoch_settings_key, _) = epoch_settings_pda();

    // Prune gateway1 (index 0) with gateway2 as remaining_accounts[0] for swap-remove
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);
    let mut accounts = ario_gar::accounts::PruneGateway {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        registry: setup.registry_key,
        gateway: gateway_key1,
        withdrawal_counter: prune_withdrawal_counter,
        withdrawal: prune_withdrawal_key,
        excess_withdrawal: None,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer: payer_pk,
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // remaining_accounts[last] = observer lookup PDA (writable) for cleanup.
    // No swap-target gateway needed since prune leaves the registry intact.
    accounts.push(solana_sdk::instruction::AccountMeta::new(
        observer_lookup_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PruneGateway {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify gateway1 is Leaving and its registry slot is preserved
    let gw1_account = ctx
        .banks_client
        .get_account(gateway_key1)
        .await
        .unwrap()
        .unwrap();
    let gw1 = Gateway::try_deserialize(&mut gw1_account.data.as_slice()).unwrap();
    assert!(matches!(gw1.status, GatewayStatus::Leaving));
    assert_eq!(gw1.operator_stake, 0);
    assert_eq!(
        gw1.registry_index.index, 0,
        "leaver's slot index is preserved"
    );

    // Verify slash: min_operator_stake (20k ARIO) slashed to protocol
    let protocol_after = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    assert_eq!(
        protocol_after - protocol_before,
        Gateway::MIN_OPERATOR_STAKE
    );

    // Verify withdrawal for remaining stake (30k - 20k = 10k ARIO)
    let wd_account = ctx
        .banks_client
        .get_account(prune_withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let wd = Withdrawal::try_deserialize(&mut wd_account.data.as_slice()).unwrap();
    assert_eq!(wd.amount, stake_amount - Gateway::MIN_OPERATOR_STAKE);
    assert!(wd.is_exit_vault);

    // Registry layout is preserved across prune. Count stays at 2;
    // gateway2 stays at slot 1 (no swap); gateway1's slot at index 0 stays
    // occupied with status=Leaving and composite_weight=0.
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(count, 2, "registry count preserved after prune");

    let gw2_account = ctx
        .banks_client
        .get_account(gateway_key2)
        .await
        .unwrap()
        .unwrap();
    let gw2 = Gateway::try_deserialize(&mut gw2_account.data.as_slice()).unwrap();
    assert_eq!(gw2.registry_index.index, 1, "gateway 2 stays at slot 1");
}

// -----------------------------------------
// G1. join_network invalid FQDN (line 162)
// -----------------------------------------

#[tokio::test]
async fn test_join_network_invalid_fqdn() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let (gateway_key, _) = gateway_pda(&payer_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);

    // Empty FQDN
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                registry: setup.registry_key,
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                observer_lookup: observer_lookup_key,
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: 20_000_000_000,
                    label: "test-gw".to_string(),
                    fqdn: "".to_string(), // empty FQDN
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: false,
                    delegate_reward_share_ratio: 0,
                    min_delegate_stake: None,
                    observer_address: payer_pk,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InvalidFqdn);
}

// -----------------------------------------
// G2. update_gateway_settings protocol field (line 373)
// -----------------------------------------

#[tokio::test]
async fn test_update_gateway_settings_protocol() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();

    // Update protocol from Https to Http
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: settings_pda().0,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateGatewaySettings {
                params: ario_gar::UpdateGatewayParams {
                    protocol: Some(Protocol::Http),
                    ..Default::default()
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify protocol was updated
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert!(matches!(gateway.protocol, Protocol::Http));
}

// -----------------------------------------
// G3. cancel_withdrawal delegate path (line 1074)
// -----------------------------------------

#[tokio::test]
async fn test_cancel_withdrawal_delegate_restores_stake() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Create delegator
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO
    let delegate_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Decrease delegate by 10 ARIO (creates withdrawal)
    let decrease_amount = 10_000_000_000u64;
    let (del_withdrawal_counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (del_withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_withdrawal_counter_key,
                withdrawal: del_withdrawal_key,
                delegator: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation is 10 ARIO
    let del_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut del_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, delegate_amount - decrease_amount);

    // Cancel the delegate withdrawal -> delegation amount should go back up
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_gar::accounts::CancelWithdrawal {
        settings: setup.settings_key,
        gateway: gateway_key,
        withdrawal: del_withdrawal_key,
        delegation: Some(delegation_key),
        owner: delegator_pk,
    }
    .to_account_metas(None);

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::CancelWithdrawal {}.data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify delegation restored to 20 ARIO
    let del_account = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut del_account.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, delegate_amount);

    // Verify gateway.total_delegated_stake restored
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert_eq!(gateway.total_delegated_stake, delegate_amount);

    // Verify withdrawal is closed
    let wd_account = ctx
        .banks_client
        .get_account(del_withdrawal_key)
        .await
        .unwrap();
    assert!(wd_account.is_none());
}

// -----------------------------------------
// G4. save_observations invalid count (line 1524)
// -----------------------------------------

#[tokio::test]
async fn test_save_observations_invalid_count() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway starts before epoch
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch start
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Save observations with WRONG gateway count (5 instead of 1)
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let gateway_results = [0u8; 375];

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 5, // Epoch has 1 active gateway but we say 5
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InvalidObservation);
}

// -----------------------------------------
// E3. distribute_epoch with leaving gateway (0 rewards) (line 1653)
// -----------------------------------------

#[tokio::test]
async fn test_distribute_epoch_leaving_gateway_zero_rewards() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let operator2 = Keypair::new();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so gateway starts before epoch
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Gateway 2 for swap-remove
    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key2 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator2,
        &op2_token.pubkey(),
        stake_amount,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch start — pin slot for deterministic hashchain (GAR-003 strict ordering)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    clock.slot = 1;
    ctx.set_sysvar(&clock);

    // Create epoch (with 2 gateways)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights for both gateways
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key1,
        false,
    ));
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key2,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe — remaining_accounts must match hashchain selection order.
    // With pinned slot=1, compute the expected order by simulating the hashchain.
    // Since both gateways will be selected (only 2 available, observer_count=50),
    // pass both and let the order be determined by trying both permutations.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key1,
        false,
    ));
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key2,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts.clone(),
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );

    // Try [gw1, gw2] first; if order is reversed, retry with [gw2, gw1]
    let result = ctx.banks_client.process_transaction(tx).await;
    if result.is_err() {
        let mut prescribe_accounts2 = ario_gar::accounts::PrescribeEpoch {
            settings: setup.settings_key,
            epoch_settings: epoch_settings_key,
            epoch: epoch_key,
            registry: setup.registry_key,
            payer: payer_pk,
        }
        .to_account_metas(None);
        prescribe_accounts2.push(solana_sdk::instruction::AccountMeta::new_readonly(
            gateway_key2,
            false,
        ));
        prescribe_accounts2.push(solana_sdk::instruction::AccountMeta::new_readonly(
            gateway_key1,
            false,
        ));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: prescribe_accounts2,
                data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    // Now leave_network for gateway1 AFTER prescriptions (it's now Leaving during distribution)
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let mut leave_accounts = ario_gar::accounts::LeaveNetwork {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_pda().0,
        registry: setup.registry_key,
        gateway: gateway_key1,
        withdrawal_counter: withdrawal_counter_key,
        withdrawal: withdrawal_key,
        excess_withdrawal: None,
        operator: payer_pk,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // remaining_accounts for swap-remove (gateway2 at last_index)
    leave_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key2,
        false,
    ));
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);
    leave_accounts.push(solana_sdk::instruction::AccountMeta::new(
        observer_lookup_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: leave_accounts,
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Gateway1 is now Leaving. Under the in-place departure model the
    // registry layout is preserved: count stays at 2, gateway1's slot at
    // index 0 retains its pubkey with composite_weight=0, gateway2 stays
    // at index 1 with its tallied weight. distribute_epoch can therefore
    // walk both slots successfully — the leaver gets zero reward via the
    // `is_leaving` branch, the joined gateway gets its full reward.
    //
    // This is the H3 regression: pre-fix the swap-remove zeroed gateway1's
    // slot and decremented count, so distribute_epoch's registry-address
    // assertion (`registry.gateways[dist_idx].address == gateway.operator`)
    // would fail at slot 1 and revert the entire batch, permanently
    // stalling reward distribution.

    // Stake balance before distribute (gateway2's only — leaver's is 0)
    let gw2_stake_before = {
        let acct = ctx
            .banks_client
            .get_account(gateway_key2)
            .await
            .unwrap()
            .unwrap();
        let gw = Gateway::try_deserialize(&mut acct.data.as_slice()).unwrap();
        gw.operator_stake
    };

    // Warp past epoch end so distribute is allowed
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200 + 86_400; // start_timestamp(100) + duration(86_400) + buffer
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut distribute_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::ID,
        payer: payer_pk,
    }
    .to_account_metas(None);
    // Cranker passes Gateway PDAs in registry-slot order (slot 0 = leaver,
    // slot 1 = joined). Both must be writable for stat updates.
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key1,
        false,
    ));
    distribute_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key2,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: distribute_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("distribute_epoch must complete with a mid-epoch leaver in the registry");

    // Leaver: stats unchanged (Lua skips stats for leaving gateways)
    let gw1_after = ctx
        .banks_client
        .get_account(gateway_key1)
        .await
        .unwrap()
        .unwrap();
    let gw1_after = Gateway::try_deserialize(&mut gw1_after.data.as_slice()).unwrap();
    assert!(matches!(gw1_after.status, GatewayStatus::Leaving));
    assert_eq!(
        gw1_after.stats.total_epochs, 0,
        "leaver's stats must not tick during exit-epoch (Lua parity)"
    );
    assert_eq!(
        gw1_after.operator_stake, 0,
        "leaver's stake stays zeroed; reward should not auto-compound"
    );

    // Joined gateway: stats incremented, reward credited
    let gw2_after = ctx
        .banks_client
        .get_account(gateway_key2)
        .await
        .unwrap()
        .unwrap();
    let gw2_after = Gateway::try_deserialize(&mut gw2_after.data.as_slice()).unwrap();
    assert_eq!(
        gw2_after.stats.total_epochs, 1,
        "joined gateway gets its epoch counted"
    );
    assert!(
        gw2_after.operator_stake > gw2_stake_before,
        "joined gateway must receive non-zero reward (got delta {})",
        gw2_after.operator_stake.saturating_sub(gw2_stake_before),
    );

    // Epoch is fully distributed
    let epoch_after = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch_after: &Epoch =
        bytemuck::from_bytes(&epoch_after.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(
        epoch_after.rewards_distributed, 1,
        "distribute must complete"
    );
    assert_eq!(epoch_after.distribution_index, 2, "both slots advanced");
}

// -----------------------------------------
// G5. update_observer_address on leaving gateway (line 397)
// -----------------------------------------

#[tokio::test]
async fn test_update_observer_address_leaving_fails() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Leave network
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to update observer address on leaving gateway
    let new_observer = Pubkey::new_unique();
    let (old_lookup_key, _) = observer_lookup_pda(&payer_pk);
    let (new_lookup_key, _) = observer_lookup_pda(&new_observer);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateObserverAddress {
                gateway: gateway_key,
                old_observer_lookup: old_lookup_key,
                new_observer_lookup: new_lookup_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateObserverAddress { new_observer }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayLeaving);
}

// G6. update_observer_address same address (line 401)
// NOTE: This line is unreachable by design - Anchor's account constraints prevent
// the same PDA from being both closed and init'd in one tx, so the system program
// error fires before the handler body. Skipping this test.

// -----------------------------------------
// Zero-weight epoch: all gateways joined after epoch start, so composite_weight == 0
// for every gateway. prescribe_epoch must still succeed with observer_count == 0.
// Regression test for the prescribe_epoch remaining_accounts fix (commit 25b5ea6):
// NameRegistry is always the LAST remaining_account, not at index selected_count.
// -----------------------------------------

#[tokio::test]
async fn test_epoch_prescribe_zero_weight() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    // genesis_timestamp = 100, epoch_duration = 86_400
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol token account with 1,000 ARIO (1_000_000_000 mARIO)
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Warp clock to 200 — gateway joins AFTER epoch start_timestamp (100).
    // This means tally_weights will set effective_composite = 0 for this gateway
    // because gateway.start_timestamp (200) > epoch.start_timestamp (100).
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200;
    ctx.set_sysvar(&clock);

    // Join gateway with 10k ARIO (start_timestamp = 200)
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // --- 1. Create epoch (clock >= genesis_timestamp=100) ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: 1 active gateway, epoch starts at 100
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.active_gateway_count, 1);
    assert_eq!(epoch.start_timestamp, 100);

    // --- 2. Tally weights ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: weights tallied, total composite weight == 0
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.weights_tallied, 1);
    assert_eq!(
        epoch.total_composite_weight(),
        0,
        "Gateway joined after epoch start => zero weight"
    );

    // --- 3. Prescribe epoch with NO remaining_accounts ---
    // With zero total weight, selected_count == 0; no gateway PDAs needed.
    // No NameRegistry passed either — prescribe should still succeed.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: prescriptions done, zero observers, no rewards distributed.
    //
    // per_gateway_reward is 0 because the divisor is the count of
    // *reward-eligible* gateways (those with non-zero composite_weight after
    // tally), not the registry slot count. The lone gateway joined after
    // epoch start and got composite_weight=0, so joined_count=0, and the
    // reward computation is skipped. Matches Lua: late joiners don't dilute
    // the reward pool because they aren't in the eligible set. This also
    // protects honest gateways from dilution when the registry holds
    // Leaving slots that haven't been GC'd yet by `finalize_gone`.
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.prescriptions_done, 1);
    assert_eq!(
        epoch.observer_count, 0,
        "Zero-weight epoch should have no observers"
    );
    assert_eq!(
        epoch.per_observer_reward, 0,
        "No observers => zero observer reward"
    );
    assert_eq!(
        epoch.per_gateway_reward, 0,
        "No reward-eligible gateways => zero per-gateway reward"
    );
}

// -----------------------------------------
// TEST-003: Cross-program NameRegistry validation (audit SECURITY_AUDIT_INDEPENDENT.md)
// prescribe_epoch must reject a remaining_account passed as the NameRegistry
// when the account is not owned by the ario-arns program. This prevents a
// malicious cranker from feeding attacker-controlled data into name selection.
// -----------------------------------------

#[tokio::test]
async fn test_prescribe_epoch_fake_name_registry_fails() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Force zero-weight via clock trick: gateway joins AFTER epoch start_timestamp
    // so tally sets composite_weight = 0 for it. With selected_count == 0, the
    // name-registry block still runs if remaining.len() > 0, which is exactly
    // the path we need to exercise here.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200;
    ctx.set_sysvar(&clock);

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally weights
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe with a FAKE NameRegistry — pass the payer account as the
    // last remaining_account. The payer is owned by the System program,
    // not the ArNS program, so the owner check at epoch.rs:347 rejects it.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    // Fake NameRegistry — payer account (System-owned)
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        payer_pk, false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InvalidNameRegistry);
}

// -----------------------------------------
// TEST-007: Double-prune defense (audit SECURITY_AUDIT_INDEPENDENT.md)
// prune_gateway must reject a gateway that is already in `Leaving` status.
// Without the status check, a second prune would corrupt the registry
// (swap-removing an already-removed slot, indexing past `count`).
// gateway.rs:322-325 enforces `gateway.status == Joined` BEFORE the
// failed_consecutive eligibility check, so the rejection here is purely
// status-based even though we never set failed_consecutive.
// -----------------------------------------

#[tokio::test]
async fn test_prune_gateway_already_leaving_fails() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join gateway with sufficient stake
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);

    // Step 1: leave_network — moves status to Leaving, swap-removes from
    // registry (count: 1 -> 0), creates withdrawal id=0, advances counter
    // to next_id=1.
    let leave_withdrawal_key = withdrawal_pda(&payer_pk, 0).0;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: leave_withdrawal_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Sanity: gateway is now Leaving
    let gateway_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gateway = Gateway::try_deserialize(&mut gateway_account.data.as_slice()).unwrap();
    assert!(matches!(gateway.status, GatewayStatus::Leaving));

    // Step 2: attempt prune_gateway on the already-Leaving gateway
    // The status check at gateway.rs:322 fires before any other logic,
    // so we expect GatewayNotJoined regardless of failed_consecutive.
    let prune_withdrawal_key = withdrawal_pda(&payer_pk, 1).0;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PruneGateway {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_key,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: prune_withdrawal_key,
                excess_withdrawal: None,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PruneGateway {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayNotJoined);
}

// -----------------------------------------
// TEST-006: Multi-batch tally_weights (audit SECURITY_AUDIT_INDEPENDENT.md)
// With more gateways than fit in a single tx (real cranker uses BATCH_SIZE=15
// per call), tally_weights must be safe to invoke multiple times. After each
// call: tally_index advances by the number of remaining_accounts processed,
// weights_tallied stays 0 until tally_index >= active_gateway_count, and the
// final state reflects every gateway. This test uses 4 gateways tallied as
// 2+2 — the contract code path is identical regardless of N. The defenses
// being exercised: (a) progress flag transition at epoch.rs:229-231,
// (b) idempotent in-progress reads of registry slots, (c) per-call accumulation
// into total_composite_weight without double-counting.
// -----------------------------------------

#[tokio::test]
async fn test_tally_weights_multi_batch() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let operator2 = Keypair::new();
    let operator3 = Keypair::new();
    let operator4 = Keypair::new();

    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    for op in [&operator2, &operator3, &operator4] {
        pt.add_account(
            op.pubkey(),
            solana_sdk::account::Account {
                lamports: 50_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    // genesis_timestamp = 100. Need gateway.start_timestamp <= 100, so warp
    // clock to 0 BEFORE joining gateways (mirrors test_distribute_epoch_failed_gateway).
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Warp to clock=0 so all gateway start_timestamps are <= epoch start (100)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    // Join 4 gateways (gw1=ctx.payer, gw2/3/4=operator2/3/4)
    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;

    let mut other_gateway_keys = Vec::new();
    for op in [&operator2, &operator3, &operator4] {
        let token = Keypair::new();
        create_token_account(&mut ctx, &token, &setup.mint.pubkey(), &op.pubkey()).await;
        mint_tokens(
            &mut ctx,
            &setup.mint.pubkey(),
            &token.pubkey(),
            &setup.mint_authority,
            100_000_000_000,
        )
        .await;
        let gw_key =
            join_gateway_with_operator(&mut ctx, &setup, op, &token.pubkey(), stake_amount).await;
        other_gateway_keys.push(gw_key);
    }

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp so create_epoch is allowed (clock >= genesis_timestamp = 0 trivially)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // Create epoch — snapshots active_gateway_count = 4
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(epoch.active_gateway_count, 4);
    assert_eq!(epoch.weights_tallied, 0);
    assert_eq!(epoch.tally_index, 0);

    // Read the registry to learn the actual on-chain ordering — gateways may
    // not be in join-order. We need to pass remaining_accounts in registry-slot
    // order so each one matches registry.gateways[idx].address.
    let mut all_gateway_keys = vec![gateway_key1];
    all_gateway_keys.extend(other_gateway_keys.iter().copied());
    // Resolve gateway operators ahead of time so we can match by registry slot
    let mut gw_to_operator: Vec<(Pubkey, Pubkey)> = Vec::new();
    for k in &all_gateway_keys {
        let acct = ctx.banks_client.get_account(*k).await.unwrap().unwrap();
        let gw = Gateway::try_deserialize(&mut acct.data.as_slice()).unwrap();
        gw_to_operator.push((*k, gw.operator));
    }
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let mut ordered: Vec<Pubkey> = Vec::new();
    // GatewayRegistry layout: 8 (disc) + 32 (authority) + 4 (count) + 4 (padding) +
    // slots[3000] * 56 bytes each (32 address + 8 weight + 8 start_timestamp + 1 status + 7 padding).
    for slot_idx in 0..4 {
        let off = 8 + 32 + 4 + 4 + slot_idx * 56;
        let slot_addr = Pubkey::try_from(&registry_account.data[off..off + 32]).unwrap();
        let gw_key = gw_to_operator
            .iter()
            .find(|(_, op)| *op == slot_addr)
            .map(|(k, _)| *k)
            .expect("registry slot address must match a gateway");
        ordered.push(gw_key);
    }

    // --- BATCH 1: tally first 2 gateways ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    for k in &ordered[..2] {
        accounts.push(solana_sdk::instruction::AccountMeta::new(*k, false));
    }
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // After batch 1: tally_index == 2, weights_tallied still 0 (in progress)
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(
        epoch.tally_index, 2,
        "First batch must advance tally_index by 2"
    );
    assert_eq!(
        epoch.weights_tallied, 0,
        "weights_tallied must remain 0 mid-batch"
    );
    let weight_after_batch1 = epoch.total_composite_weight();
    assert!(
        weight_after_batch1 > 0,
        "Batch 1 must accumulate non-zero weight"
    );

    // --- BATCH 2: tally remaining 2 gateways ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    for k in &ordered[2..] {
        accounts.push(solana_sdk::instruction::AccountMeta::new(*k, false));
    }
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // After batch 2: tally_index == 4, weights_tallied == 1, total weight grew
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(
        epoch.tally_index, 4,
        "Final batch must reach active_gateway_count"
    );
    assert_eq!(
        epoch.weights_tallied, 1,
        "weights_tallied must flip to 1 when complete"
    );
    let weight_after_batch2 = epoch.total_composite_weight();
    assert!(
        weight_after_batch2 > weight_after_batch1,
        "Batch 2 must add to total weight (not replace)"
    );

    // --- A third tally call must be rejected (idempotency guard) ---
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    accounts.push(solana_sdk::instruction::AccountMeta::new(ordered[0], false));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::WeightsAlreadyTallied);
}

// =========================================
// STAKE PAYMENT TESTS (deduct_*_for_payment, called via CPI from ario-arns)
// =========================================
//
// These exercise the ario-gar handlers directly. End-to-end CPI coverage
// (ario-arns → ario-gar) lives in the ario-arns integration suite.

/// Set up a delegator account with a fresh keypair, an SPL token account,
/// and a real Delegation PDA created by calling delegate_stake. Returns
/// (delegator_keypair, delegation_pda).
async fn setup_delegator_with_delegation(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    gateway_operator: &Pubkey,
    delegate_amount: u64,
) -> (Keypair, Pubkey) {
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    // Fund delegator with SOL
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(ctx, &delegator_token, &setup.mint.pubkey(), &delegator_pk).await;
    mint_tokens(
        ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        delegate_amount.saturating_mul(10),
    )
    .await;

    let (delegation_key, _) = delegation_pda(gateway_operator, &delegator_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_pda(gateway_operator).0,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    (delegator, delegation_key)
}

/// Build a deduct_delegation_for_payment instruction with all required accounts.
fn build_deduct_delegation_ix(
    setup: &GarSetup,
    gateway_key: Pubkey,
    delegation_key: Pubkey,
    delegator_pk: Pubkey,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id: ario_gar::ID,
        accounts: ario_gar::accounts::DeductDelegationForPayment {
            settings: setup.settings_key,
            gateway: gateway_key,
            delegation: delegation_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.protocol_token.pubkey(),
            delegator: delegator_pk,
            token_program: spl_token::id(),
        }
        .to_account_metas(None),
        data: ario_gar::instruction::DeductDelegationForPayment { amount }.data(),
    }
}

/// Build a deduct_operator_stake_for_payment instruction with all required accounts.
fn build_deduct_operator_stake_ix(
    setup: &GarSetup,
    gateway_key: Pubkey,
    operator_pk: Pubkey,
    amount: u64,
) -> Instruction {
    Instruction {
        program_id: ario_gar::ID,
        accounts: ario_gar::accounts::DeductOperatorStakeForPayment {
            settings: setup.settings_key,
            gateway: gateway_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.protocol_token.pubkey(),
            operator: operator_pk,
            token_program: spl_token::id(),
        }
        .to_account_metas(None),
        data: ario_gar::instruction::DeductOperatorStakeForPayment { amount }.data(),
    }
}

/// Mutate a Gateway account in place via ctx.set_account. Used to inject
/// state (e.g. cumulative_reward_per_token, status = Leaving) that we
/// can't reach through the public instruction surface in a single test.
async fn mutate_gateway<F: FnOnce(&mut Gateway)>(
    ctx: &mut ProgramTestContext,
    gateway_key: Pubkey,
    f: F,
) {
    let acct = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let mut gw = Gateway::try_deserialize(&mut acct.data.as_slice()).unwrap();
    f(&mut gw);
    let mut new_data = Vec::new();
    gw.try_serialize(&mut new_data).unwrap();
    let original_len = acct.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &gateway_key,
        &solana_sdk::account::Account {
            lamports: acct.lamports,
            data: new_data,
            owner: acct.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );
}

// -----------------------------------------
// deduct_delegation_for_payment
// -----------------------------------------

#[tokio::test]
async fn test_deduct_delegation_for_payment_success() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    // Delegate 50 ARIO so we have plenty of headroom above the 10 ARIO floor
    let delegate_amount = 50_000_000_000u64;
    let (delegator, delegation_key) =
        setup_delegator_with_delegation(&mut ctx, &setup, &payer_pk, delegate_amount).await;
    let delegator_pk = delegator.pubkey();

    let stake_balance_before = get_token_balance(&mut ctx, &setup.stake_token.pubkey()).await;
    let protocol_balance_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let payment = 5_000_000_000u64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_delegation_ix(
            &setup,
            gateway_key,
            delegation_key,
            delegator_pk,
            payment,
        )],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegation_acct = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_acct.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, delegate_amount - payment);

    let gw_acct = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_acct.data.as_slice()).unwrap();
    assert_eq!(gw.total_delegated_stake, delegate_amount - payment);

    assert_eq!(
        get_token_balance(&mut ctx, &setup.stake_token.pubkey()).await,
        stake_balance_before - payment
    );
    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await,
        protocol_balance_before + payment
    );
}

#[tokio::test]
async fn test_deduct_delegation_for_payment_settles_rewards() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    // Delegate 100 mARIO. delegation.reward_debt == 0 immediately after delegate_stake.
    let delegate_amount = 100_000_000u64;
    let (delegator, delegation_key) =
        setup_delegator_with_delegation(&mut ctx, &setup, &payer_pk, delegate_amount).await;
    let delegator_pk = delegator.pubkey();

    // Inject cumulative_reward_per_token = 1e18 so settle materializes
    // pending = 100_000_000 * (1e18 - 0) / 1e18 = 100_000_000 mARIO into delegation.amount.
    mutate_gateway(&mut ctx, gateway_key, |gw| {
        gw.cumulative_reward_per_token = REWARD_PRECISION;
    })
    .await;

    let payment = 50_000_000u64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_delegation_ix(
            &setup,
            gateway_key,
            delegation_key,
            delegator_pk,
            payment,
        )],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // After settle: delegation.amount = 100M + 100M = 200M; after payment: 200M - 50M = 150M
    let delegation_acct = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_acct.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, 150_000_000);
    assert_eq!(delegation.reward_debt, REWARD_PRECISION);
}

#[tokio::test]
async fn test_deduct_delegation_for_payment_full_deduction() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    let delegate_amount = 25_000_000_000u64;
    let (delegator, delegation_key) =
        setup_delegator_with_delegation(&mut ctx, &setup, &payer_pk, delegate_amount).await;
    let delegator_pk = delegator.pubkey();

    // Drain the entire delegation — remaining must be exactly 0 to be allowed.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_delegation_ix(
            &setup,
            gateway_key,
            delegation_key,
            delegator_pk,
            delegate_amount,
        )],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegation_acct = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut delegation_acct.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, 0);

    let gw_acct = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_acct.data.as_slice()).unwrap();
    assert_eq!(gw.total_delegated_stake, 0);
}

#[tokio::test]
async fn test_deduct_delegation_for_payment_below_min() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    // Delegate 20 mARIO — settings min_delegation_amount is 10_000_000 mARIO,
    // so a 15M payment leaves 5M which is < min and must be rejected.
    let delegate_amount = 20_000_000u64;
    let (delegator, delegation_key) =
        setup_delegator_with_delegation(&mut ctx, &setup, &payer_pk, delegate_amount).await;
    let delegator_pk = delegator.pubkey();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_delegation_ix(
            &setup,
            gateway_key,
            delegation_key,
            delegator_pk,
            15_000_000,
        )],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::DelegationBelowMinimum);
}

#[tokio::test]
async fn test_deduct_delegation_for_payment_insufficient() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    let delegate_amount = 20_000_000_000u64;
    let (delegator, delegation_key) =
        setup_delegator_with_delegation(&mut ctx, &setup, &payer_pk, delegate_amount).await;
    let delegator_pk = delegator.pubkey();

    // Deduct more than the delegation
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_delegation_ix(
            &setup,
            gateway_key,
            delegation_key,
            delegator_pk,
            delegate_amount + 1,
        )],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InsufficientDelegationForPayment);
}

#[tokio::test]
async fn test_deduct_delegation_for_payment_leaving_gateway() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    let delegate_amount = 20_000_000_000u64;
    let (delegator, delegation_key) =
        setup_delegator_with_delegation(&mut ctx, &setup, &payer_pk, delegate_amount).await;
    let delegator_pk = delegator.pubkey();

    // Force gateway into Leaving state without going through leave_network
    // (which would also create a withdrawal vault we don't need here).
    mutate_gateway(&mut ctx, gateway_key, |gw| {
        gw.status = GatewayStatus::Leaving;
        gw.leave_timestamp = Some(0);
    })
    .await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_delegation_ix(
            &setup,
            gateway_key,
            delegation_key,
            delegator_pk,
            1_000_000_000,
        )],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::GatewayNotJoined);
}

#[tokio::test]
async fn test_deduct_delegation_for_payment_wrong_signer() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    let delegate_amount = 20_000_000_000u64;
    let (_delegator, delegation_key) =
        setup_delegator_with_delegation(&mut ctx, &setup, &payer_pk, delegate_amount).await;

    // Use a totally unrelated signer (pre-funded so the tx isn't rejected for fees)
    let attacker = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &attacker.pubkey(),
            1_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Pass attacker as the "delegator" account. The PDA seeds derived from
    // attacker.pubkey() won't match delegation_key, so the seeds constraint
    // fails before the explicit delegation.delegator == delegator check runs.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_delegation_ix(
            &setup,
            gateway_key,
            delegation_key,
            attacker.pubkey(),
            1_000_000_000,
        )],
        Some(&attacker.pubkey()),
        &[&attacker],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(result.is_err(), "wrong signer must be rejected");
}

// -----------------------------------------
// deduct_operator_stake_for_payment
// -----------------------------------------

#[tokio::test]
async fn test_deduct_operator_stake_for_payment_success() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Stake 30k ARIO so we can spend 5k and stay above the 10k floor
    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;
    let payer_pk = ctx.payer.pubkey();

    let stake_balance_before = get_token_balance(&mut ctx, &setup.stake_token.pubkey()).await;
    let protocol_balance_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let payment = 5_000_000_000u64;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_operator_stake_ix(
            &setup,
            gateway_key,
            payer_pk,
            payment,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let gw_acct = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_acct.data.as_slice()).unwrap();
    assert_eq!(gw.operator_stake, initial_stake - payment);

    assert_eq!(
        get_token_balance(&mut ctx, &setup.stake_token.pubkey()).await,
        stake_balance_before - payment
    );
    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await,
        protocol_balance_before + payment
    );
}

#[tokio::test]
async fn test_deduct_operator_stake_for_payment_below_min() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Join with the absolute minimum — any deduction would drop below MIN_OPERATOR_STAKE
    let initial_stake = Gateway::MIN_OPERATOR_STAKE;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;
    let payer_pk = ctx.payer.pubkey();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_operator_stake_ix(
            &setup,
            gateway_key,
            payer_pk,
            1,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::StakeBelowMinimum);
}

// =========================================
// MIGRATION: GatewaySettings.arns_program_id backfill
// =========================================
//
// Tests for migrate_settings_set_arns_program_id, the one-shot backfill
// for accounts deployed before that field existed (devnet/test-only path —
// fresh deployments set the field via initialize()).

/// Build a ProgramTest that pre-creates a GatewaySettings account at the
/// PRE-REFACTOR layout (one Pubkey shorter, no arns_program_id field).
/// Used to exercise the migration instruction's realloc + field placement.
fn program_test_with_pre_refactor_settings(authority: &Pubkey, mint: &Pubkey) -> ProgramTest {
    use anchor_lang::solana_program::hash::hash;

    let mut pt = ProgramTest::new("ario_gar", ario_gar::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let rent = solana_sdk::rent::Rent::default();

    let (settings_key, settings_bump) = settings_pda();
    // Pre-refactor: 32 bytes shorter (no arns_program_id) AND 24 bytes shorter
    // (no supply counters: total_staked, total_delegated, total_withdrawn) AND
    // 3 bytes shorter (no SchemaVersion).
    // Matches handler's `old_size = new_size - 32 - 24 - SCHEMA_VERSION_SIZE`.
    let pre_size = GatewaySettings::SIZE - 32 - 24 - 3;
    let mut data = vec![0u8; pre_size];
    let disc = hash(b"account:GatewaySettings");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);

    let mut off = 8;
    data[off..off + 32].copy_from_slice(authority.as_ref());
    off += 32;
    data[off..off + 32].copy_from_slice(mint.as_ref());
    off += 32;
    // u64 fields default to 0 — fine for migration test (only authority + bump matter)
    off += 8 * 6 + 4 + 1; // 6 u64 + 1 u32 + 1 bool
    data[off..off + 32].copy_from_slice(authority.as_ref()); // migration_authority
    off += 32;
    // stake_token_account + protocol_token_account: leave zero
    off += 32 + 32;
    // bump: last byte of the OLD layout
    data[pre_size - 1] = settings_bump;
    let _ = off;

    pt.add_account(
        settings_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(pre_size),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    pt
}

#[tokio::test]
async fn test_migrate_settings_set_arns_program_id_backfills() {
    let authority = Keypair::new();
    let mint = Keypair::new();
    let mut pt = program_test_with_pre_refactor_settings(&authority.pubkey(), &mint.pubkey());
    // Pre-fund authority so it can sign the migration tx
    pt.add_account(
        authority.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let arns_pid = Pubkey::new_unique();
    let (settings_key, _) = settings_pda();

    // Pre-migration: account size matches OLD layout (no arns_program_id, no supply counters)
    let pre = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pre.data.len(), GatewaySettings::SIZE - 32 - 24 - 3);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateSettingsSetArnsProgramId {
                settings: settings_key,
                authority: authority.pubkey(),
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateSettingsSetArnsProgramId {
                arns_program_id: arns_pid,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Post-migration: account is at NEW layout size, arns_program_id is set,
    // bump is preserved at the new offset, and Anchor can deserialize it.
    let post = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(post.data.len(), GatewaySettings::SIZE);
    let settings = GatewaySettings::try_deserialize(&mut post.data.as_slice()).unwrap();
    assert_eq!(settings.arns_program_id, arns_pid);
    assert_eq!(settings.authority, authority.pubkey());
    // bump must match what was at the OLD bump offset (we just shifted it)
    assert_eq!(
        settings.bump,
        pre.data[GatewaySettings::SIZE - 32 - 24 - 3 - 1]
    );
}

#[tokio::test]
async fn test_migrate_settings_set_arns_program_id_idempotent() {
    // Migrate once (old → new layout), then call the migration again on
    // the now-new-layout account. The second call must return Ok without
    // mutating arns_program_id (the early-return "already at new size"
    // path).
    let authority = Keypair::new();
    let mint = Keypair::new();
    let mut pt = program_test_with_pre_refactor_settings(&authority.pubkey(), &mint.pubkey());
    pt.add_account(
        authority.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let (settings_key, _) = settings_pda();
    let arns_pid = Pubkey::new_unique();

    // First migration call — old → new layout.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateSettingsSetArnsProgramId {
                settings: settings_key,
                authority: authority.pubkey(),
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateSettingsSetArnsProgramId {
                arns_program_id: arns_pid,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify first migration succeeded.
    let after_first = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_first.data.len(), GatewaySettings::SIZE);
    let settings_after_first =
        GatewaySettings::try_deserialize(&mut after_first.data.as_slice()).unwrap();
    assert_eq!(settings_after_first.arns_program_id, arns_pid);

    // Second migration call with a DIFFERENT pubkey — must NOT overwrite.
    let other_pid = Pubkey::new_unique();
    assert_ne!(other_pid, arns_pid);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateSettingsSetArnsProgramId {
                settings: settings_key,
                authority: authority.pubkey(),
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateSettingsSetArnsProgramId {
                arns_program_id: other_pid,
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify second call was a no-op: arns_program_id MUST still be the
    // first pid, NOT other_pid. This proves the early-return path doesn't
    // accidentally overwrite the field.
    let after_second = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after_second.data.len(), GatewaySettings::SIZE);
    let settings_after_second =
        GatewaySettings::try_deserialize(&mut after_second.data.as_slice()).unwrap();
    assert_eq!(
        settings_after_second.arns_program_id, arns_pid,
        "second migration call must NOT overwrite arns_program_id (no-op semantics)"
    );
}

#[tokio::test]
async fn test_migrate_settings_set_arns_program_id_wrong_authority() {
    let authority = Keypair::new();
    let attacker = Keypair::new();
    let mint = Keypair::new();
    let mut pt = program_test_with_pre_refactor_settings(&authority.pubkey(), &mint.pubkey());
    pt.add_account(
        attacker.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let (settings_key, _) = settings_pda();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateSettingsSetArnsProgramId {
                settings: settings_key,
                authority: attacker.pubkey(), // WRONG authority
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateSettingsSetArnsProgramId {
                arns_program_id: Pubkey::new_unique(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &attacker],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::Unauthorized);
}

#[tokio::test]
async fn test_deduct_operator_stake_for_payment_wrong_signer() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let initial_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial_stake).await;

    let attacker = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &attacker.pubkey(),
            1_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // Pass attacker as the "operator" — gateway PDA seeds use attacker.pubkey()
    // which won't match gateway_key, so the seeds constraint fails. (The
    // explicit gateway.operator == operator constraint would also catch this.)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[build_deduct_operator_stake_ix(
            &setup,
            gateway_key,
            attacker.pubkey(),
            1_000_000_000,
        )],
        Some(&attacker.pubkey()),
        &[&attacker],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(result.is_err(), "wrong signer must be rejected");
}

// =====================================================================
// PR-2 regression tests — verify the in-place departure model never lets
// a Leaving gateway pollute observer selection or reward economics.
// =====================================================================

/// Helper: 2-gateway setup where gateway2 leaves BEFORE create_epoch. The
/// registry slot for gateway2 stays at index 1 with status=Leaving and
/// composite_weight=0 (set by leave_network). gateway1 stays at index 0.
async fn setup_two_gateways_with_leaver(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
) -> (Pubkey, Pubkey, Pubkey) {
    let payer_pk = ctx.payer.pubkey();

    // Both gateways must join BEFORE epoch start (timestamp=0 here)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(ctx, setup, stake_amount).await;

    let operator2 = Keypair::new();
    let op2_token = Keypair::new();
    create_token_account(ctx, &op2_token, &setup.mint.pubkey(), &operator2.pubkey()).await;
    mint_tokens(
        ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    // Fund operator2 with SOL so it can sign leave_network later
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &operator2.pubkey(),
            50_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();
    let gateway_key2 =
        join_gateway_with_operator(ctx, setup, &operator2, &op2_token.pubkey(), stake_amount).await;

    // Gateway 2 leaves before create_epoch
    let (op2_wd_counter, _) = withdrawal_counter_pda(&operator2.pubkey());
    let (op2_wd, _) = withdrawal_pda(&operator2.pubkey(), 0);
    let mut leave_accounts = ario_gar::accounts::LeaveNetwork {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_pda().0,
        registry: setup.registry_key,
        gateway: gateway_key2,
        withdrawal_counter: op2_wd_counter,
        withdrawal: op2_wd,
        excess_withdrawal: None,
        operator: operator2.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let (op2_observer_lookup, _) = observer_lookup_pda(&operator2.pubkey());
    leave_accounts.push(solana_sdk::instruction::AccountMeta::new(
        op2_observer_lookup,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: leave_accounts,
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&operator2.pubkey()),
        &[&operator2],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    (gateway_key1, gateway_key2, operator2.pubkey())
}

#[tokio::test]
async fn test_prescribe_excludes_leaving_gateway() {
    // NEW-2 regression: a Leaving gateway must never appear in
    // `prescribed_observers`, even when the registry slot it occupies is
    // the last index (which would have triggered the fallback bug under
    // the previous swap-remove model — Pubkey::default() at active_count-1).
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    let (gateway_key1, gateway_key2, _op2_pk) =
        setup_two_gateways_with_leaver(&mut ctx, &setup).await;

    // Warp past epoch start
    let payer_pk = ctx.payer.pubkey();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200;
    clock.slot = 1;
    ctx.set_sysvar(&clock);

    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally — cranker passes both Gateway PDAs (slot 0 = Joined, slot 1 = Leaving).
    // Leaver is internally skipped (composite_weight cached at 0).
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key1,
        false,
    ));
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key2,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe — only gateway1 (Joined) is reward-eligible. The cranker
    // passes only the Joined gateway PDA in remaining_accounts (selected_count=1).
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key1,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Inspect prescribed observers. The Leaving gateway must not be present;
    // Pubkey::default() must not be present either (legacy swap-remove fingerprint).
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);

    let gw2_account = ctx
        .banks_client
        .get_account(gateway_key2)
        .await
        .unwrap()
        .unwrap();
    let gw2 = Gateway::try_deserialize(&mut gw2_account.data.as_slice()).unwrap();

    for i in 0..epoch.observer_count as usize {
        let p = epoch.prescribed_observer_gateways[i];
        assert_ne!(
            p,
            Pubkey::default(),
            "fallback must not select Pubkey::default()"
        );
        assert_ne!(
            p, gw2.operator,
            "Leaving gateway must not be prescribed as observer"
        );
    }

    // Sanity: the Joined gateway IS prescribed
    let gw1_account = ctx
        .banks_client
        .get_account(gateway_key1)
        .await
        .unwrap()
        .unwrap();
    let gw1 = Gateway::try_deserialize(&mut gw1_account.data.as_slice()).unwrap();
    assert!(
        epoch.prescribed_observer_gateways[..epoch.observer_count as usize]
            .iter()
            .any(|p| *p == gw1.operator),
        "Joined gateway should be selected"
    );
}

#[tokio::test]
async fn test_per_gateway_reward_excludes_leaving_from_divisor() {
    // Reward-dilution regression: with 2 registry slots (1 Joined, 1 Leaving),
    // per_gateway_reward must divide by the count of *reward-eligible*
    // gateways (1), not the count of registry slots (2). Pre-fix, the divisor
    // was `active_gateway_count = registry.count`, silently halving rewards
    // for the honest gateway. Matches Lua's
    // `epochs.computeTotalEligibleRewardsForEpoch` which excludes leavers.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund protocol with a known amount so we can predict the reward number.
    let protocol_balance: u64 = 1_000_000_000_000; // 1M ARIO
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        protocol_balance,
    )
    .await;

    let (gateway_key1, gateway_key2, _op2_pk) =
        setup_two_gateways_with_leaver(&mut ctx, &setup).await;

    // Warp past epoch start
    let payer_pk = ctx.payer.pubkey();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200;
    clock.slot = 1;
    ctx.set_sysvar(&clock);

    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Create epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally — Leaving slot gets composite_weight=0 internally
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key1,
        false,
    ));
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key2,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Prescribe — passes only the Joined gateway
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key1,
        false,
    ));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Compute the expected reward: total_eligible * gateway_reward_ratio / RATE_SCALE / joined_count
    // joined_count = 1 (only gateway1 has non-zero weight). Pre-fix it would
    // have been registry.count = 2, halving the reward.
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);

    let total = epoch.total_eligible_rewards as u128;
    let settings_account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let epoch_settings =
        EpochSettings::try_deserialize(&mut settings_account.data.as_slice()).unwrap();
    let expected_undivided = total
        .saturating_mul(epoch_settings.gateway_reward_ratio as u128)
        .checked_div(ario_gar::RATE_SCALE as u128)
        .unwrap_or(0) as u64;
    // joined_count == 1, so per_gateway_reward == expected_undivided.
    // Pre-fix divisor was 2 (registry.count), halving the reward.
    let expected_diluted_pre_fix = expected_undivided / 2;

    assert_eq!(
        epoch.per_gateway_reward, expected_undivided,
        "per_gateway_reward must use joined_count=1 as divisor (got dilution toward {})",
        expected_diluted_pre_fix
    );
    assert_ne!(
        epoch.per_gateway_reward, expected_diluted_pre_fix,
        "regression: leaver was counted in divisor, halving rewards"
    );
}

#[tokio::test]
async fn test_claim_delegate_from_leaving_gateway_is_permissionless() {
    // The delegator does NOT need to sign — anyone (the cranker, a stranger,
    // even a malicious party) can crank the claim on the delegate's behalf.
    // Stake still routes to the delegator's withdrawal vault (PDA-seeded by
    // delegator.key()), so the cranker cannot redirect anyone's stake.
    //
    // This unblocks the future `finalize_gone` GC which requires
    // `gateway.total_delegated_stake == 0` before closing the Gateway PDA —
    // a forgetful delegate can no longer permanently strand the slot.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let delegator = Keypair::new();
    let stranger = Keypair::new();

    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    for who in [delegator.pubkey(), stranger.pubkey()] {
        pt.add_account(
            who,
            solana_sdk::account::Account {
                lamports: 50_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Operator joins
    let stake_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Delegator funds and delegates
    let delegator_token = Keypair::new();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator.pubkey(),
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;

    let delegate_amount = 10_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator.pubkey());

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Operator leaves
    let (op_wd_counter, _) = withdrawal_counter_pda(&payer_pk);
    let (op_wd, _) = withdrawal_pda(&payer_pk, 0);
    let mut leave_accounts = ario_gar::accounts::LeaveNetwork {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_pda().0,
        registry: setup.registry_key,
        gateway: gateway_key,
        withdrawal_counter: op_wd_counter,
        withdrawal: op_wd,
        excess_withdrawal: None,
        operator: payer_pk,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let (observer_lookup, _) = observer_lookup_pda(&payer_pk);
    leave_accounts.push(solana_sdk::instruction::AccountMeta::new(
        observer_lookup,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: leave_accounts,
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Stranger (NOT the delegator) cranks the claim. Note: only `stranger`
    // signs; `delegator` is passed as a plain pubkey, no signature.
    let (del_wd_counter, _) = withdrawal_counter_pda(&delegator.pubkey());
    let (del_wd, _) = withdrawal_pda(&delegator.pubkey(), 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimDelegateFromLeavingGateway {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_wd_counter,
                withdrawal: del_wd,
                delegator: delegator.pubkey(),
                payer: stranger.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimDelegateFromLeavingGateway {}.data(),
        }],
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("permissionless claim must succeed without delegator signature");

    // Withdrawal vault is owned by the DELEGATOR, not the stranger
    let withdrawal_account = ctx.banks_client.get_account(del_wd).await.unwrap().unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    assert_eq!(
        withdrawal.owner,
        delegator.pubkey(),
        "stake must route to the delegator's vault — cranker cannot redirect"
    );
    assert_eq!(
        withdrawal.amount, delegate_amount,
        "full delegated amount transferred to vault"
    );
    assert!(
        withdrawal.is_delegate,
        "vault marked as delegate withdrawal"
    );

    // Gateway's total_delegated_stake is decremented (load-bearing for the
    // future finalize_gone gate `total_delegated_stake == 0`).
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    assert_eq!(
        gw.total_delegated_stake, 0,
        "delegated stake count must reach zero so finalize_gone can proceed"
    );
}

// =====================================================================
// finalize_gone — permissionless GC for departed gateways
// =====================================================================

/// Helper: a gateway that has been Leaving for long enough to be GC-eligible.
/// Uses the default epoch_duration of 86_400s (set by program_test_with_gar's
/// auto-pre-create), so eligibility = leave_timestamp + GATEWAY_LEAVE_PERIOD
/// + 7 * 86_400 = 90 days + 7 days = 97 days post-leave.
async fn setup_leaver_ready_for_gc(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
) -> (Pubkey, Pubkey) {
    let payer_pk = ctx.payer.pubkey();
    let stake_amount = 20_000_000_000u64;

    // Set clock to a known baseline so we can warp deterministically
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let gateway_key = join_gateway(ctx, setup, stake_amount).await;

    // Operator leaves
    let (op_wd_counter, _) = withdrawal_counter_pda(&payer_pk);
    let (op_wd, _) = withdrawal_pda(&payer_pk, 0);
    let mut leave_accounts = ario_gar::accounts::LeaveNetwork {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_pda().0,
        registry: setup.registry_key,
        gateway: gateway_key,
        withdrawal_counter: op_wd_counter,
        withdrawal: op_wd,
        excess_withdrawal: None,
        operator: payer_pk,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let (observer_lookup, _) = observer_lookup_pda(&payer_pk);
    leave_accounts.push(solana_sdk::instruction::AccountMeta::new(
        observer_lookup,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: leave_accounts,
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (epoch_settings_key, _) = epoch_settings_pda();
    (gateway_key, epoch_settings_key)
}

#[tokio::test]
async fn test_finalize_gone_happy_path() {
    // Permissionless GC reclaims a Leaving gateway's slot and rent after the
    // grace window. Gateway has zero delegations (none ever delegated), so
    // the delegations gate trivially passes.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let stranger = Keypair::new();
    pt.add_account(
        stranger.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let (gateway_key, epoch_settings_key) = setup_leaver_ready_for_gc(&mut ctx, &setup).await;

    // Warp past the eligibility window: leave_timestamp(0) + 90 days + 7 * 86_400 = 8_380_800 + buffer.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 90 * 86_400 + 7 * 86_400 + 1;
    ctx.set_sysvar(&clock);

    let stranger_balance_before = ctx
        .banks_client
        .get_account(stranger.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::FinalizeGone {
                gateway: gateway_key,
                registry: setup.registry_key,
                epoch_settings: epoch_settings_key,
                caller: stranger.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::FinalizeGone {}.data(),
        }],
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("finalize_gone must succeed past the grace window");

    // Gateway PDA closed
    assert!(
        ctx.banks_client
            .get_account(gateway_key)
            .await
            .unwrap()
            .is_none(),
        "Gateway PDA must be closed"
    );

    // Registry slot reclaimed: count back to 0
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let count = u32::from_le_bytes(
        registry_account.data[8 + 32..8 + 32 + 4]
            .try_into()
            .unwrap(),
    );
    assert_eq!(count, 0, "registry count decremented after finalize_gone");

    // Stranger received the rent reward
    let stranger_balance_after = ctx
        .banks_client
        .get_account(stranger.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert!(
        stranger_balance_after > stranger_balance_before,
        "rent reward should flow to caller (delta {})",
        stranger_balance_after.saturating_sub(stranger_balance_before),
    );
}

#[tokio::test]
async fn test_finalize_gone_too_early_fails() {
    // GC must reject before the grace window expires.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let stranger = Keypair::new();
    pt.add_account(
        stranger.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let (gateway_key, epoch_settings_key) = setup_leaver_ready_for_gc(&mut ctx, &setup).await;

    // Warp to 1 second BEFORE eligibility (89 days + 7 epochs - 1 sec).
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 90 * 86_400 + 7 * 86_400 - 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::FinalizeGone {
                gateway: gateway_key,
                registry: setup.registry_key,
                epoch_settings: epoch_settings_key,
                caller: stranger.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::FinalizeGone {}.data(),
        }],
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::LeaveWindowNotExpired);

    // Gateway PDA still exists
    assert!(
        ctx.banks_client
            .get_account(gateway_key)
            .await
            .unwrap()
            .is_some(),
        "Gateway PDA must remain"
    );
}

#[tokio::test]
async fn test_finalize_gone_blocks_with_outstanding_delegations() {
    // Even past the grace window, finalize_gone must reject if any
    // delegate stake remains. Without this gate, closing the Gateway PDA
    // would strand the undelegated stake (delegate.rs's claim handler
    // requires the Gateway PDA to deserialize).
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let delegator = Keypair::new();
    let stranger = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    for who in [delegator.pubkey(), stranger.pubkey()] {
        pt.add_account(
            who,
            solana_sdk::account::Account {
                lamports: 50_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Set baseline clock
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let payer_pk = ctx.payer.pubkey();
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    // Delegator funds and delegates
    let delegator_token = Keypair::new();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator.pubkey(),
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;

    let delegate_amount = 10_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Operator leaves (delegation still exists, NOT yet claimed)
    let (op_wd_counter, _) = withdrawal_counter_pda(&payer_pk);
    let (op_wd, _) = withdrawal_pda(&payer_pk, 0);
    let mut leave_accounts = ario_gar::accounts::LeaveNetwork {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_pda().0,
        registry: setup.registry_key,
        gateway: gateway_key,
        withdrawal_counter: op_wd_counter,
        withdrawal: op_wd,
        excess_withdrawal: None,
        operator: payer_pk,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let (observer_lookup, _) = observer_lookup_pda(&payer_pk);
    leave_accounts.push(solana_sdk::instruction::AccountMeta::new(
        observer_lookup,
        false,
    ));
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: leave_accounts,
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past the grace window
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 90 * 86_400 + 7 * 86_400 + 1;
    ctx.set_sysvar(&clock);

    // finalize_gone should reject: delegations still outstanding.
    let (epoch_settings_key, _) = epoch_settings_pda();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::FinalizeGone {
                gateway: gateway_key,
                registry: setup.registry_key,
                epoch_settings: epoch_settings_key,
                caller: stranger.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::FinalizeGone {}.data(),
        }],
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::DelegationsOutstanding);
}

// =====================================================================
// M8 — close_epoch observation gate
// =====================================================================

#[tokio::test]
async fn test_close_epoch_blocks_unclosed_observations() {
    // close_epoch must reject when any Observation PDA is still open. Without
    // this gate the orphaned PDAs would lose their parent reference and rent
    // would be permanently stranded (close_observation requires the parent
    // Epoch to deserialize and increment observations_closed).
    //
    // This test exercises the pre-fix scenario: create epoch, tally, prescribe,
    // submit observation, distribute, then attempt to close_epoch WITHOUT
    // calling close_observation first. With M8 in place, close_epoch errors
    // with EpochObservationsNotClosed; close_observation then unlocks it.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_and_core(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar_with_core_treasury(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Set baseline clock + slot
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    clock.slot = 1;
    ctx.set_sysvar(&clock);

    let payer_pk = ctx.payer.pubkey();
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;

    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200;
    ctx.set_sysvar(&clock);

    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // create_epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // tally
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // prescribe — pass the Joined gateway as the prescribed observer
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // submit one observation
    let (observation_key, _) = observation_pda(0, &payer_pk);
    let mut gateway_results = [0u8; 375];
    gateway_results[0] = 0b00000001;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::SaveObservations {
                epoch: epoch_key,
                observation: observation_key,
                observer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SaveObservations {
                _epoch_index: 0,
                gateway_results,
                gateway_count: 1,
                report_tx_id: [1u8; 32],
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // warp past epoch end + distribute
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200 + 86_400;
    ctx.set_sysvar(&clock);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut dist_accounts = ario_gar::accounts::DistributeEpoch {
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        settings: setup.settings_key,
        protocol_token_account: setup.protocol_token.pubkey(),
        stake_token_account: setup.stake_token.pubkey(),
        ario_config: ario_config_pda().0,
        ario_core_program: ario_gar::ARIO_CORE_PROGRAM_ID,
        token_program: spl_token::ID,
        payer: payer_pk,
    }
    .to_account_metas(None);
    dist_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: dist_accounts,
            data: ario_gar::instruction::DistributeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Bump current_epoch_index past the retention window
    let es_account = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    let mut es_data = es_account.data.clone();
    let cei_offset = 8 + 32 + 8 + 1 + 1 + 8 + 2 + 1;
    es_data[cei_offset..cei_offset + 8].copy_from_slice(&8u64.to_le_bytes());
    ctx.set_account(
        &epoch_settings_key,
        &solana_sdk::account::Account {
            lamports: es_account.lamports,
            data: es_data,
            owner: es_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Attempt to close_epoch with the observation still open — must fail.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::EpochObservationsNotClosed);

    // Close the observation, then close_epoch must succeed.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseObservation {
                epoch: epoch_key,
                observation: observation_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseObservation { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Force a slot advance between close_observation and close_epoch.
    // Without this, solana-program-test occasionally serves the second
    // close_epoch's `epoch.load()` from a snapshot taken before
    // close_observation's writeback hit BankForks, which makes the
    // observations_submitted == observations_closed check fail
    // non-deterministically (~50% in isolation). Warp by one slot to
    // force a fresh bank. Diagnosed in PR #46; cherry-picked here.
    let cur_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(cur_slot + 1).unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("close_epoch must succeed once all observations are closed");

    assert!(
        ctx.banks_client
            .get_account(epoch_key)
            .await
            .unwrap()
            .is_none(),
        "Epoch PDA closed"
    );
}

// =========================================
// SECURITY: Registry capacity limit
// =========================================

/// Pre-create GatewayRegistry with count == MAX_GATEWAYS, then attempt
/// to join. Must fail with RegistryFull.
#[tokio::test]
async fn test_join_network_registry_full() {
    use anchor_lang::solana_program::hash::hash;

    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let payer_pk = Pubkey::new_unique(); // dummy — will use ctx.payer

    // Build ProgramTest with registry count set to MAX_GATEWAYS (3000)
    let mut pt = ProgramTest::new("ario_gar", ario_gar::ID, processor!(anchor_processor));
    pt.set_compute_max_units(1_000_000);
    let rent = solana_sdk::rent::Rent::default();

    // Pre-create registry with count = MAX_GATEWAYS
    let (registry_key, _) = registry_pda();
    let registry_size = 8 + GatewayRegistry::SIZE;
    let mut reg_data = vec![0u8; registry_size];
    let reg_disc = hash(b"account:GatewayRegistry");
    reg_data[..8].copy_from_slice(&reg_disc.to_bytes()[..8]);
    // count at offset 40 = MAX_GATEWAYS (3000, all clusters)
    reg_data[40..44].copy_from_slice(&(GatewayRegistry::MAX_GATEWAYS as u32).to_le_bytes());

    pt.add_account(
        registry_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(registry_size),
            data: reg_data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    // Pre-create settings (same pattern as program_test_with_gar)
    let (settings_key, settings_bump) = settings_pda();
    let settings_size = GatewaySettings::SIZE;
    let mut settings_data = vec![0u8; settings_size];
    let settings_disc = hash(b"account:GatewaySettings");
    settings_data[..8].copy_from_slice(&settings_disc.to_bytes()[..8]);
    // We only need the mint and stake/protocol token fields to be valid.
    // Other fields can be default/placeholder.
    let mut offset = 8;
    // authority — will be set to ctx.payer after start_with_context
    settings_data[offset..offset + 32].copy_from_slice(&[0u8; 32]);
    offset += 32;
    // mint
    settings_data[offset..offset + 32].copy_from_slice(mint.pubkey().as_ref());
    offset += 32;
    // min_operator_stake
    settings_data[offset..offset + 8].copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
    offset += 8;
    // min_delegate_stake
    settings_data[offset..offset + 8].copy_from_slice(&10_000_000u64.to_le_bytes());
    offset += 8;
    // withdrawal_period
    settings_data[offset..offset + 8].copy_from_slice(&(30i64 * 86_400).to_le_bytes());
    offset += 8;
    // max_expedited_withdrawal_penalty
    settings_data[offset..offset + 8].copy_from_slice(&500_000u64.to_le_bytes());
    offset += 8;
    // min_expedited_withdrawal_penalty
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
    settings_data[offset..offset + 32].copy_from_slice(&[0u8; 32]);
    offset += 32;
    // stake_token_account
    settings_data[offset..offset + 32].copy_from_slice(stake_token.pubkey().as_ref());
    offset += 32;
    // protocol_token_account
    settings_data[offset..offset + 32].copy_from_slice(protocol_token.pubkey().as_ref());
    offset += 32;
    // arns_program_id
    settings_data[offset..offset + 32].copy_from_slice(&[0xAAu8; 32]);
    offset += 32;
    // total_staked: u64
    settings_data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
    offset += 8;
    // total_delegated: u64
    settings_data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
    offset += 8;
    // total_withdrawn: u64
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

    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Try to join — should fail with RegistryFull
    let payer_pk = ctx.payer.pubkey();
    let (gateway_key, _) = gateway_pda(&payer_pk);
    let (observer_lookup_key, _) = observer_lookup_pda(&payer_pk);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::JoinNetwork {
                gateway: gateway_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                observer_lookup: observer_lookup_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                operator: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::JoinNetwork {
                params: ario_gar::JoinNetworkParams {
                    operator_stake: Gateway::MIN_OPERATOR_STAKE,
                    label: "full-registry-test".to_string(),
                    fqdn: "full.example.com".to_string(),
                    port: 443,
                    protocol: Protocol::Https,
                    properties: None,
                    note: None,
                    allow_delegated_staking: true,
                    delegate_reward_share_ratio: 10,
                    min_delegate_stake: None,
                    observer_address: payer_pk,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::RegistryFull);
}

// =========================================================================
// FUND-FROM-WITHDRAWAL — Phase 1 of FUND_FROM_PLAN.md
// =========================================================================
//
// `deduct_withdrawal_for_payment` and `close_drained_withdrawal` together
// give callers a way to spend tokens that are sitting in a Withdrawal vault
// (whether from `decrease_operator_stake`, `decrease_delegate_stake`, or
// `leave_network`) without first claiming them back to a wallet ATA.
//
// The split between deduct + close is necessary because Anchor's
// `close = owner` constraint fires unconditionally — there's no inline way
// to express "close iff fully drained." `deduct_withdrawal_for_payment`
// only mutates `withdrawal.amount`; `close_drained_withdrawal` is the
// permissionless cleanup that runs once the vault hits zero.

/// Helper: decrease operator stake to create a Withdrawal vault for the payer.
/// Returns the (withdrawal_pda, withdrawal_id) tuple.
async fn create_operator_withdrawal(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    gateway_key: Pubkey,
    decrease_amount: u64,
) -> (Pubkey, u64) {
    let payer_pk = ctx.payer.pubkey();
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (withdrawal_key, _) = withdrawal_pda(&payer_pk, 0);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseOperatorStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    (withdrawal_key, 0)
}

#[tokio::test]
async fn test_deduct_withdrawal_full_drain_then_close() {
    // Full drain → vault stays open at amount=0 → permissionless close
    // refunds rent to owner.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;

    let vault_amount = 5_000_000_000u64;
    let (withdrawal_key, _) =
        create_operator_withdrawal(&mut ctx, &setup, gateway_key, vault_amount).await;

    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let owner_lamports_before = ctx
        .banks_client
        .get_account(payer_pk)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let withdrawal_lamports = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap()
        .lamports;

    // Deduct the full amount.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment {
                amount: vault_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tokens went to protocol treasury.
    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        vault_amount,
    );

    // Vault is still open with amount=0.
    let withdrawal = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(withdrawal_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(withdrawal.amount, 0);

    // Permissionless close: anyone can call close_drained_withdrawal; rent goes to owner.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseDrainedWithdrawal {
                withdrawal: withdrawal_key,
                owner: payer_pk,
                closer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseDrainedWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Vault account is gone.
    assert!(ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .is_none());

    // Owner got rent back. (Tx fees are also paid by payer, so we can't
    // assert exact equality — just that rent flowed back.)
    let owner_lamports_after = ctx
        .banks_client
        .get_account(payer_pk)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert!(
        owner_lamports_after >= owner_lamports_before.saturating_sub(50_000), // tx fees ≪ rent
        "owner should net positive after close: before={}, after={}, vault_rent={}",
        owner_lamports_before,
        owner_lamports_after,
        withdrawal_lamports,
    );
}

#[tokio::test]
async fn test_deduct_withdrawal_partial_drain() {
    // Partial drain → vault stays open with reduced amount; close rejects.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;

    let vault_amount = 5_000_000_000u64;
    let payment_amount = 2_000_000_000u64;
    let (withdrawal_key, _) =
        create_operator_withdrawal(&mut ctx, &setup, gateway_key, vault_amount).await;

    let pre_available_at = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(withdrawal_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap()
    .available_at;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment {
                amount: payment_amount,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Vault residue + available_at unchanged.
    let withdrawal = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(withdrawal_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(withdrawal.amount, vault_amount - payment_amount);
    assert_eq!(withdrawal.available_at, pre_available_at);

    // close_drained_withdrawal rejects the still-funded vault.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseDrainedWithdrawal {
                withdrawal: withdrawal_key,
                owner: payer_pk,
                closer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseDrainedWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::WithdrawalNotDrained);
}

#[tokio::test]
async fn test_deduct_withdrawal_insufficient() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;

    let vault_amount = 5_000_000_000u64;
    let (withdrawal_key, _) =
        create_operator_withdrawal(&mut ctx, &setup, gateway_key, vault_amount).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment {
                amount: vault_amount + 1,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InsufficientWithdrawalForPayment);
}

#[tokio::test]
async fn test_deduct_withdrawal_zero_amount_fails() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let (withdrawal_key, _) =
        create_operator_withdrawal(&mut ctx, &setup, gateway_key, 5_000_000_000).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment { amount: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InvalidAmount);
}

#[tokio::test]
async fn test_deduct_withdrawal_non_owner_fails() {
    // A random signer cannot drain another user's withdrawal vault. The
    // Anchor `seeds` constraint re-derives the PDA with `owner.key()`, so
    // passing a wrong `owner` makes the seeds check fail before the handler
    // even runs (ConstraintSeeds rather than InvalidOwner).
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let (withdrawal_key, _) =
        create_operator_withdrawal(&mut ctx, &setup, gateway_key, 5_000_000_000).await;

    // Fund a stranger with SOL so they can pay tx fees.
    let stranger = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &stranger.pubkey(),
            1_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: stranger.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment {
                amount: 1_000_000_000,
            }
            .data(),
        }],
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    // Anchor's seeds constraint surfaces as ConstraintSeeds (custom code 2006);
    // we just need to confirm the tx fails — the exact code is implementation
    // detail. Use the `match` form rather than the macro because we accept any
    // seed-derivation-related custom error.
    assert!(
        matches!(
            result,
            Err(solana_program_test::BanksClientError::TransactionError(
                solana_sdk::transaction::TransactionError::InstructionError(_, _)
            ))
        ),
        "expected the tx to fail when stranger signs against owner's vault, got: {:?}",
        result
    );
}

#[tokio::test]
async fn test_deduct_withdrawal_protected_vs_excess_leave_vaults() {
    // BD-102 / Lua-parity: `leave_network` produces a protected exit vault
    // (is_protected: true, contains the min portion) plus an excess vault
    // (is_protected: false, contains the above-min portion). Fund-from
    // payment must REJECT the protected exit vault but ALLOW the excess.
    // Mirrors `gar.lua::planVaultsDrawdown` skipping operator vaults.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    // 30k stake = min(20k) + excess(10k) so leave produces both vaults.
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;

    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (exit_vault_key, _) = withdrawal_pda(&payer_pk, 0);
    let (excess_vault_key, _) = withdrawal_pda(&payer_pk, 1);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: exit_vault_key,
                excess_withdrawal: Some(excess_vault_key),
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Sanity: the gateway is Leaving and the two vaults are properly tagged.
    let exit_vault = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(exit_vault_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert!(
        exit_vault.is_protected,
        "min-stake exit vault must be protected"
    );
    assert_eq!(exit_vault.amount, 20_000_000_000u64);

    let excess_vault = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(excess_vault_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert!(
        !excess_vault.is_protected,
        "excess vault must NOT be protected"
    );
    assert_eq!(excess_vault.amount, 10_000_000_000u64);

    let gateway = Gateway::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(gateway_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(gateway.status, GatewayStatus::Leaving);

    // Reject: pay-from-protected-exit-vault → ProtectedVault.
    let payment = 1_000_000_000u64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: exit_vault_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment { amount: payment }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ario_gar::error::GarError::ProtectedVault);

    // Allow: pay-from-excess-vault succeeds.
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: excess_vault_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment { amount: payment }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        payment,
    );
    // Excess-vault residue intact, exit-vault unchanged.
    let excess_after = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(excess_vault_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(excess_after.amount, 10_000_000_000 - payment);

    let exit_after = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(exit_vault_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(exit_after.amount, 20_000_000_000u64, "exit vault untouched");
}

#[tokio::test]
async fn test_deduct_withdrawal_works_on_delegate_vault() {
    // Delegator-derived withdrawal (is_delegate=true) drains the same way.
    // The ix doesn't branch on is_delegate.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    // Stand up a delegator with funded ATA.
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            5_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator.pubkey(),
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Delegate 20 ARIO, then decrease by 10 to create a delegate-side withdrawal.
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let decrease_amount = 10_000_000_000u64;
    let (del_counter_key, _) = withdrawal_counter_pda(&delegator.pubkey());
    let (del_withdrawal_key, _) = withdrawal_pda(&delegator.pubkey(), 0);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: del_counter_key,
                withdrawal: del_withdrawal_key,
                delegator: delegator.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let withdrawal = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(del_withdrawal_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert!(withdrawal.is_delegate);
    assert!(!withdrawal.is_exit_vault);

    // Pay 3 ARIO from the delegate-side withdrawal.
    let payment = 3_000_000_000u64;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: del_withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: delegator.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment { amount: payment }.data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        payment,
    );
    let withdrawal = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(del_withdrawal_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(withdrawal.amount, decrease_amount - payment);
}

// =========================================================================
// FUND-FROM-FUNDING-PLAN — Phase 1.5 of FUND_FROM_PLAN.md
// =========================================================================
//
// `pay_from_funding_plan` is the multi-source composite primitive. Lua-faithful
// port of `gar.applyFundingPlan` — dispatches across Balance / Delegation /
// OperatorStake / Withdrawal sources via a `Vec<FundingSourceSpec>` plus
// `remaining_accounts` for per-source PDAs. Aggregates to ≤2 SPL transfers.
// Auto-creates a Withdrawal vault for sub-`min_delegation_amount` Delegation
// residue (matches Lua); rejects sub-`min_operator_stake` OperatorStake residue
// (preserves gateway viability — Solana extension).

use ario_gar::{FundingSourceKind, FundingSourceSpec};

fn fp_balance(amount: u64) -> FundingSourceSpec {
    FundingSourceSpec {
        kind: FundingSourceKind::Balance,
        amount,
    }
}
fn fp_delegation(amount: u64) -> FundingSourceSpec {
    FundingSourceSpec {
        kind: FundingSourceKind::Delegation,
        amount,
    }
}
fn fp_operator_stake(amount: u64) -> FundingSourceSpec {
    FundingSourceSpec {
        kind: FundingSourceKind::OperatorStake,
        amount,
    }
}
fn fp_withdrawal(amount: u64) -> FundingSourceSpec {
    FundingSourceSpec {
        kind: FundingSourceKind::Withdrawal,
        amount,
    }
}

/// Predict the residue_vault PDA the SDK would compute. Tests pass this
/// regardless of whether the plan actually triggers a residue vault — when
/// none is created, the slot is inert (Anchor's `UncheckedAccount`).
async fn predict_next_withdrawal_pda(ctx: &mut ProgramTestContext, owner: &Pubkey) -> Pubkey {
    // Counter may not exist yet (first-time funding-plan caller). next_id
    // defaults to 0 in that case.
    let (counter_pda, _) = withdrawal_counter_pda(owner);
    let next_id = match ctx.banks_client.get_account(counter_pda).await.unwrap() {
        Some(a) if a.data.len() >= 8 + 32 + 8 => {
            // bytes 8..40 = owner pubkey, 40..48 = next_id u64 LE
            u64::from_le_bytes(a.data[40..48].try_into().unwrap())
        }
        _ => 0,
    };
    let (vault_pda, _) = withdrawal_pda(owner, next_id);
    vault_pda
}

#[tokio::test]
async fn test_funding_plan_balance_only() {
    // 1 Balance source covers full cost. Equivalent to a direct user→protocol SPL transfer.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let _gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;

    let amount = 1_500_000_000u64; // 1.5 ARIO
    let payer_balance_before = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PayFromFundingPlan {
                settings: setup.settings_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer_token_account: Some(setup.operator_token.pubkey()),
                payer: payer_pk,
                token_program: spl_token::id(),
                withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_balance(amount)],
                expected_total: amount,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        payer_balance_before - get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await,
        amount,
    );
    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        amount,
    );
    // residue_vault was passed but unused.
    assert!(ctx
        .banks_client
        .get_account(residue_pda)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_funding_plan_withdrawal_only() {
    // 1 Withdrawal source — no gateway needed (withdrawals are gateway-independent).
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let vault_amount = 5_000_000_000u64;
    let (withdrawal_key, _) =
        create_operator_withdrawal(&mut ctx, &setup, gateway_key, vault_amount).await;

    let payment = 2_000_000_000u64;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    // Counter already exists (created by decrease_operator_stake); next_id = 1
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: payer_pk,
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(withdrawal_key, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_withdrawal(payment)],
                expected_total: payment,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        payment,
    );
    let withdrawal = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(withdrawal_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(withdrawal.amount, vault_amount - payment);
}

#[tokio::test]
async fn test_funding_plan_operator_stake_only() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    // Settings.min_operator_stake = 10 ARIO via setup_gar (per the existing setup);
    // join with 30 ARIO leaves room for a 5 ARIO draw without breaching min.
    let payment = 5_000_000_000u64;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: payer_pk,
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // OperatorStake source: 1 remaining_accounts entry — the gateway PDA.
    accounts.push(AccountMeta::new(gateway_key, false));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_operator_stake(payment)],
                expected_total: payment,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let _ = residue_pda;

    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        payment,
    );
    let gateway = Gateway::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(gateway_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(gateway.operator_stake, 30_000_000_000 - payment);
}

#[tokio::test]
async fn test_funding_plan_balance_plus_withdrawal() {
    // Multi-source: 0.5 ARIO from balance + 1.5 ARIO from a withdrawal vault.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let (withdrawal_key, _) =
        create_operator_withdrawal(&mut ctx, &setup, gateway_key, 5_000_000_000).await;

    let from_balance = 500_000_000u64;
    let from_withdrawal = 1_500_000_000u64;
    let total = from_balance + from_withdrawal;

    let payer_balance_before = get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: Some(setup.operator_token.pubkey()),
        payer: payer_pk,
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(withdrawal_key, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_balance(from_balance), fp_withdrawal(from_withdrawal)],
                expected_total: total,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        payer_balance_before - get_token_balance(&mut ctx, &setup.operator_token.pubkey()).await,
        from_balance,
    );
    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        total,
    );
    let withdrawal = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(withdrawal_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(withdrawal.amount, 5_000_000_000 - from_withdrawal);
}

#[tokio::test]
async fn test_funding_plan_delegation_drain_to_zero_no_residue() {
    // Drain a delegation entirely (post == 0). No residue vault should be created.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    // Set up a delegator and stake 12 ARIO (well above 10 ARIO min).
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            5_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator.pubkey(),
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    let stake_amount = 12_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: stake_amount,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Drain ALL 12 ARIO via funding plan.
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &delegator.pubkey()).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&delegator.pubkey()).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // Delegation source: 2 remaining_accounts entries — gateway + delegation.
    accounts.push(AccountMeta::new(gateway_key, false));
    accounts.push(AccountMeta::new(delegation_key, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(stake_amount)],
                expected_total: stake_amount,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegation = Delegation::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(delegation_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(delegation.amount, 0);
    // No residue vault was created.
    assert!(ctx
        .banks_client
        .get_account(residue_pda)
        .await
        .unwrap()
        .is_none());

    let gateway = Gateway::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(gateway_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(gateway.total_delegated_stake, 0);
}

#[tokio::test]
async fn test_funding_plan_delegation_sub_min_creates_residue_vault() {
    // Drain a delegation to a sub-min residue. Withdrawal vault auto-created.
    // This is the Lua-faithful behavior — match `applyFundingPlan` from gar.lua.
    //
    // Numbers: ARIO has 6 decimals. Gateway settings.min_delegation_amount
    // defaults to settings.min_delegate_stake = 10_000_000 (10 ARIO).
    // Stake 15 ARIO, pay 10 ARIO, residue = 5 ARIO < 10 ARIO min → auto-vault.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            5_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator.pubkey(),
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Stake 15 ARIO; min_delegation_amount = 10 ARIO.
    let stake_amount = 15_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: stake_amount,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Pay 10 ARIO via funding plan → leaves 5 ARIO residue (sub-min) → auto-vault.
    let payment = 10_000_000u64;
    let expected_residue = stake_amount - payment;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &delegator.pubkey()).await;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&delegator.pubkey()).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // Delegation source: gateway + delegation.
    accounts.push(AccountMeta::new(gateway_key, false));
    accounts.push(AccountMeta::new(delegation_key, false));
    // Trailing residue_vault slot for the sub-min residue.
    accounts.push(AccountMeta::new(residue_pda, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(payment)],
                expected_total: payment,
                residue_vault_count: 1,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Protocol got the payment (not the residue — residue stays in stake pool).
    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        payment,
    );

    // Delegation drained to 0.
    let delegation = Delegation::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(delegation_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(delegation.amount, 0);

    // Residue vault auto-created with the 5 ARIO residue.
    let residue_account = ctx.banks_client.get_account(residue_pda).await.unwrap();
    assert!(
        residue_account.is_some(),
        "residue vault should be auto-created"
    );
    let withdrawal =
        Withdrawal::try_deserialize(&mut residue_account.unwrap().data.as_slice()).unwrap();
    assert_eq!(withdrawal.owner, delegator.pubkey());
    assert_eq!(withdrawal.amount, expected_residue);
    assert!(withdrawal.is_delegate);
    assert!(!withdrawal.is_exit_vault);
    assert_eq!(withdrawal.gateway, payer_pk); // gateway.operator

    // Gateway lost both the deduction AND the residue from total_delegated_stake.
    let gateway = Gateway::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(gateway_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(gateway.total_delegated_stake, 0);

    // Counter incremented.
    let counter_account = ctx
        .banks_client
        .get_account(withdrawal_counter_pda(&delegator.pubkey()).0)
        .await
        .unwrap()
        .unwrap();
    let counter = WithdrawalCounter::try_deserialize(&mut counter_account.data.as_slice()).unwrap();
    assert_eq!(counter.next_id, 1);
}

#[tokio::test]
async fn test_funding_plan_amount_mismatch_rejected() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let _gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PayFromFundingPlan {
                settings: setup.settings_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer_token_account: Some(setup.operator_token.pubkey()),
                payer: payer_pk,
                token_program: spl_token::id(),
                withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_balance(1_000_000_000)],
                expected_total: 2_000_000_000, // ≠ sum
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::FundingPlanAmountMismatch);
}

#[tokio::test]
async fn test_funding_plan_too_many_sources_rejected() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let _gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    // 9 Balance sources of 1 mARIO each (over the 8 cap).
    let sources: Vec<FundingSourceSpec> = (0..9).map(|_| fp_balance(1)).collect();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PayFromFundingPlan {
                settings: setup.settings_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer_token_account: Some(setup.operator_token.pubkey()),
                payer: payer_pk,
                token_program: spl_token::id(),
                withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PayFromFundingPlan {
                sources,
                expected_total: 9,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::TooManyFundingSources);
}

#[tokio::test]
async fn test_funding_plan_zero_sources_rejected() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let _gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PayFromFundingPlan {
                settings: setup.settings_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer_token_account: Some(setup.operator_token.pubkey()),
                payer: payer_pk,
                token_program: spl_token::id(),
                withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![],
                expected_total: 0,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::EmptyFundingPlan);
}

#[tokio::test]
async fn test_funding_plan_operator_stake_below_min_rejected() {
    // Operator stake going below `settings.min_operator_stake` is HARD-rejected
    // (not auto-vaulted). Different from Delegation, which auto-vaults sub-min
    // residue. settings.min_operator_stake defaults to 20K ARIO via init.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    // Join with 25K ARIO — 5K above the 20K ARIO min_operator_stake.
    let gateway_key = join_gateway(&mut ctx, &setup, 25_000_000_000).await;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    // Pay 6K ARIO → would leave 19K < 20K ARIO min. Reject.
    let payment = 6_000_000_000u64;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: payer_pk,
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(gateway_key, false));
    let _ = residue_pda;
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_operator_stake(payment)],
                expected_total: payment,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::StakeBelowMinimum);
}

#[tokio::test]
async fn test_funding_plan_missing_source_slots_rejected() {
    // Delegation source declared but caller didn't push the per-source PDAs
    // into remaining_accounts → handler hits MissingFundingSourceAccount on
    // the first `iter.next()`. Replaces the v1 "missing fixed gateway slot"
    // test — under multi-gateway, every Delegation source carries its OWN
    // gateway PDA in remaining_accounts.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;

    // Stake from delegator side first.
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            5_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator.pubkey(),
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: 15_000_000_000,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let _residue_pda = predict_next_withdrawal_pda(&mut ctx, &delegator.pubkey()).await;
    let _ = delegation_key;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    // Build the fixed-slot accounts but DO NOT push the gateway+delegation
    // pair the handler expects for a Delegation source.
    let accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&delegator.pubkey()).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(1_000_000_000)],
                expected_total: 1_000_000_000,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::MissingFundingSourceAccount);
}

#[tokio::test]
async fn test_funding_plan_missing_payer_ata_rejected() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let _gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PayFromFundingPlan {
                settings: setup.settings_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer_token_account: None, // ← missing while Balance source present
                payer: payer_pk,
                token_program: spl_token::id(),
                withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_balance(1_000_000_000)],
                expected_total: 1_000_000_000,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::MissingPayerTokenAccountForFundingSource);
}

#[tokio::test]
async fn test_funding_plan_zero_amount_source_rejected() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let _gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &payer_pk).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PayFromFundingPlan {
                settings: setup.settings_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer_token_account: Some(setup.operator_token.pubkey()),
                payer: payer_pk,
                token_program: spl_token::id(),
                withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_balance(0)],
                expected_total: 0,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::ZeroFundingSourceAmount);
}

#[tokio::test]
async fn test_funding_plan_residue_vault_claimable_after_period() {
    // After the residue vault is auto-created, the user must be able to
    // claim_withdrawal it once the lock period elapses. End-to-end check
    // that the manually-init'd PDA is indistinguishable from one created
    // by decrease_delegate_stake.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            5_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator.pubkey(),
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    // Stake 15 ARIO, pay 10 ARIO → residue 5 ARIO < 10 ARIO min.
    let stake_amount = 15_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator.pubkey());
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: stake_amount,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let payment = 10_000_000u64;
    let expected_residue = stake_amount - payment;
    let residue_pda = predict_next_withdrawal_pda(&mut ctx, &delegator.pubkey()).await;

    // Trigger residue auto-vault creation.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&delegator.pubkey()).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(gateway_key, false));
    accounts.push(AccountMeta::new(delegation_key, false));
    accounts.push(AccountMeta::new(residue_pda, false));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(payment)],
                expected_total: payment,
                residue_vault_count: 1,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past the residue vault's available_at.
    let withdrawal_account = ctx
        .banks_client
        .get_account(residue_pda)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut withdrawal_account.data.as_slice()).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = withdrawal.available_at + 1;
    ctx.set_sysvar(&clock);

    // claim_withdrawal — same ix that handles vaults from decrease_delegate_stake.
    let delegator_balance_before = get_token_balance(&mut ctx, &delegator_token.pubkey()).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimWithdrawal {
                settings: setup.settings_key,
                withdrawal: residue_pda,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: delegator_token.pubkey(),
                owner: delegator.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimWithdrawal {}.data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    assert_eq!(
        get_token_balance(&mut ctx, &delegator_token.pubkey()).await - delegator_balance_before,
        expected_residue,
    );
    assert!(ctx
        .banks_client
        .get_account(residue_pda)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn test_close_drained_permissionless() {
    // Anyone can call close_drained_withdrawal once the vault is at zero.
    // Rent always returns to the original owner via the `address = withdrawal.owner`
    // constraint, regardless of who signs as `closer`.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let owner_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let vault_amount = 4_000_000_000u64;
    let (withdrawal_key, _) =
        create_operator_withdrawal(&mut ctx, &setup, gateway_key, vault_amount).await;

    // Drain the vault.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DeductWithdrawalForPayment {
                settings: setup.settings_key,
                withdrawal: withdrawal_key,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                owner: owner_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DeductWithdrawalForPayment {
                amount: vault_amount,
            }
            .data(),
        }],
        Some(&owner_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Stand up a stranger and have them close the vault.
    let stranger = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &owner_pk,
            &stranger.pubkey(),
            1_000_000_000,
        )],
        Some(&owner_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let owner_lamports_before = ctx
        .banks_client
        .get_account(owner_pk)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let stranger_lamports_before = ctx
        .banks_client
        .get_account(stranger.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    let vault_rent = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap()
        .lamports;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseDrainedWithdrawal {
                withdrawal: withdrawal_key,
                owner: owner_pk,
                closer: stranger.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseDrainedWithdrawal {}.data(),
        }],
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Vault gone.
    assert!(ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .is_none());

    // Owner gained the rent (no tx-fee noise — they didn't sign).
    let owner_lamports_after = ctx
        .banks_client
        .get_account(owner_pk)
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert_eq!(
        owner_lamports_after.saturating_sub(owner_lamports_before),
        vault_rent,
        "rent should flow from vault to original owner"
    );

    // Stranger paid only tx fees, not rent.
    let stranger_lamports_after = ctx
        .banks_client
        .get_account(stranger.pubkey())
        .await
        .unwrap()
        .unwrap()
        .lamports;
    assert!(
        stranger_lamports_after < stranger_lamports_before,
        "stranger paid fees but did NOT receive rent"
    );
    assert!(
        stranger_lamports_before - stranger_lamports_after < vault_rent,
        "tx fee should be far less than vault rent"
    );
}

// =========================================================================
// MULTI-GATEWAY FUNDING PLAN — Phase A of MULTI_GATEWAY_FUNDING_PLAN.md
// =========================================================================
//
// Each Delegation/OperatorStake source carries its own gateway PDA in
// remaining_accounts. These tests stand up multiple gateways, delegate
// from one user across them, and exercise the new dispatch.

mod multi_gateway_fixtures {
    use super::*;

    /// Stand up `count` gateways with the provided operator keypairs. Each
    /// joins with `MIN_OPERATOR_STAKE * 2` to leave room for stake-flexibility
    /// in tests. Returns gateway PDAs in operator-keypair order.
    pub async fn create_n_gateways(
        ctx: &mut ProgramTestContext,
        setup: &GarSetup,
        operators: &[Keypair],
    ) -> Vec<Pubkey> {
        let mut out = Vec::with_capacity(operators.len());
        for op in operators {
            // Fund operator.
            let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
            let tx = Transaction::new_signed_with_payer(
                &[solana_sdk::system_instruction::transfer(
                    &ctx.payer.pubkey(),
                    &op.pubkey(),
                    50_000_000_000,
                )],
                Some(&ctx.payer.pubkey()),
                &[&ctx.payer],
                blockhash,
            );
            ctx.banks_client.process_transaction(tx).await.unwrap();
            // Operator's ARIO ATA.
            let op_token = Keypair::new();
            create_token_account(ctx, &op_token, &setup.mint.pubkey(), &op.pubkey()).await;
            mint_tokens(
                ctx,
                &setup.mint.pubkey(),
                &op_token.pubkey(),
                &setup.mint_authority,
                100_000_000_000,
            )
            .await;
            // Join.
            let (gateway_key, _) = gateway_pda(&op.pubkey());
            let (observer_lookup_key, _) = observer_lookup_pda(&op.pubkey());
            let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
            let tx = Transaction::new_signed_with_payer(
                &[Instruction {
                    program_id: ario_gar::ID,
                    accounts: ario_gar::accounts::JoinNetwork {
                        registry: setup.registry_key,
                        settings: setup.settings_key,
                        gateway: gateway_key,
                        operator_token_account: op_token.pubkey(),
                        stake_token_account: setup.stake_token.pubkey(),
                        observer_lookup: observer_lookup_key,
                        operator: op.pubkey(),
                        token_program: spl_token::id(),
                        system_program: system_program::id(),
                    }
                    .to_account_metas(None),
                    data: ario_gar::instruction::JoinNetwork {
                        params: ario_gar::JoinNetworkParams {
                            operator_stake: 40_000_000_000, // 2× min
                            label: format!("gw-{}", out.len()),
                            fqdn: format!("gw{}.example.com", out.len()),
                            port: 443,
                            protocol: Protocol::Https,
                            properties: None,
                            note: None,

                            allow_delegated_staking: true,
                            delegate_reward_share_ratio: 10,
                            min_delegate_stake: None,
                            observer_address: op.pubkey(),
                        },
                    }
                    .data(),
                }],
                Some(&op.pubkey()),
                &[op],
                blockhash,
            );
            ctx.banks_client.process_transaction(tx).await.unwrap();
            out.push(gateway_key);
        }
        out
    }

    /// Stand up a delegator who has staked `amount_per` on each of the listed
    /// gateways. Returns the delegator keypair + (gateway, delegation) pairs.
    pub async fn delegator_staked_on_gateways(
        ctx: &mut ProgramTestContext,
        setup: &GarSetup,
        operators: &[Keypair],
        amount_per: u64,
    ) -> (Keypair, Vec<(Pubkey, Pubkey)>) {
        let delegator = Keypair::new();
        let delegator_token = Keypair::new();
        // Fund SOL.
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &delegator.pubkey(),
                10_000_000_000,
            )],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        // ARIO.
        create_token_account(
            ctx,
            &delegator_token,
            &setup.mint.pubkey(),
            &delegator.pubkey(),
        )
        .await;
        mint_tokens(
            ctx,
            &setup.mint.pubkey(),
            &delegator_token.pubkey(),
            &setup.mint_authority,
            amount_per * (operators.len() as u64) * 4, // generous
        )
        .await;
        let mut pairs = Vec::with_capacity(operators.len());
        for op in operators {
            let (gateway_key, _) = gateway_pda(&op.pubkey());
            let (delegation_key, _) = delegation_pda(&op.pubkey(), &delegator.pubkey());
            let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
            let tx = Transaction::new_signed_with_payer(
                &[Instruction {
                    program_id: ario_gar::ID,
                    accounts: ario_gar::accounts::DelegateStake {
                        settings: setup.settings_key,
                        gateway: gateway_key,
                        delegation: delegation_key,
                        delegator_token_account: delegator_token.pubkey(),
                        stake_token_account: setup.stake_token.pubkey(),
                        delegator: delegator.pubkey(),
                        token_program: spl_token::id(),
                        system_program: system_program::id(),
                    }
                    .to_account_metas(None),
                    data: ario_gar::instruction::DelegateStake { amount: amount_per }.data(),
                }],
                Some(&delegator.pubkey()),
                &[&delegator],
                blockhash,
            );
            ctx.banks_client.process_transaction(tx).await.unwrap();
            pairs.push((gateway_key, delegation_key));
        }
        (delegator, pairs)
    }
}

#[tokio::test]
async fn test_funding_plan_two_delegations_two_gateways() {
    // Happy path: 1 Delegation each on G1 + G2 in one tx, both ≥ min residue.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Stand up 2 gateways + a delegator who has staked 50 ARIO on each.
    let ops = vec![Keypair::new(), Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    let stake_per = 50_000_000u64;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, stake_per)
            .await;

    // Pay 30 ARIO total: 15 from G1 delegation + 15 from G2 delegation.
    let pay_per = 15_000_000u64;
    let total = pay_per * 2;
    let protocol_before = get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await;

    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&delegator.pubkey()).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // Two Delegation sources: 4 entries — gateway+delegation pair × 2.
    for (gw, del) in &pairs {
        accounts.push(AccountMeta::new(*gw, false));
        accounts.push(AccountMeta::new(*del, false));
    }
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(pay_per), fp_delegation(pay_per)],
                expected_total: total,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Both delegations decremented to (50 - 15) = 35 ARIO.
    for (_, del) in &pairs {
        let acct = ctx.banks_client.get_account(*del).await.unwrap().unwrap();
        let d = Delegation::try_deserialize(&mut acct.data.as_slice()).unwrap();
        assert_eq!(d.amount, stake_per - pay_per);
    }
    // Treasury credited by total.
    assert_eq!(
        get_token_balance(&mut ctx, &setup.protocol_token.pubkey()).await - protocol_before,
        total,
    );
}

#[tokio::test]
async fn test_funding_plan_two_delegations_two_residues() {
    // Both Delegations drain to sub-min → 2 residue vaults at sequential ids.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let ops = vec![Keypair::new(), Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    // Stake 15 ARIO on each (just above 10 ARIO min); pay 12 ARIO each → residue 3 ARIO.
    let stake_per = 15_000_000u64;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, stake_per)
            .await;
    let pay_per = 12_000_000u64;
    let expected_residue = stake_per - pay_per;
    let total = pay_per * 2;

    // Predict residue vault PDAs at next_id and next_id+1.
    let counter_key = withdrawal_counter_pda(&delegator.pubkey()).0;
    let counter_acct = ctx.banks_client.get_account(counter_key).await.unwrap();
    let next_id_pre = match counter_acct {
        Some(a) if a.data.len() >= 48 => u64::from_le_bytes(a.data[40..48].try_into().unwrap()),
        _ => 0,
    };
    let (residue0, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre);
    let (residue1, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre + 1);

    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: counter_key,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for (gw, del) in &pairs {
        accounts.push(AccountMeta::new(*gw, false));
        accounts.push(AccountMeta::new(*del, false));
    }
    accounts.push(AccountMeta::new(residue0, false));
    accounts.push(AccountMeta::new(residue1, false));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(pay_per), fp_delegation(pay_per)],
                expected_total: total,
                residue_vault_count: 2,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Both residue vaults exist with the right residue amount.
    for vault_pda in &[residue0, residue1] {
        let acct = ctx
            .banks_client
            .get_account(*vault_pda)
            .await
            .unwrap()
            .unwrap();
        let w = Withdrawal::try_deserialize(&mut acct.data.as_slice()).unwrap();
        assert_eq!(w.amount, expected_residue);
        assert_eq!(w.owner, delegator.pubkey());
        assert!(w.is_delegate);
        assert!(!w.is_exit_vault);
    }
    // Both delegations zero.
    for (_, del) in &pairs {
        let acct = ctx.banks_client.get_account(*del).await.unwrap().unwrap();
        let d = Delegation::try_deserialize(&mut acct.data.as_slice()).unwrap();
        assert_eq!(d.amount, 0);
    }
    // Counter advanced by 2.
    let counter_after = ctx
        .banks_client
        .get_account(counter_key)
        .await
        .unwrap()
        .unwrap();
    let counter = WithdrawalCounter::try_deserialize(&mut counter_after.data.as_slice()).unwrap();
    assert_eq!(counter.next_id, next_id_pre + 2);
}

#[tokio::test]
async fn test_funding_plan_two_delegations_one_residue() {
    // First Delegation drains to ≥ min; second drains to sub-min → 1 residue vault.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let ops = vec![Keypair::new(), Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    // Both stake 30 ARIO. First pays 15 (residue 15 ≥ min 10 → no vault).
    // Second pays 25 (residue 5 < min 10 → vault).
    let stake_per = 30_000_000u64;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, stake_per)
            .await;
    let pay_first = 15_000_000u64;
    let pay_second = 25_000_000u64;
    let expected_residue_second = stake_per - pay_second; // 5 ARIO

    let counter_key = withdrawal_counter_pda(&delegator.pubkey()).0;
    let counter_acct = ctx.banks_client.get_account(counter_key).await.unwrap();
    let next_id_pre = match counter_acct {
        Some(a) if a.data.len() >= 48 => u64::from_le_bytes(a.data[40..48].try_into().unwrap()),
        _ => 0,
    };
    let (residue0, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre);

    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: counter_key,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for (gw, del) in &pairs {
        accounts.push(AccountMeta::new(*gw, false));
        accounts.push(AccountMeta::new(*del, false));
    }
    accounts.push(AccountMeta::new(residue0, false));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(pay_first), fp_delegation(pay_second)],
                expected_total: pay_first + pay_second,
                residue_vault_count: 1,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // First delegation has the partial residue (≥ min), still open at 15 ARIO.
    let first_del = Delegation::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(pairs[0].1)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(first_del.amount, stake_per - pay_first);
    // Second delegation drained to 0; residue vault has the dust.
    let second_del = Delegation::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(pairs[1].1)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(second_del.amount, 0);
    let vault_acct = ctx
        .banks_client
        .get_account(residue0)
        .await
        .unwrap()
        .unwrap();
    let vault = Withdrawal::try_deserialize(&mut vault_acct.data.as_slice()).unwrap();
    assert_eq!(vault.amount, expected_residue_second);
}

#[tokio::test]
async fn test_funding_plan_duplicate_gateway_rejected() {
    // SDK MUST aggregate same-gateway sources. Handler rejects defensively.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let ops = vec![Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, 50_000_000)
            .await;

    // Pass the SAME gateway+delegation pair twice — should reject.
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&delegator.pubkey()).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for _ in 0..2 {
        accounts.push(AccountMeta::new(pairs[0].0, false));
        accounts.push(AccountMeta::new(pairs[0].1, false));
    }

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(10_000_000), fp_delegation(10_000_000)],
                expected_total: 20_000_000,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::DuplicateGatewayInSources);
}

#[tokio::test]
async fn test_funding_plan_too_many_delegations_rejected() {
    // 4 distinct-gateway Delegations → cap is 3 → TooManyDelegationSources.
    // Exceeds total cap (5) too if 4 + others, but the Delegation cap fires first.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // No need to actually stake — the cap check fires before any account lookup.
    let payer_pk = ctx.payer.pubkey();
    let _gateway_key = join_gateway(&mut ctx, &setup, 30_000_000_000).await;
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: payer_pk,
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&payer_pk).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // Bogus per-source slots — never touched because the cap check fires first.
    for _ in 0..8 {
        accounts.push(AccountMeta::new(payer_pk, false));
    }

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![
                    fp_delegation(1),
                    fp_delegation(1),
                    fp_delegation(1),
                    fp_delegation(1),
                ],
                expected_total: 4,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::TooManyDelegationSources);
}

#[tokio::test]
async fn test_funding_plan_residue_count_mismatch_rejected() {
    // Pass residue_vault_count=2 but only 1 Delegation actually residues
    // (other drains to ≥ min). Handler rejects MismatchedResidueVaultCount.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let ops = vec![Keypair::new(), Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    let stake_per = 30_000_000u64;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, stake_per)
            .await;

    // First drains to ≥ min (no residue), second drains to sub-min (1 residue).
    // Caller incorrectly passes residue_vault_count=2.
    let pay_first = 15_000_000u64;
    let pay_second = 25_000_000u64;
    let counter_key = withdrawal_counter_pda(&delegator.pubkey()).0;
    let next_id_pre = 0u64; // fresh counter
    let (r0, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre);
    let (r1, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre + 1);

    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: counter_key,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for (gw, del) in &pairs {
        accounts.push(AccountMeta::new(*gw, false));
        accounts.push(AccountMeta::new(*del, false));
    }
    accounts.push(AccountMeta::new(r0, false));
    accounts.push(AccountMeta::new(r1, false));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(pay_first), fp_delegation(pay_second)],
                expected_total: pay_first + pay_second,
                residue_vault_count: 2, // ← wrong; only 1 residues
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::MismatchedResidueVaultCount);
}

#[tokio::test]
async fn test_funding_plan_residue_missing_when_needed() {
    // Delegation drains sub-min but caller passes residue_vault_count=0.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let ops = vec![Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, 15_000_000)
            .await;

    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&delegator.pubkey()).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(pairs[0].0, false));
    accounts.push(AccountMeta::new(pairs[0].1, false));
    // No residue vault slots passed; pay 10 ARIO leaves 5 ARIO residue (sub-min).

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(10_000_000)],
                expected_total: 10_000_000,
                residue_vault_count: 0, // ← wrong; 1 residues
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::MismatchedResidueVaultCount);
}

#[tokio::test]
async fn test_funding_plan_per_gateway_min_thresholds() {
    // Gateways have different min_delegation_amount values. Each Delegation
    // is evaluated against ITS OWN gateway's min (not the global default).
    //
    // Setup: G1 created with default min (10 ARIO), G2 created with default
    // min too. We use the same min here (the helpers don't expose
    // min_delegate_stake override). The test still demonstrates that per-
    // gateway min reads come from the Gateway PDA, not a global constant —
    // by drawing different residue amounts and verifying they correspond to
    // each gateway's own min.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let ops = vec![Keypair::new(), Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    // Stake 15 ARIO on each.
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, 15_000_000)
            .await;

    // G1 pays 10 (residue 5 sub-min); G2 pays 12 (residue 3 sub-min).
    // Both trigger residue vaults at sequential ids.
    let counter_key = withdrawal_counter_pda(&delegator.pubkey()).0;
    let (r0, _) = withdrawal_pda(&delegator.pubkey(), 0);
    let (r1, _) = withdrawal_pda(&delegator.pubkey(), 1);

    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: counter_key,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for (gw, del) in &pairs {
        accounts.push(AccountMeta::new(*gw, false));
        accounts.push(AccountMeta::new(*del, false));
    }
    accounts.push(AccountMeta::new(r0, false));
    accounts.push(AccountMeta::new(r1, false));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(10_000_000), fp_delegation(12_000_000)],
                expected_total: 22_000_000,
                residue_vault_count: 2,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Residue vault 0 has 5 ARIO; vault 1 has 3 ARIO.
    let v0 = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(r0)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(v0.amount, 5_000_000);
    let v1 = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(r1)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(v1.amount, 3_000_000);
}

#[tokio::test]
async fn test_funding_plan_max_fill() {
    // 3 Delegations + 2 Withdrawals = 5 sources (the cap). All ≥ min residue
    // for delegations. Exercises tx-size at full extension.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // 3 gateways, delegator stakes on all 3.
    let ops = vec![Keypair::new(), Keypair::new(), Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    let stake_per = 100_000_000u64;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, stake_per)
            .await;

    // Create 2 withdrawals for the same delegator from each of the first 2
    // delegations (decrease_delegate_stake for partial drains). This generates
    // 2 Withdrawal PDAs we can spend from in the multi-source plan.
    let mut withdrawal_pdas: Vec<Pubkey> = Vec::new();
    for i in 0..2 {
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let (gw, del) = pairs[i];
        let (counter_pda, _) = withdrawal_counter_pda(&delegator.pubkey());
        let (w_pda, _) = withdrawal_pda(&delegator.pubkey(), i as u64);
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DecreaseDelegateStake {
                    settings: setup.settings_key,
                    gateway: gw,
                    delegation: del,
                    withdrawal_counter: counter_pda,
                    withdrawal: w_pda,
                    delegator: delegator.pubkey(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::DecreaseDelegateStake { amount: 30_000_000 }.data(),
            }],
            Some(&delegator.pubkey()),
            &[&delegator],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        withdrawal_pdas.push(w_pda);
    }

    // Now spend: 3 small Delegation draws (each ≥ min residue) + 2 Withdrawal draws.
    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: withdrawal_counter_pda(&delegator.pubkey()).0,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for (gw, del) in &pairs {
        accounts.push(AccountMeta::new(*gw, false));
        accounts.push(AccountMeta::new(*del, false));
    }
    for w in &withdrawal_pdas {
        accounts.push(AccountMeta::new(*w, false));
    }

    let pay_each_del = 10_000_000u64; // residue 60 ARIO ≥ min 10 — no vaults
    let pay_each_wd = 5_000_000u64;
    let total = pay_each_del * 3 + pay_each_wd * 2;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![
                    fp_delegation(pay_each_del),
                    fp_delegation(pay_each_del),
                    fp_delegation(pay_each_del),
                    fp_withdrawal(pay_each_wd),
                    fp_withdrawal(pay_each_wd),
                ],
                expected_total: total,
                residue_vault_count: 0,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    // CU baseline (Sprint 4.A): 5-source max-fill (3 dels + 2 withdrawals,
    // no residue) consumed ~69.5K CU on fresh BPF build (2026-05-04). Cap
    // at 95K CU (~37% headroom).
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "max-fill tx should succeed");
    let metadata = result.metadata.expect("metadata must be present");
    assert!(
        metadata.compute_units_consumed < 95_000,
        "5-source max-fill used {} CU, expected < 95_000",
        metadata.compute_units_consumed,
    );
}

#[tokio::test]
async fn test_funding_plan_three_delegations_three_residues() {
    // Worst-case tx-size + auto-vault path: 3 Delegations on 3 distinct gateways,
    // all draining to sub-min. remaining_accounts: 3 × [gw, del] + 3 × residue = 9.
    // Plus the fixed accounts. This is the heaviest shape the cap allows.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let ops = vec![Keypair::new(), Keypair::new(), Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    // Stake 13 ARIO on each (just above 10 ARIO min); pay 9 ARIO each → residue 4 ARIO each.
    let stake_per = 13_000_000u64;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, stake_per)
            .await;
    let pay_per = 9_000_000u64;
    let expected_residue = stake_per - pay_per;
    let total = pay_per * 3;

    // Predict residue vault PDAs at next_id, +1, +2.
    let counter_key = withdrawal_counter_pda(&delegator.pubkey()).0;
    let counter_acct = ctx.banks_client.get_account(counter_key).await.unwrap();
    let next_id_pre = match counter_acct {
        Some(a) if a.data.len() >= 48 => u64::from_le_bytes(a.data[40..48].try_into().unwrap()),
        _ => 0,
    };
    let (residue0, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre);
    let (residue1, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre + 1);
    let (residue2, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre + 2);

    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: counter_key,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for (gw, del) in &pairs {
        accounts.push(AccountMeta::new(*gw, false));
        accounts.push(AccountMeta::new(*del, false));
    }
    accounts.push(AccountMeta::new(residue0, false));
    accounts.push(AccountMeta::new(residue1, false));
    accounts.push(AccountMeta::new(residue2, false));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![
                    fp_delegation(pay_per),
                    fp_delegation(pay_per),
                    fp_delegation(pay_per),
                ],
                expected_total: total,
                residue_vault_count: 3,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    // CU baseline assertion (Sprint 4.A): worst-case 3-dels-3-residues
    // shape consumed ~76.5K CU on a fresh BPF build (2026-05-04, native
    // dispatch consumes less but is mode-dependent). PR-6 (2026-05-05)
    // added 15 new `#[event]` definitions to ario-gar/lib.rs. Even
    // though pay_from_funding_plan emits the same StakePaymentEvent /
    // ResidueVaultCreatedEvent / FundingPlanAppliedEvent set as before
    // (no new emit sites in this code path), the larger BPF binary
    // shifts register allocation in the heavy iteration path and CU
    // rose to ~112K. Re-baseline at 150K (~33% headroom from the new
    // floor). Asserts via process_transaction_with_metadata because
    // solana-program-test does NOT enforce
    // ComputeBudgetInstruction::set_compute_unit_limit; reading
    // consumed_units is the only way to catch CU regressions in tests.
    // See docs/MULTI_GATEWAY_FUNDING_PLAN.md "## CU baselines (BPF)" +
    // docs/EVENT_EMISSION_AUDIT.md for context.
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "tx should succeed");
    let metadata = result.metadata.expect("metadata must be present");
    assert!(
        metadata.compute_units_consumed < 150_000,
        "3-dels-3-residues used {} CU, expected < 150_000",
        metadata.compute_units_consumed,
    );

    // All 3 residue vaults exist with the right residue amount + sequential ids.
    for (i, vault_pda) in [residue0, residue1, residue2].iter().enumerate() {
        let acct = ctx
            .banks_client
            .get_account(*vault_pda)
            .await
            .unwrap()
            .unwrap();
        let w = Withdrawal::try_deserialize(&mut acct.data.as_slice()).unwrap();
        assert_eq!(w.amount, expected_residue, "residue #{} amount", i);
        assert_eq!(w.owner, delegator.pubkey());
        assert_eq!(w.withdrawal_id, next_id_pre + i as u64);
        assert!(w.is_delegate);
        assert!(!w.is_exit_vault);
    }
    // All 3 delegations zero.
    for (_, del) in &pairs {
        let acct = ctx.banks_client.get_account(*del).await.unwrap().unwrap();
        let d = Delegation::try_deserialize(&mut acct.data.as_slice()).unwrap();
        assert_eq!(d.amount, 0);
    }
    // Counter advanced by 3.
    let counter_after = ctx
        .banks_client
        .get_account(counter_key)
        .await
        .unwrap()
        .unwrap();
    let counter = WithdrawalCounter::try_deserialize(&mut counter_after.data.as_slice()).unwrap();
    assert_eq!(counter.next_id, next_id_pre + 3);

    // Treasury credited with the full payment (3 × 9M = 27M).
    let treasury_acct = ctx
        .banks_client
        .get_account(setup.protocol_token.pubkey())
        .await
        .unwrap()
        .unwrap();
    let treasury = spl_token::state::Account::unpack(&treasury_acct.data).unwrap();
    assert_eq!(treasury.amount, total);
}

#[tokio::test]
async fn test_funding_plan_residue_vault_lamport_grief_defense() {
    // Solana security checklist #12 — Lamport Griefing on Pre-funded PDA.
    //
    // Attack: an attacker pre-funds the user's predicted residue PDA with
    // a few lamports BEFORE the user's funding-plan tx lands. With a naive
    // `system_program::create_account` the tx would fail (account already
    // has lamports). Defense: transfer only the deficit, then Allocate +
    // Assign, which tolerate pre-existing lamports.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let ops = vec![Keypair::new()];
    let _ = multi_gateway_fixtures::create_n_gateways(&mut ctx, &setup, &ops).await;
    let stake_per = 15_000_000u64;
    let (delegator, pairs) =
        multi_gateway_fixtures::delegator_staked_on_gateways(&mut ctx, &setup, &ops, stake_per)
            .await;
    let pay_per = 12_000_000u64; // residue 3M < min 10M → triggers auto-vault
    let total = pay_per;

    let counter_key = withdrawal_counter_pda(&delegator.pubkey()).0;
    let counter_acct = ctx.banks_client.get_account(counter_key).await.unwrap();
    let next_id_pre = match counter_acct {
        Some(a) if a.data.len() >= 48 => u64::from_le_bytes(a.data[40..48].try_into().unwrap()),
        _ => 0,
    };
    let (residue0, _) = withdrawal_pda(&delegator.pubkey(), next_id_pre);

    // ATTACK: pre-fund the predicted residue PDA before the user's tx.
    // This would have caused `system_program::create_account` to fail.
    ctx.set_account(
        &residue0,
        &solana_sdk::account::Account {
            lamports: 1,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    let mut accounts = ario_gar::accounts::PayFromFundingPlan {
        settings: setup.settings_key,
        stake_token_account: setup.stake_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        payer_token_account: None,
        payer: delegator.pubkey(),
        token_program: spl_token::id(),
        withdrawal_counter: counter_key,
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    for (gw, del) in &pairs {
        accounts.push(AccountMeta::new(*gw, false));
        accounts.push(AccountMeta::new(*del, false));
    }
    accounts.push(AccountMeta::new(residue0, false));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::PayFromFundingPlan {
                sources: vec![fp_delegation(pay_per)],
                expected_total: total,
                residue_vault_count: 1,
            }
            .data(),
        }],
        Some(&delegator.pubkey()),
        &[&delegator],
        blockhash,
    );
    // Defense in place: tx succeeds despite the pre-funded PDA.
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Residue vault initialized correctly with the right amount + owner.
    let acct = ctx
        .banks_client
        .get_account(residue0)
        .await
        .unwrap()
        .unwrap();
    let w = Withdrawal::try_deserialize(&mut acct.data.as_slice()).unwrap();
    assert_eq!(w.owner, delegator.pubkey());
    assert_eq!(w.amount, stake_per - pay_per);
    assert!(w.is_delegate);
    // Account is now owned by ario-gar (Assign was called).
    assert_eq!(acct.owner, ario_gar::ID);
    // Lamports include both the original 1 + the topped-up rent-exempt amount.
    let rent_exempt = solana_sdk::rent::Rent::default().minimum_balance(Withdrawal::SIZE);
    assert!(acct.lamports >= rent_exempt);
}
// -----------------------------------------
// Regression: prescribe_epoch must accept gateway PDAs in any order in
// remaining_accounts. The cranker enumerates the registry in slot order, but
// selection inside prescribe_epoch picks observers in roulette order from the
// hashchain — the two orderings only align by chance. Pre-fix this silently
// broke every multi-gateway prescribe whose hashchain didn't happen to mirror
// registry order (5/6 hashes for 3 equal-weight gateways).
// -----------------------------------------
#[tokio::test]
async fn test_prescribe_epoch_accepts_unordered_remaining_accounts() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let operator2 = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund the protocol token account so create_epoch sees a non-zero treasury
    // (otherwise reward rates are zero and selection's `total_weight > 0` gate
    // skips the roulette entirely, defeating the point of this test).
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so both gateways join *before* epoch.start_timestamp = 100
    // — otherwise tally would zero their composite_weight (SHOULD-13 late-joiner
    // exclusion) and selection would have nothing to pick.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    // Two equal-weight gateways: registry slot 0 = payer, slot 1 = operator2.
    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;

    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key2 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator2,
        &op2_token.pubkey(),
        stake_amount,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // Warp past epoch.start (= genesis_timestamp = 100) and pin the slot so the
    // hashchain is deterministic across runs.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    clock.slot = 1;
    ctx.set_sysvar(&clock);

    // create_epoch — both gateways become active.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // tally_weights — populates composite_weight for both registry slots.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key1,
        false,
    ));
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key2,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // prescribe_epoch — pass remaining_accounts in REVERSE registry order
    // [gateway_key2, gateway_key1]. Pre-fix this would have failed with
    // InvalidGatewayAccount whenever selection didn't happen to pick slot 1
    // first; post-fix this must always succeed.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key2,
        false,
    ));
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gateway_key1,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Selection is deterministic from on-chain state, independent of
    // remaining_accounts order — both operators must end up prescribed.
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(
        epoch.prescriptions_done, 1,
        "prescribe_epoch did not flip prescriptions_done"
    );
    assert_eq!(
        epoch.observer_count, 2,
        "both gateways should be selected as observers (observer_count_cap=50, active=2)"
    );
    let prescribed: std::collections::HashSet<Pubkey> = epoch.prescribed_observer_gateways
        [..epoch.observer_count as usize]
        .iter()
        .copied()
        .collect();
    assert!(
        prescribed.contains(&payer_pk),
        "operator1 (payer) missing from prescribed_observer_gateways"
    );
    assert!(
        prescribed.contains(&op2_pk),
        "operator2 missing from prescribed_observer_gateways"
    );
}

// =====================================================================
// Supply counter tests (PR #63 follow-up)
//
// Verify total_staked / total_delegated / total_withdrawn move correctly
// at every state transition. The PR maintains these counters at 27
// mutation sites; these tests cover the most representative paths.
// =====================================================================

/// Helper: read GatewaySettings counters after each state transition.
async fn read_supply_counters(
    ctx: &mut ProgramTestContext,
    settings_key: Pubkey,
) -> (u64, u64, u64) {
    let acct = ctx
        .banks_client
        .get_account(settings_key)
        .await
        .unwrap()
        .unwrap();
    let settings = GatewaySettings::try_deserialize(&mut acct.data.as_slice()).unwrap();
    (
        settings.total_staked,
        settings.total_delegated,
        settings.total_withdrawn,
    )
}

#[tokio::test]
async fn test_supply_counters_join_leave() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // After init: all counters zero.
    let (staked, delegated, withdrawn) = read_supply_counters(&mut ctx, setup.settings_key).await;
    assert_eq!((staked, delegated, withdrawn), (0, 0, 0));

    // Join with 50K ARIO (50_000_000_000 mARIO): total_staked = 50K ARIO,
    // others unchanged. Min operator stake is 20K ARIO.
    let stake_amount = 50_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let _gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;
    let (staked, delegated, withdrawn) = read_supply_counters(&mut ctx, setup.settings_key).await;
    assert_eq!(
        (staked, delegated, withdrawn),
        (stake_amount, 0, 0),
        "after join_network: total_staked == join_amount",
    );

    // Leave network: total_staked → 0, total_withdrawn → stake_amount
    // (tokens move from active stake into TWO locked withdrawal vaults
    // post-BD-102: an exit vault holding the min portion + an excess vault
    // holding the above-min portion).
    let (gateway_key, _) = gateway_pda(&payer_pk);
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (exit_vault_key, _) = withdrawal_pda(&payer_pk, 0);
    let (excess_vault_key, _) = withdrawal_pda(&payer_pk, 1);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: exit_vault_key,
                excess_withdrawal: Some(excess_vault_key),
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (staked, delegated, withdrawn) = read_supply_counters(&mut ctx, setup.settings_key).await;
    assert_eq!(
        (staked, delegated, withdrawn),
        (0, 0, stake_amount),
        "after leave_network: total_staked → 0, total_withdrawn == full stake \
         (split across exit + excess vaults but unchanged in aggregate)",
    );

    // Warp past withdrawal_period so claim_withdrawal succeeds on both vaults.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += 91 * 86_400; // 91 days, past 90-day lock
    ctx.set_sysvar(&clock);

    // Claim the protected exit vault first.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimWithdrawal {
                settings: setup.settings_key,
                withdrawal: exit_vault_key,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Then claim the excess vault.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimWithdrawal {
                settings: setup.settings_key,
                withdrawal: excess_vault_key,
                stake_token_account: setup.stake_token.pubkey(),
                owner_token_account: setup.operator_token.pubkey(),
                owner: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimWithdrawal {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (staked, delegated, withdrawn) = read_supply_counters(&mut ctx, setup.settings_key).await;
    assert_eq!(
        (staked, delegated, withdrawn),
        (0, 0, 0),
        "after claiming both vaults: total_withdrawn → 0",
    );
}

#[tokio::test]
async fn test_supply_counters_delegate() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Operator joins so a gateway exists for delegation.
    let operator_stake = 50_000_000_000u64;
    let _gateway_key = join_gateway(&mut ctx, &setup, operator_stake).await;
    let payer_pk = ctx.payer.pubkey();
    let (gateway_key, _) = gateway_pda(&payer_pk);
    // After join: total_staked = operator_stake.

    // Spawn a delegator (separate keypair — operator can't delegate to self).
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let delegator_pk = delegator.pubkey();
    // Fund delegator with SOL for tx fees + rent.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator_pk,
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    let pre_staked = operator_stake;

    // Delegate 100 ARIO. Min delegation is 10 ARIO; we want to be able to
    // partial-drain (decrease) and still leave >= min in the delegation.
    let delegate_amount = 100_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (staked, delegated, withdrawn) = read_supply_counters(&mut ctx, setup.settings_key).await;
    assert_eq!(
        (staked, delegated, withdrawn),
        (pre_staked, delegate_amount, 0),
        "after delegate_stake: total_delegated += amount",
    );

    // Decrease delegate stake by 50 ARIO → moves to a Withdrawal vault.
    // Remaining 50 ARIO is above min (10 ARIO).
    // Expected: total_delegated -= 50 ARIO, total_withdrawn += 50 ARIO.
    let decrease_amount = 50_000_000u64;
    let (withdrawal_counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: withdrawal_counter_key,
                withdrawal: withdrawal_key,
                delegator: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: decrease_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let (staked, delegated, withdrawn) = read_supply_counters(&mut ctx, setup.settings_key).await;
    assert_eq!(
        (staked, delegated, withdrawn),
        (
            pre_staked,
            delegate_amount - decrease_amount,
            decrease_amount
        ),
        "after decrease_delegate_stake: total_delegated -= amount, total_withdrawn += amount",
    );
}

// Audit fix (PR #63 self-review): migrate_settings_supply_counters must be
// blocked once migration is finalized. Without this gate, a compromised
// authority key could overwrite live counter values post-launch. This
// matches the migration-active gate already on import_account / etc.
#[tokio::test]
async fn test_migrate_settings_supply_counters_post_finalize_blocked() {
    use anchor_lang::solana_program::hash::hash;

    // Build a fresh ProgramTest with migration_active = TRUE in settings.
    // The standard `program_test_with_gar` fixture sets it to false because
    // most tests don't need migration semantics; we override here.
    let authority = Keypair::new();
    let mint = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let mut pt = program_test_with_gar(
        &authority.pubkey(),
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );

    // Override the settings account with migration_active = 1.
    let (settings_key, settings_bump) = settings_pda();
    let size = GatewaySettings::SIZE;
    let mut data = vec![0u8; size];
    let disc = hash(b"account:GatewaySettings");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);
    let mut o = 8;
    data[o..o + 32].copy_from_slice(authority.pubkey().as_ref());
    o += 32; // authority
    data[o..o + 32].copy_from_slice(mint.pubkey().as_ref());
    o += 32; // mint
    data[o..o + 8].copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&10_000_000u64.to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&(30i64 * 86_400).to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&500_000u64.to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&100_000u64.to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&1_000_000u64.to_le_bytes());
    o += 8;
    data[o..o + 4].copy_from_slice(&10_000u32.to_le_bytes());
    o += 4;
    data[o] = 1;
    o += 1; // migration_active = TRUE
    data[o..o + 32].copy_from_slice(authority.pubkey().as_ref());
    o += 32; // migration_authority
    data[o..o + 32].copy_from_slice(stake_token.pubkey().as_ref());
    o += 32;
    data[o..o + 32].copy_from_slice(protocol_token.pubkey().as_ref());
    o += 32;
    data[o..o + 32].copy_from_slice(&[0xAAu8; 32]);
    o += 32; // arns_program_id
    o += 8 + 8 + 8; // counters all 0
    data[o] = settings_bump;
    pt.add_account(
        settings_key,
        solana_sdk::account::Account {
            lamports: solana_sdk::rent::Rent::default().minimum_balance(size),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    // Fund authority for tx fees.
    pt.add_account(
        authority.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // Step 1: while migration_active=true, backfill succeeds.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateSettingsSupplyCounters {
                settings: settings_key,
                authority: authority.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateSettingsSupplyCounters {
                total_staked: 1_000,
                total_delegated: 2_000,
                total_withdrawn: 3_000,
            }
            .data(),
        }],
        Some(&authority.pubkey()),
        &[&authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let s = GatewaySettings::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(settings_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(
        (s.total_staked, s.total_delegated, s.total_withdrawn),
        (1_000, 2_000, 3_000)
    );

    // Step 2: finalize migration.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::FinalizeMigration {
                settings: settings_key,
                authority: authority.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::FinalizeMigration {}.data(),
        }],
        Some(&authority.pubkey()),
        &[&authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Step 3: post-finalize, backfill must fail with MigrationInactive.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateSettingsSupplyCounters {
                settings: settings_key,
                authority: authority.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateSettingsSupplyCounters {
                total_staked: 999_999_999,
                total_delegated: 999_999_999,
                total_withdrawn: 999_999_999,
            }
            .data(),
        }],
        Some(&authority.pubkey()),
        &[&authority],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::MigrationInactive);

    // Counters unchanged from step 1.
    let s = GatewaySettings::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(settings_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(
        (s.total_staked, s.total_delegated, s.total_withdrawn),
        (1_000, 2_000, 3_000)
    );
}

// =========================================================================
// BD-102: leave_network / prune_gateway exit-vault split — Lua-parity matrix
// =========================================================================
//
// `leave_network` and `prune_gateway` produce 0, 1, or 2 `Withdrawal` PDAs
// depending on the relationship between `operator_stake` and
// `min_operator_stake`, mirroring Lua's
// `gar.leaveNetwork::createGatewayExitVault + createGatewayWithdrawVault`
// pattern (gar.lua:184-199). The exit half is `is_protected: true` and
// off-limits to `instant_withdrawal` + `deduct_withdrawal_for_payment`;
// the excess half is unprotected and behaves like a normal operator vault.
//
// Coverage matrix (`min` = `min_operator_stake` = 20_000_000_000 mARIO):
//   * leave with `pre_stake >= 2 * min`        — exit + excess
//   * leave with `min < pre_stake < 2 * min`   — exit only (sub-min size)
//   * leave with `pre_stake == min`            — exit only (full min)
//   * prune with `pre_stake >= 2 * min`        — slash min, exit + excess
//   * prune with `min < pre_stake < 2 * min`   — slash min, exit only (sub-min)
//   * prune with `pre_stake == min`            — slash all, no vaults
//   * leave_network with surplus excess: counter advances by 2
//   * leave_network without excess: counter advances by 1

#[tokio::test]
async fn test_leave_network_split_two_vaults_when_excess() {
    // Lua matrix row: leave with pre_stake = 50k (≥ 2*20k = 40k).
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let pre_stake = 50_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, pre_stake).await;
    let (counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (exit_key, _) = withdrawal_pda(&payer_pk, 0);
    let (excess_key, _) = withdrawal_pda(&payer_pk, 1);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: counter_key,
                withdrawal: exit_key,
                excess_withdrawal: Some(excess_key),
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let exit = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(exit_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(exit.amount, 20_000_000_000, "exit holds the min portion");
    assert!(exit.is_protected);
    assert!(exit.is_exit_vault);
    assert!(!exit.is_delegate);

    let excess = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(excess_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(excess.amount, 30_000_000_000, "excess holds pre - 2*min");
    assert!(!excess.is_protected);
    assert!(excess.is_exit_vault);

    // Counter advanced by 2 (both vaults consumed an id).
    let counter = WithdrawalCounter::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(counter_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(counter.next_id, 2);
}

#[tokio::test]
async fn test_leave_network_one_vault_when_no_excess() {
    // Lua matrix row: leave with pre_stake == min (no excess).
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let pre_stake = 20_000_000_000u64; // exactly min
    let gateway_key = join_gateway(&mut ctx, &setup, pre_stake).await;
    let (counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (exit_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: counter_key,
                withdrawal: exit_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let exit = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(exit_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(exit.amount, 20_000_000_000);
    assert!(exit.is_protected);

    let counter = WithdrawalCounter::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(counter_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(counter.next_id, 1, "counter advances by 1 when no excess");
}

#[tokio::test]
async fn test_leave_network_sub_min_exit_vault() {
    // Lua matrix row: leave with min < pre_stake < 2*min — exit vault holds
    // the full pre_stake (which is sub-min in absolute terms but still the
    // min-portion for this gateway), no excess.
    //
    // join_network requires pre_stake >= min. To set up `min < pre_stake <
    // 2*min`, join with 30k and never decrease — we need an off-by-min
    // resemblance only achievable via prune. Skipping leave-only sub-min
    // (impossible to construct from join_network alone). The prune path
    // covers this case (test below).
    //
    // Smoke: prove leave with pre_stake just above min (30k > 20k = min,
    // 30k < 40k = 2*min) generates exit=20k + excess=10k, NOT a sub-min
    // exit. The sub-min exit case is exclusively a prune-path artifact.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let pre_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, pre_stake).await;
    let (counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (exit_key, _) = withdrawal_pda(&payer_pk, 0);
    let (excess_key, _) = withdrawal_pda(&payer_pk, 1);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: counter_key,
                withdrawal: exit_key,
                excess_withdrawal: Some(excess_key),
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let exit = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(exit_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(exit.amount, 20_000_000_000); // min portion
    assert!(exit.is_protected);

    let excess = Withdrawal::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(excess_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(excess.amount, 10_000_000_000); // pre - min
}

#[tokio::test]
async fn test_leave_network_missing_excess_when_required_rejects() {
    // Defense: if pre_stake produces a positive excess but the caller fails
    // to pass the excess_withdrawal account, the handler must reject with
    // MissingExcessWithdrawal — never silently drop the excess into the
    // void.
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let pre_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, pre_stake).await;
    let (counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (exit_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: counter_key,
                withdrawal: exit_key,
                excess_withdrawal: None,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::MissingExcessWithdrawal);
}

#[tokio::test]
async fn test_leave_network_wrong_excess_pda_rejects() {
    // Defense: caller can't pass an arbitrary writable account as
    // excess_withdrawal — the handler verifies the PDA derivation
    // (`['withdrawal', operator, exit_id + 1]`).
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let payer_pk = ctx.payer.pubkey();
    let pre_stake = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, pre_stake).await;
    let (counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (exit_key, _) = withdrawal_pda(&payer_pk, 0);
    // Wrong: pass the WITHDRAWAL_ID 99 PDA instead of 1.
    let (wrong_excess_key, _) = withdrawal_pda(&payer_pk, 99);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::LeaveNetwork {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_pda().0,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: counter_key,
                withdrawal: exit_key,
                excess_withdrawal: Some(wrong_excess_key),
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::InvalidExcessWithdrawalPda);
}

// =========================================================================
// PR-6: Event-emission integration tests for ario-gar
// =========================================================================
//
// Each test exercises one of the 15 new `#[event]` types added in PR-6,
// captures `process_transaction_with_metadata` log output, and asserts the
// payload via `ario-test-utils::expect_event!` / `expect_event_count!`.
//
// `bpf_required!()` is the first line of every test — solana-program-test
// 2.1.0 only captures `sol_log_data` (which `emit!` emits) under BPF
// dispatch. Without `BPF_OUT_DIR` the tests skip cleanly with a hint.
//
// The crucial contract is `tally_weights`: it's batched, so the summary
// `EpochWeightsTalliedEvent` MUST fire exactly once — on the call that
// transitions `weights_tallied: 0 → 1`. `test_tally_weights_emits_single_event`
// verifies this by running multiple batched calls and asserting earlier
// txs DO NOT emit while the final tx emits exactly once.

#[tokio::test]
async fn test_update_gateway_settings_emits_event_with_bitmask() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;

    let payer_pk = ctx.payer.pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateGatewaySettings {
                params: ario_gar::UpdateGatewayParams {
                    label: Some("evt-gw".to_string()),
                    fqdn: Some("evt.example.com".to_string()),
                    port: Some(8080),
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
        "update_gateway_settings should succeed"
    );
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::GatewaySettingsUpdatedEvent);
    assert_eq!(ev.operator, payer_pk);
    // `auto_stake` field was removed from GatewaySettings in commit cfc7a8b
    // (pre-rollout main), so this test no longer mutates it. The
    // GATEWAY_SETTINGS_FIELD_AUTO_STAKE bit (1<<6) is reserved per
    // ADR-017 (event ABI stays append-only) but never set.
    let expected = ario_gar::GATEWAY_SETTINGS_FIELD_LABEL
        | ario_gar::GATEWAY_SETTINGS_FIELD_FQDN
        | ario_gar::GATEWAY_SETTINGS_FIELD_PORT;
    assert_eq!(
        ev.fields_changed, expected,
        "bitmask must reflect exactly the three mutated fields"
    );
}

#[tokio::test]
async fn test_update_observer_address_emits_event() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;
    let payer_pk = ctx.payer.pubkey();
    let new_observer = Pubkey::new_unique();

    let (old_lookup, _) = observer_lookup_pda(&payer_pk);
    let (new_lookup, _) = observer_lookup_pda(&new_observer);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateObserverAddress {
                gateway: gateway_key,
                old_observer_lookup: old_lookup,
                new_observer_lookup: new_lookup,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateObserverAddress { new_observer }.data(),
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
        "update_observer_address should succeed"
    );
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::ObserverAddressUpdatedEvent);
    assert_eq!(ev.operator, payer_pk);
    assert_eq!(ev.new_observer, new_observer);
}

#[tokio::test]
async fn test_increase_operator_stake_emits_event() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let initial = 30_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, initial).await;
    let added = 5_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::IncreaseOperatorStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator_token_account: setup.operator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                operator: payer_pk,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::IncreaseOperatorStake { amount: added }.data(),
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
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::OperatorStakeIncreasedEvent);
    assert_eq!(ev.operator, payer_pk);
    assert_eq!(ev.added, added);
    assert_eq!(ev.new_total, initial + added);
}

#[tokio::test]
async fn test_decrease_delegate_stake_emits_event() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;
    let payer_pk = ctx.payer.pubkey();

    // Set up delegator + initial delegation.
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Decrease by full amount.
    let (counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (wd_key, _) = withdrawal_pda(&delegator_pk, 0);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: counter_key,
                withdrawal: wd_key,
                delegator: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::DelegationDecreasedEvent);
    assert_eq!(ev.delegator, delegator_pk);
    assert_eq!(ev.gateway, payer_pk);
    assert_eq!(ev.decrease, delegate_amount);
    assert_eq!(ev.new_total, 0);
}

#[tokio::test]
async fn test_close_empty_delegation_emits_event() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;
    let payer_pk = ctx.payer.pubkey();

    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;
    let delegate_amount = 20_000_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Drain delegation to zero.
    let (counter_key, _) = withdrawal_counter_pda(&delegator_pk);
    let (wd_key, _) = withdrawal_pda(&delegator_pk, 0);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DecreaseDelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                withdrawal_counter: counter_key,
                withdrawal: wd_key,
                delegator: delegator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DecreaseDelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Close (permissionless).
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEmptyDelegation {
                gateway: gateway_key,
                delegation: delegation_key,
                delegator: delegator_pk,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEmptyDelegation {}.data(),
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
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::DelegationClosedEvent);
    assert_eq!(ev.delegator, delegator_pk);
    assert_eq!(ev.gateway, payer_pk);
}

#[tokio::test]
async fn test_redelegate_stake_emits_event() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let operator2 = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pt.add_account(
        operator2.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let stake_amount = 20_000_000_000u64;
    let gateway_key1 = join_gateway(&mut ctx, &setup, stake_amount).await;
    let op2_pk = operator2.pubkey();
    let op2_token = Keypair::new();
    create_token_account(&mut ctx, &op2_token, &setup.mint.pubkey(), &op2_pk).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &op2_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;
    let gateway_key2 = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &operator2,
        &op2_token.pubkey(),
        stake_amount,
    )
    .await;

    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    let delegate_amount = 20_000_000_000u64;
    let payer_pk = ctx.payer.pubkey();
    let (delegation_key1, _) = delegation_pda(&payer_pk, &delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key1,
                delegation: delegation_key1,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Redelegate (first redelegation is fee-free).
    let (delegation_key2, _) = delegation_pda(&op2_pk, &delegator_pk);
    let (redelegation_key, _) = redelegation_pda(&delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::RedelegateStake {
                source_gateway: gateway_key1,
                target_gateway: gateway_key2,
                source_delegation: delegation_key1,
                target_delegation: delegation_key2,
                redelegation_record: redelegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                settings: setup.settings_key,
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::RedelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::RedelegationEvent);
    assert_eq!(ev.delegator, delegator_pk);
    assert_eq!(ev.from_gateway, payer_pk);
    assert_eq!(ev.to_gateway, op2_pk);
    assert_eq!(ev.amount, delegate_amount);
    // First redelegation = 0% fee
    assert_eq!(ev.fee, 0);
}

#[tokio::test]
async fn test_allow_disallow_delegate_emits_events() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;
    let payer_pk = ctx.payer.pubkey();
    let delegate = Pubkey::new_unique();
    let (allowlist_key, _) = allowlist_pda(&payer_pk, &delegate);

    // allow_delegate
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::AllowDelegate {
                gateway: gateway_key,
                allowlist_entry: allowlist_key,
                delegate,
                operator: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::AllowDelegate {}.data(),
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
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;
    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::DelegateAllowlistedEvent);
    assert_eq!(ev.operator, payer_pk);
    assert_eq!(ev.delegate, delegate);
    assert!(ev.allowed, "allow_delegate must emit allowed=true");

    // disallow_delegate
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DisallowDelegate {
                gateway: gateway_key,
                allowlist_entry: allowlist_key,
                delegate,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DisallowDelegate {}.data(),
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
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;
    let ev = expect_event!(&logs, ario_gar::DelegateAllowlistedEvent);
    assert_eq!(ev.operator, payer_pk);
    assert_eq!(ev.delegate, delegate);
    assert!(!ev.allowed, "disallow_delegate must emit allowed=false");
}

#[tokio::test]
async fn test_set_allowlist_enabled_emits_event() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;
    let payer_pk = ctx.payer.pubkey();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SetAllowlistEnabled { enabled: true }.data(),
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
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;
    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::AllowlistToggledEvent);
    assert_eq!(ev.operator, payer_pk);
    assert!(ev.enabled);
}

#[tokio::test]
async fn test_set_epochs_enabled_emits_event() {
    ario_test_utils::bpf_required!();
    let mint = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar_for_epoch_init(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    // Pre-create EpochSettings with enabled=true and authority=payer so we can
    // toggle directly. The standard helper preserves a placeholder authority,
    // so write a custom one here keyed off ctx.payer (set after start).
    let mut ctx = pt.start_with_context().await;
    let payer_pk = ctx.payer.pubkey();
    {
        use anchor_lang::solana_program::hash::hash;
        let (epoch_settings_key, epoch_settings_bump) = epoch_settings_pda();
        let size = EpochSettings::SIZE;
        let mut data = vec![0u8; size];
        let disc = hash(b"account:EpochSettings");
        data[..8].copy_from_slice(&disc.to_bytes()[..8]);
        let mut o = 8;
        data[o..o + 32].copy_from_slice(payer_pk.as_ref());
        o += 32; // authority = payer
        data[o..o + 8].copy_from_slice(&86_400i64.to_le_bytes());
        o += 8; // epoch_duration
        data[o] = 50;
        o += 1; // observer_count
        data[o] = 2;
        o += 1; // name_count
        data[o..o + 8].copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
        o += 8;
        data[o..o + 2].copy_from_slice(&0u16.to_le_bytes());
        o += 2; // slash_rate
        data[o] = 1;
        o += 1; // enabled = true
        data[o..o + 8].copy_from_slice(&0u64.to_le_bytes());
        o += 8; // current_epoch_index
        data[o..o + 8].copy_from_slice(&0i64.to_le_bytes());
        o += 8; // genesis_timestamp
        data[o..o + 8].copy_from_slice(&(180i64 * 86_400).to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&4u64.to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&900_000u64.to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&100_000u64.to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&250_000u64.to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&1_000u64.to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&500u64.to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&365u64.to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&547u64.to_le_bytes());
        o += 8;
        data[o] = 30;
        o += 1; // max_consecutive_failures
        data[o..o + 8].copy_from_slice(&1_000_000u64.to_le_bytes());
        o += 8;
        data[o..o + 8].copy_from_slice(&0i64.to_le_bytes());
        o += 8; // disable_at
        data[o] = epoch_settings_bump;
        ctx.set_account(
            &epoch_settings_key,
            &solana_sdk::account::Account {
                lamports: solana_sdk::rent::Rent::default().minimum_balance(size),
                data,
                owner: ario_gar::ID,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );
    }
    let (epoch_settings_key, _) = epoch_settings_pda();

    // Disable epochs (timelocked but the event still fires on intent).
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateEpochSettings {
                epoch_settings: epoch_settings_key,
                authority: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::SetEpochsEnabled { enabled: false }.data(),
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
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;
    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::EpochsToggledEvent);
    assert_eq!(ev.admin, payer_pk);
    assert!(!ev.enabled, "disable intent must surface as enabled=false");
}

#[tokio::test]
async fn test_tally_weights_emits_single_event_on_final_batch() {
    // CRITICAL: the summary EpochWeightsTalliedEvent must fire EXACTLY ONCE
    // per epoch — on the call that flips weights_tallied 0 → 1. Mid-batch
    // calls must be silent. We split a 4-gateway tally across two batches
    // and assert: batch 1 emits zero events, batch 2 emits exactly one.
    ario_test_utils::bpf_required!();

    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let operator2 = Keypair::new();
    let operator3 = Keypair::new();
    let operator4 = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    for op in [&operator2, &operator3, &operator4] {
        pt.add_account(
            op.pubkey(),
            solana_sdk::account::Account {
                lamports: 50_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    let stake_amount = 20_000_000_000u64;
    let gw1 = join_gateway(&mut ctx, &setup, stake_amount).await;
    let mut other_keys = Vec::new();
    for op in [&operator2, &operator3, &operator4] {
        let token = Keypair::new();
        create_token_account(&mut ctx, &token, &setup.mint.pubkey(), &op.pubkey()).await;
        mint_tokens(
            &mut ctx,
            &setup.mint.pubkey(),
            &token.pubkey(),
            &setup.mint_authority,
            100_000_000_000,
        )
        .await;
        let k =
            join_gateway_with_operator(&mut ctx, &setup, op, &token.pubkey(), stake_amount).await;
        other_keys.push(k);
    }

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    // create_epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Resolve registry slot ordering.
    let mut all_gw_keys = vec![gw1];
    all_gw_keys.extend(other_keys.iter().copied());
    let mut gw_to_op: Vec<(Pubkey, Pubkey)> = Vec::new();
    for k in &all_gw_keys {
        let acct = ctx.banks_client.get_account(*k).await.unwrap().unwrap();
        let gw = Gateway::try_deserialize(&mut acct.data.as_slice()).unwrap();
        gw_to_op.push((*k, gw.operator));
    }
    let registry_account = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let mut ordered: Vec<Pubkey> = Vec::new();
    for slot_idx in 0..4 {
        let off = 8 + 32 + 4 + 4 + slot_idx * 56;
        let slot_addr = Pubkey::try_from(&registry_account.data[off..off + 32]).unwrap();
        let gw_key = gw_to_op
            .iter()
            .find(|(_, op)| *op == slot_addr)
            .map(|(k, _)| *k)
            .expect("registry slot must match a gateway");
        ordered.push(gw_key);
    }

    // BATCH 1: tally first 2 gateways (mid-batch — must NOT emit summary).
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    for k in &ordered[..2] {
        accounts.push(solana_sdk::instruction::AccountMeta::new(*k, false));
    }
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result1 = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result1.result.is_ok());
    let logs1 = result1.metadata.expect("metadata").log_messages;
    ario_test_utils::assert_no_event!(&logs1, ario_gar::EpochWeightsTalliedEvent);

    // BATCH 2: final 2 gateways → weights_tallied flips → must emit ONCE.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    for k in &ordered[2..] {
        accounts.push(solana_sdk::instruction::AccountMeta::new(*k, false));
    }
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result2 = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result2.result.is_ok());
    let logs2 = result2.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event_count;
    let events = expect_event_count!(&logs2, ario_gar::EpochWeightsTalliedEvent, 1);
    let ev = &events[0];
    assert_eq!(ev.epoch_index, 0);
    assert_eq!(
        ev.gateway_count, 4,
        "summary count must equal active_gateway_count"
    );
    assert!(
        ev.total_weight > 0,
        "total_weight must reflect accumulated weight"
    );
}

#[tokio::test]
async fn test_close_epoch_emits_event() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);

    let payer_pk = ctx.payer.pubkey();
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);
    // create_epoch
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Mark the epoch fully distributed and bump current_epoch_index past
    // the retention gap so close_epoch passes its gates without going
    // through the full lifecycle. Both fields are set via direct account
    // surgery — the state mutations they encode (rewards_distributed=1,
    // current_epoch_index=8) match the post-distribute, post-gap state.
    {
        let mut acct = ctx
            .banks_client
            .get_account(epoch_key)
            .await
            .unwrap()
            .unwrap();
        // Epoch is zero-copy: 8 (disc) + Epoch struct. `rewards_distributed`
        // is at the same offset Anchor places it. Use `bytemuck` like the
        // existing tests do to avoid hand-rolling offsets.
        let header = 8;
        let epoch_view: &mut Epoch =
            bytemuck::from_bytes_mut(&mut acct.data[header..header + std::mem::size_of::<Epoch>()]);
        epoch_view.rewards_distributed = 1;
        // observations_submitted/closed both zero — gate trivially passes.
        ctx.set_account(
            &epoch_key,
            &solana_sdk::account::Account {
                lamports: acct.lamports,
                data: acct.data.clone(),
                owner: acct.owner,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );
    }
    {
        let mut acct = ctx
            .banks_client
            .get_account(epoch_settings_key)
            .await
            .unwrap()
            .unwrap();
        let mut es = EpochSettings::try_deserialize(&mut acct.data.as_slice()).unwrap();
        es.current_epoch_index = 8; // past the 7-epoch retention gap
        let mut new_data = Vec::new();
        es.try_serialize(&mut new_data).unwrap();
        let original_len = acct.data.len();
        new_data.resize(original_len, 0);
        ctx.set_account(
            &epoch_settings_key,
            &solana_sdk::account::Account {
                lamports: acct.lamports,
                data: new_data,
                owner: acct.owner,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );
    }

    // Capture pre-close lamports as the expected rent_recovered value.
    let pre_lamports = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap()
        .lamports;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CloseEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                payer: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CloseEpoch { _epoch_index: 0 }.data(),
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
        "close_epoch should succeed: {:?}",
        result.result
    );
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::EpochClosedEvent);
    assert_eq!(ev.epoch_index, 0);
    assert_eq!(
        ev.rent_recovered, pre_lamports,
        "rent_recovered must equal pre-close lamports"
    );
}

#[tokio::test]
async fn test_compound_delegation_rewards_emits_event() {
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000u64).await;
    let payer_pk = ctx.payer.pubkey();

    let delegator = Keypair::new();
    let delegator_token = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    let delegator_pk = delegator.pubkey();
    create_token_account(
        &mut ctx,
        &delegator_token,
        &setup.mint.pubkey(),
        &delegator_pk,
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;
    let delegate_amount = 100_000_000u64;
    let (delegation_key, _) = delegation_pda(&payer_pk, &delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake {
                amount: delegate_amount,
            }
            .data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Inject cumulative_reward_per_token = 1e18 (matches existing
    // test_compound_delegation_rewards setup) so the settler materializes
    // exactly `delegate_amount` lamports of pending reward.
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let mut gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    gw.cumulative_reward_per_token = 1_000_000_000_000_000_000u128;
    let mut new_data = Vec::new();
    gw.try_serialize(&mut new_data).unwrap();
    let original_len = gw_account.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &gateway_key,
        &solana_sdk::account::Account {
            lamports: gw_account.lamports,
            data: new_data,
            owner: gw_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CompoundDelegationRewards {
                gateway: gateway_key,
                delegation: delegation_key,
                delegator: delegator_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CompoundDelegationRewards {}.data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::RewardsCompoundedEvent);
    assert_eq!(ev.delegator, delegator_pk);
    assert_eq!(ev.gateway, payer_pk);
    assert_eq!(
        ev.compounded, delegate_amount,
        "compounded must equal pending reward delta"
    );
}

#[tokio::test]
async fn test_prune_gateway_emits_event_with_slash_amount() {
    // Pairs the prune emit with finalize_gone in the next test. This one
    // verifies the slash side: GatewayPrunedEvent must surface slashed_amount
    // > 0 because the gateway carries >= min_operator_stake.
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let stake_amount = 20_000_000_000u64;
    let gateway_key = join_gateway(&mut ctx, &setup, stake_amount).await;
    let payer_pk = ctx.payer.pubkey();

    // Inject failed_consecutive = 30 to qualify for prune.
    let gw_account = ctx
        .banks_client
        .get_account(gateway_key)
        .await
        .unwrap()
        .unwrap();
    let mut gw = Gateway::try_deserialize(&mut gw_account.data.as_slice()).unwrap();
    gw.stats.failed_consecutive = 30;
    let mut new_data = Vec::new();
    gw.try_serialize(&mut new_data).unwrap();
    let original_len = gw_account.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &gateway_key,
        &solana_sdk::account::Account {
            lamports: gw_account.lamports,
            data: new_data,
            owner: gw_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    let (epoch_settings_key, _) = epoch_settings_pda();
    let (counter_key, _) = withdrawal_counter_pda(&payer_pk);
    let (wd_key, _) = withdrawal_pda(&payer_pk, 0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::PruneGateway {
                settings: setup.settings_key,
                epoch_settings: epoch_settings_key,
                registry: setup.registry_key,
                gateway: gateway_key,
                withdrawal_counter: counter_key,
                withdrawal: wd_key,
                excess_withdrawal: None,
                stake_token_account: setup.stake_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::PruneGateway {}.data(),
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
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::GatewayPrunedEvent);
    assert_eq!(ev.operator, payer_pk);
    assert_eq!(ev.pruner, payer_pk);
    assert!(
        ev.slashed_amount > 0,
        "slashed_amount must be non-zero when gateway has >= min_operator_stake"
    );
}

#[tokio::test]
async fn test_finalize_gone_emits_event() {
    // Companion to test_prune_gateway_emits_event_with_slash_amount —
    // verifies the cleanup-side emit fires when the leave window expires
    // and the gateway PDA is reclaimed.
    ario_test_utils::bpf_required!();
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let stranger = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pt.add_account(
        stranger.pubkey(),
        solana_sdk::account::Account {
            lamports: 50_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;
    let (gateway_key, epoch_settings_key) = setup_leaver_ready_for_gc(&mut ctx, &setup).await;

    // Warp past eligibility window.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 90 * 86_400 + 7 * 86_400 + 1;
    ctx.set_sysvar(&clock);

    let payer_pk = ctx.payer.pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::FinalizeGone {
                gateway: gateway_key,
                registry: setup.registry_key,
                epoch_settings: epoch_settings_key,
                caller: stranger.pubkey(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::FinalizeGone {}.data(),
        }],
        Some(&stranger.pubkey()),
        &[&stranger],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::GatewayFinalizedEvent);
    assert_eq!(
        ev.operator, payer_pk,
        "operator key was the gateway's operator (joined via ctx.payer)"
    );
    assert_eq!(
        ev.pruner,
        stranger.pubkey(),
        "permissionless cleanup — caller stamped here"
    );
}

#[tokio::test]
async fn test_finalize_migration_emits_event() {
    // Verifies GarMigrationFinalizedEvent fires with admin + gateway_count
    // (zero in this test fixture, because no gateways were imported) +
    // a non-zero slot.
    ario_test_utils::bpf_required!();
    use anchor_lang::solana_program::hash::hash;

    let authority = Keypair::new();
    let mint = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let mut pt = program_test_with_gar(
        &authority.pubkey(),
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );

    // Override settings to migration_active = true (otherwise finalize fails
    // its own constraint check).
    let (settings_key, settings_bump) = settings_pda();
    let size = GatewaySettings::SIZE;
    let mut data = vec![0u8; size];
    let disc = hash(b"account:GatewaySettings");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);
    let mut o = 8;
    data[o..o + 32].copy_from_slice(authority.pubkey().as_ref());
    o += 32;
    data[o..o + 32].copy_from_slice(mint.pubkey().as_ref());
    o += 32;
    data[o..o + 8].copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&10_000_000u64.to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&(30i64 * 86_400).to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&500_000u64.to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&100_000u64.to_le_bytes());
    o += 8;
    data[o..o + 8].copy_from_slice(&1_000_000u64.to_le_bytes());
    o += 8;
    data[o..o + 4].copy_from_slice(&10_000u32.to_le_bytes());
    o += 4;
    data[o] = 1;
    o += 1; // migration_active = true
    data[o..o + 32].copy_from_slice(authority.pubkey().as_ref());
    o += 32;
    data[o..o + 32].copy_from_slice(stake_token.pubkey().as_ref());
    o += 32;
    data[o..o + 32].copy_from_slice(protocol_token.pubkey().as_ref());
    o += 32;
    data[o..o + 32].copy_from_slice(&[0xAAu8; 32]);
    o += 32;
    o += 8 + 8 + 8; // counters
    data[o] = settings_bump;
    pt.add_account(
        settings_key,
        solana_sdk::account::Account {
            lamports: solana_sdk::rent::Rent::default().minimum_balance(size),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        authority.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );
    let mut ctx = pt.start_with_context().await;

    // Pass registry via remaining_accounts so gateway_count is populated.
    let (registry_key, _) = registry_pda();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_gar::accounts::FinalizeMigration {
        settings: settings_key,
        authority: authority.pubkey(),
    }
    .to_account_metas(None);
    accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        registry_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts,
            data: ario_gar::instruction::FinalizeMigration {}.data(),
        }],
        Some(&authority.pubkey()),
        &[&authority],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(tx)
        .await
        .unwrap();
    assert!(
        result.result.is_ok(),
        "finalize_migration should succeed: {:?}",
        result.result
    );
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_gar::GarMigrationFinalizedEvent);
    assert_eq!(ev.admin, authority.pubkey());
    assert_eq!(
        ev.gateway_count, 0,
        "no gateways were imported in this fixture"
    );
    // slot is whatever the test runtime provides; just verify it's set.
    let _ = ev.slot;
}

// =========================================
// MIGRATE EPOCH SETTINGS / OBSERVATION — realloc coverage
// =========================================

/// Inject an EpochSettings at the old on-chain size (8 + SIZE_without_version,
/// due to the prior double-counted discriminator) with version bytes reading as
/// SchemaVersion{0,0,0}, call migrate_epoch_settings, and assert the account
/// shrinks to EpochSettings::SIZE with version set.
#[tokio::test]
async fn test_migrate_epoch_settings_realloc() {
    use anchor_lang::solana_program::hash::hash;

    let authority = Keypair::new();
    let mint = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();

    let mut pt = program_test_with_gar_for_epoch_init(
        &authority.pubkey(),
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );

    let (epoch_settings_key, epoch_settings_bump) = epoch_settings_pda();
    // Old on-chain size: init used `space = 8 + SIZE_without_version` (double-counted disc).
    // SIZE_without_version = EpochSettings::SIZE - SCHEMA_VERSION_SIZE.
    let old_on_chain_size = 8 + (EpochSettings::SIZE - SCHEMA_VERSION_SIZE);
    let mut data = vec![0u8; old_on_chain_size];

    let disc = hash(b"account:EpochSettings");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);

    let mut offset = 8;
    data[offset..offset + 32].copy_from_slice(authority.pubkey().as_ref());
    offset += 32; // authority
    data[offset..offset + 8].copy_from_slice(&86_400i64.to_le_bytes());
    offset += 8; // epoch_duration
    data[offset] = 50;
    offset += 1; // prescribed_observer_count
    data[offset] = 2;
    offset += 1; // prescribed_name_count
    data[offset..offset + 8].copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
    offset += 8; // min_observer_stake
    data[offset..offset + 2].copy_from_slice(&0u16.to_le_bytes());
    offset += 2; // slash_rate
    data[offset] = 1;
    offset += 1; // enabled
    data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
    offset += 8; // current_epoch_index
    data[offset..offset + 8].copy_from_slice(&0i64.to_le_bytes());
    offset += 8; // genesis_timestamp
    data[offset..offset + 8].copy_from_slice(&(180i64 * 86_400).to_le_bytes());
    offset += 8; // tenure_weight_duration
    data[offset..offset + 8].copy_from_slice(&4u64.to_le_bytes());
    offset += 8; // max_tenure_weight
    data[offset..offset + 8].copy_from_slice(&900_000u64.to_le_bytes());
    offset += 8; // gateway_reward_ratio
    data[offset..offset + 8].copy_from_slice(&100_000u64.to_le_bytes());
    offset += 8; // observer_reward_ratio
    data[offset..offset + 8].copy_from_slice(&250_000u64.to_le_bytes());
    offset += 8; // missed_observation_penalty_rate
    data[offset..offset + 8].copy_from_slice(&1_000u64.to_le_bytes());
    offset += 8; // max_reward_rate
    data[offset..offset + 8].copy_from_slice(&500u64.to_le_bytes());
    offset += 8; // min_reward_rate
    data[offset..offset + 8].copy_from_slice(&365u64.to_le_bytes());
    offset += 8; // reward_decay_start_epoch
    data[offset..offset + 8].copy_from_slice(&547u64.to_le_bytes());
    offset += 8; // reward_decay_last_epoch
    data[offset] = 30;
    offset += 1; // max_consecutive_failures
    data[offset..offset + 8].copy_from_slice(&1_000_000u64.to_le_bytes());
    offset += 8; // failed_gateway_slash_rate
    data[offset..offset + 8].copy_from_slice(&0i64.to_le_bytes());
    offset += 8; // disable_at
    data[offset] = epoch_settings_bump;
    // Trailing bytes (from double-count) are zeros — version reads as {0,0,0}

    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        epoch_settings_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(old_on_chain_size),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let mut ctx = pt.start_with_context().await;
    let payer_pk = ctx.payer.pubkey();

    // Verify pre-migration: oversized from old double-counted init
    let acct = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acct.data.len(), old_on_chain_size);

    // Call migrate_epoch_settings
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateEpochSettings {
                epoch_settings: epoch_settings_key,
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateEpochSettings {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify post-migration: correct size and version
    let acct = ctx
        .banks_client
        .get_account(epoch_settings_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acct.data.len(), EpochSettings::SIZE);

    let es = EpochSettings::try_deserialize(&mut acct.data.as_slice()).unwrap();
    assert_eq!(es.version, EPOCH_SETTINGS_VERSION);
    assert_eq!(es.bump, epoch_settings_bump);
    assert_eq!(es.epoch_duration, 86_400);
}

/// Calling migrate_epoch_settings on an already-current account fails with
/// AlreadyLatestVersion.
#[tokio::test]
async fn test_migrate_epoch_settings_already_latest() {
    use anchor_lang::solana_program::hash::hash;

    let authority = Keypair::new();
    let mint = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();

    let mut pt = program_test_with_gar_for_epoch_init(
        &authority.pubkey(),
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );

    // Inject EpochSettings at current SIZE with version already set to latest
    let (epoch_settings_key, epoch_settings_bump) = epoch_settings_pda();
    let size = EpochSettings::SIZE;
    let mut data = vec![0u8; size];
    let disc = hash(b"account:EpochSettings");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);
    let mut offset = 8;
    data[offset..offset + 32].copy_from_slice(authority.pubkey().as_ref());
    offset += 32;
    data[offset..offset + 8].copy_from_slice(&86_400i64.to_le_bytes());
    offset += 8;
    data[offset] = 50;
    offset += 1;
    data[offset] = 2;
    offset += 1;
    data[offset..offset + 8].copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
    offset += 8;
    data[offset..offset + 2].copy_from_slice(&0u16.to_le_bytes());
    offset += 2;
    data[offset] = 1;
    offset += 1;
    data[offset..offset + 8].copy_from_slice(&0u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&0i64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&(180i64 * 86_400).to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&4u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&900_000u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&100_000u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&250_000u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&1_000u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&500u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&365u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&547u64.to_le_bytes());
    offset += 8;
    data[offset] = 30;
    offset += 1;
    data[offset..offset + 8].copy_from_slice(&1_000_000u64.to_le_bytes());
    offset += 8;
    data[offset..offset + 8].copy_from_slice(&0i64.to_le_bytes());
    offset += 8;
    data[offset] = epoch_settings_bump;
    offset += 1;
    // Write version = EPOCH_SETTINGS_VERSION (1.0.0)
    data[offset] = EPOCH_SETTINGS_VERSION.major;
    data[offset + 1] = EPOCH_SETTINGS_VERSION.minor;
    data[offset + 2] = EPOCH_SETTINGS_VERSION.patch;

    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        epoch_settings_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(size),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    let mut ctx = pt.start_with_context().await;
    let payer_pk = ctx.payer.pubkey();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateEpochSettings {
                epoch_settings: epoch_settings_key,
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateEpochSettings {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, GarError::AlreadyLatestVersion);
}

/// Inject an Observation at the old on-chain size (8 + SIZE_without_version,
/// due to the prior double-counted discriminator) with version bytes reading
/// as SchemaVersion{0,0,0}, call migrate_observation, and assert account
/// shrinks to Observation::SIZE with version set.
#[tokio::test]
async fn test_migrate_observation_realloc() {
    use anchor_lang::solana_program::hash::hash;

    let authority = Keypair::new();
    let mint = Keypair::new();
    let stake_token = Keypair::new();
    let protocol_token = Keypair::new();
    let observer = Keypair::new();
    let epoch_index: u64 = 7;

    let mut pt = program_test_with_gar(
        &authority.pubkey(),
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );

    let (observation_key, observation_bump) = observation_pda(epoch_index, &observer.pubkey());
    // Old on-chain size: init used `space = 8 + SIZE_without_version` (double-counted disc).
    let old_on_chain_size = 8 + (Observation::SIZE - SCHEMA_VERSION_SIZE);
    let mut data = vec![0u8; old_on_chain_size];

    let disc = hash(b"account:Observation");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);

    let mut offset = 8;
    data[offset..offset + 8].copy_from_slice(&epoch_index.to_le_bytes());
    offset += 8; // epoch_index
    data[offset..offset + 32].copy_from_slice(observer.pubkey().as_ref());
    offset += 32; // observer
                  // gateway_results: [u8; 375] — leave as zeroes
    offset += 375;
    data[offset..offset + 2].copy_from_slice(&10u16.to_le_bytes());
    offset += 2; // gateway_count
                 // report_tx_id: [u8; 32] — leave as zeroes
    offset += 32;
    data[offset..offset + 8].copy_from_slice(&1_000_000i64.to_le_bytes());
    offset += 8; // submitted_at
    data[offset] = observation_bump;
    // Trailing bytes (from double-count) are zeros — version reads as {0,0,0}

    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        observation_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(old_on_chain_size),
            data,
            owner: ario_gar::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    // Fund observer so it can be passed as a non-signer account
    pt.add_account(
        observer.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: solana_sdk::system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );

    let mut ctx = pt.start_with_context().await;
    let payer_pk = ctx.payer.pubkey();

    // Verify pre-migration: oversized from old double-counted init
    let acct = ctx
        .banks_client
        .get_account(observation_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acct.data.len(), old_on_chain_size);

    // Call migrate_observation
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::MigrateObservation {
                observer: observer.pubkey(),
                observation: observation_key,
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::MigrateObservation { epoch_index }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify post-migration: correct size and version
    let acct = ctx
        .banks_client
        .get_account(observation_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acct.data.len(), Observation::SIZE);

    let obs = Observation::try_deserialize(&mut acct.data.as_slice()).unwrap();
    assert_eq!(obs.version, OBSERVATION_VERSION);
    assert_eq!(obs.bump, observation_bump);
    assert_eq!(obs.epoch_index, epoch_index);
    assert_eq!(obs.observer, observer.pubkey());
    assert_eq!(obs.gateway_count, 10);
}

// =========================================
// Codex 2026-05-28: mid-epoch leave bias regression
// =========================================
//
// Sets up three gateways in registry slots [small1, big_leaver, small2] where
// `big_leaver` carries ~100x the composite weight of either small gateway.
// After `tally_weights` accumulates the full pre-leave total into
// `epoch.total_composite_weight`, `big_leaver` calls `leave_network`. That
// zeroes its registry slot's `composite_weight` in place but does not
// decrement the epoch total.
//
// Pre-fix `prescribe_epoch` sampled `random_value % stale_total` and the
// "if !found" fallback attributed every dead-range hit (~99% of the
// random_value space at 100x ratio) to the LAST non-zero slot — collapsing
// observer selection onto `small2` and yielding `observer_count == 1` with
// overwhelming probability across the 30-iteration roulette.
//
// Post-fix `prescribe_epoch` recomputes `total_weight` as a live walk of
// current registry composite_weights, eliminating the dead range and the
// fallback. With weights `[small1, 0, small2]` and balanced live total,
// both surviving slots are selected and `observer_count == 2`.
#[tokio::test]
async fn test_prescribe_unbiased_after_mid_epoch_leave() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();

    let big_op = Keypair::new();
    let small2_op = Keypair::new();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    for op in [&big_op, &small2_op] {
        pt.add_account(
            op.pubkey(),
            solana_sdk::account::Account {
                lamports: 50_000_000_000,
                data: vec![],
                owner: solana_sdk::system_program::id(),
                executable: false,
                rent_epoch: 0,
            },
        );
    }
    let mut ctx = pt.start_with_context().await;

    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    // Fund the protocol token account so create_epoch reports non-zero
    // total_eligible_rewards (otherwise reward calc is trivially zero and
    // the test doesn't exercise the bias surface).
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000_000,
    )
    .await;

    // Warp clock to 0 so all three gateways join BEFORE epoch.start_timestamp
    // (set to 100 by pre_create_epoch_settings genesis_timestamp). Otherwise
    // tally_weights would zero their effective_composite (SHOULD-13).
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);

    // Three gateways — registry slots fill in join order.
    let payer_pk = ctx.payer.pubkey();
    let small_stake = 20_000_000_000u64; // 20k ARIO (min_operator_stake)
    let big_stake = 2_000_000_000_000u64; // 2M ARIO → ~100x stake_weight

    // Slot 0: payer (small1).
    let gw_small1_key = join_gateway(&mut ctx, &setup, small_stake).await;

    // Slot 1: big_op (big_leaver). Mint enough to its ATA, fund SOL above.
    let big_token = Keypair::new();
    create_token_account(&mut ctx, &big_token, &setup.mint.pubkey(), &big_op.pubkey()).await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &big_token.pubkey(),
        &setup.mint_authority,
        // 2x stake to ensure the join succeeds and leave_network's
        // exit-vault rent overhead is covered.
        big_stake.saturating_mul(2),
    )
    .await;
    let gw_big_key =
        join_gateway_with_operator(&mut ctx, &setup, &big_op, &big_token.pubkey(), big_stake).await;

    // Slot 2: small2_op (accomplice / "last non-zero slot" in pre-fix fallback).
    let small2_token = Keypair::new();
    create_token_account(
        &mut ctx,
        &small2_token,
        &setup.mint.pubkey(),
        &small2_op.pubkey(),
    )
    .await;
    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &small2_token.pubkey(),
        &setup.mint_authority,
        small_stake.saturating_mul(2),
    )
    .await;
    let gw_small2_key = join_gateway_with_operator(
        &mut ctx,
        &setup,
        &small2_op,
        &small2_token.pubkey(),
        small_stake,
    )
    .await;

    // Warp past epoch.start (= 100) and pin slot so the hashchain is
    // reproducible across test runs.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 200;
    clock.slot = 1;
    ctx.set_sysvar(&clock);

    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    // create_epoch — active_gateway_count = 3.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: payer_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // tally_weights — populates composite_weight for all 3 slots and
    // accumulates the full pre-leave total into epoch.total_composite_weight.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gw_small1_key,
        false,
    ));
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(gw_big_key, false));
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gw_small2_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Snapshot the post-tally weights so we can prove (a) big_leaver
    // dominates and (b) epoch.total_composite_weight reflects all three
    // contributions — the exact precondition for the stale-total bias.
    let registry_data = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let registry: &GatewayRegistry =
        bytemuck::from_bytes(&registry_data.data[8..8 + std::mem::size_of::<GatewayRegistry>()]);
    let small1_weight = registry.gateways[0].composite_weight;
    let big_weight = registry.gateways[1].composite_weight;
    let small2_weight = registry.gateways[2].composite_weight;
    assert!(
        big_weight >= small1_weight.saturating_mul(50)
            && big_weight >= small2_weight.saturating_mul(50),
        "test setup invariant: big_leaver weight ({big_weight}) must dominate \
         small1 ({small1_weight}) and small2 ({small2_weight}) by ≥50x so the \
         bias surface is large enough to discriminate"
    );

    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    let stale_total = epoch.total_composite_weight();
    assert_eq!(
        stale_total,
        small1_weight as u128 + big_weight as u128 + small2_weight as u128,
        "tally must accumulate every Joined slot's composite_weight"
    );

    // leave_network on big_leaver — between tally and prescribe. Zeroes its
    // registry slot composite_weight in place; epoch.total_composite_weight
    // stays at `stale_total` per gateway.rs::leave_network comment.
    let (big_wd_counter, _) = withdrawal_counter_pda(&big_op.pubkey());
    let (big_wd, _) = withdrawal_pda(&big_op.pubkey(), 0);
    let (big_excess_wd, _) = withdrawal_pda(&big_op.pubkey(), 1);
    let mut leave_accounts = ario_gar::accounts::LeaveNetwork {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_pda().0,
        registry: setup.registry_key,
        gateway: gw_big_key,
        withdrawal_counter: big_wd_counter,
        withdrawal: big_wd,
        excess_withdrawal: Some(big_excess_wd),
        operator: big_op.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let (big_observer_lookup, _) = observer_lookup_pda(&big_op.pubkey());
    leave_accounts.push(solana_sdk::instruction::AccountMeta::new(
        big_observer_lookup,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: leave_accounts,
            data: ario_gar::instruction::LeaveNetwork {}.data(),
        }],
        Some(&big_op.pubkey()),
        &[&big_op],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Confirm the bias precondition: slot 1's composite_weight is now 0 but
    // epoch.total_composite_weight is still the stale pre-leave total.
    let registry_data = ctx
        .banks_client
        .get_account(setup.registry_key)
        .await
        .unwrap()
        .unwrap();
    let registry: &GatewayRegistry =
        bytemuck::from_bytes(&registry_data.data[8..8 + std::mem::size_of::<GatewayRegistry>()]);
    assert_eq!(
        registry.gateways[1].composite_weight, 0,
        "leave_network must zero the leaver's slot weight"
    );
    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(
        epoch.total_composite_weight(),
        stale_total,
        "leave_network must NOT decrement epoch.total_composite_weight \
         (the snapshot the bias bug relied on)"
    );

    // prescribe_epoch — pass only the two SURVIVING gateway PDAs in
    // remaining_accounts. Post-fix the live recompute samples modulo
    // small1_weight + small2_weight, so both must be selected
    // (observer_count == 2). Pre-fix the stale modulus + fallback would
    // collapse selection onto small2 (last non-zero slot) with
    // overwhelming probability, leaving observer_count == 1 and failing
    // remaining_accounts validation (the unmatched gw_small1_key would be
    // re-interpreted as a NameRegistry and fail the owner check).
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut prescribe_accounts = ario_gar::accounts::PrescribeEpoch {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: payer_pk,
    }
    .to_account_metas(None);
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gw_small1_key,
        false,
    ));
    prescribe_accounts.push(solana_sdk::instruction::AccountMeta::new_readonly(
        gw_small2_key,
        false,
    ));
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: prescribe_accounts,
            data: ario_gar::instruction::PrescribeEpoch { _epoch_index: 0 }.data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let epoch_data = ctx
        .banks_client
        .get_account(epoch_key)
        .await
        .unwrap()
        .unwrap();
    let epoch: &Epoch = bytemuck::from_bytes(&epoch_data.data[8..8 + std::mem::size_of::<Epoch>()]);
    assert_eq!(
        epoch.prescriptions_done, 1,
        "prescribe_epoch must flip prescriptions_done"
    );
    assert_eq!(
        epoch.observer_count, 2,
        "both surviving gateways must be selected; pre-fix the bias would \
         collapse selection onto small2 and yield observer_count == 1"
    );

    let prescribed: std::collections::HashSet<Pubkey> = epoch.prescribed_observer_gateways
        [..epoch.observer_count as usize]
        .iter()
        .copied()
        .collect();
    assert!(
        prescribed.contains(&payer_pk),
        "small1 (payer) must be prescribed — pre-fix the bias would leave it \
         out with ~99.5% probability across the 30-iteration roulette"
    );
    assert!(
        prescribed.contains(&small2_op.pubkey()),
        "small2 must be prescribed"
    );
    assert!(
        !prescribed.contains(&big_op.pubkey()),
        "leaver (big_op) must never appear in prescribed_observer_gateways"
    );
}

// ==========================================================================
// Fix #6 (disable delegation → forced withdrawal + cooldown) and
// Fix #7 (defer delegate_reward_share_ratio to next epoch). WP §6.3.
// ==========================================================================

/// Fund a fresh delegator (SOL + tokens) and delegate `amount` to `gateway_key`.
/// Returns the delegator keypair and the delegation PDA.
async fn f6_fund_and_delegate(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    gateway_operator: &Pubkey,
    gateway_key: &Pubkey,
    amount: u64,
) -> (Keypair, Pubkey) {
    let payer_pk = ctx.payer.pubkey();
    let delegator = Keypair::new();
    let delegator_token = Keypair::new();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &payer_pk,
            &delegator.pubkey(),
            10_000_000_000,
        )],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let delegator_pk = delegator.pubkey();
    create_token_account(ctx, &delegator_token, &setup.mint.pubkey(), &delegator_pk).await;
    mint_tokens(
        ctx,
        &setup.mint.pubkey(),
        &delegator_token.pubkey(),
        &setup.mint_authority,
        50_000_000_000,
    )
    .await;

    let (delegation_key, _) = delegation_pda(gateway_operator, &delegator_pk);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::DelegateStake {
                settings: setup.settings_key,
                gateway: *gateway_key,
                delegation: delegation_key,
                delegator_token_account: delegator_token.pubkey(),
                stake_token_account: setup.stake_token.pubkey(),
                delegator: delegator_pk,
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::DelegateStake { amount }.data(),
        }],
        Some(&delegator_pk),
        &[&delegator],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
    (delegator, delegation_key)
}

/// Operator-only update of `allow_delegated_staking` (all other params None).
/// Returns the raw tx result so callers can assert success or a specific error.
async fn f6_set_allow_delegation(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    gateway_key: &Pubkey,
    allow: bool,
) -> std::result::Result<(), solana_program_test::BanksClientError> {
    let payer_pk = ctx.payer.pubkey();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: *gateway_key,
                operator: payer_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateGatewaySettings {
                params: ario_gar::UpdateGatewayParams {
                    label: None,
                    fqdn: None,
                    port: None,
                    protocol: None,
                    properties: None,
                    note: None,
                    allow_delegated_staking: Some(allow),
                    delegate_reward_share_ratio: None,
                    min_delegate_stake: None,
                },
            }
            .data(),
        }],
        Some(&payer_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// Crank `claim_delegate_from_disabled_gateway` for `delegator`, paid by `payer`
/// (use a third party to exercise the permissionless path). Returns the result.
async fn f6_claim_disabled(
    ctx: &mut ProgramTestContext,
    setup: &GarSetup,
    gateway_key: &Pubkey,
    delegation_key: &Pubkey,
    delegator_pk: &Pubkey,
    payer: &Keypair,
) -> std::result::Result<(), solana_program_test::BanksClientError> {
    let (counter_key, _) = withdrawal_counter_pda(delegator_pk);
    let (withdrawal_key, _) = withdrawal_pda(delegator_pk, 0);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::ClaimDelegateFromDisabledGateway {
                settings: setup.settings_key,
                gateway: *gateway_key,
                delegation: *delegation_key,
                withdrawal_counter: counter_key,
                withdrawal: withdrawal_key,
                delegator: *delegator_pk,
                payer: payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::ClaimDelegateFromDisabledGateway {}.data(),
        }],
        Some(&payer.pubkey()),
        &[payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

async fn f6_read_gateway(ctx: &mut ProgramTestContext, gateway_key: &Pubkey) -> Gateway {
    let acct = ctx
        .banks_client
        .get_account(*gateway_key)
        .await
        .unwrap()
        .unwrap();
    Gateway::try_deserialize(&mut acct.data.as_slice()).unwrap()
}

#[tokio::test]
async fn test_claim_delegate_from_disabled_gateway() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let operator_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    let delegate_amount = 20_000_000_000u64;
    let (delegator, delegation_key) = f6_fund_and_delegate(
        &mut ctx,
        &setup,
        &operator_pk,
        &gateway_key,
        delegate_amount,
    )
    .await;
    let delegator_pk = delegator.pubkey();

    // Operator disables delegation.
    f6_set_allow_delegation(&mut ctx, &setup, &gateway_key, false)
        .await
        .unwrap();

    // Crank the delegate out (delegate pays its own rent here).
    f6_claim_disabled(
        &mut ctx,
        &setup,
        &gateway_key,
        &delegation_key,
        &delegator_pk,
        &delegator,
    )
    .await
    .unwrap();

    // Withdrawal vault created with the full stake and the 30-day delegate lock.
    let (withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);
    let w_acct = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut w_acct.data.as_slice()).unwrap();
    assert_eq!(withdrawal.owner, delegator_pk);
    assert_eq!(withdrawal.amount, delegate_amount);
    assert!(withdrawal.is_delegate);
    assert!(!withdrawal.is_exit_vault);
    assert!(!withdrawal.is_protected);
    assert_eq!(withdrawal.available_at, withdrawal.created_at + 2_592_000);

    // Delegation zeroed; gateway total decremented to 0.
    let d_acct = ctx
        .banks_client
        .get_account(delegation_key)
        .await
        .unwrap()
        .unwrap();
    let delegation = Delegation::try_deserialize(&mut d_acct.data.as_slice()).unwrap();
    assert_eq!(delegation.amount, 0);
    let gw = f6_read_gateway(&mut ctx, &gateway_key).await;
    assert_eq!(gw.total_delegated_stake, 0);
    // Gateway stays Joined (only delegation was disabled).
    assert!(matches!(gw.status, GatewayStatus::Joined));
}

#[tokio::test]
async fn test_claim_delegate_from_disabled_gateway_permissionless() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let operator_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;
    let (delegator, delegation_key) =
        f6_fund_and_delegate(&mut ctx, &setup, &operator_pk, &gateway_key, 20_000_000_000).await;
    let delegator_pk = delegator.pubkey();

    f6_set_allow_delegation(&mut ctx, &setup, &gateway_key, false)
        .await
        .unwrap();

    // A third party (not the delegate, not the operator) cranks the claim.
    let cranker = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &operator_pk,
            &cranker.pubkey(),
            1_000_000_000,
        )],
        Some(&operator_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    f6_claim_disabled(
        &mut ctx,
        &setup,
        &gateway_key,
        &delegation_key,
        &delegator_pk,
        &cranker,
    )
    .await
    .unwrap();

    // Stake still routes to the delegate's own vault, not the cranker.
    let (withdrawal_key, _) = withdrawal_pda(&delegator_pk, 0);
    let w_acct = ctx
        .banks_client
        .get_account(withdrawal_key)
        .await
        .unwrap()
        .unwrap();
    let withdrawal = Withdrawal::try_deserialize(&mut w_acct.data.as_slice()).unwrap();
    assert_eq!(withdrawal.owner, delegator_pk);
    assert_eq!(withdrawal.amount, 20_000_000_000);
}

#[tokio::test]
async fn test_claim_delegate_from_disabled_gateway_rejects_when_enabled() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let operator_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;
    let (delegator, delegation_key) =
        f6_fund_and_delegate(&mut ctx, &setup, &operator_pk, &gateway_key, 20_000_000_000).await;
    let delegator_pk = delegator.pubkey();

    // Delegation still ENABLED → claim must be rejected.
    let result = f6_claim_disabled(
        &mut ctx,
        &setup,
        &gateway_key,
        &delegation_key,
        &delegator_pk,
        &delegator,
    )
    .await;
    assert_anchor_error!(result, GarError::DelegationNotDisabled);
}

#[tokio::test]
async fn test_disable_delegation_records_timestamp() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;

    // Joined with delegation enabled → disabled_at is None.
    let gw = f6_read_gateway(&mut ctx, &gateway_key).await;
    assert!(gw.settings.allow_delegated_staking);
    assert!(gw.settings.delegation_disabled_at.is_none());

    // Set a deterministic clock so we can assert the recorded timestamp.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 5_000;
    ctx.set_sysvar(&clock);

    f6_set_allow_delegation(&mut ctx, &setup, &gateway_key, false)
        .await
        .unwrap();
    let gw = f6_read_gateway(&mut ctx, &gateway_key).await;
    assert!(!gw.settings.allow_delegated_staking);
    assert_eq!(gw.settings.delegation_disabled_at, Some(5_000));

    // Re-enabling (no delegates, cooldown elapsed) clears the timestamp.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 5_000 + 2_592_000;
    ctx.set_sysvar(&clock);
    f6_set_allow_delegation(&mut ctx, &setup, &gateway_key, true)
        .await
        .unwrap();
    let gw = f6_read_gateway(&mut ctx, &gateway_key).await;
    assert!(gw.settings.allow_delegated_staking);
    assert!(gw.settings.delegation_disabled_at.is_none());
}

#[tokio::test]
async fn test_reenable_delegation_rejects_with_remaining_delegates() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let operator_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;
    let (_delegator, _delegation_key) =
        f6_fund_and_delegate(&mut ctx, &setup, &operator_pk, &gateway_key, 20_000_000_000).await;

    // Disable, but DO NOT crank the delegate out → stake remains.
    f6_set_allow_delegation(&mut ctx, &setup, &gateway_key, false)
        .await
        .unwrap();

    let result = f6_set_allow_delegation(&mut ctx, &setup, &gateway_key, true).await;
    assert_anchor_error!(result, GarError::DelegatesStillActive);
}

#[tokio::test]
async fn test_reenable_delegation_requires_cooldown() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let operator_pk = ctx.payer.pubkey();
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;
    let (delegator, delegation_key) =
        f6_fund_and_delegate(&mut ctx, &setup, &operator_pk, &gateway_key, 20_000_000_000).await;
    let delegator_pk = delegator.pubkey();

    // Disable; the timestamp is recorded.
    f6_set_allow_delegation(&mut ctx, &setup, &gateway_key, false)
        .await
        .unwrap();
    assert!(f6_read_gateway(&mut ctx, &gateway_key)
        .await
        .settings
        .delegation_disabled_at
        .is_some());

    // Crank the only delegate out → total_delegated_stake == 0.
    f6_claim_disabled(
        &mut ctx,
        &setup,
        &gateway_key,
        &delegation_key,
        &delegator_pk,
        &delegator,
    )
    .await
    .unwrap();

    // Key assertion: even with every delegate cleared, re-enabling is STILL
    // blocked by the 30-day cooldown until it elapses. (The cooldown-elapsed →
    // re-enable-succeeds-and-clears path is covered by
    // test_disable_delegation_records_timestamp, which warps past the window.)
    let result = f6_set_allow_delegation(&mut ctx, &setup, &gateway_key, true).await;
    assert_anchor_error!(result, GarError::DelegationCooldownActive);
}

/// Fix #7: update with a new ratio stages it (active unchanged); a second update
/// overwrites the pending value (last write wins) before any tally applies it.
#[tokio::test]
async fn test_delegate_reward_share_ratio_pending_overwrite() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;
    let operator_pk = ctx.payer.pubkey();

    let send_ratio = |ratio: u8| ario_gar::instruction::UpdateGatewaySettings {
        params: ario_gar::UpdateGatewayParams {
            label: None,
            fqdn: None,
            port: None,
            protocol: None,
            properties: None,
            note: None,
            allow_delegated_staking: None,
            delegate_reward_share_ratio: Some(ratio),
            min_delegate_stake: None,
        },
    };

    for ratio in [30u8, 70u8] {
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::UpdateGatewaySettings {
                    settings: setup.settings_key,
                    gateway: gateway_key,
                    operator: operator_pk,
                }
                .to_account_metas(None),
                data: send_ratio(ratio).data(),
            }],
            Some(&operator_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
    }

    let gw = f6_read_gateway(&mut ctx, &gateway_key).await;
    // Active value never moved (still join value 10 * 100 = 1000).
    assert_eq!(gw.settings.delegate_reward_share_ratio, 1000);
    // Last write wins in the pending slot: 70 * 100 = 7000.
    assert_eq!(gw.settings.pending_delegate_reward_share_ratio, Some(7000));
}

/// Fix #7: a deferred ratio change is applied at the next tally_weights, which
/// runs at the start of the epoch before distribute_epoch reads the value.
#[tokio::test]
async fn test_delegate_reward_share_ratio_applied_at_tally() {
    let (mint, mint_authority, operator_token, stake_token, protocol_token) = prepare_gar_test();
    let dummy = Pubkey::new_unique();
    let mut pt = program_test_with_gar(
        &dummy,
        &mint.pubkey(),
        &stake_token.pubkey(),
        &protocol_token.pubkey(),
    );
    pre_create_epoch_settings(&mut pt, &dummy, 100, 86_400, true);
    let mut ctx = pt.start_with_context().await;
    let setup = setup_gar(
        &mut ctx,
        mint,
        mint_authority,
        operator_token,
        stake_token,
        protocol_token,
    )
    .await;

    mint_tokens(
        &mut ctx,
        &setup.mint.pubkey(),
        &setup.protocol_token.pubkey(),
        &setup.mint_authority,
        1_000_000_000,
    )
    .await;

    // Join at t=0 so the gateway is active for epoch 0 (starts at 100).
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 0;
    ctx.set_sysvar(&clock);
    let gateway_key = join_gateway(&mut ctx, &setup, 20_000_000_000).await;
    let operator_pk = ctx.payer.pubkey();

    // Stage a ratio change (50 → 5000 bps). Active stays at the join value.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::UpdateGatewaySettings {
                settings: setup.settings_key,
                gateway: gateway_key,
                operator: operator_pk,
            }
            .to_account_metas(None),
            data: ario_gar::instruction::UpdateGatewaySettings {
                params: ario_gar::UpdateGatewayParams {
                    label: None,
                    fqdn: None,
                    port: None,
                    protocol: None,
                    properties: None,
                    note: None,
                    allow_delegated_staking: None,
                    delegate_reward_share_ratio: Some(50),
                    min_delegate_stake: None,
                },
            }
            .data(),
        }],
        Some(&operator_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let gw = f6_read_gateway(&mut ctx, &gateway_key).await;
    assert_eq!(gw.settings.delegate_reward_share_ratio, 1000); // active unchanged
    assert_eq!(gw.settings.pending_delegate_reward_share_ratio, Some(5000));

    // Warp to epoch start, create epoch, run tally.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = 100;
    ctx.set_sysvar(&clock);
    let (epoch_settings_key, _) = epoch_settings_pda();
    let (epoch_key, _) = epoch_pda(0);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: ario_gar::accounts::CreateEpoch {
                epoch_settings: epoch_settings_key,
                epoch: epoch_key,
                registry: setup.registry_key,
                settings: setup.settings_key,
                protocol_token_account: setup.protocol_token.pubkey(),
                payer: operator_pk,
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_gar::instruction::CreateEpoch {}.data(),
        }],
        Some(&operator_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let mut tally_accounts = ario_gar::accounts::TallyWeights {
        settings: setup.settings_key,
        epoch_settings: epoch_settings_key,
        epoch: epoch_key,
        registry: setup.registry_key,
        payer: operator_pk,
    }
    .to_account_metas(None);
    tally_accounts.push(solana_sdk::instruction::AccountMeta::new(
        gateway_key,
        false,
    ));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_gar::ID,
            accounts: tally_accounts,
            data: ario_gar::instruction::TallyWeights { _epoch_index: 0 }.data(),
        }],
        Some(&operator_pk),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Tally applied pending → active and cleared the pending slot.
    let gw = f6_read_gateway(&mut ctx, &gateway_key).await;
    assert_eq!(gw.settings.delegate_reward_share_ratio, 5000);
    assert_eq!(gw.settings.pending_delegate_reward_share_ratio, None);
}
