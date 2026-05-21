// `pub use *` brings every module's `handler` fn into this namespace,
// which the compiler flags as ambiguous because multiple modules export
// the same name. lib.rs always calls handlers via the module-qualified
// path (`instructions::deposit::handler(...)`), so the ambiguity is
// inert — the glob re-exports exist only to surface the per-ix
// Accounts structs (`DepositAnt`, `ClaimAntArweaveAttested`, …) up
// to lib.rs without listing each one. Matches the convention in
// `ario-core` / `ario-arns` / `ario-gar`.
#![allow(ambiguous_glob_reexports)]

pub mod admin_purge_unclaimed;
pub mod cancel;
pub mod cancel_token_deposit;
pub mod cancel_vault_deposit;
pub mod claim_arweave_attested;
pub mod claim_ethereum;
pub mod claim_tokens_arweave_attested;
pub mod claim_tokens_ethereum;
pub mod claim_vault_arweave_attested;
pub mod claim_vault_ethereum;
pub mod deposit;
pub mod deposit_tokens;
pub mod deposit_vault;
pub mod update_recipient;
pub mod update_token_recipient;
pub mod update_vault_recipient;

pub use admin_purge_unclaimed::*;
pub use cancel::*;
pub use cancel_token_deposit::*;
pub use cancel_vault_deposit::*;
pub use claim_arweave_attested::*;
pub use claim_ethereum::*;
pub use claim_tokens_arweave_attested::*;
pub use claim_tokens_ethereum::*;
pub use claim_vault_arweave_attested::*;
pub use claim_vault_ethereum::*;
pub use deposit::*;
pub use deposit_tokens::*;
pub use deposit_vault::*;
pub use update_recipient::*;
pub use update_token_recipient::*;
pub use update_vault_recipient::*;
