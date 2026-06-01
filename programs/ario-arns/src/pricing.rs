// AR.IO ArNS Pricing Calculations
// All monetary values are u64 in mARIO. Uses u128 intermediates to prevent overflow.

use anchor_lang::prelude::*;
use anchor_lang::solana_program::hash::hash;

use crate::error::ArnsError;
use crate::state::*;

// =========================================
// PRICING CONSTANTS
// =========================================

/// Scale factor for fixed-point arithmetic (1_000_000 = 1.0)
pub const DEMAND_FACTOR_SCALE: u64 = 1_000_000;

/// Annual percentage fee: 0.2 * SCALE
pub const ANNUAL_PERCENTAGE_FEE: u64 = 200_000;

/// Equivalent lease length in years used for permabuy pricing
pub const PERMABUY_LEASE_FEE_LENGTH_YEARS: u64 = 20;

/// Undername fee percentage for leases: 0.001 * SCALE
pub const UNDERNAME_LEASE_FEE_PCT: u64 = 1_000;

/// Undername fee percentage for permabuys: 0.005 * SCALE
pub const UNDERNAME_PERMABUY_FEE_PCT: u64 = 5_000;

/// Maximum premium multiplier for returned names (50x)
pub const RETURNED_NAME_MAX_MULTIPLIER: u64 = 50;

/// Duration in seconds over which the returned name premium decays (14 days)
pub const RETURNED_NAME_DURATION_SECONDS: i64 = 14 * 86_400;

/// Gateway operator discount percentage: 0.2 * SCALE
pub const GATEWAY_OPERATOR_DISCOUNT_PCT: u64 = 200_000;

/// Minimum gateway tenure for ArNS discount eligibility (180 days)
const GATEWAY_DISCOUNT_MIN_TENURE: i64 = 15_552_000;

// =========================================
// PRICING FUNCTIONS
// =========================================

/// Calculate the registration fee for a new ArNS name.
///
/// - Lease: `floor(demand_factor * base_fee * (1 + 0.2 * years))`
/// - Permabuy: `floor(demand_factor * base_fee * (1 + 0.2 * 20))` = `base_fee * demand_factor * 5 / SCALE`
pub fn calculate_registration_fee(
    base_fee: u64,
    purchase_type: PurchaseType,
    years: u8,
    demand_factor: u64,
) -> Result<u64> {
    let scale = DEMAND_FACTOR_SCALE as u128;
    let bf = base_fee as u128;
    let df = demand_factor as u128;

    let result = match purchase_type {
        PurchaseType::Lease => {
            // base_fee * demand_factor * (SCALE + ANNUAL_PERCENTAGE_FEE * years) / SCALE / SCALE
            let annual_pct = ANNUAL_PERCENTAGE_FEE as u128;
            let y = years as u128;
            let year_factor = scale
                .checked_add(
                    annual_pct
                        .checked_mul(y)
                        .ok_or(ArnsError::ArithmeticOverflow)?,
                )
                .ok_or(ArnsError::ArithmeticOverflow)?;
            bf.checked_mul(df)
                .ok_or(ArnsError::ArithmeticOverflow)?
                .checked_mul(year_factor)
                .ok_or(ArnsError::ArithmeticOverflow)?
                .checked_div(scale)
                .ok_or(ArnsError::ArithmeticOverflow)?
                .checked_div(scale)
                .ok_or(ArnsError::ArithmeticOverflow)?
        }
        PurchaseType::Permabuy => {
            // base_fee * demand_factor * 5 / SCALE
            // Derived from: (1 + 0.2 * 20) = 5.0
            bf.checked_mul(df)
                .ok_or(ArnsError::ArithmeticOverflow)?
                .checked_mul(5)
                .ok_or(ArnsError::ArithmeticOverflow)?
                .checked_div(scale)
                .ok_or(ArnsError::ArithmeticOverflow)?
        }
    };

    u64::try_from(result).map_err(|_| error!(ArnsError::ArithmeticOverflow))
}

/// Calculate the extension fee for an existing lease.
///
/// `floor(demand_factor * base_fee * 0.2 * years)`
/// Scaled: `base_fee * demand_factor * ANNUAL_PERCENTAGE_FEE * years / SCALE / SCALE`
pub fn calculate_extension_fee(base_fee: u64, years: u8, demand_factor: u64) -> Result<u64> {
    let scale = DEMAND_FACTOR_SCALE as u128;
    let bf = base_fee as u128;
    let df = demand_factor as u128;
    let annual_pct = ANNUAL_PERCENTAGE_FEE as u128;
    let y = years as u128;

    let result = bf
        .checked_mul(df)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_mul(annual_pct)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_mul(y)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_div(scale)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_div(scale)
        .ok_or(ArnsError::ArithmeticOverflow)?;

    u64::try_from(result).map_err(|_| error!(ArnsError::ArithmeticOverflow))
}

