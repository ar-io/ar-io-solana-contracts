//! Indexer-facing events for `ario-ant-escrow` (PR-4 of
//! `EVENT_EMISSION_IMPLEMENTATION_PLAN`).
//!
//! The program has 15 state-changing instructions across three asset
//! types (ANT NFT / SPL Tokens / time-locked Vault) and two claim
//! protocols (Arweave RSA-PSS-4096 / Ethereum ECDSA secp256k1). Rather
//! than ship 15 separate event shapes, every instruction emits one of
//! 4 unified events with `u8` discriminator fields:
//!
//! * `EscrowDepositedEvent`         — every `deposit_*` instruction
//! * `EscrowClaimedEvent`           — every `claim_*` instruction
//! * `EscrowCancelledEvent`         — every `cancel_*` instruction
//! * `EscrowRecipientUpdatedEvent`  — every `update_*_recipient`
//!
//! Indexers subscribe to one event type per operation and filter
//! client-side by `asset_type` / `claim_protocol`.
//!
//! Field shapes are part of the published ABI (see ADR-017 — shipped
//! events are append-only; ship a `*EventV2` if a field has to change).

use anchor_lang::prelude::*;

// =========================================================================
// Discriminators (stable wire encoding — append-only, never repurpose)
// =========================================================================

/// Re-export the canonical AssetType / ClaimProtocol discriminators from
/// `state` for event consumers. `ASSET_TYPE_*` is carried by every escrow
/// event; `PROTOCOL_*` is carried by `EscrowDepositedEvent.recipient_protocol`
/// and `EscrowClaimedEvent.claim_protocol`.
///   ASSET_TYPE_ANT (0)    = Metaplex Core NFT
///   ASSET_TYPE_TOKEN (1)  = SPL ARIO
///   ASSET_TYPE_VAULT (2)  = time-locked tokens
///   PROTOCOL_ARWEAVE (0)  = RSA-PSS-4096
///   PROTOCOL_ETHEREUM (1) = ECDSA secp256k1
pub use crate::state::{
    ASSET_TYPE_ANT, ASSET_TYPE_TOKEN, ASSET_TYPE_VAULT, PROTOCOL_ARWEAVE, PROTOCOL_ETHEREUM,
};

// =========================================================================
// Events
// =========================================================================

/// Emitted on every successful deposit (ANT / Tokens / Vault).
///
/// `asset_id` encoding by `asset_type`:
/// - `ASSET_TYPE_ANT`: the Metaplex Core asset (ANT) mint pubkey.
/// - `ASSET_TYPE_TOKEN`: the SPL token mint pubkey (ARIO mint).
/// - `ASSET_TYPE_VAULT`: the client-supplied 32-byte `asset_id` from
///   the deposit, decoded as a `Pubkey`. This uniquely identifies the
///   vault escrow (it's part of the escrow PDA seeds), letting
///   indexers correlate deposit/claim/cancel/update events for the
///   same vault.
///
/// `amount`:
/// - `ASSET_TYPE_ANT`: always `0` (NFTs aren't fungible).
/// - `ASSET_TYPE_TOKEN`: mARIO transferred into escrow.
/// - `ASSET_TYPE_VAULT`: mARIO locked into the vault escrow.
///
/// `recipient_pubkey` is fixed-size for borsh stability and IDL
/// predictability. Active prefix length lives in `recipient_pubkey_len`:
/// - Arweave: 64 bytes is too small for a full RSA-4096 modulus (512 B);
///   we record the SHA-256 hash of the modulus (32 bytes, len = 32) so
///   indexers can correlate to a specific Arweave identity without
///   bloating every event by 480 bytes. The full modulus is on-chain in
///   the escrow PDA for verifiers that need it.
/// - Ethereum: the 20-byte Keccak address (len = 20).
/// Unused suffix bytes are zero-padded.
#[event]
pub struct EscrowDepositedEvent {
    pub escrow: Pubkey,
    pub depositor: Pubkey,
    pub asset_id: Pubkey,
    pub asset_type: u8,
    pub amount: u64,
    pub recipient_protocol: u8,
    pub recipient_pubkey: [u8; 64],
    pub recipient_pubkey_len: u16,
    pub timestamp: i64,
}

/// Emitted on every successful claim (ANT / Tokens / Vault) on either
/// the Arweave or Ethereum signing path.
///
/// `asset_id` and `amount` follow the same conventions as
/// `EscrowDepositedEvent`. `claim_protocol` discriminates between the
/// two signing paths (`PROTOCOL_*`).
#[event]
pub struct EscrowClaimedEvent {
    pub escrow: Pubkey,
    pub claimer: Pubkey,
    pub asset_id: Pubkey,
    pub asset_type: u8,
    pub amount: u64,
    pub claim_protocol: u8,
    pub timestamp: i64,
}

/// Emitted when a depositor cancels an escrow and recovers the asset.
/// `asset_id` follows the same conventions as `EscrowDepositedEvent`.
#[event]
pub struct EscrowCancelledEvent {
    pub escrow: Pubkey,
    pub depositor: Pubkey,
    pub asset_id: Pubkey,
    pub asset_type: u8,
    pub timestamp: i64,
}

/// Emitted when a depositor re-targets an escrow at a new recipient.
/// The new recipient pubkey is intentionally NOT included in the event
/// — it's already public in the escrow account, and including it would
/// inflate every update event by 64 bytes for marginal indexer value.
#[event]
pub struct EscrowRecipientUpdatedEvent {
    pub escrow: Pubkey,
    pub depositor: Pubkey,
    pub asset_type: u8,
    pub timestamp: i64,
}

// =========================================================================
// Helpers
// =========================================================================

/// Pad the active recipient-pubkey bytes into the fixed-size 64-byte
/// event field. For Arweave (RSA-PSS-4096) the on-chain modulus is 512
/// bytes — too large to embed in every event — so we hash it down to a
/// 32-byte SHA-256 digest. For Ethereum the 20-byte address fits as-is.
///
/// Returns `(buffer, active_len)`. Unused trailing bytes are zero.
pub fn encode_recipient_pubkey_for_event(protocol: u8, active: &[u8]) -> ([u8; 64], u16) {
    use anchor_lang::solana_program::hash::hash as sha256;

    let mut out = [0u8; 64];
    match protocol {
        // Arweave: hash the 512-byte RSA modulus to 32 bytes.
        crate::state::PROTOCOL_ARWEAVE => {
            let h = sha256(active).to_bytes();
            out[..32].copy_from_slice(&h);
            (out, 32)
        }
        // Ethereum: 20-byte address fits directly.
        crate::state::PROTOCOL_ETHEREUM => {
            let n = core::cmp::min(active.len(), 20);
            out[..n].copy_from_slice(&active[..n]);
            (out, n as u16)
        }
        // Unknown protocol: copy as much as fits, report actual length.
        // (Unreachable in practice — `validated_protocol_and_len` rejects
        // anything other than {0,1} before we reach the emit site — but
        // gives a sane fallback if a future protocol is added.)
        _ => {
            let n = core::cmp::min(active.len(), 64);
            out[..n].copy_from_slice(&active[..n]);
            (out, n as u16)
        }
    }
}
