# docs/archive — Superseded documentation

When a doc in [`docs/`](..) gets replaced by a newer one (or its
contents land in code / IDLs / on-chain config and the doc no longer
needs to exist), it moves here rather than being deleted.

## Why

* External trackers (audit reports, GitHub issues, search engines,
  pinned chat messages, the public AR.IO blog) link to specific files
  in `docs/`. Deleting them produces silent 404s.
* Historical context is load-bearing for the next round of audits and
  for the next porting / refactoring sprint — the *reasons* something
  was discarded are as valuable as the current design.
* Comparing "what we shipped" against "what we considered" is one of
  the fastest ways to onboard a new engineer.

## When to archive

Archive a doc once **all four** are true:

1. A successor doc exists, or the content has been absorbed into
   another canonical location (an ADR in [`adrs/`](../adrs/), a code
   comment, [`docs/EVENTS.md`](../EVENTS.md), an IDL, etc.).
2. The successor has been merged for at least one release cycle on
   `develop` and survived one mainnet upgrade unchanged.
3. No active PR / sprint / audit task references the old doc.
4. The old doc is no longer accurate as written.

## How to archive

1. Move the file: `git mv docs/FOO.md docs/archive/FOO.md`. **Don't
   rename it** — preserving the original filename is what keeps
   external links resolving.
2. Edit the archived copy to add a banner at the very top:

   ```markdown
   > [!WARNING]
   > **Archived <YYYY-MM-DD>.** This document is preserved for
   > historical reference only. The current source of truth is
   > [`docs/<successor>.md`](../<successor>.md) (or `<other location>`).
   > Do not treat statements below as binding — sections may have been
   > superseded or refuted by later decisions.
   ```

3. Update inbound links:
   * `git grep -F 'docs/FOO.md'` from the repo root.
   * Replace each match with `docs/archive/FOO.md` (or, if the new
     location supersedes, point at that instead).
4. Add a row to the table below.
5. If the doc captured an ADR-style decision, also drop a forward-link
   entry in [`docs/adrs/README.md`](../adrs/README.md) under
   "Historical decisions" so future authors see the lineage.

## Index

| Archived | File | Successor / reason |
|----------|------|--------------------|
| _empty_  |      |                    |

(Once entries exist, sort newest-first.)
