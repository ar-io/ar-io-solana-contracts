use anchor_lang::prelude::*;

// =========================================
// SCHEMA VERSIONING
// =========================================

/// Semver-style schema version for forward-compatible migrations.
#[derive(
    AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Debug,
)]
pub struct SchemaVersion {
    pub major: u8,
    pub minor: u8,
    pub patch: u8,
}

impl SchemaVersion {
    pub const fn new(major: u8, minor: u8, patch: u8) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl std::fmt::Display for SchemaVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

pub const SCHEMA_VERSION_SIZE: usize = 3;

/// Current schema version for EscrowAnt accounts.
#[cfg(not(feature = "migration-test"))]
pub const ESCROW_ANT_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);
#[cfg(feature = "migration-test")]
pub const ESCROW_ANT_VERSION: SchemaVersion = SchemaVersion::new(1, 3, 0);

/// Current schema version for EscrowToken accounts.
pub const ESCROW_TOKEN_VERSION: SchemaVersion = SchemaVersion::new(1, 0, 0);

/// Recipient identity protocols supported by v1.
pub const PROTOCOL_ARWEAVE: u8 = 0;
pub const PROTOCOL_ETHEREUM: u8 = 1;

/// Pubkey blob length (bytes) per protocol. Padded to 512 in the account so
/// every escrow has the same fixed layout regardless of protocol; the unused
/// suffix is zeroed.
pub const ARWEAVE_PUBKEY_LEN: usize = 512; // RSA-4096 modulus
pub const ETHEREUM_PUBKEY_LEN: usize = 20; // Keccak address

/// Maximum length the `recipient_pubkey` field can hold. Sized for the
/// largest protocol (Arweave RSA-4096 = 512 bytes).
pub const RECIPIENT_PUBKEY_MAX_LEN: usize = ARWEAVE_PUBKEY_LEN;

/// PDA seed prefix. Combined with the ANT mint pubkey to derive exactly one
/// escrow account per ANT.
pub const ESCROW_ANT_SEED: &[u8] = b"escrow_ant";

/// Grace period in slots before `admin_purge_unclaimed_ant` will accept a
/// purge call on an escrow. ~5 years at Solana's ~2.5 slots/s
/// (`5 * 365 * 86_400 * 2.5`). Slot-based because `EscrowAnt.deposit_slot`
/// is the only on-chain time anchor on the escrow record. At a 5-year
/// horizon the slot-vs-wallclock drift is irrelevant for abandonment.
pub const UNCLAIMED_PURGE_GRACE_SLOTS: u64 = 394_200_000;

/// Metaplex Core program ID. Mirrored from `ario-ant` so this crate stays
/// self-contained (no `cpi`-feature dependency on `ario-ant`).
pub const MPL_CORE_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d");

/// Solana Ed25519 sigverify native program ID.
/// Used by `claim_*_attested` instructions to verify off-chain
/// attestor signatures via instruction introspection (sysvar pattern).
pub const ED25519_PROGRAM_ID: Pubkey =
    solana_program::pubkey!("Ed25519SigVerify111111111111111111111111111");

/// Ed25519 public key of the AR.IO attestor service.
///
/// Compiled into the program — rotation requires a `BPFLoaderUpgradeable`
/// upgrade swapping this constant. See the
/// `ar-io/ar-io-solana-attestor` repo's `README.md` § "Key rotation"
/// for the runbook.
///
/// The value below (`CKgG3xMKEzd2gWEZTvyrukdZHYb4hwyeTsBMeh8w9mkW`) is the
/// **production devnet** attestor pubkey — NOT a test placeholder. Its
/// secret is held by the running `ar-io/ar-io-solana-attestor` service.
///
/// Per-cluster keys: this constant is the same across `network-devnet` and
/// `network-mainnet` builds (only the canonical-message network string is
/// feature-gated). A **mainnet deploy MUST first replace this with the
/// dedicated mainnet attestor pubkey** — mainnet must not inherit the
/// devnet key. `scripts/check-attestor-pubkey.sh --cluster <name>` pins the
/// compiled value to `program-ids/<cluster>.json`'s `attestor_pubkey` and
/// fails the deploy on a mismatch (or if the cluster's key isn't pinned
/// yet). Rotation = a `BPFLoaderUpgradeable` upgrade swapping this constant;
/// see the `ar-io/ar-io-solana-attestor` repo's `README.md` § "Key rotation".
///
/// All `claim_*_attested` instructions match the Ed25519Program ix signer
/// against this constant via `verify::attested::verify_attested_signature`.
///
/// **Build-mode swap (tests only):** the `unsafe-allow-test-attestor-pubkey`
/// feature — deliberately **NOT in `default`** (see Cargo.toml) — substitutes
/// the deterministic test pubkey (seed `[1u8; 32]`) so integration tests can
/// sign with a known secret. Real-network builds never enable it and use the
/// production pubkey below.
#[cfg(not(feature = "unsafe-allow-test-attestor-pubkey"))]
pub const ATTESTOR_PUBKEY: Pubkey =
    solana_program::pubkey!("CKgG3xMKEzd2gWEZTvyrukdZHYb4hwyeTsBMeh8w9mkW");

