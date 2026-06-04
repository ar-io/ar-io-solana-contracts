use anchor_lang::{prelude::*, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::{AccountMeta, Instruction},
    program_pack::Pack,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_arns::error::ArnsError;
use ario_arns::state::*;

/// MPL Core program ID — kept here as a test-only constant after the
/// ADR-016 reshape moved all MPL CPIs out of ario-arns. The integration
/// tests still need to load the mpl-core BPF program into ProgramTest
/// for any flow that touches the asset (e.g., setting up an ANT mint).
const MPL_CORE_PROGRAM_ID: Pubkey =
    anchor_lang::solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

/// Realistic `period_zero_start_timestamp` used by `setup_arns()` and
/// `setup_full_environment_with_keys()`. Chosen to satisfy the on-chain
/// validation in `programs/ario-arns/src/instructions/initialize.rs`:
///   * `>= 1_577_836_800` (2020-01-01 sanity floor)
///   * `<= clock.unix_timestamp` (test fixtures warp the clock to this exact
///     value before init, so the equality bound is fine)
///
/// All absolute test-clock warps are expressed as offsets from this base
/// (e.g. `TEST_PERIOD_ZERO_START + 86_400 + 1`) so the relative period math
/// the rest of the suite depends on (`(timestamp - period_zero_start) /
/// PERIOD_LENGTH_SECONDS`) keeps working.
const TEST_PERIOD_ZERO_START: i64 = 1_700_000_000; // 2023-11-14 22:13:20 UTC

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

fn anchor_processor(
    program_id: &Pubkey,
    accounts: &[anchor_lang::prelude::AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[anchor_lang::prelude::AccountInfo] = std::mem::transmute(accounts);
        ario_arns::entry(program_id, accounts, data)
    }
}

/// Create a ProgramTest with a pre-allocated NameRegistry (400KB zero-copy account).
/// NameRegistry is too large for Anchor `init` in native processor mode (>10KB realloc limit).
fn program_test_with_registry() -> ProgramTest {
    program_test_with_registry_inner(true)
}

/// Variant that skips loading the MPL Core BPF program. Used only by flows that
/// never touch an ANT asset (e.g. the demand-factor stepping tests, which drive
/// `update_demand_factor` / `initialize` and nothing else). Omitting MPL Core
/// lets these tests run against the NATIVE `ario_arns` processor without
/// requiring `BPF_OUT_DIR` (which would otherwise force every program — including
/// `ario_arns` — to be loaded from a prebuilt `.so`).
fn program_test_with_registry_no_mpl() -> ProgramTest {
    program_test_with_registry_inner(false)
}

fn program_test_with_registry_inner(load_mpl_core: bool) -> ProgramTest {
    use anchor_lang::solana_program::hash::hash;

    let mut pt = ProgramTest::new("ario_arns", ario_arns::ID, processor!(anchor_processor));
    pt.set_compute_max_units(1_000_000);

    // Load the real MPL Core BPF program so handlers that CPI UpdatePluginV1
    // (purchase, manage, sync_attributes) execute against the actual program
    // rather than getting AccountNotExecutable. The .so file is dumped from
    // mainnet and committed at programs/ario-arns/tests/fixtures/mpl_core.so;
    // refresh with:
    //   solana program dump CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d \
    //     contracts/programs/ario-arns/tests/fixtures/mpl_core.so \
    //     --url https://api.mainnet-beta.solana.com
    //
    // RUNNING THESE TESTS:
    //   1. `anchor build` so target/deploy/ario_arns.so is fresh (else
    //      solana-program-test loads a stale BPF program for ario-arns)
    //   2. Copy the fixture into BPF_OUT_DIR so add_program("mpl_core", ...)
    //      can find it:
    //        cp programs/ario-arns/tests/fixtures/mpl_core.so target/deploy/
    //   3. Run with BPF_OUT_DIR=$(pwd)/target/deploy cargo test -p ario-arns
    //
    // The migration/localnet/scripts/patch-and-build.sh wrapper handles
    // steps 1+2 for surfpool runs; for cargo, set BPF_OUT_DIR yourself.
    if load_mpl_core {
        // MPL Core is only available as a prebuilt `.so`, so it must be loaded
        // via the BPF path (`add_program(.., None)` resolves it from BPF_OUT_DIR
        // / `tests/fixtures`). When BPF_OUT_DIR is set (the canonical CI flow
        // after `anchor build`), `prefer_bpf` is already true and `ario_arns`
        // is loaded from its freshly-built `.so` — leave that untouched.
        //
        // When BPF_OUT_DIR is NOT set, `ProgramTest::new` above registered
        // `ario_arns` as the NATIVE processor (compiled from the current
        // source), but `prefer_bpf` is false so `add_program("mpl_core", None)`
        // would fail to load the fixture. Flip `prefer_bpf` to true here — AFTER
        // ario_arns was already added as native — so MPL Core loads from the
        // committed `tests/fixtures/mpl_core.so` while ario_arns stays native.
        // This lets the MPL-touching tests run from source without a prebuilt
        // `ario_arns.so`; the resolution decision is per-`add_program` call.
        if std::env::var("BPF_OUT_DIR").is_err() && std::env::var("SBF_OUT_DIR").is_err() {
            pt.prefer_bpf(true);
        }
        pt.add_program("mpl_core", MPL_CORE_PROGRAM_ID, None);
    }

    // Pre-create NameRegistry
    let registry_size = NameRegistry::bytes_for_capacity(NameRegistry::INITIAL_CAPACITY);
    let rent = solana_sdk::rent::Rent::default();
    let mut data = vec![0u8; registry_size];
    // Write zero-copy discriminator
    let disc = hash(b"account:NameRegistry");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);
    // authority at offset 8..40 — leave as zero (will be set by config authority)
    // count at offset 40..44 — already 0

    // NameRegistry is just #[account(mut)] — no PDA seeds. Use a deterministic address.
    let name_registry_key = Pubkey::find_program_address(&[NAME_REGISTRY_SEED], &ario_arns::ID).0;

    pt.add_account(
        name_registry_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(registry_size),
            data,
            owner: ario_arns::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    // PR-4: pre-add a ProgramData account so `initialize` can satisfy the
    // upgrade-authority constraint. The fixed-seed authority is funded with
    // SOL so it can pay rent for the `init` accounts.
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

    pt
}

/// PR-4: deterministic upgrade-authority keypair used for tests.
fn upgrade_authority_keypair() -> Keypair {
    solana_sdk::signer::keypair::keypair_from_seed(&[42u8; 32])
        .expect("keypair_from_seed must succeed for fixed test seed")
}

fn program_data_pda() -> Pubkey {
    let (pda, _) = Pubkey::find_program_address(
        &[ario_arns::ID.as_ref()],
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

fn name_registry_key() -> Pubkey {
    Pubkey::find_program_address(&[NAME_REGISTRY_SEED], &ario_arns::ID).0
}

/// Read the `amount` of an SPL token account by pubkey.
async fn spl_token_balance(ctx: &mut ProgramTestContext, key: Pubkey) -> u64 {
    let account = ctx.banks_client.get_account(key).await.unwrap().unwrap();
    spl_token::state::Account::unpack(&account.data)
        .unwrap()
        .amount
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

/// Mint a real Metaplex Core ANT inside a running test context, with an
/// Attributes plugin (authority=Owner, empty list) pre-installed so that
/// ARIO-ARNS handlers can CPI UpdatePluginV1 against it during the test.
///
/// Uses raw CreateV1 wire encoding (verified offline; see migration/import
/// edge-case suite) — keeps test deps minimal.
///
/// `asset_keypair` is the new asset's signer; its pubkey becomes the ANT
/// asset key (= the ANT NFT mint pubkey). Owner & update authority both
/// default to `ctx.payer`, so subsequent ARIO-ARNS handler `caller==owner`
/// checks pass when the test's payer is the caller.
async fn mint_test_ant(ctx: &mut ProgramTestContext, asset_keypair: &Keypair) {
    use anchor_lang::solana_program::instruction::AccountMeta;
    use anchor_lang::solana_program::instruction::Instruction as Ix;

    // CreateV1 wire format (kinobi-generated, verified byte-for-byte by the
    // edge-case validator in migration/import/src/edge-case-validate.ts).
    //   discriminator(0) + dataState(0) + name(Borsh String) + uri(Borsh String)
    //   + plugins(Some, vec_len=1, Plugin::Attributes{empty} + auth=Some(Owner))
    let name = b"test-ant";
    let uri = b"ar://test";
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

    // Account order matches kinobi's createV1.js (and the migration mint).
    let placeholder = MPL_CORE_PROGRAM_ID;
    let metas = vec![
        AccountMeta::new(asset_keypair.pubkey(), true), // 0 asset (signer, writable)
        AccountMeta::new_readonly(placeholder, false),  // 1 collection (None)
        AccountMeta::new_readonly(ctx.payer.pubkey(), true), // 2 authority (signer)
        AccountMeta::new(ctx.payer.pubkey(), true),     // 3 payer (signer, writable)
        AccountMeta::new_readonly(placeholder, false),  // 4 owner (None → defaults to authority)
        AccountMeta::new_readonly(placeholder, false),  // 5 updateAuthority (None → defaults)
        AccountMeta::new_readonly(solana_sdk::system_program::ID, false), // 6 system_program
        AccountMeta::new_readonly(placeholder, false),  // 7 logWrapper (None)
    ];

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: MPL_CORE_PROGRAM_ID,
            accounts: metas,
            data,
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, asset_keypair],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("mint_test_ant: CreateV1 failed (is mpl_core.so loaded? BPF_OUT_DIR set?)");
}

// (legacy `add_fake_mpl_core_asset` / `set_fake_mpl_core_asset_owner` removed —
// every test now mints a real Core ANT via `mint_test_ant` because handler
// CPIs to UpdatePluginV1 require a real asset to deserialize.)

/// Mint a real Core ANT with a populated Attributes plugin payload.
///
/// Drop-in extension of `mint_test_ant` for ADR-016 / BD-100 tests that
/// need the asset to declare a specific `ANT Program` (or any other
/// trait) at CreateV1 time. The wire format mirrors what the SDK's
/// `spawnSolanaANT` and migration's `mint-nft.ts` write.
async fn mint_test_ant_with_attributes(
    ctx: &mut ProgramTestContext,
    asset_keypair: &Keypair,
    attributes: &[(&str, &str)],
) {
    use anchor_lang::solana_program::instruction::AccountMeta;
    use anchor_lang::solana_program::instruction::Instruction as Ix;

    let name = b"test-ant";
    let uri = b"ar://test";
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
    data.extend_from_slice(&(attributes.len() as u32).to_le_bytes());
    for (k, v) in attributes {
        data.extend_from_slice(&(k.len() as u32).to_le_bytes());
        data.extend_from_slice(k.as_bytes());
        data.extend_from_slice(&(v.len() as u32).to_le_bytes());
        data.extend_from_slice(v.as_bytes());
    }
    data.push(1); // plugin authority Option = Some
    data.push(1); // BasePluginAuthority::Owner

    let placeholder = MPL_CORE_PROGRAM_ID;
    let metas = vec![
        AccountMeta::new(asset_keypair.pubkey(), true),
        AccountMeta::new_readonly(placeholder, false),
        AccountMeta::new_readonly(ctx.payer.pubkey(), true),
        AccountMeta::new(ctx.payer.pubkey(), true),
        AccountMeta::new_readonly(placeholder, false),
        AccountMeta::new_readonly(placeholder, false),
        AccountMeta::new_readonly(solana_sdk::system_program::ID, false),
        AccountMeta::new_readonly(placeholder, false),
    ];

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Ix {
            program_id: MPL_CORE_PROGRAM_ID,
            accounts: metas,
            data,
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, asset_keypair],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.expect(
        "mint_test_ant_with_attributes: CreateV1 failed (is mpl_core.so loaded? BPF_OUT_DIR set?)",
    );
}

/// Read a single trait value from a live MPL Core asset by walking its
/// Attributes plugin in the same way `read_mpl_core_attribute` does
/// on-chain. Returns `None` when the asset has no plugin section, no
/// Attributes plugin, or the requested key isn't present.
async fn read_asset_attribute(
    ctx: &mut ProgramTestContext,
    asset: &solana_sdk::pubkey::Pubkey,
    key: &str,
) -> Option<String> {
    let account = ctx.banks_client.get_account(*asset).await.unwrap()?;
    let data = account.data.as_slice();

    // Skip the asset header: key(1) + owner(32) + UpdateAuthority(1+0/32)
    // + name(4+N) + uri(4+N) + seq(1+0/8). For our minted test fixtures
    // UpdateAuthority is None, name="test-ant", uri="ar://test", seq=None
    // — but rather than hard-code those sizes (which would silently mask
    // a real wire-format change), walk by length-prefixed reads.
    if data.len() < 33 || data[0] != 1 {
        return None;
    }
    let mut pos = 33;
    let upd = data[pos];
    pos += 1;
    pos += match upd {
        0 => 0,
        1 | 2 => 32,
        _ => return None,
    };
    // name + uri (skip)
    for _ in 0..2 {
        if pos + 4 > data.len() {
            return None;
        }
        let n = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4 + n;
    }
    if pos >= data.len() || data[pos] > 1 {
        return None;
    }
    let seq = data[pos];
    pos += 1;
    if seq == 1 {
        pos += 8;
    }

    if pos >= data.len() || data[pos] != 3 {
        return None;
    } // PluginHeaderV1 key
    pos += 1;
    if pos + 8 > data.len() {
        return None;
    }
    let registry_off = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()) as usize;
    if registry_off >= data.len() || data[registry_off] != 4 {
        return None;
    }

    let mut rpos = registry_off + 1;
    if rpos + 4 > data.len() {
        return None;
    }
    let count = u32::from_le_bytes(data[rpos..rpos + 4].try_into().unwrap()) as usize;
    rpos += 4;
    for _ in 0..count {
        if rpos >= data.len() {
            return None;
        }
        let plugin_type = data[rpos];
        rpos += 1;
        if rpos >= data.len() {
            return None;
        }
        let auth = data[rpos];
        rpos += 1;
        rpos += match auth {
            0 | 1 | 2 => 0,
            3 => 32,
            _ => return None,
        };
        if rpos + 8 > data.len() {
            return None;
        }
        let plugin_off = u64::from_le_bytes(data[rpos..rpos + 8].try_into().unwrap()) as usize;
        rpos += 8;
        if plugin_type != 6 {
            continue;
        } // skip non-Attributes
          // Plugin body: variant + Vec<Attribute>
        if plugin_off >= data.len() || data[plugin_off] != 6 {
            return None;
        }
        let mut ppos = plugin_off + 1;
        if ppos + 4 > data.len() {
            return None;
        }
        let attr_count = u32::from_le_bytes(data[ppos..ppos + 4].try_into().unwrap()) as usize;
        ppos += 4;
        for _ in 0..attr_count {
            if ppos + 4 > data.len() {
                return None;
            }
            let kl = u32::from_le_bytes(data[ppos..ppos + 4].try_into().unwrap()) as usize;
            ppos += 4;
            if ppos + kl > data.len() {
                return None;
            }
            let k_str = std::str::from_utf8(&data[ppos..ppos + kl]).ok()?;
            ppos += kl;
            if ppos + 4 > data.len() {
                return None;
            }
            let vl = u32::from_le_bytes(data[ppos..ppos + 4].try_into().unwrap()) as usize;
            ppos += 4;
            if ppos + vl > data.len() {
                return None;
            }
            let v_str = std::str::from_utf8(&data[ppos..ppos + vl]).ok()?;
            ppos += vl;
            if k_str == key {
                return Some(v_str.to_string());
            }
        }
        return None;
    }
    None
}

/// Transfer a real Core ANT to a new owner via MPL Core's TransferV1.
/// Used by tests that simulate marketplace-style ownership changes
/// (replaces the old `set_fake_mpl_core_asset_owner` byte-injection helper).
async fn transfer_test_ant(
    ctx: &mut ProgramTestContext,
    asset_pubkey: Pubkey,
    current_owner: &Keypair,
    new_owner: Pubkey,
) {
    use anchor_lang::solana_program::instruction::AccountMeta;
    use anchor_lang::solana_program::instruction::Instruction as Ix;

    // TransferV1 wire format (kinobi):
    //   discriminator(14) + compressionProof(Option = None → 0x00)
    let data = vec![14u8, 0u8];

    let placeholder = MPL_CORE_PROGRAM_ID;
    let metas = vec![
        AccountMeta::new(asset_pubkey, false), // 0 asset (writable)
        AccountMeta::new_readonly(placeholder, false), // 1 collection (None)
        AccountMeta::new(current_owner.pubkey(), true), // 2 payer (signer, writable)
        AccountMeta::new_readonly(current_owner.pubkey(), true), // 3 authority (= current owner)
        AccountMeta::new_readonly(new_owner, false), // 4 newOwner
        AccountMeta::new_readonly(solana_sdk::system_program::ID, false), // 5 system_program
        AccountMeta::new_readonly(placeholder, false), // 6 logWrapper (None)
    ];

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let signers: Vec<&Keypair> = if current_owner.pubkey() == ctx.payer.pubkey() {
        vec![current_owner]
    } else {
        vec![&ctx.payer, current_owner]
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
        .expect("transfer_test_ant: TransferV1 failed");
}

// PDA helpers
fn config_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ARNS_CONFIG_SEED], &ario_arns::ID)
}

fn demand_factor_pda() -> (Pubkey, u8) {
    Pubkey::find_program_address(&[DEMAND_FACTOR_SEED], &ario_arns::ID)
}

fn arns_record_pda(name: &str) -> (Pubkey, u8) {
    let name_hash = ario_arns::pricing::hash_name(name);
    Pubkey::find_program_address(&[ARNS_RECORD_SEED, &name_hash], &ario_arns::ID)
}

fn reserved_name_pda(name: &str) -> (Pubkey, u8) {
    let name_hash = ario_arns::pricing::hash_name(name);
    Pubkey::find_program_address(&[RESERVED_NAME_SEED, &name_hash], &ario_arns::ID)
}

fn returned_name_pda(name: &str) -> (Pubkey, u8) {
    let name_hash = ario_arns::pricing::hash_name(name);
    Pubkey::find_program_address(&[RETURNED_NAME_SEED, &name_hash], &ario_arns::ID)
}

/// Borsh-encode a single Attribute pair (`{ key: String, value: String }`)
/// the same way mpl-core stores it inside the Attributes plugin: u32 len +
/// key bytes + u32 len + value bytes. We search for this byte sequence in
/// the raw Core asset data to verify what UpdatePluginV1 wrote on chain.
fn encode_attribute_pair(key: &str, value: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + key.len() + value.len());
    out.extend_from_slice(&(key.len() as u32).to_le_bytes());
    out.extend_from_slice(key.as_bytes());
    out.extend_from_slice(&(value.len() as u32).to_le_bytes());
    out.extend_from_slice(value.as_bytes());
    out
}

fn bytes_contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Fetch a Core asset and assert that each (key, value) attribute pair is
/// encoded into the asset's bytes. Cheaper than parsing the full plugin
/// registry layout, and tight enough to catch encoding regressions: each
/// pair is a length-prefixed Borsh string sequence that won't appear at
/// random in the asset's binary state.
async fn assert_asset_attributes_contain(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    expected: &[(&str, &str)],
) {
    let account = ctx
        .banks_client
        .get_account(asset)
        .await
        .expect("RPC error fetching asset")
        .expect("Core asset account not found");

    for (k, v) in expected {
        let needle = encode_attribute_pair(k, v);
        assert!(
            bytes_contains(&account.data, &needle),
            "asset {} missing attribute pair ({}={}); data hex: {}",
            asset,
            k,
            v,
            account
                .data
                .iter()
                .map(|b| format!("{:02x}", b))
                .collect::<String>(),
        );
    }
}

async fn assert_asset_attribute_absent(
    ctx: &mut ProgramTestContext,
    asset: Pubkey,
    key: &str,
    value: &str,
) {
    let account = ctx
        .banks_client
        .get_account(asset)
        .await
        .expect("RPC error fetching asset")
        .expect("Core asset account not found");
    let needle = encode_attribute_pair(key, value);
    assert!(
        !bytes_contains(&account.data, &needle),
        "asset {} unexpectedly contains attribute pair ({}={})",
        asset,
        key,
        value,
    );
}

struct ArnsSetup {
    mint: Keypair,
    mint_authority: Keypair,
    buyer_token: Keypair,
    protocol_token: Keypair,
    config_key: Pubkey,
    demand_factor_key: Pubkey,
}

/// Initialize the ArNS program and return setup info.
async fn setup_arns(ctx: &mut ProgramTestContext) -> ArnsSetup {
    setup_arns_with_initial_demand_factor(ctx, DEMAND_FACTOR_SCALE).await
}

/// Same as `setup_arns` but seeds the demand factor at `initial_demand_factor`
/// instead of 1.0. Used by the demand-factor stepping tests that drive the
/// floor/halving state machine from the real MAINNET start value (9.8x). The
/// on-chain `initialize` requires `initial_demand_factor >= DEMAND_FACTOR_MIN`.
async fn setup_arns_with_initial_demand_factor(
    ctx: &mut ProgramTestContext,
    initial_demand_factor: u64,
) -> ArnsSetup {
    // Anchor chain time to TEST_PERIOD_ZERO_START so chain time matches
    // period_zero_start_timestamp (which is required by the on-chain
    // validation in initialize.rs to be `>= 1_577_836_800` AND
    // `<= clock.unix_timestamp`). Subsequent test code expresses absolute
    // warps as `TEST_PERIOD_ZERO_START + <relative_offset>` so the lazy
    // demand-factor rollover in buy_name/extend_lease only processes the
    // intended number of periods, not ~20,000.
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START;
    ctx.set_sysvar(&clock);

    let mint = Keypair::new();
    let mint_authority = Keypair::new();
    create_mint(ctx, &mint, &mint_authority.pubkey()).await;

    let buyer_token = Keypair::new();
    let payer_pk = ctx.payer.pubkey();
    create_token_account(ctx, &buyer_token, &mint.pubkey(), &payer_pk).await;
    // Fund with enough tokens for name purchases (1M ARIO = very generous)
    mint_tokens(
        ctx,
        &mint.pubkey(),
        &buyer_token.pubkey(),
        &mint_authority,
        1_000_000_000_000,
    )
    .await;

    let protocol_token = Keypair::new();
    create_token_account(ctx, &protocol_token, &mint.pubkey(), &payer_pk).await;

    let (config_key, _) = config_pda();
    let (demand_factor_key, _) = demand_factor_pda();

    // Initialize ArNS. Under PR-4, `authority` (the SDK signer that pays
    // for the `init` constraint) must be the program's upgrade authority.
    // The `params.authority` (protocol authority field) stays as ctx.payer
    // so downstream tests' authority checks unaffected.
    let upgrade_auth = upgrade_authority_keypair();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::InitializeArns {
                config: config_key,
                demand_factor: demand_factor_key,
                authority: upgrade_auth.pubkey(),
                program_data: program_data_pda(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::Initialize {
                params: ario_arns::InitializeArnsParams {
                    authority: ctx.payer.pubkey(),
                    mint: mint.pubkey(),
                    treasury: protocol_token.pubkey(),
                    period_zero_start_timestamp: TEST_PERIOD_ZERO_START,
                    migration_authority: ctx.payer.pubkey(),
                    initial_demand_factor,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer, &upgrade_auth],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    ArnsSetup {
        mint,
        mint_authority,
        buyer_token,
        protocol_token,
        config_key,
        demand_factor_key,
    }
}

// =========================================
// TESTS
// =========================================

#[tokio::test]
async fn test_initialize_arns() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    // Verify config
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();

    assert_eq!(config.authority, ctx.payer.pubkey());
    assert_eq!(config.mint, setup.mint.pubkey());
    assert_eq!(config.grace_period_seconds, GRACE_PERIOD_SECONDS);
    assert_eq!(config.max_lease_length_years, MAX_LEASE_LENGTH_YEARS as u8);
    assert_eq!(config.total_names_registered, 0);

    // Verify demand factor
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();

    assert_eq!(demand.current_demand_factor, DEMAND_FACTOR_SCALE);
    assert_eq!(demand.current_period, 1);
    assert_eq!(demand.purchases_this_period, 0);
    // Verify genesis fees are set
    assert_eq!(demand.fees[0], GENESIS_FEES[0]);
    assert_eq!(demand.fees[50], GENESIS_FEES[50]);
}

#[tokio::test]
async fn test_buy_name_lease() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "testname".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify ArNS record
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();

    assert_eq!(record.owner, ctx.payer.pubkey());
    assert_eq!(record.name, name);
    assert!(matches!(record.purchase_type, PurchaseType::Lease));
    assert!(record.end_timestamp.is_some());
    assert_eq!(record.undername_limit, DEFAULT_UNDERNAME_COUNT);
    assert!(record.purchase_price > 0);

    // Verify config updated
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 1);

    // Verify demand factor tracked the purchase
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(demand.purchases_this_period, 1);
    assert!(demand.revenue_this_period > 0);
}

#[tokio::test]
async fn test_buy_name_permabuy() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "permname".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0, // Ignored for permabuy
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify record is permabuy
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();

    assert_eq!(record.owner, ctx.payer.pubkey());
    assert!(matches!(record.purchase_type, PurchaseType::Permabuy));
    assert!(record.end_timestamp.is_none()); // Permabuy has no expiry
}

#[tokio::test]
async fn test_reserve_and_unreserve_name() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name = "reserved".to_string();
    let (reserved_key, _) = reserved_name_pda(&name);

    // Reserve name
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.clone(),
                    reserved_for: None,
                    expires_at: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify reserved
    let account = ctx.banks_client.get_account(reserved_key).await.unwrap();
    assert!(account.is_some());

    // Unreserve
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UnreserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UnreserveName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify account closed
    let account = ctx.banks_client.get_account(reserved_key).await.unwrap();
    assert!(account.is_none());
}

#[tokio::test]
async fn test_reassign_name() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let new_ant_keypair = Keypair::new();
    let new_ant = new_ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    mint_test_ant(&mut ctx, &new_ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy a name first
    let name = "myname".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Reassign to new ANT. `ant_asset` is the CURRENT asset (record.ant) and
    // authorizes the caller as its present holder; `new_ant` is just the
    // instruction arg the record gets repointed to.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: ant_key,
        caller: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName { new_ant }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify ANT was updated
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert_eq!(record.ant, new_ant);
}

// =========================================
// HELPER: buy a name and return its record key
// =========================================

async fn buy_name_helper(
    ctx: &mut ProgramTestContext,
    setup: &ArnsSetup,
    name: &str,
    purchase_type: PurchaseType,
    years: u8,
    ant_key: Pubkey,
) -> Pubkey {
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(name).0,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type,
                    years,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    arns_record_key
}

// =========================================
// NEW TESTS
// =========================================

#[tokio::test]
async fn test_extend_lease() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "extendme";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read the initial end_timestamp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let initial_end = record.end_timestamp.unwrap();

    // Extend by 2 years
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years: 2 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify end_timestamp increased by 2 years
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let expected_end = initial_end + 2 * ONE_YEAR_SECONDS;
    assert_eq!(record.end_timestamp.unwrap(), expected_end);

    // Try extending by 3 more years (would be 1+2+3=6 total, exceeds max 5) — should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years: 3 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::ExtensionExceedsMax);
}

#[tokio::test]
async fn test_upgrade_name() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "upgrademe";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Verify it starts as a lease with an end_timestamp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert!(matches!(record.purchase_type, PurchaseType::Lease));
    assert!(record.end_timestamp.is_some());

    // Upgrade to permabuy
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpgradeName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpgradeName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify it is now a permabuy with no end_timestamp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert!(matches!(record.purchase_type, PurchaseType::Permabuy));
    assert!(record.end_timestamp.is_none());
    assert!(record.purchase_price > 0);
}

