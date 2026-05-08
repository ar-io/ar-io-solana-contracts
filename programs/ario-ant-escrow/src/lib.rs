use anchor_lang::prelude::*;

// Network string baked into the canonical claim message at compile time. The
// canonical message format prevents cross-network signature replay by binding
// every signed message to either `solana-mainnet` or `solana-devnet`.
//
// Exactly one of `network-mainnet` / `network-devnet` must be enabled. The
// default feature set picks mainnet so plain `cargo check -p ario-ant-escrow`
// works; devnet/CI builds opt in with
// `cargo build-sbf --no-default-features --features network-devnet`.
#[cfg(all(feature = "network-mainnet", feature = "network-devnet"))]
compile_error!(
    "ario-ant-escrow: features `network-mainnet` and `network-devnet` are mutually exclusive"
);
#[cfg(not(any(feature = "network-mainnet", feature = "network-devnet")))]
compile_error!("ario-ant-escrow: one of `network-mainnet` or `network-devnet` must be enabled");

/// Network string included verbatim in the canonical claim message.
/// See `docs/ANT_ESCROW_DESIGN.md` § Canonical message format.
#[cfg(feature = "network-mainnet")]
pub const NETWORK: &[u8] = b"solana-mainnet";
#[cfg(all(feature = "network-devnet", not(feature = "network-mainnet")))]
pub const NETWORK: &[u8] = b"solana-devnet";

// Placeholder — intentionally does NOT match any real keypair, mirroring
// the convention used by the other workspace programs. At deploy time
// `./build-sbf.sh --sync` (or `anchor keys sync`) patches this to the
// pubkey of `target/deploy/ario_ant_escrow-keypair.json` and restores
// after the build via the EXIT trap. See CLAUDE.md → "Contracts" for the
// full flow.
declare_id!("ARioAntEscrowXXXXXXXXXXXXXXXXXXXXXXXXXXXXXX");

pub mod canonical;
pub mod error;
pub mod events;
pub mod instructions;
pub mod mpl_core_cpi;
pub mod state;
pub mod vault_introspect;
pub mod verify;

pub use events::{
    EscrowCancelledEvent, EscrowClaimedEvent, EscrowDepositedEvent, EscrowRecipientUpdatedEvent,
    ASSET_TYPE_ANT, ASSET_TYPE_TOKEN, ASSET_TYPE_VAULT, PROTOCOL_ARWEAVE, PROTOCOL_ETHEREUM,
};
use instructions::*;

/// AR.IO Trustless ANT Escrow Program
///
/// Holds Metaplex Core ANT NFTs in a per-mint PDA and releases them to a
/// claimant designated by an Arweave RSA-PSS-4096 or Ethereum ECDSA secp256k1
/// signature verified fully on-chain. No off-chain authority, no oracle.
///
/// See `docs/ANT_ESCROW_DESIGN.md` for the full technical design and
/// `docs/ANT_ESCROW_IMPLEMENTATION_PLAN.md` for the phase-by-phase roadmap.
#[program]
pub mod ario_ant_escrow {
    use super::*;

    /// Lock an ANT into escrow targeted at an Arweave or Ethereum identity.
    ///
    /// `recipient_protocol`: `0` = Arweave RSA-PSS-4096, `1` = Ethereum ECDSA.
    /// `recipient_pubkey`: 512-byte RSA modulus (Arweave) or 20-byte address
    /// (Ethereum); any other (protocol, length) pair is rejected.
    pub fn deposit_ant(
        ctx: Context<DepositAnt>,
        recipient_protocol: u8,
        recipient_pubkey: Vec<u8>,
    ) -> Result<()> {
        instructions::deposit::handler(ctx, recipient_protocol, recipient_pubkey)
    }

    /// Return the escrowed ANT to the depositor and close the escrow PDA.
    /// Only callable by `escrow.depositor`.
    pub fn cancel_deposit(ctx: Context<CancelDeposit>) -> Result<()> {
        instructions::cancel::handler(ctx)
    }

    /// Re-target the escrow at a different recipient identity. Rotates the
    /// nonce so any in-flight claim sigs bound to the prior recipient are
    /// invalidated. Only callable by `escrow.depositor`.
    pub fn update_recipient(
        ctx: Context<UpdateRecipient>,
        new_protocol: u8,
        new_pubkey: Vec<u8>,
    ) -> Result<()> {
        instructions::update_recipient::handler(ctx, new_protocol, new_pubkey)
    }

    /// Release the ANT to `claimant` using an off-chain RSA-PSS
    /// attestation that has been re-signed with Ed25519 by the
    /// AR.IO attestor service. The transaction must include an
    /// Ed25519Program native sigverify ix immediately preceding this
    /// one; we use sysvar instruction-introspection to confirm the
    /// signing pubkey matches `ATTESTOR_PUBKEY` and the signed bytes
    /// match the canonical claim message reconstructed from escrow
    /// state. See `migration/attestor/README.md` and the module-level
    /// docs in `instructions/claim_arweave_attested.rs`.
    pub fn claim_ant_arweave_attested(
        ctx: Context<ClaimAntArweaveAttested>,
        message_nonce: [u8; 32],
    ) -> Result<()> {
        instructions::claim_arweave_attested::handler(ctx, message_nonce)
    }

