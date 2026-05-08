# `ario-ant-escrow` — Trustless ANT Custody Program

**Status:** Implemented and verified (2026-05-06).
**Audience:** Engineers implementing the program, auditors, and integrators building escrow flows.

---

## Overview

A Solana program that holds Metaplex Core ANT NFTs (and ARIO tokens, and time-locked vaults) in custody and releases them to a designated Solana wallet when presented with a valid signature from a pre-specified Arweave (RSA-PSS-4096) or Ethereum (ECDSA secp256k1) key.

**Custody is fully on-chain.** Assets sit in program-owned PDAs, and release decisions are gated by on-chain authorization checks. The program is **indefinite by design** — escrows have no expiry. Permanent ANTs (permabuy ArNS names) live forever; leased ANTs survive their lease expiration as orphaned NFTs that the recipient can still claim. The depositor retains control via `update_recipient` and `cancel_deposit`, providing a recovery path without requiring time-based logic.

**Signature verification splits by protocol:**

- **Ethereum (ECDSA secp256k1 + EIP-191):** verified fully on-chain via the
  `secp256k1_recover` syscall (~25K CU). No external dependencies.
- **Arweave (RSA-PSS-4096 with SHA-256 + MGF1):** verified by an
  AR.IO-operated off-chain attestor service that re-signs the canonical
  claim message with Ed25519. The on-chain program then verifies the
  Ed25519 signature via Solana's native sigverify program (~720 CU)
  using instruction introspection. See ADR-017 for the full rationale —
  on-chain RSA-PSS-4096 modexp exceeds Solana's per-tx CU limit by
  7-13× with every available implementation, and `sol_big_mod_exp` is
  feature-gated and blocked on every public cluster. The earlier
  on-chain `claim_*_arweave` instructions were removed in commit
  4ce73e4 — they referenced the gated syscall, which prevented the BPF
  loader from accepting the .so on devnet/mainnet.

For both protocols, the *canonical claim message* is reconstructed entirely on-chain from escrow state — clients never supply the message bytes themselves, eliminating message-malleability attacks.

---

## Goals

1. **Trustless custody** — ANT release gated only by cryptographic verification on-chain.
2. **Multi-signer support** — Arweave RSA-PSS-4096 and Ethereum ECDSA secp256k1 (covers the dominant identity surfaces in the AR.IO ecosystem and the broader EVM world).
3. **Replay-safe** — signatures bound to specific (escrow, claimant, nonce) tuples; no cross-escrow or cross-claimant reuse.
4. **Indefinite** — no timeout. Escrows persist until claimed, cancelled, or redirected.
5. **Reversible by depositor** — `cancel_deposit` and `update_recipient` give the depositor full control before any claim happens.
6. **MEV-resistant** — claim transactions can't be front-run to redirect funds (the claimant Solana address is bound into the signed message).

## Non-goals (v1)

- Atomic swaps with payment escrow. Future work; can layer on top via a `lock_deposit` instruction that flips an escrow to irrevocable.
- Solana → Solana escrows (Ed25519 path). Trivial to add but not in v1 scope.
- Solana → Arweave reverse direction. Solana ANTs are the canonical form.
- Multi-recipient escrows. v1 is one recipient per deposit; depositor can change it via `update_recipient`.
- Time-locked release. No timeouts of any kind.

---

## Architecture

Single Solana program: `ario-ant-escrow`. One PDA per escrowed ANT, derived deterministically from the ANT mint.

```
┌─────────────────┐         ┌──────────────────────┐
│ Depositor       │ deposit │ EscrowAnt PDA        │
│ (Solana wallet) ├────────►│ holds ANT in custody │
└─────────────────┘         │ (mpl-core asset is   │
                            │  owned by this PDA)  │
                            └──────────┬───────────┘
                                       │
                   sig verified        │ claim with valid
                   against on-chain    │ Arweave RSA-PSS or
                   public key          ▼ Ethereum ECDSA sig
                            ┌──────────────────────┐
                            │ Claimant             │
                            │ (Solana wallet       │
                            │  named in message)   │
                            └──────────────────────┘
```

The escrow PDA is the on-chain owner of the Metaplex Core asset during custody. The PDA signs for the asset using its seeds when releasing or cancelling.

---

## Account model

```rust
#[account]
pub struct EscrowAnt {
    /// Schema version for forward compatibility. v1 = 1.
    pub version: u8,                      //  1

    /// PDA bump.
    pub bump: u8,                         //  1

    /// Solana wallet that deposited the ANT and pays rent.
    /// Receives rent on close; can call cancel_deposit and update_recipient.
    pub depositor: Pubkey,                // 32

    /// The ANT mint pubkey. Denormalized from PDA seeds for safety.
    pub ant_mint: Pubkey,                 // 32

    /// 0 = Arweave RSA-PSS-4096, 1 = Ethereum ECDSA secp256k1.
    pub recipient_protocol: u8,           //  1

    /// Length of recipient_pubkey in bytes:
    /// - Arweave: 512 (4096-bit RSA modulus)
    /// - Ethereum: 20 (Keccak address)
    pub recipient_pubkey_len: u16,        //  2

    /// Padded to 512 bytes. For Arweave, the full RSA modulus.
    /// For Ethereum, the 20-byte address followed by 492 zero bytes.
    pub recipient_pubkey: [u8; 512],      // 512

    /// Anti-replay nonce. Rotated on every update_recipient.
    /// Initial value: sha256(deposit_slot || ant_mint || depositor).
    pub nonce: [u8; 32],                  // 32

    /// Slot at deposit. Informational; not used in any logic.
    pub deposit_slot: u64,                //  8

    /// Reserved for future fields without breaking layout.
    pub _reserved: [u8; 32],              // 32

    // Total: 653 bytes
    // + 8 byte Anchor discriminator = 661 bytes on-chain
    // Rent at 0.00007 SOL/byte ≈ 0.046 SOL ≈ $0.67 at SOL $146
}
```