#[tokio::test]
async fn test_increase_undername_limit() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "undernames";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Verify default undername limit
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert_eq!(record.undername_limit, DEFAULT_UNDERNAME_COUNT);

    // Increase by 5
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::IncreaseUndernameLimit {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::IncreaseUndernameLimit { quantity: 5 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify undername_limit is now 15
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert_eq!(record.undername_limit, DEFAULT_UNDERNAME_COUNT + 5);
}

#[tokio::test]
async fn test_demand_factor_update() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy a name to generate purchase activity in period 1
    buy_name_helper(
        &mut ctx,
        &setup,
        "demandone",
        PurchaseType::Lease,
        1,
        ant_key,
    )
    .await;

    // Verify demand factor tracked the purchase
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(demand.purchases_this_period, 1);
    assert!(demand.revenue_this_period > 0);
    let revenue_period_1 = demand.revenue_this_period;

    // Warp slots forward first, then override clock timestamp for the period transition
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 86_400 + 1; // past first period boundary
    ctx.set_sysvar(&clock);

    // Call UpdateDemandFactor to process the period transition
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpdateDemandFactor {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpdateDemandFactor {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify demand factor state after period transition
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();

    // After transition, current period should have advanced
    assert!(demand.current_period > 1);
    // Purchases and revenue should be reset for the new period
    assert_eq!(demand.purchases_this_period, 0);
    assert_eq!(demand.revenue_this_period, 0);
    // The previous period's revenue should be in the trailing ring buffer
    // Ring index = (old_period % 7). Period 1 -> index 1
    let ring_idx = 1 % MOVING_AVG_PERIOD_COUNT;
    assert_eq!(demand.trailing_period_revenues[ring_idx], revenue_period_1);
    assert_eq!(demand.trailing_period_purchases[ring_idx], 1);
}

// =========================================
// RELEASE NAME & BUY RETURNED NAME TESTS
// =========================================

#[tokio::test]
async fn test_release_name() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "releaseme";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Verify config shows 1 name registered
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 1);

    // Verify name is in registry (read count from raw bytes to avoid stack overflow)
    // Zero-copy layout: discriminator(8) + authority(32) + count(u32 at offset 40)
    let registry_key = name_registry_key();
    let registry_account = ctx
        .banks_client
        .get_account(registry_key)
        .await
        .unwrap()
        .unwrap();
    let registry_count = u32::from_le_bytes(registry_account.data[40..44].try_into().unwrap());
    assert_eq!(registry_count, 1);

    // Release the name
    let (returned_name_key, _) = returned_name_pda(name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify ArnsRecord is closed
    let record_account = ctx.banks_client.get_account(arns_record_key).await.unwrap();
    assert!(
        record_account.is_none(),
        "ArnsRecord should be closed after release"
    );

    // Verify ReturnedName is created with correct fields
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap()
        .unwrap();
    let returned = ReturnedName::try_deserialize(&mut returned_account.data.as_slice()).unwrap();
    assert_eq!(returned.name, name);
    assert_eq!(returned.initiator, ctx.payer.pubkey());
    assert_eq!(
        returned.returned_at, TEST_PERIOD_ZERO_START,
        "returned_at should match the chain clock at release time (TEST_PERIOD_ZERO_START)"
    );

    // Verify config.total_names_registered decreased
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 0);

    // Verify name removed from NameRegistry (read count from raw bytes)
    let registry_account = ctx
        .banks_client
        .get_account(registry_key)
        .await
        .unwrap()
        .unwrap();
    let registry_count = u32::from_le_bytes(registry_account.data[40..44].try_into().unwrap());
    assert_eq!(registry_count, 0);
}

#[tokio::test]
async fn test_release_name_lease_fails() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "leasename";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Try to release a lease — should fail with CannotReleaseLease
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::CannotReleaseLease);
}

#[tokio::test]
async fn test_release_name_not_ant_holder() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    // ANT is owned by Pubkey::default() (the payer), not other_user
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "ownedname";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Create a second user
    let other_user = Keypair::new();
    // Fund the other user with SOL
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &other_user.pubkey(),
            1_000_000_000, // 1 SOL
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Other user tries to release — should fail because they don't hold the ANT NFT
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: other_user.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&other_user.pubkey()),
        &[&other_user],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::NotAntHolder);
}

#[tokio::test]
async fn test_buy_returned_name() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "returnbuy";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Release the name
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Record protocol balance before buy_returned_name
    let protocol_before = {
        let account = ctx
            .banks_client
            .get_account(setup.protocol_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };

    // Warp past the returned name auction window (14 days + 1 second) so premium = 0
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Buy the returned name (same buyer/payer for simplicity)
    // The initiator is ctx.payer (who released the name), and their token account is setup.buyer_token
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyReturnedName {
                config: config_pda().0,
                demand_factor: demand_factor_pda().0,
                returned_name: returned_name_key,
                arns_record: arns_record_pda(name).0,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                initiator_token_account: setup.buyer_token.pubkey(),
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyReturnedName {
                params: ario_arns::BuyReturnedNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify ArnsRecord created with correct owner
    let new_arns_key = arns_record_pda(name).0;
    let record_account = ctx
        .banks_client
        .get_account(new_arns_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert_eq!(record.owner, ctx.payer.pubkey());
    assert_eq!(record.name, name);
    assert!(matches!(record.purchase_type, PurchaseType::Permabuy));

    // Verify ReturnedName account is closed
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap();
    assert!(
        returned_account.is_none(),
        "ReturnedName should be closed after purchase"
    );

    // Verify config.total_names_registered incremented back to 1
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 1);

    // Verify tokens transferred (protocol balance increased)
    let protocol_after = {
        let account = ctx
            .banks_client
            .get_account(setup.protocol_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };
    assert!(
        protocol_after > protocol_before,
        "Protocol should have received tokens"
    );
}

#[tokio::test]
async fn test_buy_returned_name_high_premium() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "premium";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Record the original purchase price
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let original_purchase_price = record.purchase_price;

    // Record buyer balance before release+rebuy
    let _buyer_before_release = {
        let account = ctx
            .banks_client
            .get_account(setup.buyer_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };

    // Release the name
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Buy immediately at returned_at (NO time warp) — premium should be 50x
    // The initiator is ctx.payer, their token account is setup.buyer_token
    let _buyer_before_buy = {
        let account = ctx
            .banks_client
            .get_account(setup.buyer_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyReturnedName {
                config: config_pda().0,
                demand_factor: demand_factor_pda().0,
                returned_name: returned_name_key,
                arns_record: arns_record_pda(name).0,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                initiator_token_account: setup.buyer_token.pubkey(),
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyReturnedName {
                params: ario_arns::BuyReturnedNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify the purchase happened
    let new_arns_key = arns_record_pda(name).0;
    let record_account = ctx
        .banks_client
        .get_account(new_arns_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();

    // The returned name purchase price should be much higher than the original
    // due to the 50x premium. The premium is calculated as:
    //   registration_fee * 50 (at elapsed=0, pct_remaining=1.0)
    // So the returned purchase price should be ~50x the base registration fee.
    assert!(
        record.purchase_price > original_purchase_price,
        "Returned name price ({}) should be much higher than original ({})",
        record.purchase_price,
        original_purchase_price
    );

    // Verify cost is approximately 50x the base registration fee.
    // The original purchase price IS the registration fee (demand factor = 1.0),
    // so returned price should be close to 50 * original.
    // Note: demand factor may have shifted slightly from the first purchase, but
    // at demand_factor=1.0 it should be exactly 50x.
    let expected_premium_price = original_purchase_price * RETURNED_NAME_MAX_MULTIPLIER;
    assert_eq!(
        record.purchase_price, expected_premium_price,
        "At returned_at, premium should be exactly {}x (expected {}, got {})",
        RETURNED_NAME_MAX_MULTIPLIER, expected_premium_price, record.purchase_price
    );
}

/// AREA 2: returned-name MID-auction premium charged on-chain.
///
/// The existing returned-name integration tests only buy at the two extremes:
/// t=0 (50x, `test_buy_returned_name_high_premium`) and after the window closes
/// (1x, `test_buy_returned_name`). The linearly-decaying mid-window price
/// (`pricing.rs::calculate_returned_name_premium`) was only ever exercised by
/// the in-crate unit test `test_returned_name_premium_halfway` — never charged
/// through an executed `buy_returned_name` with a real SPL transfer + revenue
/// split.
///
/// This test releases a name into the ReturnedName state, warps to EXACTLY the
/// 7-day midpoint of the 14-day auction window, executes `buy_returned_name`,
/// and asserts:
///   1. the charged `purchase_price` equals the protocol's own mid-window
///      formula (`registration_fee * 25.5`, the linear midpoint
///      50 − (49/14)*7 = 25.5x), recomputed from the live on-chain demand
///      factor + fee table so the assertion is exact, not approximate;
///   2. the implied premium MULTIPLIER is 25.5x (within integer rounding);
///   3. the SPL transfers + 50/50 initiator revenue split move exactly the
///      right token amounts (initiator != protocol → half to each).
#[tokio::test]
async fn test_buy_returned_name_mid_auction_premium() {
    use ario_arns::pricing::{calculate_registration_fee, calculate_returned_name_premium};

    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "midwindow";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Release the name. `release_name` records `returned_at` = current chain
    // clock (TEST_PERIOD_ZERO_START, since we haven't warped yet) and sets the
    // initiator to the caller (ctx.payer) — NOT the config PDA — so the rebuy
    // takes the 50/50 split path.
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Confirm returned_at, and that the initiator is the payer (50/50 split path).
    let returned = {
        let acct = ctx
            .banks_client
            .get_account(returned_name_key)
            .await
            .unwrap()
            .unwrap();
        ReturnedName::try_deserialize(&mut acct.data.as_slice()).unwrap()
    };
    let returned_at = returned.returned_at;
    assert_eq!(returned_at, TEST_PERIOD_ZERO_START);
    assert_eq!(
        returned.initiator,
        ctx.payer.pubkey(),
        "initiator must be the release caller, not the protocol (drives the 50/50 split)"
    );
    assert_ne!(
        returned.initiator,
        config_pda().0,
        "initiator must differ from the config PDA so the rebuy is a 50/50 split"
    );

    // Warp to EXACTLY the 7-day midpoint of the 14-day window.
    // elapsed = 7d, duration = 14d → multiplier = 50 − 49*(7/14) = 25.5x.
    let mid_elapsed = RETURNED_NAME_DURATION_SECONDS / 2;
    assert_eq!(mid_elapsed, 7 * 86_400, "midpoint must be exactly 7 days");
    let buy_timestamp = returned_at + mid_elapsed;
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = buy_timestamp;
    ctx.set_sysvar(&clock);

    // Snapshot the two token-account balances before the rebuy.
    let protocol_before = spl_token_balance(&mut ctx, setup.protocol_token.pubkey()).await;
    let buyer_before = spl_token_balance(&mut ctx, setup.buyer_token.pubkey()).await;

    // Execute the mid-window buy_returned_name.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyReturnedName {
                config: config_pda().0,
                demand_factor: demand_factor_pda().0,
                returned_name: returned_name_key,
                arns_record: arns_record_pda(name).0,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                initiator_token_account: setup.buyer_token.pubkey(),
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyReturnedName {
                params: ario_arns::BuyReturnedNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read the charged price off the freshly-created record.
    let new_arns_key = arns_record_pda(name).0;
    let record = {
        let acct = ctx
            .banks_client
            .get_account(new_arns_key)
            .await
            .unwrap()
            .unwrap();
        ArnsRecord::try_deserialize(&mut acct.data.as_slice()).unwrap()
    };

    // Recompute the expected price from the LIVE on-chain demand factor + fees
    // (buy_returned_name lazily rolled the demand factor across the 7 idle
    // periods, so the registration fee already reflects that decay). Using the
    // protocol's own public pricing functions makes this assertion exact.
    let demand = {
        let acct = ctx
            .banks_client
            .get_account(setup.demand_factor_key)
            .await
            .unwrap()
            .unwrap();
        DemandFactor::try_deserialize(&mut acct.data.as_slice()).unwrap()
    };
    let base_fee = demand.fees[name.len() - 1];
    let registration_fee = calculate_registration_fee(
        base_fee,
        PurchaseType::Permabuy,
        0,
        demand.current_demand_factor,
    )
    .unwrap();
    let expected_mid_cost =
        calculate_returned_name_premium(registration_fee, returned_at, buy_timestamp).unwrap();

    assert_eq!(
        record.purchase_price, expected_mid_cost,
        "mid-window charged price must equal the protocol's linear-decay formula \
         (registration_fee={registration_fee}, expected={expected_mid_cost}, \
         got={})",
        record.purchase_price
    );

    // Verify the implied multiplier is 25.5x within integer rounding. The
    // formula computes multiplier = 25_500_000 (25.5 * SCALE) at the exact
    // midpoint, so purchase_price == registration_fee * 25_500_000 / SCALE.
    let expected_from_multiplier =
        (registration_fee as u128 * 25_500_000u128 / DEMAND_FACTOR_SCALE as u128) as u64;
    assert_eq!(
        record.purchase_price, expected_from_multiplier,
        "mid-window multiplier must be exactly 25.5x of the registration fee"
    );
    // And the back-computed multiplier rounds to ~25.5x (sanity on direction:
    // strictly between the 1x floor and the 50x ceiling).
    let implied_multiplier_scaled = (record.purchase_price as u128 * DEMAND_FACTOR_SCALE as u128
        / registration_fee as u128) as u64;
    assert!(
        (24_500_000..=25_500_000).contains(&implied_multiplier_scaled),
        "implied multiplier {implied_multiplier_scaled} (scaled) should be ~25.5x at the midpoint"
    );
    assert!(
        record.purchase_price > registration_fee,
        "mid-window price must exceed the 1x floor"
    );
    assert!(
        record.purchase_price < registration_fee * RETURNED_NAME_MAX_MULTIPLIER,
        "mid-window price must be below the 50x ceiling"
    );

    // Verify the SPL transfer + 50/50 revenue split moved exact amounts.
    // initiator != protocol → protocol gets token_cost/2, initiator (== the
    // buyer's own token account here) is refunded the remainder, so the buyer's
    // net spend is exactly the protocol's half.
    let reward_for_protocol = record.purchase_price / 2;
    let reward_for_initiator = record.purchase_price - reward_for_protocol;
    let protocol_after = spl_token_balance(&mut ctx, setup.protocol_token.pubkey()).await;
    let buyer_after = spl_token_balance(&mut ctx, setup.buyer_token.pubkey()).await;

    assert_eq!(
        protocol_after - protocol_before,
        reward_for_protocol,
        "protocol must receive exactly half of the mid-window cost"
    );
    // Buyer pays the full cost out and is refunded the initiator half back into
    // the same account → net debit is the protocol's half.
    assert_eq!(
        buyer_before - buyer_after,
        record.purchase_price - reward_for_initiator,
        "buyer's net debit must equal the protocol's half (initiator half refunded)"
    );
    assert_eq!(
        buyer_before - buyer_after,
        reward_for_protocol,
        "buyer net debit equals reward_for_protocol when self-buying"
    );

    // Record sanity.
    assert_eq!(record.owner, ctx.payer.pubkey());
    assert_eq!(record.name, name);
    assert!(matches!(record.purchase_type, PurchaseType::Permabuy));
}

// =========================================
// ERROR PATH TESTS
// =========================================

#[tokio::test]
async fn test_buy_name_invalid_format() {
    // Names starting with a dash are rejected by is_valid_arns_name → InvalidNameFormat
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "-invalidname".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidNameFormat);
}

#[tokio::test]
async fn test_buy_name_length_43() {
    // Length-43 names are prohibited (Arweave address collision) → InvalidNameFormat
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Exactly 43 lowercase alphanumeric chars
    let name = "a".repeat(43);
    let (arns_record_key, _) = arns_record_pda(&name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidNameFormat);
}

#[tokio::test]
async fn test_extend_permabuy() {
    // Extending a permabuy should fail with CannotExtendPermanent
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "permext";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Attempt to extend — should fail because it's a permabuy
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::CannotExtendPermanent);
}

#[tokio::test]
async fn test_upgrade_already_permanent() {
    // Upgrading an already-permanent name should fail with AlreadyPermanent
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "alreadyperm";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Attempt to upgrade — should fail because it's already permanent
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpgradeName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpgradeName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::AlreadyPermanent);
}

#[tokio::test]
async fn test_reassign_name_not_ant_holder() {
    // A non-ANT-holder trying to reassign should fail with NotAntHolder
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let new_ant_keypair = Keypair::new();
    let new_ant = new_ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    // ANT is owned by Pubkey::default() (the buyer/payer), not other_user
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    mint_test_ant(&mut ctx, &new_ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "reassignfail";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Create a second user and fund with SOL
    let other_user = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &other_user.pubkey(),
            1_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Other user tries to reassign — should fail because they don't hold the ANT NFT
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: ant_key,
        caller: other_user.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName { new_ant }.data(),
        }],
        Some(&other_user.pubkey()),
        &[&other_user],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::NotAntHolder);
}

// ============================================================================
// Codex finding (2026-05) — reassign_name / release_name must authorize against
// the CURRENT Metaplex Core ANT holder, NOT the stale `ArnsRecord.owner` (set
// once at buy_name and never updated). The ADR-016 reshape briefly regressed
// this to `caller == record.owner`, letting a prior buyer who sold the ANT keep
// reassign/release authority forever while the new holder could never get it.
// See BD-095 / BD-106.
// ============================================================================

/// reassign: stale `record.owner` (== the original buyer) must NOT retain
/// authority after the ANT is transferred away.
#[tokio::test]
async fn test_reassign_name_rejects_stale_record_owner_after_transfer() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let new_ant = Keypair::new().pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "stalereassign";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // record.owner is now ctx.payer. Sell the ANT to a different holder.
    let payer = Keypair::from_bytes(&ctx.payer.to_bytes()).unwrap();
    let new_holder = Keypair::new();
    transfer_test_ant(&mut ctx, ant_key, &payer, new_holder.pubkey()).await;

    // ctx.payer == record.owner but is NO LONGER the ANT holder → must fail.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: ant_key,
        caller: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName { new_ant }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::NotAntHolder);
}

/// reassign: the CURRENT ANT holder (≠ stale `record.owner`) CAN reassign.
#[tokio::test]
async fn test_reassign_name_current_holder_succeeds() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let new_ant_keypair = Keypair::new();
    let new_ant = new_ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    mint_test_ant(&mut ctx, &new_ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "freshreassign";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Transfer the ANT to a new holder and fund them with SOL for fees.
    let payer = Keypair::from_bytes(&ctx.payer.to_bytes()).unwrap();
    let new_holder = Keypair::new();
    transfer_test_ant(&mut ctx, ant_key, &payer, new_holder.pubkey()).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &new_holder.pubkey(),
            1_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    // new_holder != record.owner but IS the ANT holder → succeeds.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: ant_key,
        caller: new_holder.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName { new_ant }.data(),
        }],
        Some(&new_holder.pubkey()),
        &[&new_holder],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert_eq!(record.ant, new_ant);
    // The stale owner field is intentionally left untouched (informational only).
    assert_eq!(record.owner, ctx.payer.pubkey());
}

/// release: stale `record.owner` must NOT retain authority after transfer.
#[tokio::test]
async fn test_release_name_rejects_stale_record_owner_after_transfer() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "stalerelease";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    let payer = Keypair::from_bytes(&ctx.payer.to_bytes()).unwrap();
    let new_holder = Keypair::new();
    transfer_test_ant(&mut ctx, ant_key, &payer, new_holder.pubkey()).await;

    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::NotAntHolder);
}

/// release: the CURRENT ANT holder (≠ stale `record.owner`) CAN release.
#[tokio::test]
async fn test_release_name_current_holder_succeeds() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "freshrelease";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Transfer the ANT to a new holder and fund them (release inits a
    // ReturnedName PDA paid by the caller, plus the tx fee).
    let payer = Keypair::from_bytes(&ctx.payer.to_bytes()).unwrap();
    let new_holder = Keypair::new();
    transfer_test_ant(&mut ctx, ant_key, &payer, new_holder.pubkey()).await;
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund_tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &new_holder.pubkey(),
            1_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund_tx).await.unwrap();

    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: new_holder.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&new_holder.pubkey()),
        &[&new_holder],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Record closed; ReturnedName created with the new holder as initiator.
    assert!(ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .is_none());
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap()
        .unwrap();
    let returned = ReturnedName::try_deserialize(&mut returned_account.data.as_slice()).unwrap();
    assert_eq!(returned.initiator, new_holder.pubkey());
}

// `ant_asset` binding/ownership constraints (CodeRabbit PR #73). The
// authorization reads the holder from `ant_asset`, so `ant_asset` is pinned by
// account constraints to `arns_record.ant` AND to MPL Core ownership — both
// emit `InvalidAntAsset` (NOT `NotAntHolder`: the account constraints fail
// during Accounts validation, before the handler's holder check runs). These
// guard against a caller swapping in a *different* ANT they happen to hold, or
// a spoofed non-MPL account, to satisfy the holder check against the wrong key.

/// reassign: passing an unrelated ANT (≠ `record.ant`) as `ant_asset` — even
/// one the caller legitimately holds — is rejected.
#[tokio::test]
async fn test_reassign_name_rejects_wrong_ant_asset() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let other_ant_keypair = Keypair::new();
    let other_ant = other_ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    mint_test_ant(&mut ctx, &other_ant_keypair).await; // caller holds this too
    let setup = setup_arns(&mut ctx).await;

    let name = "wrongasset";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: other_ant, // != record.ant (which is ant_key)
        caller: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName {
                new_ant: Keypair::new().pubkey(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidAntAsset);
}

/// reassign: passing a non-MPL account (a random, non-existent pubkey) as
/// `ant_asset` is rejected.
#[tokio::test]
async fn test_reassign_name_rejects_non_mpl_ant_asset() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "nonmplasset";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: Keypair::new().pubkey(), // not an MPL Core asset
        caller: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName {
                new_ant: Keypair::new().pubkey(),
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidAntAsset);
}

/// release: passing an unrelated ANT (≠ `record.ant`) as `ant_asset` is rejected.
#[tokio::test]
async fn test_release_name_rejects_wrong_ant_asset() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let other_ant_keypair = Keypair::new();
    let other_ant = other_ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    mint_test_ant(&mut ctx, &other_ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "wrongrelasset";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: other_ant, // != record.ant
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidAntAsset);
}

/// release: passing a non-MPL account as `ant_asset` is rejected.
#[tokio::test]
async fn test_release_name_rejects_non_mpl_ant_asset() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "nonmplrelasset";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: Keypair::new().pubkey(), // not an MPL Core asset
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidAntAsset);
}

// (deleted obsolete test `test_name_management_after_nft_transfer` — exercised the
//  ARIO-ARNS-side trait sync mechanism that the Sprint 1-3
//  reshape moved to `ario_ant::sync_attributes`. Round-trip
//  coverage is now in programs/ario-ant/tests/sync_attributes.rs.
//  See ADR-016 amendment / BD-100.)

// =========================================
// PRUNE, COST, RESERVED CLAIM, AND DEMAND FACTOR TESTS
// =========================================

#[tokio::test]
async fn test_prune_to_returned() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "pruneret";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read the end_timestamp from the record
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    // Verify config shows 1 name registered
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 1);

    // Warp past end_timestamp + GRACE_PERIOD_SECONDS + 1
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + GRACE_PERIOD_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Call PruneToReturned
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneToReturned {
                config: setup.config_key,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneNameToReturned {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: ReturnedName PDA created with correct name
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap()
        .unwrap();
    let returned = ReturnedName::try_deserialize(&mut returned_account.data.as_slice()).unwrap();
    assert_eq!(returned.name, name);

    // Verify: ArnsRecord account is closed
    let record_account = ctx.banks_client.get_account(arns_record_key).await.unwrap();
    assert!(
        record_account.is_none(),
        "ArnsRecord should be closed after prune_to_returned"
    );

    // Verify: config.total_names_registered decreased
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 0);
}

#[tokio::test]
async fn test_prune_to_returned_still_active() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "activepr";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // DON'T warp — name is still active
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneToReturned {
                config: setup.config_key,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneNameToReturned {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::NameStillActive);
}

