# ADR-0035: Anchor Error Codes Are a Published, Append-Only ABI

* **Status:** proposed (2026-09-16; guard implemented in the same PR)
* **Date:** 2026-09-16
* **Deciders:** @vilenarios
* **Consulted:** post-incident review of mainnet epochs 523 and 540; [PR #128](https://github.com/ar-io/ar-io-solana-contracts/pull/128) review
* **Informed:** cranker/observer maintainers, SDK consumers, AR.IO gateway operators

> **TL;DR:** Anchor derives error codes positionally (`6000 + variant index`),
> so inserting a variant mid-enum silently renumbers every later code. Off-chain
> consumers match on those numbers, and this has already cost the network twice.
> Error codes are hereby a published ABI on the same footing as event layouts
> (ADR-018): append-only, never inserted, reordered, or deleted — enforced by
> `scripts/error-code-snapshot.mjs` in the PR-time CI job.

## Context and problem statement

`#[error_code]` assigns each variant `6000 + its index in the enum`. The number
appears nowhere in the source. A reviewer reading a diff sees only:

```rust
  #[msg("Unknown schema version — no migration path exists")]
  UnknownSchemaVersion,

+ #[msg("Gateway predates the 1.1.0 layout and cannot be migrated in place")]
+ PreV110GatewayLayout,
+
  // DELEGATION LIFECYCLE ERRORS
```

Four added lines. What actually happens is that **every variant after the
insertion point shifts by one**, and each shifted code silently changes meaning
for every off-chain consumer that matches on it.

Those consumers are load-bearing, not incidental. Both the cranker
(`src/errors.ts`) and the observer (`src/epoch/errors.ts`) branch on numeric GAR
codes to decide whether a failed epoch tick is a retryable no-op or a genuine
failure. Getting that classification wrong means either a hot retry loop or a
silently swallowed fault.

### This has already happened twice

**Mainnet epoch 523 — realised loss.** The observer's error table drifted two
codes behind the deployed program. Every observation submission in that epoch
was misclassified and lost. The regression comment in
`ar-io-observer/src/epoch/errors.test.ts` still records it.

**PR #128 — caught in review, hours from merging.** That PR inserts
`PreV110GatewayLayout` after `UnknownSchemaVersion`, at variant index 89 of 99.
Ten variants renumber. Two of them had gone live on mainnet `ario_gar` that
same day (slot 447280441):

| variant | code before | code after #128 |
|---|---|---|
| `EpochWeightsClobbered` | 6097 | **6098** |
| `EpochNoLongerLive` | 6098 | **6099** |

The `6097 → 6098` shift is worse than drift — it is a **silent semantic
collision**. A cranker or observer on `@ar.io/sdk` 4.3.1 /
`@ar.io/solana-contracts` 1.3.0 that receives 6098 reports `EpochNoLongerLive`
while the program means `EpochWeightsClobbered`. Both codes are legitimate, both
decode without error, and the two call for opposite operational responses.

### The rule existed but was unwritten and unenforced

The constraint was already understood — it appears as prose in at least three
places:

* [`0024-retire-devnet-shrunk-standardize-full-size-registries.md`](0024-retire-devnet-shrunk-standardize-full-size-registries.md):
  "error variants are positional"
* `docs/ANT_ESCROW_PROTOCOL_SPEC.md`: "must not be reordered without a major
  version bump"
* `docs/FIX_PLANS.md`: "keep existing error codes stable"

But there was no ADR making it policy, and **nothing mechanical enforced it**.
`idl-event-snapshots.json` covers events only; `scripts/idl-event-snapshot.mjs`
never inspects `idl.errors`. Events got a policy (ADR-018), a snapshot, and a
check. Error codes got three sentences scattered across unrelated documents.

## Decision drivers

* The failure is silent, and its blast radius is every off-chain consumer.
* Review cannot be the control. The renumbering is invisible in the diff; #128
  was caught only because an unrelated incident had just put those two specific
  codes in working memory.
* The check must run **at PR time**. Discovering a broken ABI during a release
  or a mainnet upgrade is discovering it too late.
* It must not slow the fast PR job, which deliberately does no `anchor build`.

## Considered options

1. **Document the rule only** — an ADR, no automation.
2. **Snapshot the error tables and check in CI** (chosen).
3. **Pin codes explicitly in source**, e.g. `#[error_code(offset = …)]` or
   manual discriminants.
4. **Extend `idl-event-snapshot.mjs`** to cover `idl.errors` alongside events.

## Decision

**Option 2.** `scripts/error-code-snapshot.mjs` snapshots the `code -> name`
binding for all four deployed programs into `error-code-snapshots.json`
(256 codes today) and fails CI on any reassignment.

Permitted: appending new variants to the **end** of an enum (codes above the
snapshot high-water mark). Rejected: a code whose name changed, a variant that
moved, a code that vanished, and a new code appearing below the high-water mark.

To retire a variant, **keep it** so its code stays bound to its name, and stop
returning it. Deleting it renumbers everything after.

**It reads Rust source, not the IDL.** The ordering rule is purely syntactic, so
no Anchor/Solana toolchain is needed. That is what lets the check run in
`build-test.yml`, which has no `anchor build` step — the whole point being
PR-time feedback. The parser was validated by reproducing all four programs'
IDL error tables exactly (256/256 codes, identical order) at the commit that
introduced it.

**Option 3 is rejected** because Anchor 0.31 offers no per-variant code
attribute; the offset applies to the whole enum. Manual discriminants on
`#[error_code]` are not supported.

**Option 4 is rejected on coupling.** The two invariants are different in kind —
events are keyed by *name* and additions are positionally free, whereas error
codes are keyed by *position* and additions are only safe at the tail. Folding
both into one script would mean two snapshot formats and two comparison models
behind one entry point. More importantly, the event check requires built IDLs
and therefore cannot run in the fast PR job, so merging them would drag the
error check out of PR-time CI — losing the property that motivated this ADR.

## Consequences

### Positive

* A #128-class renumbering now fails CI on the PR, naming every affected code
  and where each variant moved to.
* The rule is written down once, in policy form, instead of inferred from three
  scattered sentences.
* Zero added toolchain and negligible runtime in the PR job.

### Negative / risks

* **The snapshot must be blessed when appending.** Adding a variant requires
  `--update` in the same PR. On its own that is a hole: `--update` would happily
  bless a *bad* diff, and a PR could delete or truncate the snapshot to disable
  the guard while still reporting success. Relying on reviewers to notice would
  contradict this ADR's own premise that review cannot be the control, so the
  snapshot's integrity is checked mechanically instead:
  * a missing snapshot, or one missing any guarded program's entry, **fails**
    rather than passes;
  * on pull requests CI passes `--baseline <target branch's snapshot>`, and the
    committed snapshot must **extend** it — preserving every existing
    `code -> name` as an exact prefix, with additions only at the tail.
* **Source parsing is a second implementation of Anchor's numbering.** It is
  validated against the IDLs at introduction and re-validated advisorily
  whenever built IDLs are present, but it is not the compiler. A future Anchor
  change to code derivation would need this script updated.
* **Escrow is deliberately unguarded.** `ario_ant_escrow` is excluded from
  `PROGRAMS`: it is never deployed to any cluster, so its codes have no
  consumers and guarding them would only produce false failures. If it ever
  ships, it must be added.

### Neutral

* The 256 codes snapshotted today are a *baseline*, not an audit. They record
  what the bindings currently are; they do not assert that the current numbering
  matches every already-deployed program. Consumers pinned to older contract
  clients remain the SDK's versioning problem, not this guard's.

## Implementation notes

* Check: `node scripts/error-code-snapshot.mjs`
* Check + snapshot integrity: `... --baseline <target's snapshot>`
* Bless an append: `node scripts/error-code-snapshot.mjs --update`
* Wired into `.github/workflows/build-test.yml` (runs on every PR to
  `main`/`develop`); the workflow resolves the baseline from
  `github.event.pull_request.base.sha`.

  The workflow deliberately distinguishes *why* a baseline is unavailable,
  because the two reasons must not look alike:

  * **base commit unresolvable** (not fetchable on the runner) — the integrity
    comparison cannot run, so the step **fails loudly**. Degrading to a plain
    check here would leave a guard that reports success while enforcing
    nothing, which is the exact failure mode this ADR exists to prevent.
  * **base commit resolved but carries no snapshot** — legitimately not
    applicable; expected only for the PR that introduces the file. Prints a
    note and proceeds.
  * **no PR context** (`workflow_dispatch` / `workflow_call`) — no base SHA
    exists; plain check.

### A related gap this ADR does not close

While wiring the above it emerged that the **event** ABI check has the same
timing problem: `scripts/idl-event-snapshot.mjs` is invoked only from
`release.yml` and `upgrade-mainnet.yml` — never on a pull request. CLAUDE.md
claimed it ran in `build-test.yml`; that was inaccurate and is corrected in this
PR. Moving the event check to PR time requires an `anchor build` (and therefore
the Anchor/Solana toolchain) in a job that deliberately avoids both, so it is
left as follow-up rather than bundled here.

## Related

* [ADR-018 — Anchor `#[event]` ABI policy](../DECISIONS.md) — the precedent this
  mirrors for events.
* [ADR-0032](0032-distribution-skips-untallied-gateways.md) and
  [ADR-0033](0033-epoch-weights-are-destroyed-by-the-next-tally.md) — introduced
  `EpochWeightsClobbered` (6097) and `EpochNoLongerLive` (6098), the two live
  codes #128 would have renumbered.
* [PR #128](https://github.com/ar-io/ar-io-solana-contracts/pull/128) — the
  motivating near-miss; reworked to append rather than insert.
