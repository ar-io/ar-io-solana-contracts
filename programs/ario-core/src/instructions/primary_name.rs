use anchor_lang::prelude::*;
use anchor_spl::token::{self, Token, TokenAccount, Transfer as SplTransfer};

use crate::constants;
use crate::error::ArioError;
use crate::state::*;

/// Maximum undername length (matches Lua MAX_UNDERNAME_LENGTH = 61)
const MAX_UNDERNAME_LENGTH: usize = 61;
/// Maximum base name length (matches Lua MAX_BASE_NAME_LENGTH = 51)
const MAX_BASE_NAME_LENGTH: usize = 51;

/// Validate primary name format (matches Lua assertValidPrimaryName).
/// Format: "basename" or "undername_basename"
/// - base name: 1-51 chars, alphanumeric + hyphens, starts/ends alphanumeric
/// - undername: 1-61 chars, alphanumeric + hyphens, starts/ends alphanumeric
/// - Total max 63 chars
pub fn validate_primary_name_format(name: &str) -> Result<()> {
    require!(!name.is_empty(), ArioError::NameEmpty);
    require!(
        name.len() <= PrimaryNameRequest::MAX_NAME_LENGTH,
        ArioError::NameTooLong
    );

    // Split into undername and base name
    let parts: Vec<&str> = name.splitn(2, '_').collect();
    let (base_name, undername) = if parts.len() == 2 {
        (parts[1], Some(parts[0]))
    } else {
        (parts[0], None)
    };

    // Validate base name (1-51 chars, alphanumeric + hyphens)
    require!(
        !base_name.is_empty() && base_name.len() <= MAX_BASE_NAME_LENGTH,
        ArioError::InvalidNameFormat
    );
    require!(
        is_valid_name_segment(base_name),
        ArioError::InvalidNameFormat
    );
    // Length 43 is prohibited (Arweave address collision)
    require!(base_name.len() != 43, ArioError::InvalidNameFormat);

    // Validate undername if present (1-61 chars, alphanumeric + hyphens)
    if let Some(un) = undername {
        require!(
            !un.is_empty() && un.len() <= MAX_UNDERNAME_LENGTH,
            ArioError::InvalidNameFormat
        );
        require!(is_valid_name_segment(un), ArioError::InvalidNameFormat);
    }

    Ok(())
}

/// Validate a name segment: alphanumeric + hyphens, starts/ends alphanumeric
pub fn is_valid_name_segment(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if bytes.len() == 1 {
        return bytes[0].is_ascii_alphanumeric();
    }
    // Must start and end with alphanumeric
    if !bytes[0].is_ascii_alphanumeric() || !bytes[bytes.len() - 1].is_ascii_alphanumeric() {
        return false;
    }
    // Middle chars can be alphanumeric or hyphen
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
}

/// Read and validate an ArNS record from remaining_accounts.
/// Returns the demand factor from the next remaining account (if provided).
/// account_info must be owned by arns_program and match the derived PDA for the base name.
fn validate_arns_record_exists(
    arns_record_info: &AccountInfo,
    arns_program_id: &Pubkey,
    base_name: &str,
    current_timestamp: i64,
) -> Result<()> {
    // Verify owned by arns program
    require!(
        arns_record_info.owner == arns_program_id,
        ArioError::InvalidAccountState
    );

    // Verify PDA matches expected base name
    let name_hash = anchor_lang::solana_program::hash::hash(base_name.as_bytes());
    let (expected_pda, _) =
        Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], arns_program_id);
    require!(
        arns_record_info.key() == expected_pda,
        ArioError::InvalidAccountState
    );

    // Verify account has data (record exists and is initialized)
    require!(
        !arns_record_info.data_is_empty(),
        ArioError::ArnsRecordNotFound
    );

    // M-4: Validate ArnsRecord discriminator
    {
        let data = arns_record_info
            .try_borrow_data()
            .map_err(|_| error!(ArioError::InvalidAccountState))?;
        if data.len() >= 8 {
            let expected_disc = anchor_lang::solana_program::hash::hash(b"account:ArnsRecord");
            require!(
                data[..8] == expected_disc.to_bytes()[..8],
                ArioError::InvalidAccountState
            );
        } else {
            return err!(ArioError::InvalidAccountState);
        }
    }

    // BUG-2: Verify the ArNS record is still active (lease not expired)
    verify_arns_record_active(arns_record_info, current_timestamp)?;

    Ok(())
}

/// BUG-2: Check that an ArNS record is still active (lease not expired).
/// Leases are considered active if end_timestamp + 14-day grace period >= current_timestamp.
/// Permabuys are always active.
fn verify_arns_record_active(arns_record_info: &AccountInfo, current_timestamp: i64) -> Result<()> {
    let data = arns_record_info.try_borrow_data()?;
    // Layout: disc(8) + name_hash(32) + owner(32) + ant(32) + purchase_type(1)
    //       + start_ts(8) + end_ts(Option<i64>=1+8) + undername_limit(2)
    //       + purchase_price(8) + bump(1) + name(4+N)
    let purchase_type_offset: usize = 8 + 32 + 32 + 32; // 104
    if data.len() > purchase_type_offset {
        let purchase_type = data[purchase_type_offset];
        // 0 = Lease, 1 = Permabuy
        if purchase_type == 0 {
            // Lease: check end_timestamp + grace period
            let end_ts_offset = purchase_type_offset + 1 + 8; // skip purchase_type(1) + start_timestamp(8) = 113
            // end_timestamp is Option<i64>: 1 byte discriminant + 8 bytes
            if data.len() >= end_ts_offset + 1 + 8 && data[end_ts_offset] == 1 {
                let end_ts = i64::from_le_bytes(
                    data[end_ts_offset + 1..end_ts_offset + 9].try_into().unwrap_or([0; 8])
                );
                let grace_end = end_ts.checked_add(constants::LEASE_GRACE_PERIOD).unwrap_or(i64::MAX);
                require!(
                    grace_end >= current_timestamp,
                    ArioError::ArnsRecordNotFound
                );
            }
        }
        // Permabuy (purchase_type == 1) is always active — no check needed
    }
    Ok(())
}

/// Read demand factor from an account info, erroring if the account is invalid.
/// Callers MUST pass the correct DemandFactor PDA from ario-arns.
fn read_demand_factor(df_info: &AccountInfo, arns_program_id: &Pubkey) -> Result<u64> {
    use anchor_lang::solana_program::hash::hash as sha256_hash;

    require!(
        *df_info.owner == *arns_program_id,
        ArioError::InvalidAccountState
    );

    // Validate expected PDA
    let (expected_pda, _) = Pubkey::find_program_address(&[b"demand_factor"], arns_program_id);
    require!(
        df_info.key() == expected_pda,
        ArioError::InvalidAccountState
    );

    let data = df_info
        .try_borrow_data()
        .map_err(|_| error!(ArioError::InvalidAccountState))?;

    require!(data.len() >= 16, ArioError::InvalidAccountState);

    let expected_disc = sha256_hash(b"account:DemandFactor");
    require!(
        data[..8] == expected_disc.to_bytes()[..8],
        ArioError::InvalidAccountState
    );

    let df_bytes: [u8; 8] = data[8..16].try_into().unwrap_or([0; 8]);
    Ok(u64::from_le_bytes(df_bytes))
}

