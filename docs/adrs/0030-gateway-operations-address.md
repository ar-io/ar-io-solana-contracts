# ADR-0030: A Gateway May Delegate Operations to a Second Address

* **Status:** proposed
* **Date:** 2026-09-02
* **Deciders:** protocol engineering

> **TL;DR:** Add `Gateway.operations_address` — a second signer, defaulting to
> the operator and rotatable only by the operator — so routine metadata edits and
> the ArNS discount no longer require loading the staked operator key into a
> browser or a bundler.

## Context and problem statement

A gateway's **operator address is its staking wallet**. It holds the stake, and
it is the only signer that can `leave_network`, `decrease_operator_stake`, or
change any gateway setting. That single key is currently required for two things
that have nothing to do with custody:

**1. Spending the ArNS discount.** A gateway `Joined` for 180 days with a ≥90%
pass rate earns 20% off every ArNS purchase (`GATEWAY_OPERATOR_DISCOUNT_PCT =
200_000`, `GATEWAY_DISCOUNT_MIN_TENURE = 15_552_000`). The gate derives the
Gateway PDA **from the signer**:

```rust
let (expected_pda, _) = Pubkey::find_program_address(
    &[GATEWAY_SEED, signer.as_ref()],      // <-- seeded by the SIGNER
    &ario_gar::ID,
);
require!(gateway_info.key() == expected_pda, ArnsError::NotGatewayOperator);
require!(gateway.operator == *signer,         ArnsError::NotGatewayOperator);
```

No other wallet can produce a matching address, so the discount is unreachable
from a bundler, a downstream client, or any automated purchasing flow.

**2. Routine gateway maintenance.** Changing an FQDN, port, or note means
`update_gateway_settings`, which is operator-gated — so an operator must load
their **staked wallet into a browser** and drive the network portal. That is the
single most routine gateway task, and it currently demands the highest-value key
the operator holds.

Both push operators toward the same bad habit: putting a staking key somewhere it
should never be. The only workaround available today *is* the thing we would tell
them never to do.

The protocol already solved this shape once. `Gateway.observer_address` (M3) is a
second `Pubkey` authorising a different wallet for one specific action, so the
staking key need not live on the observing box: *"Defaults to operator address if
not set. Allows a different wallet to submit observations."*

**Assumption worth flagging.** The framing below treats "who may spend the
discount" as a key-management problem. It is worth being explicit that the
discount is **already commercialisable without sharing any key**:
`BuyNameParams.ant` (`lib.rs:723`) is a free parameter, never constrained against
`buyer`, so an operator can today buy a name at 20% off and point it at a
customer's ANT. Any argument of the form "delegating X will deter operators from
renting out their discount" is therefore unsound — the rental channel does not
run through the delegate key at all. If discount rental is judged a real problem,
the lever belongs on the ArNS side, not here (see Considered options 5).

## Decision drivers

* The staked operator key should not be required for routine, non-custodial work.
* A compromised delegate must not be able to move funds, end the gateway, or make
  itself permanent.
* Third parties (delegators) must not be exposed to a delegation they never
  consented to.
* The discount's *earning* conditions must be untouched — this is about who may
  **spend** an earned discount, never who earns one.
* Prefer an existing, reviewed pattern over a new mechanism.

## Considered options

1. **`operations_address` covering metadata + the ArNS discount** — one new
   field, one new rotation instruction, one new metadata instruction.
2. **`operations_address` covering everything non-custodial**, including
   delegation economics (`allow_delegated_staking`,
   `delegate_reward_share_ratio`, `min_delegation_amount`, the allowlist).
3. **Reuse `observer_address`** rather than adding a second delegate field.
4. **`Vec<Pubkey>` of operations addresses**, for operators running several
   independent systems.
5. **Leave gar alone; put a limiter on the ArNS side** — cap discounted
   purchases per epoch, or require the buyer to control the receiving ANT.
6. **Do nothing** — operators keep using the staking key.

## Decision

> Take option 1. Add `operations_address: Pubkey` to `Gateway` — a second
> authorised signer, defaulting to the operator, rotatable **only** by the
> operator — and let it sign two things: the ArNS discount, and gateway
> *metadata* updates.

### The security boundary

Operator-gated actions divide into five groups. The `signer` column is the whole
decision: only the last two widen.

| group | instructions | signer |
|---|---|---|
| **Custodial** — moves funds or ends the gateway | `join_network`, `leave_network`, `increase_operator_stake`, `decrease_operator_stake`, `deduct_operator_stake_for_payment` | **operator only** |
| **Delegation economics** — affects third parties | `update_gateway_settings` (`allow_delegated_staking`, `delegate_reward_share_ratio`, `min_delegation_amount`), `set_allowlist_enabled`, `allow_delegate`, `disallow_delegate` | **operator only** |
| **Rotation** — defines the delegation itself | `update_observer_address`, `update_operations_address` | **operator only** |
| **Operational metadata** — routing and presentation | `label`, `fqdn`, `port`, `protocol`, `properties`, `note` | operator **or** `operations_address` |
| **ArNS discount** — spends an earned benefit | `buy_*` / `extend_lease` / `increase_undername_limit` discount path | operator **or** `operations_address` |

