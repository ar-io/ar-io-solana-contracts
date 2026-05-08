//! Canonical claim-message reconstruction.
//!
//! The bytes a recipient signs to release an ANT MUST be reproducible
//! on-chain from the escrow account state and the claim transaction's
//! accounts alone — no client-supplied message bytes. This eliminates an
//! entire class of message-malleability attacks.
//!
//! Two distinct message shapes live here, one per claim category:
//!
//! - `build_ant_escrow_claim_message` — header `ar.io ant-escrow claim`;
//!   used by `claim_ant_arweave*` and `claim_ant_ethereum*` instructions.
//! - `build_escrow_claim_message` — header `ar.io escrow claim`; used by
//!   `claim_tokens_*` and `claim_vault_*` instructions, with a `type:`
//!   field discriminating between token and vault claims.
//!
//! These are not versions of the same shape — the field sets differ —
//! so they sit side-by-side without any `v1`/`v2` distinction.
//!
//! The TypeScript SDK ships a byte-equivalent helper. Both implementations
//! are pinned by the cross-language test in `sdk/src/solana/canonical-message.test.ts`
//! (it execs `cargo run -p ario-ant-escrow --example canonical -- ...` and
//! diffs the output).

use anchor_lang::prelude::*;
use anchor_lang::solana_program::hash::hashv;

use crate::NETWORK;

/// SHA-256 the active recipient pubkey, base64url-encode (no padding).
///
/// Binds the canonical message to the specific recipient identity stored
/// at deposit time. For Arweave recipients, this is exactly the wallet
/// address (Arweave addresses are `base64url(sha256(rsa_modulus))`). For
/// Ethereum, it's `base64url(sha256(eth_address))` — no special meaning
/// beyond binding the recipient.
///
/// **Why this exists (security-critical):** The off-chain attestor in
/// the Arweave-attested claim path verifies an RSA-PSS signature using
/// a *client-supplied* modulus. Without including the recipient's
/// identity hash in the canonical message, an attacker could supply
/// their own (modulus, signature) pair and substitute their own
/// claimant pubkey — the attestor would happily verify against the
/// attacker's modulus and the on-chain Ed25519 introspection would
/// pass because the canonical message it reconstructs from escrow
/// state never referenced the modulus.
///
/// By forcing both sides to compute `recipient_id` from a modulus and
/// commit to it inside the signed bytes, the binding is restored: the
/// attestor's canonical (built from client-supplied modulus) and the
/// on-chain canonical (built from `escrow.recipient_pubkey_active()`)
/// only match if the modulus is the right one. See
/// `docs/ATTESTOR_SECURITY_REVIEW.md` finding F-1.
pub fn derive_recipient_id_b64url(recipient_pubkey_active: &[u8]) -> String {
    let h = hashv(&[recipient_pubkey_active]);
    base64url_no_pad(&h.to_bytes())
}

/// Base64url-encode without padding. 32 input bytes → 43 output chars.
fn base64url_no_pad(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity((bytes.len() * 4 + 2) / 3);
    let mut chunks = bytes.chunks_exact(3);
    for chunk in &mut chunks {
        let n = ((chunk[0] as u32) << 16) | ((chunk[1] as u32) << 8) | (chunk[2] as u32);
        out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        out.push(ALPHABET[(n & 0x3F) as usize] as char);
    }
    let rem = chunks.remainder();
    match rem.len() {
        0 => {}
        1 => {
            let n = (rem[0] as u32) << 16;
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
        }
        2 => {
            let n = ((rem[0] as u32) << 16) | ((rem[1] as u32) << 8);
            out.push(ALPHABET[((n >> 18) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 12) & 0x3F) as usize] as char);
            out.push(ALPHABET[((n >> 6) & 0x3F) as usize] as char);
        }
        _ => unreachable!(),
    }
    out
}

/// Header literal anchoring the ANT-escrow canonical claim message.
/// Distinct from the token/vault header below — these are two
/// different message shapes, not two versions of one shape.
pub const ANT_ESCROW_CLAIM_HEADER: &[u8] = b"ar.io ant-escrow claim";