/// Read the record owner from an AntRecord PDA in the ANT program named
/// by the asset's `ANT Program` attribute (or `ario_ant::ID` fallback).
///
/// `ant_program` selects which program owns the AntRecord PDA — derived
/// from the asset's Metaplex Core Attributes plugin in callers, with a
/// canonical fallback. Hard-coding `ario_ant::ID` here would defeat the
/// asset-side pluggability (ADR-016 / BD-100) — the AntRecord PDA lives
/// in whichever program manages the asset's per-mint state.
///
/// Returns Some(owner) if the record exists and has a delegated owner,
/// None otherwise.
///
/// Borsh layout (mirrors `ario_ant::state::AntRecord` — keep in sync with
/// `programs/ario-ant/src/state.rs`):
///   8     discriminator: hash("account:AntRecord")[..8]
///   32    mint: Pubkey
///   4+N   undername: String (u32 le len + bytes)
///   4+N   target: String
///   1     target_protocol: u8
///   4     ttl_seconds: u32
///   1+[4] priority: Option<u32> (0x00 = None, 0x01 + u32 = Some)
///   1+[32] owner: Option<Pubkey> (0x00 = None, 0x01 + 32 bytes = Some)
///   32    last_reconciled_owner — not read here
///   1     bump — not read here
///
/// The layout-pin test `test_ant_record_layout_parse_pin` exercises this
/// parser end-to-end. If `AntRecord` field order changes before `owner`,
/// that test fails — refresh this helper and its comment.
///
/// Conformance: third-party ANT programs MUST keep AntRecord's prefix
/// (mint, undername, target, target_protocol, ttl_seconds, priority,
/// owner) byte-compatible with the canonical `ario_ant`. Anything else
/// makes them invisible to ario-core's BD-097 path.
fn read_ant_record_owner(
    ant_record_info: &AccountInfo,
    ant_mint: &Pubkey,
    undername: &str,
    ant_program: &Pubkey,
) -> Result<Option<Pubkey>> {
    require!(
        ant_record_info.owner == ant_program,
        ArioError::InvalidAccountState
    );

    let undername_hash =
        anchor_lang::solana_program::hash::hash(undername.to_lowercase().as_bytes());
    let (expected_pda, _) = Pubkey::find_program_address(
        &[b"ant_record", ant_mint.as_ref(), undername_hash.as_ref()],
        ant_program,
    );
    require!(
        ant_record_info.key() == expected_pda,
        ArioError::InvalidAccountState
    );

    require!(
        !ant_record_info.data_is_empty(),
        ArioError::InvalidAccountState
    );
    let data = ant_record_info
        .try_borrow_data()
        .map_err(|_| error!(ArioError::InvalidAccountState))?;
    require!(data.len() >= 8, ArioError::InvalidAccountState);
    let expected_disc = anchor_lang::solana_program::hash::hash(b"account:AntRecord");
    require!(
        data[..8] == expected_disc.to_bytes()[..8],
        ArioError::InvalidAccountState
    );

    // disc(8) + mint(32)
    let mut offset = 8usize
        .checked_add(32)
        .ok_or(ArioError::InvalidAccountState)?;

    // undername: String
    require!(
        data.len()
            >= offset
                .checked_add(4)
                .ok_or(ArioError::InvalidAccountState)?,
        ArioError::InvalidAccountState
    );
    let undername_len = u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .map_err(|_| ArioError::InvalidAccountState)?,
    ) as usize;
    offset = offset
        .checked_add(4)
        .and_then(|o| o.checked_add(undername_len))
        .ok_or(ArioError::InvalidAccountState)?;

    // target: String
    require!(
        data.len()
            >= offset
                .checked_add(4)
                .ok_or(ArioError::InvalidAccountState)?,
        ArioError::InvalidAccountState
    );
    let target_len = u32::from_le_bytes(
        data[offset..offset + 4]
            .try_into()
            .map_err(|_| ArioError::InvalidAccountState)?,
    ) as usize;
    offset = offset
        .checked_add(4)
        .and_then(|o| o.checked_add(target_len))
        .ok_or(ArioError::InvalidAccountState)?;

    // target_protocol(1) + ttl_seconds(4)
    offset = offset
        .checked_add(5)
        .ok_or(ArioError::InvalidAccountState)?;

    // priority: Option<u32>
    require!(data.len() > offset, ArioError::InvalidAccountState);
    let priority_size = if data[offset] == 0 { 1 } else { 5 };
    offset = offset
        .checked_add(priority_size)
        .ok_or(ArioError::InvalidAccountState)?;

    // owner: Option<Pubkey>
    require!(data.len() > offset, ArioError::InvalidAccountState);
    if data[offset] == 0 {
        Ok(None)
    } else {
        let owner_end = offset
            .checked_add(1)
            .and_then(|o| o.checked_add(32))
            .ok_or(ArioError::InvalidAccountState)?;
        require!(data.len() >= owner_end, ArioError::InvalidAccountState);
        let owner = Pubkey::try_from(&data[offset + 1..offset + 33])
            .map_err(|_| error!(ArioError::InvalidAccountState))?;
        Ok(Some(owner))
    }
}

pub mod request_primary_name {
    use super::*;

    pub fn handler(ctx: Context<RequestPrimaryName>, name: String) -> Result<()> {
        let clock = Clock::get()?;
        let config = &ctx.accounts.config;

        // GAP-1/4/7: Validate primary name format (base name + undername rules)
        validate_primary_name_format(&name)?;

        // GAP-1: Validate base ArNS record exists
        let name_lower = name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };

        let remaining = ctx.remaining_accounts;
        require!(!remaining.is_empty(), ArioError::InvalidParameter);

        let arns_record_info = &remaining[0];
        validate_arns_record_exists(
            arns_record_info,
            &config.arns_program,
            base_name,
            clock.unix_timestamp,
        )?;

        // M1: Read demand factor from remaining_accounts[1] (mandatory)
        require!(remaining.len() > 1, ArioError::InvalidParameter);
        let demand_factor = read_demand_factor(&remaining[1], &config.arns_program)?;

        let fee = u64::try_from(
            (ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE as u128)
                .checked_mul(demand_factor as u128)
                .ok_or(ArioError::ArithmeticOverflow)?
                .checked_div(1_000_000u128) // DEMAND_FACTOR_SCALE
                .ok_or(ArioError::ArithmeticOverflow)?,
        )
        .map_err(|_| ArioError::ArithmeticOverflow)?;

        if fee > 0 {
            let cpi_accounts = SplTransfer {
                from: ctx.accounts.initiator_token_account.to_account_info(),
                to: ctx.accounts.protocol_token_account.to_account_info(),
                authority: ctx.accounts.initiator.to_account_info(),
            };
            let cpi_ctx =
                CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
            token::transfer(cpi_ctx, fee)?;
        }

