# ANT Escrow Protocol Specification

**Program:** `ario-ant-escrow`
**Version:** v1
**Status:** Implemented
**Last updated:** 2026-05-05

> **Scope note:** This spec covers the original **ANT NFT escrow** instructions
> (5 instructions: `deposit_ant`, `cancel_deposit`, `update_recipient`,
> `claim_ant_arweave`, `claim_ant_ethereum`). The program was later extended
> with **token escrow** (`deposit_tokens` / `claim_tokens_*` / `cancel_token_deposit` /
> `update_token_recipient`) and **vault escrow** (`deposit_vault` / `claim_vault_*` /
> `cancel_vault_deposit` / `update_vault_recipient`) — 10 additional instructions.
> Those follow the same canonical message and signing conventions documented
> here but use `EscrowToken` and `EscrowVault` account types respectively.
> A v2 spec covering all three escrow modes is pending.

---

## 1. Overview

The `ario-ant-escrow` program is a Solana program that holds Metaplex Core ANT (Arweave Name Token) NFTs, ARIO tokens, and time-locked vaults in trustless custody and releases them when presented with a valid cryptographic signature from a designated Arweave (RSA-PSS-4096) or Ethereum (ECDSA secp256k1) key. Ethereum signatures verify fully on-chain via the always-enabled `secp256k1_recover` syscall. Arweave signatures take a hybrid path: a single-purpose off-chain attestor service ([`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor)) verifies the RSA-PSS signature and re-signs the canonical claim message with Ed25519; the on-chain program verifies the cheap Ed25519 signature via Solana's native sigverify program + sysvar instruction-introspection. The attestor's Ed25519 public key is compiled into the program; rotation requires a `BPFLoaderUpgradeable` upgrade. Escrows persist indefinitely until claimed, cancelled by the depositor, or redirected to a new recipient. See ADR-017 for the architecture rationale.

---

## 2. Canonical Message Format

Every claim requires a cryptographic signature over a canonical message. The program reconstructs this message entirely from on-chain state -- signers never supply message bytes. This section specifies the exact byte format so that wallet integrators can produce valid signatures.

### 2.1 Format

The canonical message is a UTF-8 encoded string with five lines separated by line-feed characters (`0x0A`). There is **no trailing newline**.

```
ar.io ant-escrow claim v1
network: <network>
ant: <ant_mint_base58>
claimant: <claimant_solana_pubkey_base58>
nonce: <nonce_hex_lowercase>
```

### 2.2 Field Specification

| Field | Format | Byte source | Notes |
|---|---|---|---|
| Line 1 (header) | Literal `ar.io ant-escrow claim v1` | Constant | Exactly 25 ASCII bytes. Never changes within a program version. |
| `network` | `solana-mainnet` or `solana-devnet` | Compiled into program binary | Two separate program deployments exist, one per network. Prevents cross-network replay. |
| `ant` | Base58 encoding of the ANT mint pubkey | `escrow.ant_mint` | 32-44 ASCII characters. Standard Solana base58 (Bitcoin alphabet). |
| `claimant` | Base58 encoding of the Solana pubkey that will receive the ANT | Transaction account | 32-44 ASCII characters. Bound into the signature to prevent front-running. |
| `nonce` | Lowercase hex encoding of the 32-byte nonce, no `0x` prefix | `escrow.nonce` | Exactly 64 ASCII characters (`[0-9a-f]`). |

### 2.3 Encoding Rules

- **Character encoding:** UTF-8 throughout. All fields are ASCII-safe (base58 + hex), so UTF-8 and ASCII produce identical bytes.
- **Line separator:** Single `\n` (0x0A) between each line.
- **No trailing newline:** The message ends immediately after the last hex character of the nonce.
- **No leading whitespace:** Each line starts with its content -- `network:`, `ant:`, etc. There is one space after each colon.
- **Base58 alphabet:** The Bitcoin/Solana base58 alphabet: `123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz` (no `0`, `O`, `I`, `l`).

### 2.4 Worked Example

Given:
- ANT mint: `9PnRFwk2Yp7QyU3sQzXwUhJj6tVyM4nN2KqL5fT8RbAW`
- Claimant: `Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A`
- Nonce (hex): `a3f1c8d92e0b4f7a8e1d6c5b4a3920817f6e5d4c3b2a19188776655443322110`
- Network: `solana-mainnet`

The canonical message bytes (displayed as a string) are:

```
ar.io ant-escrow claim v1
network: solana-mainnet
ant: 9PnRFwk2Yp7QyU3sQzXwUhJj6tVyM4nN2KqL5fT8RbAW
claimant: Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A
nonce: a3f1c8d92e0b4f7a8e1d6c5b4a3920817f6e5d4c3b2a19188776655443322110
```

This is approximately 210-250 bytes depending on base58 pubkey lengths (each pubkey encodes to 32-44 characters).

### 2.5 Construction Pseudocode

```
function buildCanonicalMessage(network, antMint, claimant, nonce):
    header  = "ar.io ant-escrow claim v1"
    antB58  = base58encode(antMint)           // 32 bytes -> 32-44 chars
    cB58    = base58encode(claimant)           // 32 bytes -> 32-44 chars
    nonceHx = lowercaseHex(nonce)             // 32 bytes -> 64 chars

    return utf8encode(
        header + "\n" +
        "network: " + network + "\n" +
        "ant: " + antB58 + "\n" +
        "claimant: " + cB58 + "\n" +
        "nonce: " + nonceHx
    )
```

The TypeScript SDK exports this as `canonicalMessage()` from `@ar.io/sdk/solana`. The Rust implementation lives in `contracts/programs/ario-ant-escrow/src/canonical.rs::build_canonical_message`. Both are pinned to produce byte-identical output via cross-language tests.

---

## 3. Signing Conventions

### 3.1 Arweave (RSA-PSS-4096, off-chain attestor)

The Arweave recipient signs the raw canonical message bytes directly with their wallet. No prefix or wrapper is applied.

**Parameters (wallet-side):**

| Parameter | Value |
|---|---|
| Algorithm | RSASSA-PSS (RFC 8017) |
| Hash | SHA-256 |
| MGF | MGF1-SHA-256 |
| Modulus size | 4096 bits (512 bytes) |
| Public exponent | 65537 (fixed; not configurable) |
| Salt length | 32 bytes (wallet default; values 0-32 accepted) |
| Signature size | 512 bytes |

**Signing flow:**

```
canonical_bytes = buildAntEscrowClaimMessage(network, antMint, claimant, nonce, recipient_pubkey)
rsa_signature = wallet.signMessage(canonical_bytes)
// rsa_signature is 512 bytes
```

Standard Arweave wallets (Wander, ArConnect) expose `wallet.signMessage(bytes)` which performs this operation. The `SubtleCrypto` browser API can also produce compatible signatures via `crypto.subtle.sign("RSA-PSS", key, data)` with the parameters above. Note: the canonical message includes a `recipient: <43-char base64url(sha256(recipient_pubkey))>` line that binds the signature to the deposit-time recipient identity (closes F-1; see `docs/ATTESTOR_SECURITY_REVIEW.md`).

**Off-chain attestor flow:**

```
1. Browser POSTs (canonical_bytes, rsa_signature, rsa_modulus) to the attestor.
2. Attestor verifies the RSA-PSS signature off-chain (`node:crypto`,
   RSA_PKCS1_PSS_PADDING, SHA-256, salt 32) using the client-supplied modulus.
3. Attestor reconstructs the canonical message from its own builder, with
   `recipient` derived from sha256(client_supplied_modulus). Diverges from
   the on-chain canonical iff the modulus is wrong → on-chain rejects.
4. Attestor signs the canonical bytes with its Ed25519 secret seed.
5. Returns (attestor_pubkey_b58, ed25519_signature_b64url, canonical_b64url).
6. Browser builds and submits a Solana tx with two instructions:
     [0] Ed25519Program native sigverify ix carrying
         (attestor_pubkey, ed25519_signature, canonical_bytes)
     [1] claim_*_arweave_attested ix carrying just (message_nonce: 32 bytes)
```

**On-chain verification (`programs/ario-ant-escrow/src/verify/attested.rs`):**

```
1. Read sysvar::instructions to get the ix at current_ix_index - 1.
2. Require it is the Ed25519Program (Ed25519SigVerify111…) ix.
3. Parse the ix data layout (header + offsets + inline pubkey/sig/message).
4. Require all three *_ix_index fields == 0xFFFF (DATA_IN_SAME_IX) so the
   Ed25519Program verified against bytes inside its own ix data, not
   somewhere else in the tx.
5. Require pubkey == ATTESTOR_PUBKEY (compiled-in constant).
6. Reconstruct expected_canonical from on-chain escrow state, including
   the recipient: <43-char base64url(sha256(escrow.recipient_pubkey))> line.
7. Require message bytes == expected_canonical.
```

The Solana runtime already cryptographically verified the Ed25519 signature when it executed the Ed25519Program ix; on-chain we only confirm WHAT was verified. Total cost: ~75-80K CU per claim (vs 200K+ for the equivalent on-chain RSA-PSS, which was removed because `sol_big_mod_exp` is feature-gated and disabled on every public Solana cluster).

See `docs/DECISIONS.md` ADR-017 for the architecture rationale and [`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor)`/README.md` for the off-chain service contract.

### 3.2 Ethereum (ECDSA secp256k1 + EIP-191)

Ethereum wallets apply the EIP-191 `personal_sign` prefix before signing. The on-chain verifier re-applies the same prefix, so signers pass the raw canonical bytes to their wallet's `signMessage` API.

**Signing flow:**

```
canonical_bytes = buildCanonicalMessage(network, antMint, claimant, nonce)
signature = wallet.signMessage(canonical_bytes)
// wallet internally computes: keccak256("\x19Ethereum Signed Message:\n" + len(canonical_bytes) + canonical_bytes)
// signature is 65 bytes: r (32) || s (32) || v (1)
```

Standard Ethereum wallets (MetaMask, WalletConnect) and libraries (ethers.js, viem) all produce compatible signatures via their `signMessage` / `personal_sign` APIs.

**On-chain verification algorithm:**

```
1. prefix = "\x19Ethereum Signed Message:\n" + ascii_decimal(len(canonical))
2. msg_hash = keccak256(prefix || canonical)           // 32 bytes
3. v = signature[64]
   if v >= 27: v = v - 27                             // normalize {27,28} -> {0,1}
   require v in {0, 1}                                // reject all other values
4. s = signature[32..64]
   require s <= secp256k1_n / 2                       // EIP-2 low-S enforcement
5. pubkey = secp256k1_recover(msg_hash, v, signature[0..64])   // Solana syscall
6. address = keccak256(pubkey)[12..32]                 // 20 bytes
7. require address == escrow.recipient_pubkey[0..20]   // XOR-accumulated comparison
```

**EIP-191 prefix detail:** The length suffix is the ASCII decimal representation of the canonical message byte length. For a 210-byte message, the prefix is the byte string `\x19Ethereum Signed Message:\n210`. This matches what every standard EVM wallet produces.

**Recovery id (`v`):** Accepted values are `0`, `1` (modern) and `27`, `28` (legacy). All other values are rejected with `InvalidRecoveryId`.

**Low-S enforcement (EIP-2):** The `s` component (bytes 32-63 of the signature, big-endian) must satisfy `s <= 0x7fffffffffffffffffffffffffffffff5d576e7357a4501ddfe92f46681b20a0` (secp256k1 curve order divided by 2). Signatures with high-S are rejected with `EcdsaHighS`.

---

## 4. Account Model

### 4.1 `EscrowAnt` PDA Layout

Total on-chain size: **661 bytes** (8-byte Anchor discriminator + 653-byte payload).

Rent cost: ~0.046 SOL (returned to depositor on close).

| Offset | Size (bytes) | Field | Type | Description |
|--------|-------------|-------|------|-------------|
| 0 | 8 | (discriminator) | `[u8; 8]` | Anchor account discriminator: `sha256("account:EscrowAnt")[0..8]` |
| 8 | 1 | `version` | `u8` | Schema version. Always `1` in v1. |
| 9 | 1 | `bump` | `u8` | Cached PDA bump seed. |
| 10 | 32 | `depositor` | `Pubkey` | Solana wallet that deposited the ANT. Sole authority for `cancel_deposit` and `update_recipient`. Receives rent on close. |
| 42 | 32 | `ant_mint` | `Pubkey` | Metaplex Core asset key of the escrowed ANT. Denormalized from PDA seeds. |
| 74 | 1 | `recipient_protocol` | `u8` | `0` = Arweave RSA-PSS-4096, `1` = Ethereum ECDSA secp256k1. |
| 75 | 2 | `recipient_pubkey_len` | `u16` (LE) | Active byte length of the pubkey blob: `512` (Arweave) or `20` (Ethereum). |
| 77 | 512 | `recipient_pubkey` | `[u8; 512]` | Recipient identity, zero-padded to 512 bytes. Arweave: full RSA modulus (big-endian). Ethereum: 20-byte address followed by 492 zero bytes. |
| 589 | 32 | `nonce` | `[u8; 32]` | Anti-replay nonce. Rotated on every `update_recipient`. |
| 621 | 8 | `deposit_slot` | `u64` (LE) | Slot at deposit time. Informational only. |
| 629 | 32 | `_reserved` | `[u8; 32]` | Reserved for future fields. All zeros. |

### 4.2 Protocol Constants

| Constant | Value |
|---|---|
| `PROTOCOL_ARWEAVE` | `0` |
| `PROTOCOL_ETHEREUM` | `1` |
| `ARWEAVE_PUBKEY_LEN` | `512` |
| `ETHEREUM_PUBKEY_LEN` | `20` |
| `ESCROW_VERSION_V1` | `1` |

---

## 5. Instructions

All instruction discriminators follow the Anchor convention: `sha256("global:<instruction_name>")[0..8]`.

### 5.1 `deposit_ant`

Lock an ANT into escrow targeted at an Arweave or Ethereum identity.

**Discriminator:** `sha256("global:deposit_ant")[0..8]`

**Parameters (Borsh-encoded, appended after discriminator):**

| Field | Type | Size | Description |
|---|---|---|---|
| `recipient_protocol` | `u8` | 1 | `0` = Arweave, `1` = Ethereum |
| `recipient_pubkey` | `Vec<u8>` | 4 + len | Borsh Vec: 4-byte LE length prefix + raw bytes. 512 bytes for Arweave (RSA modulus), 20 bytes for Ethereum (address). |

**Accounts (ordered):**

| # | Name | Writable | Signer | Description |
|---|---|---|---|---|
| 0 | `escrow` | Yes | No | PDA to initialize. Seeds: `["escrow_ant", ant_asset.key()]`. |
| 1 | `ant_asset` | Yes | No | Metaplex Core asset (ANT). Must be owned by `mpl-core` program. |
| 2 | `depositor` | Yes | Yes | Current ANT owner. Pays rent + tx fee. |
| 3 | `mpl_core_program` | No | No | `CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d` |
| 4 | `system_program` | No | No | `11111111111111111111111111111111` |

**Effects:**
- Initializes the `EscrowAnt` PDA (fails if PDA already exists -- no double deposit).
- CPIs to mpl-core `TransferV1` to move ANT ownership from depositor to escrow PDA.
- Computes initial nonce: `sha256(deposit_slot.to_le_bytes() || ant_mint || depositor)`.

**Errors:**
- `InvalidRecipientProtocol` -- protocol byte is not 0 or 1.
- `InvalidRecipientPubkeyLength` -- pubkey length doesn't match protocol (512 for Arweave, 20 for Ethereum).
- `NotAntOwner` -- depositor is not the current Metaplex Core asset owner.
- `InvalidAsset` -- account is not a valid Metaplex Core AssetV1.

### 5.2 `claim_ant_arweave`

Release the ANT to a claimant after verifying an Arweave RSA-PSS-4096 signature.

**Discriminator:** `sha256("global:claim_ant_arweave")[0..8]`

**Parameters (Borsh-encoded):**

| Field | Type | Size | Description |
|---|---|---|---|
| `message_nonce` | `[u8; 32]` | 32 | Must equal `escrow.nonce`. Provides a clear `NonceMismatch` error for stale signatures. |
| `signature` | `[u8; 512]` | 512 | RSA-PSS-SHA256 signature over the canonical message. |
| `salt_len` | `u8` | 1 | PSS salt length used when signing. Typically `32`. Range: `0..=32`. |

**Accounts (ordered):**

| # | Name | Writable | Signer | Description |
|---|---|---|---|---|
| 0 | `escrow` | Yes | No | Escrow PDA. Closed on success (rent returned to depositor). |
| 1 | `ant_asset` | Yes | No | Metaplex Core asset. Must equal `escrow.ant_mint`. |
| 2 | `claimant` | No | No | Solana pubkey that receives the ANT. Bound into the canonical message. |
| 3 | `depositor` | Yes | No | Original depositor. Receives rent on escrow close. Must match `escrow.depositor`. |
| 4 | `payer` | Yes | Yes | Transaction fee payer. Can be anyone -- does not have to be the claimant. |
| 5 | `mpl_core_program` | No | No | `CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d` |
| 6 | `system_program` | No | No | `11111111111111111111111111111111` |

**Effects:**
- Verifies `escrow.recipient_protocol == 0` (Arweave).
- Verifies `message_nonce == escrow.nonce`.
- Reconstructs canonical message from on-chain state and verifies RSA-PSS signature.
- CPIs to mpl-core `TransferV1` to move ANT from escrow PDA to claimant.
- Closes escrow PDA, returning rent to depositor.

**Errors:**
- `ProtocolMismatch` -- escrow is not targeting Arweave.
- `NonceMismatch` -- supplied nonce doesn't match escrow's current nonce.
- `SignatureVerificationFailed` -- RSA-PSS verification rejected the signature.
- `InvalidSaltLength` -- `salt_len` exceeds 32.
- `AntMintMismatch` -- `ant_asset` key doesn't match `escrow.ant_mint`.
- `InvalidAsset` -- `ant_asset` is not a valid Metaplex Core asset.

> **Production note:** `claim_ant_arweave` requires the `sol_big_mod_exp`
> syscall, which is feature-gated and currently blocked on every public
> Solana cluster. Until the gate is activated, production Arweave
> claims must use `claim_ant_arweave_attested` (§ 5.2a) instead. This
> instruction remains in the program as a reference and as a fallback
> for the day the syscall is enabled. See ADR-017 for full context.

### 5.2a `claim_ant_arweave_attested`

Release the ANT to a claimant using an off-chain RSA-PSS attestation
re-signed with Ed25519 by the AR.IO attestor service. The transaction
must include an Ed25519Program native sigverify ix immediately
preceding this one. See ADR-017 for the full design rationale.

**Discriminator:** `sha256("global:claim_ant_arweave_attested")[0..8]`

**Parameters (Borsh-encoded):**

| Field | Type | Size | Description |
|---|---|---|---|
| `message_nonce` | `[u8; 32]` | 32 | Must equal `escrow.nonce`. |

The signature itself is not in this instruction's data — it lives in
the preceding Ed25519Program ix's data buffer, alongside the signed
canonical message bytes and the attestor's pubkey.

**Accounts (ordered):**

| # | Name | Writable | Signer | Description |
|---|---|---|---|---|
| 0 | `escrow` | Yes | No | Escrow PDA. Closed on success. |
| 1 | `ant_asset` | Yes | No | Metaplex Core asset. Must equal `escrow.ant_mint`. |
| 2 | `claimant` | No | No | Solana pubkey that receives the ANT. Bound into canonical message. |
| 3 | `depositor` | Yes | No | Original depositor. Receives rent on close. |
| 4 | `payer` | Yes | Yes | Transaction fee payer. |
| 5 | `mpl_core_program` | No | No | `CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d` |
| 6 | `instructions_sysvar` | No | No | `Sysvar1nstructions1111111111111111111111111` -- required for introspection. |
| 7 | `system_program` | No | No | `11111111111111111111111111111111` |

**Required preceding instruction:**

The transaction MUST include a Solana Ed25519Program native sigverify
ix at `current_ix_index - 1` (i.e., immediately before this one).
The Ed25519Program ix must:

- Use program ID `Ed25519SigVerify111111111111111111111111111`.
- Contain exactly one signature (`num_signatures == 1`).
- Have all three offset references (signature, pubkey, message)
  pointing into the ix's own data (`*_ix_index == 0xFFFF`).
- Verify a 64-byte Ed25519 signature by `ATTESTOR_PUBKEY` (compiled-in
  constant in `state.rs`) over the canonical claim message bytes
  reconstructed from this escrow's state.

**Effects:**
- Verifies `escrow.recipient_protocol == 0` (Arweave).
- Verifies `message_nonce == escrow.nonce`.
- Loads the preceding ix via `sysvar::instructions` introspection.
- Confirms it is an Ed25519Program ix verifying a signature by
  `ATTESTOR_PUBKEY` over the canonical message reconstructed from
  escrow state.
- CPIs to mpl-core `TransferV1` and `UpdateV1` (for UpdateAuthority
  rotation to claimant).
- Closes escrow PDA, returning rent to depositor.

**Errors:**
- `ProtocolMismatch`, `NonceMismatch`, `AntMintMismatch`, `InvalidAsset`
  (same as § 5.2).
- `MissingAttestationInstruction` -- no Ed25519Program ix at
  `current_ix_index - 1`, or sysvar lookup failed.
- `MalformedAttestationInstruction` -- ix data too short, wrong
  `num_signatures`, or uses cross-instruction data references.
- `AttestationSignerMismatch` -- the Ed25519Program ix verifies a
  signature by a key other than `ATTESTOR_PUBKEY`.
- `AttestationMessageMismatch` -- the bytes signed do not equal the
  canonical message reconstructed from escrow state.

**CU cost:** ~77K end-to-end (including MPL Core CPIs).

### 5.3 `claim_ant_ethereum`

Release the ANT to a claimant after verifying an Ethereum ECDSA + EIP-191 signature.

**Discriminator:** `sha256("global:claim_ant_ethereum")[0..8]`

**Parameters (Borsh-encoded):**

| Field | Type | Size | Description |
|---|---|---|---|
| `message_nonce` | `[u8; 32]` | 32 | Must equal `escrow.nonce`. |
| `signature` | `[u8; 65]` | 65 | ECDSA signature: `r` (32) \|\| `s` (32) \|\| `v` (1, recovery id). |

**Accounts (ordered):**

Same as `claim_ant_arweave` (7 accounts in the same order).

**Effects:**
- Verifies `escrow.recipient_protocol == 1` (Ethereum).
- Verifies `message_nonce == escrow.nonce`.
- Reconstructs canonical message, applies EIP-191 prefix, verifies ECDSA signature.
- Enforces low-S (EIP-2).
- Recovers the signing pubkey and derives the Ethereum address; compares against `escrow.recipient_pubkey[0..20]`.
- CPIs to mpl-core `TransferV1` to move ANT from escrow PDA to claimant.
- Closes escrow PDA, returning rent to depositor.

**Errors:**
- `ProtocolMismatch` -- escrow is not targeting Ethereum.
- `NonceMismatch` -- nonce mismatch.
- `SignatureVerificationFailed` -- ECDSA recovery or verification failed.
- `EcdsaHighS` -- signature `s` component exceeds `secp256k1_n / 2`.
- `InvalidRecoveryId` -- `v` byte is not in `{0, 1, 27, 28}`.
- `EthereumAddressMismatch` -- recovered address doesn't match stored recipient.
- `AntMintMismatch`, `InvalidAsset` -- account validation failures.

### 5.4 `update_recipient`

Change the recipient identity of an active escrow. Only callable by the depositor.

**Discriminator:** `sha256("global:update_recipient")[0..8]`

**Parameters (Borsh-encoded):**

| Field | Type | Size | Description |
|---|---|---|---|
| `new_protocol` | `u8` | 1 | `0` = Arweave, `1` = Ethereum |
| `new_pubkey` | `Vec<u8>` | 4 + len | Borsh Vec with the new recipient pubkey. Same length rules as `deposit_ant`. |

**Accounts (ordered):**

| # | Name | Writable | Signer | Description |
|---|---|---|---|---|
| 0 | `escrow` | Yes | No | Escrow PDA being updated. |
| 1 | `depositor` | No | Yes | Must match `escrow.depositor`. |

**Effects:**
- Updates `recipient_protocol`, `recipient_pubkey`, `recipient_pubkey_len`.
- Rotates nonce: `sha256(current_slot.to_le_bytes() || ant_mint || depositor || old_nonce)`. All in-flight claim signatures bound to the old nonce become invalid.

**Errors:**
- `NotDepositor` -- signer is not the original depositor.
- `InvalidRecipientProtocol`, `InvalidRecipientPubkeyLength` -- invalid parameters.

### 5.5 `cancel_deposit`

Return the ANT to the depositor and close the escrow. Only callable by the depositor.

**Discriminator:** `sha256("global:cancel_deposit")[0..8]`

**Parameters:** None (instruction data is the 8-byte discriminator only).

**Accounts (ordered):**

| # | Name | Writable | Signer | Description |
|---|---|---|---|---|
| 0 | `escrow` | Yes | No | Escrow PDA to close. |
| 1 | `ant_asset` | Yes | No | Metaplex Core asset. Must equal `escrow.ant_mint`. |
| 2 | `depositor` | Yes | Yes | Must match `escrow.depositor`. Receives ANT + rent. |
| 3 | `mpl_core_program` | No | No | `CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d` |
| 4 | `system_program` | No | No | `11111111111111111111111111111111` |

**Effects:**
- CPIs to mpl-core `TransferV1` to return ANT from escrow PDA to depositor.
- Closes escrow PDA, returning rent to depositor.

**Errors:**
- `NotDepositor` -- signer is not the original depositor.
- `AntMintMismatch`, `InvalidAsset` -- account validation failures.

---

## 6. PDA Derivation

Each ANT can have at most one escrow, enforced by the deterministic PDA derivation.

**Seeds:** `["escrow_ant", <ant_mint_pubkey_bytes>]`

- Seed 0: The ASCII bytes of `escrow_ant` (10 bytes).
- Seed 1: The 32-byte public key of the Metaplex Core ANT asset.

**Program ID:** The `ario-ant-escrow` program ID. This differs between mainnet and devnet deployments.

**Derivation:**

```
(pda, bump) = findProgramDerivedAddress(
    seeds: [b"escrow_ant", antMint.toBytes()],
    programId: ARIO_ANT_ESCROW_PROGRAM_ID
)
```

In `@solana/kit`:

```typescript
import { getProgramDerivedAddress, getAddressEncoder } from '@solana/kit';

const encoder = getAddressEncoder();
const [pda, bump] = await getProgramDerivedAddress({
    programAddress: ARIO_ANT_ESCROW_PROGRAM_ID,
    seeds: [
        new TextEncoder().encode("escrow_ant"),
        encoder.encode(antMint),
    ],
});
```

The bump is cached in `escrow.bump` to avoid recomputation in CPI signer-seed reconstruction.

---

## 7. Replay Protection

Three independent layers prevent signature reuse:

### Layer 1: Claimant binding

The claimant's Solana pubkey is included in the canonical message that gets signed. The on-chain verifier reads the claimant from the transaction's account list and reconstructs the message using that pubkey. A valid signature for "release ANT to wallet X" cannot be reused to release the ANT to wallet Y -- the verifier would reconstruct a different message and the signature would fail.

### Layer 2: Nonce binding

Each escrow has a unique 32-byte nonce stored on-chain. This nonce is included in the canonical message (as 64-character lowercase hex). The nonce rotates on every `update_recipient` call (mixed with the old nonce, current slot, and deterministic account data via SHA-256). A signature produced against nonce N is invalid after the nonce rotates.

**Initial nonce:** `sha256(deposit_slot.to_le_bytes() || ant_mint || depositor)`

**Rotated nonce:** `sha256(current_slot.to_le_bytes() || ant_mint || depositor || old_nonce)`

### Layer 3: Escrow closure

On successful claim, the escrow PDA is closed (account data zeroed, lamports returned to depositor). Even if an identical signature is submitted a second time, the transaction fails because the escrow account no longer exists.

### Cross-network replay prevention

The `network` field in the canonical message is compiled into the program binary at build time (`solana-mainnet` or `solana-devnet`). A signature valid on devnet is invalid on mainnet because the programs reconstruct different canonical messages.

### Cross-protocol replay prevention

Arweave signatures are 512 bytes; Ethereum signatures are 65 bytes. Structurally incompatible. Additionally, the `recipient_protocol` field is checked before verification begins -- calling `claim_ant_arweave` on an Ethereum escrow (or vice versa) returns `ProtocolMismatch`.

---

## 8. SDK Usage

The TypeScript SDK provides the `ANTEscrow` client and the `canonicalMessage` helper. Both are exported from `@ar.io/sdk/solana`.

### 8.1 Initialize

```typescript
import { ANTEscrow, canonicalMessage } from '@ar.io/sdk/solana';
import { createSolanaRpc, createSolanaRpcSubscriptions } from '@solana/kit';

const rpc = createSolanaRpc('https://api.mainnet-beta.solana.com');
const rpcSubscriptions = createSolanaRpcSubscriptions('wss://api.mainnet-beta.solana.com');

// Read-only (no signer needed)
const reader = ANTEscrow.init({ rpc });

// Read + write
const writer = ANTEscrow.init({ rpc, rpcSubscriptions, signer });
```

### 8.2 Deposit an ANT

```typescript
// Arweave recipient (512-byte RSA modulus from JWK `n` field, base64url-decoded)
const txSig = await writer.deposit({
    antMint: 'BdJ8...', // ANT asset address
    recipient: {
        protocol: 'arweave',
        publicKey: rsaModulusBytes, // Uint8Array, 512 bytes
    },
});

// Ethereum recipient (20-byte address)
const txSig = await writer.deposit({
    antMint: 'BdJ8...',
    recipient: {
        protocol: 'ethereum',
        publicKey: ethAddressBytes, // Uint8Array, 20 bytes
    },
});
```

### 8.3 Read Escrow State

```typescript
const state = await reader.get(antMint);
if (state === null) {
    console.log('No active escrow for this ANT');
} else {
    console.log('Depositor:', state.depositor);
    console.log('Protocol:', state.recipientProtocol); // 'arweave' | 'ethereum'
    console.log('Nonce:', state.nonce); // Uint8Array, 32 bytes
    console.log('Deposit slot:', state.depositSlot); // bigint
}
```

### 8.4 Build Canonical Message and Sign

```typescript
import { canonicalMessage } from '@ar.io/sdk/solana';

// 1. Build the message (deterministic, must match on-chain reconstruction)
const message = canonicalMessage({
    network: 'solana-mainnet',
    antMint,
    claimant: claimantSolanaPubkey,
    nonce: state.nonce,
});

// 2. Sign with the appropriate wallet
// Arweave: raw bytes signed directly
const arweaveSig = await arweaveWallet.signMessage(message);

// Ethereum: wallet applies EIP-191 prefix internally
const ethSig = await ethereumWallet.signMessage(message);
```

### 8.5 Submit a Claim

```typescript
// Arweave claim
const txSig = await writer.claimArweave({
    antMint,
    claimant: claimantSolanaPubkey,
    signature: arweaveSig,    // Uint8Array, 512 bytes
    saltLen: 32,              // optional, defaults to 32
});

// Ethereum claim
const txSig = await writer.claimEthereum({
    antMint,
    claimant: claimantSolanaPubkey,
    signature: ethSig,        // Uint8Array, 65 bytes (r||s||v)
});
```

The `claimant` does not need to be the transaction fee payer. Anyone can submit a valid claim -- only the named claimant receives the ANT.

### 8.6 Depositor Management

```typescript
// Change recipient identity (rotates nonce, invalidates old signatures)
await writer.updateRecipient({
    antMint,
    newRecipient: {
        protocol: 'ethereum',
        publicKey: newEthAddressBytes,
    },
});

// Cancel and recover ANT + rent
await writer.cancel({ antMint });
```

### 8.7 PDA Lookup (No RPC)

```typescript
const pdaAddress = await reader.getPda(antMint);
```

---

## 9. Error Reference

Error codes follow the Anchor convention: `ERROR_CODE_OFFSET + variant_index`. The offset for custom Anchor errors is `6000`.

| Code | Name | Message |
|------|------|---------|
| 6000 | `InvalidRecipientProtocol` | Invalid recipient protocol (must be 0=Arweave or 1=Ethereum) |
| 6001 | `InvalidRecipientPubkeyLength` | Invalid recipient pubkey length for the given protocol |
| 6002 | `NotDepositor` | Unauthorized: only the original depositor can perform this action |
| 6003 | `InvalidAsset` | Invalid Metaplex Core asset (wrong owner program or asset discriminator) |
| 6004 | `NotAntOwner` | Unauthorized: caller is not the current ANT owner |
| 6005 | `AntMintMismatch` | Escrow PDA does not correspond to the supplied ANT mint |
| 6006 | `NonceMismatch` | Claim signature nonce does not match the escrow's current nonce |
| 6007 | `ProtocolMismatch` | Wrong claim path: this escrow targets a different protocol |
| 6008 | `SignatureVerificationFailed` | Signature verification failed |
| 6009 | `InvalidSaltLength` | Invalid PSS salt length (must be <= 32 bytes) |
| 6010 | `EthereumAddressMismatch` | Recovered Ethereum address does not match recipient |
| 6011 | `EcdsaHighS` | ECDSA signature is malleable (high-S form rejected per EIP-2) |
| 6012 | `InvalidRecoveryId` | Invalid ECDSA recovery id |

Error variant ordering is stable. These codes must not be reordered without a major version bump.

Note: `SignatureVerificationFailed` (6008) is intentionally opaque. It does not distinguish between a tampered message, wrong key, malformed signature, or any other cryptographic failure. This is by design -- callers should not need to differentiate these cases.

---

## 10. Security Properties

### What is protected

| Property | Mechanism |
|---|---|
| Only the designated recipient's key can release the ANT | Signature verification (RSA-PSS or ECDSA) against the stored pubkey |
| ANT goes to the correct Solana wallet | Claimant pubkey bound into the signed canonical message; on-chain reconstruction from tx accounts |
| Front-running cannot redirect the ANT | Claimant binding. A bot can resubmit the tx but only pays the fee for the claimant. |
| Signatures cannot be replayed | Three layers: claimant binding, nonce binding, escrow PDA closure |
| Cross-network replay | Network string compiled into program binary |
| Cross-protocol replay | Different signature sizes (512 vs 65) + explicit protocol check |
| PKCS#1 v1.5 downgrade | Strict `0xBC` trailer check in PSS verification |
| ECDSA malleability | Low-S enforcement per EIP-2 |
| Depositor retains control until claim | `update_recipient` rotates nonce (invalidates in-flight sigs); `cancel_deposit` recovers ANT immediately |

### What is NOT protected

| Non-goal | Rationale |
|---|---|
| Compromised recipient key | The key holder is the legitimate owner of the claim right |
| Depositor griefing via repeated `update_recipient` | By design -- depositor controls the escrow. Recipients should claim promptly. |
| ANT abandonment (both parties lose keys) | ~$0.67 in rent locked forever. Acceptable cost for simple semantics. |
| Key custody / social engineering | Infrastructure concern, not protocol-level |

### Compute Budget

| Instruction | Measured CU | Budget |
|---|---|---|
| `deposit_ant` | ~45,000 | 200,000 (default) |
| `cancel_deposit` | ~39,000 | 200,000 (default) |
| `update_recipient` | ~16,000 | 200,000 (default) |
| `claim_ant_arweave` | 80,000-120,000 (estimated) | 200,000 (default) |
| `claim_ant_ethereum` | 60,000-80,000 (estimated) | 200,000 (default) |

All instructions fit within Solana's default 200K CU per-instruction budget. No `SetComputeUnitLimit` prefix instruction is required.

### Transaction Size

The largest transaction is `claim_ant_arweave`:

| Component | Bytes |
|---|---|
| Tx header + 1 signature | ~136 |
| Account metas (7 accounts) | ~250 |
| Instruction data (8 disc + 32 nonce + 512 sig + 1 salt_len) | 553 |
| **Total** | **~939 / 1,232** |

`claim_ant_ethereum` is ~485 bytes total. All transactions fit within Solana's 1,232-byte MTU without Address Lookup Tables.

---

## Appendix A: Constant Reference

| Constant | Value | Source |
|---|---|---|
| Metaplex Core program ID | `CoREENxT6tW1HoK8ypY1SxRMZTcVPm7R94rH4PZNhX7d` | Fixed |
| System program ID | `11111111111111111111111111111111` | Fixed |
| PDA seed prefix | `escrow_ant` (ASCII) | `state.rs::ESCROW_ANT_SEED` |
| RSA public exponent | `65537` (big-endian: `0x01, 0x00, 0x01`) | Hardcoded, not user-configurable |
| PSS trailer byte | `0xBC` | RFC 8017 |
| Max salt length | `32` | Bounds MGF1 computation |
| EIP-191 prefix | `\x19Ethereum Signed Message:\n` | EIP-191 standard |
| secp256k1 curve order (n) | `0xFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFEBAAEDCE6AF48A03BBFD25E8CD0364141` | SEC 2 |
| secp256k1 n/2 | `0x7FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF5D576E7357A4501DDFE92F46681B20A0` | n >> 1 |

## Appendix B: Instruction Data Wire Format

All instruction data begins with an 8-byte discriminator followed by Borsh-encoded parameters.

### `deposit_ant`

```
[0..8]    discriminator: sha256("global:deposit_ant")[0..8]
[8]       recipient_protocol: u8
[9..13]   recipient_pubkey length: u32 LE
[13..]    recipient_pubkey bytes: [u8; length]
```

Total: 8 + 1 + 4 + pubkey_len = 525 bytes (Arweave) or 33 bytes (Ethereum).

### `claim_ant_arweave`

```
[0..8]    discriminator: sha256("global:claim_ant_arweave")[0..8]
[8..40]   message_nonce: [u8; 32]
[40..552] signature: [u8; 512]
[552]     salt_len: u8
```

Total: 553 bytes.

### `claim_ant_ethereum`

```
[0..8]    discriminator: sha256("global:claim_ant_ethereum")[0..8]
[8..40]   message_nonce: [u8; 32]
[40..105] signature: [u8; 65]
```

Total: 105 bytes.

### `update_recipient`

Same layout as `deposit_ant` parameters (after a different discriminator):

```
[0..8]    discriminator: sha256("global:update_recipient")[0..8]
[8]       new_protocol: u8
[9..13]   new_pubkey length: u32 LE
[13..]    new_pubkey bytes: [u8; length]
```

### `cancel_deposit`

```
[0..8]    discriminator: sha256("global:cancel_deposit")[0..8]
```

Total: 8 bytes (no parameters).

---

## 11. Token and Vault Escrow (v2)

The escrow program generalizes beyond ANTs to support two additional asset types: **ARIO tokens** (liquid SPL balances) and **vaults** (time-locked token positions from `ario-core`). The v1 ANT escrow instructions and canonical message format are unchanged. Token and vault escrows use a v2 canonical message and separate PDA namespaces.

### 11.1 Canonical Message v2 Format

Token and vault claims sign over a v2 canonical message. The format is similar to v1 but replaces the `ant` field with `type`, `asset`, and `amount` fields.

```
ar.io escrow claim v2
network: <solana-mainnet|solana-devnet>
type: <token|vault>
asset: <asset_id_hex_lowercase_64chars>
amount: <mARIO_decimal>
claimant: <claimant_solana_pubkey_base58>
nonce: <nonce_hex_lowercase_64chars>
```

| Field | Format | Description |
|---|---|---|
| Line 1 (header) | Literal `ar.io escrow claim v2` | Version 2 header. Exactly 22 ASCII bytes. |
| `network` | `solana-mainnet` or `solana-devnet` | Compiled into program binary. Prevents cross-network replay. |
| `type` | `token` or `vault` | Asset type being escrowed. |
| `asset` | Lowercase hex, 64 chars, no `0x` prefix | The 32-byte asset identifier. For tokens: the SPL mint pubkey. For vaults: the vault PDA pubkey. |
| `amount` | ASCII decimal of mARIO (u64) | The token amount in micro-ARIO (e.g., `1000000` = 1 ARIO). No separators, no decimals. |
| `claimant` | Base58 Solana pubkey | The Solana wallet that receives the tokens/vault. |
| `nonce` | Lowercase hex, 64 chars | Anti-replay nonce from the escrow account. |

The v1 message (`ar.io ant-escrow claim v1`) remains the canonical format for ANT escrows. The header prefix difference prevents cross-version replay.

### 11.2 `EscrowToken` Account Model (711 bytes)

Total on-chain size: **711 bytes** (8-byte Anchor discriminator + 703-byte payload).

Per-escrow rent: ~0.0079 SOL (includes the associated token account for holding SPL tokens). Returned to depositor on close.

| Offset | Size (bytes) | Field | Type | Description |
|--------|-------------|-------|------|-------------|
| 0 | 8 | (discriminator) | `[u8; 8]` | Anchor account discriminator |
| 8 | 1 | `version` | `u8` | Schema version. `1` for v2 token/vault escrows. |
| 9 | 1 | `bump` | `u8` | Cached PDA bump seed. |
| 10 | 1 | `asset_type` | `u8` | `0` = token, `1` = vault. |
| 11 | 32 | `depositor` | `Pubkey` | Solana wallet that deposited. Sole authority for cancel/update. Receives rent on close. |
| 43 | 32 | `asset_id` | `[u8; 32]` | Asset identifier (SPL mint pubkey for tokens, vault PDA pubkey for vaults). |
| 75 | 8 | `amount` | `u64` (LE) | Token amount in mARIO. |
| 83 | 1 | `recipient_protocol` | `u8` | `0` = Arweave RSA-PSS-4096, `1` = Ethereum ECDSA secp256k1. |
| 84 | 2 | `recipient_pubkey_len` | `u16` (LE) | Active byte length: `512` (Arweave) or `20` (Ethereum). |
| 86 | 512 | `recipient_pubkey` | `[u8; 512]` | Recipient identity, zero-padded to 512 bytes. |
| 598 | 32 | `nonce` | `[u8; 32]` | Anti-replay nonce. Rotated on every `update_*_recipient`. |
| 630 | 8 | `deposit_slot` | `u64` (LE) | Slot at deposit time. Informational. |
| 638 | 8 | `end_timestamp` | `u64` (LE) | Vault lock expiry (Unix seconds). `0` for token escrows. |
| 646 | 8 | `start_timestamp` | `u64` (LE) | Vault lock start (Unix seconds). `0` for token escrows. |
| 654 | 1 | `vault_id` | `u8` | Vault index within the user's vault set. `0` for token escrows. |
| 655 | 16 | `_reserved` | `[u8; 16]` | Reserved for future fields. All zeros. |

### 11.3 Token/Vault Instructions

Ten instructions handle token and vault escrows. All use the same Arweave RSA-PSS and Ethereum ECDSA verification as the ANT path.

#### `deposit_tokens`

Lock liquid ARIO tokens into escrow targeted at an Arweave or Ethereum identity. Transfers SPL tokens from the depositor's token account to an escrow-owned token account. Initializes the `EscrowToken` PDA with `asset_type = 0`.

#### `deposit_vault`

Lock a time-locked vault into escrow. Reads vault metadata (amount, start/end timestamps) from the `ario-core` vault account and stores it in the `EscrowToken` PDA with `asset_type = 1`. The vault itself remains in `ario-core` custody; the escrow PDA is recorded as the authorized claimant.

#### `claim_tokens_arweave`

Release escrowed tokens to a claimant after verifying an Arweave RSA-PSS-4096 signature over the v2 canonical message. Transfers SPL tokens from the escrow token account to the claimant. Closes the escrow PDA and returns rent to depositor. Requires `SetComputeUnitLimit(400_000)`.

#### `claim_tokens_ethereum`

Same as `claim_tokens_arweave` but with Ethereum ECDSA + EIP-191 verification. Fits within the 200K default CU budget.

#### `claim_vault_arweave`

Release an escrowed vault to a claimant after Arweave RSA-PSS verification. Behavior depends on vault expiry:
- **Active vault** (`end_timestamp > now`): The escrow transfers tokens to the payer's ATA, then verifies via `sysvar::instructions` introspection that a matching `ario_core::vaulted_transfer` instruction exists in the same transaction. The `vaulted_transfer` creates a new time-locked vault for the claimant with the remaining lock duration. Verification happens BEFORE the token transfer (defense-in-depth). The introspection loop checks up to 20 instructions and allows a 60-second tolerance on lock duration to account for clock drift.
- **Expired vault** (`end_timestamp <= now`): Performs a liquid SPL transfer directly to the claimant (same as `claim_tokens_arweave`). No `vaulted_transfer` needed.

Requires `SetComputeUnitLimit(400_000)`.

#### `claim_vault_ethereum`

Same as `claim_vault_arweave` but with Ethereum ECDSA + EIP-191 verification. Active vaults use the same instruction introspection pattern to enforce a sibling `vaulted_transfer`; expired vaults do a liquid transfer.

#### `cancel_token_deposit`

Return escrowed tokens to the depositor and close the escrow. Only callable by the depositor.

#### `cancel_vault_deposit`

Cancel a vault escrow and restore the depositor's claim on the vault. Only callable by the depositor.

#### `update_token_recipient`

Change the recipient identity of an active token escrow. Rotates the nonce, invalidating in-flight signatures. Only callable by the depositor.

#### `update_vault_recipient`

Change the recipient identity of an active vault escrow. Rotates the nonce. Only callable by the depositor.

### 11.4 PDA Derivation

Token and vault escrows use separate PDA namespaces to avoid collisions with ANT escrows and with each other.

**Token escrow:** `["escrow_token", depositor, asset_id]`

- Seed 0: ASCII bytes of `escrow_token` (12 bytes).
- Seed 1: 32-byte depositor pubkey.
- Seed 2: 32-byte asset identifier (SPL mint pubkey).

**Vault escrow:** `["escrow_vault", depositor, asset_id]`

- Seed 0: ASCII bytes of `escrow_vault` (12 bytes).
- Seed 1: 32-byte depositor pubkey.
- Seed 2: 32-byte asset identifier (vault PDA pubkey).

Including the depositor in the PDA seeds (unlike ANT escrows which use only the ANT mint) allows multiple depositors to escrow the same token mint independently.

### 11.5 Vault Claim Behavior

Vault escrows handle two cases depending on the vault's lock status at claim time:

**Active vault** (`end_timestamp > clock.unix_timestamp`):

The escrow uses **transaction instruction introspection** (the same pattern as Solana's Ed25519/secp256k1 precompile programs) to enforce that the claimant receives a time-locked vault, not liquid tokens. The flow:

1. The escrow program reads `sysvar::instructions` and iterates through all instructions in the current transaction (up to a **20-instruction loop limit**).
2. It looks for a matching `ario_core::vaulted_transfer` instruction — verified by program ID, Anchor discriminator, amount, lock duration, and recipient account. The re-lock must be **non-revocable** (a revocable one is rejected with `RevocableVaultUnsupported`; see ADR-021 / BD-105) — otherwise the unbound claim-tx payer would become the revocation controller and could steal the funds before expiry.
3. A **60-second tolerance** is applied to the lock duration to account for clock drift between transaction construction and execution.
4. If no matching instruction is found, the transaction reverts atomically with `MissingVaultedTransferInstruction`.
5. If verification passes, the escrow transfers tokens to the payer's ATA. The sibling `vaulted_transfer` instruction then creates a time-locked vault for the claimant with the remaining lock duration.

Verification happens BEFORE the token transfer (defense-in-depth). This approach avoids CPI into ario-core, which would fail because `system_program` rejects data-carrying PDAs as payers for `create_account`. The claimant receives a vault position, not liquid tokens. This preserves the protocol's staking/locking invariants -- tokens that were locked remain locked for the original duration.

**Expired vault** (`end_timestamp <= clock.unix_timestamp`):

The vault's lock has expired, so the tokens are effectively liquid. The claim instruction performs a standard SPL token transfer directly to the claimant's associated token account. No `vaulted_transfer` instruction is needed in the transaction.

The `end_timestamp` is recorded in the `EscrowToken` account at deposit time and checked at claim time. The canonical message includes the `amount` but not the timestamps -- the on-chain account is the source of truth for lock status.

### 11.6 Rent Cost

| Escrow type | Per-escrow rent | Notes |
|-------------|----------------|-------|
| ANT | ~0.0055 SOL | EscrowAnt PDA only (661 bytes) |
| Token | ~0.0079 SOL | EscrowToken PDA (711 bytes) + associated token account |
| Vault | ~0.0079 SOL | Same as token (vault metadata stored in EscrowToken PDA) |

All rent is refundable -- returned to the depositor when the escrow is closed (via claim or cancel).


---

## 12. Token + Vault Attested Claim Instructions

Mirroring `claim_ant_arweave_attested` (§ 5.2a), the token and vault
escrow programs expose attested-claim equivalents for the Arweave
path. These instructions follow the same architecture: a preceding
Ed25519Program ix verifies the attestor's signature over the canonical
claim message; the claim instruction introspects via
`sysvar::instructions` and confirms `pubkey == ATTESTOR_PUBKEY` and
`message == reconstructed canonical from escrow state`.

### 12.1 `claim_tokens_arweave_attested`

Drop-in replacement for `claim_tokens_arweave` using the attested
path. Same accounts plus `instructions_sysvar`. Args reduce to
`message_nonce: [u8; 32]` only (the signature lives in the preceding
Ed25519Program ix).

**CU cost:** ~48K end-to-end (SPL transfer + close).

### 12.2 `claim_vault_arweave_attested`

Drop-in replacement for `claim_vault_arweave`. Same active/expired
branching logic — for active vaults, the transaction must include a
sibling `ario_core::vaulted_transfer` ix (anywhere in the tx) to
re-lock the released tokens for the claimant. The Ed25519Program ix
must still be at `current_ix_index - 1`; `vaulted_transfer` may be at
any other position.

**CU cost:** ~50K (expired path), ~80K (active path with sibling).

### 12.3 Common errors (all three attested ixs)

In addition to protocol/nonce/asset-type errors per the underlying
non-attested instruction:

- `MissingAttestationInstruction`
- `MalformedAttestationInstruction`
- `AttestationSignerMismatch`
- `AttestationMessageMismatch`

See § 5.2a for the full spec of the introspection contract.

### 12.4 ATTESTOR_PUBKEY constant

`ATTESTOR_PUBKEY` lives in
`contracts/programs/ario-ant-escrow/src/state.rs`. It is compiled
into the program at deploy time and ships with a deterministic test
value (`AKnL4NN...`) derived from the public seed `[1u8; 32]` so
integration tests work without external setup. **This MUST be
replaced before deploying to any cluster that holds real value** —
see [`ar-io/ar-io-solana-attestor`](https://github.com/ar-io/ar-io-solana-attestor)`/README.md` § "Initial deploy" for the runbook
and `contracts/scripts/check-attestor-pubkey.sh --strict` for the
automated guardrail wired into `devnet-deploy.sh`.

Rotation is via `BPFLoaderUpgradeable` upgrade swapping the constant
— see § "Key rotation" in the attestor README.
