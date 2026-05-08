# ADR-NNNN: <Short noun-phrase title>

* **Status:** proposed | accepted | rejected | deprecated | superseded by [ADR-XXXX](XXXX-...md)
* **Date:** YYYY-MM-DD
* **Deciders:** @handle, @handle
* **Consulted:** _(optional)_ @handle, @handle
* **Informed:** _(optional)_ @handle, @handle

> **TL;DR:** _(One sentence — what's the decision and why.)_

## Context and problem statement

What forces are at play, what is the problem, and what are the
constraints? Cite source code, prior ADRs, and external research
(audit reports, Anchor / Solana docs, AO / Lua reference) by file +
line / URL. Be specific about the assumptions baked into the framing —
those are the first things to revisit if the decision needs to be
reopened later.

## Decision drivers

* _(driver 1 — e.g. "must run within 1.4M CU per call")_
* _(driver 2 — e.g. "downstream SDK consumers should not need to
  re-derive PDAs")_
* …

## Considered options

1. **Option A — \<one-line summary\>**
2. **Option B — \<one-line summary\>**
3. **Option C — \<one-line summary\>** (do nothing / status quo)

## Decision

> _The chosen option, in one paragraph._

Justification, in concrete terms — quote the decision drivers above and
explain how the chosen option satisfies them better than the rest. If
the decision is reversible, say what would trigger reopening it.

## Consequences

### Positive

* …

### Negative / risks

* …

### Neutral

* _(things that change but are neither wins nor losses, e.g. an extra
  CPI hop that doesn't move the CU needle but is now part of the
  surface area)_

## Implementation notes

_(Optional.)_ Concrete next steps, owners, target sprint, related
issues / PRs. Delete the section if the ADR is purely a posture choice
without follow-up work.

## Related

* Code: `programs/<crate>/src/<file>.rs`
* Docs: [`docs/<…>.md`](../…)
* Behavioral diff entry: BD-NNN
* Audit finding: \<id\>
* External: <links>

---

<!--
Authoring notes (delete before merging):

* MADR (https://adr.github.io/madr/) format. Don't reorder sections —
  the consistency makes the index easier to skim.
* Numbering is monotonic, four-digit, zero-padded. Pick the next free
  number from `docs/adrs/README.md`.
* File name: `NNNN-kebab-case-title.md`. Match the title in the H1.
* Once an ADR ships on a release branch (develop or main), its body is
  append-only. Status changes ("superseded by …") are the only allowed
  edit. To overturn a decision, write a new ADR that supersedes it.
* If this ADR overturns or supersedes another, update the older ADR's
  Status line + add a "Superseded by" link, in the same PR.
* If this ADR is contracts-internal, end of story. If it has SDK /
  migration / downstream impact, also link the relevant external repo
  issue / PR so the change is discoverable from there.
-->