        let request = &mut ctx.accounts.request;
        request.initiator = ctx.accounts.initiator.key();
        request.name = name.to_lowercase();
        request.created_at = clock.unix_timestamp;
        request.expires_at = clock
            .unix_timestamp
            .checked_add(config.primary_name_request_expiry)
            .ok_or(ArioError::ArithmeticOverflow)?;
        request.bump = ctx.bumps.request;

        emit!(PrimaryNameRequestedEvent {
            initiator: request.initiator,
            name: request.name.clone(),
            fee,
            request_pda: ctx.accounts.request.key(),
            funding_source: crate::FUNDING_SOURCE_BALANCE,
            timestamp: clock.unix_timestamp,
        });

        msg!(
            "Primary name request created for: {} (fee: {} mARIO)",
            name,
            fee
        );
        Ok(())
    }
}

pub mod request_and_set_primary_name {
    use super::*;

    pub fn handler(
        ctx: Context<RequestAndSetPrimaryName>,
        name: String,
        reverse_lookup_hash: [u8; 32],
        ant_program_id: Pubkey,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let config = &ctx.accounts.config;

        // GAP-1/4/7: Validate primary name format
        validate_primary_name_format(&name)?;

        let expected =
            anchor_lang::solana_program::hash::hash(name.to_lowercase().as_bytes()).to_bytes();
        require!(reverse_lookup_hash == expected, ArioError::InvalidParameter);

        // Validate that initiator owns the base ArNS name
        let remaining = ctx.remaining_accounts;
        require!(!remaining.is_empty(), ArioError::InvalidParameter);
        let arns_record_info = &remaining[0];

        let arns_program_id = config.arns_program;
        let name_lower = name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };

        // Validate ArNS record exists
        validate_arns_record_exists(
            arns_record_info,
            &arns_program_id,
            base_name,
            clock.unix_timestamp,
        )?;

        // Read the ant mint from ArnsRecord (opaque pubkey to ario-core).
        // Layout: disc(8) + name_hash(32) + owner(32) + ant(32) + ...
        let ant = {
            let data = arns_record_info.try_borrow_data()?;
            let ant_offset: usize = 8 + 32 + 32; // 72
            require!(data.len() >= ant_offset + 32, ArioError::InvalidAccountState);

            // M-4: Validate ArnsRecord discriminator
            let expected_disc = anchor_lang::solana_program::hash::hash(b"account:ArnsRecord");
            require!(
                data[..8] == expected_disc.to_bytes()[..8],
                ArioError::InvalidAccountState
            );

            Pubkey::try_from(&data[ant_offset..ant_offset + 32])
                .map_err(|_| ArioError::InvalidAccountState)?
        };

        // ADR-016 reshape: ario-core is MPL-agnostic. Authorization is
        // "caller is the AntRecord.owner for this name" via PDA-seed-pinned
        // lookup at remaining_accounts[2]. The undername part of the name
        // selects which AntRecord to check (full base names use the "@"
        // sentinel which the canonical ario-ant creates at mint time).
        require!(remaining.len() > 2, ArioError::UndernameRecordOwnerRequired);
        let undername = if parts.len() == 2 { parts[0] } else { "@" };
        let initiator_key = ctx.accounts.initiator.key();
        let record_owner = read_ant_record_owner(&remaining[2], &ant, undername, &ant_program_id)?;
        require!(record_owner == Some(initiator_key), ArioError::NotAntHolder);

        // Read demand factor from remaining_accounts[1] (mandatory)
        require!(remaining.len() > 1, ArioError::InvalidParameter);
        let demand_factor = read_demand_factor(&remaining[1], &config.arns_program)?;

        // Charge fee
        let fee = u64::try_from(
            (ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE as u128)
                .checked_mul(demand_factor as u128)
                .ok_or(ArioError::ArithmeticOverflow)?
                .checked_div(1_000_000u128)
                .ok_or(ArioError::ArithmeticOverflow)?,
        )
        .map_err(|_| ArioError::ArithmeticOverflow)?;

        if fee > 0 {
            let cpi_accounts = SplTransfer {
                from: ctx.accounts.initiator_token_account.to_account_info(),
                to: ctx.accounts.protocol_token_account.to_account_info(),
                authority: ctx.accounts.initiator.to_account_info(),
            };
            let cpi_ctx =
                CpiContext::new(ctx.accounts.token_program.to_account_info(), cpi_accounts);
            token::transfer(cpi_ctx, fee)?;
        }

        // CORE-008: If user already has a primary name set to a DIFFERENT name,
        // they must call remove_primary_name first to close the old PrimaryNameReverse.
        // This prevents orphaned reverse lookup entries.
        let primary_name = &mut ctx.accounts.primary_name;
        if primary_name.owner != Pubkey::default() && primary_name.name != name_lower {
            return Err(ArioError::MustRemoveExistingPrimaryName.into());
        }

        // Set primary name directly (auto-approve)
        primary_name.owner = ctx.accounts.initiator.key();
        primary_name.name = name.to_lowercase();
        primary_name.set_at = clock.unix_timestamp;
        primary_name.bump = ctx.bumps.primary_name;

        // BUG-1: Enforce primary name uniqueness via reverse lookup
        let reverse = &mut ctx.accounts.primary_name_reverse;
        if reverse.owner != Pubkey::default() && reverse.owner != ctx.accounts.initiator.key() {
            return Err(ArioError::PrimaryNameAlreadySet.into());
        }
        reverse.name = name.to_lowercase();
        reverse.owner = ctx.accounts.initiator.key();
        reverse.bump = ctx.bumps.primary_name_reverse;

        emit!(PrimaryNameSetEvent {
            owner: primary_name.owner,
            name: primary_name.name.clone(),
            timestamp: clock.unix_timestamp,
        });

        msg!(
            "Primary name '{}' set (auto-approved, fee: {} mARIO)",
            name,
            fee
        );
        Ok(())
    }
}

pub mod approve_primary_name {
    use super::*;

    pub fn handler(
        ctx: Context<ApprovePrimaryName>,
        reverse_lookup_hash: [u8; 32],
        ant_program_id: Pubkey,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let request = &ctx.accounts.request;

        let expected = anchor_lang::solana_program::hash::hash(request.name.as_bytes()).to_bytes();
        require!(reverse_lookup_hash == expected, ArioError::InvalidParameter);

        require!(
            !request.is_expired(clock.unix_timestamp),
            ArioError::PrimaryNameRequestExpired
        );

        // Validate that name_owner actually controls the requested ArNS name.
        // The ArnsRecord account must be passed as remaining_accounts[0].
        let remaining = ctx.remaining_accounts;
        require!(!remaining.is_empty(), ArioError::InvalidParameter);
        let arns_record_info = &remaining[0];

        // Verify the account is owned by the ario-arns program
        let arns_program_id = ctx.accounts.config.arns_program;
        require!(
            *arns_record_info.owner == arns_program_id,
            ArioError::InvalidAccountState
        );

        // Derive expected PDA for the base name
        // Base name = part after first underscore (or whole name if no underscore)
        let name_lower = request.name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };
        let name_hash = anchor_lang::solana_program::hash::hash(base_name.as_bytes());
        let (expected_pda, _) =
            Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
        require!(
            arns_record_info.key() == expected_pda,
            ArioError::InvalidAccountState
        );