    /// Release the ANT to `claimant` after on-chain ECDSA secp256k1 +
    /// EIP-191 verification. Verifies via the always-enabled
    /// `secp256k1_recover` syscall.
    pub fn claim_ant_ethereum(
        ctx: Context<ClaimAntEthereum>,
        message_nonce: [u8; 32],
        signature: [u8; 65],
    ) -> Result<()> {
        instructions::claim_ethereum::handler(ctx, message_nonce, signature)
    }

    // =================================================================
    // Token + Vault Escrow
    // =================================================================

    /// Deposit ARIO tokens into escrow for a designated Arweave or
    /// Ethereum recipient. Tokens transfer from the depositor's ATA to
    /// the escrow PDA's ATA.
    pub fn deposit_tokens(
        ctx: Context<DepositTokens>,
        asset_id: [u8; 32],
        amount: u64,
        recipient_protocol: u8,
        recipient_pubkey: Vec<u8>,
    ) -> Result<()> {
        instructions::deposit_tokens::handler(
            ctx,
            asset_id,
            amount,
            recipient_protocol,
            recipient_pubkey,
        )
    }

    /// Release escrowed ARIO tokens to `claimant` using an off-chain
    /// RSA-PSS attestation re-signed with Ed25519 by the AR.IO attestor
    /// service. The transaction must include an Ed25519Program native
    /// sigverify ix immediately preceding this one. See
    /// `instructions/claim_tokens_arweave_attested.rs`.
    pub fn claim_tokens_arweave_attested(
        ctx: Context<ClaimTokensArweaveAttested>,
        message_nonce: [u8; 32],
    ) -> Result<()> {
        instructions::claim_tokens_arweave_attested::handler(ctx, message_nonce)
    }

    /// Release escrowed ARIO tokens to `claimant` after on-chain
    /// ECDSA secp256k1 + EIP-191 verification (Ethereum path).
    pub fn claim_tokens_ethereum(
        ctx: Context<ClaimTokensEthereum>,
        message_nonce: [u8; 32],
        signature: [u8; 65],
    ) -> Result<()> {
        instructions::claim_tokens_ethereum::handler(ctx, message_nonce, signature)
    }

    /// Return escrowed ARIO tokens to the depositor and close the
    /// escrow PDA. Only callable by `escrow.depositor`.
    pub fn cancel_token_deposit(ctx: Context<CancelTokenDeposit>) -> Result<()> {
        instructions::cancel_token_deposit::handler(ctx)
    }

    /// Re-target a token/vault escrow at a different recipient identity.
    /// Rotates the nonce. Only callable by `escrow.depositor`.
    pub fn update_token_recipient(
        ctx: Context<UpdateTokenRecipient>,
        new_protocol: u8,
        new_pubkey: Vec<u8>,
    ) -> Result<()> {
        instructions::update_token_recipient::handler(ctx, new_protocol, new_pubkey)
    }

    // =================================================================
    // Vault Escrow (time-locked)
    // =================================================================

    /// Deposit ARIO tokens into escrow as a time-locked vault for a
    /// designated Arweave or Ethereum recipient.
    pub fn deposit_vault(
        ctx: Context<DepositVault>,
        asset_id: [u8; 32],
        amount: u64,
        lock_duration_seconds: i64,
        revocable: bool,
        recipient_protocol: u8,
        recipient_pubkey: Vec<u8>,
    ) -> Result<()> {
        instructions::deposit_vault::handler(
            ctx,
            asset_id,
            amount,
            lock_duration_seconds,
            revocable,
            recipient_protocol,
            recipient_pubkey,
        )
    }

    /// Release escrowed vault tokens to `claimant` using an off-chain
    /// RSA-PSS attestation re-signed with Ed25519 by the AR.IO attestor
    /// service. If the vault is still active, this ix releases tokens
    /// to `payer_token_account` and requires a sibling
    /// `ario_core::vaulted_transfer` ix in the same tx; if expired,
    /// transfers liquid to `claimant_token_account`.
    /// The transaction must include an Ed25519Program native sigverify
    /// ix immediately preceding this one (introspected via sysvar);
    /// for the active-vault path it must ALSO include a sibling
    /// `ario_core::vaulted_transfer` ix anywhere in the tx. See
    /// `instructions/claim_vault_arweave_attested.rs`.
    pub fn claim_vault_arweave_attested(
        ctx: Context<ClaimVaultArweaveAttested>,
        message_nonce: [u8; 32],
    ) -> Result<()> {
        instructions::claim_vault_arweave_attested::handler(ctx, message_nonce)
    }

    /// Release escrowed vault tokens to `claimant` after on-chain
    /// ECDSA secp256k1 + EIP-191 verification (Ethereum path). Same
    /// active/expired branching as `claim_vault_arweave_attested`.
    pub fn claim_vault_ethereum(
        ctx: Context<ClaimVaultEthereum>,
        message_nonce: [u8; 32],
        signature: [u8; 65],
    ) -> Result<()> {
        instructions::claim_vault_ethereum::handler(ctx, message_nonce, signature)
    }

    /// Return escrowed vault tokens to the depositor. Uses ESCROW_VAULT_SEED.
    pub fn cancel_vault_deposit(ctx: Context<CancelVaultDeposit>) -> Result<()> {
        instructions::cancel_vault_deposit::handler(ctx)
    }

    /// Re-target a vault escrow at a different recipient identity.
    pub fn update_vault_recipient(
        ctx: Context<UpdateVaultRecipient>,
        new_protocol: u8,
        new_pubkey: Vec<u8>,
    ) -> Result<()> {
        instructions::update_vault_recipient::handler(ctx, new_protocol, new_pubkey)
    }
}