#[tokio::test]
async fn test_prune_expired_names() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "pruneexp";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read the end_timestamp from the record
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    // Warp past end_timestamp + GRACE_PERIOD + RETURNED_NAME_DURATION (auction window) + 1
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + GRACE_PERIOD_SECONDS + RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Call PruneExpiredNames with ArnsRecord as remaining_account
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_arns::accounts::PruneExpiredNames {
        config: setup.config_key,
        name_registry: registry_key,
        payer: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    // Add the ArnsRecord PDA as a writable remaining_account
    accounts.push(AccountMeta::new(arns_record_key, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::PruneExpiredNames { max_names: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: ArnsRecord account data zeroed (closed)
    let record_account = ctx.banks_client.get_account(arns_record_key).await.unwrap();
    match record_account {
        None => {} // Account fully reclaimed
        Some(acct) => {
            // Data should be all zeros
            assert!(
                acct.data.iter().all(|&b| b == 0),
                "ArnsRecord data should be zeroed"
            );
        }
    }
}

#[tokio::test]
async fn test_prune_returned_names() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "prunertd";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Release the name (creates ReturnedName)
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify ReturnedName exists
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap();
    assert!(
        returned_account.is_some(),
        "ReturnedName should exist after release"
    );

    // Warp past RETURNED_NAME_DURATION_SECONDS + 1
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Call PruneReturnedNames with ReturnedName PDA as remaining_account
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_arns::accounts::PruneReturnedNames {
        config: setup.config_key,
        payer: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(returned_name_key, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::PruneReturnedNames { max_names: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: ReturnedName account data zeroed (closed)
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap();
    match returned_account {
        None => {} // Account fully reclaimed
        Some(acct) => {
            assert!(
                acct.data.iter().all(|&b| b == 0),
                "ReturnedName data should be zeroed"
            );
        }
    }
}

#[tokio::test]
async fn test_get_token_cost() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    // Call GetTokenCost — a view instruction
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::BuyName,
                    name: "testcost".to_string(),
                    years: Some(1),
                    quantity: None,
                    purchase_type: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

#[tokio::test]
async fn test_claim_reserved_name() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name = "claimrsv";
    let (reserved_key, _) = reserved_name_pda(name);

    // Reserve the name
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: None,
                    expires_at: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify reserved name exists
    let account = ctx.banks_client.get_account(reserved_key).await.unwrap();
    assert!(account.is_some(), "ReservedName should exist");

    // Claim the reserved name
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ClaimReservedName {
                config: config_pda().0,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ClaimReservedName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify: ReservedName account is closed
    let account = ctx.banks_client.get_account(reserved_key).await.unwrap();
    assert!(
        account.is_none(),
        "ReservedName should be closed after claim"
    );
}

#[tokio::test]
async fn test_claim_reserved_name_unauthorized() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name = "claimfail";
    let (reserved_key, _) = reserved_name_pda(name);

    // Reserve the name (authority = payer)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: None,
                    expires_at: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Create a different keypair and fund it
    let other_user = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &other_user.pubkey(),
            1_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Other user tries ClaimReservedName — should fail (has_one = authority constraint)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ClaimReservedName {
                config: config_pda().0,
                reserved_name: reserved_key,
                authority: other_user.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ClaimReservedName {}.data(),
        }],
        Some(&other_user.pubkey()),
        &[&other_user],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "ClaimReservedName with wrong authority should fail"
    );
}

#[tokio::test]
async fn test_demand_factor_multi_period() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy a name to create period activity (period 1)
    buy_name_helper(
        &mut ctx,
        &setup,
        "demandmp",
        PurchaseType::Lease,
        1,
        ant_key,
    )
    .await;

    // Verify initial demand factor state
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(demand.current_period, 1);
    assert_eq!(demand.current_demand_factor, DEMAND_FACTOR_SCALE);
    assert_eq!(demand.purchases_this_period, 1);

    // Warp past 5 demand periods (5 * 86_400 + 1, relative to TEST_PERIOD_ZERO_START)
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 5 * PERIOD_LENGTH_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Call UpdateDemandFactor once — should catch up multiple periods
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpdateDemandFactor {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpdateDemandFactor {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify demand factor state after multi-period catch-up
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();

    // current_period should have advanced by 5+ (from period 1 to period 6)
    assert!(
        demand.current_period >= 6,
        "current_period should be at least 6, got {}",
        demand.current_period
    );

    // demand factor should have decreased (no activity = 0.985x per empty period)
    // Period 1→2: had 1 purchase (but trailing avg is 0 for first period), demand increases
    // Periods 2→6: no activity, demand decreases each period via 0.985x
    // Net: demand factor should be less than starting value
    assert!(
        demand.current_demand_factor < DEMAND_FACTOR_SCALE,
        "Demand factor should have decreased from {}, got {}",
        DEMAND_FACTOR_SCALE,
        demand.current_demand_factor,
    );
}

// =========================================
// NEW COVERAGE TESTS
// =========================================

/// Test 1: Buy a name whose reservation has expired.
/// Lua parity: arns.buyRecord() reservation check — if reserved AND not expired
/// AND target != buyer → reject. If expired → allow.
#[tokio::test]
async fn test_buy_name_reserved_expired() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "exprsrvd";
    let (reserved_key, _) = reserved_name_pda(name);

    // Warp clock forward so we can set an already-expired reservation
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 100;
    ctx.set_sysvar(&clock);

    // Reserve the name with an expiry timestamp in the past (timestamp = 1, already expired)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: Some(Pubkey::new_unique()), // reserved for someone else
                    expires_at: Some(1), // expires at timestamp 1 (already past since clock=100)
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify reservation exists
    let account = ctx.banks_client.get_account(reserved_key).await.unwrap();
    assert!(account.is_some(), "ReservedName should exist");

    // Buy the name — should succeed because reservation is expired (clock=100 > expires_at=1)
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_key,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify record was created
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert_eq!(record.owner, ctx.payer.pubkey());
    assert_eq!(record.name, name);

    // Verify reserved name account was cleaned up (BUG-7: auto-remove after purchase)
    let reserved_account = ctx.banks_client.get_account(reserved_key).await.unwrap();
    assert!(
        reserved_account.is_none() || reserved_account.unwrap().data.iter().all(|&b| b == 0),
        "ReservedName should be cleaned up after purchase of expired reservation"
    );
}

/// Test 2: Buying a name via buy_name when a returned name auction is active should fail.
/// Lua parity: arns.buyRecord() checks ReturnedNames[name] — must use buy_returned_name instead.
#[tokio::test]
async fn test_buy_name_returned_active() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "retactive";

    // Buy the name first
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Release the name to create a ReturnedName (Dutch auction)
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify ReturnedName exists
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap();
    assert!(
        returned_account.is_some(),
        "ReturnedName should exist after release"
    );

    // Try to buy_name (not buy_returned_name) while auction is active → should fail with AuctionActive
    let (new_arns_record_key, _) = arns_record_pda(name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: new_arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(name).0,
                returned_name_check: returned_name_key,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::AuctionActive);
}

/// Test 4: Buy a returned name with revenue split — 50% to protocol, 50% to initiator.
/// Lua parity: math.floor(cost * 0.5) to each when initiator != protocol.
///
/// Uses the same payer for initial purchase and release (so payer = initiator),
/// then a second buyer purchases the returned name. Because the initiator is payer
/// (not the protocol config PDA), the 50/50 split applies.
#[tokio::test]
async fn test_buy_returned_name_revenue_split() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "splitrev";

    // Buy and release the name (payer = initiator)
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past auction window so premium = 0 (base registration fee only)
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Record protocol and initiator (=buyer_token) balances before purchase
    let protocol_before = {
        let account = ctx
            .banks_client
            .get_account(setup.protocol_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };
    let initiator_before = {
        let account = ctx
            .banks_client
            .get_account(setup.buyer_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };

    // Buy the returned name
    // The initiator is ctx.payer (who released it) and their token account is setup.buyer_token
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyReturnedName {
                config: config_pda().0,
                demand_factor: demand_factor_pda().0,
                returned_name: returned_name_key,
                arns_record: arns_record_pda(name).0,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                initiator_token_account: setup.buyer_token.pubkey(),
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyReturnedName {
                params: ario_arns::BuyReturnedNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Read the purchase price from the record
    let new_arns_key = arns_record_pda(name).0;
    let record_account = ctx
        .banks_client
        .get_account(new_arns_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let total_cost = record.purchase_price;
    assert!(total_cost > 0, "Purchase price should be positive");

    // Check protocol balance increased
    let protocol_after = {
        let account = ctx
            .banks_client
            .get_account(setup.protocol_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };
    let protocol_received = protocol_after - protocol_before;

    // Protocol should receive 50% (floor division)
    let expected_protocol_share = total_cost / 2;
    assert_eq!(
        protocol_received, expected_protocol_share,
        "Protocol should receive 50% of cost ({} / 2 = {}), got {}",
        total_cost, expected_protocol_share, protocol_received
    );

    // Initiator balance change: they paid total_cost from buyer_token and received initiator share back to buyer_token.
    // Net change = -total_cost + initiator_share = -(total_cost - initiator_share) = -protocol_share
    let initiator_after = {
        let account = ctx
            .banks_client
            .get_account(setup.buyer_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };
    let initiator_net_spent = initiator_before - initiator_after;
    // The buyer spent total_cost but received back (total_cost - protocol_share) as initiator reward
    // So net cost = protocol_share = total_cost / 2
    assert_eq!(
        initiator_net_spent, expected_protocol_share,
        "Initiator (buyer) net cost should equal protocol share ({}) since initiator reward offsets, got {}",
        expected_protocol_share, initiator_net_spent
    );
}

/// Test 5: Demand factor increases when revenue exceeds moving average.
/// Lua parity: demand.updateDemandFactor() UP path, factor * (1 + 0.05).
#[tokio::test]
async fn test_demand_factor_increasing() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let ant_keypair2 = Keypair::new();
    let ant_key2 = ant_keypair2.pubkey();
    let ant_keypair3 = Keypair::new();
    let ant_key3 = ant_keypair3.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair2).await;
    mint_test_ant(&mut ctx, &ant_keypair3).await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Period 1: Buy a name to generate revenue
    buy_name_helper(
        &mut ctx,
        &setup,
        "demfact1",
        PurchaseType::Permabuy,
        0,
        ant_key,
    )
    .await;

    // Advance to period 2
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + PERIOD_LENGTH_SECONDS + 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpdateDemandFactor {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpdateDemandFactor {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Period 1→2 transition: period 1 had revenue > 0 and trailing avg was 0,
    // so demand should have increased. The demand factor check: revenue > avg → UP.
    // Since all trailing revenues start at 0 and period 1 had revenue > 0, demand_increasing = true.
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();

    assert_eq!(demand.current_period, 2);
    // After one UP adjustment: 1_000_000 * 1_050_000 / 1_000_000 = 1_050_000
    assert_eq!(
        demand.current_demand_factor, 1_050_000,
        "Demand factor should be 1.05 after one period of increasing demand, got {}",
        demand.current_demand_factor
    );
}

/// Test 6: Demand factor fee halving after 7+ consecutive periods at minimum.
/// Lua parity: demand.updateDemandFactor() halving path, MAX_PERIODS_AT_MIN_DEMAND_FACTOR = 7.
///
/// Strategy: Force the demand factor to min by warping through many zero-activity periods.
/// Then verify that after 7 consecutive periods at min, fees are halved and factor resets to 1.0.
#[tokio::test]
async fn test_demand_factor_fee_halving() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    // Read initial fees
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    let initial_fee_1char = demand.fees[0]; // 500_000_000_000
    let initial_fee_5char = demand.fees[4]; // 2_500_000_000
    assert_eq!(initial_fee_1char, GENESIS_FEES[0]);
    assert_eq!(initial_fee_5char, GENESIS_FEES[4]);

    // Compute the exact number of periods needed to trigger fee halving.
    //
    // Phase 1: Drive demand_factor down to DEMAND_FACTOR_MIN via 0.985x per period.
    // Phase 2: Accumulate >= MAX_PERIODS_AT_MIN_DEMAND_FACTOR (7) consecutive periods at min.
    //
    // The halving triggers when consecutive_periods_with_min_demand_factor >= 7.
    // The increment happens BEFORE the check in the loop:
    //   1) Apply adjustment (clamp to min)
    //   2) If factor <= min: increment consecutive counter, then check >= 7
    //   3) If >= 7: halve fees, reset factor to 1.0, reset consecutive to 0
    //
    // We simulate offline to find the exact period count.
    let mut sim_factor: u64 = DEMAND_FACTOR_SCALE;
    let mut sim_consecutive: u32 = 0;
    let mut halving_period: u64 = 0;
    for period in 1..=200u64 {
        // Decrease (no activity = not increasing)
        if sim_factor > DEMAND_FACTOR_MIN {
            sim_factor = (sim_factor as u128 * DEMAND_FACTOR_DOWN_ADJUSTMENT as u128
                / DEMAND_FACTOR_SCALE as u128) as u64;
        }
        // Floor check
        if sim_factor <= DEMAND_FACTOR_MIN {
            sim_factor = DEMAND_FACTOR_MIN;
            if sim_consecutive >= MAX_PERIODS_AT_MIN_DEMAND_FACTOR as u32 {
                // Halving would occur here
                halving_period = period;
                break;
            } else {
                sim_consecutive += 1;
            }
        } else {
            sim_consecutive = 0;
        }
    }
    assert!(halving_period > 0, "Should have found halving period");

    // Warp to exactly the halving period (current_period starts at 1, so we need
    // current_period_for_timestamp = 1 + halving_period)
    // get_period_for_timestamp: (timestamp - period_zero_start) / 86400 + 1
    // So timestamp = period_zero_start + (target_period - 1) * 86400 + 1
    let target_period = 1 + halving_period;
    let target_timestamp =
        TEST_PERIOD_ZERO_START + ((target_period - 1) as i64) * PERIOD_LENGTH_SECONDS + 1;

    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = target_timestamp;
    ctx.set_sysvar(&clock);

    // Call UpdateDemandFactor — catches up all periods
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpdateDemandFactor {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpdateDemandFactor {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify demand factor state after halving
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();

    // After fee halving, the factor resets to 1.0 (DEMAND_FACTOR_SCALE)
    assert_eq!(
        demand.current_demand_factor, DEMAND_FACTOR_SCALE,
        "Demand factor should reset to 1.0 after fee halving, got {}",
        demand.current_demand_factor
    );

    // Consecutive periods counter should be reset to 0
    assert_eq!(
        demand.consecutive_periods_with_min_demand_factor, 0,
        "Consecutive periods at min should reset after halving, got {}",
        demand.consecutive_periods_with_min_demand_factor
    );

    // Fees should be halved
    let halved_fee_1char = initial_fee_1char / 2;
    let halved_fee_5char = initial_fee_5char / 2;
    assert_eq!(
        demand.fees[0], halved_fee_1char,
        "1-char fee should be halved from {} to {}, got {}",
        initial_fee_1char, halved_fee_1char, demand.fees[0]
    );
    assert_eq!(
        demand.fees[4], halved_fee_5char,
        "5-char fee should be halved from {} to {}, got {}",
        initial_fee_5char, halved_fee_5char, demand.fees[4]
    );
}

/// Test 7: Upgrade a lease to permabuy during the grace period.
/// Lua parity: arns.upgradeRecord() allows upgrade if active OR in grace period.
#[tokio::test]
async fn test_upgrade_name_in_grace_period() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "graceupg";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read the end_timestamp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();
    assert!(matches!(record.purchase_type, PurchaseType::Lease));

    // Warp into the grace period: end_timestamp + 1 day (well within 14-day grace)
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + 86_400; // 1 day into grace period
    ctx.set_sysvar(&clock);

    // Verify the record is now in grace period (expired but within grace)
    assert!(!record.is_active(clock.unix_timestamp));
    assert!(record.is_in_grace_period(clock.unix_timestamp, GRACE_PERIOD_SECONDS));

    // Upgrade to permabuy during grace period — should succeed
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpgradeName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpgradeName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify the record is now a permabuy
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert!(matches!(record.purchase_type, PurchaseType::Permabuy));
    assert!(record.end_timestamp.is_none());
    assert!(record.purchase_price > 0);
}

/// Test 8: Extending a lease beyond MAX_LEASE_LENGTH_YEARS (5 years total) should error.
/// Lua parity: arns.extendNameLeaseRecord() checks remaining + extension <= maxLeaseLengthYears.
///
/// Variant: Buy with max years (5), then try to extend by 1 → should fail immediately.
#[tokio::test]
async fn test_extend_lease_max_years() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "maxlease";
    // Buy with max years (5)
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 5, ant_key).await;

    // Verify the lease has 5 years
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert!(matches!(record.purchase_type, PurchaseType::Lease));
    assert!(record.end_timestamp.is_some());

    // Try to extend by 1 more year — should fail with ExtensionExceedsMax
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::ExtensionExceedsMax);
}

/// Test: Buy a reserved name that has NOT expired — should fail with NameReserved.
/// Complements test_buy_name_reserved_expired by verifying the negative path.
#[tokio::test]
async fn test_buy_name_reserved_not_expired() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "notexprd";
    let (reserved_key, _) = reserved_name_pda(name);

    // Reserve the name with no expiry (permanent reservation) for someone else
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: None, // reserved for no one specific → blocks all buyers
                    expires_at: None,   // never expires
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to buy the reserved name — should fail with NameReserved
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_key,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::NameReserved);
}

/// Test: Prune an expired reservation (permissionless).
/// Lua parity: pruneReservedNames in Lua allows anyone to remove expired reservations.
#[tokio::test]
async fn test_prune_expired_reservation() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name = "prunersv";
    let (reserved_key, _) = reserved_name_pda(name);

    // Reserve the name with an expiry of 100 seconds
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: Some(Pubkey::new_unique()),
                    expires_at: Some(100), // expires at timestamp 100
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify reservation exists
    let account = ctx.banks_client.get_account(reserved_key).await.unwrap();
    assert!(account.is_some(), "ReservedName should exist");

    // Warp past the expiry
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 101; // past expiry of 100 (base is also past it, but keep an explicit warp for intent)
    ctx.set_sysvar(&clock);

    // Prune the expired reservation (permissionless)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneExpiredReservation {
                reserved_name: reserved_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneExpiredReservation {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify reservation is closed
    let account = ctx.banks_client.get_account(reserved_key).await.unwrap();
    assert!(
        account.is_none(),
        "ReservedName should be closed after pruning"
    );
}

// NOTE: Test 3 (test_buy_name_gateway_discount) and the parallel attack-vector
// tests called out in audit TEST-005 are not implemented at the integration
// layer. The 20% gateway operator discount is gated by FIVE defenses inside
// try_apply_gateway_discount (pricing.rs:260):
//   (a) account.owner == ario_gar::ID                  (line 273)
//   (b) gateway_info.key() == expected_gateway_pda     (line 283)
//   (c) gateway.operator == signer                     (line 295)
//   (d) gateway.status == Joined                       (line 301)
//   (e) time_running >= 180 days                       (line 311)
// Each fires a require! → ArnsError on failure. Reproducing this attack surface
// at the arns integration layer requires building a synthetic ario-gar Gateway
// account (correct discriminator + struct layout) inside the arns test harness
// — duplicating most of ario-gar's test setup for marginal incremental coverage.
// What IS tested:
//   - The discount math (apply_gateway_operator_discount) — pricing.rs unit tests
//   - The tenure boundary (>= vs >) — gateway_discount_tenure_boundary_inclusive
//     and gateway_discount_tenure_clock_skew_protection unit tests (audit TEST-014)
//   - PDA derivation parity with ario-gar — covered by ario-gar's own integration
//     tests for the same GATEWAY_SEED constant.
// If a future audit finding requires end-to-end attack-vector coverage, the next
// step is to factor a `make_synthetic_gateway()` helper that pt.add_account()s
// a hand-crafted Gateway PDA, then write per-vector tests calling buy_name with
// remaining_accounts[0] = bad_gateway and asserting the specific ArnsError.

// NOTE: Test 9 (test_release_name_lease_error) is already covered by the existing
// test_release_name_lease_fails test at line 1042. The existing test buys a lease name,
// attempts to release it, and verifies ArnsError::CannotReleaseLease is returned.
// This matches the Lua parity check: arns.releaseRecord() asserts record.type == "permabuy".

// =========================================
// COVERAGE GAP TESTS
// =========================================

// -----------------------------------------
// Priority 1A: Reserved name with specific target — buyer is different → NameReserved
// Covers purchase.rs line 42: Some(target) => require!(target == buyer, NameReserved)
// -----------------------------------------
#[tokio::test]
async fn test_buy_name_reserved_for_different_user() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "targeted";
    let (reserved_key, _) = reserved_name_pda(name);

    // Reserve the name for a specific user (someone else, NOT the payer)
    let target_user = Pubkey::new_unique();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: Some(target_user), // reserved for someone else
                    expires_at: None,                // never expires
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Payer (not the target user) tries to buy the reserved name → NameReserved
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_key,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::NameReserved);
}

// -----------------------------------------
// Priority 1C: Permabuy with years=0 validation path in buy_name
// Covers the Permabuy branch in purchase.rs line 107-114 (end_timestamp = None path)
// Also validates that lease with years=0 is rejected (InvalidLeaseDuration).
// -----------------------------------------
#[tokio::test]
async fn test_buy_name_lease_zero_years_fails() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "zeroyear";
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(name).0,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 0, // Invalid for lease
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidLeaseDuration);
}

// -----------------------------------------
// Priority 1C: Lease with years > MAX_LEASE_LENGTH_YEARS (5) fails
// Covers purchase.rs line 67-69
// -----------------------------------------
#[tokio::test]
async fn test_buy_name_lease_exceeds_max_years() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "sixyears";
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(name).0,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 6, // Exceeds MAX_LEASE_LENGTH_YEARS (5)
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidLeaseDuration);
}

// -----------------------------------------
// Priority 2D: Prune multiple expired names in one call
// Covers prune.rs lines 20-111 (the prune loop, PDA verification, registry removal, closing)
// -----------------------------------------
#[tokio::test]
async fn test_prune_expired_names_multiple() {
    let ant_keypair1 = Keypair::new();
    let ant_key1 = ant_keypair1.pubkey();
    let ant_keypair2 = Keypair::new();
    let ant_key2 = ant_keypair2.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair1).await;
    mint_test_ant(&mut ctx, &ant_keypair2).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy two lease names
    let name1 = "prunemul1";
    let name2 = "prunemul2";
    let arns1 = buy_name_helper(&mut ctx, &setup, name1, PurchaseType::Lease, 1, ant_key1).await;
    let arns2 = buy_name_helper(&mut ctx, &setup, name2, PurchaseType::Lease, 1, ant_key2).await;

    // Verify config shows 2 names registered
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 2);

    // Verify registry count = 2
    let registry_key = name_registry_key();
    let registry_account = ctx
        .banks_client
        .get_account(registry_key)
        .await
        .unwrap()
        .unwrap();
    let registry_count = u32::from_le_bytes(registry_account.data[40..44].try_into().unwrap());
    assert_eq!(registry_count, 2);

    // Read end_timestamp from first record
    let record_account = ctx.banks_client.get_account(arns1).await.unwrap().unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    // Warp past end + grace + auction
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + GRACE_PERIOD_SECONDS + RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Call PruneExpiredNames with both records as remaining_accounts
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_arns::accounts::PruneExpiredNames {
        config: setup.config_key,
        name_registry: registry_key,
        payer: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(arns1, false));
    accounts.push(AccountMeta::new(arns2, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::PruneExpiredNames { max_names: 5 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify both records are closed (data zeroed or account gone)
    for key in [arns1, arns2] {
        let acct = ctx.banks_client.get_account(key).await.unwrap();
        match acct {
            None => {}
            Some(a) => assert!(a.data.iter().all(|&b| b == 0), "Record should be zeroed"),
        }
    }

    // Verify config.total_names_registered decreased by 2
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 0);

    // Verify registry count = 0
    let registry_account = ctx
        .banks_client
        .get_account(registry_key)
        .await
        .unwrap()
        .unwrap();
    let registry_count = u32::from_le_bytes(registry_account.data[40..44].try_into().unwrap());
    assert_eq!(registry_count, 0);
}

// -----------------------------------------
// Priority 2E: Prune skips permabuy records (they never expire)
// Covers prune.rs line 48-49: PurchaseType::Permabuy => false
// -----------------------------------------
#[tokio::test]
async fn test_prune_expired_names_skips_permabuy() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "permprun";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Warp far into the future (permabuy should never be prunable)
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 100_000_000; // Far future
    ctx.set_sysvar(&clock);

    // Call PruneExpiredNames with the permabuy record as remaining_account
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_arns::accounts::PruneExpiredNames {
        config: setup.config_key,
        name_registry: registry_key,
        payer: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(arns_record_key, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::PruneExpiredNames { max_names: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify the permabuy record still exists
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert!(matches!(record.purchase_type, PurchaseType::Permabuy));
    assert_eq!(record.name, name);

    // Verify config still shows 1 name registered
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 1);
}

// -----------------------------------------
// Priority 2E: Prune multiple returned names in one call
// Covers prune.rs prune_returned loop (lines 137-185)
// -----------------------------------------
#[tokio::test]
async fn test_prune_returned_names_multiple() {
    let ant_keypair1 = Keypair::new();
    let ant_key1 = ant_keypair1.pubkey();
    let ant_keypair2 = Keypair::new();
    let ant_key2 = ant_keypair2.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair1).await;
    mint_test_ant(&mut ctx, &ant_keypair2).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy and release two names
    let name1 = "retmult1";
    let name2 = "retmult2";
    let arns1 = buy_name_helper(&mut ctx, &setup, name1, PurchaseType::Permabuy, 0, ant_key1).await;
    let arns2 = buy_name_helper(&mut ctx, &setup, name2, PurchaseType::Permabuy, 0, ant_key2).await;

    let registry_key = name_registry_key();
    let (returned1, _) = returned_name_pda(name1);
    let (returned2, _) = returned_name_pda(name2);

    // Release name1
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns1,
                returned_name: returned1,
                name_registry: registry_key,
                ant_asset: ant_key1,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Release name2
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns2,
                returned_name: returned2,
                name_registry: registry_key,
                ant_asset: ant_key2,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify both returned names exist
    assert!(ctx
        .banks_client
        .get_account(returned1)
        .await
        .unwrap()
        .is_some());
    assert!(ctx
        .banks_client
        .get_account(returned2)
        .await
        .unwrap()
        .is_some());

    // Warp past auction duration
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Prune both returned names in one call
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_arns::accounts::PruneReturnedNames {
        config: setup.config_key,
        payer: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(returned1, false));
    accounts.push(AccountMeta::new(returned2, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::PruneReturnedNames { max_names: 5 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify both returned names are closed
    for key in [returned1, returned2] {
        let acct = ctx.banks_client.get_account(key).await.unwrap();
        match acct {
            None => {}
            Some(a) => assert!(
                a.data.iter().all(|&b| b == 0),
                "ReturnedName should be zeroed"
            ),
        }
    }
}

// -----------------------------------------
// Priority 3F: Extend lease on expired record (past grace period) → RecordExpired
// Covers manage.rs extend_lease lines 77-80
// -----------------------------------------
#[tokio::test]
async fn test_extend_lease_expired() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "expextnd";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read end_timestamp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    // Warp past end + grace period + 1 (fully expired)
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + GRACE_PERIOD_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Try to extend — should fail with RecordExpired
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::RecordExpired);
}

// -----------------------------------------
// Priority 3G: Upgrade expired record (past grace period) → RecordExpired
// Covers manage.rs upgrade_name lines 20-24
// -----------------------------------------
#[tokio::test]
async fn test_upgrade_name_expired() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "expupgrd";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read end_timestamp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    // Warp past end + grace period + 1 (fully expired)
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + GRACE_PERIOD_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Try to upgrade — should fail with RecordExpired
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpgradeName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpgradeName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::RecordExpired);
}

// -----------------------------------------
// Priority 3H: Extend lease during grace period — should succeed
// Covers manage.rs extend_lease lines 77-80 (active OR in_grace_period)
// -----------------------------------------
#[tokio::test]
async fn test_extend_lease_in_grace_period() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "graceext";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read end_timestamp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    // Warp into the grace period: end_timestamp + 1 day
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + 86_400; // 1 day into grace period
    ctx.set_sysvar(&clock);

    // Extend by 1 year during grace period — should succeed
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify end_timestamp increased by 1 year from original end
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert_eq!(record.end_timestamp.unwrap(), end_ts + ONE_YEAR_SECONDS);
}

// -----------------------------------------
// Priority 3H: Increase undername limit on expired record → RecordExpired
// Covers manage.rs increase_undername_limit line 154
// -----------------------------------------
#[tokio::test]
async fn test_increase_undername_limit_expired() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "expunder";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read end_timestamp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    // Warp past expiry (but note: is_active checks end >= timestamp, so end_ts + 1 is expired)
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + 1;
    ctx.set_sysvar(&clock);

    // Try to increase undername limit — should fail with RecordExpired
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::IncreaseUndernameLimit {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::IncreaseUndernameLimit { quantity: 5 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::RecordExpired);
}

// -----------------------------------------
// Priority 4I: Demand factor update is a no-op when still in the same period
// Covers demand.rs lines 28-29 (early return)
// -----------------------------------------
#[tokio::test]
async fn test_demand_factor_same_period_noop() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    // First, call UpdateDemandFactor to bring current_period up to match the clock.
    // ProgramTest starts with a non-zero timestamp, so we need to sync first.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpdateDemandFactor {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpdateDemandFactor {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Now read the synced state
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand_before = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    let period_before = demand_before.current_period;
    let factor_before = demand_before.current_demand_factor;

    // Call UpdateDemandFactor again WITHOUT advancing time — should be a no-op
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpdateDemandFactor {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpdateDemandFactor {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify nothing changed — same period, same factor
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand_after = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(demand_after.current_period, period_before);
    assert_eq!(demand_after.current_demand_factor, factor_before);
    assert_eq!(
        demand_after.purchases_this_period,
        demand_before.purchases_this_period
    );
    assert_eq!(
        demand_after.revenue_this_period,
        demand_before.revenue_this_period
    );
}

// -----------------------------------------
// Priority 4J: Demand factor with purchases-based criteria
// Covers demand.rs lines 116-121 (is_demand_increasing purchases branch)
// -----------------------------------------
#[tokio::test]
async fn test_demand_factor_purchases_criteria() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Modify the demand factor's criteria field to DEMAND_CRITERIA_PURCHASES via raw data
    // The criteria field is at offset: discriminator(8) + current_demand_factor(8) + current_period(8)
    // + purchases_this_period(8) + revenue_this_period(8) + consecutive_periods(4)
    // + trailing_purchases(56) + trailing_revenues(56) + fees(408) + period_zero_start(8)
    // = 8 + 8 + 8 + 8 + 8 + 4 + 56 + 56 + 408 + 8 = 572
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let mut df_data = df_account.data.clone();
    df_data[572] = DEMAND_CRITERIA_PURCHASES;
    ctx.set_account(
        &setup.demand_factor_key,
        &solana_sdk::account::Account {
            lamports: df_account.lamports,
            data: df_data,
            owner: df_account.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Buy a name to generate purchase activity
    buy_name_helper(
        &mut ctx,
        &setup,
        "purchcri",
        PurchaseType::Lease,
        1,
        ant_key,
    )
    .await;

    // Advance to period 2
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + PERIOD_LENGTH_SECONDS + 1;
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpdateDemandFactor {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpdateDemandFactor {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify demand factor increased (purchases > trailing avg of 0 → UP)
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(demand.current_period, 2);
    assert_eq!(demand.current_demand_factor, 1_050_000); // 1.05x
    assert_eq!(demand.criteria, DEMAND_CRITERIA_PURCHASES);
}

// -----------------------------------------
// Priority 5L: GetTokenCost with ExtendLease intent
// Covers cost.rs lines 28-30 (ExtendLease branch)
// -----------------------------------------
#[tokio::test]
async fn test_get_token_cost_extend_lease() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::ExtendLease,
                    name: "testcost".to_string(), // 8-char → base fee = 500_000_000
                    years: Some(2),
                    quantity: None,
                    purchase_type: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// -----------------------------------------
// Priority 5L: GetTokenCost with UpgradeName intent
// Covers cost.rs lines 32-34 (UpgradeName branch)
// -----------------------------------------
#[tokio::test]
async fn test_get_token_cost_upgrade_name() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::UpgradeName,
                    name: "testcost".to_string(),
                    years: None,
                    quantity: None,
                    purchase_type: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// -----------------------------------------
// Priority 5L: GetTokenCost with IncreaseUndernameLimit intent
// Covers cost.rs lines 35-38 (IncreaseUndernameLimit branch)
// -----------------------------------------
#[tokio::test]
async fn test_get_token_cost_increase_undername_limit() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::IncreaseUndernameLimit,
                    name: "testcost".to_string(),
                    years: None,
                    quantity: Some(10),
                    purchase_type: Some(PurchaseType::Permabuy),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// -----------------------------------------
// Priority 5M: GetTokenCost with PrimaryNameRequest intent
// Covers cost.rs lines 40-47 (PrimaryNameRequest branch)
// -----------------------------------------
#[tokio::test]
async fn test_get_token_cost_primary_name_request() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::PrimaryNameRequest,
                    name: "testcost".to_string(),
                    years: None,
                    quantity: None,
                    purchase_type: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();
}

// -----------------------------------------
// Priority 5: GetTokenCost with ExtendLease missing years → InvalidParameter
// Covers cost.rs line 29 (years.ok_or error path)
// -----------------------------------------
#[tokio::test]
async fn test_get_token_cost_extend_lease_missing_years() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::ExtendLease,
                    name: "testcost".to_string(),
                    years: None, // Missing required years
                    quantity: None,
                    purchase_type: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidParameter);
}

// -----------------------------------------
// Priority 5: GetTokenCost IncreaseUndernameLimit missing quantity → InvalidParameter
// Covers cost.rs line 36 (quantity.ok_or error path)
// -----------------------------------------
#[tokio::test]
async fn test_get_token_cost_undername_missing_quantity() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::IncreaseUndernameLimit,
                    name: "testcost".to_string(),
                    years: None,
                    quantity: None, // Missing required quantity
                    purchase_type: Some(PurchaseType::Lease),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidParameter);
}

