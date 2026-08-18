# ADR-0029: Epoch Rent Refunds the Creator, Not the Closer

- Status: accepted
- Date: 2026-08-17
- Deciders: protocol engineering
- Related: gar #116 (`close_observation` refunds the observer — the precedent
  this generalises), audit M8 (`close_epoch` requires every observation closed),
  ADR-018 (event ABI policy), ADR-026 (admin authority → Squads)

## Context and problem statement

`create_epoch` and `close_epoch` are both permissionless. `create_epoch` funds
the `Epoch` account — **0.06637056 SOL** for its 9,408 bytes — from whoever
signs. `close_epoch` refunds that rent via `close = payer`, where `payer` is
whoever signs the *close*, not whoever paid for the *create*.

The two roles are therefore economically inverted:

| action | economics | rational response |
|---|---|---|
| `create_epoch` | −0.0664 SOL, pure cost | don't run it |
| `close_epoch` | +0.0664 SOL, pure profit | run it |

Because both are open races, an operator can run a close-only cranker and
collect rent funded by operators doing the full pipeline. A creator who wants
their own capital back must *win a race* against a dedicated bot.

This is not hypothetical. Mainnet, sampled 2026-08-17:

| signer | landed instructions | net SOL |
|---|---|---|
| `4mVbPuLwa9K42HmA…` (third party) | 189 `close_observation`, 18 `close_epoch`, **0 `create_epoch`** | **+1.761781** |
| `3D8n4cFeTfHVjgMr…` | 223 `distribute_epoch`, 6 `create_epoch`, 5 `close_epoch` | −0.067680 |
| `ErEgD7dq1yR9W1Cn…` (also an observer) | 688 `distribute_epoch`, 109 `close_observation`, 23 `create_epoch`, 9 `close_epoch` | −0.482341 |

At daily epochs this is ~24 SOL/year flowing from the operators the network
depends on to a free-rider performing none of the mandatory work.

We have already decided this question once. gar #116 changed
`close_observation` to `close = observer`; its doc comment states the rationale
directly — it "removes the scavenger incentive where a third party could pocket
an observer's ~0.004 SOL rent." `close_epoch` was left on the old pattern with
**16× the rent** at stake.

## Decision drivers

- Creating epochs is mandatory recurring work; the incentive must not punish it.
- Closing must stay permissionless so a vanished creator cannot strand cleanup.
- Third parties run crankers we cannot compel to upgrade — a change that breaks
  un-upgraded clients risks stalling epoch creation network-wide.
- `Epoch` is zero-copy with only 5 spare bytes; resizing it breaks
  `AccountLoader` for live accounts and drags in the `migrate_*`
  grow-then-deserialize constraint (ADR-020).
- The refund target must be decided by program-controlled state, not by what
  the caller chooses to pass.

## Considered options

1. **Auxiliary receipt PDA + program-controlled flag** (chosen).
2. Add a `creator: Pubkey` field to `Epoch`.
3. Receipt PDA, with `close_epoch` branching on whether the caller passed it.
4. Require the receipt unconditionally on both instructions.
4b. Carry the receipt as Anchor `Option<Account<…>>` rather than
    `remaining_accounts`.
5. Restrict `close_epoch` to the creator.
6. Do nothing — treat close as a cleanup bounty like `finalize_gone`.

## Decision

**The `Epoch` account's rent is refunded to the account that created it.**

Add an auxiliary PDA recording the creator:

```rust
#[account]
pub struct EpochRentReceipt {
    pub creator: Pubkey,
    pub bump: u8,
    pub version: SchemaVersion,
}
// space 8 + 32 + 1 + 3 = 44 bytes -> 0.00119712 SOL, refunded on close
// seeds = [EPOCH_RENT_RECEIPT_SEED, &epoch_index.to_le_bytes()]
```

Repurpose one `Epoch` padding byte as a program-controlled flag:

```rust
pub has_rent_receipt: u8,   // was part of _padding2: [u8; 2]
pub _padding2: [u8; 1],
```

**No layout change** — same size, same offsets — mirroring `observations_closed`,
which "replaces a former `_padding1` byte." Pre-upgrade accounts have zeroed
padding, which correctly reads as "no receipt."

- `create_epoch` takes the receipt as a **trailing `remaining_accounts` slot**
  (see "Why not `Option<Account>`" below). When supplied it is initialised with
  `creator = payer` and `epoch.has_rent_receipt = 1`. When omitted the epoch is
  created exactly as today.
- `close_epoch` branches on the **flag**, never on what the caller passed:
  - `has_rent_receipt == 1` → the receipt is **required**; the Epoch's rent goes
    to `receipt.creator`, and the receipt is closed to the same target.
  - `has_rent_receipt == 0` → fall back to today's `close = payer`.

Closing remains permissionless; the signer pays only the tx fee.

### Why not `Option<Account<…>>`

The obvious encoding of "optional account" is Anchor's own. It does not work
here, and using it would cause the exact failure this ADR exists to prevent.

**Anchor optional accounts are positional.** A caller that does not want the
account must still pass a placeholder (the program ID) in that slot. Without the
`allow-missing-optionals` feature, `Option::try_accounts` returns
`AccountNotEnoughKeys` on a short account list
(`anchor-lang-0.31.1/src/accounts/option.rs`). So any cranker built against the
pre-upgrade IDL — which sends the old, shorter account list — would fail
`create_epoch` the instant the program is upgraded. Since creation is
permissionless and nobody is obligated to perform it, that is a network-wide
stall.

Enabling `allow-missing-optionals` is not an escape hatch either: it is a
feature on the shared `anchor-lang` crate, so Cargo's feature unification would
turn it on for ario-core, ario-arns, ario-ant and ario-ant-escrow as well —
silently changing how *their* existing optional accounts behave.