/// Test-build override of `ATTESTOR_PUBKEY` — the deterministic test
/// pubkey (seed `[1u8; 32]`). Compiled only when the opt-in
/// `unsafe-allow-test-attestor-pubkey` feature is enabled (NOT in `default`;
/// tests pass it explicitly). Real-network builds never enable it and use
/// the prod constant above instead.
#[cfg(feature = "unsafe-allow-test-attestor-pubkey")]
pub const ATTESTOR_PUBKEY: Pubkey =
    solana_program::pubkey::Pubkey::new_from_array(TEST_ATTESTOR_PUBKEY_BYTES);

/// Deterministic test value of `ATTESTOR_PUBKEY` (derived from Ed25519
/// seed `[1u8; 32]`). Public knowledge — used by integration tests so
/// they can construct valid Ed25519Program ixs without provisioning a
/// real attestor key. **Must never reach a real cluster** because
/// anyone can recompute the secret seed; the const-eval guard below
/// enforces that at build time.
pub(crate) const TEST_ATTESTOR_PUBKEY_BYTES: [u8; 32] = [
    0x8a, 0x88, 0xe3, 0xdd, 0x74, 0x09, 0xf1, 0x95, 0xfd, 0x52, 0xdb, 0x2d, 0x3c, 0xba, 0x5d, 0x72,
    0xca, 0x67, 0x09, 0xbf, 0x1d, 0x94, 0x12, 0x1b, 0xf3, 0x74, 0x88, 0x01, 0xb4, 0x0f, 0x6f, 0x5c,
];

// F-4: refuse to compile any *real-network* deployment that still has
// the test ATTESTOR_PUBKEY baked in. The check runs at compile time, so
// no deploy path — `devnet-deploy.sh`, manual `solana program deploy`,
// future `mainnet-deploy.sh`, anything — can ship the test key. The
// `ATTESTOR_PUBKEY` const above must be replaced with the real attestor
// pubkey before building for `network-mainnet` or `network-devnet`.
//
// This is in addition to the runtime `check-attestor-pubkey.sh
// --strict` guardrail wired into `devnet-deploy.sh`; defense in depth.
//
// The runtime equality check is duplicated here as a `const fn` so the
// compiler can evaluate it without needing `Pubkey::eq` to be const.
// Used only by the `cfg`-gated F-4 const-eval guard below. Default
// non-SBF builds enable `unsafe-allow-test-attestor-pubkey`, so the
// guard is compiled out and this becomes "unused" — silence the lint.
#[allow(dead_code)]
const fn pubkey_bytes_eq(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut i = 0;
    while i < 32 {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

// Fires on any real-network build that has neither replaced the test
// pubkey nor explicitly opted in via `unsafe-allow-test-attestor-pubkey`.
// The escape hatch exists for localnet integration tests that need the
// deterministic seed `[1u8; 32]` to construct valid Ed25519Program ixs.
#[cfg(all(
    any(feature = "network-mainnet", feature = "network-devnet"),
    not(feature = "unsafe-allow-test-attestor-pubkey"),
))]
const _: () = {
    if pubkey_bytes_eq(&ATTESTOR_PUBKEY.to_bytes(), &TEST_ATTESTOR_PUBKEY_BYTES) {
        panic!(
            "REFUSING TO BUILD: ATTESTOR_PUBKEY in src/state.rs still has the deterministic test \
             value (derived from public seed [1u8; 32]). Anyone reading this code can mint valid \
             attestations. Replace it with the real attestor pubkey before building for any real \
             network. See the ar-io/ar-io-solana-attestor repo § \"Initial deploy\"."
        );
    }
};

