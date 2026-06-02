# Architecture Decision Records (ADRs)

This directory holds the [MADR](https://adr.github.io/madr/) (Markdown
Any Decision Records) for AR.IO's Solana contracts. **One file per
decision**, monotonic four-digit numbering, append-only once merged to
`develop`.

* **Template:** [`0000-template.md`](0000-template.md). Copy it, pick
  the next free number from the table below, and submit alongside the
  PR that implements (or proposes implementing) the decision.
* **Format:** strict MADR — sections in the documented order so the
  index is skimmable. Don't invent new top-level sections unless you're
  also updating the template.
* **Numbering:** four-digit, zero-padded (`NNNN-…md`). Increment from
  the highest number in this directory; never reuse a number, never
  skip one. _(See "Historical decisions" below for why we restarted at
  `0001` here even though `DECISIONS.md` got to `ADR-018`.)_

## How to add a new ADR

1. **Pick the next free number.** Look at the table below and the
   filenames in this directory; pick `max + 1`, zero-padded to four
   digits.
2. **Copy the template.**
   ```bash
   cp docs/adrs/0000-template.md docs/adrs/NNNN-kebab-case-title.md
   ```
3. **Author.** Status starts at `proposed`. Don't change the number
   after the first review pass — if the discussion changes the scope
   substantially, supersede instead.
4. **Wire references.** Update this README (add a row to the table),
   any [`CLAUDE.md`](../../CLAUDE.md) / [`README.md`](../../README.md)
   sections that link to the relevant decision, and any code that
   should cite the ADR in a comment or doc string.
5. **Land it.** Once merged to `develop`, status flips to `accepted`
   (or whatever the agreed terminal state is). After that, the body is
   append-only — only the Status line is editable, and only to mark
   "superseded by ADR-XXXX" with a forward link.

## How to supersede an ADR

Adding a follow-up decision that replaces an earlier one:

1. Write the new ADR. Title it for the *new* decision (not "supersede
   ADR-XXXX"), set Status to `accepted`, and reference the old ADR in
   the **Related** section.
2. Edit the old ADR's Status line in the same PR:
   ```
   * **Status:** superseded by [ADR-XXXX](XXXX-…md) (YYYY-MM-DD)
   ```
3. Do **not** delete or rewrite the old body. Future readers need to
   see the original framing to understand why the decision was
   reversed.
4. If the old ADR ships in this directory, leave it here. If it lives
   in `DECISIONS.md` (see "Historical decisions"), edit the Status
   inline in `DECISIONS.md`.

## Index

| #     | Title | Status | File |
|-------|-------|--------|------|
| _empty — first MADR-format ADR will be `0001`_ | | | |

## Historical decisions

Decisions ADR-001 through ADR-018 predate this folder. They live in
[`../DECISIONS.md`](../DECISIONS.md) as a single file. They are
authoritative — treat them exactly the same as a MADR file in this
directory — but format-wise they're free-form rather than strict MADR.

| # (legacy) | Title | Status | Source |
|-----------:|-------|--------|--------|
| ADR-001 | Target Platform Selection | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-002 | ANT Architecture | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-003 | State Migration Strategy | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-004 | RSA Wallet Migration | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-005 | Epoch Automation | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-006 | Program Architecture | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-007 | Indexer Selection | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-008 | Multi-sig and Upgrade Authority | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-009 | Import-Then-Claim Migration Strategy | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-010 | Migration Authority Model | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-011 _(a)_ | On-chain ANT ACL Registry + MPL Core Lifecycle Hook | **superseded** by ADR-012 _(a)_ | [`DECISIONS.md` line 388](../DECISIONS.md) |
| ADR-012 _(a)_ | Paginated per-user ANT ACL | accepted | [`DECISIONS.md` line 529](../DECISIONS.md) |
| ADR-011 _(b)_ | ArNS Rebrand and Multi-Protocol Resolution | proposed _(numbering collision — see "Numbering anomalies")_ | [`DECISIONS.md` line 720](../DECISIONS.md) |
| ADR-012 _(b)_ | ANT NFT Marketplace Metadata Architecture | accepted _(numbering collision — see "Numbering anomalies")_ | [`DECISIONS.md` line 780](../DECISIONS.md) |
| ADR-013 | ANT Authority Lifecycle (Owner / UpdateAuthority / Plugin / Program) | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-014 | Trustless ANT Escrow Program (Multi-Protocol Signature Verification) | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-015 | Mint Authority Revoked at End of Migration | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-016 | Pluggable ANT Program via Asset Attributes Plugin | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-017 | Off-Chain Attestor for Arweave RSA-PSS Signature Verification | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-018 | Anchor `#[event]` ABI Policy (Pre-Release Append-Only, Frozen at Mainnet Cutover) | accepted | [`DECISIONS.md`](../DECISIONS.md) |
| ADR-020 | Schema-Migration Loading — Grow-Then-Deserialize, Append-Only Versioning | accepted | [`0020-schema-migration-grow-then-deserialize.md`](0020-schema-migration-grow-then-deserialize.md) |
| ADR-021 | Escrow Vault Re-locks Are Non-Revocable | accepted | [`0021-escrow-vault-relocks-non-revocable.md`](0021-escrow-vault-relocks-non-revocable.md) |
| ADR-022 | Disable the Escrow Active-Vault Re-lock Path | accepted | [`0022-escrow-disable-active-vault-relock.md`](0022-escrow-disable-active-vault-relock.md) |
| ADR-023 | `prescribe_epoch` Roulette Modulus Is the Live Registry Sum | accepted | [`0023-prescribe-live-total-weight.md`](0023-prescribe-live-total-weight.md) |
| ADR-024 | Retire `devnet-shrunk`, Standardize Full-Size Registries | accepted | [`0024-retire-devnet-shrunk-standardize-full-size-registries.md`](0024-retire-devnet-shrunk-standardize-full-size-registries.md) |

### Numbering anomalies

`DECISIONS.md` has two cases where the same ADR number was reused for
unrelated decisions:

* **ADR-011** appears at lines 388 and 720. The first (ACL registry,
  2026-04-17) is superseded; the second (ArNS rebrand, 2026-04-20) is
  still active.
* **ADR-012** appears at lines 529 and 780. The first (paginated ACL,
  2026-04-27) is the active ACL design; the second (NFT marketplace
  metadata, 2026-04-21) is also active.

To resolve this when the historical ADRs are migrated to MADR files in
this directory: assign each duplicate a fresh four-digit number based
on its `Date:` field, leave a redirect note in `DECISIONS.md`, and
update inbound links. The "_(a)_ / _(b)_" suffixes in the table above
are temporary disambiguation only.

### Migration plan _(future cleanup, not blocking)_

When time permits, port `ADR-001` … `ADR-018` from `DECISIONS.md` into
this directory:

1. For each ADR, create `NNNN-kebab-case-title.md` from the template.
2. Reformat the body into the MADR section structure where it doesn't
   already match.
3. For the duplicate-numbered entries, pick fresh numbers (the
   chronological ordering by `Date:` is the cleanest tiebreaker).
4. Replace the corresponding section in `DECISIONS.md` with a stub:
   ```
   ## ADR-NNN: <Title>
   Migrated to [`adrs/NNNN-…md`](adrs/NNNN-…md). Original revision
   history available via `git log`.
   ```
5. Update this README's tables; remove the row from "Historical
   decisions" once it has a real MADR entry.
6. After all 21 entries land here, archive `DECISIONS.md` per the
   policy in [`../archive/README.md`](../archive/README.md).