`remaining_accounts` is genuinely absent when unused. The decisive evidence:
with this design the `create_epoch` and `close_epoch` account lists are
**byte-identical in the IDL before and after**, and all 191 pre-existing
integration tests pass unmodified.

The cost is that constraints cannot be expressed declaratively on a
`remaining_accounts` slot. The equivalent checks are performed in the handler —
owner check, `create_program_address` against the stored bump to bind the
receipt to its epoch, and `require_keys_eq!` against `receipt.creator` — and
each is covered by a dedicated rejection test.

### IDL visibility

Anchor only emits `#[account]` types that are referenced by some `Accounts`
struct, so a type reached solely through `remaining_accounts` does not appear in
the IDL. `EpochRentReceipt` is consequently the only gar account type absent
from it, which costs downstream a generated decoder and PDA helper, costs
explorers the ability to decode it, and removes compile-time drift protection
for hand-rolled offsets.

This is recoverable without touching `create_epoch`/`close_epoch`: any *other*
instruction referencing the type pulls it into the IDL. `admin_close_stale_epoch`
already orphans receipts, so an `admin_close_orphaned_epoch_rent_receipt`
cleanup instruction serves a real purpose and restores IDL visibility as a side
effect.

That instruction now exists. It is authority-gated, takes the receipt as a
declared `Account<'info, EpochRentReceipt>`, refunds to `receipt.creator` (not
to the authority), and requires the parent `Epoch` to be gone — System-Program-
owned with zero data, the same post-close test `ario-ant`'s
`close_orphaned_record_metadata` uses. While the epoch lives, `close_epoch`
remains the path that closes the pair, and it is permissionless. The
`create_epoch` / `close_epoch` account lists are unchanged.

## Consequences

### Positive

- The arbitrage disappears: close-only cranking earns nothing.
- Creators recover capital without winning a race — the actual incentive to
  keep creating epochs.
- No flag day. Un-upgraded crankers keep working, degraded to today's
  behaviour, so epoch creation cannot stall.
- Rollback is clean: the program is upgradeable (never `--final`) and the
  `Epoch` layout is untouched; redeploying the prior `.so` leaves receipts inert.

### Negative / risks

- Third parties lose any rent incentive to close. Creators still have one — it
  is their own capital — and they already run crankers. If close latency
  regresses, add a protocol-funded closing fee from the reward pool rather than
  reinstating a creator-funded one.
- A creator that vanishes before closing orphans their rent. It is their
  capital, it blocks nothing (new epochs create/tally/prescribe/distribute
  independently), and anyone may still close for cleanup. Strictly better than
  today, where an *active* operator loses capital to a bot.
- `create_epoch` now initialises two accounts. Measured on BPF: create
  17,894 -> 28,707 CU (+10,813, 14% of the 200k default budget), close
  6,026 -> 8,432 CU. Comfortable headroom.
- `EpochRentReceipt` reaches the IDL only because
  `admin_close_orphaned_epoch_rent_receipt` declares it (see "IDL visibility").
  Any future refactor that drops that instruction silently removes the type,
  its PDA seeds and its decoder from every generated client.

### Neutral

- +0.00119712 SOL transient rent per epoch (1.8% of the epoch rent), refunded
  on close.
- A ~8-day transition window (epochs close at `current − 7`) during which both
  the receipt and fallback paths are live.

## Implementation notes

- Anchor cannot express a *dynamic* `close =` target; implement the refund
  manually (drain lamports, zero the account). `close_epoch` already has a
  "capture the rent lamports before Anchor's `close = payer` constraint" block
  to build on.
- `Option<UncheckedAccount<'info>>` is already used in
  `ario-ant-escrow/src/instructions/claim_vault_*.rs`; auxiliary PDAs are
  already used via `OBSERVER_LOOKUP_SEED`.
- Test matrix must cover: closer ≠ creator, closer == creator, no receipt
  (pre-upgrade epoch), mismatched receipt (rejected), receipt always closed, and
  the M8 gate unchanged.
- Leave `BPF_OUT_DIR` unset when testing `ario-gar`.

## Related

- gar #116 — `close_observation` refunds the observer.
- `finalize_gone` and `prune_to_returned` also use `close = caller`, but both
  are documented deliberate cleanup bounties over *forfeited* assets (a departed
  gateway, an expired name). An epoch creator has forfeited nothing.
- **Rejected — add `creator` to `Epoch`:** resizing a zero-copy account breaks
  `AccountLoader` for live accounts and pulls in ADR-020's migration
  constraints. The auxiliary PDA is purely additive.
- **Rejected — branch on whether the caller passed the receipt:** exploitable.
  Account optionality is caller-controlled, so a scavenger omits the receipt to
  force the fallback and pockets the rent, reproducing the exact behaviour being
  fixed. The branch must be on program-controlled state.
- **Rejected — require the receipt unconditionally:** un-upgraded third-party
  crankers would fail, and because creation is permissionless and nobody is
  obligated to perform it, epoch creation could stall network-wide.
- **Rejected — Anchor `Option<Account<…>>`:** optional accounts are
  positional, so a short account list from an un-upgraded cranker throws
  `AccountNotEnoughKeys` and `create_epoch` fails network-wide. See "Why
  not `Option<Account<…>>`" above.
- **Rejected — restrict `close_epoch` to the creator:** removes permissionless
  liveness; a vanished creator could never be cleaned up.
- **Rejected — do nothing:** cleanup bounties apply to forfeited assets. Epoch
  creation is mandatory recurring work; taxing it to pay the role that
  free-rides on it inverts the incentive on the one job the pipeline cannot do
  without.
