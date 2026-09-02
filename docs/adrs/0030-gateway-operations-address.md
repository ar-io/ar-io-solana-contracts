# ADR-0030: A Gateway May Delegate Its ArNS Discount to an Operations Address

- Status: proposed
- Date: 2026-09-02
- Deciders: protocol engineering
- Related: M3 `observer_address` (the precedent this copies), SHOULD-11 (the
  discount's tenure + performance gates), ADR-012 (`version: SchemaVersion` +
  `realloc` schema migration)

## Context and problem statement

A gateway that has been `Joined` for 180 days with a ≥90% epoch pass rate earns a
**20% discount on every ArNS purchase** (`GATEWAY_OPERATOR_DISCOUNT_PCT =
200_000`, `GATEWAY_DISCOUNT_MIN_TENURE = 15_552_000` seconds).

**Today only the staking wallet can spend it.** The gate in
`ario-arns::pricing::try_apply_gateway_discount` derives the Gateway PDA **from
the signer**:

```rust
let (expected_pda, _) = Pubkey::find_program_address(
    &[ario_gar::state::GATEWAY_SEED, signer.as_ref()],   // <-- seeded by the SIGNER
    &ario_gar::ID,
);
require!(gateway_info.key() == expected_pda, ArnsError::NotGatewayOperator);
require!(gateway.operator == *signer,         ArnsError::NotGatewayOperator);
```

No wallet other than the operator can produce a matching address, so the discount
is unreachable from a bundler, a downstream client, or any automated purchasing
flow. The only workaround available today is to **put the staked operator key
into that system** — the wallet holding the gateway's stake, able to
`leave_network`, withdraw, and rotate the gateway's own settings. That is a
strictly worse security posture than the problem it solves, and it is what
operators are currently pushed toward.

The protocol already solved this exact shape once. `Gateway.observer_address`
(M3) is a second `Pubkey` that authorises a *different* wallet for one specific
action — submitting observations — precisely so the staking key does not have to
live on the observing box. Its doc comment states the intent plainly: *"Defaults
to operator address if not set. Allows a different wallet to submit
observations."*

## Decision drivers

- The staked operator key should not need to be present wherever ArNS names are
  purchased.
- The discount's *earning* conditions (tenure, pass rate, `Joined`) must be
  untouched — this is about who may **spend** an earned discount, never about who
  earns one.
- Prefer an existing, reviewed pattern over a new mechanism.
- Existing gateways must keep working with no action required.

## Considered options

1. **Second authorised address on `Gateway`** (chosen) — mirrors `observer_address`.
2. **Derive the Gateway PDA from a passed operator argument** rather than the
   signer, and let anyone claim any gateway's discount. Rejected outright: it
   hands every gateway's discount to the whole network.
3. **Off-chain attestation** — operator signs a delegation the buyer presents.
   Rejected: needs signature verification plumbing in `ario-arns`, a revocation
   story, and replay protection, to reach the same place as one stored `Pubkey`.
4. **A list of authorised addresses.** Deferred, not rejected — see
   "Consequences".
5. **Do nothing.** Rejected: the status quo actively encourages operators to
   deploy staked keys into bundlers.

## Decision

**Add `operations_address: Pubkey` to `Gateway`, and let the ArNS discount be
claimed by either the operator or that address.**

```rust
/// Separate address authorised to spend this gateway's ArNS discount.
/// Defaults to the operator address. Lets a bundler or downstream client use
/// the discount without holding the staked operator key.
pub operations_address: Pubkey,
```

Three changes:

**1. `ario-gar`** — new field, defaulted to `operator` on join and on migration,
plus `update_operations_address` gated on the operator, mirroring
`update_observer_address` (`gateway.rs:534`) including its `Joined` requirement
and no-op rejection.

**2. `ario-arns`** — invert the PDA derivation so it comes from the account's
**stored** operator rather than from the signer:

```rust
let gateway: Gateway = Gateway::try_deserialize(&mut &gateway_data[..])?;

// Canonical Gateway PDA for the operator recorded IN the account.
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

The tenure, pass-rate and `Joined` checks are unchanged.

**3. Schema migration** — bump `Gateway.version` and default
`operations_address = operator` for every existing gateway, so behaviour is
identical until an operator opts in.

## Why this is safe

The inverted derivation looks like it weakens the check. It does not:

- `gateway_info.owner == ario_gar::ID` still holds, so the account cannot be
  fabricated — only `ario-gar` can create one.
- Deriving from `gateway.operator` and comparing to `gateway_info.key()` proves
  the passed account is *the* canonical Gateway PDA for the operator it claims.
  A forged account with an attacker-chosen `operator` field would derive to a
  different address and fail.
- The signer must then equal one of two values **stored inside that verified
  account**, both writable only by the operator.

Gaming vectors considered:

| vector | outcome |
|---|---|
| Point `operations_address` at a wallet that has not earned a discount | No gain — tenure/pass-rate/`Joined` are read from the Gateway account, never from the signer. |
| Share one operations wallet across many buyers | The discount reaches whoever that wallet buys for. **Already possible today by sharing the operator key**; this makes it safe rather than reckless. A policy limit, if wanted, is a separate decision. |
| Use one wallet as `operations_address` for many gateways | No gain. The discount is a flat 20%, not cumulative. |
| Set `operations_address` to a wallet you do not control | You give away your own discount. Self-limiting, no effect on anyone else. |
| Rotate rapidly to dodge something | Nothing is time-bound to the address; the tenure clock lives on `start_timestamp`. |

Net effect on risk is **negative**: the change removes the only current reason to
deploy a staked operator key into third-party infrastructure.

## Consequences

### Positive

- The discount becomes usable from bundlers and downstream clients with a
  low-value, rotatable key.
- Compromise of an operations wallet costs at most the discount. It cannot
  `leave_network`, withdraw stake, or alter gateway settings.
- One reviewed pattern (`observer_address`) now covers both delegated actions.

### Negative / risks

- `Gateway` grows by 32 bytes and needs a `realloc` migration. `migrate_gateway`
  already exists for this, but it is a per-account pass across the registry
  (currently 646 gateways on mainnet).
- **Un-migrated gateways read `operations_address` as zeroes.** The migration
  must default it to `operator`; a zeroed field must never be treated as
  "matches anything". Worth an explicit test.
- Two ways to authorise one action is marginally more surface to reason about.

### Neutral

- Purely additive to the ABI: a new instruction and a new trailing field. No
  existing instruction's account list changes, so un-upgraded clients keep
  working — buyers signing as the operator are unaffected.
- Nothing about how the discount is *earned* changes.

## Deferred

**Multiple operations addresses.** A `Vec<Pubkey>` would serve operators running
several independent purchasing systems, but costs a variable-length field, a cap,
and add/remove instructions. Start with one — `Gateway.version` exists precisely
so a list can replace it later without a redesign.

## Implementation notes

- Mirror `update_observer_address` exactly, including its `GatewayLeaving` guard
  and its rejection of a no-op update.
- Emit an event alongside `ObserverAddressUpdated` for parity (ADR-018).
- Test matrix: operator still gets the discount; `operations_address` gets it;
  an unrelated signer does not; a zeroed/un-migrated field does not authorise
  anyone; a forged Gateway account with a spoofed `operator` fails the PDA check;
  tenure and pass-rate failures still deny both signers.
- Bundle with the `EpochSettings.authority` transfer gap — both are `ario-gar`
  and would otherwise cost two separate mainnet upgrade cycles.
