//! Fuzz target for the Arweave RSA-PSS-4096 verifier.
//!
//! Pass criteria (per design doc § Phase 4 acceptance):
//! - No panics
//! - No infinite loops (fuzzer driver kills runs > 25s; we should never
//!   come close)
//! - No false positives — random byte strings must NEVER make
//!   `verify_rsa_pss_sha256` return `Ok`
//!
//! Run on a Linux dev box (cargo-fuzz is not supported on Windows /
//! ARM Windows):
//! ```bash
//! cargo install cargo-fuzz   # one-time
//! cd contracts/programs/ario-ant-escrow
//! cargo +nightly fuzz run verify_rsa_pss -- -max_total_time=86400
//! ```
//!
//! cargo-fuzz pulls `nightly` because libfuzzer-sys uses the
//! `LLVMFuzzerTestOneInput` exported symbol and the unstable
//! `panic = "abort"` crate-type knobs.

#![no_main]

use libfuzzer_sys::{arbitrary::Arbitrary, fuzz_target};

use ario_ant_escrow::verify::arweave::verify_rsa_pss_sha256;

#[derive(Arbitrary, Debug)]
struct FuzzInput<'a> {
    message: &'a [u8],
    /// Sized inside the fuzz body via `salt_len & 0x3F` so most inputs
    /// land in the valid [0, 32] range and exercise the verifier deeply
    /// (bounds-check rejections are uninteresting).
    salt_len: u8,
    sig_bytes: [u8; 512],
    modulus_bytes: [u8; 512],
}

fuzz_target!(|input: FuzzInput<'_>| {
    // Bias salt_len into the accepted range most of the time so the
    // fuzzer spends compute on the actual PSS math, not on the
    // length-validation branch.
    let salt_len = input.salt_len & 0x3F;

    // Force modulus to be odd + 4096-bit (top bit set) so big_mod_exp
    // doesn't trivially zero out. We don't need a real RSA modulus —
    // the contract is "this returns Err for any random sig". Constant
    // tweaks below don't change that contract.
    let mut modulus = input.modulus_bytes;
    modulus[0] |= 0x80;
    modulus[511] |= 0x01;

    let result = verify_rsa_pss_sha256(input.message, &input.sig_bytes, &modulus, salt_len);

    // The invariant: random/structured inputs MUST reject. If we ever see
    // an Ok here, that's a fuzzer-found false positive, which is a
    // catastrophic verifier bug. Panic so the fuzzer captures it.
    if result.is_ok() {
        panic!(
            "verify_rsa_pss_sha256 accepted random input — false positive!\n\
             message_len={}, salt_len={}\nmodulus[..16]={:?}\nsig[..16]={:?}",
            input.message.len(),
            salt_len,
            &modulus[..16],
            &input.sig_bytes[..16],
        );
    }
});
