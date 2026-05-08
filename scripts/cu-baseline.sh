#!/usr/bin/env bash
# CU baseline capture for event-emission regression tracking.
#
# Runs `cargo test` for one or more programs in BPF mode with verbose
# logs, parses each "Program <id> consumed N of M compute units" line,
# and writes the result to contracts/cu-baseline.json keyed by
# (program, test_name). Each event-emission PR (PR-1..6) re-runs this
# and the diff lands in the PR description so reviewers can see the CU
# delta from new emit!() sites.
#
# This is a tool, not a CI gate. The +5% regression guardrail in the
# implementation plan is enforced at PR review time by reading
# `bash contracts/scripts/cu-baseline.sh --diff` output.
#
# Usage:
#   bash scripts/cu-baseline.sh                    # capture for all 5 programs
#   bash scripts/cu-baseline.sh ario-core          # one program
#   bash scripts/cu-baseline.sh --diff             # compare current vs baseline
#   bash scripts/cu-baseline.sh --diff ario-arns
#
# Requires: a fresh BPF build (run `bash build-sbf.sh` first).
# Sets BPF_OUT_DIR for the test run so events + emit-count CU costs flow
# through the real syscall path.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONTRACTS_DIR="$REPO_ROOT"
BASELINE_PATH="$REPO_ROOT/cu-baseline.json"
ALL_PROGRAMS=(ario-core ario-gar ario-arns ario-ant ario-ant-escrow)

DIFF_MODE=false
TARGET_PROGRAMS=()
for arg in "$@"; do
  case "$arg" in
    --diff) DIFF_MODE=true ;;
    -h|--help)
      sed -n '2,/^set/p' "$0" | sed -n 's/^# \?//p'
      exit 0
      ;;
    *) TARGET_PROGRAMS+=("$arg") ;;
  esac
done

if [[ ${#TARGET_PROGRAMS[@]} -eq 0 ]]; then
  TARGET_PROGRAMS=("${ALL_PROGRAMS[@]}")
fi

if [[ ! -d "$REPO_ROOT/target/deploy" ]]; then
  echo "ERROR: $REPO_ROOT/target/deploy missing." >&2
  echo "Run 'bash build-sbf.sh' first." >&2
  exit 1
fi

export BPF_OUT_DIR="$REPO_ROOT/target/deploy"

# tmp file we'll merge per-program results into
TMP_RESULTS=$(mktemp)
trap 'rm -f "$TMP_RESULTS"' EXIT
echo '{}' > "$TMP_RESULTS"

for program in "${TARGET_PROGRAMS[@]}"; do
  echo ">>> capturing CU for $program (BPF)..."
  # Run tests, capture full log including stable_log lines.
  log_file=$(mktemp)
  if ! (cd "$CONTRACTS_DIR" && cargo test -p "$program" -- --nocapture 2>&1) > "$log_file"; then
    echo "WARN: tests for $program failed; baseline for that program will be incomplete." >&2
  fi

  # Extract "Program data:" emit lines and the matching "consumed N of M" line.
  # We keyed by test name (the line preceding `running ...` style output) so
  # each instruction's CU is tracked in isolation.
  python3 - "$program" "$log_file" "$TMP_RESULTS" <<'PY'
import json, re, sys

program, log_path, out_path = sys.argv[1], sys.argv[2], sys.argv[3]

# Parse pattern: "test test_<name> ... ok\n" (test_runner output) interleaved
# with "Program <id> consumed <cu> of <max> compute units" (stable_log).
test_pat = re.compile(r"^test ([\w:]+) \.\.\. (ok|FAILED)")
cu_pat   = re.compile(r"Program \S+ consumed (\d+) of \d+ compute units")

current_test = None
samples = {}  # test_name -> [cu, cu, ...]
with open(log_path, errors="replace") as f:
    for line in f:
        m = test_pat.match(line)
        if m:
            current_test = m.group(1)
            samples.setdefault(current_test, [])
            continue
        m = cu_pat.search(line)
        if m and current_test:
            samples[current_test].append(int(m.group(1)))

# Aggregate: max CU per test (the "worst" instruction in that test
# is what indicates regression risk).
result = {}
for name, cus in samples.items():
    if not cus: continue
    result[name] = {"max_cu": max(cus), "samples": len(cus)}

with open(out_path) as f: existing = json.load(f)
existing[program] = result
with open(out_path, "w") as f: json.dump(existing, f, indent=2, sort_keys=True)
print(f"  -> {len(result)} tests captured")
PY
  rm -f "$log_file"
done

if $DIFF_MODE; then
  if [[ ! -f "$BASELINE_PATH" ]]; then
    echo "No baseline to diff against ($BASELINE_PATH missing)." >&2
    exit 2
  fi
  python3 - "$TMP_RESULTS" "$BASELINE_PATH" <<'PY'
import json, sys
cur = json.load(open(sys.argv[1]))
base = json.load(open(sys.argv[2]))
threshold = 0.05  # 5% regression budget

worst = []
for prog, tests in cur.items():
    base_tests = base.get(prog, {})
    for name, c in tests.items():
        b = base_tests.get(name)
        if b is None:
            print(f"NEW: {prog}::{name}  cu={c['max_cu']} (no baseline)")
            continue
        delta = c['max_cu'] - b['max_cu']
        pct = delta / b['max_cu'] if b['max_cu'] else 0
        if abs(pct) >= 0.01:
            sym = "+" if delta > 0 else ""
            tag = "  REGRESSION" if pct > threshold else ""
            print(f"{prog}::{name}  {b['max_cu']} -> {c['max_cu']}  ({sym}{delta} CU, {sym}{pct*100:.1f}%){tag}")
            if pct > threshold: worst.append((prog, name, pct))
sys.exit(1 if worst else 0)
PY
else
  cp "$TMP_RESULTS" "$BASELINE_PATH"
  echo
  echo "Wrote baseline to $BASELINE_PATH"
  echo "Commit it alongside event-emission PRs and run with --diff to track regressions."
fi