/// Per-escrow custody record. One PDA per escrowed ANT, derived from
/// `[ESCROW_ANT_SEED, ant_mint]`. Layout matches `docs/ANT_ESCROW_DESIGN.md`
/// § Account model byte-for-byte (653 bytes payload + 8-byte Anchor
/// discriminator = 661 bytes on-chain).
///
/// The `_reserved` tail and the explicit `version` byte are forward-compat
/// hooks so additive field changes can ship without a `realloc` migration.
#[account]
pub struct EscrowAnt {
    /// Schema version. v1 = `ESCROW_ANT_VERSION`.
    pub version: SchemaVersion,
    /// Cached PDA bump. Faster than `find_program_address` on signer-seed
    /// reconstruction during transfer CPIs.
    pub bump: u8,
    /// Wallet that called `deposit_ant`. Sole authority for
    /// `cancel_deposit` and `update_recipient`; receives rent on close.
    pub depositor: Pubkey,
    /// Metaplex Core asset (ANT) mint pubkey held in custody.
    /// Denormalized from PDA seeds for defense-in-depth account validation.
    pub ant_mint: Pubkey,
    /// `0` = Arweave RSA-PSS-4096, `1` = Ethereum ECDSA secp256k1.
    pub recipient_protocol: u8,
    /// Active length (in bytes) of the recipient pubkey blob below.
    /// Constrained at write time to {512, 20} per protocol.
    pub recipient_pubkey_len: u16,
    /// Recipient identity bytes, zero-padded to `RECIPIENT_PUBKEY_MAX_LEN`.
    /// - Arweave: 512-byte big-endian RSA modulus (the JWK `n` field).
    /// - Ethereum: 20-byte Keccak address followed by 492 zero bytes.
    pub recipient_pubkey: [u8; RECIPIENT_PUBKEY_MAX_LEN],
    /// Anti-replay nonce. Initial value:
    ///   `sha256(deposit_slot_le_bytes || ant_mint || depositor)`
    /// Rotated by `update_recipient` to invalidate any in-flight claim sigs.
    pub nonce: [u8; 32],
    /// Slot at deposit. Informational only — used as a salt for nonce
    /// derivation but not consulted on claim. Useful for indexers and
    /// support tooling.
    pub deposit_slot: u64,
    /// Reserved bytes. Hold zero until repurposed in a future schema bump.
    /// Shrunk from 32 to 30 to absorb the SchemaVersion expansion (u8→3 bytes).
    #[cfg(not(feature = "migration-test"))]
    pub _reserved: [u8; 30],
    #[cfg(feature = "migration-test")]
    pub _reserved: [u8; 17],
    #[cfg(feature = "migration-test")]
    pub field_1: u64,
    #[cfg(feature = "migration-test")]
    pub field_2: u32,
    #[cfg(feature = "migration-test")]
    pub field_3: bool,
}

impl EscrowAnt {
    /// Total on-chain byte size including the 8-byte Anchor discriminator.
    /// Used by the `init` constraint's `space = EscrowAnt::SIZE` literal.
    /// The migration-test feature carves test fields out of `_reserved`
    /// (17 + 8 + 4 + 1 = 30), so the total stays identical.
    pub const SIZE: usize = 8  // anchor discriminator
        + 3                    // version (SchemaVersion)
        + 1                    // bump
        + 32                   // depositor
        + 32                   // ant_mint
        + 1                    // recipient_protocol
        + 2                    // recipient_pubkey_len
        + RECIPIENT_PUBKEY_MAX_LEN
        + 32                   // nonce
        + 8                    // deposit_slot
        + 30; // _reserved (or 17 + 8 + 4 + 1 under migration-test)

