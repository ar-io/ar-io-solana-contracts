// Genuine pre-version regression test for the ant grow-then-deserialize
// migration. ant's existing test_migrate_ant_version builds a FULL-SIZE
// account with version={0,0,0}, which does not exercise the EOF/grow path.
// This builds a real pre-version (3-bytes-short) AclConfig and asserts
// migrate_acl_config grows it, stamps 1.0.0, and preserves all fields.
//
// migrate_acl_config does not CPI Metaplex Core → native processor.

use anchor_lang::{prelude::*, Discriminator, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_ant::state::*;

fn anchor_processor(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[AccountInfo] = std::mem::transmute(accounts);
        ario_ant::entry(program_id, accounts, data)
    }
}

#[tokio::test]
async fn migrate_acl_config_grows_and_versions_pre_version_account() {
    let mut pt = ProgramTest::new("ario_ant", ario_ant::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let user = Pubkey::new_unique();
    let payer = Keypair::new();
    let (acl_key, bump) =
        Pubkey::find_program_address(&[ACL_CONFIG_SEED, user.as_ref()], &ario_ant::ID);

    // pre-version AclConfig: disc(8)+user(32)+page_count(8)+total_entries(8)+bump(1) = 57.
    let mut old = Vec::new();
    old.extend_from_slice(&AclConfig::DISCRIMINATOR);
    old.extend_from_slice(user.as_ref());
    old.extend_from_slice(&3u64.to_le_bytes()); // page_count
    old.extend_from_slice(&7u64.to_le_bytes()); // total_entries
    old.push(bump);
    assert_eq!(old.len(), 57, "pre-version AclConfig is 57 bytes");
    assert_eq!(
        AclConfig::SIZE,
        60,
        "current AclConfig::SIZE = 57 + 3-byte version"
    );

    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        acl_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(old.len()),
            data: old,
            owner: ario_ant::ID,
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

    let accounts = ario_ant::accounts::AclConfigMigration {
        user,
        acl_config: acl_key,
        payer: payer.pubkey(),
        system_program: system_program::id(),
    };
    let ix = Instruction {
        program_id: ario_ant::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_ant::instruction::MigrateAclConfig {}.data(),
    };
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("migrate_acl_config over a pre-version account must succeed");

    let acct = ctx
        .banks_client
        .get_account(acl_key)
        .await
        .unwrap()
        .expect("acl_config still exists");
    assert_eq!(acct.data.len(), AclConfig::SIZE, "grown to new SIZE");
    let got = AclConfig::try_deserialize(&mut &acct.data[..]).expect("deserializes post-migrate");
    assert_eq!(got.version, SchemaVersion::new(1, 0, 0), "stamped 1.0.0");
    assert_eq!(got.user, user, "user preserved");
    assert_eq!(got.page_count, 3, "page_count preserved");
    assert_eq!(got.total_entries, 7, "total_entries preserved");
    assert_eq!(got.bump, bump, "bump preserved");
}
