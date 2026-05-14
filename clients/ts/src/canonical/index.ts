//! Hand-written canonical-message builders. Sit alongside the
//! Codama-generated per-program clients (`./core`, `./gar`, etc.) but
//! are NOT generated — they mirror Rust serialization helpers that
//! don't appear in any IDL (and so codegen can't see them).
//!
//! Currently:
//!   - ./ant-escrow: ANT-escrow + token/vault escrow claim messages,
//!     signed off-chain by the attestor and reconstructed on-chain by
//!     `ario-ant-escrow` for Ed25519 verification (ADR-017).
//!
//! Future modules following the same pattern can be added as siblings
//! without touching codegen; the `clean` script in package.json only
//! wipes per-program codegen output and leaves this directory alone.

export * as antEscrow from "./ant-escrow.js";