**PDA seeds:** `["escrow_ant", ant_mint]` — exactly one escrow per ANT mint.

**Why fixed 512 bytes for Ethereum addresses:** uniform layout simplifies verifier dispatch and audit. The 492-byte overhead per Ethereum escrow costs ~$0.45 in extra rent — acceptable for the simpler code path. v2 can split into protocol-specific account types if rent becomes a concern at scale.

---

## Instructions

### `deposit_ant`

Lock an ANT into escrow targeted at a specific Arweave or Ethereum identity.

```rust
pub fn deposit_ant(
    ctx: Context<DepositAnt>,
    recipient_protocol: u8,
    recipient_pubkey: Vec<u8>,  // 512 bytes (Arweave) or 20 bytes (Ethereum)
) -> Result<()>
```

**Validations:**
- `recipient_protocol` must be `0` (Arweave) or `1` (Ethereum). Reject all other values.
- For protocol `0`: `recipient_pubkey.len() == 512`.
- For protocol `1`: `recipient_pubkey.len() == 20`.
- The signer must be the current owner of the ANT (verified by mpl-core `TransferV1` requiring asset-owner signature).

**Effects:**
- Initializes the EscrowAnt PDA at `["escrow_ant", ant_mint]` with `init` constraint.
- CPI to `mpl-core::TransferV1` to move the ANT from depositor to the escrow PDA.
- Computes nonce: `sha256(deposit_slot.to_le_bytes() || ant_mint || depositor)`.
- Pads the recipient pubkey into the fixed 512-byte field, stores `recipient_pubkey_len`.

**Accounts:**

| Name | Mut | Signer | Notes |
|------|-----|--------|-------|
| `escrow` | Yes | No | `init`, seeds = `["escrow_ant", ant_mint]` |
| `ant_asset` | Yes | No | Metaplex Core asset; transferred by the CPI |
| `ant_mint` | No | No | Reference for PDA derivation |
| `depositor` | Yes | Yes | Pays rent + tx fee |
| `mpl_core_program` | No | No | For TransferV1 CPI |
| `system_program` | No | No | For account creation |
| `clock` (sysvar) | No | No | For nonce derivation |

---

### `claim_ant_arweave_attested`

Release the ANT to a Solana address designated in an Arweave RSA-PSS-signed message that has been re-attested off-chain by the AR.IO attestor service.

```rust
pub fn claim_ant_arweave_attested(
    ctx: Context<ClaimAntArweaveAttested>,
    message_nonce: [u8; 32],     // user-chosen, must match escrow.nonce
) -> Result<()>
```

The transaction must include a Solana `Ed25519Program` native sigverify instruction at index `current_ix - 1` carrying the attestor's pubkey, Ed25519 signature, and the canonical message bytes. The on-chain handler reads `sysvar::instructions` to confirm those bytes verified.

**Validations:**
1. `escrow.recipient_protocol == 0` (Arweave).
2. `message_nonce == escrow.nonce` (replay protection).
3. Reconstruct the canonical message bytes from accounts, including the `recipient: <43-char base64url(sha256(escrow.recipient_pubkey))>` line that binds the canonical to the deposit-time recipient identity (closes F-1).
4. Read the preceding `Ed25519Program` ix via `sysvar::instructions`; require its signing pubkey == `ATTESTOR_PUBKEY` (compiled-in constant) and its signed message == the reconstructed canonical bytes.

**Effects:**
- CPI to `mpl-core::TransferV1` from escrow PDA → `claimant` account, signed by escrow PDA seeds.
- CPI to `mpl-core::UpdateAuthorityV1` to rotate UA to the claimant atomically.
- Closes the escrow PDA via Anchor's `close = depositor` constraint, returning rent to the depositor.

**Accounts:**