    /// Returns the active prefix of the recipient pubkey blob. Convenience
    /// for verifiers that only need the meaningful bytes.
    #[inline]
    pub fn recipient_pubkey_active(&self) -> &[u8] {
        let len = self.recipient_pubkey_len as usize;
        debug_assert!(len <= RECIPIENT_PUBKEY_MAX_LEN);
        &self.recipient_pubkey[..len]
    }
}

/// Exact on-chain size literal — guard against accidental field reordering
/// shifting the storage cost. The constant is asserted in unit tests.
pub const ESCROW_ANT_ON_CHAIN_SIZE: usize = 661;

// =========================================================================
// Token / Vault Escrow (coexists with EscrowAnt)
// =========================================================================

/// Asset type discriminant for the generalized escrow.
///
/// `ASSET_TYPE_ANT` covers the legacy `EscrowAnt` flow (NFT custody) and
/// is carried in `EscrowDepositedEvent` / `EscrowClaimedEvent` /
/// `EscrowCancelledEvent` / `EscrowRecipientUpdatedEvent` to discriminate
/// from the generalized token / vault escrows.
pub const ASSET_TYPE_ANT: u8 = 0;
pub const ASSET_TYPE_TOKEN: u8 = 1;
pub const ASSET_TYPE_VAULT: u8 = 2;

/// PDA seed prefixes for token and vault escrows.
pub const ESCROW_TOKEN_SEED: &[u8] = b"escrow_token";
pub const ESCROW_VAULT_SEED: &[u8] = b"escrow_vault";

/// Escrow account for ARIO tokens (liquid) or vaulted positions (time-locked).
///
/// The `asset_type` field discriminates between a liquid token transfer
/// (claim sends tokens to claimant's ATA) and a vaulted transfer (claim
/// CPIs into `ario-core::vaulted_transfer` to create a time-locked vault
/// for the claimant with the remaining lock duration).
///
/// PDA seeds:
///   Token: ["escrow_token", depositor, asset_id]
///   Vault: ["escrow_vault", depositor, asset_id]
///
/// `asset_id` is a client-supplied 32-byte identifier. For the migration
/// batch, it's deterministic: `sha256("token-escrow:" + arweave_addr)` or
/// `sha256("vault-escrow:" + arweave_addr + ":" + vault_id)`.
#[account]
pub struct EscrowToken {
    /// Schema version. v1 = `SchemaVersion::new(1, 0, 0)`.
    pub version: SchemaVersion,
    /// PDA bump.
    pub bump: u8,
    /// Wallet that deposited the tokens.
    pub depositor: Pubkey,
    /// `ASSET_TYPE_TOKEN` (1) or `ASSET_TYPE_VAULT` (2).
    pub asset_type: u8,
    /// Amount of ARIO (mARIO) held in escrow.
    pub amount: u64,
    /// The ARIO SPL token mint. Validated at deposit time.
    pub ario_mint: Pubkey,
    /// Client-supplied unique identifier for this escrow. Part of PDA seeds.
    pub asset_id: [u8; 32],
    /// `0` = Arweave RSA-PSS-4096, `1` = Ethereum ECDSA secp256k1.
    pub recipient_protocol: u8,
    /// Active length of the recipient pubkey blob.
    pub recipient_pubkey_len: u16,
    /// Recipient identity bytes, zero-padded to 512.
    pub recipient_pubkey: [u8; RECIPIENT_PUBKEY_MAX_LEN],
    /// Anti-replay nonce.
    pub nonce: [u8; 32],
    /// Slot at deposit.
    pub deposit_slot: u64,
    /// For vault escrows: the unix timestamp at which the vault unlocks.
    /// Zero for liquid token escrows.
    pub vault_end_timestamp: i64,
    /// Reserved. Always `false`: `deposit_vault` rejects `revocable=true` and
    /// claim re-locks are always non-revocable (the escrow has no field for a
    /// legitimate revoker, so a revocable re-lock could only be controlled by
    /// the unbound claim-tx payer — a theft vector). Kept for layout/ABI
    /// stability. See ADR-021. (Also `false` for liquid token escrows.)
    pub vault_revocable: bool,
    /// Reserved for future fields.
    /// Shrunk from 32 to 30 to absorb the SchemaVersion expansion (u8→3 bytes).
    pub _reserved: [u8; 30],
}