        // BUG-2: Verify the ArNS record is still active (lease not expired)
        verify_arns_record_active(arns_record_info, clock.unix_timestamp)?;

        // Read the ANT pubkey from ArnsRecord.
        // Layout: disc(8) + name_hash(32) + owner(32) + ant(32) + ...
        let ant = {
            let data = arns_record_info.try_borrow_data()?;
            let ant_offset: usize = 8 + 32 + 32; // 72
            require!(data.len() >= ant_offset + 32, ArioError::InvalidAccountState);

            // M-4: Validate ArnsRecord discriminator
            let expected_disc = anchor_lang::solana_program::hash::hash(b"account:ArnsRecord");
            require!(
                data[..8] == expected_disc.to_bytes()[..8],
                ArioError::InvalidAccountState
            );


            Pubkey::try_from(&data[ant_offset..ant_offset + 32])
                .map_err(|_| ArioError::InvalidAccountState)?
        };

        // ADR-016 reshape: ario-core is MPL-agnostic. Authorization is
        // "approver is the AntRecord.owner for this name" via PDA-seed-pinned
        // lookup at remaining_accounts[1]. The undername part of the name
        // selects which AntRecord (the canonical ario-ant uses "@" for base
        // names). `ant_program_id` is supplied by the caller, but the PDA
        // seed check pins (program_id, ant_mint, undername) — lying about it
        // fails the seed match.
        require!(remaining.len() > 1, ArioError::UndernameRecordOwnerRequired);
        let undername = if parts.len() == 2 { parts[0] } else { "@" };
        let approver_key = ctx.accounts.name_owner.key();
        let record_owner = read_ant_record_owner(&remaining[1], &ant, undername, &ant_program_id)?;
        require!(record_owner == Some(approver_key), ArioError::NotAntHolder);

        // CORE-008: If user already has a primary name set to a DIFFERENT name,
        // they must call remove_primary_name first to close the old PrimaryNameReverse.
        let primary_name = &mut ctx.accounts.primary_name;
        if primary_name.owner != Pubkey::default() && primary_name.name != request.name {
            return Err(ArioError::MustRemoveExistingPrimaryName.into());
        }

        // Set primary name
        primary_name.owner = request.initiator;
        primary_name.name = request.name.clone();
        primary_name.set_at = clock.unix_timestamp;
        primary_name.bump = ctx.bumps.primary_name;

        // BUG-1: Enforce primary name uniqueness via reverse lookup
        let reverse = &mut ctx.accounts.primary_name_reverse;
        if reverse.owner != Pubkey::default() && reverse.owner != request.initiator {
            return Err(ArioError::PrimaryNameAlreadySet.into());
        }
        reverse.name = request.name.clone();
        reverse.owner = request.initiator;
        reverse.bump = ctx.bumps.primary_name_reverse;

        emit!(PrimaryNameSetEvent {
            owner: primary_name.owner,
            name: primary_name.name.clone(),
            timestamp: clock.unix_timestamp,
        });

        // Request account will be closed by close constraint

        Ok(())
    }
}

pub mod close_expired_request {
    use super::*;

    pub fn handler(ctx: Context<CloseExpiredRequest>) -> Result<()> {
        let clock = Clock::get()?;
        let request = &ctx.accounts.request;
        require!(
            request.is_expired(clock.unix_timestamp),
            ArioError::PrimaryNameRequestNotExpired
        );

        // Capture rent that will be refunded to the initiator. Anchor's
        // `close = initiator` constraint runs after the handler returns,
        // so the request account still holds the rent reservation here.
        // The fee that was charged at request time is NOT refunded — only
        // the rent reservation comes back.
        let refunded = ctx.accounts.request.to_account_info().lamports();
        let initiator = request.initiator;
        let name = request.name.clone();

        emit!(PrimaryNameRequestExpiredEvent {
            initiator,
            name,
            refunded,
            timestamp: clock.unix_timestamp,
        });

        // Account closed by close constraint, rent returned to payer
        msg!("Expired primary name request closed for: {}", initiator);
        Ok(())
    }
}

pub mod remove_primary_name {
    use super::*;

    pub fn handler(ctx: Context<RemovePrimaryName>, reverse_lookup_hash: [u8; 32]) -> Result<()> {
        let expected = anchor_lang::solana_program::hash::hash(
            ctx.accounts.primary_name.name.to_lowercase().as_bytes(),
        )
        .to_bytes();
        require!(reverse_lookup_hash == expected, ArioError::InvalidParameter);

        // Holder-removal path: caller == owner. Same event shape as the
        // base-name override path (`remove_primary_name_for_base_name`)
        // — consumers branch on `caller == owner` to distinguish.
        let owner = ctx.accounts.primary_name.owner;
        let name = ctx.accounts.primary_name.name.clone();
        let caller = ctx.accounts.owner.key();
        let clock = Clock::get()?;
        emit!(PrimaryNameRemovedEvent {
            owner,
            name,
            caller,
            timestamp: clock.unix_timestamp,
        });

        msg!("Primary name removed for: {}", caller);
        // Account closed by close constraint
        Ok(())
    }
}

pub mod remove_primary_name_for_base_name {
    use super::*;

    pub fn handler(
        ctx: Context<RemovePrimaryNameForBaseName>,
        reverse_lookup_hash: [u8; 32],
        ant_program_id: Pubkey,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let primary_name = &ctx.accounts.primary_name;
        let config = &ctx.accounts.config;

        let expected =
            anchor_lang::solana_program::hash::hash(primary_name.name.to_lowercase().as_bytes())
                .to_bytes();
        require!(reverse_lookup_hash == expected, ArioError::InvalidParameter);

        // Extract base name from the primary name (part after first underscore)
        let name_lower = primary_name.name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };

        // Validate that name_owner controls the base ArNS name via remaining_accounts[0]
        let remaining = ctx.remaining_accounts;
        require!(!remaining.is_empty(), ArioError::InvalidParameter);
        let arns_record_info = &remaining[0];

        // Verify the account is owned by the ario-arns program
        let arns_program_id = config.arns_program;
        require!(
            *arns_record_info.owner == arns_program_id,
            ArioError::InvalidAccountState
        );

        // Derive expected PDA for the base name
        let name_hash = anchor_lang::solana_program::hash::hash(base_name.as_bytes());
        let (expected_pda, _) =
            Pubkey::find_program_address(&[b"arns_record", name_hash.as_ref()], &arns_program_id);
        require!(
            arns_record_info.key() == expected_pda,
            ArioError::InvalidAccountState
        );

        // BUG-2: Verify the ArNS record is still active (lease not expired)
        verify_arns_record_active(arns_record_info, clock.unix_timestamp)?;