**The rotation row is the load-bearing rule.** `operations_address` must never be
able to change `operations_address`. If it could, a compromised delegate rotates
to an attacker key and locks the operator out permanently — the delegation
becomes irrevocable by the only party entitled to revoke it. The same applies to
`observer_address`: one delegated key must not be able to grant another.

**Option 2 was rejected — delegation economics stay operator-only.** They affect
people who never agreed to the delegation. Delegators staked against a reward
share they chose, and disabling delegated staking does not merely stop new
stake: per `gateway.rs`, *"Existing delegates are NOT auto-withdrawn… the cranker
moves them to withdrawal vaults via `claim_delegate_from_disabled_gateway`"* —
so a stolen ops key can force-eject every delegator. There is a
`pending_delegate_reward_share_ratio` ratchet applied at epoch boundaries
(`epoch.rs:781`) that would give notice on the ratio specifically, but "harms
third parties with notice" is a materially worse blast radius than "operator
misroutes their own gateway", and it buys no operational convenience worth that.

The argument *for* option 2 was that a heavier delegated key would deter
operators from handing it out to give away their discount. That does not hold:
the anti-sharing disincentive already exists at metadata alone (whoever holds the
key can set FQDN to garbage and fail the gateway's observations), and the
discount can be rented out without sharing any key at all, since `BuyNameParams.ant`
is unconstrained. Option 2 would add third-party blast radius to buy a deterrent
against an attack that routes around it.

**Option 3** was rejected because it conflates two unrelated jobs on one key: the
observer key lives on an always-on box submitting observations, which is exactly
the key most likely to be exposed, and it would then also carry the discount and
metadata. Separate duties want separate keys.

**Options 4 and 5 are deferred, not refused** — see Implementation notes.

**Option 6** leaves operators with a standing incentive to put a staking key in a
browser, which is the problem.

### Blast radius of a compromised operations wallet

It can point the gateway at a bad FQDN or port, so the gateway fails observations
and loses rewards until the operator notices and rotates. It can spend the
gateway's ArNS discount. That is the whole list: **bounded, self-inflicted, and
recoverable by the operator at any time.** It cannot touch stake, cannot leave
the network, cannot harm delegators, and cannot make itself permanent.

### Changes

**1. `ario-gar`**

* `operations_address: Pubkey` on `Gateway`, **appended after `version`** — the
  migration path grows the account and zero-extends the tail, so a new field must
  be last or old data misaligns (ADR-020 §3, append-only versioning). Defaults to `operator` at `join_network`.
* `update_operations_address` — operator-gated, mirroring
  `update_observer_address` including its `Joined` guard and no-op rejection.
* `update_gateway_metadata` — a **new** instruction covering only the six
  routing/presentation fields, accepting operator **or** `operations_address`.
  Deliberately separate from `update_gateway_settings`, which stays operator-only
  and unchanged: splitting by signer rather than adding a mode keeps both
  instructions' account lists byte-stable and makes the boundary legible at the
  call site.

**2. `ario-arns`** — derive the Gateway PDA from the account's **stored**
operator instead of the signer:

```rust
let gateway: Gateway = Gateway::try_deserialize(&mut &gateway_data[..])?;
let (expected_pda, _) = Pubkey::find_program_address(
    &[GATEWAY_SEED, gateway.operator.as_ref()],
    &ario_gar::ID,
);
require!(gateway_info.key() == expected_pda, ArnsError::NotGatewayOperator);
require!(
    *signer == gateway.operator || *signer == gateway.operations_address,
    ArnsError::NotGatewayOperator
);
```

Tenure, pass-rate and `Joined` checks are unchanged.

**3. Schema migration** — bump `GATEWAY_VERSION`, default
`operations_address = operator` for every existing gateway.

### Why the inverted derivation is still sound

Seeding the PDA from stored state rather than from the signer looks like a
weakening. It is not:

* `gateway_info.owner == ario_gar::ID` still holds — only `ario-gar` can create
  the account, so it cannot be fabricated.
* Deriving from `gateway.operator` and comparing against `gateway_info.key()`
  proves the account is *the* canonical Gateway PDA for the operator it claims. A
  forged account carrying an attacker-chosen `operator` derives to a different
  address and fails.
* The signer must then match one of two values stored **inside that verified
  account**, both writable only by the operator.

This is not a new pattern. ADR-020 §4 already establishes it for exactly this
case — *"where the seed derives from stored data … the handler re-derives and
matches the PDA after deserialize"* — so the change satisfies the "prefer an
existing, reviewed pattern" driver on the derivation as well as on the
delegation.

## Consequences

### Positive

* Routine gateway maintenance and ArNS purchasing no longer require the staked
  key. The portal can be driven by a low-value, rotatable wallet.
* Compromise of an operations wallet costs misrouting and a discount — never
  stake, never the gateway itself, never delegator returns.
* One reviewed pattern (`observer_address`) now covers three delegated actions.

### Negative / risks

* `Gateway` grows 32 bytes and needs a `realloc` migration across 646 live
  accounts, over a ladder that is **currently broken** (see Implementation
  notes). This migration is the risky half of the work, not the field.
* **An un-migrated `operations_address` reads as zeroes.** It must default to
  `operator`; a zeroed field must never be treated as "matches anything". This is
  an authorisation bypass if got wrong, and needs an explicit test.
* Two ways to authorise some actions is more surface to reason about. The
  boundary table above is the mitigation and should be kept current.
* Splitting metadata out of `update_gateway_settings` means two instructions
  where operators previously knew one. Clients must be updated to route
  correctly, though the old path keeps working for operators.

### Neutral

* Additive to the ABI: new instructions and a new trailing field. No existing
  instruction's account list changes, so un-upgraded clients keep working and
  operator-signed flows are entirely unaffected.
* Nothing about how the discount is *earned* changes.
* The discount remains rentable via the unconstrained `BuyNameParams.ant`, exactly
  as it is today. This ADR neither opens nor closes that; option 5 would.

## Implementation notes

### Prerequisite: the schema-migration ladder is broken

`migrate_gateway` **cannot migrate any live mainnet gateway today.** All 646 are
stamped `version = 0.0.0`, `GATEWAY_VERSION` is `1.1.0` (`state/mod.rs:120`), and
`migrate_gateway_version` (`schema_migration.rs:147`) only has a `0.0.0 → 1.0.0`
arm:

```rust
while account.version < GATEWAY_VERSION {   // 0.0.0 < 1.1.0 -> enter
    match account.version {
        0.0.0 => version = 1.0.0            // now 1.0.0
        _ => return err!(UnknownSchemaVersion)
    }
}                                            // 1.0.0 < 1.1.0 -> enter again
                                             // 1.0.0 matches `_` -> ERROR
```

The `1.1.0` bump landed without its migration arm. It is latent only because
`Gateway::SIZE` still matches every live account, so nothing has ever needed
migrating and nobody has run it. **This ADR is the first change that actually
requires the ladder**, so it must supply the missing `1.0.0 → 1.1.0` arm before
adding the next one.

### Build notes

* Mirror `update_observer_address` for the rotation instruction, including its
  `GatewayLeaving` guard and no-op rejection.
* Emit events for both new instructions (ADR-018).
* Test matrix: operator retains every capability; `operations_address` gets the
  discount and metadata updates; `operations_address` is **rejected** for every
  custodial, delegation-economics and rotation instruction; **a zeroed /
  un-migrated field authorises nobody**; a forged Gateway account with a spoofed
  `operator` fails the PDA check; tenure and pass-rate failures still deny both
  signers.
* **Ship separately from ADR-0031, deployed after it.** Independent changes with
  very different risk: ADR-0031 is additive with no migration, this rewrites 646
  live accounts. Bundling would gate a zero-migration fix behind a migration.

### Deferred

* **Option 4 — multiple operations addresses.** A `Vec<Pubkey>` would serve
  operators running several independent systems, but costs a variable-length
  field, a cap, and add/remove instructions. Start with one — `Gateway.version`
  exists precisely so a list can replace it later without a redesign.
* **Option 5 — an ArNS-side discount limiter.** If renting out the discount
  proves to be a real problem, the fix is a cap on discounted purchases per epoch
  or a constraint tying the receiving ANT to the buyer. Both are independent of
  this ADR and neither is blocked by it. No evidence of abuse today, so no
  mechanism yet.

### Downstream

* **SDK** — `updateOperationsAddress`, `updateGatewayMetadata`, and the discount
  path must pass the Gateway PDA rather than deriving it from the signer.
* **network-portal** — surface the operations address in gateway settings, and
  route metadata edits through the new instruction so the portal can be driven by
  a non-staking wallet. This is the change operators will actually feel.

## Related

* Code: `programs/ario-gar/src/state/mod.rs` (`Gateway`, `GATEWAY_VERSION`),
  `programs/ario-gar/src/instructions/gateway.rs`,
  `programs/ario-gar/src/schema_migration.rs`,
  `programs/ario-arns/src/instructions/purchase.rs`
* Precedent: `Gateway.observer_address` (M3) — the delegation pattern generalised
  here
* Requirement: SHOULD-11 — the ArNS discount's tenure + performance gates
* ADR: [ADR-020](0020-schema-migration-grow-then-deserialize.md) —
  grow-then-deserialize, append-only versioning, and the `{0,0,0} → 1.0.0`
  bootstrap arms; the ladder this ADR must repair, and the rule that puts
  `operations_address` at the byte-end
* ADR: ADR-018 (in [`docs/DECISIONS.md`](../DECISIONS.md)) — Anchor `#[event]`
  ABI policy for the two new instructions. Note the numbering split: ADR-001–019
  live in `DECISIONS.md`, `docs/adrs/` restarts at 0020.
* ADR: [ADR-0031](0031-transferable-epoch-settings-authority.md) — ships first,
  separately
