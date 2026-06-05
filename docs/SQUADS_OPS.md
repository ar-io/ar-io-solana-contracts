# Squads Operations Runbook (Admin Guide)

How the AR.IO programs are governed by a **Squads V3** multisig, and the
step-by-step ceremonies for the two things admins do: **upgrade a program**
and **run a privileged admin instruction**. Grounded in our actual tooling
(`scripts/mainnet-prepare-upgrade.sh`, `scripts/staging-v2-deploy-and-squads.sh`),
[ADR-026](adrs/0026-admin-authority-transfer.md), and the
[Squads V3 (legacy) docs](https://docs.squads.so/main/squads-legacy).

> **We use Squads V3 (the SMPL program), NOT V4.** Mainnet upgrade authority
> is a V3 multisig. Drive every ceremony from the **legacy Squads app**, not
> `app.squads.so` (that is V4). This was confirmed by a full staging dress
> rehearsal (a real `BPFLoaderUpgradeable` upgrade executed by the 2-of-4
> vault).

## The two authority planes

Everything below is one of these two. They are **independent** — full
multisig governance means putting *both* on the vault.

| Plane | Controls | Vault acts as | Inner instruction |
|---|---|---|---|
| **Upgrade** (`BPFLoaderUpgradeable`) | the program binary (`.so`) | the program's **upgrade authority** | `BPFLoaderUpgradeable::Upgrade` |
| **Admin** (in-program `authority`) | on-chain state / params | the in-program **`authority`** field | `transfer_authority`, `admin_set_*`, `finalize_*`, `reserve_name`, … |

A third, transient plane — the **`migration_authority`** (a hot key for the
bulk import) — is **not** Squads-governed by design: it self-retires at
`finalize_migration` and does nothing afterward.

Both ceremonies share the same rhythm: **prepare → propose → 2-of-N approve →
execute.** Only the inner instruction differs.

## Know your Squads accounts (and verify them)

Three distinct accounts — never confuse them:

| Account | What it is | Owner | Role |
|---|---|---|---|
| **Multisig config** | members + threshold | the SMPL program | derivation source; **never** signs, **never** receives assets/authority |
| **Vault** (authority **index 1**) | the signing PDA | **System program** | holds upgrade + admin authority; signs everything the multisig executes |
| Transaction / Proposal | per-action | SMPL program | created at propose time |

**Staging (devnet) reference values:**
- SMPL program: `SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu`
- Multisig config: `G8Wja3zCGk4dqdK1Me5RwEw5zVvcjs3ZFy646ePZJPtM` (2-of-4)
- **Vault (index 1)** → `4sBzyU2P14jhvit6ckjqAzy1VB5kymtsSqh2rQsjMPSv`

Mainnet uses its own multisig + vault (pin them in `program-ids/mainnet.json`
once known).

**Always verify before pointing authority or a buffer at it:**

```bash
# Vault must be System-owned (a config account is SMPL-owned = WRONG target).
solana account <VAULT> -u <rpc> --output json | jq -r '.account.owner'
#   -> 11111111111111111111111111111111   (System)

# Multisig config must be SMPL-owned (proves V3, not V4).
solana account <MULTISIG> -u <rpc> --output json | jq -r '.account.owner'
#   -> SMPLecH534NA9acpos4G6x7uf3LWbCAwZQE9e8ZekMu
```

The vault is **authority index 1** — `PDA(["squad", <multisig>, 1u32_le,
"authority"], SMPL)`. Index 0 is reserved/internal; never target it. Our
scripts (`verify_vault` / `verify_v3_vault`) assert the System-owned check
automatically.

---

## Ceremony A — Upgrade a program (new `.so`)

A dev/CI prepares the buffer; the multisig executes. No contract changes are
ever needed — upgrades are code-agnostic.

### 1. Stage the buffer (off-chain, hot/CI key)

```bash
# Mainnet:
SQUADS_V3_VAULT=<vault> SQUADS_V3_MULTISIG=<multisig> \
  BUFFER_AUTHORITY_KEYPAIR=<hot-wallet> \
  bash scripts/mainnet-prepare-upgrade.sh

# Staging:
SQUADS_VAULT=<vault> SQUADS_V3_MULTISIG=<multisig> \
  AUTHORITY_KEYPAIR=<deployer> \
  bash scripts/staging-v2-deploy-and-squads.sh prepare-upgrade
```

These scripts:
1. verify the vault (System-owned),
2. build the `.so` with mainnet/devnet feature flags,
3. **auto-`program extend`** any program whose new `.so` exceeds its on-chain
   ProgramData capacity (see the gotcha below),
4. `write-buffer` the `.so`,
5. `set-buffer-authority → the vault`,
6. emit `release/.../buffer-manifest.json` = `{program, buffer, buffer_sha256, so_size}`.

### 2. Propose in the legacy Squads V3 app

Developers → **Programs**:
- If the program isn't listed, **Add Program** (its upgrade authority is the
  vault, so "Verify authority" passes immediately).
- Click the program → **Add upgrade**: `buffer` (from manifest) + `spill`
  (rent-refund address). Buffer authority is already the vault → "Verify
  authority".

### 3. Verify, approve, execute

Each signer independently checks the buffer **before voting**:

```bash
solana program dump <buffer> /tmp/x.so -u <rpc>
shasum -a 256 /tmp/x.so          # must equal buffer_sha256 from the manifest
```