        // Read the ANT pubkey from ArnsRecord.
        // Layout: disc(8) + name_hash(32) + owner(32) + ant(32) + ...
        let ant = {
            let data = arns_record_info.try_borrow_data()?;
            let ant_offset: usize = 8 + 32 + 32; // 72
            require!(data.len() >= ant_offset + 32, ArioError::InvalidAccountState);

            // M-4: Validate ArnsRecord discriminator
            let expected_disc = anchor_lang::solana_program::hash::hash(b"account:ArnsRecord");
            require!(
                data[..8] == expected_disc.to_bytes()[..8],
                ArioError::InvalidAccountState
            );

            Pubkey::try_from(&data[ant_offset..ant_offset + 32])
                .map_err(|_| ArioError::InvalidAccountState)?
        };

        // ADR-016 reshape: ario-core is MPL-agnostic. The base-name owner is
        // the AntRecord.owner for the BASE name's "@" undername (the canonical
        // ario-ant creates this record at mint time). PDA seeds pin
        // (program_id, ant_mint, "@") so a caller lying about
        // `ant_program_id` fails the seed check at remaining_accounts[1].
        require!(remaining.len() > 1, ArioError::UndernameRecordOwnerRequired);
        let caller_key = ctx.accounts.name_owner.key();
        let record_owner = read_ant_record_owner(&remaining[1], &ant, "@", &ant_program_id)?;
        require!(record_owner == Some(caller_key), ArioError::NotAntHolder);

        // Base-name override path: caller != owner (the base-name holder
        // is revoking someone else's primary name). Same event shape as
        // the holder-removal path so consumers handle one type.
        let owner = primary_name.owner;
        let name = primary_name.name.clone();
        let caller = ctx.accounts.name_owner.key();
        emit!(PrimaryNameRemovedEvent {
            owner,
            name,
            caller,
            timestamp: clock.unix_timestamp,
        });

        msg!(
            "Primary name '{}' removed by base name owner {}",
            ctx.accounts.primary_name.name,
            caller
        );
        // PrimaryName account closed by close constraint
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS
// =========================================

#[derive(Accounts)]
#[instruction(name: String)]
pub struct RequestPrimaryName<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArioConfig>,

    #[account(
        init,
        payer = initiator,
        space = PrimaryNameRequest::SIZE,
        seeds = [PRIMARY_NAME_REQUEST_SEED, initiator.key().as_ref()],
        bump,
    )]
    pub request: Account<'info, PrimaryNameRequest>,

    /// M1: Initiator's token account for fee payment
    #[account(
        mut,
        constraint = initiator_token_account.owner == initiator.key(),
        constraint = initiator_token_account.mint == config.mint,
    )]
    pub initiator_token_account: Account<'info, TokenAccount>,

    /// M1: Protocol token account to receive fee
    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArioError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    #[account(mut)]
    pub initiator: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(reverse_lookup_hash: [u8; 32])]
pub struct ApprovePrimaryName<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArioConfig>,

    #[account(
        mut,
        seeds = [PRIMARY_NAME_REQUEST_SEED, request.initiator.as_ref()],
        bump = request.bump,
        close = initiator,
    )]
    pub request: Account<'info, PrimaryNameRequest>,

    /// CHECK: Receives rent refund, validated against request.initiator
    #[account(mut, constraint = initiator.key() == request.initiator @ ArioError::InvalidParameter)]
    pub initiator: UncheckedAccount<'info>,

    /// GAP-2: init_if_needed allows overwriting existing primary name (matches Lua auto-remove)
    #[account(
        init_if_needed,
        payer = name_owner,
        space = PrimaryName::SIZE,
        seeds = [PRIMARY_NAME_SEED, request.initiator.as_ref()],
        bump,
    )]
    pub primary_name: Account<'info, PrimaryName>,

    /// BUG-1: Reverse lookup PDA for uniqueness enforcement
    /// Seeds: ["primary_name_reverse", hash(request.name.to_lowercase())]
    #[account(
        init_if_needed,
        payer = name_owner,
        space = PrimaryNameReverse::SIZE,
        seeds = [PRIMARY_NAME_REVERSE_SEED, reverse_lookup_hash.as_ref()],
        bump,
    )]
    pub primary_name_reverse: Account<'info, PrimaryNameReverse>,

    /// Name owner — must be the ANT NFT holder (Metaplex Core asset).
    /// Client must pass ArnsRecord as remaining_accounts[0] and ANT asset as remaining_accounts[1].
    #[account(mut)]
    pub name_owner: Signer<'info>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(reverse_lookup_hash: [u8; 32])]
pub struct RemovePrimaryName<'info> {
    #[account(
        mut,
        seeds = [PRIMARY_NAME_SEED, owner.key().as_ref()],
        bump = primary_name.bump,
        close = owner,
        constraint = primary_name.owner == owner.key() @ ArioError::InvalidOwner,
    )]
    pub primary_name: Account<'info, PrimaryName>,

    /// BUG-1: Close reverse lookup on primary name removal
    #[account(
        mut,
        seeds = [PRIMARY_NAME_REVERSE_SEED, reverse_lookup_hash.as_ref()],
        bump = primary_name_reverse.bump,
        close = owner,
    )]
    pub primary_name_reverse: Account<'info, PrimaryNameReverse>,

    #[account(mut)]
    pub owner: Signer<'info>,
}

/// M2: Request and set primary name in one tx (auto-approve for base name owners).
#[derive(Accounts)]
#[instruction(name: String, reverse_lookup_hash: [u8; 32])]
pub struct RequestAndSetPrimaryName<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Box<Account<'info, ArioConfig>>,

    /// GAP-2: init_if_needed allows overwriting existing primary name (matches Lua auto-remove)
    #[account(
        init_if_needed,
        payer = initiator,
        space = PrimaryName::SIZE,
        seeds = [PRIMARY_NAME_SEED, initiator.key().as_ref()],
        bump,
    )]
    pub primary_name: Box<Account<'info, PrimaryName>>,

    /// BUG-1: Reverse lookup PDA for uniqueness enforcement
    #[account(
        init_if_needed,
        payer = initiator,
        space = PrimaryNameReverse::SIZE,
        seeds = [PRIMARY_NAME_REVERSE_SEED, reverse_lookup_hash.as_ref()],
        bump,
    )]
    pub primary_name_reverse: Box<Account<'info, PrimaryNameReverse>>,

    /// Initiator's token account for fee payment
    #[account(
        mut,
        constraint = initiator_token_account.owner == initiator.key(),
        constraint = initiator_token_account.mint == config.mint,
    )]
    pub initiator_token_account: Box<Account<'info, TokenAccount>>,

    /// Protocol token account to receive fee
    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArioError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    #[account(mut)]
    pub initiator: Signer<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// H1: Base name owner can remove primary names using their domain.
