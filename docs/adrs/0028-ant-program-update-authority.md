# ADR-0028: ario-ant Program PDA Holds the ANT UpdateAuthority

* **Status:** accepted
* **Date:** 2026-07-16
* **Deciders:** @gocryptoyourself

> **TL;DR:** New ANTs mint with their Metaplex Core UpdateAuthority (and
> Attributes-plugin authority) set to a per-asset `ant_authority` PDA owned by
> the ario-ant program, so attribute syncs and other MPL Core updates route
> through the program (signed by the PDA) instead of the holder's wallet. The
> user keeps `Owner` (custody). **Escrow (`ario-ant-escrow`) is out of scope for
> this ADR** — see "Escrow compatibility (deferred)" below.

## Context and problem statement

ANTs are Metaplex Core NFTs (ADR-016). Before this ADR they minted with **both
`Owner` and `UpdateAuthority` set to the spawning wallet**, and the Attributes
plugin (ArNS Name / Type / Undername Limit — used for DAS queryability) had
authority `Owner`. The only way to update the plugin — e.g.
`ario_ant::sync_attributes` after an ArNS name is bought — was for the current
ANT holder to sign (`programs/ario-ant/src/lib.rs`, the
`require!(authority == nft_owner)` gate + plain `invoke`). Consequences:

* A name bought by someone who is **not** the ANT owner could not reconcile the
  on-chain traits; the sync was deferred to whenever the holder happened to call
  `sync_attributes`.
* No metadata update could be performed "by the protocol"; everything required
  the holder's wallet signature.

We want the **ario-ant program** to be the authority that controls Metaplex Core
updates on ANTs, so trait syncs (and future metadata control) are signed by the
program, and permissionless reconciliation becomes possible.

The `Owner == UpdateAuthority` coupling was an unchecked convention (ADR-013),
relied on by `ario-ant-escrow`'s deposit/claim/cancel flows, which rotate
UpdateAuthority alongside Owner (audit L23). Decoupling them means a
program-controlled ANT is **not** compatible with the current escrow deposit
path (which rotates UA via the depositor's signature, assuming the depositor
*is* the UA) — this ADR deliberately leaves escrow untouched and defers that
compatibility work (see "Escrow compatibility (deferred)").

## Decision drivers

* Attribute syncs for ArNS names must be executable **through the program**,
  ideally permissionlessly (a cranker, not only the holder).
* The user must retain **custody** — the program must never own the NFT, so
  holders can still transfer/sell on marketplaces.
* **Do not touch the audited escrow flow** in this change — keep the blast
  radius on `ario-ant` only; handle escrow compatibility as separate, later work.
* Existing (already-minted) ANTs must keep working, with an opt-in path onto the
  new model.

## Considered options

1. **Attributes-plugin authority only** — move just the plugin authority to a
   program PDA, leave the asset UpdateAuthority with the user. Smallest change;
   escrow untouched. But the program does not control name/uri or plugin
   add/remove — only the Attributes plugin.
2. **Program PDA holds the asset UpdateAuthority** (chosen) — the PDA is the
   asset UpdateAuthority *and* (via `Authority::UpdateAuthority`) the
   Attributes-plugin authority. The program controls all UpdateAuthority-gated
   MPL Core operations. Trade-off: program-controlled ANTs become incompatible
   with the current escrow deposit until escrow is updated separately (deferred).
3. **Do nothing** — keep owner-signed syncs.

## Decision

> Adopt **Option 2**. Introduce a per-asset signer-only PDA in ario-ant —
> `["ant_authority", asset]` (`ANT_AUTHORITY_SEED`). New ANTs mint with
> `updateAuthority = ant_authority` and Attributes-plugin authority =
> `Authority::UpdateAuthority` (which resolves to that PDA). `sync_attributes`
> and `clear_attributes` sign `UpdatePluginV1` with the PDA via `invoke_signed`;
> `sync_attributes` becomes **permissionless** (its existing `ArnsRecord`
> validation — owner + PDA seeds + `record.ant == asset` — is what keeps it
> safe). `clear_attributes` stays owner-gated (it removes live traits). Existing
> ANTs opt in via a new owner-signed `adopt_authority` instruction. **Escrow is
> unchanged** by this ADR — escrow compatibility with program-controlled ANTs is
> deferred (see below).

`Owner` custody stays with the user, satisfying the custody driver: MPL Core
`TransferV1` and `BurnV1` are **Owner-gated** and never consult UpdateAuthority,
so holders keep full transfer/sell rights. UpdateAuthority in MPL Core cannot
transfer ownership — it can update metadata (name/uri), add/remove plugins, and
sign plugins whose authority is `UpdateAuthority` — so parking it at the program
PDA gives the program metadata/plugin control without any custody risk.

Legacy compatibility: `sync_attributes`/`clear_attributes` detect the asset's
current UpdateAuthority. If it equals the `ant_authority` PDA the program signs
(new/adopted ANTs); otherwise they fall back to the pre-ADR-028 owner-signed
path (un-adopted legacy ANTs). One instruction serves both populations.