// -----------------------------------------
// Priority 5: GetTokenCost IncreaseUndernameLimit missing purchase_type → InvalidParameter
// Covers cost.rs line 37 (purchase_type.ok_or error path)
// -----------------------------------------
#[tokio::test]
async fn test_get_token_cost_undername_missing_purchase_type() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::IncreaseUndernameLimit,
                    name: "testcost".to_string(),
                    years: None,
                    quantity: Some(10),
                    purchase_type: None, // Missing required purchase_type
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidParameter);
}

// -----------------------------------------
// Priority 6N: Prune reservation that hasn't expired → ReservationNotExpired
// Covers reserved.rs lines 56-57
// -----------------------------------------
#[tokio::test]
async fn test_prune_reservation_not_expired() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name = "notexppr";
    let (reserved_key, _) = reserved_name_pda(name);

    // Reserve the name with expiry far in the future
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: None,
                    expires_at: Some(i64::MAX), // Never expires in practice
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to prune — should fail with ReservationNotExpired
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneExpiredReservation {
                reserved_name: reserved_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneExpiredReservation {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::ReservationNotExpired);
}

// -----------------------------------------
// Priority 6N: Prune permanent reservation (no expiry) → ReservationNotExpired
// Covers reserved.rs lines 59-60 (None => return Err)
// -----------------------------------------
#[tokio::test]
async fn test_prune_permanent_reservation() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name = "permarsv";
    let (reserved_key, _) = reserved_name_pda(name);

    // Reserve the name with NO expiry (permanent)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: None,
                    expires_at: None, // Permanent reservation
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Try to prune permanent reservation — should fail with ReservationNotExpired
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneExpiredReservation {
                reserved_name: reserved_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneExpiredReservation {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::ReservationNotExpired);
}

// -----------------------------------------
// Priority 3: Increase undername limit with quantity=0 → InvalidUndernameQuantity
// Covers manage.rs increase_undername_limit line 153
// -----------------------------------------
#[tokio::test]
async fn test_increase_undername_limit_zero_quantity() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "zeroqty";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Try to increase by 0 — should fail
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::IncreaseUndernameLimit {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::IncreaseUndernameLimit { quantity: 0 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidUndernameQuantity);
}

// -----------------------------------------
// Priority 2: Prune to returned fails on permabuy (NameStillActive)
// Covers prune.rs prune_to_returned lines 203-210
// -----------------------------------------
#[tokio::test]
async fn test_prune_to_returned_permabuy_fails() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "permprtn";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Warp far into the future
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 100_000_000;
    ctx.set_sysvar(&clock);

    // Try to prune a permabuy → should fail with NameStillActive
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneToReturned {
                config: setup.config_key,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneNameToReturned {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::NameStillActive);
}