impl EscrowToken {
    pub const SIZE: usize = 8  // anchor discriminator
        + 3                    // version (SchemaVersion)
        + 1                    // bump
        + 32                   // depositor
        + 1                    // asset_type
        + 8                    // amount
        + 32                   // ario_mint
        + 32                   // asset_id
        + 1                    // recipient_protocol
        + 2                    // recipient_pubkey_len
        + RECIPIENT_PUBKEY_MAX_LEN
        + 32                   // nonce
        + 8                    // deposit_slot
        + 8                    // vault_end_timestamp
        + 1                    // vault_revocable
        + 30; // _reserved

    /// Returns the active prefix of the recipient pubkey blob.
    #[inline]
    pub fn recipient_pubkey_active(&self) -> &[u8] {
        let len = self.recipient_pubkey_len as usize;
        debug_assert!(len <= RECIPIENT_PUBKEY_MAX_LEN);
        &self.recipient_pubkey[..len]
    }
}

/// On-chain size: 8 + 1+1+32+1+8+32+32+1+2+512+32+8+8+1+32 = 711
pub const ESCROW_TOKEN_ON_CHAIN_SIZE: usize = 711;

/// Derive the initial nonce for a freshly-deposited escrow. Centralized so
/// test code, integration tests, and the on-chain handler all agree on the
/// scheme byte-for-byte.
///
/// Format:
///   `sha256(deposit_slot.to_le_bytes() || ant_mint || depositor)`
pub fn derive_initial_nonce(deposit_slot: u64, ant_mint: &Pubkey, depositor: &Pubkey) -> [u8; 32] {
    use anchor_lang::solana_program::hash::hashv;

    let slot_bytes = deposit_slot.to_le_bytes();
    let h = hashv(&[&slot_bytes, ant_mint.as_ref(), depositor.as_ref()]);
    h.to_bytes()
}

/// Derive the rotated nonce for `update_recipient`. Mixes the previous
/// nonce in so the new value is unguessable even if `slot`, `ant_mint`,
/// and `depositor` are publicly known.
///
/// Format:
///   `sha256(slot.to_le_bytes() || ant_mint || depositor || old_nonce)`
pub fn derive_rotated_nonce(
    slot: u64,
    ant_mint: &Pubkey,
    depositor: &Pubkey,
    old_nonce: &[u8; 32],
) -> [u8; 32] {
    use anchor_lang::solana_program::hash::hashv;

    let slot_bytes = slot.to_le_bytes();
    let h = hashv(&[
        &slot_bytes,
        ant_mint.as_ref(),
        depositor.as_ref(),
        old_nonce,
    ]);
    h.to_bytes()
}

/// Validate that `(protocol, pubkey_len)` is one of the accepted pairs.
/// Returns the canonical `(protocol_byte, expected_len)` tuple on success.
pub fn validated_protocol_and_len(protocol: u8, pubkey_len: usize) -> Option<(u8, usize)> {
    match (protocol, pubkey_len) {
        (PROTOCOL_ARWEAVE, ARWEAVE_PUBKEY_LEN) => Some((PROTOCOL_ARWEAVE, ARWEAVE_PUBKEY_LEN)),
        (PROTOCOL_ETHEREUM, ETHEREUM_PUBKEY_LEN) => Some((PROTOCOL_ETHEREUM, ETHEREUM_PUBKEY_LEN)),
        _ => None,
    }
}

/// Read the current owner pubkey from a Metaplex Core AssetV1 raw account
/// data buffer. The first byte of an AssetV1 account is the discriminator
/// `1` and the next 32 bytes are the owner pubkey. Lifted byte-for-byte
/// from `programs/ario-ant/src/lib.rs::read_mpl_core_owner` so the escrow
/// program can do the same NFT-holder check without a circular dep.
pub fn read_mpl_core_owner(data: &[u8]) -> Result<Pubkey> {
    use crate::error::EscrowError;
    require!(data.len() >= 33, EscrowError::InvalidAsset);
    require!(data[0] == 1, EscrowError::InvalidAsset);

    let owner_bytes: [u8; 32] = data[1..33]
        .try_into()
        .map_err(|_| error!(EscrowError::InvalidAsset))?;
    Ok(Pubkey::from(owner_bytes))
}