**Two MPL Core gotchas (verified against the deployed `mpl_core.so` in
integration tests), both load-bearing for correctness:**

1. **`owner` must be passed explicitly at `CreateV1`.** MPL Core defaults
   `owner` to the `updateAuthority` when the `owner` account is omitted — so
   setting `updateAuthority = ant_authority` PDA without pinning `owner` would
   make the **program PDA the NFT owner**, silently stripping the user of
   custody. The SDK `spawn` path and every test mint pin `owner` to the holder.
2. **Adopt uses `RevokePluginAuthorityV1`, not `ApprovePluginAuthorityV1`.** The
   Attributes plugin is UpdateAuthority-managed, so a legacy ANT's `Owner`
   setting is a *delegation away from the default*. `ApprovePluginAuthorityV1`
   errors `CannotRedelegate` (0x1b); `RevokePluginAuthorityV1` (`0A 06`) resets
   the plugin to its `UpdateAuthority` default, which is exactly the state new
   ANTs mint into.

## Escrow compatibility (deferred)

This ADR changes **`ario-ant` only**; `ario-ant-escrow` is untouched and still
rotates UpdateAuthority with the depositor's signature at deposit (assuming
`Owner == UpdateAuthority`, per ADR-013 / audit L23). Consequences:

* **Legacy ANTs** (UA = owner wallet) deposit into escrow exactly as before.
* **Program-controlled ANTs** (UA = `ant_authority` PDA) **cannot** be deposited
  into the current escrow: `deposit_ant`'s wallet-signed `UpdateV1` requires the
  depositor to be the UA, which it no longer is, so the CPI reverts. Escrow's
  existing UA-rotation resolution of audit L23 is unaffected for the ANTs it
  actually handles.

Making escrow accept program-controlled ANTs (e.g. an admission check that the
UA is the `ant_authority` PDA + dropping the now-redundant UA rotation) is a
**separate, later change**, coordinated with the migration importer
(`solana-ar-io`) that mints + deposits ANTs.

## Consequences

### Positive

* ArNS attribute syncs run through the program and are permissionless for
  program-controlled ANTs — a name bought by a non-owner reconciles immediately.
* The Attributes-plugin surface is program-locked: only `ario-ant` can write it,
  and `sync_attributes` whole-list-replaces with exactly the canonical ArNS
  traits (+ the preserved `ANT Program` routing trait).

### Negative / risks

* The `Owner == UpdateAuthority` invariant (ADR-013) is intentionally broken for
  program-controlled ANTs. Any future code must not assume it — most notably the
  escrow deposit path, which is why escrow compatibility is deferred (above).
* The asset's on-chain `name`/`uri` (`UpdateV1`) become mutable only by the
  program, and there is currently **no** ario-ant instruction that signs
  `UpdateV1` for name/uri. ANT display metadata lives in the `AntConfig` PDA
  (off the MPL asset), so this is acceptable today; a PDA-signed
  `update_metadata` instruction is a follow-up if on-chain asset name/uri must
  stay mutable.
* Trust posture: AR.IO retains BPFLoaderUpgradeable upgrade authority
  (non-`--final` deploys), so a program upgrade could in principle abuse the UA
  PDA. This is the pre-existing protocol trust model, not new to this ADR.

### Neutral

* `sync_attributes`/`clear_attributes` gain the per-asset `ant_authority` PDA
  account and pay ~1.5k CU for its seed derivation per call.
* `clear_attributes` remains owner-gated in both models (only the signing model
  differs: PDA-signed vs owner-signed).

## Implementation notes

* Contracts: `ANT_AUTHORITY_SEED` in `programs/ario-ant/src/state.rs`; PDA
  signing + `RevokePluginAuthorityV1` (`0A 06`) and `UpdateV1` set-UA
  encoders + `read_mpl_core_update_authority` in ario-ant `mpl_core_cpi.rs` /
  `lib.rs`; new `adopt_authority` instruction + `AuthorityAdoptedEvent`.
* Escrow (`ario-ant-escrow`): **not modified** by this ADR — see "Escrow
  compatibility (deferred)".
* MPL Core wire facts (from `clients/ts/idls/mpl_core.json`): plugin `Authority`
  enum `None=0, Owner=1, UpdateAuthority=2, Address=3` — distinct from the
  *asset* UpdateAuthority enum used by `UpdateV1` (`None=0, Address=1,
  Collection=2`). New CreateV1 plugin pair ends `01 02` (was `01 01`).
* SDK (`ar-io-sdk`): `getAntAuthorityPDA`; `spawn-ant.ts` sets
  `updateAuthority` + plugin authority `UpdateAuthority`; `io-writeable.ts`
  `sync_attributes` builder drops `authority`, passes `antAuthority`.
* Event ABI: `AuthorityAdoptedEvent` added to `idl-event-snapshots.json`
  (append-only, ADR-018).
