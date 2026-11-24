import { readRetailPriceDocument, readSimpleRetailPrice } from './simple-retail-price.ts';

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

// Administrative summaries have no request group or token/time facts. Keep the
// document intact and never choose a branch or advertise a cross-group price.
export function retailPriceValues(item?: { price: unknown }) {
  if (!item) return null;
  const doc = readRetailPriceDocument(item.price);
  if (!doc || doc.groups.some((group) => !group.base && !group.rules.length)) return { mode: 'unavailable' as const };
  const leaves = [doc.global, ...doc.groups.flatMap((group) => [group.base, ...group.rules.map((rule) => rule.price)])].filter(
    (leaf) => leaf !== null
  );
  if (
    !leaves.length ||
    leaves.some(
      (leaf) =>
        !leaf.items.length ||
        leaf.items.some((entry) => {
          if (!entry || typeof entry !== 'object' || !('itemCode' in entry) || typeof entry.itemCode !== 'string' || !('pricing' in entry))
            return true;
          const pricing = entry.pricing;
          if (!pricing || typeof pricing !== 'object' || !('mode' in pricing)) return true;
          if (pricing.mode === 'flat_fee')
            return !('flatFee' in pricing) || typeof pricing.flatFee !== 'string' || !/^\d+(?:\.\d+)?$/.test(pricing.flatFee);
          if (pricing.mode === 'usage_per_unit')
            return (
              !('usagePerUnit' in pricing) || typeof pricing.usagePerUnit !== 'string' || !/^\d+(?:\.\d+)?$/.test(pricing.usagePerUnit)
            );
          if (pricing.mode !== 'usage_tiered' && pricing.mode !== 'usage_volume') return true;
          if (
            !('usageTiered' in pricing) ||
            !pricing.usageTiered ||
            typeof pricing.usageTiered !== 'object' ||
            !('tiers' in pricing.usageTiered)
          )
            return true;
          const tiers = pricing.usageTiered.tiers;
          return (
            !Array.isArray(tiers) ||
            !tiers.length ||
            tiers.some(
              (tier: unknown) =>
                !tier ||
                typeof tier !== 'object' ||
                !('pricePerUnit' in tier) ||
                typeof tier.pricePerUnit !== 'string' ||
                !/^\d+(?:\.\d+)?$/.test(tier.pricePerUnit)
            )
          );
        })
    )
  )
    return { mode: 'unavailable' as const };
  if (doc.groups.length) return { mode: 'grouped' as const, globalFallback: doc.global !== null };
  const simple = readSimpleRetailPrice(doc.global);
  if (!simple) return { mode: 'structured' as const };
  if (simple.mode === 'request') return { mode: 'request' as const, flatFee: simple.flatFee };
  return {
    mode: 'tokens' as const,
    values: [
      { code: 'prompt_tokens', label: 'models.catalog.retailInput', value: simple.prompt || undefined },
      { code: 'completion_tokens', label: 'models.catalog.retailOutput', value: simple.completion || undefined },
      { code: 'prompt_cached_tokens', label: 'models.catalog.retailCacheRead', value: simple.cacheRead || undefined },
      { code: 'prompt_write_cached_tokens', label: 'models.catalog.retailCacheWrite', value: simple.cacheWrite || undefined },
    ],
  };
}