#[derive(Accounts)]
#[instruction(reverse_lookup_hash: [u8; 32])]
pub struct RemovePrimaryNameForBaseName<'info> {
    #[account(
        seeds = [CONFIG_SEED],
        bump = config.bump,
    )]
    pub config: Account<'info, ArioConfig>,

    #[account(
        mut,
        seeds = [PRIMARY_NAME_SEED, primary_name.owner.as_ref()],
        bump = primary_name.bump,
        close = original_owner,
    )]
    pub primary_name: Account<'info, PrimaryName>,

    /// BUG-1: Close reverse lookup on primary name removal
    #[account(
        mut,
        seeds = [PRIMARY_NAME_REVERSE_SEED, reverse_lookup_hash.as_ref()],
        bump = primary_name_reverse.bump,
        close = original_owner,
    )]
    pub primary_name_reverse: Account<'info, PrimaryNameReverse>,

    /// CHECK: Receives rent refund, validated against primary_name.owner
    #[account(mut, constraint = original_owner.key() == primary_name.owner @ ArioError::InvalidParameter)]
    pub original_owner: UncheckedAccount<'info>,

    /// The ANT NFT holder (must hold the Metaplex Core asset for the ArNS name — validated in handler).
    /// Client must pass ArnsRecord as remaining_accounts[0] and ANT asset as remaining_accounts[1].
    #[account(mut)]
    pub name_owner: Signer<'info>,
}

#[derive(Accounts)]
pub struct CloseExpiredRequest<'info> {
    #[account(
        mut,
        seeds = [PRIMARY_NAME_REQUEST_SEED, request.initiator.as_ref()],
        bump = request.bump,
        close = initiator,
    )]
    pub request: Account<'info, PrimaryNameRequest>,

    /// CHECK: Receives rent refund, validated against request.initiator
    #[account(mut, constraint = initiator.key() == request.initiator @ ArioError::InvalidParameter)]
    pub initiator: UncheckedAccount<'info>,

    /// Anyone can close an expired request (permissionless)
    #[account(mut)]
    pub payer: Signer<'info>,
}

// =========================================
// TESTS
// =========================================

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================
    // Primary Name Format Validation Tests
    // =========================================

    #[test]
    fn valid_simple_base_name() {
        assert!(validate_primary_name_format("arweave").is_ok());
    }

    #[test]
    fn valid_single_char_name() {
        assert!(validate_primary_name_format("a").is_ok());
    }

    #[test]
    fn valid_name_with_hyphens() {
        assert!(validate_primary_name_format("my-name").is_ok());
    }

    #[test]
    fn valid_undername_with_base() {
        assert!(validate_primary_name_format("alice_arweave").is_ok());
    }

    #[test]
    fn valid_undername_with_hyphens() {
        assert!(validate_primary_name_format("my-sub_my-domain").is_ok());
    }

    #[test]
    fn invalid_empty_name() {
        assert!(validate_primary_name_format("").is_err());
    }

    #[test]
    fn invalid_name_too_long() {
        let long_name = "a".repeat(64);
        assert!(validate_primary_name_format(&long_name).is_err());
    }

    #[test]
    fn valid_name_at_max_length() {
        // 63 chars total: undername(11) + _ + basename(51)
        let un = "a".repeat(11);
        let base = "b".repeat(51);
        let name = format!("{}_{}", un, base);
        assert_eq!(name.len(), 63);
        assert!(validate_primary_name_format(&name).is_ok());
    }

    #[test]
    fn invalid_pure_basename_over_51() {
        // A 52-char name with no underscore exceeds base name limit
        let name = "a".repeat(52);
        assert!(validate_primary_name_format(&name).is_err());
    }

    #[test]
    fn invalid_name_starts_with_hyphen() {
        assert!(validate_primary_name_format("-name").is_err());
    }

    #[test]
    fn invalid_name_ends_with_hyphen() {
        assert!(validate_primary_name_format("name-").is_err());
    }

    #[test]
    fn invalid_undername_starts_with_hyphen() {
        assert!(validate_primary_name_format("-bad_arweave").is_err());
    }

    #[test]
    fn invalid_empty_base_after_underscore() {
        assert!(validate_primary_name_format("alice_").is_err());
    }

    #[test]
    fn invalid_base_name_too_long() {
        // base name > 51 chars
        let base = "a".repeat(52);
        assert!(validate_primary_name_format(&base).is_err());
    }

    #[test]
    fn valid_base_name_at_max_51() {
        let base = "a".repeat(51);
        assert!(validate_primary_name_format(&base).is_ok());
    }

    #[test]
    fn invalid_undername_too_long() {
        // undername > 61 chars, with a short base name (total under 63 would be fine otherwise)
        // But 62 char undername + _ + 1 char base = 64 > 63, so also fails length.
        // Use a case that fits total but undername exceeds 61.
        // Actually undername=62 + "_" + "a" = 64 > 63 total, always fails.
        // So this gap is only testable with MAX_NAME_LENGTH > 63. Document: the 63 total cap
        // naturally limits undername to 61 (since we need _ + at least 1 char base).
        // Test the boundary: undername=61 + _ + a = 63 ✓
        let un = "a".repeat(61);
        let name = format!("{}_a", un);
        assert_eq!(name.len(), 63);
        assert!(validate_primary_name_format(&name).is_ok());
    }

    #[test]
    fn valid_name_segment_function() {
        assert!(is_valid_name_segment("abc"));
        assert!(is_valid_name_segment("a"));
        assert!(is_valid_name_segment("a-b"));
        assert!(is_valid_name_segment("abc123"));
        assert!(!is_valid_name_segment("-abc"));
        assert!(!is_valid_name_segment("abc-"));
        assert!(!is_valid_name_segment(""));
        assert!(!is_valid_name_segment("a b"));
    }

    // =========================================
    // BUG-3: Base Name Length 43 Prohibition
    // =========================================

    #[test]
    fn invalid_base_name_length_43() {
        // Length 43 is prohibited (Arweave address collision)
        let name = "a".repeat(43);
        assert!(validate_primary_name_format(&name).is_err());
    }

    #[test]
    fn invalid_undername_with_43_char_base() {
        // Undername with 43-char base should also fail
        let base = "a".repeat(43);
        let name = format!("sub_{}", base);
        assert!(validate_primary_name_format(&name).is_err());
    }

    #[test]
    fn valid_base_name_length_42() {
        let name = "a".repeat(42);
        assert!(validate_primary_name_format(&name).is_ok());
    }

    #[test]
    fn valid_base_name_length_44() {
        let name = "a".repeat(44);
        assert!(validate_primary_name_format(&name).is_ok());
    }
}

// =========================================================================
// Phase 3: primary-name fund-from-funding-plan variants
// =========================================================================
//
// Lua-faithful port: `primaryNames.createPrimaryNameRequest` calls
// `gar.getFundingPlan` + `gar.applyFundingPlan` with no parallel single-source
// path. We mirror that here with two new ix that CPI into ario-gar's
// `pay_from_funding_plan`. No `_from_delegation` / `_from_operator_stake` /
// `_from_withdrawal` variants — a 1-source plan covers single-source semantics
// with marginal CU cost.
//
// remaining_accounts layout for both ix:
//   [0..validation_account_count) — primary-name validation accounts
//                                   (ArnsRecord + DemandFactor [+ ant_asset
//                                   [+ AntRecord]] depending on the variant)
//   [validation_account_count..)   — per-source PDAs forwarded to ario-gar
//                                   (Delegation / Withdrawal)
// The validation slice keeps the existing handler logic untouched; the funding
// slice is forwarded via `with_remaining_accounts` to pay_from_funding_plan.

