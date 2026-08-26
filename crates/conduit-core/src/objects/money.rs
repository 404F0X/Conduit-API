//! Canonical accounting-currency conversion for Conduit API.
//!
//! Procurement costs and retail prices are stored in one real-world
//! accounting currency. Station credits are a consumption/display unit derived
//! from that accounting amount; their display name is never a ledger identity.

use std::collections::BTreeSet;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Stable internal identity used by every station-credit wallet and allowance.
pub const STATION_CREDIT_CODE: &str = "STATION_CREDIT";
pub const DEFAULT_ACCOUNTING_CURRENCY_CODE: &str = "CNY";
pub const DEFAULT_CREDIT_DISPLAY_NAME: &str = "神社塞钱";

fn default_accounting_currency() -> String {
    DEFAULT_ACCOUNTING_CURRENCY_CODE.to_string()
}

fn default_credit_display_name() -> String {
    DEFAULT_CREDIT_DISPLAY_NAME.to_string()
}

fn default_credits_per_accounting_unit() -> Decimal {
    Decimal::from(10_000)
}

fn default_version() -> u64 {
    1
}

/// A manual/externally refreshed FX quote:
/// `1 accounting unit = quote_per_accounting_unit quote-currency units`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CurrencyExchangeRate {
    pub currency: String,
    pub quote_per_accounting_unit: Decimal,
}

/// Versioned money settings stored with the system general settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct AccountingSettings {
    #[serde(default = "default_accounting_currency")]
    pub accounting_currency: String,
    #[serde(default = "default_credit_display_name")]
    pub credit_display_name: String,
    #[serde(default = "default_credits_per_accounting_unit")]
    pub credits_per_accounting_unit: Decimal,
    #[serde(default)]
    pub exchange_rates: Vec<CurrencyExchangeRate>,
    #[serde(default = "default_version")]
    pub version: u64,
}

impl Default for AccountingSettings {
    fn default() -> Self {
        Self {
            accounting_currency: default_accounting_currency(),
            credit_display_name: default_credit_display_name(),
            credits_per_accounting_unit: default_credits_per_accounting_unit(),
            exchange_rates: Vec::new(),
            version: default_version(),
        }
    }
}

impl AccountingSettings {
    pub fn validate(&self) -> Result<(), String> {
        validate_currency_code(&self.accounting_currency, "accounting currency")?;
        if self.credit_display_name.trim().is_empty() {
            return Err("credit display name is required".into());
        }
        if self.credits_per_accounting_unit <= Decimal::ZERO {
            return Err("credits per accounting unit must be positive".into());
        }
        if self.version == 0 {
            return Err("accounting settings version must be positive".into());
        }

        let mut currencies = BTreeSet::new();
        for rate in &self.exchange_rates {
            validate_currency_code(&rate.currency, "exchange-rate currency")?;
            if rate.quote_per_accounting_unit <= Decimal::ZERO {
                return Err("exchange rates must be positive".into());
            }
            let currency = rate.currency.trim().to_ascii_uppercase();
            if currency.eq_ignore_ascii_case(self.accounting_currency.trim()) {
                return Err("accounting currency must not have an exchange-rate entry".into());
            }
            if !currencies.insert(currency.clone()) {
                return Err(format!("duplicate exchange rate for {currency}"));
            }
        }
        Ok(())
    }

    /// Convert an amount in a real currency into the accounting currency.
    pub fn real_to_accounting(&self, amount: Decimal, currency: &str) -> Result<Decimal, String> {
        self.validate()?;
        Ok(amount / self.quote_per_accounting_unit(currency)?)
    }

    /// Convert an accounting-currency amount into another real currency.
    pub fn accounting_to_real(&self, amount: Decimal, currency: &str) -> Result<Decimal, String> {
        self.validate()?;
        Ok(amount * self.quote_per_accounting_unit(currency)?)
    }

    pub fn accounting_to_credits(&self, amount: Decimal) -> Result<Decimal, String> {
        self.validate()?;
        Ok(amount * self.credits_per_accounting_unit)
    }

    /// Channel prices are quoted in channel balance units. Divide by the
    /// recharge multiplier first, then convert the resulting real currency.
    pub fn channel_units_to_accounting(
        &self,
        channel_units: Decimal,
        billing_currency: &str,
        recharge_units_per_currency: Decimal,
    ) -> Result<Decimal, String> {
        if recharge_units_per_currency <= Decimal::ZERO {
            return Err("channel recharge multiplier must be positive".into());
        }
        self.real_to_accounting(
            channel_units / recharge_units_per_currency,
            billing_currency,
        )
    }

    fn quote_per_accounting_unit(&self, currency: &str) -> Result<Decimal, String> {
        if currency.eq_ignore_ascii_case(self.accounting_currency.trim()) {
            return Ok(Decimal::ONE);
        }
        self.exchange_rates
            .iter()
            .find(|rate| rate.currency.eq_ignore_ascii_case(currency.trim()))
            .map(|rate| rate.quote_per_accounting_unit)
            .ok_or_else(|| format!("missing exchange rate for {currency}"))
    }
}

fn validate_currency_code(value: &str, field: &str) -> Result<(), String> {
    let value = value.trim();
    if value.len() == 3
        && value
            .chars()
            .all(|character| character.is_ascii_alphabetic())
    {
        Ok(())
    } else {
        Err(format!("{field} must be a 3-letter ISO currency code"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_channel_credits_through_accounting_currency() -> Result<(), String> {
        let settings = AccountingSettings {
            accounting_currency: "CNY".into(),
            credits_per_accounting_unit: Decimal::from(10_000),
            exchange_rates: vec![CurrencyExchangeRate {
                currency: "USD".into(),
                quote_per_accounting_unit: Decimal::new(1, 1), // 1 CNY = 0.1 USD
            }],
            ..Default::default()
        };
        let cost = settings.channel_units_to_accounting(Decimal::ONE, "USD", Decimal::from(10))?;
        assert_eq!(cost, Decimal::ONE); // 1 credit / 10 = $0.1 = CNY 1
        assert_eq!(settings.accounting_to_credits(cost)?, Decimal::from(10_000));
        Ok(())
    }

    #[test]
    fn accounting_and_real_currency_round_trip() -> Result<(), String> {
        let settings = AccountingSettings {
            exchange_rates: vec![CurrencyExchangeRate {
                currency: "USD".into(),
                quote_per_accounting_unit: Decimal::new(14, 2),
            }],
            ..Default::default()
        };
        let accounting = Decimal::new(125, 2);
        let usd = settings.accounting_to_real(accounting, "USD")?;
        assert_eq!(settings.real_to_accounting(usd, "USD")?, accounting);
        Ok(())
    }

    #[test]
    fn defaults_are_stable_and_display_name_is_not_the_ledger_key() -> Result<(), String> {
        let settings = AccountingSettings::default();
        assert_eq!(settings.accounting_currency, "CNY");
        assert_eq!(settings.credit_display_name, "神社塞钱");
        assert_eq!(settings.credits_per_accounting_unit, Decimal::from(10_000));
        assert_ne!(settings.credit_display_name, STATION_CREDIT_CODE);
        settings.validate()?;
        Ok(())
    }
}
