use anchor_lang::prelude::*;

/// Errors raised by `ario-ant-escrow`. Codes are stable once committed and
/// must not be reordered without a major version bump (clients pin against
/// `ERROR_CODE_OFFSET + variant_index`).
#[error_code]
pub enum EscrowError {
    /// `recipient_protocol` outside the {0=Arweave, 1=Ethereum} domain.
    #[msg("Invalid recipient protocol (must be 0=Arweave or 1=Ethereum)")]
    InvalidRecipientProtocol,

    /// `recipient_pubkey.len()` doesn't match the protocol's expected size:
    /// Arweave needs 512 bytes (RSA-4096 modulus), Ethereum needs 20 bytes
    /// (Keccak address).
    #[msg("Invalid recipient pubkey length for the given protocol")]
    InvalidRecipientPubkeyLength,

    /// Caller signing `cancel_deposit` or `update_recipient` is not the
    /// account that originally deposited the ANT.
    #[msg("Unauthorized: only the original depositor can perform this action")]
    NotDepositor,

    /// Asset account does not look like a Metaplex Core AssetV1 (wrong owner
    /// program or first-byte key discriminator). Mirrors the same check
    /// `ario-ant` runs on every NFT-touching instruction.
    #[msg("Invalid Metaplex Core asset (wrong owner program or asset discriminator)")]
    InvalidAsset,

    /// Caller of `deposit_ant` is not the current Metaplex Core asset owner.
    #[msg("Unauthorized: caller is not the current ANT owner")]
    NotAntOwner,

    /// The escrow PDA's stored `ant_mint` does not match the asset account
    /// passed by the caller. Defends against PDA seed collisions or
    /// caller-supplied account mix-ups.
    #[msg("Escrow PDA does not correspond to the supplied ANT mint")]
    AntMintMismatch,

    /// The signed `message_nonce` parameter does not equal `escrow.nonce`.
    /// Indicates either replay of a stale signature or a bug in the client
    /// canonical-message construction.
    #[msg("Claim signature nonce does not match the escrow's current nonce")]
    NonceMismatch,

    /// `claim_ant_arweave` was invoked on an escrow whose
    /// `recipient_protocol` is not Arweave (likely a misrouted client).
    #[msg("Wrong claim path: this escrow targets a different protocol")]
    ProtocolMismatch,

    /// PSS / ECDSA verification rejected the signature. The error is opaque
    /// on purpose — callers should not need to distinguish "tampered
    /// message" from "wrong key" from "malformed sig".
    #[msg("Signature verification failed")]
    SignatureVerificationFailed,

    /// `salt_len` parameter outside the `[0, 32]` range we accept for
    /// PSS-SHA256. Bounds the CU cost of MGF1 derivation.
    #[msg("Invalid PSS salt length (must be ≤ 32 bytes)")]
    InvalidSaltLength,

    /// Recovered ECDSA pubkey hashes to an address that doesn't match
    /// `escrow.recipient_pubkey[..20]`.
    #[msg("Recovered Ethereum address does not match recipient")]
    EthereumAddressMismatch,

    /// `s` component of an ECDSA signature is greater than `secp256k1_n / 2`,
    /// rejected per EIP-2 to prevent signature malleability.
    #[msg("ECDSA signature is malleable (high-S form rejected per EIP-2)")]
    EcdsaHighS,

    /// `v` byte (recovery id) is not in the accepted set {0, 1, 27, 28}.
    #[msg("Invalid ECDSA recovery id")]
    InvalidRecoveryId,

    // ----- Token / Vault escrow errors -----
    /// Token/vault deposit amount is zero.
    #[msg("Amount must be greater than zero")]
    AmountZero,

    /// Vault lock duration is below the protocol minimum (14 days).
    #[msg("Vault lock duration below minimum")]
    VaultDurationTooShort,

    /// Vault deposit amount is below the protocol minimum (100 ARIO).
    #[msg("Vault amount below minimum (100 ARIO)")]
    VaultAmountBelowMinimum,

    /// The escrow's `asset_type` does not match the claim instruction used.
    #[msg("Asset type mismatch")]
    AssetTypeMismatch,

    /// ARIO mint passed to the instruction does not match the escrow's stored mint.
    #[msg("ARIO mint mismatch")]
    MintMismatch,

    /// Token account owner does not match the expected claimant or payer.
    #[msg("Token account owner does not match the expected account")]
    TokenAccountOwnerMismatch,

    /// Arithmetic overflow in timestamp or amount calculation.
    #[msg("Arithmetic overflow")]
    ArithmeticOverflow,

    /// Vault claim requires a matching `vaulted_transfer` instruction in the
    /// same transaction. Either it's missing, or its parameters (amount,
    /// duration, recipient) don't match the escrow's stored values.
    #[msg("Missing or invalid vaulted_transfer instruction in transaction")]
    MissingVaultedTransferInstruction,

    /// `claim_*_attested` requires a preceding Ed25519Program sigverify ix
    /// in the same transaction; either none was found, the ix at the
    /// expected position is for the wrong program, or the introspection
    /// sysvar lookup failed.
    #[msg("Missing Ed25519Program sigverify instruction for attested claim")]
    MissingAttestationInstruction,

    /// The Ed25519Program ix data layout is malformed: too short, wrong
    /// signature count, or uses cross-instruction data references that
    /// we don't accept.
    #[msg("Malformed Ed25519Program sigverify instruction data")]
    MalformedAttestationInstruction,

    /// The Ed25519Program sigverify ix's signing pubkey does not match
    /// the program's `ATTESTOR_PUBKEY` constant.
    #[msg("Attestation was signed by an unauthorized pubkey")]
    AttestationSignerMismatch,

    /// The bytes signed in the Ed25519Program ix do not match the
    /// canonical claim message reconstructed from on-chain escrow state.
    #[msg("Attested message does not match canonical claim message")]
    AttestationMessageMismatch,

    // ----- Schema migration errors -----
    /// Account is already at the latest schema version.
    #[msg("Account is already at the latest schema version")]
    AlreadyLatestVersion,

    /// Unknown schema version — no migration path exists from this version.
    #[msg("Unknown schema version — no migration path exists from this version")]
    UnknownSchemaVersion,

    // ----- Admin purge errors -----
    /// `admin_purge_unclaimed_ant`: signer is not `ArioConfig.authority`.
    /// Only the protocol admin may purge abandoned escrows.
    #[msg("Unauthorized: only the protocol authority may call this instruction")]
    Unauthorized,

    /// `admin_purge_unclaimed_ant`: the grace period
    /// (`UNCLAIMED_PURGE_GRACE_SLOTS`, ~5 years) has not yet elapsed
    /// since the escrow's `deposit_slot`. Retry later.
    #[msg("Cannot purge yet: the 5-year unclaimed-grace period has not elapsed")]
    PurgeGraceNotElapsed,
}
