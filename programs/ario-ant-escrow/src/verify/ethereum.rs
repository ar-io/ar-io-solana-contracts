//! Ethereum ECDSA secp256k1 + EIP-191 signature verification.
//!
//! Matches the wire format produced by `wallet.signMessage(canonical)` in
//! every standard EVM stack — MetaMask, viem, ethers.js, WalletConnect.
//!
//! Algorithm (per design doc § Cryptographic verification → Ethereum ECDSA):
//!
//! ```text
//! prefix      = "\x19Ethereum Signed Message:\n" + ascii(len(canonical))
//! msg_hash    = keccak256(prefix || canonical)
//! v_norm      = (v - 27) if v >= 27 else v        // accepts {0, 1, 27, 28}
//! require       s ≤ secp256k1_n / 2               // EIP-2 low-S
//! pubkey      = secp256k1_recover(msg_hash, v_norm, r||s)
//! address     = keccak256(pubkey)[12..32]
//! require       address == expected_address
//! ```
//!
//! Cost on-chain: ~25K CU for `secp256k1_recover` + 2× keccak (~10K CU
//! total) + a few hundred CU for byte ops ≈ 30-40K CU per verification.
//! Well under the 200K-CU default per-instruction budget; no
//! `SetComputeUnitLimit` needed.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::{
    keccak::hash as keccak256, secp256k1_recover::secp256k1_recover,
};

use crate::error::EscrowError;

/// Ethereum address length, in bytes.
pub const ETHEREUM_ADDRESS_LEN: usize = 20;

/// `r || s || v` ECDSA signature length.
pub const ETHEREUM_SIG_LEN: usize = 65;

/// EIP-191 personal-sign prefix. Wallets prepend this (followed by the
/// ASCII decimal length of the signed message and then the message
/// itself) before keccak-hashing.
pub const EIP191_PREFIX: &[u8] = b"\x19Ethereum Signed Message:\n";

/// secp256k1 curve order n, big-endian. Value from RFC 5639 / SECG SEC 2
/// (also the constant Geth, ethers.js, and viem ship). Used to enforce
/// EIP-2 low-S form.
pub const SECP256K1_N: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xfe,
    0xba, 0xae, 0xdc, 0xe6, 0xaf, 0x48, 0xa0, 0x3b, 0xbf, 0xd2, 0x5e, 0x8c, 0xd0, 0x36, 0x41, 0x41,
];

/// secp256k1_n / 2, big-endian. Computed once at compile time from
/// `SECP256K1_N` (see the `secp_constants` test below for the proof).
pub const SECP256K1_N_HALF: [u8; 32] = [
    0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0x5d, 0x57, 0x6e, 0x73, 0x57, 0xa4, 0x50, 0x1d, 0xdf, 0xe9, 0x2f, 0x46, 0x68, 0x1b, 0x20, 0xa0,
];

/// Verify a `personal_sign`-style ECDSA signature against an expected
/// 20-byte Ethereum address.
///
/// `canonical_message` is the raw bytes the recipient signed BEFORE
/// EIP-191 wrapping. The wrapper is applied on-chain so callers never
/// need to recompute it.
pub fn verify_personal_sign(
    canonical_message: &[u8],
    signature: &[u8],
    expected_address: &[u8],
) -> Result<()> {
    require!(
        signature.len() == ETHEREUM_SIG_LEN,
        EscrowError::SignatureVerificationFailed
    );
    require!(
        expected_address.len() == ETHEREUM_ADDRESS_LEN,
        EscrowError::SignatureVerificationFailed
    );

    // 1. Apply EIP-191 wrapper. Length suffix is ASCII decimal — match
    //    what wallets produce (NOT the canonical message length encoded
    //    as a u32 or hex).
    let len_str = canonical_message.len().to_string();
    let mut to_hash =
        Vec::with_capacity(EIP191_PREFIX.len() + len_str.len() + canonical_message.len());
    to_hash.extend_from_slice(EIP191_PREFIX);
    to_hash.extend_from_slice(len_str.as_bytes());
    to_hash.extend_from_slice(canonical_message);
    let msg_hash = keccak256(&to_hash).to_bytes();

    // 2. Normalize recovery id. Accept legacy {27, 28} forms and modern
    //    {0, 1}; reject anything else. Solana's syscall takes [0, 3].
    let v = signature[64];
    let recovery_id: u8 = match v {
        0 | 1 => v,
        27 | 28 => v - 27,
        _ => return Err(error!(EscrowError::InvalidRecoveryId)),
    };

    // 3. Enforce EIP-2 low-S: s ≤ n/2.
    //    secp256k1_recover does NOT check this; without the guard, every
    //    valid signature has a malleable counterpart. While malleability
    //    can't change *who* the recovered key is, it would let two
    //    distinct on-chain claim txs both pass — extra surface we don't
    //    want.
    if !is_s_low(&signature[32..64]) {
        return Err(error!(EscrowError::EcdsaHighS));
    }

    // 4. Recover the public key. The function expects sig as r||s (64
    //    bytes) — drop the v byte.
    let recovered = secp256k1_recover(&msg_hash, recovery_id, &signature[..64])
        .map_err(|_| error!(EscrowError::SignatureVerificationFailed))?;

    // 5. Derive the Ethereum address: last 20 bytes of keccak256(pubkey).
    let pubkey_bytes = recovered.to_bytes(); // 64 bytes (X || Y, no 0x04 prefix)
    let pubkey_hash = keccak256(&pubkey_bytes).to_bytes();
    let derived_addr = &pubkey_hash[12..];

    // 6. Compare against expected address (XOR-accumulator, no early exit).
    let mut diff: u8 = 0;
    for i in 0..ETHEREUM_ADDRESS_LEN {
        diff |= derived_addr[i] ^ expected_address[i];
    }
    if diff != 0 {
        return Err(error!(EscrowError::EthereumAddressMismatch));
    }
    Ok(())
}

