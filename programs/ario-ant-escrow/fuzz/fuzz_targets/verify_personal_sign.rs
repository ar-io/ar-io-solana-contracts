//! Fuzz target for the Ethereum ECDSA + EIP-191 verifier.
//!
//! Same pass criteria as the PSS target: no panics, no false positives.
//! ECDSA signature space is tiny (65 bytes) so the fuzzer covers it
//! quickly; the more interesting inputs are malformed EIP-191 message
//! lengths and edge-case recovery ids.
//!
//! Run alongside `verify_rsa_pss`:
//! ```bash
//! cargo +nightly fuzz run verify_personal_sign -- -max_total_time=86400
//! ```

#![no_main]

use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};

use ario_ant_escrow::verify::ethereum::verify_personal_sign;

#[derive(Arbitrary, Debug)]
struct FuzzInput<'a> {
    canonical_message: &'a [u8],
    signature: [u8; 65],
    expected_address: [u8; 20],
}

fuzz_target!(|input: FuzzInput<'_>| {
    let result = verify_personal_sign(
        input.canonical_message,
        &input.signature,
        &input.expected_address,
    );

    // Invariant: random sig + random address must reject. The 1-in-2^160
    // probability of a random recover landing on a fixed address is
    // negligible; fuzzer accepting one would mean the verifier is broken.
    if result.is_ok() {
        panic!(
            "verify_personal_sign accepted random input — false positive!\n\
             msg_len={}\naddr={:?}\nsig[..16]={:?}",
            input.canonical_message.len(),
            &input.expected_address,
            &input.signature[..16],
        );
    }
});