pub mod request_primary_name_from_funding_plan {
    use super::*;

    pub fn handler<'info>(
        ctx: Context<'_, '_, 'info, 'info, RequestPrimaryNameFromFundingPlan<'info>>,
        name: String,
        sources: Vec<ario_gar::FundingSourceSpec>,
        validation_account_count: u8,
        residue_vault_count: u8,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let config = &ctx.accounts.config;

        validate_primary_name_format(&name)?;

        let name_lower = name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };

        // Split remaining_accounts: first `validation_account_count` for
        // primary-name validation; rest for funding-plan source PDAs.
        let split = validation_account_count as usize;
        require!(
            split <= ctx.remaining_accounts.len(),
            ArioError::InvalidParameter
        );
        let (validation_accounts, funding_source_accounts) = ctx.remaining_accounts.split_at(split);

        // Validation slice mirrors request_primary_name's layout:
        // [0] ArnsRecord (mandatory), [1] DemandFactor (mandatory).
        require!(validation_accounts.len() >= 2, ArioError::InvalidParameter);
        validate_arns_record_exists(
            &validation_accounts[0],
            &config.arns_program,
            base_name,
            clock.unix_timestamp,
        )?;
        let demand_factor = read_demand_factor(&validation_accounts[1], &config.arns_program)?;

        let fee = u64::try_from(
            (ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE as u128)
                .checked_mul(demand_factor as u128)
                .ok_or(ArioError::ArithmeticOverflow)?
                .checked_div(1_000_000u128)
                .ok_or(ArioError::ArithmeticOverflow)?,
        )
        .map_err(|_| ArioError::ArithmeticOverflow)?;

        // Even if fee == 0 (demand_factor = 0 edge case), still validate the
        // funding plan to keep the Vec<FundingSourceSpec> consistent.
        if fee > 0 || !sources.is_empty() {
            let gar_accounts = ario_gar::cpi::accounts::PayFromFundingPlan {
                settings: ctx.accounts.gar_settings.clone(),
                stake_token_account: ctx.accounts.stake_token_account.clone(),
                protocol_token_account: ctx.accounts.protocol_token_account.to_account_info(),
                payer_token_account: ctx.accounts.payer_token_account.clone(),
                payer: ctx.accounts.initiator.to_account_info(),
                token_program: ctx.accounts.token_program.to_account_info(),
                withdrawal_counter: ctx.accounts.withdrawal_counter.clone(),
                system_program: ctx.accounts.system_program.to_account_info(),
            };
            let cpi_ctx = CpiContext::new(ctx.accounts.gar_program.clone(), gar_accounts)
                .with_remaining_accounts(funding_source_accounts.to_vec());
            ario_gar::cpi::pay_from_funding_plan(cpi_ctx, sources, fee, residue_vault_count)?;
        }

        let request = &mut ctx.accounts.request;
        request.initiator = ctx.accounts.initiator.key();
        request.name = name.to_lowercase();
        request.created_at = clock.unix_timestamp;
        request.expires_at = clock
            .unix_timestamp
            .checked_add(config.primary_name_request_expiry)
            .ok_or(ArioError::ArithmeticOverflow)?;
        request.bump = ctx.bumps.request;

        emit!(PrimaryNameRequestedEvent {
            initiator: request.initiator,
            name: request.name.clone(),
            fee,
            request_pda: ctx.accounts.request.key(),
            funding_source: crate::FUNDING_SOURCE_FUNDING_PLAN,
            timestamp: clock.unix_timestamp,
        });

        msg!(
            "Primary name request '{}' created via {}-source funding plan (fee: {} mARIO)",
            name,
            ctx.remaining_accounts.len() - split,
            fee
        );
        Ok(())
    }
}

pub mod request_and_set_primary_name_from_funding_plan {
    use super::*;