/// Audit L22: validate that an account passed as an MPL Core ANT really is
/// an `AssetV1` (not a `HashedAssetV1`, collection, or other Core variant).
/// The account's program-owner is already pinned by the Anchor `constraint`
/// on each handler's accounts struct; this function adds the inner-data
/// discriminator check that `read_mpl_core_owner` does on the deposit path,
/// brought to the cancel/claim paths for parity.
pub fn assert_mpl_core_asset_v1(account: &AccountInfo) -> Result<()> {
    use crate::error::EscrowError;
    let data = account.try_borrow_data()?;
    require!(!data.is_empty() && data[0] == 1, EscrowError::InvalidAsset);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escrow_ant_size_matches_design_doc() {
        assert_eq!(EscrowAnt::SIZE, ESCROW_ANT_ON_CHAIN_SIZE);
        assert_eq!(EscrowAnt::SIZE, 661);
    }

    #[test]
    fn escrow_token_size_matches_plan() {
        assert_eq!(EscrowToken::SIZE, ESCROW_TOKEN_ON_CHAIN_SIZE);
        assert_eq!(EscrowToken::SIZE, 711);
    }

    #[test]
    fn validated_protocol_and_len_accepts_arweave() {
        let r = validated_protocol_and_len(PROTOCOL_ARWEAVE, ARWEAVE_PUBKEY_LEN);
        assert_eq!(r, Some((PROTOCOL_ARWEAVE, ARWEAVE_PUBKEY_LEN)));
    }

    #[test]
    fn validated_protocol_and_len_accepts_ethereum() {
        let r = validated_protocol_and_len(PROTOCOL_ETHEREUM, ETHEREUM_PUBKEY_LEN);
        assert_eq!(r, Some((PROTOCOL_ETHEREUM, ETHEREUM_PUBKEY_LEN)));
    }

    #[test]
    fn validated_protocol_and_len_rejects_wrong_pairs() {
        // Right protocol, wrong length
        assert!(validated_protocol_and_len(PROTOCOL_ARWEAVE, 256).is_none());
        assert!(validated_protocol_and_len(PROTOCOL_ETHEREUM, 32).is_none());
        // Unknown protocol
        assert!(validated_protocol_and_len(2, 512).is_none());
        assert!(validated_protocol_and_len(255, 20).is_none());
    }

    #[test]
    fn derive_initial_nonce_is_deterministic() {
        let mint = Pubkey::new_unique();
        let depositor = Pubkey::new_unique();
        let a = derive_initial_nonce(1234, &mint, &depositor);
        let b = derive_initial_nonce(1234, &mint, &depositor);
        assert_eq!(a, b);
    }

    #[test]
    fn derive_initial_nonce_changes_with_slot() {
        let mint = Pubkey::new_unique();
        let depositor = Pubkey::new_unique();
        let a = derive_initial_nonce(1, &mint, &depositor);
        let b = derive_initial_nonce(2, &mint, &depositor);
        assert_ne!(a, b);
    }

    #[test]
    fn derive_rotated_nonce_changes_with_old_nonce() {
        let mint = Pubkey::new_unique();
        let depositor = Pubkey::new_unique();
        let a = derive_rotated_nonce(1, &mint, &depositor, &[0u8; 32]);
        let b = derive_rotated_nonce(1, &mint, &depositor, &[1u8; 32]);
        assert_ne!(a, b);
    }

    #[test]
    fn read_mpl_core_owner_extracts_pubkey() {
        let target = Pubkey::new_unique();
        let mut data = vec![1u8]; // AssetV1 discriminator
        data.extend_from_slice(target.as_ref());
        data.extend_from_slice(&[0u8; 64]); // trailing junk
        assert_eq!(read_mpl_core_owner(&data).unwrap(), target);
    }

    #[test]
    fn read_mpl_core_owner_rejects_short_data() {
        assert!(read_mpl_core_owner(&[]).is_err());
        assert!(read_mpl_core_owner(&[1u8; 32]).is_err());
    }

    #[test]
    fn read_mpl_core_owner_rejects_wrong_disc() {
        let mut data = vec![2u8];
        data.extend_from_slice(&[0u8; 32]);
        assert!(read_mpl_core_owner(&data).is_err());
    }
}
