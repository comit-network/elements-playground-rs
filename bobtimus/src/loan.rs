use crate::{LiquidBtc, LiquidUsdt, Rate};
use anyhow::{Context, Result};
use baru::input::Input;
use elements::{
    bitcoin::{Amount, PublicKey},
    Address,
};
use rust_decimal::{prelude::ToPrimitive, Decimal};
use rust_decimal_macros::dec;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LoanOffer {
    // TODO: Potentially add an id if we want to track offers - for now we just check if an incoming request is acceptable
    pub rate: Rate,

    #[serde(with = "::elements::bitcoin::util::amount::serde::as_sat")]
    pub fee_sats_per_vbyte: Amount,

    #[serde(serialize_with = "LiquidUsdt::serialize_to_nominal")]
    pub min_principal: LiquidUsdt,
    #[serde(serialize_with = "LiquidUsdt::serialize_to_nominal")]
    pub max_principal: LiquidUsdt,

    /// The maximum LTV that defines at what point the lender liquidates
    ///
    /// LTV ... loan to value
    /// LTV = principal_amount/loan_value
    /// where:
    ///     principal_amount: the amount lent out
    ///     loan_value: the amount of collateral
    ///
    /// Simple Example (interest / fees not taken into account):
    ///
    /// The borrower takes out a loan at:
    ///     max_ltv = 0.7 (70%)
    ///     rate: 1 BTC = $100
    ///     principal_amount: $100
    ///     collateral: 2 BTC = $200 (over-collateralized by 200%)
    ///     current LTV = 100 / 200 = 0.5 (50%)
    /// Since the actual LTV 0.5 < 0.7, so all is good.
    ///
    /// Let's say Bitcoin value falls to $70:
    ///     LTV = 100 / 2 * 70 => 100 / 140 = 0.71
    /// The actual LTV 0.71 > 0.7 so the lender liquidates.
    ///
    /// The max_ltv protects the lender from Bitcoin falling too much.
    pub max_ltv: Decimal,

    /// Base interest in percent (to be applied to the principal amount)
    pub base_interest_rate: Decimal,

    /// Interest in relation to terms
    pub terms: Vec<Term>,

    /// Interest rates in relation to ltv
    pub initial_ltvs: Vec<Ltv>,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Term {
    pub days: u32,
    /// Interest to be added on top of the base interest rate for this term
    pub interest_mod: Decimal,
}

/// LTV (Loan-to-value) ratio defines the relation of loan value to collateral value, i.e.
/// (principal_amount / (collateral_amount * current_rate)
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct Ltv {
    pub ltv: Decimal,
}

// TODO: Make sure that removing sat_per_vbyte is OK here
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LoanRequest {
    /// Loan term in days
    pub term: u32,
    pub principal_amount: LiquidUsdt,
    pub ltv: Decimal,
    pub collateral_amount: LiquidBtc,
    pub collateral_inputs: Vec<Input>,
    pub borrower_pk: PublicKey,
    pub borrower_address: Address,
}

pub struct ValidatedLoan {
    pub repayment_amount: LiquidUsdt,
    pub liquidation_price: LiquidUsdt,
}

#[derive(Debug, Clone)]
struct LoanValidationParams {
    request_price: LiquidUsdt,
    current_price: LiquidUsdt,
    price_fluctuation_interval: (Decimal, Decimal),
    request_principal: LiquidUsdt,
    min_principal: LiquidUsdt,
    max_principal: LiquidUsdt,
    /// the LTV ratio the user has actually provided, i.e. it's calculated using the provided collateral
    provided_ltv: Decimal,
    /// the max allowed LTV
    max_ltv: Decimal,
    request_term: u32,
    terms: Vec<Term>,
    /// the LTV ratio the user has requested
    requested_ltv: Decimal,
    ltvs: Vec<Ltv>,
}

pub fn loan_calculation_and_validation(
    loan_request: &LoanRequest,
    loan_offer: &LoanOffer,
    price_fluctuation_interval: (Decimal, Decimal),
    current_price: LiquidUsdt,
) -> Result<ValidatedLoan> {
    let selected_term = match loan_offer
        .terms
        .iter()
        .find(|term| loan_request.term == term.days)
    {
        None => Err(LoanValidationError::TermNotAllowed {
            term: loan_request.term,
        }),
        Some(term) => Ok(term),
    }?;

    let interest_rate = calculate_interest_rate(&selected_term, loan_offer.base_interest_rate)?;

    let repayment_amount =
        calculate_repayment_amount(loan_request.principal_amount, interest_rate)?;

    let request_price = calculate_request_price(
        repayment_amount,
        loan_request.collateral_amount,
        loan_request.ltv,
    )?;

    let provided_ltv = calculate_ltv(
        repayment_amount,
        loan_request.collateral_amount,
        current_price,
    )?;

    validate_loan_is_acceptable(LoanValidationParams {
        request_price,
        current_price,
        price_fluctuation_interval,
        request_principal: loan_request.principal_amount,
        min_principal: loan_offer.min_principal,
        max_principal: loan_offer.max_principal,
        provided_ltv,
        max_ltv: loan_offer.max_ltv,
        request_term: loan_request.term,
        terms: loan_offer.terms.clone(),
        requested_ltv: loan_request.ltv,
        ltvs: loan_offer.initial_ltvs.clone(),
    })??;

    let liquidation_price = calculate_liquidation_price(
        repayment_amount,
        loan_request.collateral_amount,
        loan_offer.max_ltv,
    )?;

    let validated_loan = ValidatedLoan {
        repayment_amount,
        liquidation_price,
    };

    Ok(validated_loan)
}

fn calculate_interest_rate(selected_term: &Term, base_interest_rate: Decimal) -> Result<Decimal> {
    base_interest_rate
        .checked_add(selected_term.interest_mod)
        .context("Overflow due to addition")
}

fn calculate_repayment_amount(
    principal_amount: LiquidUsdt,
    interest_percentage: Decimal,
) -> Result<LiquidUsdt> {
    let principal_amount = Decimal::from(principal_amount.as_satodollar());

    let repayment_amount = principal_amount
        .checked_add(
            principal_amount
                .checked_mul(interest_percentage)
                .context("multiplication overflow")?,
        )
        .context("addition overflow")?;
    let repayment_amount = LiquidUsdt::from_satodollar(
        repayment_amount
            .to_u64()
            .context("decimal cannot be represented as u64")?,
    );

    Ok(repayment_amount)
}

/// Calculates the liquidation price
///
/// The liquidation price must depict the borrower's over-collateralization and the lender's risk hunger.
/// Thus, the borrower's collateral amount and the lender's maximum LTV ratio are set into relation.
///
/// We can use the formula to calculate the current LTV to reason about the liquidation price:
///
/// given: repayment_amount / (collateral_amount * current_price) = current_LTV
///     > repayment_amount = current_ltv * (collateral_amount * current_price)
///     > repayment_amount / current_ltv = collateral_amount * current_price
///     > (repayment_amount / current_ltv) / collateral_amount = current_price
///     > let current_ltv = max_ltv
/// -----------------------------------------------------------------------------
/// (repayment_amount / max_ltv) / collateral_amount = liquidation_price
///
/// note: collateral_amount in BTC
fn calculate_liquidation_price(
    repayment_amount: LiquidUsdt,
    collateral_amount: LiquidBtc,
    max_ltv: Decimal,
) -> Result<LiquidUsdt> {
    let repayment_amount = Decimal::from(repayment_amount.as_satodollar());
    let one_btc_as_sat = Decimal::from(Amount::ONE_BTC.as_sat());
    let collateral_as_btc = Decimal::from(collateral_amount.0.as_sat())
        .checked_div(one_btc_as_sat)
        .context("division error")?;

    let liquidation_price = repayment_amount
        .checked_div(max_ltv)
        .context("division error")?
        .checked_div(collateral_as_btc)
        .context("division error")?;

    let liquidation_price = LiquidUsdt::from_satodollar(
        liquidation_price
            .to_u64()
            .context("decimal cannot be represented as u64")?,
    );

    Ok(liquidation_price)
}

/// calculates the `request_price`, i.e. the exchange rate the borrower took when selecting loan terms
///
/// the `request_price` is calculated as following:
///
/// LTV = repayment_amount / (collateral_amount * rate) | * (collateral_amount * rate)
/// LTV * (collateral_amount * rate) = repayment_amount | / collateral_amount
/// LTV * rate = repayment_amount * collateral_amount | / LTV
/// rate = (repayment_amount / collateral) / LTV
///
/// --> rate = request_price
fn calculate_request_price(
    repayment_amount: LiquidUsdt,
    collateral_amount: LiquidBtc,
    ltv: Decimal,
) -> Result<LiquidUsdt> {
    let one_btc_as_sat = Decimal::from(Amount::ONE_BTC.as_sat());

    let repayment_amount = Decimal::from(repayment_amount.as_satodollar());

    let collateral_as_btc = Decimal::from(collateral_amount.0.as_sat())
        .checked_div(one_btc_as_sat)
        .context("division error")?;

    let repayment_div_collateral = repayment_amount
        .checked_div(collateral_as_btc)
        .context("division error")?;

    let price = repayment_div_collateral
        .checked_div(ltv)
        .context("division error")?;

    let price = LiquidUsdt::from_satodollar(
        price
            .to_u64()
            .context("decimal cannot be represented as u64")?,
    );

    Ok(price)
}

/// calculates provided LTV ratio
///
/// provided_ltv = repayment_amount / (collateral_amount * current_bid_price)
fn calculate_ltv(
    repayment_amount: LiquidUsdt,
    collateral_amount: LiquidBtc,
    current_bid_price: LiquidUsdt,
) -> Result<Decimal> {
    let repayment_amount = Decimal::from(repayment_amount.as_satodollar());
    let price = Decimal::from(current_bid_price.as_satodollar());

    let one_btc = Decimal::from(Amount::ONE_BTC.as_sat());
    let collateral_in_btc = Decimal::from(collateral_amount.0.as_sat())
        .checked_div(one_btc)
        .context("division error")?;

    let ltv = repayment_amount
        .checked_div(
            collateral_in_btc
                .checked_mul(price)
                .context("multiplication error")?,
        )
        .context("division error")?;

    Ok(ltv)
}

#[derive(Debug, PartialEq, thiserror::Error)]
pub enum LoanValidationError {
    #[error(
        "The given price {request_price} is not acceptable with current price {current_price}"
    )]
    PriceNotAcceptable {
        request_price: LiquidUsdt,
        current_price: LiquidUsdt,
    },

    #[error("The given principal amount {request_principal} is below the configured minimum {min_principal}")]
    PrincipalBelowMin {
        request_principal: LiquidUsdt,
        min_principal: LiquidUsdt,
    },

    #[error("The given principal amount {request_principal} is above the configured maximum {max_principal}")]
    PrincipalAboveMax {
        request_principal: LiquidUsdt,
        max_principal: LiquidUsdt,
    },

    #[error("The LTV value {provided_ltv} is above the configured maximum {max_ltv}")]
    LtvAboveMax {
        provided_ltv: Decimal,
        max_ltv: Decimal,
    },

    #[error("The given term {term} is not allowed")]
    TermNotAllowed { term: u32 },

    #[error(
        "The requested ltv {requested_ltv} was not offered (available ltvs: {:?})",
        offered_ltvs
    )]
    InvalidRequestedLtv {
        requested_ltv: Decimal,
        offered_ltvs: Vec<Decimal>,
    },
    #[error(
        "The provided ltv {provided_ltv} does not match the not selected LTV {requested_ltv})"
    )]
    InvalidProvidedLtv {
        provided_ltv: Decimal,
        requested_ltv: Decimal,
    },
}