/// Calculate the cost to increase the undername limit.
///
/// - Lease: `floor(demand_factor * base_fee * 0.001 * qty)`
/// - Permabuy: `floor(demand_factor * base_fee * 0.005 * qty)`
/// Scaled: `base_fee * demand_factor * pct * qty / SCALE / SCALE`
pub fn calculate_undername_cost(
    base_fee: u64,
    qty: u16,
    purchase_type: PurchaseType,
    demand_factor: u64,
) -> Result<u64> {
    let scale = DEMAND_FACTOR_SCALE as u128;
    let bf = base_fee as u128;
    let df = demand_factor as u128;
    let q = qty as u128;

    let pct = match purchase_type {
        PurchaseType::Lease => UNDERNAME_LEASE_FEE_PCT as u128,
        PurchaseType::Permabuy => UNDERNAME_PERMABUY_FEE_PCT as u128,
    };

    let result = bf
        .checked_mul(df)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_mul(pct)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_mul(q)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_div(scale)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_div(scale)
        .ok_or(ArnsError::ArithmeticOverflow)?;

    u64::try_from(result).map_err(|_| error!(ArnsError::ArithmeticOverflow))
}

/// Calculate the permabuy fee for a name.
///
/// Equivalent to `calculate_registration_fee` with `PurchaseType::Permabuy`.
pub fn calculate_permabuy_fee(base_fee: u64, demand_factor: u64) -> Result<u64> {
    calculate_registration_fee(base_fee, PurchaseType::Permabuy, 0, demand_factor)
}

/// Calculate the premium cost for purchasing a returned name.
///
/// The premium decays linearly from `RETURNED_NAME_MAX_MULTIPLIER` (50x) down
/// to 1x over `RETURNED_NAME_DURATION_SECONDS` (14 days), matching the
/// whitepaper formula (§12.3): `RNP = 50 − (49/14) × t` where t is in days.
///
/// Integer form:
/// - `multiplier = (MAX × duration − (MAX−1) × elapsed) × SCALE / duration`
/// - `cost = registration_fee × multiplier / SCALE`
///
/// At t=0 the multiplier is 50×SCALE → cost = 50 × registration_fee.
/// At t=duration the multiplier is 1×SCALE → cost = registration_fee (1x).
/// The premium never drops below 1x during the window, eliminating the
/// boundary discontinuity that existed in the prior `50×remaining/duration`
/// formula (which reached 0x at t=duration and required a guard + caused a
/// dead zone where `require!(token_cost > 0)` rejected purchases).
pub fn calculate_returned_name_premium(
    registration_fee: u64,
    returned_at: i64,
    current_timestamp: i64,
) -> Result<u64> {
    let elapsed = current_timestamp.saturating_sub(returned_at);
    let duration = RETURNED_NAME_DURATION_SECONDS;

    // If the auction window has passed, no premium applies
    if elapsed >= duration {
        return Ok(registration_fee);
    }

    let scale = DEMAND_FACTOR_SCALE as u128;
    let dur = duration as u128;
    let el = elapsed as u128;
    let max_mult = RETURNED_NAME_MAX_MULTIPLIER as u128;

    // multiplier = (MAX * duration - (MAX - 1) * elapsed) * SCALE / duration
    // At elapsed=0: (50*dur) * SCALE / dur = 50*SCALE
    // At elapsed=dur: (50*dur - 49*dur) * SCALE / dur = 1*SCALE
    let numerator = max_mult
        .checked_mul(dur)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_sub(
            (max_mult - 1)
                .checked_mul(el)
                .ok_or(ArnsError::ArithmeticOverflow)?,
        )
        .ok_or(ArnsError::ArithmeticOverflow)?;

    let multiplier = numerator
        .checked_mul(scale)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_div(dur)
        .ok_or(ArnsError::ArithmeticOverflow)?;

    // cost = registration_fee * multiplier / SCALE
    let fee = registration_fee as u128;
    let result = fee
        .checked_mul(multiplier)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_div(scale)
        .ok_or(ArnsError::ArithmeticOverflow)?;

    u64::try_from(result).map_err(|_| error!(ArnsError::ArithmeticOverflow))
}

/// Apply the gateway operator discount (20% off).
///
/// `token_cost - token_cost * GATEWAY_OPERATOR_DISCOUNT_PCT / SCALE`
pub fn apply_gateway_operator_discount(token_cost: u64) -> Result<u64> {
    let scale = DEMAND_FACTOR_SCALE as u128;
    let cost = token_cost as u128;
    let discount_pct = GATEWAY_OPERATOR_DISCOUNT_PCT as u128;

    let discount = cost
        .checked_mul(discount_pct)
        .ok_or(ArnsError::ArithmeticOverflow)?
        .checked_div(scale)
        .ok_or(ArnsError::ArithmeticOverflow)?;

    let result = cost
        .checked_sub(discount)
        .ok_or(ArnsError::ArithmeticOverflow)?;

    u64::try_from(result).map_err(|_| error!(ArnsError::ArithmeticOverflow))
}

