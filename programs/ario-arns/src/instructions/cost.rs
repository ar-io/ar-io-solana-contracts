use crate::error::ArnsError;
use crate::pricing::*;
use crate::state::*;
use crate::CostIntent;
use crate::TokenCostParams;
use anchor_lang::prelude::*;

pub mod get_token_cost {
    use super::*;

    /// Calculate token cost for an operation.
    /// This is a "view" instruction - call via simulate to read the return data.
    /// Does not modify state.
    pub fn handler(ctx: Context<GetTokenCost>, params: TokenCostParams) -> Result<()> {
        let demand = &ctx.accounts.demand_factor;
        let name_len = params.name.len();
        let base_fee = get_base_fee_for_name_length(&demand.fees, name_len)?;
        let df = demand.current_demand_factor;

        let cost = match params.intent {
            CostIntent::BuyName => {
                let years = params.years.unwrap_or(1);
                let purchase_type = params.purchase_type.unwrap_or(if years > 0 {
                    PurchaseType::Lease
                } else {
                    PurchaseType::Permabuy
                });
                calculate_registration_fee(base_fee, purchase_type, years, df)?
            }
            CostIntent::ExtendLease => {
                let years = params.years.ok_or(ArnsError::InvalidParameter)?;
                calculate_extension_fee(base_fee, years, df)?
            }
            CostIntent::UpgradeName => calculate_permabuy_fee(base_fee, df)?,
            CostIntent::IncreaseUndernameLimit => {
                let qty = params.quantity.ok_or(ArnsError::InvalidParameter)?;
                let pt = params.purchase_type.ok_or(ArnsError::InvalidParameter)?;
                calculate_undername_cost(base_fee, qty, pt, df)?
            }
            CostIntent::PrimaryNameRequest => {
                // Uses base fee for max name length (51), qty=1
                let primary_base_fee = get_base_fee_for_name_length(&demand.fees, MAX_NAME_LENGTH)?;
                calculate_undername_cost(
                    primary_base_fee,
                    1,
                    params.purchase_type.unwrap_or(PurchaseType::Lease),
                    df,
                )?
            }
        };

        // Return cost via set_return_data for simulation
        let cost_bytes = cost.to_le_bytes();
        anchor_lang::solana_program::program::set_return_data(&cost_bytes);

        msg!("Token cost: {} mARIO", cost);
        Ok(())
    }
}

// =========================================
// ACCOUNT CONTEXT
// =========================================

#[derive(Accounts)]
pub struct GetTokenCost<'info> {
    #[account(
        seeds = [DEMAND_FACTOR_SEED],
        bump = demand_factor.bump,
    )]
    pub demand_factor: Account<'info, DemandFactor>,

    pub payer: Signer<'info>,
}
