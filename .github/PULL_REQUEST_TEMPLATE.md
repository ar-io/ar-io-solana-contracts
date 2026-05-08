<!--
Thanks for the patch! Tick the items that apply; delete the rest.
-->

## Summary

<!-- One or two sentences explaining what this PR does and why. -->

## Changes

- [ ] Adds / changes a program instruction
- [ ] Adds / changes an Anchor `#[event]` (see [`docs/EVENTS.md`](../docs/EVENTS.md) and ADR-017)
- [ ] Adds / changes a PDA layout or account size (run `cargo test` + check `docs/COMPUTE_AND_LIMITS.md`)
- [ ] Adds a CPI between programs (update the CPI graph in `README.md`)
- [ ] Touches the off-chain attestor protocol (`programs/ario-ant-escrow/src/canonical.rs`)
- [ ] Tooling / CI / scripts only

## Test plan

- [ ] `anchor build` clean
- [ ] `cargo test --workspace` green
- [ ] `BPF_OUT_DIR=$(pwd)/target/deploy cargo test --workspace` green (event tests)
- [ ] `node scripts/idl-event-snapshot.mjs` (no event ABI regressions; ran `--update` if intentionally adding events)
- [ ] If touching a CPI / size / CU-heavy path: `bash scripts/cu-baseline.sh --diff` summary attached

## Behavioral diff / ADR

If this changes protocol behavior vs the AO Lua source, link the BD-XXX
entry in [`docs/BEHAVIORAL_DIFFERENCES.md`](../docs/BEHAVIORAL_DIFFERENCES.md).
For architectural decisions, add a MADR file under
[`docs/adrs/`](../docs/adrs/) (workflow in
[`adrs/README.md`](../docs/adrs/README.md)).

## Deployment notes

<!--
Anything ops needs to know for the devnet auto-deploy and the eventual
mainnet upgrade — e.g. "requires a follow-up `update_settings` ix",
"new account types — clients need to upgrade before this lands",
"breaking event change — coordinate with indexers".
-->
