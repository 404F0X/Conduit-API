interface CatalogPricing {
  mode: string;
  flatFee?: string | number | null;
  usagePerUnit?: string | number | null;
  usageTiered?: {
    tiers: Array<{ pricePerUnit: string | number }>;
  } | null;
}

interface CatalogModelPrice {
  currencyCode: string;
  price: {
    items: Array<{
      itemCode: string;
      pricing: CatalogPricing;
    }>;
  };
}

function formatPricing(pricing?: CatalogPricing) {
  if (!pricing) return '—';
  if (pricing.mode === 'flat_fee') return pricing.flatFee == null ? '—' : String(pricing.flatFee);
  if (pricing.mode === 'usage_per_unit' || pricing.mode === 'usage_volume') {
    return pricing.usagePerUnit == null ? '—' : String(pricing.usagePerUnit);
  }
  const first = pricing.usageTiered?.tiers?.[0];
  return first ? `${first.pricePerUnit}+` : '—';
}

export function priceColumns(price?: CatalogModelPrice) {
  const items = price?.price.items || [];
  const request = items.find((item) => item.pricing.mode === 'flat_fee')?.pricing;
  const getUsage = (code: string) => items.find((item) => item.itemCode === code && item.pricing.mode !== 'flat_fee')?.pricing;

  return {
    currency: price?.currencyCode,
    request: formatPricing(request),
    input: formatPricing(getUsage('prompt_tokens')),
    output: formatPricing(getUsage('completion_tokens')),
    cacheRead: formatPricing(getUsage('prompt_cached_tokens')),
    cacheWrite: formatPricing(getUsage('prompt_write_cached_tokens')),
  };
}
