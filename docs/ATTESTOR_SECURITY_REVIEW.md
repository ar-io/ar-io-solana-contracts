# Attestor Security Review (Adversarial)

**Reviewer:** Claude (adversarial pass)
**Date:** 2026-05-06
**Scope:** all code introduced by `fix/rsa-software-modexp` (PR #109)
**Initial verdict:** **DO NOT MERGE.** One CRITICAL vulnerability.
**Updated verdict (post-fix):** Critical + all medium/low findings remediated.
External audit still recommended before mainnet.

> **Scope update (commit 4ce73e4):** the legacy on-chain `claim_*_arweave`
> ixs + `verify/arweave.rs` were removed (they referenced the gated
> `sol_big_mod_exp` syscall, blocking devnet/mainnet deployment). All
> findings below apply to the **attested** Arweave claim path and the
> off-chain attestor service. The on-chain RSA-PSS verifier no longer
> exists, so the historical "verify/arweave.rs" code-path concerns are
> N/A — only the attestor's `node:crypto` RSA-PSS verifier remains.

---

## ✅ CRITICAL — F-1: Attested Arweave claim path does not bind the RSA modulus to the escrow's recipient

**Status: FIXED in commit `1611c13`.**

The fix adds a `recipient: <43-char base64url(sha256(recipient_pubkey))>`
line to the canonical message, derived on-chain from
`escrow.recipient_pubkey_active()` and on the attestor side from the
client-supplied modulus. Mismatched modulus → divergent canonical →
on-chain Ed25519 introspection rejects.

Touched: `programs/ario-ant-escrow/src/canonical.rs` (new
`derive_recipient_id_b64url`), all 6 claim ix handlers,
`migration/attestor/src/canonical.ts`, `sdk/src/solana/canonical-message.ts`,
`migration/solana-escrow-app/src/services/escrow-client.ts`. New regression
test `test_claim_ant_arweave_attested_rejects_wrong_modulus_in_canonical`
asserts the F-1 attack is rejected on-chain. Cross-tests (Rust ↔ TS)
regenerated.

---

### Affected paths

- `claim_ant_arweave_attested` (`programs/ario-ant-escrow/src/instructions/claim_arweave_attested.rs`)
- `claim_tokens_arweave_attested` (`.../claim_tokens_arweave_attested.rs`)
- `claim_vault_arweave_attested` (`.../claim_vault_arweave_attested.rs`)
- `migration/attestor/src/app.ts` `POST /attest`
- All SDK + frontend code that builds the attested-claim canonical and posts to the attestor

### What is supposed to happen

The deposit-time intent of an Arweave-recipient escrow is "release the asset only to whoever controls the RSA private key matching the modulus stored in `escrow.recipient_pubkey`." The legacy on-chain path (`claim_arweave.rs:70-71`) enforces this:

```rust
let modulus = escrow.recipient_pubkey_active();
verify_rsa_pss_sha256(&message, &signature, modulus, salt_len)?;
```

The signature must verify under the **stored** modulus.

### What actually happens in the attested path

The on-chain attested instructions reconstruct a canonical message that does **not** include the recipient's identity, only `(ant_mint, claimant, nonce)`:

```rust
// claim_arweave_attested.rs lines 73-78
let message = build_ant_escrow_claim_message(
    &escrow.ant_mint,
    &ctx.accounts.claimant.key(),
    &escrow.nonce,
);
verify_attested_signature(&ctx.accounts.instructions_sysvar, &message)?;
```

`verify_attested_signature` only checks `(ATTESTOR_PUBKEY, expected_message)`. The escrow's `recipient_pubkey` field is never read.

The off-chain attestor accepts a client-supplied `rsaModulusBase64Url` and verifies the sig under that modulus — **but the attestor has no way to know the on-chain `recipient_pubkey`** and the request does not pin it.

### Exploit (concrete)

Alice deposits an ANT into escrow with `recipient_pubkey = M_alice` (her Arweave wallet's RSA modulus). Eve wants to steal it.

1. Eve generates her own RSA-4096 keypair `(M_eve, K_eve)`.
2. Eve constructs the canonical message for Alice's escrow with **Eve's** Solana pubkey as `claimant`:
   ```
   ar.io ant-escrow claim
   network: solana-mainnet
   ant: <Alice's ANT mint>
   claimant: <Eve's Solana pubkey>      ← Eve substitutes her own
   nonce: <Alice's escrow nonce>         ← public, readable from the PDA
   ```
3. Eve signs that exact byte sequence with `K_eve`. Standard RSA-PSS-4096.
4. Eve posts to the attestor: `{ rsaModulusBase64Url: M_eve, rsaSignatureBase64Url: <her sig>, claimantBase58: <Eve>, antMintBase58: <Alice's ANT>, nonceHex: <Alice's nonce>, saltLength: 32 }`.
5. Attestor verifies the sig under `M_eve` — succeeds. Attestor signs the canonical with Ed25519.
6. Eve submits the claim tx. The on-chain `verify_attested_signature` verifies that ATTESTOR_PUBKEY signed exactly `(Alice's ANT, Eve's pubkey, Alice's nonce)` — succeeds.
7. The ANT is transferred to Eve's wallet.

**Severity:** CRITICAL. Anyone with HTTP access to the attestor can claim any Arweave-targeted escrowed ANT, ARIO tokens, or vault. The escrow program's recipient binding is reduced to a label.

### Why the legacy path is safe

The legacy `claim_*_arweave` ixs include the 512-byte signature in the on-chain ix data and verify against `escrow.recipient_pubkey` directly. Eve's sig under `M_eve` would not verify under the stored `M_alice`, so the on-chain check rejects.

The vulnerability is *unique* to the attested path because the on-chain code has been explicitly delegated to the attestor service — but the attestor was never given the binding to enforce.

### Required fix

**Option A (recommended): bind the Arweave address into the canonical message.**

The Arweave address of an RSA wallet is `base64url(sha256(modulus))`, deterministically derived. If the canonical message includes a `arweave_addr: <43-char base64url>` line, the attestor builds it from the **client-supplied** modulus and the on-chain code builds it from `sha256(escrow.recipient_pubkey_active())`. Mismatched modulus → divergent canonical → on-chain Ed25519 verify fails.

Touched code:
- `programs/ario-ant-escrow/src/canonical.rs` — append the `arweave_addr` line to both `build_ant_escrow_claim_message` and `build_escrow_claim_message`. Compute `sha256` on-chain (~3K CU).
- `migration/attestor/src/canonical.ts` — mirror.
- `migration/attestor/src/app.ts` — pass the modulus into the canonical builders.
- `sdk/src/solana/canonical-message.ts` — mirror (TS used for cross-tests; not used directly by attested-claim path but stays in sync).
- All cross-tests must be regenerated.

CU impact: +1 SHA-256 hash per claim (~3K CU). Tx-size impact: ~+55 bytes (label + address + newline). Both negligible — claim_*_arweave_attested still fits in the 200K default budget and 1232-byte tx limit.

**Option B (alternative): pass the modulus into the on-chain ix and compare to escrow state directly.**

Cost: +512 bytes tx data, ~+1K CU for memcmp. Tx still fits. Doesn't change the attestor at all. Slightly less elegant — the canonical message is no longer the sole binding — but more conservative because it requires no canonical-message version bump.

**Either fix MUST land before this PR can merge.** Both also require new integration tests that assert the attack is rejected.

---

## ✅ HIGH — F-2: Attestor service has no auth and per-IP rate limit only

**Status: PARTIALLY FIXED in code; OPS HARDENING DOCUMENTED in `migration/attestor/README.md`.**

Code changes (`migration/attestor/src/app.ts`):
- System-wide concurrent RSA-PSS verify cap (`MAX_CONCURRENT_VERIFIES`,
  default 10). Excess requests fast-reject 503 `BUSY` so upstream load
  balancers shed early instead of letting CPU saturate.
- Anomaly detection: rolling-window counter on `(arweave_address,
  escrow_key)` tuples; emits `level: warn` log line when the same
  tuple hits 5+ times per minute. Strong signal of nonce / claimant
  brute-forcing for a specific escrow.

Operational hardening that remains REQUIRED for any real-value deploy
(documented in the README under "REQUIRED operational hardening (F-2)"):
- WAF (Cloudflare / AWS WAF / fastly).
- Per-session auth via captcha challenge.
- Alarm on the structured anomaly log line.
- Box-level CPU caps via cgroup / k8s requests.

---

## ⚠️ HIGH — F-2 (original text)

### Where

`migration/attestor/src/app.ts:27-34`. Default 30 req/min/IP.

### Why it matters

Once the attestor service is reachable, any holder of a valid (RSA modulus, sig) for any escrow can issue valid Ed25519 attestations at will. The on-chain bug above (F-1) makes this catastrophic; even after F-1 is fixed, an open attestor:

- Lets attackers iterate offline (testing canonical-message variants)
- Allows DoS — an attacker with 1000 IPs (botnet, residential proxies, IPv6 ranges) can saturate
- Provides an oracle for which (escrow, claimant) combinations are well-formed

### Required mitigations (post F-1 fix)

1. Put the attestor behind Cloudflare or AWS WAF with rate limiting per session.
2. Optionally require a short-lived API key minted from a captcha challenge (proof-of-humanity).
3. Cap RSA-PSS verifies system-wide (not per-IP) at 10/sec to bound CPU.
4. Add structured anomaly logging — same `arweaveAddress` from N different IPs in M seconds is suspicious.

These are operational hardening, not code changes. They go into the attestor README's "Production deploy" section as required (not optional).

---

## ✅ MEDIUM — F-3: Attestor `decodeHex` accepts negative numbers in mid-string

**Status: FIXED in commit `1611c13`.** Strict `/^[0-9a-fA-F]*$/` pre-check
added to `decodeHex` in `migration/attestor/src/app.ts`.

---

## MEDIUM — F-3 (original text)

### Where

`migration/attestor/src/app.ts:283-292`.

```ts
function decodeHex(s: string): Uint8Array {
  if (s.length % 2 !== 0) throw new Error("hex string must be even length");
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) {
    const byte = parseInt(s.slice(i * 2, i * 2 + 2), 16);
    if (Number.isNaN(byte)) throw new Error(`invalid hex at offset ${i * 2}`);
    out[i] = byte;
  }
  return out;
}
```

`parseInt("-c", 16)` returns `-12`, which `Number.isNaN(-12)` rejects as **not** NaN, so the byte slot is set to `-12`. `Uint8Array` coerces that to `244`. So a hex like `"a-cd..."` decodes silently to garbage instead of erroring.

### Severity

Cosmetic in practice — the resulting nonce/assetId mismatch causes the attestor's canonical to diverge from the on-chain canonical, so the claim fails on-chain anyway. But the attestor would happily issue attestations for malformed inputs, returning success to a confused caller. Tighten to:

```ts
if (!/^[0-9a-fA-F]{2}$/.test(s.slice(i * 2, i * 2 + 2))) throw …
```

---

## ✅ MEDIUM — F-4: Test ATTESTOR_PUBKEY shipped in source

**Status: FIXED in commit `1611c13`.** Added a const-eval check in
`programs/ario-ant-escrow/src/state.rs` that panics at compile time if
`ATTESTOR_PUBKEY` equals the test bytes AND a real-network feature
(`network-mainnet` or `network-devnet`) is enabled AND the new
`unsafe-allow-test-attestor-pubkey` escape hatch feature is NOT enabled.
Tests use the escape hatch (default-on for non-SBF builds);
`build-sbf.sh` now opts out via `--no-default-features --features
network-{mainnet,devnet}` so SBF deploys cannot ship the test value
through any path. Defense in depth alongside the existing
`check-attestor-pubkey.sh --strict` shell guardrail.

---

## MEDIUM — F-4 (original text)

### Where

`programs/ario-ant-escrow/src/state.rs:60` — the constant is the deterministic test value `AKnL4NN…` derived from public seed `[1u8; 32]`.

### Why it's a finding

The deploy guardrail (`contracts/scripts/check-attestor-pubkey.sh --strict`) is wired into `devnet-deploy.sh` Phase 0 only. Other deploy paths (manual `solana program deploy`, custom scripts, future `mainnet-deploy.sh`) bypass it. Anyone reading `state.rs` can derive the secret seed and mint valid attestations.

### Required mitigation

1. Move the guardrail call into a pre-build hook (`build-sbf.sh --strict-attestor-pubkey`) so any path that produces a deployable `.so` is gated.
2. Or: refuse to compile when both `network-mainnet`/`network-devnet` is set AND `ATTESTOR_PUBKEY == TEST_VALUE` via a `compile_error!()` in `state.rs`.

Pinning at compile-time is the more robust answer — guardrails in shell scripts don't survive contributors who use a different deploy command.

---

## ✅ LOW — F-5: Multiple Ed25519Program ixs in one tx are not enforced

**Status: FIXED in commit `1611c13` via documentation + regression test.**
The behavior was correct already; the gap was that the position-pinning
invariant wasn't documented as load-bearing. Strengthened doc comment in
`programs/ario-ant-escrow/src/verify/attested.rs` marks the
"Ed25519Program ix at `claim_ix_index - 1`" rule as a hard invariant
with the threat model spelled out. Regression test
`test_claim_ant_arweave_attested_rejects_sigverify_not_immediately_preceding`
asserts the position-pin holds against `[Ed25519, no-op-spacer, claim]`.

---

## LOW — F-5 (original text)

### Where

`verify/attested.rs:90-101` only inspects `current_ix_idx - 1`.

### Why it isn't worse

Other Ed25519Program ixs elsewhere in the tx are simply ignored. Each `claim_*_attested` ix demands its OWN preceding sigverify ix, and the canonical-message + nonce binding ties each pair together. A malicious tx with `[Ed25519(msg_for_A), claim_A, Ed25519(msg_for_B), claim_B]` works as expected — both claims succeed against their matching sigs. A reordered `[Ed25519(msg_for_A), Ed25519(msg_for_B), claim_A]` makes claim_A look at the second Ed25519, which contains B's canonical → mismatch → rejected.

### Why it's still worth flagging

If a future change relaxes the position check (e.g., "find any Ed25519Program ix in the tx with matching pubkey"), the per-claim binding could be lost. Document the position-pinning as a hard invariant in the module-level docs of `verify/attested.rs` and add a test that constructs `[Ed25519(msg_for_A), Ed25519(msg_for_B), claim_A]` and asserts it rejects.

---

## LOW — F-6: Frontend logs canonical message bytes via setClaimMessage

### Where

`migration/solana-escrow-app/src/pages/ClaimPage.tsx` setClaimMessage paths surface raw error strings from the attestor and SDK back into the UI.

### Severity

Information disclosure to the user themselves (no privilege boundary). A network operator or browser extension can already see the canonical message in plaintext via the `/attest` POST. No fix required, but don't let production builds enable verbose debug logs that include attestation responses with the user's RSA modulus.

---

## NOT-AN-ISSUE checks (verified clean)

- ✅ Bounds checks in `verify/attested.rs` use `checked_add` for all three offset+len computations; `pk_end <= data.len() && sig_end <= data.len() && msg_end <= data.len()` is checked before any indexing.
- ✅ All three `*_ix_index` fields must equal `0xFFFF` (DATA_IN_SAME_IX). Cross-instruction signature/pubkey/message references are rejected.
- ✅ `sigverify_ix.program_id == ED25519_PROGRAM_ID` is verified — a forged ix carrying matching layout but different program-id is rejected.
- ✅ `data[NUM_SIGS_OFFSET] == 1` ensures only single-sig sigverify ixs are accepted; bundling many sigs into one ix to confuse pubkey/message lookup is blocked.
- ✅ `data[PADDING_OFFSET] == 0` rejects layout variants with non-zero padding.
- ✅ `current_ix_idx > 0` prevents the claim ix from being at position 0 (which would underflow the `idx - 1` lookup).
- ✅ Express body limit `16kb` prevents memory-exhaustion via huge JSON.
- ✅ `BigInt(amount)` rejects floats and overflow; explicit `amount > 0xFFFF_FFFF_FFFF_FFFFn` check enforces u64.
- ✅ Network value validated against allowlist `["solana-mainnet", "solana-devnet", "localnet"]` at config load.
- ✅ Canonical message has no characters that could inject CRLF or HTTP smuggling — base58 + lowercase hex + fixed labels only.
- ✅ Replay protection layered: claimant pubkey in canonical, nonce rotated on `update_recipient`, PDA closed on claim.
- ✅ Cross-network replay prevented via the `network` field in the canonical and the compile-time feature gate.
- ✅ TS attestor canonical bytes ↔ Rust on-chain canonical bytes pinned by cross-tests in both directions.
- ✅ Attestor secret never logged; only `arweaveAddress` (= sha256(modulus) base64url) appears in logs.

---

## Recommended action

1. **Block PR #109 from merging.**
2. **Land the F-1 fix on this branch** before any further review or merge consideration:
   - Add `arweave_addr` line to both Rust canonical builders.
   - Mirror in the attestor TS canonical.
   - Pass the modulus into the attestor's canonical-builder calls.
   - Update SDK + frontend canonical builders for parity (these are tested by the cross-test, even though they're not on the attested-claim path itself).
   - Regenerate the cross-test golden vectors.
   - Add a new test to `programs/ario-ant-escrow/tests/integration.rs` named `attested_claim_with_wrong_modulus_fails` that constructs a valid Ed25519 attestation over a canonical built from a non-stored modulus and asserts the on-chain claim rejects with `AttestationMessageMismatch`.
3. **Add F-4's compile-time guard** so the test ATTESTOR_PUBKEY cannot be deployed by any path.
4. **Tighten F-3's hex parser** — small change, free.
5. **Document F-2's required ops hardening** in the attestor README under "Production deploy" and mark it as REQUIRED, not optional.
6. Re-run all test suites after the fix. Add regression tests from F-1 and F-5.
7. Re-review.

Estimated effort to remediate: 4-8 hours including tests and cross-toolchain regeneration.
