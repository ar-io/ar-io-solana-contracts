#!/usr/bin/env bash
# Point this repo at version-controlled hooks under .githooks/ (no
# Husky / npm — same pattern as scripts/*.sh elsewhere).
#
#   bash scripts/install-git-hooks.sh
#
# This sets repo-local:  git config core.hooksPath .githooks
# (relative to the work tree). It overrides .git/hooks for this clone
# only; re-run after a fresh clone.

set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
cd "$ROOT"

git config core.hooksPath .githooks

for f in "$ROOT"/.githooks/*; do
	[[ -f "$f" ]] || continue
	chmod +x "$f"
done

echo "Git hooks path set to .githooks for this repository."
echo "Active pre-push checks: cargo fmt --check, cargo clippy (-D warnings)."
echo "Skip on a single push: AR_IO_SKIP_PREPUSH=1 git push ..."
