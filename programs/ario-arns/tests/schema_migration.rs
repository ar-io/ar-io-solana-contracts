// Regression test for the ArnsRecord schema-version migration.
//
// `ArnsRecord.version` was reordered to the byte-END (after the variable
// `name`); previously it sat BEFORE `name`, which corrupted the string-length
// read and made pre-version records unloadable. This test builds a genuine
// pre-version record (the new serialization minus the trailing 3 version
// bytes — valid precisely BECAUSE version is now last) and asserts
// `migrate_arns_record` grows it, stamps 1.0.0, and preserves every field.
//
// migrate_arns_record does not CPI Metaplex Core, so it runs on the native
// processor (no BPF_OUT_DIR needed).

use anchor_lang::{prelude::*, AccountSerialize, Discriminator, InstructionData, ToAccountMetas};
use solana_program_test::*;
use solana_sdk::{
    instruction::Instruction,
    signature::{Keypair, Signer},
    system_program,
    transaction::Transaction,
};

use ario_arns::state::*;

fn anchor_processor(
    program_id: &Pubkey,
    accounts: &[AccountInfo],
    data: &[u8],
) -> anchor_lang::solana_program::entrypoint::ProgramResult {
    unsafe {
        let accounts: &[AccountInfo] = std::mem::transmute(accounts);
        ario_arns::entry(program_id, accounts, data)
    }
}

#[tokio::test]
async fn migrate_arns_record_grows_and_versions_pre_version_account() {
    let mut pt = ProgramTest::new("ario_arns", ario_arns::ID, processor!(anchor_processor));
    pt.set_compute_max_units(400_000);

    let name = "example".to_string();
    let name_hash =
        anchor_lang::solana_program::hash::hash(name.to_lowercase().as_bytes()).to_bytes();
    let (record_key, bump) =
        Pubkey::find_program_address(&[ARNS_RECORD_SEED, name_hash.as_ref()], &ario_arns::ID);

    let owner = Pubkey::new_unique();
    let ant = Pubkey::new_unique();

    // Build the CURRENT-layout record, serialize, then strip the trailing
    // 3-byte version → exactly the pre-version on-chain bytes (works only
    // because `version` is now the last field).
    let rec = ArnsRecord {
        name_hash,
        owner,
        ant,
        purchase_type: PurchaseType::Permabuy,
        start_timestamp: 1_700_000_000,
        end_timestamp: None,
        undername_limit: 10,
        purchase_price: 123_456,
        bump,
        name: name.clone(),
        version: SchemaVersion::new(0, 0, 0),
    };
    let mut full = Vec::new();
    rec.try_serialize(&mut full).unwrap();
    assert_eq!(&full[..8], ArnsRecord::DISCRIMINATOR);
    let pre_version = full[..full.len() - 3].to_vec(); // drop trailing version(3)

    let rent = solana_sdk::rent::Rent::default();
    pt.add_account(
        record_key,
        solana_sdk::account::Account {
            lamports: rent.minimum_balance(pre_version.len()),
            data: pre_version,
            owner: ario_arns::ID,
            executable: false,
            rent_epoch: 0,
        },
    );
    let payer = Keypair::new();
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

    let accounts = ario_arns::accounts::MigrateArnsRecord {
        record: record_key,
        payer: payer.pubkey(),
        system_program: system_program::id(),
    };
    let ix = Instruction {
        program_id: ario_arns::ID,
        accounts: accounts.to_account_metas(None),
        data: ario_arns::instruction::MigrateArnsRecord {}.data(),
    };
    let bh = ctx.banks_client.get_latest_blockhash().await.unwrap();
    let tx = Transaction::new_signed_with_payer(&[ix], Some(&payer.pubkey()), &[&payer], bh);
    ctx.banks_client
        .process_transaction(tx)
        .await
        .expect("migrate_arns_record over a pre-version record must succeed");

    let acct = ctx
        .banks_client
        .get_account(record_key)
        .await
        .unwrap()
        .expect("record still exists");
    assert_eq!(acct.data.len(), ArnsRecord::SIZE, "grown to new SIZE");
    let got = ArnsRecord::try_deserialize(&mut &acct.data[..]).expect("deserializes post-migrate");
    assert_eq!(got.version, SchemaVersion::new(1, 0, 0), "stamped 1.0.0");
    assert_eq!(got.name, name, "name preserved");
    assert_eq!(got.owner, owner, "owner preserved");
    assert_eq!(got.ant, ant, "ant preserved");
    assert_eq!(got.undername_limit, 10, "undername_limit preserved");
    assert_eq!(got.purchase_price, 123_456, "price preserved");
    assert_eq!(got.bump, bump, "bump preserved");
}