/// Build the canonical ANT-escrow claim message bytes.
///
/// `nonce` is the escrow's current 32-byte anti-replay nonce; the
/// 64-character lowercase hex encoding goes into the message.
///
/// Allocates a single `Vec<u8>` sized exactly for the output to keep CU
/// usage tight inside the on-chain claim path (header + 4 fixed labels +
/// 2× 44-char base58 pubkeys + 64-char hex + a few newlines = ~250 bytes
/// for a typical claim).
pub fn build_ant_escrow_claim_message(
    ant_mint: &Pubkey,
    claimant: &Pubkey,
    nonce: &[u8; 32],
    recipient_pubkey_active: &[u8],
) -> Vec<u8> {
    // Pubkey::to_string() uses base58 encoding; result is 32-44 ASCII chars.
    let ant_b58 = ant_mint.to_string();
    let claimant_b58 = claimant.to_string();
    let nonce_hex = encode_hex_lowercase(nonce);
    let recipient_id = derive_recipient_id_b64url(recipient_pubkey_active);

    // Pre-size: header + "\nnetwork: "(10) + NETWORK + "\nrecipient: "(12)
    //         + recipient_id(43) + "\nant: "(6) + ant_b58 + "\nclaimant: "(11)
    //         + claimant_b58 + "\nnonce: "(8) + nonce_hex
    let mut out = Vec::with_capacity(
        ANT_ESCROW_CLAIM_HEADER.len()
            + 10
            + NETWORK.len()
            + 12
            + recipient_id.len()
            + 6
            + ant_b58.len()
            + 11
            + claimant_b58.len()
            + 8
            + nonce_hex.len(),
    );

    out.extend_from_slice(ANT_ESCROW_CLAIM_HEADER);
    out.extend_from_slice(b"\nnetwork: ");
    out.extend_from_slice(NETWORK);
    out.extend_from_slice(b"\nrecipient: ");
    out.extend_from_slice(recipient_id.as_bytes());
    out.extend_from_slice(b"\nant: ");
    out.extend_from_slice(ant_b58.as_bytes());
    out.extend_from_slice(b"\nclaimant: ");
    out.extend_from_slice(claimant_b58.as_bytes());
    out.extend_from_slice(b"\nnonce: ");
    out.extend_from_slice(nonce_hex.as_bytes());

    out
}

/// Lowercase hex encoder dedicated to the canonical message. Avoids
/// pulling in a `hex` crate (the SBF Cargo 1.79 toolchain rejects newer
/// versions' edition2024 manifests, and we'd only use one function).
fn encode_hex_lowercase(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0F) as usize] as char);
    }
    s
}

/// Header literal anchoring the token/vault escrow canonical claim message.
pub const ESCROW_CLAIM_HEADER: &[u8] = b"ar.io escrow claim";

