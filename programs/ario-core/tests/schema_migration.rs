// Regression tests for the schema-migration grow-then-deserialize pattern.
//
// Before the fix, `migrate_*` loaded the target as a typed `Account<T>`, so a
// pre-versioning (shorter) account hit Borsh EOF (AccountDidNotDeserialize,
// 3003) BEFORE the realloc could grow it — the migration was unreachable.
// These tests build a *genuine* old-layout account (old SIZE, no `version`
// bytes) and assert `migrate_*` now grows + version-stamps it successfully.

use anchor_lang::{prelude::*, Discriminator, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_core::state::*;

fn anchor_processor(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[AccountInfo] = std::mem::transmute(accounts);
        ario_core::entry(program_id, accounts, data)
    }
}

#[tokio::test]
async fn migrate_balance_grows_and_versions_pre_version_account() {
    let mut pt = ProgramTest::new("ario_core", ario_core::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let owner = Pubkey::new_unique();
    let payer = Keypair::new();
    let (balance_key, balance_bump) =
        Pubkey::find_program_address(&[BALANCE_SEED, owner.as_ref()], &ario_core::ID);

    // OLD (pre-version) Balance: disc(8) + owner(32) + amount(8) + bump(1) = 49.
    // No trailing SchemaVersion (3 bytes) — exactly what a pre-#53 account is.
    const AMOUNT: u64 = 7_654_321;
    let mut old = Vec::new();
    old.extend_from_slice(&Balance::DISCRIMINATOR);
    old.extend_from_slice(owner.as_ref());
    old.extend_from_slice(&AMOUNT.to_le_bytes());
    old.push(balance_bump);
    assert_eq!(old.len(), 49, "pre-version Balance is 49 bytes");
    assert_eq!(
        Balance::SIZE,
        52,
        "current Balance::SIZE = 49 + 3-byte version"
    );

    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        balance_key,
        solana_sdk::account::Account {
            // Intentionally rent-exempt for the OLD size only — the migration
            // must top up the delta for the grown size.
            lamports: rent.minimum_balance(old.len()),
            data: old,
            owner: ario_core::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    pt.add_account(
        payer.pubkey(),
        solana_sdk::account::Account {
            lamports: 10_000_000_000,
            data: vec![],
            owner: system_program::id(),
            executable: false,
            rent_epoch: 0,
        },
    );

    let mut ctx = pt.start_with_context().await;

    let accounts = ario_core::accounts::MigrateBalance {
        balance: balance_key,
        owner,
        payer: payer.pubkey(),
        system_program: system_program::id(),
    };
    let ix = Instruction {
        program_id: ario_core::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_core::instruction::MigrateBalance {}.data(),
    };
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("migrate_balance over a pre-version account must succeed");

    // Account grew to the new size and now deserializes with the baseline
    // version, preserving the original fields.
    let acct = ctx
        .banks_client
        .get_account(balance_key)
        .await
        .unwrap()
        .expect("balance still exists");
    assert_eq!(acct.data.len(), Balance::SIZE, "grown to new SIZE");
    let balance = Balance::try_deserialize(&mut &acct.data[..]).expect("deserializes post-migrate");
    assert_eq!(
        balance.version,
        SchemaVersion::new(1, 0, 0),
        "stamped 1.0.0"
    );
    assert_eq!(balance.amount, AMOUNT, "amount preserved");
    assert_eq!(balance.owner, owner, "owner preserved");
    assert_eq!(balance.bump, balance_bump, "bump preserved");
}
