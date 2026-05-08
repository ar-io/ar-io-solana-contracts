//! CLI helper that prints canonical claim-message bytes to stdout.
//!
//! Used by the cross-language test in
//! `sdk/src/solana/canonical-message.cross.test.ts` to assert byte-equality
//! between the Rust and TypeScript implementations.
//!
//! Usage (ANT escrow claim):
//!   cargo run -p ario-ant-escrow --example canonical -- \
//!       <ant_mint_base58> <claimant_base58> <nonce_hex_64chars> \
//!       <recipient_pubkey_hex>
//!
//! Usage (token/vault escrow claim):
//!   cargo run -p ario-ant-escrow --example canonical -- --escrow \
//!       <asset_type> <asset_id_hex_64chars> <amount_u64> \
//!       <claimant_base58> <nonce_hex_64chars> <recipient_pubkey_hex>
//!
//! `recipient_pubkey_hex` is the recipient's identity bytes hex-encoded
//! (1024 hex chars for an Arweave RSA-4096 modulus, 40 hex chars for an
//! Ethereum address). It's included verbatim in the canonical message
//! via `derive_recipient_id_b64url(sha256(...))` — see
//! `docs/ATTESTOR_SECURITY_REVIEW.md` finding F-1.
//!
//! Output: raw canonical message bytes to stdout (no trailing newline).

use std::env;
use std::io::{self, Write};
use std::str::FromStr;

fn main() {
    let args: Vec<String> = env::args().collect();

    if args.len() >= 2 && args[1] == "--escrow" {
        run_escrow(&args);
    } else {
        run_ant_escrow(&args);
    }
}

fn run_ant_escrow(args: &[String]) {
    if args.len() != 5 {
        eprintln!(
            "Usage: {} <ant_mint_base58> <claimant_base58> <nonce_hex_64chars> <recipient_pubkey_hex>",
            args[0]
        );
        std::process::exit(1);
    }

    let ant_mint = solana_program::pubkey::Pubkey::from_str(&args[1]).unwrap_or_else(|e| {
        eprintln!("invalid ant_mint: {}", e);
        std::process::exit(1);
    });
    let claimant = solana_program::pubkey::Pubkey::from_str(&args[2]).unwrap_or_else(|e| {
        eprintln!("invalid claimant: {}", e);
        std::process::exit(1);
    });
    let nonce_hex = &args[3];
    if nonce_hex.len() != 64 {
        eprintln!(
            "nonce must be exactly 64 hex chars (32 bytes), got {}",
            nonce_hex.len()
        );
        std::process::exit(1);
    }
    let nonce = decode_hex(nonce_hex);
    let recipient_pubkey = decode_hex_var(&args[4]);

    let msg = ario_ant_escrow::canonical::build_ant_escrow_claim_message(
        &ant_mint,
        &claimant,
        &nonce,
        &recipient_pubkey,
    );

    io::stdout().write_all(&msg).expect("write stdout");
    io::stdout().flush().expect("flush stdout");
}

fn run_escrow(args: &[String]) {
    // args: [binary, "--escrow", asset_type, asset_id_hex, amount, claimant, nonce_hex, recipient_hex]
    if args.len() != 8 {
        eprintln!(
            "Usage: {} --escrow <token|vault> <asset_id_hex_64chars> <amount_u64> <claimant_base58> <nonce_hex_64chars> <recipient_pubkey_hex>",
            args[0]
        );
        std::process::exit(1);
    }

    let asset_type = &args[2];
    if asset_type != "token" && asset_type != "vault" {
        eprintln!(
            "asset_type must be 'token' or 'vault', got '{}'",
            asset_type
        );
        std::process::exit(1);
    }

    let asset_id_hex = &args[3];
    if asset_id_hex.len() != 64 {
        eprintln!(
            "asset_id must be exactly 64 hex chars (32 bytes), got {}",
            asset_id_hex.len()
        );
        std::process::exit(1);
    }
    let asset_id = decode_hex(asset_id_hex);

    let amount: u64 = args[4].parse().unwrap_or_else(|e| {
        eprintln!("invalid amount: {}", e);
        std::process::exit(1);
    });

    let claimant = solana_program::pubkey::Pubkey::from_str(&args[5]).unwrap_or_else(|e| {
        eprintln!("invalid claimant: {}", e);
        std::process::exit(1);
    });

    let nonce_hex = &args[6];
    if nonce_hex.len() != 64 {
        eprintln!(
            "nonce must be exactly 64 hex chars (32 bytes), got {}",
            nonce_hex.len()
        );
        std::process::exit(1);
    }
    let nonce = decode_hex(nonce_hex);
    let recipient_pubkey = decode_hex_var(&args[7]);

    let msg = ario_ant_escrow::canonical::build_escrow_claim_message(
        asset_type,
        &asset_id,
        amount,
        &claimant,
        &nonce,
        &recipient_pubkey,
    );

    io::stdout().write_all(&msg).expect("write stdout");
    io::stdout().flush().expect("flush stdout");
}

fn decode_hex(hex: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or_else(|e| {
            eprintln!("invalid hex at byte {}: {}", i, e);
            std::process::exit(1);
        });
    }
    out
}

fn decode_hex_var(hex: &str) -> Vec<u8> {
    if hex.len() % 2 != 0 {
        eprintln!(
            "recipient_pubkey hex must be even length, got {}",
            hex.len()
        );
        std::process::exit(1);
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    for i in 0..hex.len() / 2 {
        let byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap_or_else(|e| {
            eprintln!("invalid hex at byte {}: {}", i, e);
            std::process::exit(1);
        });
        out.push(byte);
    }
    out
}