Then **2-of-N approve → Execute**. Confirm:

```bash
solana program show <program_id> -u <rpc>   # "Last Deployed In Slot" bumps
```

### ⚠️ The ProgramData size gotcha (learned the hard way)

A Squads `Upgrade` **cannot grow the program**. `solana program deploy`
(Agave 2.1) allocates ProgramData to *exactly* the program size, so if a new
build is larger, the multisig **Execute fails** ("account data too small").

Fix = `solana program extend <program_id> <bytes>` **first**. It is
**permissionless** (no upgrade-authority signer), so the buffer-authority hot
wallet can run it even after handoff. Our `prepare-upgrade` does this
automatically; if you ever upgrade by hand, extend manually:

```bash
need=$(( new_so_size + 45 ))                 # 45-byte ProgramData header
# extend by (need - current_capacity + cushion); see prepare-upgrade for the math
solana program extend <program_id> <extra_bytes> -u <rpc>
```

---

## Ceremony B — Run an admin instruction

For any in-program privileged instruction (`transfer_authority`,
`admin_set_epoch_duration`, `admin_set_gar_program`, `reserve_name`,
`finalize_migration`, `admin_repair_*`, …) once the vault holds `authority`.

> No dedicated script exists for this yet (tracked as a follow-up). For now,
> build the instruction with the TS client and propose it through the V3 app's
> **TX Builder**.

### 1. Build the instruction (off-chain)

Use `@ar.io/solana-contracts` (Codama builders). Set the `authority` account
to **the vault PDA**:

```ts
import { getAdminSetEpochDurationInstructionAsync } from '@ar.io/solana-contracts/gar';
// or getTransferAuthorityInstructionAsync, getAdminSetGarProgramInstructionAsync, ...

const ix = await getAdminSetEpochDurationInstructionAsync({
  settings: GAR_SETTINGS_PDA,
  authority: VAULT,            // the Squads V3 vault (index 1) — the signer
  newDuration: 86_400n,
});
// -> programId, accounts (authority = VAULT, isSigner), data
```

### 2. Propose via the V3 TX Builder

In the legacy Squads app → **TX Builder** → **Create transaction** (saved as a
draft) → **Add instruction** with the program ID, accounts, and data from
step 1 → **run a simulation** to confirm it assembles correctly → **Initiate
Transaction** to propose it.

### 3. Approve + execute

**2-of-N approve → Execute.** Squads CPIs the instruction signing as the
vault. Verify the on-chain state change (e.g. re-read the config account).

---

## Rotating governance / recovery (`transfer_authority`, ADR-026)

`transfer_authority(new_authority)` is itself an **admin instruction**
(Ceremony B) — so moving governance to a *different* multisig, or recovering
from a compromised key, runs through the same propose→approve→execute path,
signed by the current vault. Single-step, null-pubkey-guarded. The admin
`authority` is the *only* rotatable plane; `migration_authority` self-retires
and is never rotated.

`ario-ant-escrow` has no admin authority of its own — it reads
`ario_core::ArioConfig.authority` cross-program, so rotating **core's**
authority rotates escrow's admin automatically. No escrow ceremony needed.

---

## One-time bootstrap (for reference)

How a deployment *gets* onto the vault in the first place (never deploy
*to* Squads directly — the upgrade authority is handed off after the fact):

1. **Deploy** (not `--final`) → the deployer holds upgrade authority.
2. **network-init** → run by the deployer (genesis `initialize*` is bound to
   the *upgrade* authority, so it must run before handoff). Use a hot
   `migration_authority` for the bulk import.
3. **Hand off upgrade authority → vault**:
   `solana program set-upgrade-authority <id> --new-upgrade-authority <vault> --skip-new-upgrade-authority-signer-check`
   (the vault is a PDA and can't sign, hence the skip flag).
4. **(Optional) rotate admin authority → vault** via `transfer_authority`
   (Ceremony B). Or set `authority = vault` at `initialize`.

After step 3, **CI can no longer auto-upgrade** that cluster — every upgrade
is a multisig ceremony (this is the point: a true production-governance
mirror).

---

## Safety checklist

- [ ] Vault is **System-owned** and is **authority index 1** of the intended multisig.
- [ ] Multisig is **SMPL-owned** (V3, not V4).
- [ ] Using the **legacy** Squads app, not `app.squads.so`.
- [ ] (Upgrades) buffer `shasum` verified against the manifest **before** voting.
- [ ] (Upgrades) new `.so` fits ProgramData, or `program extend` ran first.
- [ ] (Admin) the instruction's `authority` account is the **vault**, and you **simulated** it.
- [ ] Threshold reached, then **Execute** — confirm the on-chain effect (slot bump / state read).

## References

- [ADR-026 — single-step `transfer_authority`](adrs/0026-admin-authority-transfer.md)
- `scripts/mainnet-prepare-upgrade.sh`, `scripts/staging-v2-deploy-and-squads.sh`
- Squads V3 (legacy): [Programs](https://docs.squads.so/main/squads-legacy/navigating-your-squad/developers/programs)
  · [TX Builder](https://docs.squads.so/squads-v3-docs/navigating-your-squad/developers/tx-builder)
  · [Authorities](https://docs.squads.so/squads-v3-docs/development/authorities)