fn validate_loan_is_acceptable(
    loan_validation_params: LoanValidationParams,
) -> Result<Result<(), LoanValidationError>> {
    let LoanValidationParams {
        request_price,
        current_price,
        price_fluctuation_interval,
        request_principal,
        min_principal,
        max_principal,
        provided_ltv,
        max_ltv,
        request_term,
        terms,
        requested_ltv,
        ltvs,
    } = loan_validation_params;

    let request_price_dec = Decimal::from(request_price.as_satodollar());
    let current_price_dec = Decimal::from(current_price.as_satodollar());

    // TODO: Evaluate if we want to use an upper and a lower bound.
    //  We could just restrict by upper bound, because that is what makes it more expensive for the lender
    //  i.e. if price was 1000 and is 100 now we must ensure to accept only if the current price it not higher than 100 + x%
    let (lower, upper) = price_fluctuation_interval;
    let lower_bound = current_price_dec
        .checked_mul(lower)
        .context("multiplication error")?;
    let upper_bound = current_price_dec
        .checked_mul(upper)
        .context("multiplication error")?;

    if request_price_dec < lower_bound || request_price_dec > upper_bound {
        return Ok(Err(LoanValidationError::PriceNotAcceptable {
            request_price,
            current_price,
        }));
    }

    if request_principal < min_principal {
        return Ok(Err(LoanValidationError::PrincipalBelowMin {
            request_principal,
            min_principal,
        }));
    }

    if request_principal > max_principal {
        return Ok(Err(LoanValidationError::PrincipalAboveMax {
            request_principal,
            max_principal,
        }));
    }

    // we allow for slight price variations of 1% up or down
    let one_percent = requested_ltv
        .checked_mul(dec!(0.01))
        .context("multiplication error")?;
    let plus_one_percent = requested_ltv
        .checked_add(one_percent)
        .context("Addition error")?;
    let minus_one_percent = requested_ltv
        .checked_sub(one_percent)
        .context("Subtraction error")?;
    if provided_ltv > plus_one_percent || provided_ltv < minus_one_percent {
        return Ok(Err(LoanValidationError::InvalidProvidedLtv {
            provided_ltv,
            requested_ltv,
        }));
    }

    // we take the requested_ltv because the actual (provided_ltv) might be impacted by price fluctuations
    if !ltvs.iter().any(|ltv| requested_ltv == ltv.ltv) {
        return Ok(Err(LoanValidationError::InvalidRequestedLtv {
            requested_ltv,
            offered_ltvs: ltvs.iter().map(|ltv| ltv.ltv).collect(),
        }));
    }

    if provided_ltv > max_ltv {
        return Ok(Err(LoanValidationError::LtvAboveMax {
            provided_ltv,
            max_ltv,
        }));
    }

    if !terms.iter().any(|a| a.days == request_term) {
        return Ok(Err(LoanValidationError::TermNotAllowed {
            term: request_term,
        }));
    }

    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::proptest;
    use rust_decimal::prelude::FromPrimitive;
    use rust_decimal_macros::dec;
    use std::str::FromStr;

    #[test]
    fn test_loan_calculation_and_validation() {
        // LTV of 0.5 = 200% collateralization
        //
        // interest rate = 5%
        // principal_amount = 10,000,
        // initial_price = 10,500
        // then we need 2 BTC collateral
        //
        // collateral = (principal_amount + principal_amount * interest_rate) / initial_price / ltv
        // collateral = repayment_amount / initial_price / ltv
        let loan_request = LoanRequest {
            term: 30,
            principal_amount: LiquidUsdt::from_str_in_dollar("10000").unwrap(),
            ltv: dec!(0.5),
            collateral_amount: Amount::from_btc(2.0).unwrap().into(),
            // irrelevant for this test
            collateral_inputs: vec![],
            borrower_pk: PublicKey::from_str("0218845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166").unwrap(),
            borrower_address: Address::from_str("el1qq0zel5lg55nvhv9kkrq8gme8hnvp0lemuzcmu086dn2m8laxjgkewkhqnh8vxdnlp4cejs3925j0gu9n9krdgmqm89vku0kc8").unwrap()
        };

        let loan_offer = LoanOffer {
            min_principal: LiquidUsdt::from_str_in_dollar("1000").unwrap(),
            max_principal: LiquidUsdt::from_str_in_dollar("10000").unwrap(),
            max_ltv: dec!(0.75),
            base_interest_rate: dec!(0.05),

            terms: vec![Term {
                days: 30,
                interest_mod: Decimal::ZERO,
            }],
            // not relevant for test but to show as reference - that's what the borrower can chose
            initial_ltvs: vec![Ltv { ltv: dec!(0.5) }],

            // irrelevant for this test
            rate: Rate {
                ask: LiquidUsdt::from_str_in_dollar("10500").unwrap(),
                bid: LiquidUsdt::from_str_in_dollar("10500").unwrap(),
            },
            fee_sats_per_vbyte: Default::default(),
        };

        let current_price = LiquidUsdt::from_str_in_dollar("10500").unwrap();
        let price_fluctuation_interval = (dec!(0.99), dec!(1.01));

        let ValidatedLoan {
            repayment_amount,
            liquidation_price,
        } = loan_calculation_and_validation(
            &loan_request,
            &loan_offer,
            price_fluctuation_interval,
            current_price,
        )
        .unwrap();

        assert_eq!(
            repayment_amount,
            LiquidUsdt::from_str_in_dollar("10500").unwrap()
        );
        assert_eq!(
            liquidation_price,
            LiquidUsdt::from_str_in_dollar("7000").unwrap()
        );
    }

    #[test]
    fn test_loan_calculation_and_validation_whole_numbers() {
        // LTV of 0.70 = 150% collateralization
        //
        // interest rate = 5%
        // principal_amount = 10,000,
        // initial_price = 10,000
        // then we need 1.5 BTC collateral
        //
        // collateral = (principal_amount + principal_amount * interest_rate) / initial_price / ltv
        // collateral = repayment_amount / initial_price / ltv
        let loan_request = LoanRequest {
            term: 30,
            principal_amount: LiquidUsdt::from_str_in_dollar("10000").unwrap(),
            ltv: dec!(0.70),
            collateral_amount: Amount::from_btc(1.5).unwrap().into(),

            // irrelevant for this test
            collateral_inputs: vec![],
            borrower_pk: PublicKey::from_str("0218845781f631c48f1c9709e23092067d06837f30aa0cd0544ac887fe91ddd166").unwrap(),
            borrower_address: Address::from_str("el1qq0zel5lg55nvhv9kkrq8gme8hnvp0lemuzcmu086dn2m8laxjgkewkhqnh8vxdnlp4cejs3925j0gu9n9krdgmqm89vku0kc8").unwrap()
        };

        let loan_offer = LoanOffer {
            min_principal: LiquidUsdt::from_str_in_dollar("1000").unwrap(),
            max_principal: LiquidUsdt::from_str_in_dollar("10000").unwrap(),
            max_ltv: dec!(0.75),
            base_interest_rate: dec!(0.05),

            terms: vec![Term {
                days: 30,
                interest_mod: Decimal::ZERO,
            }],
            // not relevant for test but to show as reference - that's what the borrower can chose
            initial_ltvs: vec![Ltv { ltv: dec!(0.70) }],

            // irrelevant for this test
            rate: Rate {
                ask: LiquidUsdt::from_str_in_dollar("10500").unwrap(),
                bid: LiquidUsdt::from_str_in_dollar("10500").unwrap(),
            },
            fee_sats_per_vbyte: Default::default(),
        };

        let current_price = LiquidUsdt::from_str_in_dollar("10000").unwrap();
        let price_fluctuation_interval = (dec!(0.99), dec!(1.01));

        let ValidatedLoan {
            repayment_amount,
            liquidation_price,
        } = loan_calculation_and_validation(
            &loan_request,
            &loan_offer,
            price_fluctuation_interval,
            current_price,
        )
        .unwrap();

        assert_eq!(
            repayment_amount,
            LiquidUsdt::from_str_in_dollar("10500").unwrap()
        );
        // The `liquidation_price` is calculated by :
        // `(repayment_amount / current_ltv) / collateral_amount`
        // or with numbers in this example:
        // (10000 / 0.75 ) / 1.5 = 9333.33333333
        assert_eq!(
            liquidation_price,
            LiquidUsdt::from_str_in_dollar("9333.33333333").unwrap()
        );
    }

    #[test]
    fn test_calculate_interest_rate() {
        let term_thresholds = vec![
            Term {
                days: 30,
                interest_mod: dec!(0.001),
            },
            Term {
                days: 90,
                interest_mod: dec!(0.004),
            },
            Term {
                days: 180,
                interest_mod: dec!(0.008),
            },
        ];
        let base_interest_rate = dec!(0.05);

        let interest_rate =
            calculate_interest_rate(&term_thresholds[0], base_interest_rate).unwrap();
        assert_eq!(interest_rate, dec!(0.051));

        let interest_rate =
            calculate_interest_rate(&term_thresholds[1], base_interest_rate).unwrap();
        assert_eq!(interest_rate, dec!(0.054));

        let interest_rate =
            calculate_interest_rate(&term_thresholds[2], base_interest_rate).unwrap();
        assert_eq!(interest_rate, dec!(0.058));
    }

    #[test]
    fn test_calculate_repayment_amount() {
        let principal_amount = LiquidUsdt::from_satodollar(10000);
        let interest_percentage = dec!(0.05);

        let repayment_amount =
            calculate_repayment_amount(principal_amount, interest_percentage).unwrap();
        assert_eq!(repayment_amount, LiquidUsdt::from_satodollar(10500));
    }

    proptest! {
        #[test]
        fn test_calculate_repayment_amount_no_panic(
            // we eventually hit decimal limits, but the amounts are so high that is should not matter
            principal_amount in 1u64..15_000_000_000_000_000_000, // satdollar ^= 150 billion usd
            interest_percentage in 0.001f32..0.2,
        ) {
            let principal_amount = LiquidUsdt::from_satodollar(principal_amount);
            let interest_percentage = Decimal::from_f32(interest_percentage).unwrap();

            let _ = calculate_repayment_amount(principal_amount, interest_percentage).unwrap();
        }
    }

    #[test]
    fn test_calculate_liquidation_price() {
        let repayment_amount = LiquidUsdt::from_str_in_dollar("10500").unwrap();
        let collateral = LiquidBtc::from(Amount::from_btc(0.35).unwrap());
        let max_ltv = dec!(0.8);

        let liquidation_price =
            calculate_liquidation_price(repayment_amount, collateral, max_ltv).unwrap();

        assert_eq!(
            liquidation_price,
            LiquidUsdt::from_str_in_dollar("37500").unwrap()
        )
    }

    proptest! {
        #[test]
        fn test_calculate_liquidation_price_no_panic(
            repayment_amount in 1u64..,
            collateral in 1u64..,
            max_ltv in 0.0f32..0.9999,
        ) {
            let repayment_amount = LiquidUsdt::from_satodollar(repayment_amount);
            let collateral = LiquidBtc::from(Amount::from_sat(collateral));
            let max_ltv = Decimal::from_f32(max_ltv).unwrap();

            let _ = calculate_liquidation_price(repayment_amount, collateral, max_ltv).unwrap();
        }
    }

    #[test]
    fn test_calculate_price_using_ltv_and_collateral_of_one_btc() {
        let repayment_amount = LiquidUsdt::from_str_in_dollar("10000").unwrap();
        let collateral = LiquidBtc::from(Amount::from_btc(1.0).unwrap());
        let ltv = dec!(1.0);

        let price = calculate_request_price(repayment_amount, collateral, ltv).unwrap();

        assert_eq!(price, LiquidUsdt::from_str_in_dollar("10000").unwrap());
    }

    #[test]
    fn test_calculate_price_200_percent_collateralization() {
        let repayment_amount = LiquidUsdt::from_str_in_dollar("10000").unwrap();
        let collateral = LiquidBtc::from(Amount::from_btc(2.0).unwrap());
        // LTV of 0.5 = 200% collateral
        let ltv = dec!(0.5);

        let price = calculate_request_price(repayment_amount, collateral, ltv).unwrap();

        assert_eq!(price, LiquidUsdt::from_str_in_dollar("10000").unwrap());
    }

    #[test]
    fn test_calculate_price_125_percent_collateralization() {
        let repayment_amount = LiquidUsdt::from_str_in_dollar("10000").unwrap();
        let collateral = LiquidBtc::from(Amount::from_btc(1.25).unwrap());
        // LTV of 0.75 = 125% collateral
        let ltv = dec!(0.80);

        let price = calculate_request_price(repayment_amount, collateral, ltv).unwrap();

        assert_eq!(price, LiquidUsdt::from_str_in_dollar("10000").unwrap());
    }

    #[test]
    fn test_calculate_ltv() {
        let repayment_amount = LiquidUsdt::from_str_in_dollar("10500").unwrap();
        let collateral = LiquidBtc::from(Amount::from_btc(0.4).unwrap());
        let current_price = LiquidUsdt::from_str_in_dollar("40000").unwrap();
        let ltv = calculate_ltv(repayment_amount, collateral, current_price).unwrap();

        assert_eq!(ltv, dec!(0.65625))
    }

    #[test]
    fn given_loan_request_acceptable_then_dont_error() {
        let loan_validation_params = LoanValidationParams::test_defaults();

        validate_loan_is_acceptable(loan_validation_params)
            .unwrap()
            .unwrap();
    }

    #[test]
    fn given_loan_request_and_price_drop_lower_then_fluctuation_then_error() {
        let current_price = LiquidUsdt::from_str_in_dollar("39603.96039603").unwrap();
        let loan_validation_params =
            LoanValidationParams::test_defaults().with_current_price(current_price);

        let error = validate_loan_is_acceptable(loan_validation_params.clone())
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            LoanValidationError::PriceNotAcceptable {
                request_price: loan_validation_params.request_price,
                current_price
            }
        )
    }

    #[test]
    fn given_loan_request_and_price_raise_higher_then_fluctuation_then_error() {
        let current_price = LiquidUsdt::from_str_in_dollar("44444.44444445").unwrap();
        let loan_validation_params =
            LoanValidationParams::test_defaults().with_current_price(current_price);

        let error = validate_loan_is_acceptable(loan_validation_params.clone())
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            LoanValidationError::PriceNotAcceptable {
                request_price: loan_validation_params.request_price,
                current_price
            }
        )
    }

    #[test]
    fn given_loan_request_with_principal_lower_min_then_error() {
        let request_principal = LiquidUsdt::from_str_in_dollar("999.99999999").unwrap();
        let loan_validation_params =
            LoanValidationParams::test_defaults().with_request_principal(request_principal);

        let error = validate_loan_is_acceptable(loan_validation_params.clone())
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            LoanValidationError::PrincipalBelowMin {
                request_principal,
                min_principal: loan_validation_params.min_principal
            }
        )
    }

    #[test]
    fn given_loan_request_with_unknown_term_then_error() {
        let terms = vec![
            Term {
                days: 28,
                interest_mod: Decimal::ZERO,
            },
            Term {
                days: 30,
                interest_mod: Decimal::ZERO,
            },
            Term {
                days: 60,
                interest_mod: Decimal::ZERO,
            },
            Term {
                days: 120,
                interest_mod: Decimal::ZERO,
            },
        ];
        let request_term = 29;

        let loan_validation_params = LoanValidationParams::test_defaults()
            .with_terms(terms)
            .with_request_term(request_term);

        let error = validate_loan_is_acceptable(loan_validation_params)
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            LoanValidationError::TermNotAllowed { term: request_term }
        )
    }

    #[test]
    fn given_loan_request_with_ltv_not_offered_returns_error() {
        let ltvs = vec![Ltv { ltv: dec!(0.75) }];
        // honest user, provided_ltv = requested_ltv
        let requested_ltv = dec!(0.5);
        let provided_ltv = requested_ltv;

        let loan_validation_params = LoanValidationParams::test_defaults()
            .with_requested_ltv(requested_ltv)
            .with_provided_ltv(provided_ltv)
            .with_ltvs(ltvs.clone());

        let error = validate_loan_is_acceptable(loan_validation_params)
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            LoanValidationError::InvalidRequestedLtv {
                requested_ltv,
                offered_ltvs: ltvs.iter().map(|ltv| ltv.ltv).collect(),
            }
        );
    }

    #[test]
    fn given_loan_request_different_ltv_than_selected_returns_error() {
        let ltvs = vec![Ltv { ltv: dec!(0.75) }, Ltv { ltv: dec!(0.5) }];
        let requested_ltv = dec!(0.5);
        let provided_ltv = dec!(0.75);

        let loan_validation_params = LoanValidationParams::test_defaults()
            .with_requested_ltv(requested_ltv)
            .with_provided_ltv(provided_ltv)
            .with_ltvs(ltvs);

        let error = validate_loan_is_acceptable(loan_validation_params)
            .unwrap()
            .unwrap_err();

        assert_eq!(
            error,
            LoanValidationError::InvalidProvidedLtv {
                requested_ltv,
                provided_ltv
            }
        );
    }
    #[test]
    fn requested_ltv_is_slightly_different_to_provided_ltv() {
        let ltvs = vec![Ltv { ltv: dec!(0.75) }, Ltv { ltv: dec!(0.5) }];
        let requested_ltv = dec!(0.5);
        let provided_ltv = dec!(0.505);

        let loan_validation_params = LoanValidationParams::test_defaults()
            .with_requested_ltv(requested_ltv)
            .with_provided_ltv(provided_ltv)
            .with_ltvs(ltvs);

        validate_loan_is_acceptable(loan_validation_params)
            .unwrap()
            .unwrap();
    }

    #[test]
    fn given_loan_request_with_in_list_is_ok() {
        let ltvs = vec![Ltv { ltv: dec!(0.75) }];
        let requested_ltv = dec!(0.75);
        let provided_ltv = dec!(0.75);

        let loan_validation_params = LoanValidationParams::test_defaults()
            .with_requested_ltv(requested_ltv)
            .with_provided_ltv(provided_ltv)
            .with_ltvs(ltvs);

        validate_loan_is_acceptable(loan_validation_params)
            .unwrap()
            .unwrap();
    }

    #[allow(unused_variables)]
    #[allow(dead_code)]
    impl LoanValidationParams {
        fn test_defaults() -> Self {
            let request_price = LiquidUsdt::from_str_in_dollar("40000").unwrap();
            let current_price = LiquidUsdt::from_str_in_dollar("39603.96039604").unwrap();
            let price_fluctuation_interval = (dec!(0.90), dec!(1.01));
            let request_principal = LiquidUsdt::from_str_in_dollar("1000").unwrap();
            let min_principal = LiquidUsdt::from_str_in_dollar("1000").unwrap();
            let max_principal = LiquidUsdt::from_str_in_dollar("10000").unwrap();
            let provided_ltv = dec!(0.5);
            let max_ltv = dec!(0.8);
            let request_term = 30;
            let terms = vec![Term {
                days: 30,
                interest_mod: Decimal::ZERO,
            }];
            let requested_ltv = dec!(0.5);
            let ltvs = vec![Ltv { ltv: dec!(0.5) }];

            LoanValidationParams {
                request_price,
                current_price,
                price_fluctuation_interval,
                request_principal,
                min_principal,
                max_principal,
                provided_ltv,
                max_ltv,
                request_term,
                terms,
                requested_ltv,
                ltvs,
            }
        }

        pub fn with_request_price(mut self, request_price: LiquidUsdt) -> Self {
            self.request_price = request_price;
            self
        }
        pub fn with_current_price(mut self, current_price: LiquidUsdt) -> Self {
            self.current_price = current_price;
            self
        }
        pub fn with_price_fluctuation_interval(
            mut self,
            price_fluctuation_interval: (Decimal, Decimal),
        ) -> Self {
            self.price_fluctuation_interval = price_fluctuation_interval;
            self
        }
        pub fn with_request_principal(mut self, request_principal: LiquidUsdt) -> Self {
            self.request_principal = request_principal;
            self
        }
        pub fn with_min_principal(mut self, min_principal: LiquidUsdt) -> Self {
            self.min_principal = min_principal;
            self
        }
        pub fn with_max_principal(mut self, max_principal: LiquidUsdt) -> Self {
            self.max_principal = max_principal;
            self
        }
        pub fn with_provided_ltv(mut self, provided_ltv: Decimal) -> Self {
            self.provided_ltv = provided_ltv;
            self
        }
        pub fn with_max_ltv(mut self, max_ltv: Decimal) -> Self {
            self.max_ltv = max_ltv;
            self
        }
        pub fn with_request_term(mut self, request_term: u32) -> Self {
            self.request_term = request_term;
            self
        }
        pub fn with_terms(mut self, terms: Vec<Term>) -> Self {
            self.terms = terms;
            self
        }
        pub fn with_requested_ltv(mut self, requested_ltv: Decimal) -> Self {
            self.requested_ltv = requested_ltv;
            self
        }
        pub fn with_ltvs(mut self, ltvs: Vec<Ltv>) -> Self {
            self.ltvs = ltvs;
            self
        }
    }
}
