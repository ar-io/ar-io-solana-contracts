//! Cryptographic verifiers for ANT escrow claim signatures.
//!
//! - `attested`: Arweave production path. Off-chain attestor service
//!   does the RSA-PSS-4096 verification and re-signs the canonical
//!   claim message with Ed25519; this module verifies the Ed25519
//!   signature via Solana's native sigverify program + sysvar
//!   instruction-introspection.
//! - `ethereum`: ECDSA secp256k1 with EIP-191 wrapping, low-S enforced
//!   per EIP-2. Matches MetaMask / viem / ethers.js. Verifies on-chain
//!   via the `secp256k1_recover` syscall (always enabled).
//!
//! Both verifiers operate on the canonical message bytes produced by
//! `crate::canonical::build_ant_escrow_claim_message`. The verifiers
//! themselves never touch transaction accounts — the caller (claim
//! handler) is responsible for extracting account-derived inputs and
//! verifying the recipient match.
//!
//! ## Removed: legacy on-chain RSA-PSS path
//!
//! Earlier revisions of this crate had an `arweave` module that did
//! RSA-PSS verification on-chain via the `sol_big_mod_exp` syscall.
//! That syscall is feature-gated and disabled on every public Solana
//! cluster, so a `.so` referencing it fails to load on devnet /
//! mainnet. The off-chain attestor architecture (ADR-017) replaced it.
//! The legacy module + its three claim instructions
//! (`claim_*_arweave`) were removed in commit b1d5b73 to unblock
//! deployment.

pub mod attested;
pub mod ethereum;
