export function objectValue(value: unknown): Record<string, unknown> {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}
export function priceRows(snapshot: unknown) {
  const record = objectValue(snapshot);
  const price = objectValue(record.price ?? snapshot);
  return Array.isArray(price.items)
    ? price.items.flatMap((raw) => {
        const item = objectValue(raw);
        const row = (value: unknown, variantCode?: string) => {
          const pricing = objectValue(value);
          const tiered = objectValue(pricing.usageTiered ?? pricing.usage_tiered);
          return {
            code: String(item.itemCode ?? item.item_code ?? ''),
            variantCode,
            mode: String(pricing.mode ?? ''),
            amount: pricing.usagePerUnit ?? pricing.usage_per_unit ?? pricing.flatFee ?? pricing.flat_fee ?? null,
            tiers: Array.isArray(tiered.tiers)
              ? tiered.tiers.map((tier) => {
                  const value = objectValue(tier);
                  return { upTo: value.upTo ?? value.up_to ?? null, amount: value.pricePerUnit ?? value.price_per_unit ?? null };
                })
              : [],
            pricing,
          };
        };
        const variants = item.promptWriteCacheVariants ?? item.prompt_write_cache_variants;
        return [
          row(item.pricing),
          ...(Array.isArray(variants)
            ? variants.map((raw) => {
                const variant = objectValue(raw);
                return row(variant.pricing, String(variant.variantCode ?? variant.variant_code ?? ''));
              })
            : []),
        ];
      })
    : [];
}
export function displayAmount(value: unknown): string | null {
  return value === null || value === undefined || value === ''
    ? null
    : typeof value === 'string' || typeof value === 'number'
      ? String(value)
      : null;
}
export function updatePriceAmount(price: unknown, code: string, amount: string) {
  const record = structuredClone(objectValue(price));
  if (record.kind === 'NEW_API_EXPRESSION' || record.schema === 'group-conditional-price-v1')
    throw new Error('Edit expression and conditional prices in their price editor');
  const items = Array.isArray(record.items) ? record.items.map(objectValue) : [];
  const index = items.findIndex((item) => (item.itemCode ?? item.item_code) === code);
  const old = index >= 0 ? items[index] : { itemCode: code, pricing: { mode: code === 'request' ? 'flat_fee' : 'usage_per_unit' } };
  const pricing = objectValue(old.pricing);
  if (!['usage_per_unit', 'flat_fee'].includes(String(pricing.mode)))
    throw new Error('A tiered or compound price must be edited in the channel price editor');
  const next = { ...old, pricing: { ...pricing, [pricing.mode === 'flat_fee' ? 'flatFee' : 'usagePerUnit']: amount } };
  if (index >= 0) items[index] = next;
  else items.push(next);
  return { ...record, items };
}

export function snapshotFieldValues(snapshot: unknown): Record<string, unknown> {
  const record = objectValue(snapshot);
  const price = objectValue(record.billingExpression ?? record.price ?? snapshot);
  if (price.kind === 'NEW_API_EXPRESSION') return { billingExpression: price };
  if (price.schema === 'group-conditional-price-v1') return { conditionalPrice: price };
  const rows = priceRows(snapshot);
  if (rows.length) {
    return Object.fromEntries(
      (price.items as unknown[]).map((raw) => {
        const item = objectValue(raw);
        return [String(item.itemCode ?? item.item_code ?? ''), item];
      })
    );
  }
  const model = objectValue(record.publicModel ?? record);
  return Object.fromEntries(
    ['modelID', 'name', 'developer', 'type', 'group', 'deploymentID', 'routeStatus', 'status']
      .filter((key) => model[key] !== undefined || record[key] !== undefined)
      .map((key) => [key, model[key] ?? record[key]])
  );
}
function canonicalValue(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalValue).join(',')}]`;
  if (value !== null && typeof value === 'object')
    return `{${Object.entries(value)
      .sort(([a], [b]) => a.localeCompare(b))
      .map(([key, item]) => `${JSON.stringify(key)}:${canonicalValue(item)}`)
      .join(',')}}`;
  return JSON.stringify(value) ?? 'undefined';
}
export function changedSnapshotFields(before: unknown, after: unknown): string[] {
  const original = snapshotFieldValues(before);
  const proposed = snapshotFieldValues(after);
  return [...new Set([...Object.keys(original), ...Object.keys(proposed)])].filter(
    (key) => canonicalValue(original[key]) !== canonicalValue(proposed[key])
  );
}
export function isDefinitePackageRejection(error: unknown): boolean {
  return (
    error instanceof Error &&
    /^(OPERATION_CONFLICT|SOURCE_CHANGED|REVISION_CONFLICT|VALIDATION_FAILED|STATE_CHANGED|FORBIDDEN|INVALID_INPUT):/.test(error.message)
  );
}