/// Returns true iff `s` (32 bytes, big-endian) is ≤ secp256k1_n / 2.
/// Lexicographic compare on big-endian bytes is identical to numeric
/// compare for fixed-width values.
fn is_s_low(s: &[u8]) -> bool {
    debug_assert_eq!(s.len(), 32);
    // Constant-ish loop: never break early.
    let mut decided: bool = false; // once we see a differing byte, lock in the answer
    let mut answer: bool = true; // s starts ≤ until proven otherwise
    for i in 0..32 {
        if !decided {
            match s[i].cmp(&SECP256K1_N_HALF[i]) {
                core::cmp::Ordering::Less => {
                    answer = true;
                    decided = true;
                }
                core::cmp::Ordering::Greater => {
                    answer = false;
                    decided = true;
                }
                core::cmp::Ordering::Equal => { /* keep scanning */ }
            }
        }
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secp_constants_n_half_is_half_of_n() {
        // Verify SECP256K1_N_HALF == SECP256K1_N / 2 by reconstructing
        // via a simple bigint divide-by-two on bytes.
        let mut half = [0u8; 32];
        let mut carry: u16 = 0;
        for i in 0..32 {
            let v = (carry << 8) | (SECP256K1_N[i] as u16);
            half[i] = (v >> 1) as u8;
            carry = v & 1;
        }
        assert_eq!(half, SECP256K1_N_HALF);
    }

    #[test]
    fn is_s_low_accepts_zero_and_n_half() {
        assert!(is_s_low(&[0u8; 32]));
        assert!(is_s_low(&SECP256K1_N_HALF));
    }

    #[test]
    fn is_s_low_rejects_n_half_plus_one_and_n_minus_one() {
        let mut plus_one = SECP256K1_N_HALF;
        plus_one[31] = plus_one[31].wrapping_add(1);
        assert!(!is_s_low(&plus_one));

        let mut n_minus_one = SECP256K1_N;
        n_minus_one[31] -= 1;
        assert!(!is_s_low(&n_minus_one));
    }

    #[test]
    fn is_s_low_handles_first_byte_difference() {
        // s = 0x80...00 — top bit set, should be > n/2 (which is 0x7f...)
        let mut s = [0u8; 32];
        s[0] = 0x80;
        assert!(!is_s_low(&s));

        // s = 0x7f...00 — should be < n/2 (which is 0x7fff...).
        let mut s = [0u8; 32];
        s[0] = 0x7f;
        assert!(is_s_low(&s));
    }

    #[test]
    fn rejects_signature_of_wrong_length() {
        let result = verify_personal_sign(b"msg", &[0u8; 64], &[0u8; 20]);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_address_of_wrong_length() {
        let result = verify_personal_sign(b"msg", &[0u8; 65], &[0u8; 19]);
        assert!(result.is_err());
    }

    #[test]
    fn rejects_invalid_recovery_id() {
        let mut sig = [0u8; 65];
        // Will fail high-S check first if s=0, so set s to small valid:
        sig[32] = 0x01; // s = 1 (< n/2)
        sig[0] = 0x01; // r = 1
        sig[64] = 5; // invalid v ∉ {0, 1, 27, 28}
        let result = verify_personal_sign(b"msg", &sig, &[0u8; 20]);
        assert!(matches!(
            result,
            Err(e) if e.to_string().contains("recovery id") || e.to_string().contains("InvalidRecoveryId")
        ));
    }

    #[test]
    fn rejects_high_s_signature() {
        // Signature with s = n - 1 (definitely high). r = 1, v = 0.
        let mut sig = [0u8; 65];
        sig[0] = 0x01; // r = 1
        let mut s_high = SECP256K1_N;
        s_high[31] -= 1; // n - 1
        sig[32..64].copy_from_slice(&s_high);
        sig[64] = 0;
        let result = verify_personal_sign(b"msg", &sig, &[0u8; 20]);
        assert!(matches!(
            result,
            Err(e) if e.to_string().contains("high-S") || e.to_string().contains("EcdsaHighS")
        ));
    }

    // ----------------------------------------------------------------------
    // Positive verification — sign with libsecp256k1 (already a transitive
    // dep of solana-program-test, so no new dev-dep) and verify on-chain
    // path. Because libsecp256k1 produces canonical low-S sigs, this also
    // validates the EIP-191 prefix construction end-to-end.
    // ----------------------------------------------------------------------
    use libsecp256k1::{Message, PublicKey, SecretKey};

    /// Helper: derive the 20-byte Ethereum address for a given secp256k1 pubkey.
    fn eth_address_for(pubkey: &PublicKey) -> [u8; 20] {
        let pk_bytes = pubkey.serialize(); // 65 bytes: 0x04 || X || Y
        let pk_hash = keccak256(&pk_bytes[1..]).to_bytes(); // skip 0x04
        let mut addr = [0u8; 20];
        addr.copy_from_slice(&pk_hash[12..32]);
        addr
    }

    /// Helper: sign `canonical_message` with EIP-191 wrapping, returning
    /// `(signature_65, address_20)`.
    fn personal_sign(canonical_message: &[u8], secret_key: &SecretKey) -> ([u8; 65], [u8; 20]) {
        let len_str = canonical_message.len().to_string();
        let mut to_hash = Vec::new();
        to_hash.extend_from_slice(EIP191_PREFIX);
        to_hash.extend_from_slice(len_str.as_bytes());
        to_hash.extend_from_slice(canonical_message);
        let msg_hash = keccak256(&to_hash).to_bytes();

        let msg = Message::parse(&msg_hash);
        let (sig, recovery_id) = libsecp256k1::sign(&msg, secret_key);
        let sig_bytes = sig.serialize();

        let mut out = [0u8; 65];
        out[..64].copy_from_slice(&sig_bytes);
        out[64] = recovery_id.serialize();

        let pubkey = PublicKey::from_secret_key(secret_key);
        (out, eth_address_for(&pubkey))
    }

    #[test]
    fn accepts_valid_libsecp256k1_personal_sign() {
        // Deterministic secret key for reproducibility.
        let sk_bytes: [u8; 32] = {
            let mut b = [0u8; 32];
            for (i, byte) in b.iter_mut().enumerate() {
                *byte = ((i as u8).wrapping_mul(7).wrapping_add(13)) | 1;
            }
            b
        };
        let secret_key = SecretKey::parse(&sk_bytes).unwrap();
        let canonical =
            b"ar.io ant-escrow claim\nnetwork: solana-mainnet\nant: ABC\nclaimant: XYZ\nnonce: 0";
        let (sig, addr) = personal_sign(canonical, &secret_key);
        verify_personal_sign(canonical, &sig, &addr).expect("valid sig must verify");
    }

    #[test]
    fn rejects_signature_under_wrong_message() {
        let sk_bytes = [42u8; 32];
        let secret_key = SecretKey::parse(&sk_bytes).unwrap();
        let (sig, addr) = personal_sign(b"original", &secret_key);
        let result = verify_personal_sign(b"tampered", &sig, &addr);
        assert!(result.is_err(), "tampered message must reject");
    }

    #[test]
    fn rejects_signature_under_wrong_address() {
        let sk_bytes = [42u8; 32];
        let secret_key = SecretKey::parse(&sk_bytes).unwrap();
        let (sig, _addr) = personal_sign(b"msg", &secret_key);
        let mut wrong_addr = [0u8; 20];
        for (i, b) in wrong_addr.iter_mut().enumerate() {
            *b = i as u8;
        }
        let result = verify_personal_sign(b"msg", &sig, &wrong_addr);
        assert!(matches!(
            result,
            Err(e) if e.to_string().contains("Ethereum address") || e.to_string().contains("EthereumAddressMismatch")
        ));
    }

    #[test]
    fn accepts_legacy_v27_v28_recovery_id() {
        let sk_bytes = [99u8; 32];
        let secret_key = SecretKey::parse(&sk_bytes).unwrap();
        let (mut sig, addr) = personal_sign(b"legacy v test", &secret_key);
        // libsecp256k1 produces v in {0, 1}. Promote to legacy form.
        sig[64] += 27;
        verify_personal_sign(b"legacy v test", &sig, &addr).expect("legacy v=27/28 must verify");
    }

    #[test]
    fn rejects_tampered_signature_byte() {
        let sk_bytes = [55u8; 32];
        let secret_key = SecretKey::parse(&sk_bytes).unwrap();
        let (mut sig, addr) = personal_sign(b"tamper-test", &secret_key);
        sig[10] ^= 0x01; // flip a bit in r
        let result = verify_personal_sign(b"tamper-test", &sig, &addr);
        assert!(result.is_err(), "single-bit tamper must reject");
    }
}