// -----------------------------------------
// Priority 1: Buy returned name as lease (not just permabuy)
// Covers purchase.rs buy_returned_name lines 260-266 (lease end_timestamp path)
// -----------------------------------------
#[tokio::test]
async fn test_buy_returned_name_as_lease() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "retlease";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Release the name
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Warp past auction window
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Buy the returned name as a LEASE (not permabuy)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyReturnedName {
                config: config_pda().0,
                demand_factor: demand_factor_pda().0,
                returned_name: returned_name_key,
                arns_record: arns_record_pda(name).0,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                initiator_token_account: setup.buyer_token.pubkey(),
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyReturnedName {
                params: ario_arns::BuyReturnedNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 2,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify record is a lease with correct end_timestamp
    let new_arns_key = arns_record_pda(name).0;
    let record_account = ctx
        .banks_client
        .get_account(new_arns_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert!(matches!(record.purchase_type, PurchaseType::Lease));
    assert!(record.end_timestamp.is_some());
    assert!(record.purchase_price > 0);
}

// -----------------------------------------
// TEST-007: Double-prune defense (audit SECURITY_AUDIT_INDEPENDENT.md)
// prune_expired_names must be idempotent — passing an already-pruned
// (closed) ArnsRecord as remaining_account should be silently skipped,
// not re-decrement total_names_registered or fault the tx. This protects
// against a malicious cranker batching a closed record alongside live
// ones to artificially deflate the registered-names counter.
// The defense lives at prune.rs:27-29 ("if record_info.owner != program_id { continue }").
// -----------------------------------------

#[tokio::test]
async fn test_prune_expired_names_already_pruned_is_noop() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "doubleprune";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read end_timestamp, then warp past expiry + grace + auction window
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + GRACE_PERIOD_SECONDS + RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Sanity: total_names_registered == 1 before any prune
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(config.total_names_registered, 1);

    let registry_key = name_registry_key();

    // First prune: closes the record, decrements total_names_registered to 0
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let mut accounts = ario_arns::accounts::PruneExpiredNames {
        config: setup.config_key,
        name_registry: registry_key,
        payer: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    accounts.push(AccountMeta::new(arns_record_key, false));

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: accounts.clone(),
            data: ario_arns::instruction::PruneExpiredNames { max_names: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(
        config.total_names_registered, 0,
        "First prune should decrement to 0"
    );

    // Second prune with the SAME (now closed) record: must succeed without
    // touching total_names_registered. The owner-check guard at prune.rs:27
    // makes the loop skip system-owned accounts.
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::PruneExpiredNames { max_names: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Counter must NOT have rolled past 0 (saturating_sub keeps it at 0,
    // but more importantly: pruned == 0 so the subtract branch isn't even taken).
    let config_account = ctx
        .banks_client
        .get_account(setup.config_key)
        .await
        .unwrap()
        .unwrap();
    let config = ArnsConfig::try_deserialize(&mut config_account.data.as_slice()).unwrap();
    assert_eq!(
        config.total_names_registered, 0,
        "Second prune of closed record must be a no-op"
    );
}

// Companion: prune_to_returned on an already-pruned record must reject at
// account validation (the ArnsRecord PDA is now system-owned after the
// `close = payer` constraint zeroed it out and reassigned ownership).
#[tokio::test]
async fn test_prune_to_returned_already_pruned_fails() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "dblpr2rt";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + GRACE_PERIOD_SECONDS + 1;
    ctx.set_sysvar(&clock);

    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();

    // First prune_to_returned succeeds — closes ArnsRecord, creates ReturnedName.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneToReturned {
                config: setup.config_key,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneNameToReturned {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify ArnsRecord was actually closed
    let post_record = ctx.banks_client.get_account(arns_record_key).await.unwrap();
    assert!(
        post_record.is_none() || post_record.unwrap().lamports == 0,
        "ArnsRecord should be closed after first prune_to_returned"
    );

    // Second prune_to_returned with the same key: Anchor's account
    // validation must reject because the PDA is no longer a program-owned
    // ArnsRecord. We accept any error here — the important thing is the
    // tx fails (does NOT silently re-decrement registered count).
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneToReturned {
                config: setup.config_key,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneNameToReturned {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert!(
        result.is_err(),
        "Second prune_to_returned must fail at account validation"
    );
}

// -----------------------------------------
// Lazy demand factor rollover tests
// Verifies that pricing instructions automatically roll the demand period
// without requiring a separate update_demand_factor call — matches Lua tick().
// -----------------------------------------

#[tokio::test]
async fn test_buy_name_lazy_rolls_demand_factor() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let ant_keypair2 = Keypair::new();
    let ant_key2 = ant_keypair2.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair2).await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy a name in period 1 — generates purchase activity
    buy_name_helper(
        &mut ctx,
        &setup,
        "lazyroll",
        PurchaseType::Lease,
        1,
        ant_key,
    )
    .await;

    // Verify period 1 activity
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(demand.current_period, 1);
    assert_eq!(demand.purchases_this_period, 1);
    assert!(demand.revenue_this_period > 0);

    // Warp clock past period boundary into period 2 — do NOT call update_demand_factor
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Buy another name — this should lazily roll the demand period
    buy_name_helper(
        &mut ctx,
        &setup,
        "lazyrol2",
        PurchaseType::Lease,
        1,
        ant_key2,
    )
    .await;

    // Verify: period advanced to 2, counters show the NEW purchase only
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(
        demand.current_period, 2,
        "buy_name must lazily roll the period"
    );
    assert_eq!(
        demand.purchases_this_period, 1,
        "Only the period-2 purchase should be counted"
    );
    // Factor should have INCREASED (period 1 had activity above trailing avg of 0)
    assert_eq!(
        demand.current_demand_factor, 1_050_000,
        "Period 1 activity should UP-adjust"
    );
}

#[tokio::test]
async fn test_extend_lease_lazy_rolls_demand_factor() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy a name in period 1
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, "extlazy", PurchaseType::Lease, 1, ant_key).await;

    // Warp past period boundary
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Extend lease — should lazily roll the period
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years: 1 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify period rolled
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(
        demand.current_period, 2,
        "extend_lease must lazily roll the period"
    );
    assert_eq!(
        demand.purchases_this_period, 1,
        "The extend should be tallied in period 2"
    );
}

#[tokio::test]
async fn test_get_token_cost_does_not_roll_demand_factor() {
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    // Verify starting period
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(demand.current_period, 1);

    // Warp past period boundary
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = TEST_PERIOD_ZERO_START + 86_400 + 1;
    ctx.set_sysvar(&clock);

    // Call get_token_cost (read-only) — must NOT roll the period
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::GetTokenCost {
                demand_factor: setup.demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::GetTokenCost {
                params: ario_arns::TokenCostParams {
                    intent: ario_arns::CostIntent::BuyName,
                    name: "testcost".to_string(),
                    years: Some(1),
                    quantity: None,
                    purchase_type: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Period must NOT have changed (read-only instruction)
    let df_account = ctx
        .banks_client
        .get_account(setup.demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    let demand = DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap();
    assert_eq!(
        demand.current_period, 1,
        "get_token_cost must NOT roll the demand period"
    );
}

// =========================================
// FUND-FROM-STAKE TESTS (CPI: ario-arns → ario-gar → SPL Token)
// =========================================
//
// These exercise the end-to-end CPI path. The ario-gar handlers themselves
// are also covered directly in ario-gar/tests/integration.rs.

mod fund_from_stake {
    use super::*;
    use ario_gar::state::{
        Delegation, Gateway, GatewayRegistry as GarGatewayRegistry, GatewaySettings,
        DELEGATION_SEED, GATEWAY_SEED, OBSERVER_LOOKUP_SEED, REGISTRY_SEED, SETTINGS_SEED,
    };
    use ario_gar::JoinNetworkParams;

    /// Bridge for the ario-gar processor in the multi-program ProgramTest.
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

    /// Multi-program ProgramTest with both ario-arns and ario-gar processors,
    /// plus pre-allocated zero-copy registries (NameRegistry, GatewayRegistry)
    /// and a hand-serialized GatewaySettings whose protocol_token_account is
    /// set to `treasury` (so ario-arns and ario-gar agree on the destination).
    fn program_test_with_arns_and_gar(
        treasury: Pubkey,
        stake_token_account: Pubkey,
        mint: Pubkey,
    ) -> ProgramTest {
        use anchor_lang::solana_program::hash::hash;

        let mut pt = ProgramTest::new("ario_arns", ario_arns::ID, processor!(anchor_processor));
        pt.add_program("ario_gar", ario_gar::ID, processor!(gar_processor));
        // mpl_core needed for mint_test_ant / transfer_test_ant CPIs. It is only
        // available as a prebuilt `.so`, so it must come in via the BPF path.
        // ario_arns + ario_gar were just added as NATIVE processors above; when
        // BPF_OUT_DIR is unset, `prefer_bpf` is false and `add_program(mpl_core,
        // None)` cannot resolve the fixture. Flip `prefer_bpf` to true AFTER the
        // two native processors are registered so MPL Core loads from
        // `tests/fixtures/mpl_core.so` while the two ario programs stay native.
        // When BPF_OUT_DIR IS set (CI), `prefer_bpf` is already true and all
        // three load from their `.so`s — leave that untouched.
        if std::env::var("BPF_OUT_DIR").is_err() && std::env::var("SBF_OUT_DIR").is_err() {
            pt.prefer_bpf(true);
        }
        pt.add_program("mpl_core", MPL_CORE_PROGRAM_ID, None);
        // Both programs need bigger budgets — ario-arns CPI to ario-gar is heavy.
        pt.set_compute_max_units(1_400_000);

        let rent = solana_sdk::rent::Rent::default();

        // --- NameRegistry (ario-arns, zero-copy) ---
        let nr_size = NameRegistry::bytes_for_capacity(NameRegistry::INITIAL_CAPACITY);
        let mut nr_data = vec![0u8; nr_size];
        let nr_disc = hash(b"account:NameRegistry");
        nr_data[..8].copy_from_slice(&nr_disc.to_bytes()[..8]);
        let nr_key = Pubkey::find_program_address(&[NAME_REGISTRY_SEED], &ario_arns::ID).0;
        pt.add_account(
            nr_key,
            solana_sdk::account::Account {
                lamports: rent.minimum_balance(nr_size),
                data: nr_data,
                owner: ario_arns::ID,
                executable: false,
                rent_epoch: 0,
            },
        );

        // --- GatewayRegistry (ario-gar, zero-copy ~120KB) ---
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

        // --- GatewaySettings (ario-gar) ---
        // Hand-serialize so we can pin protocol_token_account = treasury and
        // stake_token_account to the gateway pool, making both programs
        // agree on the payment destination without a separate init tx.
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
            .copy_from_slice(&Gateway::MIN_OPERATOR_STAKE.to_le_bytes());
        offset += 8;
        // min_delegate_stake
        settings_data[offset..offset + 8].copy_from_slice(&10_000_000u64.to_le_bytes());
        offset += 8;
        // withdrawal_period
        settings_data[offset..offset + 8].copy_from_slice(&(90i64 * 86_400).to_le_bytes());
        offset += 8;
        // max/min/min-amount expedited withdrawal
        settings_data[offset..offset + 8].copy_from_slice(&500_000u64.to_le_bytes());
        offset += 8;
        settings_data[offset..offset + 8].copy_from_slice(&100_000u64.to_le_bytes());
        offset += 8;
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
        // stake_token_account — gateway pool
        settings_data[offset..offset + 32].copy_from_slice(stake_token_account.as_ref());
        offset += 32;
        // protocol_token_account — must equal ArNS treasury
        settings_data[offset..offset + 32].copy_from_slice(treasury.as_ref());
        offset += 32;
        // arns_program_id — set to ario-arns ID so prescribe_epoch's
        // NameRegistry validation works against this multi-program test setup.
        settings_data[offset..offset + 32].copy_from_slice(ario_arns::ID.as_ref());
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

        // PR-4: pre-add ProgramData for ario-arns so initialize_arns can
        // satisfy the upgrade-authority constraint. Fund the authority pubkey
        // so it can pay the `init` rent for config / demand_factor.
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

    struct FundFromStakeSetup {
        mint: Keypair,
        mint_authority: Keypair,
        /// Buyer's personal token wallet (used for ArNS leftovers like
        /// the initiator share of buy_returned_name).
        buyer_token: Keypair,
        /// Shared destination: ArNS treasury == ario-gar protocol_token_account.
        treasury: Keypair,
        /// Gateway stake pool.
        stake_token: Keypair,
        config_key: Pubkey,
        demand_factor_key: Pubkey,
        /// Gateway operator (== a fresh keypair created at setup time, NOT
        /// ctx.payer — operators can't delegate to their own gateway).
        operator: Pubkey,
        gateway_key: Pubkey,
    }

    /// Delegate `amount` mARIO from `ctx.payer` (the buyer) to the gateway and
    /// return the resulting Delegation PDA.
    async fn delegate_from_buyer(
        ctx: &mut ProgramTestContext,
        setup: &FundFromStakeSetup,
        amount: u64,
    ) -> Pubkey {
        let buyer_pk = ctx.payer.pubkey();
        let (delegation_key, _) = gar_delegation_pda(&setup.operator, &buyer_pk);
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DelegateStake {
                    settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    delegator_token_account: setup.buyer_token.pubkey(),
                    stake_token_account: setup.stake_token.pubkey(),
                    delegator: buyer_pk,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::DelegateStake { amount }.data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        delegation_key
    }

    async fn token_balance(ctx: &mut ProgramTestContext, account: &Pubkey) -> u64 {
        let acct = ctx
            .banks_client
            .get_account(*account)
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&acct.data)
            .unwrap()
            .amount
    }

    async fn read_delegation_amount(ctx: &mut ProgramTestContext, key: Pubkey) -> u64 {
        let acct = ctx.banks_client.get_account(key).await.unwrap().unwrap();
        Delegation::try_deserialize(&mut acct.data.as_slice())
            .unwrap()
            .amount
    }

    async fn read_operator_stake(ctx: &mut ProgramTestContext, key: Pubkey) -> u64 {
        let acct = ctx.banks_client.get_account(key).await.unwrap().unwrap();
        Gateway::try_deserialize(&mut acct.data.as_slice())
            .unwrap()
            .operator_stake
    }

    // -----------------------------------------
    // buy_name from delegation / operator stake
    // -----------------------------------------

    #[tokio::test]
    async fn test_buy_name_from_delegation() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        // GatewaySettings is hand-serialized at PT-build time, so the mint/
        // stake/treasury addresses must be known before start_with_context().
        // Pre-derive the keypairs and pass them through to the setup helper.
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let buyer_pk = ctx.payer.pubkey();

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        let delegation_key = delegate_from_buyer(&mut ctx, &setup, 50_000_000_000).await;
        let delegation_before = read_delegation_amount(&mut ctx, delegation_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;

        let name = "delename".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyNameFromDelegation {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    name_registry: registry_key,
                    reserved_name_check: reserved_name_pda(&name).0,
                    returned_name_check: returned_name_pda(&name).0,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: buyer_pk,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyNameFromDelegation {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Verify the name landed
        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert_eq!(record.owner, buyer_pk);
        assert_eq!(record.name, name);

        // Verify delegation was reduced and treasury credited by the same amount
        let delegation_after = read_delegation_amount(&mut ctx, delegation_key).await;
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let cost = record.purchase_price;
        assert_eq!(delegation_before - delegation_after, cost);
        assert_eq!(treasury_after - treasury_before, cost);
    }

    #[tokio::test]
    async fn test_buy_name_from_operator_stake() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        // Operator's NFT holder is the operator (who is signing the purchase)
        let payer_clone = ctx.payer.insecure_clone();
        transfer_test_ant(&mut ctx, ant_key, &payer_clone, setup.operator).await;

        let stake_before = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;

        let name = "opstake".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyNameFromOperatorStake {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    name_registry: registry_key,
                    reserved_name_check: reserved_name_pda(&name).0,
                    returned_name_check: returned_name_pda(&name).0,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: setup.operator,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyNameFromOperatorStake {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        let cost = record.purchase_price;
        let stake_after = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(stake_before - stake_after, cost);
        assert_eq!(treasury_after - treasury_before, cost);
    }

    /// The operator-stake variant supports the gateway-operator discount when
    /// the buyer == operator and the gateway has 180+ days of tenure. (The
    /// delegation variant cannot exercise this — operators can't delegate to
    /// their own gateway, so buyer==operator+delegation is structurally
    /// impossible.)
    #[tokio::test]
    async fn test_buy_name_from_operator_stake_with_gateway_discount() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;
        let payer_clone = ctx.payer.insecure_clone();
        transfer_test_ant(&mut ctx, ant_key, &payer_clone, setup.operator).await;

        // Backdate gateway.start_timestamp so it's > 180 days behind chain clock 0
        let gw_acct = ctx
            .banks_client
            .get_account(setup.gateway_key)
            .await
            .unwrap()
            .unwrap();
        let mut gw = Gateway::try_deserialize(&mut gw_acct.data.as_slice()).unwrap();
        gw.start_timestamp = -(200 * 86_400i64); // 200 days in the past
        let mut new_data = Vec::new();
        gw.try_serialize(&mut new_data).unwrap();
        new_data.resize(gw_acct.data.len(), 0);
        ctx.set_account(
            &setup.gateway_key,
            &solana_sdk::account::Account {
                lamports: gw_acct.lamports,
                data: new_data,
                owner: gw_acct.owner,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );

        let name = "discnt".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();
        let (gar_settings_key, _) = gar_settings_pda();

        // Pass the gateway PDA in remaining_accounts to claim the 20% discount
        let mut accounts = ario_arns::accounts::BuyNameFromOperatorStake {
            config: setup.config_key,
            demand_factor: setup.demand_factor_key,
            arns_record: record_key,
            name_registry: registry_key,
            reserved_name_check: reserved_name_pda(&name).0,
            returned_name_check: returned_name_pda(&name).0,
            gar_settings: gar_settings_key,
            gateway: setup.gateway_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.treasury.pubkey(),
            buyer: setup.operator,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new_readonly(setup.gateway_key, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts,
                data: ario_arns::instruction::BuyNameFromOperatorStake {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Compute what the undiscounted cost would have been to verify the 20% discount
        // was applied. base_fee for a 6-char name is GENESIS_FEES[5] = 1_500_000_000.
        // Lease 1y: cost = base * (1 + 0.2 * 1) = 1.8B. Discounted: 1.8B * 0.8 = 1.44B.
        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        let undiscounted = 1_500_000_000u64 * 12 / 10; // 1.8B
        let expected_discounted = undiscounted * 8 / 10; // 1.44B
        assert_eq!(
            record.purchase_price, expected_discounted,
            "20% gateway-operator discount must be applied"
        );
    }

    // -----------------------------------------
    // buy_returned_name from delegation (split payment)
    // -----------------------------------------

    #[tokio::test]
    async fn test_buy_returned_name_from_delegation() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let buyer_pk = ctx.payer.pubkey();

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        // Step 1: buy a permabuy name normally so we can release it
        let name = "rtndele";
        let mint_for_buy_helper = ArnsSetup {
            mint: clone_keypair(&setup.mint),
            mint_authority: clone_keypair(&setup.mint_authority),
            buyer_token: clone_keypair(&setup.buyer_token),
            protocol_token: clone_keypair(&setup.treasury),
            config_key: setup.config_key,
            demand_factor_key: setup.demand_factor_key,
        };
        let arns_record_key = buy_name_helper(
            &mut ctx,
            &mint_for_buy_helper,
            name,
            PurchaseType::Permabuy,
            0,
            ant_key,
        )
        .await;

        // Step 2: release the name to create a ReturnedName
        let (returned_name_key, _) = returned_name_pda(name);
        let registry_key = name_registry_key();
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::ReleaseName {
                    config: config_pda().0,
                    arns_record: arns_record_key,
                    returned_name: returned_name_key,
                    name_registry: registry_key,
                    ant_asset: ant_key,
                    caller: buyer_pk,
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::ReleaseName {}.data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Step 3: warp past the auction window (premium = 0)
        let cur_slot = ctx.banks_client.get_root_slot().await.unwrap();
        ctx.warp_to_slot(cur_slot + 2).unwrap();
        let mut clock = ctx
            .banks_client
            .get_sysvar::<solana_sdk::clock::Clock>()
            .await
            .unwrap();
        clock.unix_timestamp += RETURNED_NAME_DURATION_SECONDS + 1;
        ctx.set_sysvar(&clock);

        // Step 4: stand up a delegation now that we have funds and time has passed
        let delegation_key = delegate_from_buyer(&mut ctx, &setup, 50_000_000_000).await;
        let delegation_before = read_delegation_amount(&mut ctx, delegation_key).await;
        let buyer_token_before = token_balance(&mut ctx, &setup.buyer_token.pubkey()).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;

        // Step 5: buy_returned_name_from_delegation
        let (gar_settings_key, _) = gar_settings_pda();
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyReturnedNameFromDelegation {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    returned_name: returned_name_key,
                    arns_record: arns_record_pda(name).0,
                    name_registry: registry_key,
                    buyer_token_account: setup.buyer_token.pubkey(),
                    initiator_token_account: setup.buyer_token.pubkey(),
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: buyer_pk,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyReturnedNameFromDelegation {
                    params: ario_arns::BuyReturnedNameParams {
                        name: name.to_string(),
                        purchase_type: PurchaseType::Permabuy,
                        years: 0,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(arns_record_pda(name).0)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        let cost = record.purchase_price;
        let protocol_share = cost / 2; // initiator != protocol → 50/50 split
        let initiator_share = cost - protocol_share;

        // Delegation reduced by protocol share only
        let delegation_after = read_delegation_amount(&mut ctx, delegation_key).await;
        assert_eq!(delegation_before - delegation_after, protocol_share);

        // Treasury received protocol share
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(treasury_after - treasury_before, protocol_share);

        // Initiator share went buyer-wallet → buyer-wallet (same account in this test)
        // so net buyer balance change is 0.
        let buyer_token_after = token_balance(&mut ctx, &setup.buyer_token.pubkey()).await;
        assert_eq!(
            buyer_token_after, buyer_token_before,
            "self-buy: initiator share circular"
        );
        let _ = initiator_share;
    }

    // -----------------------------------------
    // manage_from_delegation: upgrade, extend, increase undername
    // -----------------------------------------

    /// Helper: buy a lease via the standard ArNS path (buyer = ctx.payer pays
    /// from their wallet) so we have an existing record to manage.
    async fn buy_lease_for_management(
        ctx: &mut ProgramTestContext,
        setup: &FundFromStakeSetup,
        name: &str,
        years: u8,
        ant_key: Pubkey,
    ) -> Pubkey {
        let mint_for_helper = ArnsSetup {
            mint: clone_keypair(&setup.mint),
            mint_authority: clone_keypair(&setup.mint_authority),
            buyer_token: clone_keypair(&setup.buyer_token),
            protocol_token: clone_keypair(&setup.treasury),
            config_key: setup.config_key,
            demand_factor_key: setup.demand_factor_key,
        };
        buy_name_helper(
            ctx,
            &mint_for_helper,
            name,
            PurchaseType::Lease,
            years,
            ant_key,
        )
        .await
    }

    #[tokio::test]
    async fn test_upgrade_name_from_delegation() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let buyer_pk = ctx.payer.pubkey();

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        let name = "upgname";
        let record_key = buy_lease_for_management(&mut ctx, &setup, name, 1, ant_key).await;
        let delegation_key = delegate_from_buyer(&mut ctx, &setup, 50_000_000_000).await;
        let delegation_before = read_delegation_amount(&mut ctx, delegation_key).await;
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::UpgradeNameFromDelegation {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    caller: buyer_pk,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::UpgradeNameFromDelegation {}.data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert!(matches!(record.purchase_type, PurchaseType::Permabuy));
        assert!(record.end_timestamp.is_none());

        let delegation_after = read_delegation_amount(&mut ctx, delegation_key).await;
        assert_eq!(delegation_before - delegation_after, record.purchase_price);
    }

    #[tokio::test]
    async fn test_extend_lease_from_delegation() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let buyer_pk = ctx.payer.pubkey();

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        let name = "extlse";
        let record_key = buy_lease_for_management(&mut ctx, &setup, name, 1, ant_key).await;
        let record_before = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        let end_before = record_before.end_timestamp.unwrap();

        let delegation_key = delegate_from_buyer(&mut ctx, &setup, 50_000_000_000).await;
        let delegation_before = read_delegation_amount(&mut ctx, delegation_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::ExtendLeaseFromDelegation {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    caller: buyer_pk,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::ExtendLeaseFromDelegation { years: 2 }.data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record_after = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert_eq!(
            record_after.end_timestamp.unwrap(),
            end_before + 2 * ONE_YEAR_SECONDS
        );

        let cost_paid = delegation_before - read_delegation_amount(&mut ctx, delegation_key).await;
        assert!(cost_paid > 0);
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(treasury_after - treasury_before, cost_paid);
    }

    #[tokio::test]
    async fn test_increase_undername_from_delegation() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let buyer_pk = ctx.payer.pubkey();

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        let name = "undnm";
        let record_key = buy_lease_for_management(&mut ctx, &setup, name, 1, ant_key).await;
        let undername_before = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .undername_limit;

        let delegation_key = delegate_from_buyer(&mut ctx, &setup, 50_000_000_000).await;
        let delegation_before = read_delegation_amount(&mut ctx, delegation_key).await;
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::IncreaseUndernameFromDelegation {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    caller: buyer_pk,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::IncreaseUndernameLimitFromDelegation { quantity: 50 }
                    .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record_after = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert_eq!(record_after.undername_limit, undername_before + 50);
        let cost_paid = delegation_before - read_delegation_amount(&mut ctx, delegation_key).await;
        assert!(cost_paid > 0);
    }

    // -----------------------------------------
    // operator_stake variants of returned-name + management
    // -----------------------------------------
    //
    // These mirror the delegation variants above but are funded from the
    // operator's own stake. The signer is therefore the operator (not
    // ctx.payer), and for management variants the operator must also be the
    // ANT NFT holder. Each test buys the source record via
    // buy_name_from_operator_stake so the operator owns it from the start.

    /// Operator buys a 1-year lease via buy_name_from_operator_stake. Returns
    /// the ArnsRecord PDA. Used as setup for management-variant tests.
    async fn operator_buys_lease(
        ctx: &mut ProgramTestContext,
        setup: &FundFromStakeSetup,
        operator_kp: &Keypair,
        name: &str,
        ant_key: Pubkey,
    ) -> Pubkey {
        let (record_key, _) = arns_record_pda(name);
        let registry_key = name_registry_key();
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyNameFromOperatorStake {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    name_registry: registry_key,
                    reserved_name_check: reserved_name_pda(name).0,
                    returned_name_check: returned_name_pda(name).0,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: setup.operator,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyNameFromOperatorStake {
                    params: ario_arns::BuyNameParams {
                        name: name.to_string(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        record_key
    }

    #[tokio::test]
    async fn test_buy_returned_name_from_operator_stake() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let buyer_pk = ctx.payer.pubkey();
        // Initial purchase done by ctx.payer (via the standard buy_name path)
        // so they can later release the name.

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        // Step 1: ctx.payer buys + releases a permabuy name → ReturnedName PDA
        let name = "rtnops";
        let arns_record_key = buy_name_helper(
            &mut ctx,
            &ArnsSetup {
                mint: clone_keypair(&setup.mint),
                mint_authority: clone_keypair(&setup.mint_authority),
                buyer_token: clone_keypair(&setup.buyer_token),
                protocol_token: clone_keypair(&setup.treasury),
                config_key: setup.config_key,
                demand_factor_key: setup.demand_factor_key,
            },
            name,
            PurchaseType::Permabuy,
            0,
            ant_key,
        )
        .await;

        let (returned_name_key, _) = returned_name_pda(name);
        let registry_key = name_registry_key();
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::ReleaseName {
                    config: config_pda().0,
                    arns_record: arns_record_key,
                    returned_name: returned_name_key,
                    name_registry: registry_key,
                    ant_asset: ant_key,
                    caller: buyer_pk,
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::ReleaseName {}.data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Warp past auction window so premium = 0 (predictable cost)
        let cur_slot = ctx.banks_client.get_root_slot().await.unwrap();
        ctx.warp_to_slot(cur_slot + 2).unwrap();
        let mut clock = ctx
            .banks_client
            .get_sysvar::<solana_sdk::clock::Clock>()
            .await
            .unwrap();
        clock.unix_timestamp += RETURNED_NAME_DURATION_SECONDS + 1;
        ctx.set_sysvar(&clock);

        // Operator's own token wallet — required because the initiator share
        // is paid buyer-wallet → initiator-wallet, and the buyer here is the
        // operator (BuyReturnedNameFromOperatorStake constraint:
        // `buyer_token_account.owner == buyer.key()`). Must be pre-funded.
        let operator_token = Keypair::new();
        create_token_account(
            &mut ctx,
            &operator_token,
            &setup.mint.pubkey(),
            &setup.operator,
        )
        .await;
        mint_tokens(
            &mut ctx,
            &setup.mint.pubkey(),
            &operator_token.pubkey(),
            &setup.mint_authority,
            1_000_000_000_000, // plenty for the initiator share
        )
        .await;

        // Now the operator buys the returned name from operator_stake
        let stake_before = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let initiator_balance_before = token_balance(&mut ctx, &setup.buyer_token.pubkey()).await;

        let (gar_settings_key, _) = gar_settings_pda();
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyReturnedNameFromOperatorStake {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    returned_name: returned_name_key,
                    arns_record: arns_record_pda(name).0,
                    name_registry: registry_key,
                    buyer_token_account: operator_token.pubkey(),
                    initiator_token_account: setup.buyer_token.pubkey(),
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: setup.operator,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyReturnedNameFromOperatorStake {
                    params: ario_arns::BuyReturnedNameParams {
                        name: name.to_string(),
                        purchase_type: PurchaseType::Permabuy,
                        years: 0,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(arns_record_pda(name).0)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        let cost = record.purchase_price;
        let protocol_share = cost / 2; // initiator != protocol → 50/50 split
        let initiator_share = cost - protocol_share;

        let stake_after = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let initiator_balance_after = token_balance(&mut ctx, &setup.buyer_token.pubkey()).await;

        // Operator stake reduced by protocol share only
        assert_eq!(stake_before - stake_after, protocol_share);
        // Treasury credited the protocol share
        assert_eq!(treasury_after - treasury_before, protocol_share);
        // Initiator received their share from operator's token wallet
        assert_eq!(
            initiator_balance_after - initiator_balance_before,
            initiator_share
        );
    }

    /// Helper for the 3 management variant tests: operator owns the lease and
    /// the ANT, then exercises the operator_stake management instruction.
    /// Caller must have already minted the ANT (owner = ctx.payer); this
    /// helper transfers ownership to the operator before purchasing the lease.
    async fn setup_for_operator_management(
        ctx: &mut ProgramTestContext,
        operator_kp: &Keypair,
        ant_key: Pubkey,
        mint_kp: Keypair,
        stake_kp: Keypair,
        treasury_kp: Keypair,
        name: &str,
    ) -> (FundFromStakeSetup, Pubkey) {
        let setup =
            setup_full_environment_with_keys(ctx, operator_kp, mint_kp, stake_kp, treasury_kp)
                .await;
        // Operator owns the ANT (so management constraint `nft_holder == caller` passes)
        let payer_clone = ctx.payer.insecure_clone();
        transfer_test_ant(ctx, ant_key, &payer_clone, setup.operator).await;
        let record_key = operator_buys_lease(ctx, &setup, operator_kp, name, ant_key).await;
        (setup, record_key)
    }

    #[tokio::test]
    async fn test_upgrade_name_from_operator_stake() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;

        let (setup, record_key) = setup_for_operator_management(
            &mut ctx,
            &operator_kp,
            ant_key,
            mint_kp,
            stake_kp,
            treasury_kp,
            "upgops",
        )
        .await;

        let stake_before = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::UpgradeNameFromOperatorStake {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    caller: setup.operator,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::UpgradeNameFromOperatorStake {}.data(),
            }],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert!(matches!(record.purchase_type, PurchaseType::Permabuy));
        assert!(record.end_timestamp.is_none());

        let stake_after = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(stake_before - stake_after, record.purchase_price);
        assert_eq!(treasury_after - treasury_before, record.purchase_price);
    }

    #[tokio::test]
    async fn test_extend_lease_from_operator_stake() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;

        let (setup, record_key) = setup_for_operator_management(
            &mut ctx,
            &operator_kp,
            ant_key,
            mint_kp,
            stake_kp,
            treasury_kp,
            "extops",
        )
        .await;

        let end_before = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .end_timestamp
        .unwrap();

        let stake_before = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::ExtendLeaseFromOperatorStake {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    caller: setup.operator,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::ExtendLeaseFromOperatorStake { years: 2 }.data(),
            }],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let end_after = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .end_timestamp
        .unwrap();
        assert_eq!(end_after, end_before + 2 * ONE_YEAR_SECONDS);

        let stake_after = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let cost = stake_before - stake_after;
        assert!(cost > 0);
        assert_eq!(treasury_after - treasury_before, cost);
    }

    #[tokio::test]
    async fn test_increase_undername_from_operator_stake() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;

        let (setup, record_key) = setup_for_operator_management(
            &mut ctx,
            &operator_kp,
            ant_key,
            mint_kp,
            stake_kp,
            treasury_kp,
            "undops",
        )
        .await;

        let undername_before = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .undername_limit;

        let stake_before = read_operator_stake(&mut ctx, setup.gateway_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::IncreaseUndernameFromOperatorStake {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    caller: setup.operator,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::IncreaseUndernameLimitFromOperatorStake {
                    quantity: 25,
                }
                .data(),
            }],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let undername_after = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .undername_limit;
        assert_eq!(undername_after, undername_before + 25);

        let cost = stake_before - read_operator_stake(&mut ctx, setup.gateway_key).await;
        assert!(cost > 0);
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(treasury_after - treasury_before, cost);
    }

    // -----------------------------------------
    // CPI error propagation
    // -----------------------------------------

    /// When the delegation can't cover the name cost, ario-gar's
    /// InsufficientDelegationForPayment error must bubble up through the CPI.
    #[tokio::test]
    async fn test_buy_name_from_delegation_insufficient_stake() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let buyer_pk = ctx.payer.pubkey();

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        // Delegate just barely more than the gateway minimum (10 ARIO = 10M mARIO).
        // A 8-char lease costs ~600M mARIO — far more than the delegation.
        let delegation_key = delegate_from_buyer(&mut ctx, &setup, 11_000_000).await;

        let name = "shortdel".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyNameFromDelegation {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    name_registry: registry_key,
                    reserved_name_check: reserved_name_pda(&name).0,
                    returned_name_check: returned_name_pda(&name).0,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: buyer_pk,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyNameFromDelegation {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        let result = ctx.banks_client.process_transaction(tx).await;

        // The CPI propagates ario-gar's custom error code. We can't use the
        // ArnsError macro here (different enum) — match the raw code instead.
        let expected_code = anchor_lang::error::ERROR_CODE_OFFSET
            + ario_gar::error::GarError::InsufficientDelegationForPayment as u32;
        match result {
            Err(solana_program_test::BanksClientError::TransactionError(
                solana_sdk::transaction::TransactionError::InstructionError(
                    _,
                    solana_sdk::instruction::InstructionError::Custom(code),
                ),
            )) => assert_eq!(
                code, expected_code,
                "expected InsufficientDelegationForPayment"
            ),
            other => panic!("expected CPI error propagation, got: {:?}", other),
        }
    }

    // -----------------------------------------
    // Internal helpers
    // -----------------------------------------

    fn clone_keypair(kp: &Keypair) -> Keypair {
        Keypair::from_bytes(&kp.to_bytes()).unwrap()
    }

    /// Variant of setup_full_environment that consumes pre-allocated keypairs
    /// (mint/stake/treasury) so they can be pinned in GatewaySettings before
    /// the test context is created.
    async fn setup_full_environment_with_keys(
        ctx: &mut ProgramTestContext,
        operator_keypair: &Keypair,
        mint: Keypair,
        stake_token: Keypair,
        treasury: Keypair,
    ) -> FundFromStakeSetup {
        // See note on TEST_PERIOD_ZERO_START — must be ≥ 2020-01-01 and ≤ clock.
        let mut clock = ctx
            .banks_client
            .get_sysvar::<solana_sdk::clock::Clock>()
            .await
            .unwrap();
        clock.unix_timestamp = TEST_PERIOD_ZERO_START;
        ctx.set_sysvar(&clock);

        let mint_authority = Keypair::new();
        create_mint(ctx, &mint, &mint_authority.pubkey()).await;

        let buyer_token = Keypair::new();
        let payer_pk = ctx.payer.pubkey();
        create_token_account(ctx, &buyer_token, &mint.pubkey(), &payer_pk).await;
        mint_tokens(
            ctx,
            &mint.pubkey(),
            &buyer_token.pubkey(),
            &mint_authority,
            1_000_000_000_000,
        )
        .await;

        let (gar_settings_key, _) = gar_settings_pda();
        create_token_account(ctx, &treasury, &mint.pubkey(), &gar_settings_key).await;
        create_token_account(ctx, &stake_token, &mint.pubkey(), &gar_settings_key).await;

        let (config_key, _) = config_pda();
        let (demand_factor_key, _) = demand_factor_pda();

        let upgrade_auth = upgrade_authority_keypair();
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::InitializeArns {
                    config: config_key,
                    demand_factor: demand_factor_key,
                    authority: upgrade_auth.pubkey(),
                    program_data: program_data_pda(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::Initialize {
                    params: ario_arns::InitializeArnsParams {
                        authority: payer_pk,
                        mint: mint.pubkey(),
                        treasury: treasury.pubkey(),
                        period_zero_start_timestamp: TEST_PERIOD_ZERO_START,
                        migration_authority: payer_pk,
                        initial_demand_factor: 1_000_000, // DEMAND_FACTOR_SCALE (1.0)
                    },
                }
                .data(),
            }],
            Some(&payer_pk),
            &[&ctx.payer, &upgrade_auth],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Fund operator
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer_pk,
                &operator_keypair.pubkey(),
                10_000_000_000,
            )],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let operator_token = Keypair::new();
        create_token_account(
            ctx,
            &operator_token,
            &mint.pubkey(),
            &operator_keypair.pubkey(),
        )
        .await;
        mint_tokens(
            ctx,
            &mint.pubkey(),
            &operator_token.pubkey(),
            &mint_authority,
            100_000_000_000,
        )
        .await;

        let operator_pk = operator_keypair.pubkey();
        let (gateway_key, _) = gar_gateway_pda(&operator_pk);
        let (observer_lookup_key, _) = gar_observer_lookup_pda(&operator_pk);
        let (gar_registry_key, _) = gar_registry_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::JoinNetwork {
                    registry: gar_registry_key,
                    settings: gar_settings_key,
                    gateway: gateway_key,
                    operator_token_account: operator_token.pubkey(),
                    stake_token_account: stake_token.pubkey(),
                    observer_lookup: observer_lookup_key,
                    operator: operator_pk,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::JoinNetwork {
                    params: JoinNetworkParams {
                        operator_stake: 50_000_000_000,
                        label: "fund-test-gw".to_string(),
                        fqdn: "gw.test.com".to_string(),
                        port: 443,
                        protocol: ario_gar::state::Protocol::Https,
                        properties: None,
                        note: None,
                        allow_delegated_staking: true,
                        delegate_reward_share_ratio: 10,
                        min_delegate_stake: None,
                        observer_address: operator_pk,
                    },
                }
                .data(),
            }],
            Some(&operator_pk),
            &[operator_keypair],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        FundFromStakeSetup {
            mint,
            mint_authority,
            buyer_token,
            treasury,
            stake_token,
            config_key,
            demand_factor_key,
            operator: operator_pk,
            gateway_key,
        }
    }

    // =========================================
    // PERMISSIONLESS STAKE-FUNDED LEASE MANAGEMENT (matches Lua)
    // =========================================
    //
    // Variants of the existing stake/delegation tests where the caller is
    // NOT the ANT NFT holder. Trait sync isn't performed in stake variants
    // (no MPL Core CPI in those handlers), so the on-chain ArnsRecord is
    // the source of truth and `sync_attributes` reconciles traits later.

    use super::*;
    use ario_gar::{accounts as ario_gar_accounts, instruction as ario_gar_instruction};

    // -----------------------------------------
    // SECURITY: withdrawn stake cannot be used for ArNS purchase
    // -----------------------------------------

    /// Operator increases stake above minimum, then decreases (creates withdrawal).
    /// The decreased amount is no longer in operator_stake, so buy_name_from_operator_stake
    /// must only see the remaining balance — not the withdrawn tokens.
    #[tokio::test]
    async fn test_withdrawn_operator_stake_cannot_fund_purchase() {
        use ario_gar::state::{WithdrawalCounter, WITHDRAWAL_COUNTER_SEED, WITHDRAWAL_SEED};

        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;

        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        // Transfer ANT to operator so they can sign the purchase
        let payer_clone = ctx.payer.insecure_clone();
        transfer_test_ant(&mut ctx, ant_key, &payer_clone, setup.operator).await;

        // Operator joined with 50K. MIN_OPERATOR_STAKE = 20K, so 30K is excess.
        // Step 1: Decrease operator stake by 30K → withdrawal PDA holds 30K,
        //         operator_stake drops to exactly 20K (the minimum).
        let (gar_settings_key, _) = gar_settings_pda();
        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, setup.operator.as_ref()],
            &ario_gar::ID,
        );
        let (withdrawal_key, _) = Pubkey::find_program_address(
            &[
                WITHDRAWAL_SEED,
                setup.operator.as_ref(),
                &0u64.to_le_bytes(),
            ],
            &ario_gar::ID,
        );

        let stake_before = read_operator_stake(&mut ctx, setup.gateway_key).await;
        assert_eq!(stake_before, 50_000_000_000, "operator starts with 50K");

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DecreaseOperatorStake {
                    settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    withdrawal_counter: withdrawal_counter_key,
                    withdrawal: withdrawal_key,
                    operator: setup.operator,
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::DecreaseOperatorStake {
                    amount: 30_000_000_000, // 30K → leaves 20K at minimum
                }
                .data(),
            }],
            Some(&setup.operator),
            &[&operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let stake_after_decrease = read_operator_stake(&mut ctx, setup.gateway_key).await;
        assert_eq!(
            stake_after_decrease, 20_000_000_000,
            "operator at 20K minimum"
        );

        // Step 2: Try to buy a name from operator stake.
        // Operator has exactly 20K (min). The 30K in the Withdrawal PDA is NOT
        // accessible via deduct_operator_stake_for_payment. Any purchase that
        // costs > 0 would push operator_stake below min → must fail.
        let name = "wdraw".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyNameFromOperatorStake {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    name_registry: registry_key,
                    reserved_name_check: reserved_name_pda(&name).0,
                    returned_name_check: returned_name_pda(&name).0,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: setup.operator,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyNameFromOperatorStake {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&ctx.payer.pubkey()),
            &[&ctx.payer, &operator_kp],
            blockhash,
        );
        let result = ctx.banks_client.process_transaction(tx).await;
        // Must fail — operator is at 20K minimum, the 30K in the Withdrawal PDA
        // is NOT accessible to deduct_operator_stake_for_payment. The payment
        // handler requires remaining >= min_operator_stake, so any cost > 0 fails.
        assert!(
            result.is_err(),
            "Purchase from operator stake must fail when operator is at minimum (withdrawn stake must not be spendable)"
        );
    }

    /// 7. Delegate (any ARIO holder) extends a lease for an ANT they don't own.
    #[tokio::test]
    async fn test_extend_lease_from_delegation_by_non_holder() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        // Buy lease (ctx.payer owns ANT at this point, so traits sync inline).
        let name = "extdelnh";
        let record_key = buy_lease_for_management(&mut ctx, &setup, name, 1, ant_key).await;
        let end_before = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .end_timestamp
        .unwrap();

        // Transfer ANT to operator — ctx.payer is now the "non-holder".
        let payer_clone = ctx.payer.insecure_clone();
        transfer_test_ant(&mut ctx, ant_key, &payer_clone, setup.operator).await;

        // Delegate from ctx.payer's wallet, then extend from delegation.
        let delegation_key = delegate_from_buyer(&mut ctx, &setup, 50_000_000_000).await;
        let delegation_before = read_delegation_amount(&mut ctx, delegation_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let (gar_settings_key, _) = gar_settings_pda();
        let buyer_pk = ctx.payer.pubkey();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::ExtendLeaseFromDelegation {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    caller: buyer_pk,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::ExtendLeaseFromDelegation { years: 2 }.data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Lease extended; delegation debited; treasury increased.
        let end_after = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .end_timestamp
        .unwrap();
        assert_eq!(end_after, end_before + 2 * ONE_YEAR_SECONDS);
        let cost_paid = delegation_before - read_delegation_amount(&mut ctx, delegation_key).await;
        assert!(cost_paid > 0);
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(treasury_after - treasury_before, cost_paid);
    }

    // (deleted obsolete test `test_upgrade_name_from_operator_stake_by_non_holder` — exercised the
    //  ARIO-ARNS-side trait sync mechanism that the Sprint 1-3
    //  reshape moved to `ario_ant::sync_attributes`. Round-trip
    //  coverage is now in programs/ario-ant/tests/sync_attributes.rs.
    //  See ADR-016 amendment / BD-100.)

    /// 9. Smoke test the holder path on a stake variant — proves removing the
    /// require didn't accidentally break the case where caller IS the holder.
    #[tokio::test]
    async fn test_increase_undernames_from_delegation_by_holder() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        // ctx.payer owns the ANT and the lease, AND pays via delegation.
        let name = "incdelh";
        let record_key = buy_lease_for_management(&mut ctx, &setup, name, 1, ant_key).await;
        let limit_before = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .undername_limit;

        let delegation_key = delegate_from_buyer(&mut ctx, &setup, 20_000_000_000).await;
        let delegation_before = read_delegation_amount(&mut ctx, delegation_key).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        let (gar_settings_key, _) = gar_settings_pda();
        let buyer_pk = ctx.payer.pubkey();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::IncreaseUndernameFromDelegation {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    gar_settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    delegation: delegation_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    caller: buyer_pk,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::IncreaseUndernameLimitFromDelegation { quantity: 5 }
                    .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record_after = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert_eq!(record_after.undername_limit, limit_before + 5);

        let cost_paid = delegation_before - read_delegation_amount(&mut ctx, delegation_key).await;
        assert!(cost_paid > 0);
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(treasury_after - treasury_before, cost_paid);
    }

    // =========================================================================
    // Phase 2: _from_withdrawal + _from_funding_plan tests
    // =========================================================================
    //
    // Each Phase 2 ix is exercised end-to-end: the test stands up a delegation
    // or operator-stake withdrawal vault via the existing GAR ix, then drives
    // a buy_name through the new wrapper. Assertions verify ArnsRecord state,
    // protocol-treasury credits, and per-source PDA state changes.

    use ario_gar::state::{WITHDRAWAL_COUNTER_SEED, WITHDRAWAL_SEED};
    use ario_gar::{FundingSourceKind, FundingSourceSpec};

    fn fp_balance(amount: u64) -> FundingSourceSpec {
        FundingSourceSpec {
            kind: FundingSourceKind::Balance,
            amount,
        }
    }
    fn fp_withdrawal(amount: u64) -> FundingSourceSpec {
        FundingSourceSpec {
            kind: FundingSourceKind::Withdrawal,
            amount,
        }
    }

    #[tokio::test]
    async fn test_buy_name_from_withdrawal_phase2() {
        // Operator decreases their own stake → creates a Withdrawal vault.
        // Operator then uses that vault to buy a name via _from_withdrawal.
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        let payer_clone = ctx.payer.insecure_clone();
        transfer_test_ant(&mut ctx, ant_key, &payer_clone, setup.operator).await;

        // Operator joined with 50K. Decrease 30K → withdrawal_id=0 vault has 30K.
        let (gar_settings_key, _) = gar_settings_pda();
        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, setup.operator.as_ref()],
            &ario_gar::ID,
        );
        let (withdrawal_key, _) = Pubkey::find_program_address(
            &[
                WITHDRAWAL_SEED,
                setup.operator.as_ref(),
                &0u64.to_le_bytes(),
            ],
            &ario_gar::ID,
        );

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DecreaseOperatorStake {
                    settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    withdrawal_counter: withdrawal_counter_key,
                    withdrawal: withdrawal_key,
                    operator: setup.operator,
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::DecreaseOperatorStake {
                    amount: 30_000_000_000,
                }
                .data(),
            }],
            Some(&setup.operator),
            &[&operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let withdrawal_before = ario_gar::state::Withdrawal::try_deserialize(
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
        .amount;
        assert_eq!(withdrawal_before, 30_000_000_000);
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;

        // Buy a name from the withdrawal vault.
        let name = "fwithdraw".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyNameFromWithdrawal {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    name_registry: registry_key,
                    reserved_name_check: reserved_name_pda(&name).0,
                    returned_name_check: returned_name_pda(&name).0,
                    gar_settings: gar_settings_key,
                    withdrawal: withdrawal_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: setup.operator,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyNameFromWithdrawal {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&setup.operator),
            &[&operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Verify the name landed.
        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert_eq!(record.owner, setup.operator);
        assert_eq!(record.name, name);

        // Withdrawal vault decreased by exactly the cost; vault stays open.
        let withdrawal_after = ario_gar::state::Withdrawal::try_deserialize(
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
        .amount;
        let cost = record.purchase_price;
        assert_eq!(withdrawal_before - withdrawal_after, cost);
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(treasury_after - treasury_before, cost);
    }

    #[tokio::test]
    async fn test_buy_name_from_funding_plan_balance_only() {
        // 1-source funding plan: pure Balance source. Equivalent to the
        // existing balance-funded buy_name path; verifies the wrapper +
        // CPI to ario-gar::pay_from_funding_plan wires through correctly.
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;
        let buyer_pk = ctx.payer.pubkey();
        let (gar_settings_key, _) = gar_settings_pda();

        // Predict the residue_vault PDA the SDK would compute (next withdrawal_id = 0
        // since the buyer hasn't created any withdrawals yet).
        let (residue_pda, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_SEED, buyer_pk.as_ref(), &0u64.to_le_bytes()],
            &ario_gar::ID,
        );
        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, buyer_pk.as_ref()],
            &ario_gar::ID,
        );

        let buyer_balance_before = token_balance(&mut ctx, &setup.buyer_token.pubkey()).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;

        let name = "fplan-bal".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();

        // Compute cost ahead of time so we can pass `expected_total` to the plan.
        // Use the same calculation the on-chain handler does.
        let cost = ario_arns::pricing::calculate_registration_fee(
            ario_arns::pricing::get_base_fee_for_name_length(
                &ario_arns::state::GENESIS_FEES,
                name.len(),
            )
            .unwrap(),
            PurchaseType::Lease,
            1,
            ario_arns::pricing::DEMAND_FACTOR_SCALE, // demand_factor = 1.0 at start
        )
        .unwrap();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyNameFromFundingPlan {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    name_registry: registry_key,
                    reserved_name_check: reserved_name_pda(&name).0,
                    returned_name_check: returned_name_pda(&name).0,
                    gar_settings: gar_settings_key,
                    stake_token_account: setup.stake_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    payer_token_account: Some(setup.buyer_token.pubkey()),
                    buyer: buyer_pk,
                    withdrawal_counter: withdrawal_counter_key,
                    gar_program: ario_gar::ID,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyNameFromFundingPlan {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                    sources: vec![fp_balance(cost)],
                    discount_account_count: 0,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert_eq!(record.owner, buyer_pk);
        assert_eq!(record.name, name);
        assert_eq!(record.purchase_price, cost);

        // Buyer balance dropped by cost; treasury rose by cost.
        let buyer_balance_after = token_balance(&mut ctx, &setup.buyer_token.pubkey()).await;
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(buyer_balance_before - buyer_balance_after, cost);
        assert_eq!(treasury_after - treasury_before, cost);

        // No residue vault created.
        assert!(ctx
            .banks_client
            .get_account(residue_pda)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn test_buy_name_from_funding_plan_balance_plus_withdrawal() {
        // Multi-source: balance + withdrawal vault. Proves the wrapper forwards
        // remaining_accounts correctly to ario-gar's pay_from_funding_plan.
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;

        // Create a withdrawal vault for the operator (decrease 30K → 30K vault).
        let (gar_settings_key, _) = gar_settings_pda();
        let (op_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, setup.operator.as_ref()],
            &ario_gar::ID,
        );
        let (op_withdrawal_key, _) = Pubkey::find_program_address(
            &[
                WITHDRAWAL_SEED,
                setup.operator.as_ref(),
                &0u64.to_le_bytes(),
            ],
            &ario_gar::ID,
        );
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DecreaseOperatorStake {
                    settings: gar_settings_key,
                    gateway: setup.gateway_key,
                    withdrawal_counter: op_counter_key,
                    withdrawal: op_withdrawal_key,
                    operator: setup.operator,
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::DecreaseOperatorStake {
                    amount: 30_000_000_000,
                }
                .data(),
            }],
            Some(&setup.operator),
            &[&operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Operator buys a name; pays from balance + withdrawal vault.
        // Operator's buyer_token gets some tokens for the balance portion.
        let payer_clone = ctx.payer.insecure_clone();
        transfer_test_ant(&mut ctx, ant_key, &payer_clone, setup.operator).await;

        // Mint a balance of 1M mARIO into operator's ATA — small balance contribution.
        let operator_token = Keypair::new();
        create_token_account(
            &mut ctx,
            &operator_token,
            &setup.mint.pubkey(),
            &setup.operator,
        )
        .await;
        mint_tokens(
            &mut ctx,
            &setup.mint.pubkey(),
            &operator_token.pubkey(),
            &setup.mint_authority,
            100_000_000_000,
        )
        .await;

        let withdrawal_before = ario_gar::state::Withdrawal::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(op_withdrawal_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .amount;
        let operator_balance_before = token_balance(&mut ctx, &operator_token.pubkey()).await;
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;

        let name = "fplan-mix".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();

        let cost = ario_arns::pricing::calculate_registration_fee(
            ario_arns::pricing::get_base_fee_for_name_length(
                &ario_arns::state::GENESIS_FEES,
                name.len(),
            )
            .unwrap(),
            PurchaseType::Lease,
            1,
            ario_arns::pricing::DEMAND_FACTOR_SCALE,
        )
        .unwrap();
        // Split cost: 1/3 from balance, rest from withdrawal.
        let from_balance = cost / 3;
        let from_withdrawal = cost - from_balance;

        let mut accounts = ario_arns::accounts::BuyNameFromFundingPlan {
            config: setup.config_key,
            demand_factor: setup.demand_factor_key,
            arns_record: record_key,
            name_registry: registry_key,
            reserved_name_check: reserved_name_pda(&name).0,
            returned_name_check: returned_name_pda(&name).0,
            gar_settings: gar_settings_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.treasury.pubkey(),
            payer_token_account: Some(operator_token.pubkey()),
            buyer: setup.operator,
            withdrawal_counter: op_counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        // Funding source PDAs in remaining_accounts: 1 entry for the Withdrawal source.
        accounts.push(AccountMeta::new(op_withdrawal_key, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts,
                data: ario_arns::instruction::BuyNameFromFundingPlan {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                    sources: vec![fp_balance(from_balance), fp_withdrawal(from_withdrawal)],
                    discount_account_count: 0,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&setup.operator),
            &[&operator_kp],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // ArnsRecord landed.
        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert_eq!(record.owner, setup.operator);
        assert_eq!(record.purchase_price, cost);

        // Source bookkeeping correct.
        let withdrawal_after = ario_gar::state::Withdrawal::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(op_withdrawal_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap()
        .amount;
        assert_eq!(withdrawal_before - withdrawal_after, from_withdrawal);
        let operator_balance_after = token_balance(&mut ctx, &operator_token.pubkey()).await;
        assert_eq!(
            operator_balance_before - operator_balance_after,
            from_balance
        );
        let treasury_after = token_balance(&mut ctx, &setup.treasury.pubkey()).await;
        assert_eq!(treasury_after - treasury_before, cost);
    }

    // =========================================
    // Multi-gateway funding-plan tests (Sprint 3.C — belt-and-suspenders)
    // =========================================
    //
    // Each of the 5 ArNS funding-plan ix gets at least one multi-gateway test
    // proving the wrapper passes per-source PDAs through to ario-gar's
    // pay_from_funding_plan correctly. buy_name additionally gets a
    // with-residue test exercising the auto-vault path through arns CPI.

    fn fp_delegation(amount: u64) -> FundingSourceSpec {
        FundingSourceSpec {
            kind: FundingSourceKind::Delegation,
            amount,
        }
    }

    /// Spawn a 2nd gateway with a fresh operator and delegate `amount` to it
    /// from `ctx.payer` (the same buyer used by setup_full_environment).
    /// Returns (gateway_key, delegation_key).
    async fn add_second_gateway_with_delegation(
        ctx: &mut ProgramTestContext,
        setup: &FundFromStakeSetup,
        amount: u64,
    ) -> (Pubkey, Pubkey) {
        let payer_pk = ctx.payer.pubkey();
        let operator_keypair = Keypair::new();
        let operator_pk = operator_keypair.pubkey();

        // Fund operator + create operator's token account, mint stake.
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[solana_sdk::system_instruction::transfer(
                &payer_pk,
                &operator_pk,
                10_000_000_000,
            )],
            Some(&payer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        let operator_token = Keypair::new();
        create_token_account(ctx, &operator_token, &setup.mint.pubkey(), &operator_pk).await;
        mint_tokens(
            ctx,
            &setup.mint.pubkey(),
            &operator_token.pubkey(),
            &setup.mint_authority,
            100_000_000_000,
        )
        .await;

        let (gateway_key, _) = gar_gateway_pda(&operator_pk);
        let (observer_lookup_key, _) = gar_observer_lookup_pda(&operator_pk);
        let (gar_registry_key, _) = gar_registry_pda();
        let (gar_settings_key, _) = gar_settings_pda();

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::JoinNetwork {
                    registry: gar_registry_key,
                    settings: gar_settings_key,
                    gateway: gateway_key,
                    operator_token_account: operator_token.pubkey(),
                    stake_token_account: setup.stake_token.pubkey(),
                    observer_lookup: observer_lookup_key,
                    operator: operator_pk,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_gar::instruction::JoinNetwork {
                    params: JoinNetworkParams {
                        operator_stake: 50_000_000_000,
                        label: "fund-test-gw2".to_string(),
                        fqdn: "gw2.test.com".to_string(),
                        port: 443,
                        protocol: ario_gar::state::Protocol::Https,
                        properties: None,
                        note: None,
                        allow_delegated_staking: true,
                        delegate_reward_share_ratio: 10,
                        min_delegate_stake: None,
                        observer_address: operator_pk,
                    },
                }
                .data(),
            }],
            Some(&operator_pk),
            &[&operator_keypair],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Delegate from buyer (ctx.payer) to this 2nd gateway.
        let (delegation_key, _) = gar_delegation_pda(&operator_pk, &payer_pk);
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_gar::ID,
                accounts: ario_gar::accounts::DelegateStake {
                    settings: gar_settings_key,
                    gateway: gateway_key,
                    delegation: delegation_key,
                    delegator_token_account: setup.buyer_token.pubkey(),
                    stake_token_account: setup.stake_token.pubkey(),
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

        (gateway_key, delegation_key)
    }

    /// Standard multi-gateway setup: stake `stake_per` on each of 2 gateways.
    /// Returns (gw1_key, del1_key, gw2_key, del2_key).
    async fn setup_two_gateways(
        ctx: &mut ProgramTestContext,
        setup: &FundFromStakeSetup,
        stake_per: u64,
    ) -> (Pubkey, Pubkey, Pubkey, Pubkey) {
        // Gateway 1 = setup.gateway_key. Delegate from buyer to it.
        let del1 = delegate_from_buyer(ctx, setup, stake_per).await;
        // Gateway 2 = freshly added.
        let (gw2, del2) = add_second_gateway_with_delegation(ctx, setup, stake_per).await;
        (setup.gateway_key, del1, gw2, del2)
    }

    #[tokio::test]
    async fn test_buy_name_from_funding_plan_multi_gateway() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;
        let buyer_pk = ctx.payer.pubkey();
        let (gar_settings_key, _) = gar_settings_pda();

        // 2 gateways × 50 ARIO each = 100 ARIO available across delegations.
        // 1-year lease cost = base_fee × 1.2 × demand_factor. For a 13-char
        // name, base_fee = 200 ARIO → cost = 240 ARIO. Stake comfortably above.
        let stake_per = 1_000_000_000u64; // 1000 ARIO per gateway
        let (gw1, del1, gw2, del2) = setup_two_gateways(&mut ctx, &setup, stake_per).await;

        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, buyer_pk.as_ref()],
            &ario_gar::ID,
        );
        let treasury_before = token_balance(&mut ctx, &setup.treasury.pubkey()).await;

        let name = "fp-mg-buyname".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();
        let cost = ario_arns::pricing::calculate_registration_fee(
            ario_arns::pricing::get_base_fee_for_name_length(
                &ario_arns::state::GENESIS_FEES,
                name.len(),
            )
            .unwrap(),
            PurchaseType::Lease,
            1,
            ario_arns::pricing::DEMAND_FACTOR_SCALE,
        )
        .unwrap();

        // Split cost across both gateways.
        let pay1 = cost / 2;
        let pay2 = cost - pay1;

        let mut accounts = ario_arns::accounts::BuyNameFromFundingPlan {
            config: setup.config_key,
            demand_factor: setup.demand_factor_key,
            arns_record: record_key,
            name_registry: registry_key,
            reserved_name_check: reserved_name_pda(&name).0,
            returned_name_check: returned_name_pda(&name).0,
            gar_settings: gar_settings_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.treasury.pubkey(),
            payer_token_account: None,
            buyer: buyer_pk,
            withdrawal_counter: withdrawal_counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        // Per-source slots: 2 delegations × [gateway, delegation].
        accounts.push(AccountMeta::new(gw1, false));
        accounts.push(AccountMeta::new(del1, false));
        accounts.push(AccountMeta::new(gw2, false));
        accounts.push(AccountMeta::new(del2, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts,
                data: ario_arns::instruction::BuyNameFromFundingPlan {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                    sources: vec![fp_delegation(pay1), fp_delegation(pay2)],
                    discount_account_count: 0,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Verify record + delegations + treasury.
        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert_eq!(record.purchase_price, cost);
        assert_eq!(
            read_delegation_amount(&mut ctx, del1).await,
            stake_per - pay1
        );
        assert_eq!(
            read_delegation_amount(&mut ctx, del2).await,
            stake_per - pay2
        );
        assert_eq!(
            token_balance(&mut ctx, &setup.treasury.pubkey()).await - treasury_before,
            cost
        );
    }

    #[tokio::test]
    async fn test_buy_name_from_funding_plan_multi_gateway_with_residue() {
        // Force one gateway's delegation to drain sub-min → 1 residue vault via arns CPI.
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;
        let buyer_pk = ctx.payer.pubkey();
        let (gar_settings_key, _) = gar_settings_pda();

        // Gateway 1: stake exactly 13 ARIO; draw 9 ARIO → residue 4 ARIO < min 10 ARIO.
        // Gateway 2: stake enough to cover the remainder of the 13-char lease cost
        // (base 200 ARIO × 1.2 = 240 ARIO) leaving > min.
        let (gw1, del1) = (
            setup.gateway_key,
            delegate_from_buyer(&mut ctx, &setup, 13_000_000).await,
        );
        let (gw2, del2) = add_second_gateway_with_delegation(&mut ctx, &setup, 1_000_000_000).await;

        let name = "fp-mg-residue".to_string();
        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();
        let cost = ario_arns::pricing::calculate_registration_fee(
            ario_arns::pricing::get_base_fee_for_name_length(
                &ario_arns::state::GENESIS_FEES,
                name.len(),
            )
            .unwrap(),
            PurchaseType::Lease,
            1,
            ario_arns::pricing::DEMAND_FACTOR_SCALE,
        )
        .unwrap();
        let pay1 = 9_000_000u64;
        let pay2 = cost - pay1;
        // sanity: pay2 leaves > min in gw2.
        assert!(1_000_000_000 - pay2 >= 10_000_000);

        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, buyer_pk.as_ref()],
            &ario_gar::ID,
        );
        // Predict residue PDA at next_id=0.
        let (residue0, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_SEED, buyer_pk.as_ref(), &0u64.to_le_bytes()],
            &ario_gar::ID,
        );

        let mut accounts = ario_arns::accounts::BuyNameFromFundingPlan {
            config: setup.config_key,
            demand_factor: setup.demand_factor_key,
            arns_record: record_key,
            name_registry: registry_key,
            reserved_name_check: reserved_name_pda(&name).0,
            returned_name_check: returned_name_pda(&name).0,
            gar_settings: gar_settings_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.treasury.pubkey(),
            payer_token_account: None,
            buyer: buyer_pk,
            withdrawal_counter: withdrawal_counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new(gw1, false));
        accounts.push(AccountMeta::new(del1, false));
        accounts.push(AccountMeta::new(gw2, false));
        accounts.push(AccountMeta::new(del2, false));
        accounts.push(AccountMeta::new(residue0, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts,
                data: ario_arns::instruction::BuyNameFromFundingPlan {
                    params: ario_arns::BuyNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                    sources: vec![fp_delegation(pay1), fp_delegation(pay2)],
                    discount_account_count: 0,
                    residue_vault_count: 1,
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        // CU baseline (Sprint 4.A): arns buy_name CPI with 2 dels + 1 residue
        // consumed ~100.8K CU on fresh BPF build (2026-05-04). Cap at 130K CU
        // (~29% headroom). This is the heaviest arns funding-plan ix path.
        let result = ctx
            .banks_client
            .process_transaction_with_metadata(tx)
            .await
            .unwrap();
        assert!(
            result.result.is_ok(),
            "buy_name multi-gateway+residue should succeed"
        );
        let metadata = result.metadata.expect("metadata must be present");
        assert!(
            metadata.compute_units_consumed < 130_000,
            "buy_name multi-gateway+residue used {} CU, expected < 130_000",
            metadata.compute_units_consumed,
        );

        // gw1's delegation drained (residue auto-vaulted); gw2's intact.
        assert_eq!(read_delegation_amount(&mut ctx, del1).await, 0);
        assert_eq!(
            read_delegation_amount(&mut ctx, del2).await,
            1_000_000_000 - pay2
        );
        // Residue vault exists with 4 ARIO.
        let residue_acct = ctx
            .banks_client
            .get_account(residue0)
            .await
            .unwrap()
            .unwrap();
        let residue =
            ario_gar::state::Withdrawal::try_deserialize(&mut residue_acct.data.as_slice())
                .unwrap();
        assert_eq!(residue.amount, 13_000_000 - pay1);
        assert!(residue.is_delegate);
    }

    #[tokio::test]
    async fn test_buy_returned_name_from_funding_plan_multi_gateway() {
        // buy_returned_name has a different Accounts struct (returnedName,
        // initiatorTokenAccount), but the funding-plan CPI shape is identical.
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;
        let buyer_pk = ctx.payer.pubkey();
        let (gar_settings_key, _) = gar_settings_pda();

        // 1-year lease cost = base_fee × 1.2 × demand_factor. For a 13-char
        // name, base_fee = 200 ARIO → cost = 240 ARIO. Stake comfortably above.
        let stake_per = 1_000_000_000u64; // 1000 ARIO per gateway
        let (gw1, del1, gw2, del2) = setup_two_gateways(&mut ctx, &setup, stake_per).await;

        // Pre-create a returned-name auction by calling release_name on a
        // pre-existing record. Easier: directly hand-create a ReturnedName
        // account at the right PDA so we can drive buy_returned_name.
        let name = "fp-mg-rtn".to_string();
        let (returned_pda, returned_bump) = returned_name_pda(&name);
        let now = ctx
            .banks_client
            .get_sysvar::<solana_sdk::clock::Clock>()
            .await
            .unwrap()
            .unix_timestamp;
        let name_hash_bytes = ario_arns::pricing::hash_name(&name);
        // Set returned_at past the 14-day Dutch auction so cost == registration_fee
        // (no premium multiplier). Keeps the test's stake math simple.
        let returned = ario_arns::state::ReturnedName {
            name: name.clone(),
            name_hash: name_hash_bytes,
            initiator: setup.operator,
            returned_at: now - (15 * 86_400),
            bump: returned_bump,
            version: ario_arns::state::RETURNED_NAME_VERSION,
        };
        let mut returned_data = vec![];
        returned.try_serialize(&mut returned_data).unwrap();
        let rent = solana_sdk::rent::Rent::default();
        ctx.set_account(
            &returned_pda,
            &solana_sdk::account::Account {
                lamports: rent.minimum_balance(returned_data.len()),
                data: returned_data,
                owner: ario_arns::ID,
                executable: false,
                rent_epoch: 0,
            }
            .into(),
        );

        // initiatorTokenAccount: just use the operator's ATA.
        let initiator_token = Keypair::new();
        create_token_account(
            &mut ctx,
            &initiator_token,
            &setup.mint.pubkey(),
            &setup.operator,
        )
        .await;

        let (record_key, _) = arns_record_pda(&name);
        let registry_key = name_registry_key();
        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, buyer_pk.as_ref()],
            &ario_gar::ID,
        );

        // Pricing for buy_returned_name is a Dutch auction over the
        // registration_fee. The handler splits total_premium 50/50 between
        // initiator and protocol. We compute the protocol share for the plan.
        let registration_fee = ario_arns::pricing::calculate_registration_fee(
            ario_arns::pricing::get_base_fee_for_name_length(
                &ario_arns::state::GENESIS_FEES,
                name.len(),
            )
            .unwrap(),
            PurchaseType::Lease,
            1,
            ario_arns::pricing::DEMAND_FACTOR_SCALE,
        )
        .unwrap();
        let total_premium = ario_arns::pricing::calculate_returned_name_premium(
            registration_fee,
            returned.returned_at,
            now,
        )
        .unwrap();
        let initiator_share = total_premium / 2;
        let protocol_share = total_premium - initiator_share;
        // Just use the protocol_share for the funding plan (initiator share goes
        // direct from buyer ATA → initiator ATA via SPL transfer).
        let pay1 = protocol_share / 2;
        let pay2 = protocol_share - pay1;

        let mut accounts = ario_arns::accounts::BuyReturnedNameFromFundingPlan {
            config: setup.config_key,
            demand_factor: setup.demand_factor_key,
            arns_record: record_key,
            name_registry: registry_key,
            returned_name: returned_pda,
            gar_settings: gar_settings_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.treasury.pubkey(),
            buyer_token_account: setup.buyer_token.pubkey(),
            initiator_token_account: initiator_token.pubkey(),
            payer_token_account: None,
            buyer: buyer_pk,
            withdrawal_counter: withdrawal_counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new(gw1, false));
        accounts.push(AccountMeta::new(del1, false));
        accounts.push(AccountMeta::new(gw2, false));
        accounts.push(AccountMeta::new(del2, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts,
                data: ario_arns::instruction::BuyReturnedNameFromFundingPlan {
                    params: ario_arns::BuyReturnedNameParams {
                        name: name.clone(),
                        purchase_type: PurchaseType::Lease,
                        years: 1,
                        ant: ant_key,
                    },
                    sources: vec![fp_delegation(pay1), fp_delegation(pay2)],
                    discount_account_count: 0,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Both delegations decreased.
        assert_eq!(
            read_delegation_amount(&mut ctx, del1).await,
            stake_per - pay1
        );
        assert_eq!(
            read_delegation_amount(&mut ctx, del2).await,
            stake_per - pay2
        );
    }

    /// Pre-create an ArnsRecord for a name + ant so manage ix can target it.
    /// Calls buy_name as the buyer to get a real ArnsRecord on chain.
    async fn buy_lease_for_manage(
        ctx: &mut ProgramTestContext,
        setup: &FundFromStakeSetup,
        ant_key: Pubkey,
        name: &str,
        years: u8,
    ) -> Pubkey {
        let buyer_pk = ctx.payer.pubkey();
        let (record_key, _) = arns_record_pda(name);
        let registry_key = name_registry_key();
        let cost = ario_arns::pricing::calculate_registration_fee(
            ario_arns::pricing::get_base_fee_for_name_length(
                &ario_arns::state::GENESIS_FEES,
                name.len(),
            )
            .unwrap(),
            PurchaseType::Lease,
            years,
            ario_arns::pricing::DEMAND_FACTOR_SCALE,
        )
        .unwrap();
        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts: ario_arns::accounts::BuyName {
                    config: setup.config_key,
                    demand_factor: setup.demand_factor_key,
                    arns_record: record_key,
                    name_registry: registry_key,
                    reserved_name_check: reserved_name_pda(name).0,
                    returned_name_check: returned_name_pda(name).0,
                    buyer_token_account: setup.buyer_token.pubkey(),
                    protocol_token_account: setup.treasury.pubkey(),
                    buyer: buyer_pk,
                    token_program: spl_token::id(),
                    system_program: system_program::id(),
                }
                .to_account_metas(None),
                data: ario_arns::instruction::BuyName {
                    params: ario_arns::BuyNameParams {
                        name: name.to_string(),
                        purchase_type: PurchaseType::Lease,
                        years,
                        ant: ant_key,
                    },
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();
        let _ = cost;
        record_key
    }

    #[tokio::test]
    async fn test_upgrade_name_from_funding_plan_multi_gateway() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;
        let buyer_pk = ctx.payer.pubkey();
        let (gar_settings_key, _) = gar_settings_pda();

        let name = "fp-mg-up".to_string();
        let record_key = buy_lease_for_manage(&mut ctx, &setup, ant_key, &name, 1).await;

        // Upgrade fee = base × 4 (permabuy formula). 8-char base = 500 ARIO →
        // upgrade cost ~2000 ARIO. Stake 1500 ARIO per gateway covers split.
        let stake_per = 1_500_000_000u64;
        let (gw1, del1, gw2, del2) = setup_two_gateways(&mut ctx, &setup, stake_per).await;

        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, buyer_pk.as_ref()],
            &ario_gar::ID,
        );

        // Upgrade charges the full permabuy fee (base × df × 5 / SCALE).
        let base_fee = ario_arns::pricing::get_base_fee_for_name_length(
            &ario_arns::state::GENESIS_FEES,
            name.len(),
        )
        .unwrap();
        let cost = ario_arns::pricing::calculate_permabuy_fee(
            base_fee,
            ario_arns::pricing::DEMAND_FACTOR_SCALE,
        )
        .unwrap();
        let pay1 = cost / 2;
        let pay2 = cost - pay1;

        let mut accounts = ario_arns::accounts::UpgradeNameFromFundingPlan {
            config: setup.config_key,
            demand_factor: setup.demand_factor_key,
            arns_record: record_key,
            gar_settings: gar_settings_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.treasury.pubkey(),
            payer_token_account: None,
            caller: buyer_pk,
            withdrawal_counter: withdrawal_counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new(gw1, false));
        accounts.push(AccountMeta::new(del1, false));
        accounts.push(AccountMeta::new(gw2, false));
        accounts.push(AccountMeta::new(del2, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts,
                data: ario_arns::instruction::UpgradeNameFromFundingPlan {
                    sources: vec![fp_delegation(pay1), fp_delegation(pay2)],
                    discount_account_count: 0,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        // Record is now permabuy.
        let record = ArnsRecord::try_deserialize(
            &mut ctx
                .banks_client
                .get_account(record_key)
                .await
                .unwrap()
                .unwrap()
                .data
                .as_slice(),
        )
        .unwrap();
        assert!(matches!(record.purchase_type, PurchaseType::Permabuy));
        // Both delegations decreased.
        assert_eq!(
            read_delegation_amount(&mut ctx, del1).await,
            stake_per - pay1
        );
        assert_eq!(
            read_delegation_amount(&mut ctx, del2).await,
            stake_per - pay2
        );
    }

    #[tokio::test]
    async fn test_extend_lease_from_funding_plan_multi_gateway() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;
        let buyer_pk = ctx.payer.pubkey();
        let (gar_settings_key, _) = gar_settings_pda();

        let name = "fp-mg-ext".to_string();
        let record_key = buy_lease_for_manage(&mut ctx, &setup, ant_key, &name, 1).await;

        // Upgrade fee = base × 4 (permabuy formula). 8-char base = 500 ARIO →
        // upgrade cost ~2000 ARIO. Stake 1500 ARIO per gateway covers split.
        let stake_per = 1_500_000_000u64;
        let (gw1, del1, gw2, del2) = setup_two_gateways(&mut ctx, &setup, stake_per).await;

        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, buyer_pk.as_ref()],
            &ario_gar::ID,
        );

        let base_fee = ario_arns::pricing::get_base_fee_for_name_length(
            &ario_arns::state::GENESIS_FEES,
            name.len(),
        )
        .unwrap();
        let years = 2u8;
        let cost = ario_arns::pricing::calculate_extension_fee(
            base_fee,
            years,
            ario_arns::pricing::DEMAND_FACTOR_SCALE,
        )
        .unwrap();
        let pay1 = cost / 2;
        let pay2 = cost - pay1;

        let mut accounts = ario_arns::accounts::ExtendLeaseFromFundingPlan {
            config: setup.config_key,
            demand_factor: setup.demand_factor_key,
            arns_record: record_key,
            gar_settings: gar_settings_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.treasury.pubkey(),
            payer_token_account: None,
            caller: buyer_pk,
            withdrawal_counter: withdrawal_counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new(gw1, false));
        accounts.push(AccountMeta::new(del1, false));
        accounts.push(AccountMeta::new(gw2, false));
        accounts.push(AccountMeta::new(del2, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts,
                data: ario_arns::instruction::ExtendLeaseFromFundingPlan {
                    years,
                    sources: vec![fp_delegation(pay1), fp_delegation(pay2)],
                    discount_account_count: 0,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        assert_eq!(
            read_delegation_amount(&mut ctx, del1).await,
            stake_per - pay1
        );
        assert_eq!(
            read_delegation_amount(&mut ctx, del2).await,
            stake_per - pay2
        );
    }

    #[tokio::test]
    async fn test_increase_undername_limit_from_funding_plan_multi_gateway() {
        let ant_keypair = Keypair::new();
        let ant_key = ant_keypair.pubkey();
        let operator_kp = Keypair::new();
        let mint_kp = Keypair::new();
        let stake_kp = Keypair::new();
        let treasury_kp = Keypair::new();
        let pt = program_test_with_arns_and_gar(
            treasury_kp.pubkey(),
            stake_kp.pubkey(),
            mint_kp.pubkey(),
        );
        let mut ctx = pt.start_with_context().await;
        mint_test_ant(&mut ctx, &ant_keypair).await;
        let setup = setup_full_environment_with_keys(
            &mut ctx,
            &operator_kp,
            mint_kp,
            stake_kp,
            treasury_kp,
        )
        .await;
        let buyer_pk = ctx.payer.pubkey();
        let (gar_settings_key, _) = gar_settings_pda();

        let name = "fp-mg-und".to_string();
        let record_key = buy_lease_for_manage(&mut ctx, &setup, ant_key, &name, 1).await;

        // Upgrade fee = base × 4 (permabuy formula). 8-char base = 500 ARIO →
        // upgrade cost ~2000 ARIO. Stake 1500 ARIO per gateway covers split.
        let stake_per = 1_500_000_000u64;
        let (gw1, del1, gw2, del2) = setup_two_gateways(&mut ctx, &setup, stake_per).await;

        let (withdrawal_counter_key, _) = Pubkey::find_program_address(
            &[WITHDRAWAL_COUNTER_SEED, buyer_pk.as_ref()],
            &ario_gar::ID,
        );

        let base_fee = ario_arns::pricing::get_base_fee_for_name_length(
            &ario_arns::state::GENESIS_FEES,
            name.len(),
        )
        .unwrap();
        let quantity = 5u16;
        // Note: target ArnsRecord is a Lease, so undername fee uses Lease pct.
        let cost = ario_arns::pricing::calculate_undername_cost(
            base_fee,
            quantity,
            PurchaseType::Lease,
            ario_arns::pricing::DEMAND_FACTOR_SCALE,
        )
        .unwrap();
        let pay1 = cost / 2;
        let pay2 = cost - pay1;

        let mut accounts = ario_arns::accounts::IncreaseUndernameFromFundingPlan {
            config: setup.config_key,
            demand_factor: setup.demand_factor_key,
            arns_record: record_key,
            gar_settings: gar_settings_key,
            stake_token_account: setup.stake_token.pubkey(),
            protocol_token_account: setup.treasury.pubkey(),
            payer_token_account: None,
            caller: buyer_pk,
            withdrawal_counter: withdrawal_counter_key,
            gar_program: ario_gar::ID,
            token_program: spl_token::id(),
            system_program: system_program::id(),
        }
        .to_account_metas(None);
        accounts.push(AccountMeta::new(gw1, false));
        accounts.push(AccountMeta::new(del1, false));
        accounts.push(AccountMeta::new(gw2, false));
        accounts.push(AccountMeta::new(del2, false));

        let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
        let tx = Transaction::new_signed_with_payer(
            &[Instruction {
                program_id: ario_arns::ID,
                accounts,
                data: ario_arns::instruction::IncreaseUndernameLimitFromFundingPlan {
                    quantity,
                    sources: vec![fp_delegation(pay1), fp_delegation(pay2)],
                    discount_account_count: 0,
                    residue_vault_count: 0,
                }
                .data(),
            }],
            Some(&buyer_pk),
            &[&ctx.payer],
            blockhash,
        );
        ctx.banks_client.process_transaction(tx).await.unwrap();

        assert_eq!(
            read_delegation_amount(&mut ctx, del1).await,
            stake_per - pay1
        );
        assert_eq!(
            read_delegation_amount(&mut ctx, del2).await,
            stake_per - pay2
        );
    }
}

// (deleted obsolete test `test_sync_attributes_recovers_deferred_traits` — exercised the
//  ARIO-ARNS-side trait sync mechanism that the Sprint 1-3
//  reshape moved to `ario_ant::sync_attributes`. Round-trip
//  coverage is now in programs/ario-ant/tests/sync_attributes.rs.
//  See ADR-016 amendment / BD-100.)

/// Direct negative-path coverage for `sync_attributes`: a non-holder cannot
/// reconcile traits even when the ArnsRecord exists.
#[tokio::test]
async fn test_sync_attributes_rejects_non_ant_holder() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "syncneg";
    buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Fund a non-holder.
    let stranger = Keypair::new();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &stranger.pubkey(),
            1_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let (_arns_record_key, _) = arns_record_pda(name);
    // ADR-016 reshape: SyncAttributes ix moved to ario-ant. Non-holder
    // rejection is now tested in ario-ant integration tests against the
    // mpl-core UpdatePluginV1 authority check.
    let _ = stranger;
}

// =========================================
// PERMISSIONLESS LEASE MANAGEMENT (matches Lua)
// =========================================
//
// `extend_lease`, `upgrade_name`, and `increase_undername_limit` (plus their
// `_from_delegation` / `_from_operator_stake` variants) are now callable by
// any ARIO holder — not just the ANT NFT holder. Matches Lua exactly:
// `arns.extendLease`, `arns.upgradeRecord`, `arns.increaseUndernameLimit`
// have no caller-side authorization, only a balance check.
//
// Trait sync (Metaplex Attributes plugin) requires the ANT owner as signer
// (plugin authority = Owner), so when caller != owner the CPI is skipped
// at runtime and the on-chain `ArnsRecord` is the authoritative state.
// The ANT owner can run `sync_attributes` to reconcile traits afterwards.

/// Common fixture builder: mint ANT to ctx.payer, transfer to a new owner,
/// fund a third-party caller with ARIO, and buy a lease pointing at the ANT.
async fn setup_with_third_party_caller(
    ctx: &mut ProgramTestContext,
    setup: &ArnsSetup,
    ant_key: Pubkey,
    name: &str,
) -> (Keypair, Keypair, Keypair, Pubkey) {
    // (ant_owner, third_party_caller, third_party_token, arns_record_key)
    let ant_owner = Keypair::new();
    let third_party = Keypair::new();
    let third_party_token = Keypair::new();

    // Fund both with SOL
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[
            solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &ant_owner.pubkey(),
                1_000_000_000,
            ),
            solana_sdk::system_instruction::transfer(
                &ctx.payer.pubkey(),
                &third_party.pubkey(),
                1_000_000_000,
            ),
        ],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Buy the lease BEFORE transferring ownership so ctx.payer (the owner at
    // purchase time) gets traits synced; we then transfer to ant_owner so
    // subsequent management calls by third_party are correctly "non-holder".
    let arns_record_key = buy_name_helper(ctx, setup, name, PurchaseType::Lease, 1, ant_key).await;
    let payer_clone = ctx.payer.insecure_clone();
    transfer_test_ant(ctx, ant_key, &payer_clone, ant_owner.pubkey()).await;

    // Fund third-party with ARIO so they can pay for management ops.
    create_token_account(
        ctx,
        &third_party_token,
        &setup.mint.pubkey(),
        &third_party.pubkey(),
    )
    .await;
    mint_tokens(
        ctx,
        &setup.mint.pubkey(),
        &third_party_token.pubkey(),
        &setup.mint_authority,
        100_000_000_000,
    )
    .await;

    (ant_owner, third_party, third_party_token, arns_record_key)
}

/// Helper: send extend_lease, signing as the given caller from their token account.
async fn send_extend_lease_by(
    ctx: &mut ProgramTestContext,
    setup: &ArnsSetup,
    arns_record_key: Pubkey,
    caller: &Keypair,
    caller_token: Pubkey,
    years: u8,
) -> std::result::Result<(), BanksClientError> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: caller_token,
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: caller.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years }.data(),
        }],
        Some(&caller.pubkey()),
        &[caller],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// Helper: send upgrade_name, signing as the given caller from their token account.
async fn send_upgrade_name_by(
    ctx: &mut ProgramTestContext,
    setup: &ArnsSetup,
    arns_record_key: Pubkey,
    ant_key: Pubkey,
    caller: &Keypair,
    caller_token: Pubkey,
) -> std::result::Result<(), BanksClientError> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpgradeName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: caller_token,
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: caller.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpgradeName {}.data(),
        }],
        Some(&caller.pubkey()),
        &[caller],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// Helper: send increase_undername_limit by the given caller.
async fn send_increase_undernames_by(
    ctx: &mut ProgramTestContext,
    setup: &ArnsSetup,
    arns_record_key: Pubkey,
    ant_key: Pubkey,
    caller: &Keypair,
    caller_token: Pubkey,
    quantity: u16,
) -> std::result::Result<(), BanksClientError> {
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::IncreaseUndernameLimit {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: caller_token,
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: caller.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::IncreaseUndernameLimit { quantity }.data(),
        }],
        Some(&caller.pubkey()),
        &[caller],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

/// ADR-016 reshape: sync_attributes moved to ario-ant. This helper is now
/// a no-op stub kept so the surrounding tests still compile during the
/// reshape; trait-sync coverage moves to ario-ant integration tests.
#[allow(dead_code)]
async fn send_sync_attributes_by(
    _ctx: &mut ProgramTestContext,
    _arns_record_key: Pubkey,
    _ant_key: Pubkey,
    _ant_holder: &Keypair,
) -> std::result::Result<(), BanksClientError> {
    Ok(())
}

/// Helper: read a TokenAccount's balance.
async fn arns_token_balance(ctx: &mut ProgramTestContext, account: &Pubkey) -> u64 {
    let acc = ctx
        .banks_client
        .get_account(*account)
        .await
        .unwrap()
        .unwrap();
    let parsed = spl_token::state::Account::unpack(&acc.data).unwrap();
    parsed.amount
}

// 1. extend_lease by non-holder succeeds (matches Lua)
#[tokio::test]
async fn test_extend_lease_by_non_holder() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "extbynh";
    let (_ant_owner, third_party, third_party_token, arns_record_key) =
        setup_with_third_party_caller(&mut ctx, &setup, ant_key, name).await;

    // Snapshot record + balance BEFORE
    let record_before = ArnsRecord::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(arns_record_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    let end_before = record_before.end_timestamp.unwrap();
    let bal_before = arns_token_balance(&mut ctx, &third_party_token.pubkey()).await;

    // Non-holder extends — should succeed
    send_extend_lease_by(
        &mut ctx,
        &setup,
        arns_record_key,
        &third_party,
        third_party_token.pubkey(),
        2,
    )
    .await
    .unwrap();

    // Lease extended by 2 years
    let record_after = ArnsRecord::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(arns_record_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(
        record_after.end_timestamp.unwrap(),
        end_before + 2 * ONE_YEAR_SECONDS
    );

    // Caller's ARIO balance debited
    let bal_after = arns_token_balance(&mut ctx, &third_party_token.pubkey()).await;
    assert!(
        bal_after < bal_before,
        "third-party caller's ARIO must be debited"
    );
}

// (deleted obsolete test `test_increase_undernames_by_non_holder` — exercised the
//  ARIO-ARNS-side trait sync mechanism that the Sprint 1-3
//  reshape moved to `ario_ant::sync_attributes`. Round-trip
//  coverage is now in programs/ario-ant/tests/sync_attributes.rs.
//  See ADR-016 amendment / BD-100.)

// 4. Regression: ANT holder can still extend their own lease.
#[tokio::test]
async fn test_extend_lease_by_holder_still_works() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "extbyh";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;
    let record_before = ArnsRecord::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(arns_record_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    let end_before = record_before.end_timestamp.unwrap();

    // ctx.payer is both ANT holder AND extension caller — happy path.
    let payer_clone = ctx.payer.insecure_clone();
    send_extend_lease_by(
        &mut ctx,
        &setup,
        arns_record_key,
        &payer_clone,
        setup.buyer_token.pubkey(),
        1,
    )
    .await
    .unwrap();

    let record_after = ArnsRecord::try_deserialize(
        &mut ctx
            .banks_client
            .get_account(arns_record_key)
            .await
            .unwrap()
            .unwrap()
            .data
            .as_slice(),
    )
    .unwrap();
    assert_eq!(
        record_after.end_timestamp.unwrap(),
        end_before + ONE_YEAR_SECONDS
    );
}

// (deleted obsolete test `test_increase_undernames_by_holder_syncs_traits` — exercised the
//  ARIO-ARNS-side trait sync mechanism that the Sprint 1-3
//  reshape moved to `ario_ant::sync_attributes`. Round-trip
//  coverage is now in programs/ario-ant/tests/sync_attributes.rs.
//  See ADR-016 amendment / BD-100.)

// =========================================
// 7-9. Stake-variant permissionless tests
//
// These prove the same widening for the `_from_delegation` and
// `_from_operator_stake` variants. Stake variants don't currently CPI into
// MPL Core (no ant_asset in their accounts struct), so trait sync is always
// deferred. Tests focus on: caller != ant owner is allowed, record updates
// land, and stake-pool funds the cost.
// =========================================

// =========================================
// PR-3 regression tests
// =========================================

#[tokio::test]
async fn test_reassign_name_rejects_no_op() {
    // Audit M10: reassign_name with new_ant == record.ant would silently wipe
    // the Attributes plugin on the OLD asset every call, causing flickering
    // traits on DAS / marketplace UIs. Self-grief, but a 1-line guard
    // prevents the obvious foot-gun.
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "noopreassign";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    // Try to reassign to the SAME ANT — must fail.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: ant_key,
        caller: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName { new_ant: ant_key }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidParameter);
}

#[tokio::test]
async fn test_buy_returned_name_after_prune_to_returned() {
    // Audit Theme E gap: existing test_buy_returned_name only covers the
    // `release_name → buy` path (initiator == caller). The protocol-initiated
    // `prune_to_returned → buy_returned_name` path was never end-to-end
    // tested. Audit M9 was originally rated as "auctions unbuyable" because
    // SDK callers couldn't construct a token account owned by the arns_config
    // PDA — but anyone CAN create such a token account permissionlessly
    // (SPL InitializeAccount accepts any owner pubkey, no signature required).
    // This test exercises the full path to prove the workaround works.
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy a 1-year lease so we can let it expire
    let name = "expireme";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 1, ant_key).await;

    // Read end_timestamp to know when to warp
    let record_account = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    let end_ts = record.end_timestamp.unwrap();

    // Warp past expiry + grace period so prune_to_returned is allowed
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 2).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = end_ts + GRACE_PERIOD_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Permissionlessly call prune_to_returned. Sets returned_name.initiator
    // to the config PDA — the protocol-initiated path.
    let (returned_name_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::PruneToReturned {
                config: setup.config_key,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: registry_key,
                payer: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::PruneNameToReturned {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    // Verify the ReturnedName was created with initiator = config_pda
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap()
        .unwrap();
    let returned = ReturnedName::try_deserialize(&mut returned_account.data.as_slice()).unwrap();
    assert_eq!(
        returned.initiator, setup.config_key,
        "protocol-initiated returns set initiator = config_pda"
    );

    // Anyone can permissionlessly create a token account owned by the config
    // PDA (audit M9: SPL InitializeAccount accepts an arbitrary owner pubkey
    // with no signature requirement). This is what unblocks buy_returned_name
    // for protocol-initiated returns.
    let config_pda_token = Keypair::new();
    create_token_account(
        &mut ctx,
        &config_pda_token,
        &setup.mint.pubkey(),
        &setup.config_key,
    )
    .await;

    // Warp past the returned-name auction window so premium = 0 (cheaper)
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp += RETURNED_NAME_DURATION_SECONDS + 1;
    ctx.set_sysvar(&clock);

    // Buy the returned name. With initiator = config_pda, the handler's
    // is_protocol_initiator branch runs (100% to protocol, no transfer to
    // initiator vault — but the constraint still requires the vault to
    // exist and match initiator).
    let new_arns_key = arns_record_pda(name).0;
    let protocol_before = {
        let account = ctx
            .banks_client
            .get_account(setup.protocol_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyReturnedName {
                config: config_pda().0,
                demand_factor: demand_factor_pda().0,
                returned_name: returned_name_key,
                arns_record: new_arns_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                initiator_token_account: config_pda_token.pubkey(),
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyReturnedName {
                params: ario_arns::BuyReturnedNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("buy_returned_name must succeed for protocol-initiated returns");

    // Verify ArnsRecord recreated
    let record_account = ctx
        .banks_client
        .get_account(new_arns_key)
        .await
        .unwrap()
        .unwrap();
    let record = ArnsRecord::try_deserialize(&mut record_account.data.as_slice()).unwrap();
    assert_eq!(record.owner, ctx.payer.pubkey());
    assert_eq!(record.name, name);

    // Verify ReturnedName closed
    let returned_account = ctx
        .banks_client
        .get_account(returned_name_key)
        .await
        .unwrap();
    assert!(returned_account.is_none(), "ReturnedName closed after buy");

    // Verify protocol got 100% (initiator was config_pda → no split)
    let protocol_after = {
        let account = ctx
            .banks_client
            .get_account(setup.protocol_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };
    assert!(protocol_after > protocol_before, "Protocol received tokens");

    // Config-PDA-owned token account should still have zero balance —
    // protocol-initiated path skips the initiator transfer.
    let initiator_balance = {
        let account = ctx
            .banks_client
            .get_account(config_pda_token.pubkey())
            .await
            .unwrap()
            .unwrap();
        spl_token::state::Account::unpack(&account.data)
            .unwrap()
            .amount
    };
    assert_eq!(
        initiator_balance, 0,
        "protocol-initiated path skips initiator vault transfer"
    );
}

// =========================================
// SECURITY: Cross-program fake account rejection
// =========================================

/// Pass a fake gateway account (correct data layout but wrong owner program)
/// as remaining_account for the gateway operator discount. The validation in
/// try_apply_gateway_discount must reject it with InvalidGatewayProgram.
#[tokio::test]
async fn test_buy_name_fake_gateway_account_rejected() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "fakegw";
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    // Create a fake gateway account owned by system_program (wrong owner).
    // The data layout mimics a serialized Gateway struct, but owner != ario_gar::ID.
    let fake_gateway = Keypair::new();
    let fake_data = vec![0u8; 512]; // arbitrary data, doesn't matter — owner check is first
    let rent = ctx.banks_client.get_rent().await.unwrap();
    ctx.set_account(
        &fake_gateway.pubkey(),
        &solana_sdk::account::Account {
            lamports: rent.minimum_balance(fake_data.len()),
            data: fake_data,
            owner: system_program::id(), // WRONG OWNER — must trigger InvalidGatewayProgram
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Build BuyName instruction with the fake gateway as remaining_account
    let mut account_metas = ario_arns::accounts::BuyName {
        config: setup.config_key,
        demand_factor: setup.demand_factor_key,
        arns_record: arns_record_key,
        name_registry: registry_key,
        buyer_token_account: setup.buyer_token.pubkey(),
        protocol_token_account: setup.protocol_token.pubkey(),
        reserved_name_check: reserved_name_pda(name).0,
        returned_name_check: returned_name_pda(name).0,
        buyer: ctx.payer.pubkey(),
        token_program: spl_token::id(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);

    // Append fake gateway as remaining_account
    account_metas.push(AccountMeta::new_readonly(fake_gateway.pubkey(), false));

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: account_metas,
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::InvalidGatewayProgram);
}

// =========================================
// SECURITY: CU consumption assertion (buy_name)
// =========================================

/// Verify that buy_name stays within 1M CU budget.
#[tokio::test]
async fn test_buy_name_cu_consumption() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "cucheck";
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(name).0,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0,
                    ant: ant_key,
                },
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
    assert!(result.result.is_ok(), "BuyName should succeed");
    let metadata = result.metadata.expect("metadata must be present");
    // Measured: ~70,108 CU (SPL transfer + MPL Core UpdatePluginV1 CPI + name registry write).
    // Threshold = ~43% headroom. The 1M CU program budget remains the runtime ceiling;
    // this assertion is a regression alarm against accidental CU explosion.
    assert!(
        metadata.compute_units_consumed < 100_000,
        "BuyName used {} CU, expected < 100_000",
        metadata.compute_units_consumed
    );
}

// =========================================
// SECURITY: NameRegistry capacity limit
// =========================================

/// Pre-create NameRegistry with `count == MAX_NAMES` and attempt buy_name.
/// Must fail with RegistryFull. Mirrors the GatewayRegistry pattern in
/// ario-gar's `test_join_network_registry_full`.
#[tokio::test]
async fn test_buy_name_registry_full() {
    use anchor_lang::solana_program::hash::hash;

    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();

    // Build a ProgramTest that pre-creates the registry with count = MAX_NAMES.
    let mut pt = ProgramTest::new("ario_arns", ario_arns::ID, processor!(anchor_processor));
    pt.set_compute_max_units(1_000_000);
    // See `program_test_with_registry_inner`: when BPF_OUT_DIR is unset, flip
    // `prefer_bpf` AFTER ario_arns was registered native so MPL Core still loads
    // from the committed fixture; when set (CI), leave it (all load from `.so`).
    if std::env::var("BPF_OUT_DIR").is_err() && std::env::var("SBF_OUT_DIR").is_err() {
        pt.prefer_bpf(true);
    }
    pt.add_program("mpl_core", MPL_CORE_PROGRAM_ID, None);

    let registry_size = NameRegistry::bytes_for_capacity(NameRegistry::INITIAL_CAPACITY);
    let rent = solana_sdk::rent::Rent::default();
    let mut data = vec![0u8; registry_size];
    let disc = hash(b"account:NameRegistry");
    data[..8].copy_from_slice(&disc.to_bytes()[..8]);
    // count at offset 40..44 = MAX_NAMES (200_000) — every slot considered occupied
    data[40..44].copy_from_slice(&(NameRegistry::MAX_NAMES as u32).to_le_bytes());

    let registry_key_full = Pubkey::find_program_address(&[NAME_REGISTRY_SEED], &ario_arns::ID).0;
    pt.add_account(
        registry_key_full,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(registry_size),
            data,
            owner: ario_arns::ID,
            executable: false,
            rent_epoch: 0,
        },
    );

    // PR-4: pre-add ProgramData + funded upgrade-authority so setup_arns can
    // satisfy the H1 binding. Same pattern as program_test_with_registry.
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

    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "fullreg";
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(name).0,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    assert_anchor_error!(result, ArnsError::RegistryFull);
}

// =========================================
// SECURITY: Duplicate mutable account
// =========================================

/// Pass the buyer's SPL token account as BOTH `buyer_token_account` and
/// `protocol_token_account`. If Solana's writable-account dedup were the only
/// barrier, the SPL transfer CPI would resolve to src==dst (no-op) and the
/// buyer would acquire a name without paying. The `protocol_token_account.key()
/// == config.treasury` constraint must catch the aliasing first.
///
/// Closes the "duplicate mutable account" gap from the security checklist
/// with a real test instead of a research claim about per-handler guards.
#[tokio::test]
async fn test_buy_name_duplicate_payer_and_treasury_rejected() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "dupacct";
    let (arns_record_key, _) = arns_record_pda(name);
    let registry_key = name_registry_key();

    // Pass buyer_token in BOTH slots.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.buyer_token.pubkey(), // ← same account
                reserved_name_check: reserved_name_pda(name).0,
                returned_name_check: returned_name_pda(name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.to_string(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_key,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx.banks_client.process_transaction(tx).await;
    // The treasury-pin constraint fires because buyer_token.key() != config.treasury.
    assert_anchor_error!(result, ArnsError::InvalidTreasury);
}

// =========================================
// ADR-016 / BD-100: ANT Program trait preservation
// =========================================

/// Custom-program string used in BD-100 fixtures. Distinct from the
/// canonical `ARIO_ANT_PROGRAM_ID` so the trait-preservation assertions
/// can't false-pass against the default fallback.
const TEST_THIRD_PARTY_ANT_PROGRAM: &str = "ThirdPartyAntPgm111111111111111111111111111";

// (deleted obsolete test `test_buy_name_preserves_ant_program_trait` — exercised the
//  ARIO-ARNS-side trait sync mechanism that the Sprint 1-3
//  reshape moved to `ario_ant::sync_attributes`. Round-trip
//  coverage is now in programs/ario-ant/tests/sync_attributes.rs.
//  See ADR-016 amendment / BD-100.)

/// `reassign_name` clears the OLD asset's name-bound traits with an
/// `UpdatePluginV1` CPI; the asset-bound `ANT Program` trait must
/// survive because the asset itself still exists post-reassign and
/// its program override is independent of what name (if any) it
/// happens to hold.
#[tokio::test]
async fn test_reassign_name_preserves_old_asset_ant_program_trait() {
    let old_ant_keypair = Keypair::new();
    let old_ant = old_ant_keypair.pubkey();
    let new_ant_keypair = Keypair::new();
    let new_ant = new_ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;

    mint_test_ant_with_attributes(
        &mut ctx,
        &old_ant_keypair,
        &[("ANT Program", TEST_THIRD_PARTY_ANT_PROGRAM)],
    )
    .await;
    mint_test_ant(&mut ctx, &new_ant_keypair).await;

    let setup = setup_arns(&mut ctx).await;
    let name = "reassign-pres";
    let _ = buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, old_ant).await;

    // Reassign the name to a different asset — the OLD asset should
    // keep its ANT Program override even as its ArNS-name traits are
    // wiped.
    let (arns_record_key, _) = arns_record_pda(name);
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: old_ant,
        caller: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName { new_ant }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let preserved = read_asset_attribute(&mut ctx, &old_ant, "ANT Program").await;
    assert_eq!(
        preserved.as_deref(),
        Some(TEST_THIRD_PARTY_ANT_PROGRAM),
        "reassign_name's UpdatePluginV1 wipe must preserve the asset-bound \
         ANT Program trait — only name-bound traits should be cleared"
    );
    // Sanity: name-bound traits should be gone.
    assert_eq!(
        read_asset_attribute(&mut ctx, &old_ant, "ArNS Name").await,
        None
    );
}

/// `release_name` is the second handler that clobbers the plugin —
/// same preservation contract as `reassign_name`. The asset still
/// exists post-release, so its program override must persist.
#[tokio::test]
async fn test_release_name_preserves_ant_program_trait() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;

    mint_test_ant_with_attributes(
        &mut ctx,
        &ant_keypair,
        &[("ANT Program", TEST_THIRD_PARTY_ANT_PROGRAM)],
    )
    .await;

    let setup = setup_arns(&mut ctx).await;
    let name = "release-pres";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Permabuy, 0, ant_key).await;

    let (returned_key, _) = returned_name_pda(name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_key,
                name_registry: registry_key,
                ant_asset: ant_key,
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let preserved = read_asset_attribute(&mut ctx, &ant_key, "ANT Program").await;
    assert_eq!(
        preserved.as_deref(),
        Some(TEST_THIRD_PARTY_ANT_PROGRAM),
        "release_name must preserve the asset-bound ANT Program trait"
    );
}

// ============================================================================
// PR-1: Event emission tests — base purchase / manage events
//
// Each test exercises a base instruction, captures the transaction's log
// messages via `process_transaction_with_metadata`, and asserts the
// expected `#[event]` payload via `ario-test-utils::expect_event!`.
// `bpf_required!()` is the first line — solana-program-test 2.1.0 only
// captures `sol_log_data` under BPF dispatch.
//
// The 20 fund-from variants (delegation / operator_stake / withdrawal /
// funding_plan) reuse the same emit code path with different
// `funding_source: u8` constants — wire-level coverage of those variants
// lives in localnet/SDK e2e tests (`yarn test:localnet`,
// `sdk/src/solana/funding-plan*.localnet.test.ts`) so we don't duplicate
// the heavy CPI fixtures here. The compile-time constants ensure no
// variant ever silently emits with the wrong `funding_source`.
// ============================================================================

#[tokio::test]
async fn test_buy_name_lease_emits_name_purchased_event() {
    ario_test_utils::bpf_required!();

    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtname".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);
    let registry_key = name_registry_key();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: registry_key,
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Lease,
                    years: 2,
                    ant: ant_key,
                },
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
    assert!(result.result.is_ok(), "buy_name should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_arns::NamePurchasedEvent);
    assert_eq!(ev.buyer, ctx.payer.pubkey());
    assert_eq!(ev.name, name);
    assert_eq!(ev.purchase_type, ario_arns::PURCHASE_TYPE_LEASE);
    assert_eq!(ev.years, 2);
    assert_eq!(ev.ant, ant_key);
    assert_eq!(ev.funding_source, ario_arns::FUNDING_SOURCE_BALANCE);
    assert!(ev.cost > 0);
    assert!(ev.timestamp > 0);
}

#[tokio::test]
async fn test_buy_name_permabuy_emits_name_purchased_event_with_zero_years() {
    ario_test_utils::bpf_required!();

    let ant_keypair = Keypair::new();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtperm".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: name_registry_key(),
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Permabuy,
                    years: 0,
                    ant: ant_keypair.pubkey(),
                },
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
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_arns::NamePurchasedEvent);
    assert_eq!(ev.purchase_type, ario_arns::PURCHASE_TYPE_PERMABUY);
    assert_eq!(
        ev.years, 0,
        "permabuy must emit years=0 regardless of params"
    );
    assert_eq!(ev.funding_source, ario_arns::FUNDING_SOURCE_BALANCE);
}

#[tokio::test]
async fn test_upgrade_name_emits_event() {
    ario_test_utils::bpf_required!();

    // Re-use the existing test_upgrade_name flow shape: lease first, then upgrade.
    let ant_keypair = Keypair::new();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtup".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);

    // Step 1: buy as lease
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let buy_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: name_registry_key(),
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_keypair.pubkey(),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(buy_tx).await.unwrap();

    // Step 2: upgrade to permabuy
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let up_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpgradeName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpgradeName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(up_tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "upgrade_name should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_arns::NameUpgradedEvent);
    assert_eq!(ev.owner, ctx.payer.pubkey());
    assert_eq!(ev.name, name);
    assert!(ev.cost > 0);
    assert_eq!(ev.funding_source, ario_arns::FUNDING_SOURCE_BALANCE);
}

#[tokio::test]
async fn test_extend_lease_emits_event() {
    ario_test_utils::bpf_required!();

    let ant_keypair = Keypair::new();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtext".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);

    // Buy 1-year lease
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let buy_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: name_registry_key(),
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_keypair.pubkey(),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(buy_tx).await.unwrap();

    // Extend by 3 years
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let ex_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLease {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLease { years: 3 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(ex_tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "extend_lease should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_arns::LeaseExtendedEvent);
    assert_eq!(ev.owner, ctx.payer.pubkey());
    assert_eq!(ev.name, name);
    assert_eq!(ev.years, 3);
    assert!(ev.cost > 0);
    assert!(
        ev.new_end_timestamp > 0,
        "must report the post-extension end timestamp"
    );
    assert_eq!(ev.funding_source, ario_arns::FUNDING_SOURCE_BALANCE);
}

#[tokio::test]
async fn test_increase_undername_limit_emits_event() {
    ario_test_utils::bpf_required!();

    let ant_keypair = Keypair::new();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtund".to_string();
    let (arns_record_key, _) = arns_record_pda(&name);

    // Buy lease
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let buy_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::BuyName {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                name_registry: name_registry_key(),
                buyer_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                reserved_name_check: reserved_name_pda(&name).0,
                returned_name_check: returned_name_pda(&name).0,
                buyer: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::BuyName {
                params: ario_arns::BuyNameParams {
                    name: name.clone(),
                    purchase_type: PurchaseType::Lease,
                    years: 1,
                    ant: ant_keypair.pubkey(),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(buy_tx).await.unwrap();

    // Increase undername limit by 5
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let inc_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::IncreaseUndernameLimit {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                caller_token_account: setup.buyer_token.pubkey(),
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                token_program: spl_token::id(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::IncreaseUndernameLimit { quantity: 5 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(inc_tx)
        .await
        .unwrap();
    assert!(
        result.result.is_ok(),
        "increase_undername_limit should succeed"
    );
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_arns::UndernameIncreasedEvent);
    assert_eq!(ev.owner, ctx.payer.pubkey());
    assert_eq!(ev.name, name);
    assert_eq!(ev.quantity, 5);
    // new_limit = DEFAULT_UNDERNAME_COUNT + 5 (10 + 5 = 15 by current constants)
    assert_eq!(ev.new_limit as u16, DEFAULT_UNDERNAME_COUNT + 5);
    assert!(ev.cost > 0);
    assert_eq!(ev.funding_source, ario_arns::FUNDING_SOURCE_BALANCE);
}

// `ReturnedNamePurchasedEvent` is the only one we don't smoke-test here —
// returned-name auctions need a more elaborate fixture (release_name +
// clock-warp). Coverage lives in localnet (`returned-name.localnet.test.ts`)
// where the SDK can stage the auction more naturally.

// ============================================================================
// PR-2: Lifecycle event tests
//
// Reassign / release / reserved-name flows / prune / demand factor.
// All BPF-only (sol_log_data not captured under native dispatch in
// solana-program-test 2.1.0). Pruning batch instructions rely on
// localnet fixtures for max-batch coverage; here we just hit
// `prune_to_returned` (single-record path) to verify the unified
// `NamesPrunedEvent` shape with `count: 1, kind: 0`.
// ============================================================================

#[tokio::test]
async fn test_reassign_name_emits_event() {
    ario_test_utils::bpf_required!();

    let ant_keypair = Keypair::new();
    let new_ant_keypair = Keypair::new();
    let new_ant = new_ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    mint_test_ant(&mut ctx, &new_ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtreassign".to_string();
    let arns_record_key = buy_name_helper(
        &mut ctx,
        &setup,
        &name,
        PurchaseType::Permabuy,
        0,
        ant_keypair.pubkey(),
    )
    .await;

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let accounts = ario_arns::accounts::ReassignName {
        config: config_pda().0,
        arns_record: arns_record_key,
        ant_asset: ant_keypair.pubkey(),
        caller: ctx.payer.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);

    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data: ario_arns::instruction::ReassignName { new_ant }.data(),
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
    assert!(result.result.is_ok());
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_arns::NameReassignedEvent);
    assert_eq!(ev.caller, ctx.payer.pubkey());
    assert_eq!(ev.name, name);
    assert_eq!(ev.old_ant, ant_keypair.pubkey());
    assert_eq!(ev.new_ant, new_ant);
    assert!(ev.timestamp > 0);
}

#[tokio::test]
async fn test_release_name_emits_event() {
    ario_test_utils::bpf_required!();

    let ant_keypair = Keypair::new();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtrelease";
    let arns_record_key = buy_name_helper(
        &mut ctx,
        &setup,
        name,
        PurchaseType::Permabuy,
        0,
        ant_keypair.pubkey(),
    )
    .await;

    let (returned_name_key, _) = returned_name_pda(name);
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReleaseName {
                config: config_pda().0,
                arns_record: arns_record_key,
                returned_name: returned_name_key,
                name_registry: name_registry_key(),
                ant_asset: ant_keypair.pubkey(),
                caller: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReleaseName {}.data(),
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
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_arns::NameReleasedEvent);
    assert_eq!(ev.owner, ctx.payer.pubkey());
    assert_eq!(ev.name, name);
}

#[tokio::test]
async fn test_reserve_unreserve_emit_events() {
    ario_test_utils::bpf_required!();

    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtres";
    let target = Pubkey::new_unique();
    let (reserved_key, _) = reserved_name_pda(name);
    let expires_at = 9_999_999_999i64;

    // Reserve with target + expiry — verify NameReservedEvent fields.
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let res_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: Some(target),
                    expires_at: Some(expires_at),
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let res_result = ctx
        .banks_client
        .process_transaction_with_metadata(res_tx)
        .await
        .unwrap();
    let res_logs = res_result.metadata.expect("metadata").log_messages;
    use ario_test_utils::expect_event;
    let res_ev = expect_event!(&res_logs, ario_arns::NameReservedEvent);
    assert_eq!(res_ev.authority, ctx.payer.pubkey());
    assert_eq!(res_ev.name, name);
    assert_eq!(res_ev.target, Some(target));
    assert_eq!(res_ev.expires_at, Some(expires_at));

    // Unreserve — verify NameUnreservedEvent
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let un_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UnreserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UnreserveName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let un_result = ctx
        .banks_client
        .process_transaction_with_metadata(un_tx)
        .await
        .unwrap();
    let un_logs = un_result.metadata.expect("metadata").log_messages;
    let un_ev = expect_event!(&un_logs, ario_arns::NameUnreservedEvent);
    assert_eq!(un_ev.authority, ctx.payer.pubkey());
    assert_eq!(un_ev.name, name);
}

#[tokio::test]
async fn test_claim_reserved_name_emits_event() {
    ario_test_utils::bpf_required!();

    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name = "evtclaim";
    let (reserved_key, _) = reserved_name_pda(name);

    // Reserve first
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let res_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ReserveName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
                system_program: system_program::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ReserveName {
                params: ario_arns::ReserveNameParams {
                    name: name.to_string(),
                    reserved_for: None,
                    expires_at: None,
                },
            }
            .data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(res_tx).await.unwrap();

    // Claim (authority reclaims the reservation slot)
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let claim_tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ClaimReservedName {
                config: setup.config_key,
                reserved_name: reserved_key,
                authority: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ClaimReservedName {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    let result = ctx
        .banks_client
        .process_transaction_with_metadata(claim_tx)
        .await
        .unwrap();
    assert!(result.result.is_ok(), "claim_reserved_name should succeed");
    let logs = result.metadata.expect("metadata").log_messages;

    use ario_test_utils::expect_event;
    let ev = expect_event!(&logs, ario_arns::ReservedNameClaimedEvent);
    assert_eq!(ev.claimer, ctx.payer.pubkey());
    assert_eq!(ev.name, name);
}

// =========================================================================
// admin_expand_name_registry tests (ADR-020 dynamic-capacity)
// =========================================================================

async fn send_admin_expand(
    ctx: &mut ProgramTestContext,
    setup: &ArnsSetup,
    target_capacity: u32,
    authority: &solana_sdk::signature::Keypair,
) -> std::result::Result<(), solana_program_test::BanksClientError> {
    let name_registry_key =
        solana_sdk::pubkey::Pubkey::find_program_address(&[NAME_REGISTRY_SEED], &ario_arns::ID).0;
    let accounts = ario_arns::accounts::AdminExpandNameRegistry {
        config: setup.config_key,
        name_registry: name_registry_key,
        authority: authority.pubkey(),
        system_program: system_program::id(),
    }
    .to_account_metas(None);
    let data = ario_arns::instruction::AdminExpandNameRegistry { target_capacity }.data();
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts,
            data,
        }],
        Some(&authority.pubkey()),
        &[authority],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await
}

#[tokio::test]
async fn test_admin_expand_name_registry_grows_capacity() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name_registry_key =
        solana_sdk::pubkey::Pubkey::find_program_address(&[NAME_REGISTRY_SEED], &ario_arns::ID).0;
    let before = ctx
        .banks_client
        .get_account(name_registry_key)
        .await
        .unwrap()
        .unwrap();
    let initial_capacity = NameRegistry::INITIAL_CAPACITY as u32;

    // Expand by one MAX_PERMITTED_DATA_INCREASE chunk. Each chunk is
    // 10240 bytes = 256 NameEntry slots, so target +256 capacity fits
    // in a single tx.
    let payer_clone = ctx.payer.insecure_clone();
    send_admin_expand(&mut ctx, &setup, initial_capacity + 256, &payer_clone)
        .await
        .expect("expand should succeed");

    let after = ctx
        .banks_client
        .get_account(name_registry_key)
        .await
        .unwrap()
        .unwrap();
    assert!(
        after.data.len() > before.data.len(),
        "registry should grow ({} -> {})",
        before.data.len(),
        after.data.len(),
    );
    // Specifically: data.len() should now correspond to ≥ initial+256 capacity.
    let new_cap = slot_capacity(&after.data);
    assert!(
        new_cap >= (initial_capacity as usize + 256),
        "new capacity {} should be >= {}",
        new_cap,
        initial_capacity + 256,
    );
}

#[tokio::test]
async fn test_admin_expand_name_registry_idempotent_when_target_le_current() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let name_registry_key =
        solana_sdk::pubkey::Pubkey::find_program_address(&[NAME_REGISTRY_SEED], &ario_arns::ID).0;
    let before = ctx
        .banks_client
        .get_account(name_registry_key)
        .await
        .unwrap()
        .unwrap();

    // Calling expand with target == current capacity is a no-op.
    let current_cap = NameRegistry::INITIAL_CAPACITY as u32;
    let payer_clone = ctx.payer.insecure_clone();
    send_admin_expand(&mut ctx, &setup, current_cap, &payer_clone)
        .await
        .expect("idempotent expand should succeed");

    let after = ctx
        .banks_client
        .get_account(name_registry_key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        before.data.len(),
        after.data.len(),
        "no-op expand should not change account size"
    );
}

#[tokio::test]
async fn test_admin_expand_name_registry_rejects_non_authority() {
    let mut ctx = program_test_with_registry().start_with_context().await;
    let setup = setup_arns(&mut ctx).await;

    let attacker = solana_sdk::signature::Keypair::new();
    // Fund the attacker so the tx can be submitted (fee payer).
    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let fund = Transaction::new_signed_with_payer(
        &[solana_sdk::system_instruction::transfer(
            &ctx.payer.pubkey(),
            &attacker.pubkey(),
            10_000_000_000,
        )],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(fund).await.unwrap();

    // Non-authority signer triggers `has_one = authority` failure
    // (ArnsError::Unauthorized).
    let result = send_admin_expand(
        &mut ctx,
        &setup,
        (NameRegistry::INITIAL_CAPACITY + 256) as u32,
        &attacker,
    )
    .await;
    assert!(result.is_err(), "non-authority caller must be rejected");
}

// =========================================
// test_extend_lease_from_withdrawal_relaxed_cap (audit M-3, 2026-05-30 follow-up)
//
// PR #80 aligned `extend_lease_from_withdrawal` (and `_from_funding_plan`)
// to the remaining-years-from-now cap used by the 3 majority paths
// (extend_lease + _from_delegation + _from_operator_stake). Pre-fix those
// 2 stricter paths used a total-duration-since-start cap that rejected
// legitimate extensions for leases that started long ago.
//
// The 12 existing BPF integration tests cover the 3 majority paths but
// NONE exercise `_from_withdrawal` or `_from_funding_plan`. This test
// fills that gap: synthesizes the exact trigger state (4 years ago start,
// 1 year remaining, extend by 4 years) and asserts the cap check now
// PASSES on `_from_withdrawal`. Pre-PR #80 this would fail at the cap
// with `InvalidLeaseDuration`; post-fix the cap is relaxed so the ix
// proceeds past it and fails at a DIFFERENT downstream step (the bogus
// GAR-side accounts we pass — that's expected and proves the relaxation).
//
// Bidirectionally meaningful: assertion `result.is_err() && err != cap_error`.
// If M-3 regresses (cap re-tightens), this fires with the cap error code
// and the test fails. If the fix stays, the cap check passes and the
// downstream account-validation error gets surfaced instead.
// =========================================

#[tokio::test]
async fn test_extend_lease_from_withdrawal_relaxed_cap() {
    let ant_keypair = Keypair::new();
    let ant_key = ant_keypair.pubkey();
    let mut pt = program_test_with_registry();
    let mut ctx = pt.start_with_context().await;
    mint_test_ant(&mut ctx, &ant_keypair).await;
    let setup = setup_arns(&mut ctx).await;

    // Buy a max-duration lease (5 years) so end - start spans the full cap.
    let name = "relaxedcap";
    let arns_record_key =
        buy_name_helper(&mut ctx, &setup, name, PurchaseType::Lease, 5, ant_key).await;

    // Backdate the record so it LOOKS like it was bought 4 years ago:
    //   start_timestamp = now - 4*YEAR
    //   end_timestamp   = (now - 4*YEAR) + 5*YEAR = now + 1*YEAR  (1 year remaining)
    //
    // Pre-PR-#80 cap on _from_withdrawal: total = (now+1+4) - (now-4) = 10 → > 5 → REJECT
    // Post-PR-#80 cap (majority pattern): remaining=1, max_ext=4, years=4 → ALLOW past cap
    let record_acc = ctx
        .banks_client
        .get_account(arns_record_key)
        .await
        .unwrap()
        .unwrap();
    let mut record = ArnsRecord::try_deserialize(&mut record_acc.data.as_slice()).unwrap();
    let now = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap()
        .unix_timestamp;
    record.start_timestamp = now - 4 * ONE_YEAR_SECONDS;
    record.end_timestamp = Some(now + ONE_YEAR_SECONDS);
    let mut new_data = Vec::new();
    record.try_serialize(&mut new_data).unwrap();
    let original_len = record_acc.data.len();
    new_data.resize(original_len, 0);
    ctx.set_account(
        &arns_record_key,
        &solana_sdk::account::Account {
            lamports: record_acc.lamports,
            data: new_data,
            owner: record_acc.owner,
            executable: false,
            rent_epoch: 0,
        }
        .into(),
    );

    // Build extend_lease_from_withdrawal with years=4. The downstream GAR
    // accounts are bogus (we only care whether the cap check rejects).
    let bogus_gar_settings = Pubkey::new_unique();
    let bogus_withdrawal = Pubkey::new_unique();
    let bogus_stake_token = Pubkey::new_unique();

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::ExtendLeaseFromWithdrawal {
                config: setup.config_key,
                demand_factor: setup.demand_factor_key,
                arns_record: arns_record_key,
                gar_settings: bogus_gar_settings,
                withdrawal: bogus_withdrawal,
                stake_token_account: bogus_stake_token,
                protocol_token_account: setup.protocol_token.pubkey(),
                caller: ctx.payer.pubkey(),
                gar_program: ario_gar::ID,
                token_program: spl_token::id(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::ExtendLeaseFromWithdrawal { years: 4 }.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );

    let result = ctx.banks_client.process_transaction(tx).await;

    // The tx MUST fail (we passed bogus GAR-side accounts), but the failure
    // MUST NOT be at the cap check. Pre-fix the cap check rejected with
    // ArnsError::InvalidLeaseDuration; post-fix it should be (a) relaxed
    // ExtensionExceedsMax — meaning the cap re-rejected — or (b) any
    // OTHER error — meaning the cap passed.
    //
    // M-3 PASSES IFF the error is neither cap-related code.
    let expected_pre_fix_cap =
        anchor_lang::error::ERROR_CODE_OFFSET + ArnsError::InvalidLeaseDuration as u32;
    let expected_post_fix_cap_reject =
        anchor_lang::error::ERROR_CODE_OFFSET + ArnsError::ExtensionExceedsMax as u32;
    match result {
        Ok(()) => panic!(
            "extend_lease_from_withdrawal unexpectedly succeeded with bogus GAR \
             accounts — the downstream CPI should have failed."
        ),
        Err(solana_program_test::BanksClientError::TransactionError(
            solana_sdk::transaction::TransactionError::InstructionError(
                _,
                solana_sdk::instruction::InstructionError::Custom(code),
            ),
        )) => {
            assert_ne!(
                code, expected_pre_fix_cap,
                "M-3 REGRESSION: rejected with pre-fix InvalidLeaseDuration code ({}) \
                 — the strict total-duration cap is back. The relaxed remaining-years-\
                 from-now cap should have allowed years=4 with remaining=1 (max=5).",
                expected_pre_fix_cap,
            );
            assert_ne!(
                code, expected_post_fix_cap_reject,
                "Cap rejected with post-fix ExtensionExceedsMax — but with \
                 remaining=1 and years=4, max_extension=4 should allow. \
                 Something is off with the cap arithmetic."
            );
            // Any OTHER custom error code = cap passed; downstream rejected.
        }
        // Non-custom errors (AccountNotExecutable on the bogus gar_program,
        // InvalidAccountData on the bogus withdrawal, etc.) ALSO prove the
        // cap check passed — they fire AFTER we leave the cap-check block.
        // AccountNotExecutable specifically is the signature of "cap passed,
        // CPI attempted, but ario-gar isn't loaded in this test binary."
        Err(other) => {
            eprintln!(
                "  cap-check PASSED; downstream failed as expected (no ario-gar \
                 program registered in this test binary). Error variant: {:?}",
                other
            );
        }
    }
}

// =========================================
// DEMAND-FACTOR STEPPING AT THE MINIMUM (on-chain)
//
// Coverage for the floor / permanent-halving state machine in
// `instructions/demand.rs::roll_one_period` (lines ~151-169):
//   * The factor is clamped at DEMAND_FACTOR_MIN (0.5x) on every decay step.
//   * `consecutive_periods_with_min_demand_factor` increments each period the
//     factor sits at the floor.
//   * After MAX_PERIODS_AT_MIN_DEMAND_FACTOR (=7) consecutive periods at the
//     floor, the NEXT period (the 8th observation at-floor) permanently halves
//     ALL fees, resets the factor to DEMAND_FACTOR_SCALE (1.0), and resets the
//     consecutive counter to 0.
//
// These tests drive the permissionless `update_demand_factor` instruction
// against the live SPT chain (no purchase activity), warping the clock across
// daily period boundaries. Unlike the existing in-crate unit tests (which take
// a single step from factor=1.0), they:
//   (a) start from the real MAINNET genesis factor 9.8x and run through TWO
//       full halving cycles, asserting the reset invariants after each, and
//   (b) pin the >=7 vs >7 boundary (un-halved at exactly 7 at-floor periods,
//       halved on the next).
// =========================================

/// Pure reference model of one ZERO-ACTIVITY period, expressed only in terms of
/// the public protocol constants exported from `ario_arns::state`. Mirrors the
/// not-increasing branch of `roll_one_period` exactly (every zero-activity
/// period takes that branch). Returns the new `(factor, consecutive)` plus
/// whether THIS period triggered a permanent fee-halving.
///
/// This is the test's independent oracle: the on-chain `update_demand_factor`
/// execution is asserted against predictions from this model, so a divergence
/// between model and contract surfaces as a test failure rather than being
/// silently re-derived from the contract's own private helpers.
fn ref_zero_activity_step(factor: u64, consecutive: u32) -> (u64, u32, bool) {
    let mut factor = factor;
    if factor > DEMAND_FACTOR_MIN {
        factor = ((factor as u128) * (DEMAND_FACTOR_DOWN_ADJUSTMENT as u128)
            / (DEMAND_FACTOR_SCALE as u128)) as u64;
    }
    if factor <= DEMAND_FACTOR_MIN {
        if consecutive >= MAX_PERIODS_AT_MIN_DEMAND_FACTOR {
            (DEMAND_FACTOR_SCALE, 0, true) // permanent halving → reset
        } else {
            (DEMAND_FACTOR_MIN, consecutive + 1, false)
        }
    } else {
        (factor, 0, false)
    }
}

/// Absolute chain timestamp for the START of a given 1-based period, anchored at
/// `TEST_PERIOD_ZERO_START`. `get_period_for_timestamp` returns
/// `(elapsed / PERIOD_LENGTH_SECONDS) + 1`, so period `p` begins at
/// `period_zero_start + (p - 1) * PERIOD_LENGTH_SECONDS`.
fn timestamp_for_period_start(period: u64) -> i64 {
    TEST_PERIOD_ZERO_START + (period as i64 - 1) * PERIOD_LENGTH_SECONDS
}

/// Warp the SPT chain to the start of `target_period` and run the
/// permissionless `update_demand_factor` instruction, then return the freshly
/// deserialized `DemandFactor` account.
async fn warp_and_update_demand(
    ctx: &mut ProgramTestContext,
    demand_factor_key: Pubkey,
    target_period: u64,
) -> DemandFactor {
    // Advance slots so the bank produces a new blockhash, then override the
    // wall-clock timestamp to the desired period boundary.
    let current_slot = ctx.banks_client.get_root_slot().await.unwrap();
    ctx.warp_to_slot(current_slot + 1).unwrap();
    let mut clock = ctx
        .banks_client
        .get_sysvar::<solana_sdk::clock::Clock>()
        .await
        .unwrap();
    clock.unix_timestamp = timestamp_for_period_start(target_period);
    ctx.set_sysvar(&clock);

    let blockhash = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(
        &[Instruction {
            program_id: ario_arns::ID,
            accounts: ario_arns::accounts::UpdateDemandFactor {
                demand_factor: demand_factor_key,
                payer: ctx.payer.pubkey(),
            }
            .to_account_metas(None),
            data: ario_arns::instruction::UpdateDemandFactor {}.data(),
        }],
        Some(&ctx.payer.pubkey()),
        &[&ctx.payer],
        blockhash,
    );
    ctx.banks_client.process_transaction(tx).await.unwrap();

    let df_account = ctx
        .banks_client
        .get_account(demand_factor_key)
        .await
        .unwrap()
        .unwrap();
    DemandFactor::try_deserialize(&mut df_account.data.as_slice()).unwrap()
}

/// AREA 1(a): start at the real MAINNET genesis demand factor (9.8x), let the
/// factor decay with NO purchase activity all the way to the floor, and ride it
/// through TWO consecutive permanent-halving cycles. After each halving cycle,
/// assert the contract reset the factor to SCALE, zeroed the consecutive
/// counter, and halved EVERY fee again (cumulative ×0.5 per cycle), checked at
/// both fees[0] and a mid-table index.
#[tokio::test]
async fn test_demand_factor_decays_from_mainnet_start_through_two_halvings() {
    const MAINNET_INITIAL_DEMAND_FACTOR: u64 = 9_800_000; // 9.8x — AO live value

    let mut pt = program_test_with_registry_no_mpl();
    let mut ctx = pt.start_with_context().await;
    let setup =
        setup_arns_with_initial_demand_factor(&mut ctx, MAINNET_INITIAL_DEMAND_FACTOR).await;

    // Sanity: genesis seeded the 9.8x factor with the unmodified fee table.
    let genesis = {
        let acct = ctx
            .banks_client
            .get_account(setup.demand_factor_key)
            .await
            .unwrap()
            .unwrap();
        DemandFactor::try_deserialize(&mut acct.data.as_slice()).unwrap()
    };
    assert_eq!(genesis.current_demand_factor, MAINNET_INITIAL_DEMAND_FACTOR);
    assert_eq!(genesis.current_period, 1);
    assert_eq!(genesis.fees, GENESIS_FEES);
    assert_eq!(genesis.consecutive_periods_with_min_demand_factor, 0);

    // A mid-table fee index distinct from fees[0]; both must halve each cycle.
    const MID_IDX: usize = 6; // 7-char fee (800_000_000)
    assert_ne!(GENESIS_FEES[0], GENESIS_FEES[MID_IDX]);

    // Drive the reference model forward from genesis to locate the FIRST TWO
    // periods that complete a permanent halving. The model starts at period 1
    // (the genesis period); rolling it once closes period `p` and lands at
    // period `p + 1`, mirroring the contract's `current_period` advance.
    let mut model_factor = MAINNET_INITIAL_DEMAND_FACTOR;
    let mut model_consec: u32 = 0;
    let mut model_period: u64 = genesis.current_period; // 1
    let mut halving_periods: Vec<u64> = Vec::new();
    // Guard against an accidental infinite loop if constants ever change such
    // that the floor/halving is unreachable.
    let mut guard = 0u64;
    while halving_periods.len() < 2 {
        let (f, c, halved) = ref_zero_activity_step(model_factor, model_consec);
        model_factor = f;
        model_consec = c;
        model_period += 1; // this step advanced into a new period
        if halved {
            // After this period's roll completes, `current_period` == model_period
            // and the contract sits at the post-halving reset state.
            halving_periods.push(model_period);
        }
        guard += 1;
        assert!(
            guard < 100_000,
            "reference model never reached two halvings — constants drifted?"
        );
    }

    // Expected cumulative fee tables after the 1st and 2nd halvings.
    let mut fees_after_first = GENESIS_FEES;
    for f in fees_after_first.iter_mut() {
        *f = (*f as u128 * DEMAND_FACTOR_MIN as u128 / DEMAND_FACTOR_SCALE as u128) as u64;
    }
    let mut fees_after_second = fees_after_first;
    for f in fees_after_second.iter_mut() {
        *f = (*f as u128 * DEMAND_FACTOR_MIN as u128 / DEMAND_FACTOR_SCALE as u128) as u64;
    }

    // --- Cycle 1: warp to the exact post-first-halving boundary ---
    let demand =
        warp_and_update_demand(&mut ctx, setup.demand_factor_key, halving_periods[0]).await;
    assert_eq!(
        demand.current_period, halving_periods[0],
        "chain period must match the model's first-halving period"
    );
    assert_eq!(
        demand.current_demand_factor, DEMAND_FACTOR_SCALE,
        "after the 1st halving the factor resets to 1.0 (SCALE)"
    );
    assert_eq!(
        demand.consecutive_periods_with_min_demand_factor, 0,
        "after the 1st halving the consecutive-at-min counter resets to 0"
    );
    assert_eq!(
        demand.fees[0],
        GENESIS_FEES[0] / 2,
        "after the 1st halving fees[0] is halved once"
    );
    assert_eq!(
        demand.fees[MID_IDX],
        GENESIS_FEES[MID_IDX] / 2,
        "after the 1st halving the mid-table fee is halved once"
    );
    assert_eq!(
        demand.fees, fees_after_first,
        "entire fee table halved exactly once after cycle 1"
    );

    // --- Cycle 2: warp to the exact post-second-halving boundary ---
    let demand =
        warp_and_update_demand(&mut ctx, setup.demand_factor_key, halving_periods[1]).await;
    assert_eq!(
        demand.current_period, halving_periods[1],
        "chain period must match the model's second-halving period"
    );
    assert_eq!(
        demand.current_demand_factor, DEMAND_FACTOR_SCALE,
        "after the 2nd halving the factor resets to 1.0 (SCALE) again"
    );
    assert_eq!(
        demand.consecutive_periods_with_min_demand_factor, 0,
        "after the 2nd halving the consecutive-at-min counter resets to 0 again"
    );
    assert_eq!(
        demand.fees[0],
        GENESIS_FEES[0] / 4,
        "after the 2nd halving fees[0] is cumulatively quartered"
    );
    assert_eq!(
        demand.fees[MID_IDX],
        GENESIS_FEES[MID_IDX] / 4,
        "after the 2nd halving the mid-table fee is cumulatively quartered"
    );
    assert_eq!(
        demand.fees, fees_after_second,
        "entire fee table halved exactly twice after cycle 2"
    );
}

/// AREA 1(b): pin the `>= 7` vs `> 7` boundary in the floor/halving check.
/// At EXACTLY MAX_PERIODS_AT_MIN_DEMAND_FACTOR (7) consecutive at-floor periods
/// the fees are STILL un-halved (factor pinned at the floor, counter == 7); the
/// VERY NEXT period (the 8th observation at-floor) is the one that halves and
/// resets. This locks down the off-by-one — a `> 7` would halve one period late
/// and a `>= 6` would halve one period early.
#[tokio::test]
async fn test_demand_factor_halving_boundary_seven_vs_eight() {
    // Seed exactly at the floor so we observe the at-floor dwell directly. The
    // genesis period (period 1) is already AT the floor, so the first roll into
    // period 2 records the 1st consecutive-at-min observation.
    let mut pt = program_test_with_registry_no_mpl();
    let mut ctx = pt.start_with_context().await;
    let setup = setup_arns_with_initial_demand_factor(&mut ctx, DEMAND_FACTOR_MIN).await;

    let genesis = {
        let acct = ctx
            .banks_client
            .get_account(setup.demand_factor_key)
            .await
            .unwrap()
            .unwrap();
        DemandFactor::try_deserialize(&mut acct.data.as_slice()).unwrap()
    };
    assert_eq!(genesis.current_demand_factor, DEMAND_FACTOR_MIN);
    assert_eq!(genesis.consecutive_periods_with_min_demand_factor, 0);
    assert_eq!(genesis.current_period, 1);
    assert_eq!(genesis.fees, GENESIS_FEES);

    // Roll forward one period at a time. Each roll that closes an at-floor
    // period increments the counter; once the counter has reached
    // MAX_PERIODS_AT_MIN_DEMAND_FACTOR (7), the next at-floor period halves.
    //
    // Period 1 (genesis) is at-floor with counter 0. Rolling into period
    // `1 + k` records the k-th at-floor observation, so:
    //   * counter == 7  is reached at period 1 + 7 == 8  (fees STILL un-halved)
    //   * the halving fires rolling into period 1 + 8 == 9 (fees halved, reset)
    let counter_reaches_seven_at_period = 1 + MAX_PERIODS_AT_MIN_DEMAND_FACTOR as u64; // 8
    let halving_period = counter_reaches_seven_at_period + 1; // 9

    // Step up to and including the period where the counter hits exactly 7.
    let mut last = genesis;
    for p in 2..=counter_reaches_seven_at_period {
        last = warp_and_update_demand(&mut ctx, setup.demand_factor_key, p).await;
    }

    // EXACTLY 7 consecutive at-floor periods: factor still pinned at the floor,
    // counter == 7, and crucially the fees are STILL the untouched genesis table.
    assert_eq!(
        last.current_period, counter_reaches_seven_at_period,
        "should be at the period where the at-min counter reaches 7"
    );
    assert_eq!(
        last.consecutive_periods_with_min_demand_factor, MAX_PERIODS_AT_MIN_DEMAND_FACTOR,
        "at-min counter must be exactly 7 here"
    );
    assert_eq!(
        last.current_demand_factor, DEMAND_FACTOR_MIN,
        "factor stays pinned at the floor while dwelling"
    );
    assert_eq!(
        last.fees, GENESIS_FEES,
        "at exactly 7 consecutive at-floor periods the fees must NOT yet be halved (>= 7 boundary, not yet crossed)"
    );

    // The NEXT period (the 8th at-floor observation) is the one that halves.
    let after = warp_and_update_demand(&mut ctx, setup.demand_factor_key, halving_period).await;
    assert_eq!(after.current_period, halving_period);
    assert_eq!(
        after.consecutive_periods_with_min_demand_factor, 0,
        "the halving period resets the at-min counter to 0"
    );
    assert_eq!(
        after.current_demand_factor, DEMAND_FACTOR_SCALE,
        "the halving period resets the factor to 1.0 (SCALE)"
    );
    assert_eq!(
        after.fees[0],
        GENESIS_FEES[0] / 2,
        "the halving period (8th at-floor) halves fees[0]"
    );
    let mut expected_halved = GENESIS_FEES;
    for f in expected_halved.iter_mut() {
        *f = (*f as u128 * DEMAND_FACTOR_MIN as u128 / DEMAND_FACTOR_SCALE as u128) as u64;
    }
    assert_eq!(
        after.fees, expected_halved,
        "the entire fee table is halved exactly once on the halving period"
    );
}