/// Build the canonical token/vault escrow claim message bytes.
///
/// Format (UTF-8, line-feed separated, no trailing newline):
///
/// ```text
/// ar.io escrow claim
/// network: <network>
/// type: <asset_type>
/// asset: <asset_id_hex_lowercase_64chars>
/// amount: <u64_decimal>
/// claimant: <claimant_solana_pubkey_base58>
/// nonce: <nonce_hex_lowercase_64chars>
/// ```
///
/// `asset_type` must be `"token"` or `"vault"`.
pub fn build_escrow_claim_message(
    asset_type: &str,
    asset_id: &[u8; 32],
    amount: u64,
    claimant: &Pubkey,
    nonce: &[u8; 32],
    recipient_pubkey_active: &[u8],
) -> Vec<u8> {
    let claimant_b58 = claimant.to_string();
    let asset_hex = encode_hex_lowercase(asset_id);
    let nonce_hex = encode_hex_lowercase(nonce);
    let amount_str = amount.to_string();
    let recipient_id = derive_recipient_id_b64url(recipient_pubkey_active);

    let mut out = Vec::with_capacity(
        ESCROW_CLAIM_HEADER.len()
            + 10
            + NETWORK.len()
            + 12
            + recipient_id.len()
            + 7
            + asset_type.len()
            + 8
            + asset_hex.len()
            + 9
            + amount_str.len()
            + 11
            + claimant_b58.len()
            + 8
            + nonce_hex.len(),
    );

    out.extend_from_slice(ESCROW_CLAIM_HEADER);
    out.extend_from_slice(b"\nnetwork: ");
    out.extend_from_slice(NETWORK);
    out.extend_from_slice(b"\nrecipient: ");
    out.extend_from_slice(recipient_id.as_bytes());
    out.extend_from_slice(b"\ntype: ");
    out.extend_from_slice(asset_type.as_bytes());
    out.extend_from_slice(b"\nasset: ");
    out.extend_from_slice(asset_hex.as_bytes());
    out.extend_from_slice(b"\namount: ");
    out.extend_from_slice(amount_str.as_bytes());
    out.extend_from_slice(b"\nclaimant: ");
    out.extend_from_slice(claimant_b58.as_bytes());
    out.extend_from_slice(b"\nnonce: ");
    out.extend_from_slice(nonce_hex.as_bytes());

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::pubkey::Pubkey;
    use std::str::FromStr;

    #[test]
    fn hex_encoder_matches_lowercase() {
        assert_eq!(encode_hex_lowercase(&[]), "");
        assert_eq!(encode_hex_lowercase(&[0x00]), "00");
        assert_eq!(encode_hex_lowercase(&[0xFF]), "ff");
        assert_eq!(encode_hex_lowercase(&[0xDE, 0xAD, 0xBE, 0xEF]), "deadbeef");
        // 32-byte nonce → 64 hex chars.
        let bytes = [
            0xa3, 0xf1, 0xc8, 0xd9, 0x2e, 0x0b, 0x4f, 0x7a, 0x8e, 0x1d, 0x6c, 0x5b, 0x4a, 0x39,
            0x20, 0x81, 0x7f, 0x6e, 0x5d, 0x4c, 0x3b, 0x2a, 0x19, 0x18, 0x87, 0x76, 0x65, 0x54,
            0x43, 0x32, 0x21, 0x10,
        ];
        // Note: docs/ANT_ESCROW_DESIGN.md § Canonical message format prints
        // the same nonce with a trailing extra "0" (65 chars) — that's a
        // typo in the doc. 32 bytes is exactly 64 hex chars.
        let hex = encode_hex_lowercase(&bytes);
        assert_eq!(hex.len(), 64);
        assert_eq!(
            hex,
            "a3f1c8d92e0b4f7a8e1d6c5b4a3920817f6e5d4c3b2a19188776655443322110"
        );
    }

    /// Stable "Arweave-shaped" recipient bytes used by canonical-message tests.
    /// 512 bytes of `0xAB` → known `recipient_id`. Real RSA modulus content
    /// is irrelevant; the canonical builder only cares about the byte hash.
    const TEST_RECIPIENT_AB512: [u8; 512] = [0xABu8; 512];

    #[test]
    fn canonical_message_matches_design_doc_example() {
        // From docs/ANT_ESCROW_DESIGN.md § Canonical message format. Pubkey
        // strings reproduced verbatim; we parse them back so the assertion
        // catches accidental drift in either the doc or the code.
        let ant = Pubkey::from_str("9PnRFwk2Yp7QyU3sQzXwUhJj6tVyM4nN2KqL5fT8RbAW").unwrap();
        let claimant = Pubkey::from_str("Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A").unwrap();
        let nonce = [
            0xa3, 0xf1, 0xc8, 0xd9, 0x2e, 0x0b, 0x4f, 0x7a, 0x8e, 0x1d, 0x6c, 0x5b, 0x4a, 0x39,
            0x20, 0x81, 0x7f, 0x6e, 0x5d, 0x4c, 0x3b, 0x2a, 0x19, 0x18, 0x87, 0x76, 0x65, 0x54,
            0x43, 0x32, 0x21, 0x10,
        ];

        let msg = build_ant_escrow_claim_message(&ant, &claimant, &nonce, &TEST_RECIPIENT_AB512);
        let recipient_id = derive_recipient_id_b64url(&TEST_RECIPIENT_AB512);
        let expected = format!(
            "ar.io ant-escrow claim\n\
             network: {}\n\
             recipient: {}\n\
             ant: 9PnRFwk2Yp7QyU3sQzXwUhJj6tVyM4nN2KqL5fT8RbAW\n\
             claimant: Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A\n\
             nonce: a3f1c8d92e0b4f7a8e1d6c5b4a3920817f6e5d4c3b2a19188776655443322110",
            std::str::from_utf8(NETWORK).unwrap(),
            recipient_id,
        );
        assert_eq!(
            std::str::from_utf8(&msg).unwrap(),
            expected,
            "canonical message drifted from design doc format"
        );
    }

    #[test]
    fn canonical_message_has_no_trailing_newline() {
        let msg = build_ant_escrow_claim_message(
            &Pubkey::new_unique(),
            &Pubkey::new_unique(),
            &[0u8; 32],
            &TEST_RECIPIENT_AB512,
        );
        assert!(
            !msg.ends_with(b"\n"),
            "canonical message must not end with newline"
        );
    }

    #[test]
    fn canonical_message_is_deterministic() {
        let ant = Pubkey::new_unique();
        let claimant = Pubkey::new_unique();
        let nonce = [7u8; 32];
        let a = build_ant_escrow_claim_message(&ant, &claimant, &nonce, &TEST_RECIPIENT_AB512);
        let b = build_ant_escrow_claim_message(&ant, &claimant, &nonce, &TEST_RECIPIENT_AB512);
        assert_eq!(a, b);
    }

    #[test]
    fn canonical_message_changes_with_each_input() {
        let ant1 = Pubkey::new_unique();
        let ant2 = Pubkey::new_unique();
        let c1 = Pubkey::new_unique();
        let c2 = Pubkey::new_unique();
        let n1 = [0u8; 32];
        let n2 = [1u8; 32];

        let base = build_ant_escrow_claim_message(&ant1, &c1, &n1, &TEST_RECIPIENT_AB512);
        assert_ne!(
            base,
            build_ant_escrow_claim_message(&ant2, &c1, &n1, &TEST_RECIPIENT_AB512)
        );
        assert_ne!(
            base,
            build_ant_escrow_claim_message(&ant1, &c2, &n1, &TEST_RECIPIENT_AB512)
        );
        assert_ne!(
            base,
            build_ant_escrow_claim_message(&ant1, &c1, &n2, &TEST_RECIPIENT_AB512)
        );
    }

    #[test]
    fn canonical_message_changes_with_recipient_pubkey() {
        // F-1 regression: distinct moduli must produce distinct canonical
        // messages. Without this binding, the off-chain attestor flow
        // accepts any (modulus, signature) pair the client supplies.
        let ant = Pubkey::new_unique();
        let claimant = Pubkey::new_unique();
        let nonce = [0u8; 32];
        let recipient_a = [0xAAu8; 512];
        let recipient_b = [0xBBu8; 512];
        let a = build_ant_escrow_claim_message(&ant, &claimant, &nonce, &recipient_a);
        let b = build_ant_escrow_claim_message(&ant, &claimant, &nonce, &recipient_b);
        assert_ne!(a, b);
    }

    #[test]
    fn recipient_id_b64url_is_43_chars_for_32_byte_hash() {
        // sha256 → 32 bytes → base64url-no-pad → 43 chars.
        let id = derive_recipient_id_b64url(&[0xABu8; 512]);
        assert_eq!(id.len(), 43, "recipient_id should be 43 base64url chars");
        assert!(
            id.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
            "recipient_id should be url-safe base64"
        );
    }

    #[test]
    fn base64url_no_pad_matches_known_vectors() {
        // RFC 4648 vectors, base64url without padding.
        assert_eq!(base64url_no_pad(b""), "");
        assert_eq!(base64url_no_pad(b"f"), "Zg");
        assert_eq!(base64url_no_pad(b"fo"), "Zm8");
        assert_eq!(base64url_no_pad(b"foo"), "Zm9v");
        assert_eq!(base64url_no_pad(b"foob"), "Zm9vYg");
        assert_eq!(base64url_no_pad(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url_no_pad(b"foobar"), "Zm9vYmFy");
        // Url-safe characters: bytes 0xff,0xff,0xff → "____" in base64url.
        assert_eq!(base64url_no_pad(&[0xff, 0xff, 0xff]), "____");
        // Verify - and _ map correctly (vs + and / in standard b64).
        // Standard b64 of bytes [0xfb, 0xff] = "+/8="; base64url-no-pad = "-_8".
        assert_eq!(base64url_no_pad(&[0xfb, 0xff]), "-_8");
    }

    // ---- escrow_claim_message tests (token/vault shape) ----

    #[test]
    fn escrow_message_is_deterministic() {
        let claimant = Pubkey::new_unique();
        let asset_id = [42u8; 32];
        let nonce = [7u8; 32];
        let a = build_escrow_claim_message(
            "token",
            &asset_id,
            1_000_000,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        let b = build_escrow_claim_message(
            "token",
            &asset_id,
            1_000_000,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        assert_eq!(a, b);
    }

    #[test]
    fn escrow_message_has_no_trailing_newline() {
        let msg = build_escrow_claim_message(
            "token",
            &[0u8; 32],
            100,
            &Pubkey::new_unique(),
            &[0u8; 32],
            &TEST_RECIPIENT_AB512,
        );
        assert!(
            !msg.ends_with(b"\n"),
            "escrow canonical message must not end with newline"
        );
    }

    #[test]
    fn escrow_message_changes_with_asset_type() {
        let claimant = Pubkey::new_unique();
        let asset_id = [1u8; 32];
        let nonce = [0u8; 32];
        let a = build_escrow_claim_message(
            "token",
            &asset_id,
            100,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        let b = build_escrow_claim_message(
            "vault",
            &asset_id,
            100,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn escrow_message_changes_with_asset_id() {
        let claimant = Pubkey::new_unique();
        let nonce = [0u8; 32];
        let a = build_escrow_claim_message(
            "token",
            &[0u8; 32],
            100,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        let b = build_escrow_claim_message(
            "token",
            &[1u8; 32],
            100,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn escrow_message_changes_with_amount() {
        let claimant = Pubkey::new_unique();
        let asset_id = [0u8; 32];
        let nonce = [0u8; 32];
        let a = build_escrow_claim_message(
            "token",
            &asset_id,
            100,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        let b = build_escrow_claim_message(
            "token",
            &asset_id,
            200,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn escrow_message_changes_with_claimant() {
        let c1 = Pubkey::new_unique();
        let c2 = Pubkey::new_unique();
        let asset_id = [0u8; 32];
        let nonce = [0u8; 32];
        let a =
            build_escrow_claim_message("token", &asset_id, 100, &c1, &nonce, &TEST_RECIPIENT_AB512);
        let b =
            build_escrow_claim_message("token", &asset_id, 100, &c2, &nonce, &TEST_RECIPIENT_AB512);
        assert_ne!(a, b);
    }

    #[test]
    fn escrow_message_changes_with_nonce() {
        let claimant = Pubkey::new_unique();
        let asset_id = [0u8; 32];
        let a = build_escrow_claim_message(
            "token",
            &asset_id,
            100,
            &claimant,
            &[0u8; 32],
            &TEST_RECIPIENT_AB512,
        );
        let b = build_escrow_claim_message(
            "token",
            &asset_id,
            100,
            &claimant,
            &[1u8; 32],
            &TEST_RECIPIENT_AB512,
        );
        assert_ne!(a, b);
    }

    #[test]
    fn escrow_message_changes_with_recipient_pubkey() {
        // F-1 regression: same as canonical_message_changes_with_recipient_pubkey
        // but for the token/vault canonical builder.
        let claimant = Pubkey::new_unique();
        let asset_id = [0u8; 32];
        let nonce = [0u8; 32];
        let a =
            build_escrow_claim_message("token", &asset_id, 100, &claimant, &nonce, &[0xAAu8; 512]);
        let b =
            build_escrow_claim_message("token", &asset_id, 100, &claimant, &nonce, &[0xBBu8; 512]);
        assert_ne!(a, b);
    }

    #[test]
    fn escrow_message_format_structure() {
        let claimant = Pubkey::from_str("Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A").unwrap();
        let asset_id = [0xABu8; 32];
        let nonce = [0xCDu8; 32];
        let msg = build_escrow_claim_message(
            "token",
            &asset_id,
            500_000_000,
            &claimant,
            &nonce,
            &TEST_RECIPIENT_AB512,
        );
        let text = std::str::from_utf8(&msg).unwrap();

        assert!(text.starts_with("ar.io escrow claim\n"));
        assert!(text.contains("network: "));
        assert!(text.contains("\nrecipient: "));
        assert!(text.contains("\ntype: token\n"));
        assert!(text.contains(
            "\nasset: abababababababababababababababababababababababababababababababab\n"
        ));
        assert!(text.contains("\namount: 500000000\n"));
        assert!(text.contains(&format!("\nclaimant: {}\n", claimant)));
        assert!(text
            .contains("\nnonce: cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"));
        // No trailing newline
        assert!(text.ends_with("cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd"));
    }
}
