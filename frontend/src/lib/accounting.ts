export const DEFAULT_ACCOUNTING_CURRENCY_CODE = 'CNY';
export const DEFAULT_CREDIT_DISPLAY_NAME = '神社塞钱';
export const DEFAULT_CREDITS_PER_ACCOUNTING_UNIT = '10000';

const CURRENCY_CODE_PATTERN = /^[A-Z]{3}$/;
const POSITIVE_DECIMAL_PATTERN = /^(?:\d+(?:\.\d+)?|\.\d+)$/;
const MAX_DECIMAL_MANTISSA = 79_228_162_514_264_337_593_543_950_335n;

export type AccountingExchangeRate = {
  currencyCode: string;
  quotePerAccountingUnit: string;
};

export type AccountingConversionSettings = {
  accountingCurrencyCode: string;
  exchangeRates: AccountingExchangeRate[];
};

export type InitialAccountingSettingsInput = {
  accountingCurrencyCode: string;
  creditDisplayName: string;
  creditsPerAccountingUnit: string;
};

export type InitialAccountingSettingsValidation = {
  normalized: InitialAccountingSettingsInput;
  fields: Record<keyof InitialAccountingSettingsInput, boolean>;
  isValid: boolean;
};

export function normalizeCurrencyCode(value: string) {
  return value.trim().toUpperCase();
}

export function isCurrencyCode(value: string) {
  return CURRENCY_CODE_PATTERN.test(normalizeCurrencyCode(value));
}

export function isPositiveDecimal(value: string) {
  const normalized = value.trim();
  if (!POSITIVE_DECIMAL_PATTERN.test(normalized)) return false;
  const [whole = '', fraction = ''] = normalized.split('.');
  if (fraction.length > 28) return false;
  const mantissa = BigInt(`${whole || '0'}${fraction}`);
  return mantissa > 0n && mantissa <= MAX_DECIMAL_MANTISSA;
}

export function validateInitialAccountingSettings(input: InitialAccountingSettingsInput): InitialAccountingSettingsValidation {
  const normalized = {
    accountingCurrencyCode: normalizeCurrencyCode(input.accountingCurrencyCode),
    creditDisplayName: input.creditDisplayName.trim(),
    creditsPerAccountingUnit: input.creditsPerAccountingUnit.trim(),
  };
  const fields = {
    accountingCurrencyCode: isCurrencyCode(normalized.accountingCurrencyCode),
    creditDisplayName: normalized.creditDisplayName.length > 0,
    creditsPerAccountingUnit: isPositiveDecimal(normalized.creditsPerAccountingUnit),
  };
  return {
    normalized,
    fields,
    isValid: Object.values(fields).every(Boolean),
  };
}

/**
 * Return the multiplier that converts a real-currency amount into the
 * configured accounting currency.
 *
 * Exchange-rate direction is: `1 accounting unit = quotePerAccountingUnit`
 * units of the quote currency. A quote-currency amount is therefore divided
 * by that rate to obtain the accounting-currency amount.
 */
export function currencyToAccountingFactor(settings: AccountingConversionSettings | undefined, currency: string) {
  if (!settings) return null;
  const accountingCurrency = normalizeCurrencyCode(settings.accountingCurrencyCode);
  const quoteCurrency = normalizeCurrencyCode(currency);
  if (!isCurrencyCode(accountingCurrency) || !isCurrencyCode(quoteCurrency)) return null;
  if (quoteCurrency === accountingCurrency) return 1;

  const rate = settings.exchangeRates.find((item) => normalizeCurrencyCode(item.currencyCode) === quoteCurrency);
  if (!rate || !isPositiveDecimal(rate.quotePerAccountingUnit)) return null;
  return 1 / Number(rate.quotePerAccountingUnit);
}

/** Return the multiplier for converting an amount between two real currencies. */
export function currencyConversionFactor(
  settings: AccountingConversionSettings | undefined,
  sourceCurrency: string,
  targetCurrency: string
) {
  const sourceToAccounting = currencyToAccountingFactor(settings, sourceCurrency);
  const targetToAccounting = currencyToAccountingFactor(settings, targetCurrency);
  if (sourceToAccounting == null || targetToAccounting == null) return null;
  return sourceToAccounting / targetToAccounting;
}

export function scaleDisplayAmount(value: string | number | null | undefined, factor: number | null) {
  if (value == null || factor == null) return null;
  const parsed = typeof value === 'number' ? value : Number(value);
  if (!Number.isFinite(parsed) || !Number.isFinite(factor)) return null;
  return (parsed * factor).toFixed(12).replace(/\.?0+$/, '');
}