/// Look up the base fee for a name given its character length.
///
/// The `fees` array is 0-indexed: `fees[0]` is the fee for a 1-character name,
/// `fees[50]` is the fee for a 51-character name. Valid name lengths are 1..=51.
pub fn get_base_fee_for_name_length(fees: &[u64; 51], name_length: usize) -> Result<u64> {
    if name_length < 1 || name_length > 51 {
        return Err(error!(ArnsError::InvalidNameFormat));
    }
    Ok(fees[name_length - 1])
}

// =========================================
// GATEWAY OPERATOR DISCOUNT VERIFICATION
// =========================================

/// Attempt to apply gateway operator discount by reading a Gateway account
/// from the ario-gar program via remaining_accounts.
///
/// If a Gateway account is provided (first remaining_account):
///   1. Validates the account is owned by the ario-gar program
///   2. Validates the PDA seeds match ["gateway", signer_pubkey]
///   3. Deserializes and checks operator == signer and status == Joined
///   4. Applies 20% discount
///
/// If no Gateway account is provided, returns the original cost unchanged.
pub fn try_apply_gateway_discount(
    token_cost: u64,
    remaining_accounts: &[AccountInfo],
    signer: &Pubkey,
) -> Result<u64> {
    if remaining_accounts.is_empty() {
        return Ok(token_cost);
    }

    let gateway_info = &remaining_accounts[0];

    // Validate the account is owned by the ario-gar program
    require!(
        gateway_info.owner == &ario_gar::ID,
        ArnsError::InvalidGatewayProgram
    );

    // Validate PDA: seeds = ["gateway", signer_pubkey], program = ario-gar
    let (expected_pda, _bump) = Pubkey::find_program_address(
        &[ario_gar::state::GATEWAY_SEED, signer.as_ref()],
        &ario_gar::ID,
    );
    require!(
        gateway_info.key() == expected_pda,
        ArnsError::NotGatewayOperator
    );

    // Deserialize the Gateway account
    let gateway_data = gateway_info.try_borrow_data()?;
    let gateway: ario_gar::state::Gateway =
        ario_gar::state::Gateway::try_deserialize(&mut &gateway_data[..])
            .map_err(|_| error!(ArnsError::InvalidParameter))?;

    // Verify operator matches signer
    require!(gateway.operator == *signer, ArnsError::NotGatewayOperator);

    // Verify gateway is active (Joined status)
    require!(
        gateway.status == ario_gar::state::GatewayStatus::Joined,
        ArnsError::GatewayNotActive
    );

    // SHOULD-11: Tenure check — gateway must have been running for at least 180 days
    let tenure_weight_duration: i64 = GATEWAY_DISCOUNT_MIN_TENURE;
    // Need current timestamp — derive from Clock
    let clock = Clock::get()?;
    let time_running = clock.unix_timestamp.saturating_sub(gateway.start_timestamp);
    require!(
        time_running >= tenure_weight_duration,
        ArnsError::GatewayNotActive
    );

    // SHOULD-11: Performance check — 90% pass rate
    let numerator = (1u64 + gateway.stats.passed_epochs as u64)
        .checked_mul(1_000_000)
        .ok_or(ArnsError::ArithmeticOverflow)?;
    let denominator = 1u64 + gateway.stats.total_epochs as u64;
    let ratio = numerator
        .checked_div(denominator)
        .ok_or(ArnsError::ArithmeticOverflow)?;
    require!(ratio >= 900_000, ArnsError::GatewayNotActive);

    // Apply 20% discount
    apply_gateway_operator_discount(token_cost)
}

// =========================================
// NAME VALIDATION
// =========================================

/// Validate an ArNS name according to the naming rules:
/// - Length between 1 and 51 inclusive
/// - Length must NOT be 43 (Arweave address collision)
/// - Single character: must be ASCII alphanumeric
/// - Multiple characters: first and last must be ASCII alphanumeric,
///   middle characters can be alphanumeric or hyphen
/// - All characters must be lowercase
pub fn is_valid_arns_name(name: &str) -> bool {
    let len = name.len();

    // Length checks
    if len < 1 || len > 51 {
        return false;
    }

    // Reject length 43 (Arweave address collision)
    if len == 43 {
        return false;
    }

    let bytes = name.as_bytes();

    // All characters must be lowercase
    for &b in bytes {
        if b.is_ascii_uppercase() {
            return false;
        }
    }

    if len == 1 {
        // Single char: must be ASCII alphanumeric
        return bytes[0].is_ascii_alphanumeric();
    }

    // Multi char: first and last must be ASCII alphanumeric
    if !bytes[0].is_ascii_alphanumeric() || !bytes[len - 1].is_ascii_alphanumeric() {
        return false;
    }

    // Middle chars can be alphanumeric or hyphen
    for &b in &bytes[1..len - 1] {
        if !b.is_ascii_alphanumeric() && b != b'-' {
            return false;
        }
    }

    true
}

