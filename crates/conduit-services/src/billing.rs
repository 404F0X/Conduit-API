//! Exact user Credit and subscription allowance arithmetic.

use rust_decimal::Decimal;
use rust_decimal::RoundingStrategy;
use rust_decimal::prelude::ToPrimitive;

pub const MICROS_PER_CREDIT: i64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettlementAllocation {
    pub subscription_micros: i64,
    pub credit_micros: i64,
    pub shortfall_micros: i64,
}

pub fn decimal_to_micros(value: Decimal) -> Result<i64, &'static str> {
    (value * Decimal::from(MICROS_PER_CREDIT))
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        .to_i64()
        .ok_or("amount is outside the supported range")
}

pub fn micros_to_decimal(value: i64) -> Decimal {
    Decimal::from(value) / Decimal::from(MICROS_PER_CREDIT)
}

/// Consume expiring subscription allowance first, then durable Credit.
pub fn allocate_charge(
    amount_micros: i64,
    subscription_available_micros: i64,
    credit_available_micros: i64,
) -> SettlementAllocation {
    let amount = amount_micros.max(0);
    let subscription = amount.min(subscription_available_micros.max(0));
    let after_subscription = amount - subscription;
    let credit = after_subscription.min(credit_available_micros.max(0));
    SettlementAllocation {
        subscription_micros: subscription,
        credit_micros: credit,
        shortfall_micros: after_subscription - credit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_round_trip_uses_exact_micros() -> Result<(), &'static str> {
        let micros = decimal_to_micros(Decimal::new(1234567, 6))?;
        assert_eq!(micros, 1_234_567);
        assert_eq!(micros_to_decimal(micros), Decimal::new(1234567, 6));
        Ok(())
    }

    #[test]
    fn subscription_is_spent_before_credit() {
        assert_eq!(
            allocate_charge(120, 100, 50),
            SettlementAllocation {
                subscription_micros: 100,
                credit_micros: 20,
                shortfall_micros: 0,
            }
        );
        assert_eq!(allocate_charge(200, 100, 50).shortfall_micros, 50);
    }
}
