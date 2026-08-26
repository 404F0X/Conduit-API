const INTL_CURRENCY_CODE = /^[A-Z]{3}$/;

function normalizedLocale(locale?: string) {
  return locale?.trim() || undefined;
}

function normalizedCurrencyLabel(currencyCode?: string) {
  return currencyCode?.trim() || 'USD';
}

function numericValue(value: unknown): number | bigint {
  return typeof value === 'bigint' ? value : Number(value);
}

/**
 * Format both real-world currencies and configurable unit labels.
 *
 * Intl.NumberFormat only accepts structurally valid three-letter currency
 * codes. Other labels therefore use decimal formatting and an explicit unit
 * suffix instead of pretending to be an ISO currency.
 */
export function formatCurrencyValue(value: unknown, currencyCode?: string, locale?: string, options: Intl.NumberFormatOptions = {}) {
  const label = normalizedCurrencyLabel(currencyCode);
  const code = label.toUpperCase();
  const resolvedLocale = normalizedLocale(locale);

  if (INTL_CURRENCY_CODE.test(code)) {
    return new Intl.NumberFormat(resolvedLocale, {
      ...options,
      style: 'currency',
      currency: code,
      currencyDisplay: 'narrowSymbol',
    }).format(numericValue(value));
  }

  const decimalOptions = { ...options };
  delete decimalOptions.currency;
  delete decimalOptions.currencyDisplay;
  delete decimalOptions.currencySign;

  const formattedValue = new Intl.NumberFormat(resolvedLocale, {
    ...decimalOptions,
    style: 'decimal',
  }).format(numericValue(value));
  return `${formattedValue} ${label}`;
}

/** Return the compact label shown inside a price input. */
export function getCurrencyInputAffix(currencyCode?: string, locale?: string) {
  if (!currencyCode?.trim()) return '';

  const label = normalizedCurrencyLabel(currencyCode);
  const code = label.toUpperCase();
  const resolvedLocale = normalizedLocale(locale);
  if (!INTL_CURRENCY_CODE.test(code)) {
    return label;
  }

  return (
    new Intl.NumberFormat(resolvedLocale, {
      style: 'currency',
      currency: code,
      currencyDisplay: 'narrowSymbol',
    })
      .formatToParts(0)
      .find((part) => part.type === 'currency')?.value ?? code
  );
}