    pub fn handler<'info>(
        ctx: Context<'_, '_, 'info, 'info, RequestAndSetPrimaryNameFromFundingPlan<'info>>,
        name: String,
        reverse_lookup_hash: [u8; 32],
        sources: Vec<ario_gar::FundingSourceSpec>,
        validation_account_count: u8,
        residue_vault_count: u8,
        ant_program_id: Pubkey,
    ) -> Result<()> {
        let clock = Clock::get()?;
        let config = &ctx.accounts.config;

        validate_primary_name_format(&name)?;

        let expected =
            anchor_lang::solana_program::hash::hash(name.to_lowercase().as_bytes()).to_bytes();
        require!(reverse_lookup_hash == expected, ArioError::InvalidParameter);

        let split = validation_account_count as usize;
        require!(
            split <= ctx.remaining_accounts.len(),
            ArioError::InvalidParameter
        );
        let (validation_accounts, funding_source_accounts) = ctx.remaining_accounts.split_at(split);

        // ADR-016 reshape: validation layout is
        //   [0] ArnsRecord, [1] DemandFactor, [2] AntRecord PDA.
        // Authorization is "caller is the AntRecord.owner for this name"
        // (with the "@" sentinel for base names).
        require!(validation_accounts.len() >= 3, ArioError::InvalidParameter);
        let arns_record_info = &validation_accounts[0];

        let arns_program_id = config.arns_program;
        let name_lower = name.to_lowercase();
        let parts: Vec<&str> = name_lower.splitn(2, '_').collect();
        let base_name = if parts.len() == 2 { parts[1] } else { parts[0] };

        validate_arns_record_exists(
            arns_record_info,
            &arns_program_id,
            base_name,
            clock.unix_timestamp,
        )?;

        // Read the ANT pubkey from ArnsRecord.
        // Layout: disc(8) + name_hash(32) + owner(32) + ant(32) + ...
        let ant = {
            let data = arns_record_info.try_borrow_data()?;
            let ant_offset: usize = 8 + 32 + 32; // 72
            require!(data.len() >= ant_offset + 32, ArioError::InvalidAccountState);
            let expected_disc =
                anchor_lang::solana_program::hash::hash(b"account:ArnsRecord");
            require!(
                data[..8] == expected_disc.to_bytes()[..8],
                ArioError::InvalidAccountState
            );
            Pubkey::try_from(&data[ant_offset..ant_offset + 32])
                .map_err(|_| ArioError::InvalidAccountState)?
        };

        let undername = if parts.len() == 2 { parts[0] } else { "@" };
        let initiator_key = ctx.accounts.initiator.key();
        let record_owner =
            read_ant_record_owner(&validation_accounts[2], &ant, undername, &ant_program_id)?;
        require!(record_owner == Some(initiator_key), ArioError::NotAntHolder);

        let demand_factor = read_demand_factor(&validation_accounts[1], &config.arns_program)?;

        let fee = u64::try_from(
            (ArioConfig::PRIMARY_NAME_REQUEST_BASE_FEE as u128)
                .checked_mul(demand_factor as u128)
                .ok_or(ArioError::ArithmeticOverflow)?
                .checked_div(1_000_000u128)
                .ok_or(ArioError::ArithmeticOverflow)?,
        )
        .map_err(|_| ArioError::ArithmeticOverflow)?;

        if fee > 0 || !sources.is_empty() {
            let gar_accounts = ario_gar::cpi::accounts::PayFromFundingPlan {
                settings: ctx.accounts.gar_settings.clone(),
                stake_token_account: ctx.accounts.stake_token_account.clone(),
                protocol_token_account: ctx.accounts.protocol_token_account.to_account_info(),
                payer_token_account: ctx.accounts.payer_token_account.clone(),
                payer: ctx.accounts.initiator.to_account_info(),
                token_program: ctx.accounts.token_program.to_account_info(),
                withdrawal_counter: ctx.accounts.withdrawal_counter.clone(),
                system_program: ctx.accounts.system_program.to_account_info(),
            };
            let cpi_ctx = CpiContext::new(ctx.accounts.gar_program.clone(), gar_accounts)
                .with_remaining_accounts(funding_source_accounts.to_vec());
            ario_gar::cpi::pay_from_funding_plan(cpi_ctx, sources, fee, residue_vault_count)?;
        }

        let primary_name = &mut ctx.accounts.primary_name;
        if primary_name.owner != Pubkey::default() && primary_name.name != name_lower {
            return Err(ArioError::MustRemoveExistingPrimaryName.into());
        }

        primary_name.owner = ctx.accounts.initiator.key();
        primary_name.name = name.to_lowercase();
        primary_name.set_at = clock.unix_timestamp;
        primary_name.bump = ctx.bumps.primary_name;

        let reverse = &mut ctx.accounts.primary_name_reverse;
        if reverse.owner != Pubkey::default() && reverse.owner != ctx.accounts.initiator.key() {
            return Err(ArioError::PrimaryNameAlreadySet.into());
        }
        reverse.name = name.to_lowercase();
        reverse.owner = ctx.accounts.initiator.key();
        reverse.bump = ctx.bumps.primary_name_reverse;

        emit!(PrimaryNameSetEvent {
            owner: primary_name.owner,
            name: primary_name.name.clone(),
            timestamp: clock.unix_timestamp,
        });

        msg!(
            "Primary name '{}' set via {}-source funding plan (fee: {} mARIO)",
            name,
            ctx.remaining_accounts.len() - split,
            fee
        );
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXTS — Phase 3 funding-plan variants
// =========================================

#[derive(Accounts)]
#[instruction(name: String)]
pub struct RequestPrimaryNameFromFundingPlan<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Account<'info, ArioConfig>,

    #[account(
        init,
        payer = initiator,
        space = PrimaryNameRequest::SIZE,
        seeds = [PRIMARY_NAME_REQUEST_SEED, initiator.key().as_ref()],
        bump,
    )]
    pub request: Account<'info, PrimaryNameRequest>,

    // --- Forwarded to ario-gar pay_from_funding_plan via CPI ---
    /// CHECK: GarSettings PDA — validated by ario-gar
    #[account(mut)]
    pub gar_settings: AccountInfo<'info>,

    /// CHECK: Stake token account — validated by ario-gar
    #[account(mut)]
    pub stake_token_account: AccountInfo<'info>,

    /// Protocol treasury (receives the primary-name fee).
    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArioError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Account<'info, TokenAccount>,

    /// CHECK: Optional payer SPL ATA — required when sources include Balance
    #[account(mut)]
    pub payer_token_account: Option<AccountInfo<'info>>,

    #[account(mut)]
    pub initiator: Signer<'info>,

    /// CHECK: WithdrawalCounter PDA — created/validated by ario-gar's init_if_needed
    #[account(mut)]
    pub withdrawal_counter: AccountInfo<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    // Remaining accounts layout (after fixed accounts above):
    //   1. validation_account_count: ArnsRecord + DemandFactor (2 accounts)
    //   2. Per source (in `sources` declaration order):
    //        - Balance:        0 slots
    //        - Delegation:     2 slots [gateway_pda, delegation_pda]
    //        - OperatorStake:  1 slot  [gateway_pda]
    //        - Withdrawal:     1 slot  [withdrawal_pda]
    //   3. Trailing residue_vault PDAs (count = residue_vault_count, in
    //      Delegation declaration order, only for Delegations going sub-min)
}

#[derive(Accounts)]
#[instruction(name: String, reverse_lookup_hash: [u8; 32])]
pub struct RequestAndSetPrimaryNameFromFundingPlan<'info> {
    #[account(seeds = [CONFIG_SEED], bump = config.bump)]
    pub config: Box<Account<'info, ArioConfig>>,

    #[account(
        init_if_needed,
        payer = initiator,
        space = PrimaryName::SIZE,
        seeds = [PRIMARY_NAME_SEED, initiator.key().as_ref()],
        bump,
    )]
    pub primary_name: Box<Account<'info, PrimaryName>>,

    #[account(
        init_if_needed,
        payer = initiator,
        space = PrimaryNameReverse::SIZE,
        seeds = [PRIMARY_NAME_REVERSE_SEED, reverse_lookup_hash.as_ref()],
        bump,
    )]
    pub primary_name_reverse: Box<Account<'info, PrimaryNameReverse>>,

    /// CHECK: GarSettings PDA — validated by ario-gar
    #[account(mut)]
    pub gar_settings: AccountInfo<'info>,

    /// CHECK: Stake token account
    #[account(mut)]
    pub stake_token_account: AccountInfo<'info>,

    #[account(
        mut,
        constraint = protocol_token_account.key() == config.treasury @ ArioError::InvalidTreasury,
        constraint = protocol_token_account.mint == config.mint,
    )]
    pub protocol_token_account: Box<Account<'info, TokenAccount>>,

    /// CHECK: Optional payer SPL ATA — required when sources include Balance
    #[account(mut)]
    pub payer_token_account: Option<AccountInfo<'info>>,

    #[account(mut)]
    pub initiator: Signer<'info>,

    /// CHECK: WithdrawalCounter PDA
    #[account(mut)]
    pub withdrawal_counter: AccountInfo<'info>,

    /// CHECK: ario-gar program for CPI
    #[account(address = ario_gar::ID)]
    pub gar_program: AccountInfo<'info>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    // Remaining accounts layout (after fixed accounts above):
    //   1. validation_account_count: ArnsRecord + DemandFactor (2 accounts)
    //   2. Per source (in `sources` declaration order):
    //        - Balance:        0 slots
    //        - Delegation:     2 slots [gateway_pda, delegation_pda]
    //        - OperatorStake:  1 slot  [gateway_pda]
    //        - Withdrawal:     1 slot  [withdrawal_pda]
    //   3. Trailing residue_vault PDAs (count = residue_vault_count, in
    //      Delegation declaration order, only for Delegations going sub-min)
}