/// Compute the SHA256 hash of a name (lowercased) for PDA derivation.
///
/// Returns a 32-byte hash suitable for use as a PDA seed.
pub fn hash_name(name: &str) -> [u8; 32] {
    let lowered = name.to_lowercase();
    let result = hash(lowered.as_bytes());
    result.to_bytes()
}

// =========================================
// TESTS
// =========================================

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: u64 = DEMAND_FACTOR_SCALE;

    #[test]
    fn test_lease_registration_fee_1_year() {
        // base_fee=1000, years=1, demand_factor=1.0
        // 1000 * 1_000_000 * (1_000_000 + 200_000 * 1) / 1_000_000 / 1_000_000
        // = 1000 * 1_200_000 / 1_000_000 = 1200
        let fee = calculate_registration_fee(1000, PurchaseType::Lease, 1, SCALE).unwrap();
        assert_eq!(fee, 1200);
    }

    #[test]
    fn test_lease_registration_fee_5_years() {
        // 1000 * 1.0 * (1 + 0.2 * 5) = 1000 * 2.0 = 2000
        let fee = calculate_registration_fee(1000, PurchaseType::Lease, 5, SCALE).unwrap();
        assert_eq!(fee, 2000);
    }

    #[test]
    fn test_permabuy_registration_fee() {
        // 1000 * 1.0 * 5 = 5000
        let fee = calculate_registration_fee(1000, PurchaseType::Permabuy, 0, SCALE).unwrap();
        assert_eq!(fee, 5000);
    }

    #[test]
    fn test_permabuy_fee_delegates() {
        let fee = calculate_permabuy_fee(1000, SCALE).unwrap();
        assert_eq!(fee, 5000);
    }

    #[test]
    fn test_extension_fee() {
        // 1000 * 1.0 * 0.2 * 3 = 600
        let fee = calculate_extension_fee(1000, 3, SCALE).unwrap();
        assert_eq!(fee, 600);
    }

    #[test]
    fn test_undername_cost_lease() {
        // 1000 * 1.0 * 0.001 * 5 = 5
        let cost = calculate_undername_cost(1000, 5, PurchaseType::Lease, SCALE).unwrap();
        assert_eq!(cost, 5);
    }

    #[test]
    fn test_undername_cost_permabuy() {
        // 1000 * 1.0 * 0.005 * 5 = 25
        let cost = calculate_undername_cost(1000, 5, PurchaseType::Permabuy, SCALE).unwrap();
        assert_eq!(cost, 25);
    }

    #[test]
    fn test_demand_factor_2x() {
        // base_fee=1000, demand_factor=2.0 (2_000_000), permabuy
        // 1000 * 2_000_000 * 5 / 1_000_000 = 10000
        let fee = calculate_registration_fee(1000, PurchaseType::Permabuy, 0, 2_000_000).unwrap();
        assert_eq!(fee, 10000);
    }

    #[test]
    fn test_returned_name_premium_at_start() {
        // At returned_at, elapsed=0, pct_remaining=1.0, multiplier=50x
        // cost = 1000 * 50 = 50000
        let cost = calculate_returned_name_premium(1000, 100, 100).unwrap();
        assert_eq!(cost, 50000);
    }

    #[test]
    fn test_returned_name_premium_halfway() {
        // elapsed = 7 days (half of 14-day window)
        // WP formula: multiplier = 50 - 49*(7/14) = 50 - 24.5 = 25.5
        // cost = 1000 * 25.5 = 25500
        let returned_at = 0i64;
        let current = 7 * 86_400;
        let cost = calculate_returned_name_premium(1000, returned_at, current).unwrap();
        assert_eq!(cost, 25500);
    }

    #[test]
    fn test_returned_name_premium_expired() {
        // elapsed >= duration, no premium
        let cost = calculate_returned_name_premium(1000, 0, 14 * 86_400).unwrap();
        assert_eq!(cost, 1000);
    }

    #[test]
    fn test_gateway_operator_discount() {
        // 1000 - 1000 * 0.2 = 800
        let discounted = apply_gateway_operator_discount(1000).unwrap();
        assert_eq!(discounted, 800);
    }

    #[test]
    fn test_base_fee_for_name_length() {
        let mut fees = [0u64; 51];
        fees[0] = 100_000; // 1-char
        fees[4] = 10_000; // 5-char
        fees[50] = 1_000; // 51-char

        assert_eq!(get_base_fee_for_name_length(&fees, 1).unwrap(), 100_000);
        assert_eq!(get_base_fee_for_name_length(&fees, 5).unwrap(), 10_000);
        assert_eq!(get_base_fee_for_name_length(&fees, 51).unwrap(), 1_000);
    }

    #[test]
    fn test_base_fee_out_of_range() {
        let fees = [0u64; 51];
        assert!(get_base_fee_for_name_length(&fees, 0).is_err());
        assert!(get_base_fee_for_name_length(&fees, 52).is_err());
    }

    #[test]
    fn test_valid_arns_names() {
        assert!(is_valid_arns_name("a"));
        assert!(is_valid_arns_name("abc"));
        assert!(is_valid_arns_name("my-name"));
        assert!(is_valid_arns_name("a1"));
        assert!(is_valid_arns_name("test123"));
        assert!(is_valid_arns_name("a-b-c"));
    }

    #[test]
    fn test_invalid_arns_names() {
        // Empty
        assert!(!is_valid_arns_name(""));
        // Too long (52 chars)
        assert!(!is_valid_arns_name(&"a".repeat(52)));
        // Length 43 (Arweave collision)
        assert!(!is_valid_arns_name(&"a".repeat(43)));
        // Uppercase
        assert!(!is_valid_arns_name("ABC"));
        // Starts with hyphen
        assert!(!is_valid_arns_name("-abc"));
        // Ends with hyphen
        assert!(!is_valid_arns_name("abc-"));
        // Single hyphen
        assert!(!is_valid_arns_name("-"));
        // Invalid char
        assert!(!is_valid_arns_name("ab_c"));
        assert!(!is_valid_arns_name("ab.c"));
    }

    #[test]
    fn test_hash_name_case_insensitive() {
        let h1 = hash_name("MyName");
        let h2 = hash_name("myname");
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_hash_name_deterministic() {
        let h1 = hash_name("test");
        let h2 = hash_name("test");
        assert_eq!(h1, h2);
    }

    // =========================================
    // 3A. Additional Pricing Tests (Genesis Fees)
    // =========================================

    #[test]
    fn genesis_fee_1_char() {
        assert_eq!(
            get_base_fee_for_name_length(&crate::state::GENESIS_FEES, 1).unwrap(),
            500_000_000_000
        );
    }

    #[test]
    fn genesis_fee_5_char() {
        assert_eq!(
            get_base_fee_for_name_length(&crate::state::GENESIS_FEES, 5).unwrap(),
            2_500_000_000
        );
    }

    #[test]
    fn genesis_fee_13_char() {
        assert_eq!(
            get_base_fee_for_name_length(&crate::state::GENESIS_FEES, 13).unwrap(),
            200_000_000
        );
    }

    #[test]
    fn genesis_fee_51_char() {
        assert_eq!(
            get_base_fee_for_name_length(&crate::state::GENESIS_FEES, 51).unwrap(),
            200_000_000
        );
    }

    #[test]
    fn lease_1yr_5char_df1() {
        // base=2_500_000_000, 1yr, df=1.0
        // 2_500_000_000 * 1_000_000 * (1_000_000 + 200_000) / 1e6 / 1e6
        // = 2_500_000_000 * 1.2 = 3_000_000_000
        let fee = calculate_registration_fee(2_500_000_000, PurchaseType::Lease, 1, SCALE).unwrap();
        assert_eq!(fee, 3_000_000_000);
    }

    #[test]
    fn permabuy_5char_df1() {
        // base=2_500_000_000, permabuy, df=1.0
        // 2_500_000_000 * 5 = 12_500_000_000
        let fee =
            calculate_registration_fee(2_500_000_000, PurchaseType::Permabuy, 0, SCALE).unwrap();
        assert_eq!(fee, 12_500_000_000);
    }

    #[test]
    fn extension_3yr_5char() {
        // base=2_500_000_000, 3yr, df=1.0
        // 2_500_000_000 * 0.2 * 3 = 1_500_000_000
        let fee = calculate_extension_fee(2_500_000_000, 3, SCALE).unwrap();
        assert_eq!(fee, 1_500_000_000);
    }

    #[test]
    fn undername_lease_10() {
        // base=200_000_000, qty=10, lease, df=1.0
        // 200_000_000 * 0.001 * 10 = 2_000_000
        let cost = calculate_undername_cost(200_000_000, 10, PurchaseType::Lease, SCALE).unwrap();
        assert_eq!(cost, 2_000_000);
    }

    #[test]
    fn undername_permabuy_10() {
        // base=200_000_000, qty=10, permabuy, df=1.0
        // 200_000_000 * 0.005 * 10 = 10_000_000
        let cost =
            calculate_undername_cost(200_000_000, 10, PurchaseType::Permabuy, SCALE).unwrap();
        assert_eq!(cost, 10_000_000);
    }

    // =========================================
    // 3B. Returned Name Premium Tests
    // =========================================

    #[test]
    fn premium_at_25pct() {
        // 3.5 days = 302_400 seconds elapsed (25% of window)
        // WP formula: multiplier = 50 - 49*(302_400/1_209_600) = 50 - 49*0.25 = 50 - 12.25 = 37.75
        // cost = 1000 * 37.75 = 37_750
        let cost = calculate_returned_name_premium(1000, 0, 302_400).unwrap();
        assert_eq!(cost, 37750);
    }

    #[test]
    fn premium_at_75pct() {
        // 10.5 days = 907_200 seconds elapsed (75% of window)
        // WP formula: multiplier = 50 - 49*(907_200/1_209_600) = 50 - 49*0.75 = 50 - 36.75 = 13.25
        // cost = 1000 * 13.25 = 13_250
        let cost = calculate_returned_name_premium(1000, 0, 907_200).unwrap();
        assert_eq!(cost, 13250);
    }

    #[test]
    fn premium_at_1_second() {
        // 1 second elapsed
        // WP formula: multiplier = (50*1_209_600 - 49*1) * 1_000_000 / 1_209_600
        //           = (60_480_000 - 49) * 1_000_000 / 1_209_600
        //           ≈ 49.999_959... * 1_000_000 ≈ 49_999_959
        // cost = 1000 * 49_999_959 / 1_000_000 = 49_999
        let cost = calculate_returned_name_premium(1000, 0, 1).unwrap();
        // Should be very close to 50_000 but slightly less
        assert!(cost >= 49_900 && cost <= 50_000);
    }

    #[test]
    fn primary_name_request_cost() {
        // Uses 51-char base fee (200_000_000), qty=1, lease
        // undername cost = 200_000_000 * 0.001 * 1 = 200_000
        let cost = calculate_undername_cost(200_000_000, 1, PurchaseType::Lease, SCALE).unwrap();
        assert_eq!(cost, 200_000);
    }

    #[test]
    fn premium_near_end() {
        // 13.5 days = 1_166_400 seconds elapsed (out of 1_209_600 total)
        // WP formula: multiplier = 50 - 49*(1_166_400/1_209_600) = 50 - 49*0.964286 = 50 - 47.25 = 2.75
        // Integer: numerator = 50*1_209_600 - 49*1_166_400 = 60_480_000 - 57_153_600 = 3_326_400
        // multiplier = 3_326_400 * 1_000_000 / 1_209_600 = 2_750_000
        // cost = 1000 * 2_750_000 / 1_000_000 = 2750
        let elapsed = (13.5 * 86_400.0) as i64; // 1_166_400
        let cost = calculate_returned_name_premium(1000, 0, elapsed).unwrap();
        assert_eq!(cost, 2750);
    }

    #[test]
    fn premium_past_duration_returns_base() {
        // 15 days elapsed (past 14-day window) => no premium, returns base fee
        let cost = calculate_returned_name_premium(5000, 0, 15 * 86_400).unwrap();
        assert_eq!(cost, 5000);
    }

    #[test]
    fn premium_at_last_second_never_zero() {
        // At elapsed = duration - 1, the WP formula still yields >= registration_fee.
        // This is the dead-zone fix: the old formula (50 * remaining / dur) would
        // yield 0 here because remaining=1, 50*1*SCALE/dur truncates to 0.
        // WP formula: (50*dur - 49*(dur-1)) * SCALE / dur = (dur + 49) * SCALE / dur
        //   = (1_209_600 + 49) * 1_000_000 / 1_209_600 = 1_209_649 * 1_000_000 / 1_209_600
        //   ≈ 1_000_040 (just above 1x SCALE)
        // cost = 1000 * 1_000_040 / 1_000_000 = 1000
        let duration = RETURNED_NAME_DURATION_SECONDS;
        let cost = calculate_returned_name_premium(1000, 0, duration - 1).unwrap();
        assert!(
            cost >= 1000,
            "At elapsed=duration-1, cost ({}) must be >= registration_fee (1000)",
            cost
        );
        // Also verify it's never 0
        assert!(cost > 0, "Premium must never be 0 during the window");
    }

    #[test]
    fn premium_at_exact_duration_equals_base() {
        // At elapsed = duration exactly, the guard returns registration_fee
        let cost = calculate_returned_name_premium(1000, 0, RETURNED_NAME_DURATION_SECONDS).unwrap();
        assert_eq!(cost, 1000);
    }

    #[test]
    fn registration_fee_with_high_demand_factor() {
        // base=200_000_000, demand_factor=3.5 (3_500_000), lease 2yr
        // 200_000_000 * 3_500_000 * (1_000_000 + 400_000) / 1e6 / 1e6
        // = 200_000_000 * 3.5 * 1.4 = 980_000_000
        let fee =
            calculate_registration_fee(200_000_000, PurchaseType::Lease, 2, 3_500_000).unwrap();
        assert_eq!(fee, 980_000_000);
    }

    #[test]
    fn extension_fee_with_demand_factor() {
        // base=200_000_000, 2yr, df=1.5 (1_500_000)
        // 200_000_000 * 1_500_000 * 200_000 * 2 / 1e6 / 1e6
        // = 200_000_000 * 1.5 * 0.2 * 2 = 120_000_000
        let fee = calculate_extension_fee(200_000_000, 2, 1_500_000).unwrap();
        assert_eq!(fee, 120_000_000);
    }

    #[test]
    fn gateway_discount_on_large_fee() {
        // 12_500_000_000 * 0.8 = 10_000_000_000
        let discounted = apply_gateway_operator_discount(12_500_000_000).unwrap();
        assert_eq!(discounted, 10_000_000_000);
    }

    // =========================================
    // Security Fix Tests
    // =========================================

    #[test]
    fn gateway_discount_ratio_checked_arithmetic() {
        // Verify the checked arithmetic works for normal values
        let passed: u32 = 100;
        let total: u32 = 110;
        let numerator = (1u64 + passed as u64).checked_mul(1_000_000).unwrap();
        let denominator = 1u64 + total as u64;
        let ratio = numerator.checked_div(denominator).unwrap();
        // 101/111 * 1M = 909,909 (passes 90% threshold)
        assert!(ratio >= 900_000);

        // Verify with max u32 values (shouldn't overflow u64)
        let passed_max: u32 = u32::MAX;
        let total_max: u32 = u32::MAX;
        let numerator = (1u64 + passed_max as u64).checked_mul(1_000_000);
        assert!(numerator.is_some(), "u32::MAX + 1 * 1M should fit in u64");
        let denominator = 1u64 + total_max as u64;
        let ratio = numerator.unwrap().checked_div(denominator);
        assert!(
            ratio.is_some(),
            "division should not fail with max u32 values"
        );
    }

    #[test]
    fn gateway_discount_tenure_constant_matches() {
        assert_eq!(
            GATEWAY_DISCOUNT_MIN_TENURE, 15_552_000,
            "180 days in seconds"
        );
        assert_eq!(GATEWAY_DISCOUNT_MIN_TENURE, 180 * 86_400);
    }

    // TEST-014 (audit SECURITY_AUDIT_INDEPENDENT.md): tenure boundary.
    // try_apply_gateway_discount uses `time_running >= tenure_weight_duration`.
    // The audit calls out the off-by-one risk between `>` and `>=`. This test
    // pins the comparison: exactly-180-days qualifies; one-second-shy does not.
    // (We can't directly invoke try_apply_gateway_discount in a #[test] without
    // ProgramTest infra, so we exercise the comparison the function uses.)
    #[test]
    fn gateway_discount_tenure_boundary_inclusive() {
        let min_tenure = GATEWAY_DISCOUNT_MIN_TENURE; // 15_552_000

        // Exactly at the boundary (180 days) — eligible
        let exactly_180_days: i64 = 15_552_000;
        assert!(
            exactly_180_days >= min_tenure,
            "Tenure exactly 180 days must qualify for discount (boundary is inclusive)"
        );

        // One second below — NOT eligible
        let just_under: i64 = 15_551_999;
        assert!(
            !(just_under >= min_tenure),
            "Tenure of 180 days minus 1 second must NOT qualify"
        );

        // 365 days — eligible
        let one_year: i64 = 365 * 86_400;
        assert!(
            one_year >= min_tenure,
            "Tenure of 365 days must qualify for discount"
        );

        // Zero tenure (gateway just joined) — NOT eligible
        let zero: i64 = 0;
        assert!(
            !(zero >= min_tenure),
            "Brand-new gateway (0s tenure) must NOT qualify for discount"
        );
    }

    // TEST-014: also verify a gateway whose start_timestamp is in the future
    // (clock skew or malicious setup) cannot receive a discount.
    // i64 saturating_sub returns the negative result (it saturates at i64::MIN,
    // not 0 like unsigned types), but the `>=` check still rejects negatives.
    #[test]
    fn gateway_discount_tenure_clock_skew_protection() {
        let clock_now: i64 = 1_000_000_000;
        let future_start: i64 = 2_000_000_000;
        let time_running = clock_now.saturating_sub(future_start);
        assert!(
            time_running < 0,
            "Future start yields negative time_running"
        );
        assert!(
            !(time_running >= GATEWAY_DISCOUNT_MIN_TENURE),
            "Future-dated gateway must NOT qualify for discount (negative tenure rejected by >= check)"
        );

        // Also verify the i64::MIN saturation case (extreme: start=i64::MAX)
        let extreme_start: i64 = i64::MAX;
        let time_running = 0i64.saturating_sub(extreme_start);
        assert!(
            !(time_running >= GATEWAY_DISCOUNT_MIN_TENURE),
            "Extreme future-dated start must still be rejected"
        );
    }

    // =========================================
    // Property-Based Tests (proptest)
    // =========================================

    mod proptests {
        use super::*;
        use proptest::prelude::*;

        proptest! {
            #[test]
            fn registration_fee_never_panics(
                base_fee in 0u64..=500_000_000_000u64,
                years in 1u8..=5u8,
                demand_factor in 1u64..=10_000_000u64,
            ) {
                let _ = calculate_registration_fee(base_fee, PurchaseType::Lease, years, demand_factor);
                let _ = calculate_registration_fee(base_fee, PurchaseType::Permabuy, 0, demand_factor);
            }

            #[test]
            fn lease_fee_increases_with_years(
                base_fee in 1u64..=100_000_000_000u64,
                demand_factor in 1u64..=5_000_000u64,
            ) {
                let fee_1 = calculate_registration_fee(base_fee, PurchaseType::Lease, 1, demand_factor);
                let fee_5 = calculate_registration_fee(base_fee, PurchaseType::Lease, 5, demand_factor);
                if let (Ok(f1), Ok(f5)) = (fee_1, fee_5) {
                    prop_assert!(f5 >= f1, "5yr fee {} must be >= 1yr fee {}", f5, f1);
                }
            }

            #[test]
            fn permabuy_costs_more_than_5yr_lease(
                base_fee in 1u64..=100_000_000_000u64,
                demand_factor in 1u64..=5_000_000u64,
            ) {
                let lease_5 = calculate_registration_fee(base_fee, PurchaseType::Lease, 5, demand_factor);
                let perm = calculate_registration_fee(base_fee, PurchaseType::Permabuy, 0, demand_factor);
                if let (Ok(l5), Ok(p)) = (lease_5, perm) {
                    prop_assert!(p >= l5, "permabuy {} must be >= 5yr lease {}", p, l5);
                }
            }

            #[test]
            fn returned_name_premium_decays(
                registration_fee in 1u64..=100_000_000_000u64,
                elapsed_early in 0i64..=604_800i64,
            ) {
                let elapsed_late = elapsed_early + 302_400;
                let early = calculate_returned_name_premium(registration_fee, 0, elapsed_early);
                let late = calculate_returned_name_premium(registration_fee, 0, elapsed_late);
                if let (Ok(e), Ok(l)) = (early, late) {
                    prop_assert!(e >= l, "earlier premium {} must be >= later premium {}", e, l);
                }
            }

            #[test]
            fn returned_name_at_expiry_equals_base(
                registration_fee in 1u64..=u64::MAX,
                extra_seconds in 0i64..=86_400i64,
            ) {
                let at_expiry = RETURNED_NAME_DURATION_SECONDS + extra_seconds;
                let cost = calculate_returned_name_premium(registration_fee, 0, at_expiry);
                prop_assert_eq!(cost.unwrap(), registration_fee);
            }

            #[test]
            fn gateway_discount_reduces_cost(
                // Floor of `cost * 200_000 / 1_000_000` is zero for cost < 5,
                // so the strict-less-than property only holds when the discount
                // actually rounds away from zero. Lower bound matches that.
                token_cost in 5u64..=u64::MAX,
            ) {
                let discounted = apply_gateway_operator_discount(token_cost).unwrap();
                prop_assert!(discounted < token_cost, "discounted {} must be < original {}", discounted, token_cost);
            }

            #[test]
            fn valid_lowercase_alnum_names_pass(
                len in 1usize..=51usize,
            ) {
                prop_assume!(len != 43);
                let name: String = "a".repeat(len);
                prop_assert!(is_valid_arns_name(&name), "name of length {} should be valid", len);
            }

            #[test]
            fn undername_permabuy_more_than_lease(
                base_fee in 1u64..=100_000_000_000u64,
                qty in 1u16..=1000u16,
                demand_factor in 1u64..=5_000_000u64,
            ) {
                let lease = calculate_undername_cost(base_fee, qty, PurchaseType::Lease, demand_factor);
                let perm = calculate_undername_cost(base_fee, qty, PurchaseType::Permabuy, demand_factor);
                if let (Ok(l), Ok(p)) = (lease, perm) {
                    prop_assert!(p >= l, "permabuy undername {} must be >= lease undername {}", p, l);
                }
            }
        }
    }
}