| Name | Mut | Signer | Notes |
|------|-----|--------|-------|
| `escrow` | Yes | No | `close = depositor` |
| `ant_asset` | Yes | No | Metaplex Core asset |
| `ant_mint` | No | No | For PDA seed validation |
| `claimant` | Yes | No | Receives the ANT (referenced by canonical message) |
| `depositor` | Yes | No | Receives rent on escrow close |
| `payer` | Yes | Yes | Pays tx fee (anyone can submit; doesn't have to be claimant) |
| `mpl_core_program` | No | No | For TransferV1 CPI |
| `ario_ant_program` | No | No | For reconcile CPI |
| `ant_config` | Yes | No | For reconcile |
| `ant_controllers` | Yes | No | For reconcile |
| `system_program` | No | No | — |

---

### `claim_ant_ethereum`

Same as `claim_ant_arweave` but with ECDSA secp256k1 signature verification.

```rust
pub fn claim_ant_ethereum(
    ctx: Context<ClaimAntEthereum>,
    message_nonce: [u8; 32],
    signature: [u8; 65],         // r (32) || s (32) || v (1, recovery id)
) -> Result<()>
```

**Validations:**
1. `escrow.recipient_protocol == 1` (Ethereum).
2. `message_nonce == escrow.nonce`.
3. Reconstruct the canonical message and apply EIP-191 prefix.
4. Compute Keccak-256 of `prefix || message`.
5. `secp256k1_recover` syscall to recover the signing pubkey.
6. Verify `keccak256(pubkey)[12..32] == escrow.recipient_pubkey[..20]`.
7. Enforce low-S form (per EIP-2): `s ≤ secp256k1_n/2`.

**Effects:** Identical to `claim_ant_arweave` (transfer ANT, reconcile, close escrow).

---

### `update_recipient`

Allows the depositor to change the recipient identity any time before the ANT is claimed.

```rust
pub fn update_recipient(
    ctx: Context<UpdateRecipient>,
    new_protocol: u8,
    new_pubkey: Vec<u8>,
) -> Result<()>
```

**Validations:**
- Signer must be `escrow.depositor`.
- `new_protocol` must be `0` or `1`.
- `new_pubkey.len()` must match the protocol (512 or 20).

**Effects:**
- Updates `recipient_protocol`, `recipient_pubkey`, `recipient_pubkey_len`.
- **Rotates `nonce` to `sha256(current_slot || ant_mint || depositor || old_nonce)`**. This invalidates any in-flight claim signatures bound to the old nonce.

---

### `cancel_deposit`

Returns the ANT to the depositor unilaterally.

```rust
pub fn cancel_deposit(ctx: Context<CancelDeposit>) -> Result<()>
```

**Validations:**
- Signer must be `escrow.depositor`.

**Effects:**
- CPI to `mpl-core::TransferV1` from escrow PDA → depositor, signed by escrow PDA seeds.
- CPI to `ario_ant::reconcile` (same as in claim).
- Closes the escrow PDA, returning rent to the depositor.

---

## Canonical message format

The bytes that get signed must be reproducible exactly on-chain from the escrow account state and the claim transaction's accounts. **No client-supplied message** — the program canonicalizes from accounts, eliminating message-malleability attacks.

### Format (UTF-8 text, line-feed separated)

```
ar.io ant-escrow claim v1
network: solana-mainnet
ant: <ant_mint_base58>
claimant: <claimant_solana_pubkey_base58>
nonce: <nonce_hex_lowercase>
```

**Concrete example:**

```
ar.io ant-escrow claim v1
network: solana-mainnet
ant: 9PnRFwk2Yp7QyU3sQzXwUhJj6tVyM4nN2KqL5fT8RbAW
claimant: Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A
nonce: a3f1c8d92e0b4f7a8e1d6c5b4a3920817f6e5d4c3b2a19188776655443322110
```

### Field rules

| Field | Format | Source |
|-------|--------|--------|
| Header | `ar.io ant-escrow claim v1` literal, exactly | constant |
| `network` | `solana-mainnet` or `solana-devnet`, lowercase | hardcoded by program at deploy time |
| `ant` | Base58 encoding of `escrow.ant_mint` | from escrow account |
| `claimant` | Base58 encoding of the `claimant` tx account | from tx accounts |
| `nonce` | Lowercase hex of `escrow.nonce` (no `0x` prefix), 64 chars | from escrow account |
| Line ending | Single `\n` (0x0A) between fields, no trailing newline | constant |

### Network field

The `network` value is baked into the program at deploy time as a `&'static [u8]`. We deploy two program instances (mainnet and devnet) with different program IDs and the appropriate network string compiled in. This prevents cross-network signature replay.

### Wallet wrapping

**Arweave:** wallets sign the canonical message bytes directly (no prefix). The on-chain SHA-256 hashes the canonical bytes for PSS verification.

**Ethereum:** the canonical message gets the EIP-191 `personal_sign` prefix applied by the wallet. The on-chain code prepends `\x19Ethereum Signed Message:\n<len>` before keccak hashing. This matches what `wallet.signMessage(canonical)` produces in ethers.js / viem / MetaMask.

---

## Cryptographic verification

### Arweave — production path (off-chain attestor → Ed25519 introspection)

Per ADR-017, Arweave-recipient claims route through an AR.IO-operated
off-chain attestor service. The on-chain program verifies a cheap
Ed25519 signature; the expensive RSA-PSS-4096 verification happens
off-chain.

#### Flow

```
[user / browser]                   [attestor]                  [Solana program]
   sign canonical msg
   with Arweave wallet
   (RSA-PSS-4096)
        |
        v
   POST /attest  ─────────────►  verify RSA-PSS via node:crypto
                                  sign canonical with Ed25519
                                  return Ed25519 sig + pubkey
        v
   build Solana tx with:
     1. Ed25519Program ix
        (native sigverify, ~720 CU)
     2. claim_*_arweave_attested ix
        (introspects ix #1)        ─►  reconstruct canonical message
                                       from escrow state, confirm
                                       Ed25519Program checked the
                                       attestor's sig over those bytes,
                                       release the asset
```

#### On-chain check (`verify::attested::verify_attested_signature`)

The `claim_*_arweave_attested` instruction takes the
`sysvar::instructions` account and:

1. Loads the instruction at `current_index - 1`.
2. Confirms `program_id == Ed25519SigVerify111111111111111111111111111`.
3. Confirms exactly one signature in the ix data (`num_signatures == 1`).
4. Confirms all three offset fields (signature, pubkey, message)
   point into this same instruction's data buffer (`*_ix_index ==
   0xFFFF`) — rejects cross-instruction data references which would
   otherwise let an attacker stage attestation bytes elsewhere.
5. Bounds-checks all offsets, then extracts the pubkey (32 bytes) and
   message bytes.
6. Asserts `pubkey == ATTESTOR_PUBKEY` (compiled-in constant in
   `state.rs`).
7. Asserts `message == reconstructed canonical message from escrow state`.

If all checks pass, the cryptographic verification has already been
done by Solana's runtime (the Ed25519Program ix runs before our ix).
The introspection step confirms WHAT was verified.

CU cost end-to-end: ~50-77K depending on claim type (ANT vs token vs
vault), including all MPL Core CPIs and SPL transfers.

#### Trust model

The attestor's Ed25519 secret can mint valid attestations. Compromise
allows draining all Arweave-recipient escrows. Recovery: program
upgrade swapping `ATTESTOR_PUBKEY` (~30 minute runbook in
`migration/attestor/README.md` § "Key rotation"). The trust assumption
is bounded by the migration window's existing AR.IO-operated trust
model — see ADR-017 for the full analysis.

### Arweave — historical on-chain path (REMOVED)

Earlier revisions of this crate had `claim_*_arweave` instructions
that did the RSA-PSS verification on-chain via `sol_big_mod_exp`.
Removed in commit 4ce73e4 — the syscall is feature-gated and disabled
on every public Solana cluster, so any `.so` referencing it failed to
load on devnet/mainnet. The full RFC 8017 §9.1.2 algorithm spec is
preserved in `migration/attestor/src/verify-rsa-pss.ts` (which
delegates to Node's hardware-accelerated `node:crypto.verify` rather
than re-implementing the modexp by hand).

### Ethereum ECDSA

```
Inputs:
    message:           bytes (canonical)
    signature:         [u8; 65]   (r || s || v)
    expected_address:  [u8; 20]   (escrow.recipient_pubkey[0..20])

Steps:
    1. prefix = "\x19Ethereum Signed Message:\n" + ascii(message.len())
    2. msg_hash = keccak256(prefix || message)     // 32 bytes

    3. require signature[64] in {27, 28, 0, 1}     // recovery id (legacy or modern)
       v = signature[64] - 27 if >= 27 else signature[64]

    4. Enforce low-S (EIP-2):
         require s <= secp256k1_n / 2

    5. recovered_pubkey = secp256k1_recover(msg_hash, v, signature[0..64])
       └─ uses Solana precompile (~25K CU)

    6. address = keccak256(recovered_pubkey)[12..32]

    7. require address == expected_address
```

**Estimated compute cost:** secp256k1_recover (~25K CU) + 2× keccak (~10K CU) + byte ops ~= **30K-40K CU total**.

---

## Security analysis

### Threat model

The program holds Metaplex Core ANTs that may have material value (purchased ArNS names worth hundreds to thousands of USD each, sometimes more). Adversaries may include:

- **Network observers** with access to mempool data
- **Compromised RPC providers**
- **Malicious co-signers** (depositor or claimant trying to extract more than they should)
- **Random callers** trying generic exploits (unauthorized claim, replay, parameter manipulation)

The program does **not** defend against:

- A compromised recipient wallet (Arweave or Ethereum key holder) — they're the legitimate owner of the claim right
- Compromised Solana RPC providers serving stale state — orthogonal infrastructure concern
- Physical or social engineering attacks on key custody

### Replay protection — three layers

1. **Message binds claimant Solana pubkey.** A signature for "claim to wallet X" cannot be reused to claim to wallet Y. The on-chain canonicalization reads the claimant pubkey from the tx accounts and includes it in the verified message bytes.

2. **Message binds escrow nonce.** Each escrow has a unique 32-byte nonce, rotated on every `update_recipient`. A signature valid for nonce N is invalid after the nonce rotates.

3. **Escrow PDA closes on claim.** Even if a valid signature is replayed to the same nonce, the second `claim_ant_*` call fails because the escrow account no longer exists.

### Front-running / MEV analysis

| Attack | Mitigation |
|--------|------------|
| Bot reuses sig in mempool with different claimant | Sig binds claimant pubkey via canonical message — substituting fails verification |
| Bot front-runs with higher priority fee | Doesn't help — ANT goes to claimant in message regardless of submitter; the only thing the bot wins is paying the tx fee for someone else |
| Bot grief-spam by front-running with bad sigs | Each bad attempt fails CU validation, attacker wastes gas; no DoS leverage |
| Bot front-runs `cancel_deposit` with stolen sig | If the bot has a valid sig, the recipient was authorized to claim; this isn't an attack, it's the system working |
| Bot front-runs `update_recipient` with old recipient's claim | Race condition — old recipient legitimately wins. Solution: depositor should `cancel_deposit` first if they want to definitely block, then re-deposit. |

The "anyone can submit, only the named claimant receives" pattern eliminates most MEV concerns.

### Race conditions

**Race 1: `update_recipient` vs `claim_ant_*`**

- Old recipient has a valid signature in flight
- Depositor submits `update_recipient`
- Both txs in same slot; whichever lands first wins

If old recipient's claim lands first: ANT transfers, `update_recipient` fails (no escrow). If `update_recipient` lands first: nonce rotates, old sig invalid.

This is acceptable behavior. If depositor wants to definitively block an in-flight claim, the safer pattern is:

```
Step 1: cancel_deposit         (immediate)
Step 2: deposit_ant (new key)  (separate tx, after cancel confirmed)
```

**Race 2: simultaneous `claim_ant_arweave` and `claim_ant_ethereum`**

Cannot happen — `recipient_protocol` is set at deposit and only one claim path is active. The other path's instruction would fail the protocol check.

**Race 3: `cancel_deposit` vs `claim_ant_*`**

Same as Race 1. Whoever wins the slot ordering wins.

### Implementation pitfalls

These are the known traps in PSS verification and signature handling. Auditors should flag any of them as blocker findings.

| Pitfall | Mitigation |
|---------|------------|
| Accepting PKCS#1 v1.5 sigs when expecting PSS | Strict trailer check: `EM[511] == 0xBC` (PSS) vs `0x00` (v1.5) |
| Accepting forged sig due to weak modulus | Out of scope — recipient chose their key. Affects only their escrow. |
| MGF1 incorrect counter encoding | Use big-endian 32-bit counter per RFC 8017 §B.2.1 |
| Salt length confusion | Pass `salt_len` explicitly, validate against PSS spec; reject `salt_len > 32` |
| Padding leftmost-bit zero | Mandatory `DB[0] &= 0x7F` after XOR (we have full 4096-bit modulus, so this is one bit) |
| Constant-time comparisons | Use byte-by-byte equality with no early exit. BPF does not natively guarantee CT execution; this is best-effort |
| Cross-protocol replay (Arweave sig as Ethereum) | Different sig sizes (512 vs 65) make this structurally impossible; protocol field is also checked |
| Recovery id (`v`) handling | Accept both modern (0/1) and legacy (27/28) forms; reject anything else |
| ECDSA malleability (high-S) | Enforce `s ≤ secp256k1_n / 2` per EIP-2 |
| Nonce predictability | Nonce derived from `slot || ant_mint || depositor` is deterministic but unpredictable to attackers without access to the deposit slot context. Rotated on update_recipient using old nonce as input. |
| ANT state coherence after transfer | Bundle `ario_ant::reconcile` CPI in the same tx as the claim/cancel transfer |

### What's intentionally not protected

- **Depositor changing recipient repeatedly to grief.** If depositor flips recipient back and forth, in-flight claims lose to the latest update. This is by design — depositor controls the escrow. Recipients who need certainty should claim quickly or use a v2 `lock_deposit` (future work).
- **ANT abandonment.** If both depositor and recipient lose their keys, the ANT and its rent (~$0.67) are locked forever. ~$0.67 is acceptable as the cost of maximally simple semantics. State bloat is a Solana-validator concern, not a protocol concern.
- **Recipient losing key after claim sig is signed but before submission.** The recipient still controls who gets the ANT (the claimant Solana pubkey in the message is theirs to choose). This isn't a protocol bug; it's a recipient ops issue.

---

## Performance

### Per-instruction CU budget

| Operation | CU (estimated) |
|-----------|----------------|
| `deposit_ant` | ~30K (account init + mpl-core TransferV1) |
| `claim_ant_arweave` | ~80-120K (PSS verify + TransferV1 + reconcile + close) |
| `claim_ant_ethereum` | ~60-80K (ECDSA recover + TransferV1 + reconcile + close) |
| `update_recipient` | ~5K |
| `cancel_deposit` | ~25K (TransferV1 + reconcile + close) |

All within Solana's 200K-CU default per-instruction budget. No `SetComputeUnitLimit` needed except possibly for `claim_ant_arweave` if margins are tight (cheap to add as a precaution).

### Per-tx size budget (1232 byte limit)

`claim_ant_arweave` is the biggest tx:

| Component | Bytes |
|-----------|-------|
| Tx header + 1 sig | ~136 |
| Account metas (10 accounts × 32 + flags) | ~330 |
| ix data: 8 disc + 32 nonce + 512 sig + 1 salt_len | 553 |
| **Total** | **~1019 / 1232** |

Comfortable. Address Lookup Tables can compress account metas if we ever need more headroom.

`claim_ant_ethereum` is even smaller (~570 bytes total).

### Storage

Per-escrow rent: ~0.046 SOL (~$0.67). Released on cancel or claim.

---

## State coherence with `ario-ant`

When the ANT changes hands (deposit, claim, cancel), the `ario-ant` program tracks ownership lazily via `last_known_owner` in `AntConfig`. Three coherence considerations:

1. **On `deposit_ant`:** ANT moves to escrow PDA. ario-ant is unaware until next instruction. If anyone tries to use the ANT's controllers between deposit and claim, those calls will hit the lazy reconciliation path and clear controllers. Effectively, depositing puts the ANT in a frozen state.

2. **On `claim_ant_*` and `cancel_deposit`:** The transfer CPI changes the on-chain owner. Best practice is to **also CPI `ario_ant::reconcile`** in the same instruction so:
   - `AntConfig.last_known_owner` updates immediately
   - Any stale controllers from before the deposit are cleared
   - The new owner has a clean slate

3. **Attribute plugin authority:** Per ADR-012, the Attributes plugin authority is `Owner`. When the ANT moves to escrow PDA, the PDA gains plugin authority; on claim, the claimant gains it. No additional coordination needed — Metaplex Core handles plugin authority transfer with the asset.

The reconcile CPI is non-essential (the next `set_record`/`add_controller` call would do the same) but is good hygiene and aligns with how migration claims work.

---

## Testing strategy

### Unit tests (in-program)

- ✅ Valid deposits (Arweave + Ethereum) succeed
- ❌ Invalid `recipient_protocol` (e.g., 2) rejected
- ❌ Invalid `recipient_pubkey_len` for protocol rejected
- ❌ Non-owner attempting deposit rejected (mpl-core TransferV1 fails)
- ✅ PSS verifier accepts valid signatures from `arweave-js`
- ❌ PSS verifier rejects: tampered message, tampered sig, wrong key, wrong salt_len, PKCS#1 v1.5 sig (downgrade attempt), non-0xBC trailer
- ✅ ECDSA verifier accepts valid signatures from ethers.js / viem / MetaMask
- ❌ ECDSA verifier rejects: tampered, wrong key, high-S, invalid recovery id
- ✅ Claim succeeds and ANT transfers to claimant
- ✅ Claim rotates escrow PDA closed, rent returned to depositor
- ❌ Claim with `message_nonce != escrow.nonce` rejected
- ❌ Claim using cross-protocol path (Arweave sig with `claim_ant_ethereum`) rejected
- ✅ `update_recipient` rotates nonce and updates recipient
- ❌ `update_recipient` from non-depositor rejected
- ❌ Old signature invalidated after `update_recipient`
- ✅ `cancel_deposit` returns ANT to depositor
- ❌ `cancel_deposit` from non-depositor rejected
- ✅ Replay rejection: same valid sig submitted twice — second fails (PDA closed)

### Integration tests (`solana-program-test`)

- End-to-end: deposit Arweave → user signs with arweave-js → claim → ANT received
- End-to-end: deposit Ethereum → user signs with viem → claim → ANT received
- Recipient protocol switch via `update_recipient` (Arweave → Ethereum) with new identity
- ANT held in escrow can't be transferred by anyone other than escrow program (frozen state)
- Reconcile CPI clears stale controllers on claim

### Differential / vector testing

- 100+ test fixtures generated via `arweave-js` with random keys → on-chain verify
- 100+ test fixtures generated via `ethers.js` and `viem` → on-chain verify
- Cross-check on-chain PSS implementation against:
  - OpenSSL CLI signed test vectors
  - RustCrypto `rsa` crate signed test vectors
  - Browser SubtleCrypto signed test vectors (Arweave wallet's actual signing path)

All sources should produce signatures that the on-chain verifier accepts identically.

### Fuzzing

`cargo-fuzz` target on `verify_rsa_pss(random_message, random_sig, random_modulus, random_salt_len)`:

- Run 24+ hours pre-mainnet
- Coverage-guided to maximize branch hit rate
- Pass criteria: no panics, no infinite loops, no UB, no false positives (no random bytes accepted as valid)

### NIST CAVP test vectors

Pull RSA-PSS test vectors from NIST's Cryptographic Algorithm Validation Program archive. Verify the on-chain implementation matches the published expected outputs for both positive (must accept) and negative (must reject) test cases.

### Audit-grade requirements

Pre-mainnet audit must include:
- Independent review of PSS implementation against latest CVE history (RSA-PSS bugs are common — historical examples: CVE-2017-13098, CVE-2019-1559, CVE-2020-13624)
- Review of all `unsafe` blocks (ideally none in this program)
- Review of `mpl-core` CPI usage (asset transfer, plugin coherence)
- Review of nonce derivation entropy
- Review of EIP-191 prefix construction edge cases
- Review of close-account semantics (no double-spend of rent)

---

## SDK / CLI surface

### TypeScript SDK

```typescript
import { ANTEscrow } from '@ar.io/sdk';

// Deposit
const escrow = ANTEscrow.init({ rpc, signer });

await escrow.deposit({
  antMint: 'BdJ8...',
  recipient: {
    protocol: 'arweave',
    publicKey: arweaveJwk.n,  // 512-byte RSA modulus, base64url
  },
});
// or:
await escrow.deposit({
  antMint: 'BdJ8...',
  recipient: { protocol: 'ethereum', address: '0xabc...' },
});

// Read escrow state
const state = await escrow.get(antMint);
// → { depositor, antMint, recipientProtocol, recipientPubkey, nonce, depositSlot }

// Build the canonical message client-side (deterministic, must match on-chain)
const message = ANTEscrow.canonicalMessage({
  network: 'solana-mainnet',
  antMint,
  claimant: claimantSolanaPubkey,
  nonce: state.nonce,
});

// User signs with their wallet of choice
const signature = await arweaveWallet.signMessage(message);
// or:
const signature = await ethereumWallet.signMessage(message);

// Anyone can submit the claim (claimant doesn't have to be the fee payer)
await escrow.claimArweave({ antMint, signature, saltLen: 32 });
// or:
await escrow.claimEthereum({ antMint, signature });

// Depositor controls
await escrow.updateRecipient({ antMint, newRecipient });
await escrow.cancel({ antMint });
```

### CLI

```bash
# Deposit
ar.io escrow deposit --ant <mint> \
    --recipient-arweave <jwk-pub-file> \
    --keypair depositor.json

ar.io escrow deposit --ant <mint> \
    --recipient-ethereum 0xabc... \
    --keypair depositor.json

# Read
ar.io escrow status --ant <mint>

# Claim (anyone can submit)
ar.io escrow claim --ant <mint> \
    --signature-file sig.bin \
    --claimant <solana-pubkey> \
    --keypair fee-payer.json

# Depositor management
ar.io escrow update-recipient --ant <mint> --new-recipient ... --keypair depositor.json
ar.io escrow cancel --ant <mint> --keypair depositor.json
```

The SDK exports a `canonicalMessage(...)` helper so external tooling can produce signatures without reading the program source. The function returns exact bytes — wallets sign these directly.

---

## Frontend (`migration/solana-escrow-app`)

The SDK and CLI are sufficient for power users and integrators, but most depositors and recipients will need a web UI. The good news: **this is mostly already built.** The recommended approach is to **fork the entire `solana-registration-app`** as the starting framework rather than cherry-picking components into a fresh project.

### Why fork, not cherry-pick

The migration registration app sunsets after cutover — its purpose ends when the migration window closes. Forking the whole thing repurposes that codebase as a permanent post-launch feature:

- **Preserves all hardening** already invested: mobile responsiveness, wallet edge-cases, error states, loading states, accessibility
- **Inherits the entire build/deploy/CI pipeline** — Vite config, lint rules, dependency manifest, deploy scripts
- **Same visual identity** as the registration app, which users will already associate with AR.IO multi-protocol flows
- **Faster** than building a parallel app from scratch — the structural work is done

After mainnet, the original `solana-registration-app/` can be archived in `docs/archive/` or kept as a frozen snapshot of the migration window. The fork (`solana-escrow-app/`) becomes the long-lived multi-protocol UI for AR.IO on Solana.

### The fork operation

```
cp -r migration/solana-registration-app migration/solana-escrow-app
```

Then repurpose:

| Layer | What changes |
|-------|--------------|
| `package.json` | Rename, bump deps if needed |
| Branding (logo, hero copy, social preview) | "Register your address" → "ANT escrow" |
| Routes | `/register` → `/deposit`, `/claim`, `/manage`, `/lookup` |
| API integration | Turbo attestation upload → Solana program ix submission via `@ar.io/sdk` |
| State queries | Arweave GraphQL polling → Solana RPC `getAccountInfo` for escrow PDA state |
| Components — keep | `ArweaveWalletConnect`, `EthereumWalletConnect`, `SolanaWalletConnect`, `SourceAddressSigner`, `SourceWalletConnect`, `WalletIcons` (no changes needed; multi-protocol signing is identical) |
| Components — replace | `RegistrationProgress`, `ExistingRegistrationCheck`, `useTurboAttestation`, `useAttestationStatus`, `useAOAssetLookup`, `CountdownTimer` |
| Components — repurpose | `AssetPreview` → ANT preview card with mint, ArNS name, recipient identity |

### Components inherited (zero-touch)

Direct reuse from the fork — these already do exactly what the escrow app needs:

| Component | What it gives us |
|-----------|------------------|
| `SolanaWalletConnect` | Phantom / Solflare / Wander connect; depositor + claimant tx submission |
| `ArweaveWalletConnect` | Wander / ArConnect; Arweave-recipient claim signing |
| `EthereumWalletConnect` | MetaMask / WalletConnect; Ethereum-recipient claim signing |
| `SourceAddressSigner` | Multi-protocol signing dispatcher; abstracts the per-wallet signing API |
| `SourceWalletConnect` | Wallet selection UI |
| `WalletIcons` | Branding |

The registration app's signing flow (Arweave/Ethereum source wallet signs a canonical message → bundle attestation via Turbo) is structurally identical to the escrow's claim flow (recipient signs canonical message → submit Solana tx). Same wallet APIs, same UX shape — just a different submission target.

### Three primary user flows

**Flow 1: Depositor deposits an ANT**

```
1. Connect Solana wallet
2. Select an ANT from owned NFTs (uses ANT.list({ owner }) from SDK)
3. Choose recipient identity:
     - Arweave: paste JWK public key, OR enter ArNS name to look up
     - Ethereum: paste 0x... address
4. Confirm deposit cost preview (~$0.67 rent + tx fee)
5. Sign and submit deposit_ant tx
6. Show success: escrow link, share-with-recipient text
```

**Flow 2: Recipient claims an ANT**

```
1. Land on /claim?ant=<mint> (or paste mint into search field)
2. Fetch escrow state: SDK escrow.get(antMint)
3. Connect appropriate source wallet (Arweave or Ethereum, dispatched
   from escrow.recipientProtocol)
4. Connect Solana destination wallet (the claimant — can differ from
   the source wallet's chain)
5. Display canonical message preview (formatted, human-readable)
6. Click "Sign Message" — wallet signs the canonical bytes
7. Click "Submit Claim" — tx submitted to Solana, ANT released to claimant
8. Show success: ANT appears in claimant's Solana wallet
```

**Flow 3: Depositor manages an active escrow**

```
1. /manage?ant=<mint>
2. Connect Solana wallet (must match escrow.depositor)
3. Two actions:
     - Update Recipient: change to a different Arweave/Ethereum identity
     - Cancel Deposit: pull ANT back to depositor's wallet
4. Confirm + submit
```

### Page structure (proposed)

```
/                       Landing — explainer, links to deposit / claim / lookup
/deposit                Depositor flow (1)
/claim?ant=<mint>       Recipient flow (2)
/manage?ant=<mint>      Depositor management (3)
/lookup?ant=<mint>      Read-only escrow status, no wallet connection required
/help                   Usage guide, FAQ, security notes
```

### Stack

Match the registration app to minimize integration friction:

- **React + TypeScript + Vite** (same as registration app)
- **`@ar.io/sdk`** for ANT operations and escrow PDA derivation
- **`@solana/kit`** for tx pipeline (matches main SDK direction)
- **`@solana/wallet-adapter-react`** for Solana wallet connection
- **`@othent/kms` / `arweave-wallet-kit`** for Arweave wallet connection
- **`wagmi` + `viem`** for Ethereum wallet connection
- **Tailwind / shadcn** for UI consistency with registration app

### Canonical message preview UX

Critical UX detail — the canonical message is what the user is actually signing, and they should see it clearly before signing. Show the formatted message in a readable monospace block with the protocol-specific wrapping called out:

```
─────────────────────────────────────────────────────
 You're authorizing the release of this ANT.

 Sign the message below with your Arweave wallet:

 ┌───────────────────────────────────────────────┐
 │ ar.io ant-escrow claim v1                     │
 │ network: solana-mainnet                       │
 │ ant: 9PnRFwk2Yp7QyU3sQzXwUhJj6tVyM4nN2KqL5fT… │
 │ claimant: Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN… │
 │ nonce: a3f1c8d92e0b4f7a8e1d6c5b4a3920817f6e5… │
 └───────────────────────────────────────────────┘

 The ANT will be released to:
   Hk6RfBp4FpvF2hYBmJ9kqyL5dE3xR8wPzN7sV6cTqL2A

 [ Cancel ]            [ Sign with Wander ▸ ]
─────────────────────────────────────────────────────
```

For Ethereum, show the EIP-191 prefix as a separate block so users can see what's actually being signed (matches what MetaMask shows in its prompt).

### Build effort estimate (fork-based)

- Initial fork + rename + dependency cleanup: **~½ day**
- Strip out registration-specific flows (attestation upload, status polling, Arweave GraphQL queries): **~1 day**
- Three flows (deposit, claim, manage) replacing the registration flow: **~4-5 days**
- Lookup page (read-only) + updated landing copy: **~1 day**
- Branding refresh (logo, hero, social preview): **~½ day**
- Integration testing against devnet contract: **~2 days**

**Total: ~1.5-2 weeks of focused frontend work**, faster than the from-scratch estimate because the wallet plumbing, build pipeline, and visual style are already done.

### Versioning + deployment

- Lives at `migration/solana-escrow-app/` initially (forked from `solana-registration-app`)
- Post-cutover, consider moving out of `migration/` since it's a permanent feature; archive the original `solana-registration-app/` once the registration window closes
- Deploy as a static site to AR.IO's existing hosting (likely served from an ArNS name)
- One build per network (mainnet vs devnet)

### Out of scope for v1

- ANT marketplace integration (linking from Magic Eden / Tensor listings to deposit flow)
- Multi-language i18n
- Mobile-native apps
- Notifications when an escrow is claimed (could be a small backend service later)

---

## Deployment & rollout

### Phase 0: Design lock (1 week)

- Independent crypto reviewer signs off on PSS implementation spec
- Lock canonical message format (this doc)
- Define test vector set
- Decide network-string-at-compile-time deployment strategy

### Phase 1: Implementation (3-4 weeks)

- Contract code (`programs/ario-ant-escrow/`)
- Integration tests using `solana-program-test`
- Differential test fixtures from arweave-js, ethers.js, OpenSSL, RustCrypto
- SDK `@ar.io/sdk` integration (`sdk/src/solana/escrow.ts`)
- CLI command set
- Codama codegen for the new program
- Frontend (`migration/solana-escrow-app/`) — fork wallet plumbing from `solana-registration-app`, build deposit / claim / manage / lookup flows. Can run in parallel with contract work once the SDK shape is locked.

### Phase 2: Audit (2 weeks audit + 1 week fixes)

- Independent security audit focused on:
  - PSS correctness vs CVE history
  - Replay protection layers
  - MEV resistance
  - mpl-core CPI safety
  - Nonce derivation
- Bug-bash with Anza ecosystem

### Phase 3: Devnet (4 weeks)

- Public devnet contract deployment
- Frontend pointed at devnet program ID
- Bug bounty announced (10K-50K USD pool)
- Active monitoring for unexpected reverts

### Phase 4: Mainnet rollout

- Initial deployment with no usage caps (the program has no protocol-level value cap)
- Monitor for 30 days; pause via upgrade authority if any anomaly detected
- After 30 days clean operation: consider revoking upgrade authority for full immutability

**Total timeline:** ~10-12 weeks from design lock to mainnet.

---

## Open questions / future work

1. **`lock_deposit` instruction (v2).** Flips an escrow to irrevocable — depositor loses ability to `cancel_deposit` and `update_recipient`. Enables atomic-swap-style flows where the recipient needs a perfect guarantee. One-way operation; once locked, only the recipient can release.

2. **Atomic payment escrow (v2).** Optional `payment_required: { amount, mint }` field. Claim only succeeds if the same tx includes a token transfer of the specified amount to the depositor. Combined with `lock_deposit`, this enables fully on-chain ANT-for-tokens swaps.

3. **Solana → Solana escrow (Ed25519 path).** Trivial to add given the multi-protocol design. Useful for sub-account flows and enterprise wallets that want to escrow ANTs internally.

4. **Permissionless cleanup of abandoned escrows.** Add a no-op `prune_abandoned` instruction with a long expiry (e.g., 5 years of zero activity) that allows anyone to close the escrow and burn the rent. Solves the state-bloat concern at minor UX cost. Decide if this is worth the complexity vs accepting the ~$0.67 abandonment cost.

5. **Multi-recipient escrows.** `recipient: OneOf<Vec<Pubkey>>` allowing any of N keys to claim. Useful for enterprise wallets with multi-key arrangements. Adds verification complexity and account size; defer until use case is concrete.

6. **Generalize to non-ANT NFTs.** The program design isn't ANT-specific — it could escrow any Metaplex Core asset. Worth deciding whether to position as "ANT escrow" or "Core asset escrow" for naming and marketing.

---

## Generalized Escrow Extension (v2)

The escrow program has been extended beyond ANTs to handle three asset types:

1. **ANTs** (Metaplex Core NFTs) -- the original v1 escrow, unchanged.
2. **Tokens** (liquid ARIO SPL balances) -- deposit/claim/cancel/update-recipient for fungible token amounts.
3. **Vaults** (time-locked token positions from `ario-core`) -- deposit/claim/cancel/update-recipient with lock-aware claim behavior.

The program now has **15 total instructions**: the original 5 ANT instructions plus 10 token/vault instructions (deposit, claim-arweave, claim-ethereum, cancel, update-recipient for each of tokens and vaults).

**Canonical message compatibility:**

- The **v1 canonical message** (`ar.io ant-escrow claim v1`) is unchanged and continues to be used for all ANT escrows. Existing signatures remain valid.
- A **v2 canonical message** (`ar.io escrow claim v2`) is used for token and vault escrows. It includes `type`, `asset`, and `amount` fields instead of the `ant` field. The header prefix difference prevents cross-version replay.

**Vault claim behavior:**

Active vaults (where `end_timestamp > now`) are claimed using transaction instruction introspection: the escrow verifies via `sysvar::instructions` that a matching `ario_core::vaulted_transfer` instruction exists in the same transaction (20-instruction loop limit, 60-second tolerance on lock duration). The sibling `vaulted_transfer` creates a new time-locked vault for the claimant preserving the remaining lock duration. Expired vaults are claimed as liquid SPL transfers. This preserves the protocol's staking/locking invariants.

**Account model:**

Token and vault escrows use a shared `EscrowToken` account type (711 bytes) with an `asset_type` discriminator field. PDA derivation includes the depositor pubkey (unlike ANT escrows) to allow multiple depositors to escrow the same token mint independently:
- Token: `["escrow_token", depositor, asset_id]`
- Vault: `["escrow_vault", depositor, asset_id]`

See `docs/ANT_ESCROW_PROTOCOL_SPEC.md` section 11 for the full v2 specification including byte layouts, instruction details, and signing conventions.

---

## References

- ADR-004 — Original RSA migration design (when on-chain RSA wasn't feasible)
- ADR-012 — ANT Attributes plugin architecture (relevant for state coherence on transfer)
- BD-010 — ANT as Metaplex Core NFT (lazy ownership reconciliation behavior)
- Solana `sol_big_mod_exp` syscall — `solana_program::big_mod_exp::big_mod_exp`
- Solana `secp256k1_recover` — `solana_program::secp256k1_recover`
- RFC 8017 — RSA-PSS specification
- EIP-191 — Ethereum signed message format
- EIP-2 — ECDSA low-S enforcement
- NIST CAVP — Cryptographic Algorithm Validation Program test vectors
