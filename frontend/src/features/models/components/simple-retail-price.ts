export type SimpleRetailPrice = {
  mode: 'tokens' | 'request';
  prompt: string;
  completion: string;
  cacheRead: string;
  cacheWrite: string;
  flatFee: string;
};
export const DECIMAL_PATTERN = /^\d+(?:\.\d{1,12})?$/;
const tokenFields = {
  prompt_tokens: 'prompt',
  completion_tokens: 'completion',
  prompt_cached_tokens: 'cacheRead',
  prompt_write_cached_tokens: 'cacheWrite',
} as const;
const record = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value);
const onlyKeys = (value: Record<string, unknown>, keys: string[]) => Object.keys(value).every((key) => keys.includes(key));

// Fail closed: the simple form replaces only its selected price leaf.
// Decimal strings retain zero and precision; units are fixed by the price contract.
export function readSimpleRetailPrice(value: unknown): SimpleRetailPrice | null {
  const result: SimpleRetailPrice = { mode: 'tokens', prompt: '', completion: '', cacheRead: '', cacheWrite: '', flatFee: '' };
  if (!record(value) || !onlyKeys(value, ['items']) || !Array.isArray(value.items)) return null;
  const seen = new Set<string>();
  for (const item of value.items) {
    if (!record(item) || !onlyKeys(item, ['itemCode', 'pricing', 'promptWriteCacheVariants'])) return null;
    if (
      item.promptWriteCacheVariants !== undefined &&
      (!Array.isArray(item.promptWriteCacheVariants) || item.promptWriteCacheVariants.length !== 0)
    )
      return null;
    if (typeof item.itemCode !== 'string' || seen.has(item.itemCode) || !record(item.pricing)) return null;
    seen.add(item.itemCode);
    const pricing = item.pricing;
    const flat = item.itemCode === 'request' && pricing.mode === 'flat_fee' && value.items.length === 1;
    const field = Object.hasOwn(tokenFields, item.itemCode) ? tokenFields[item.itemCode as keyof typeof tokenFields] : undefined;
    if (!flat && (!field || pricing.mode !== 'usage_per_unit')) return null;
    const amountKey = flat ? 'flatFee' : 'usagePerUnit';
    if (
      !onlyKeys(pricing, ['mode', 'flatFee', 'usagePerUnit', 'usageTiered']) ||
      Object.entries(pricing).some(([key, entry]) => key !== 'mode' && key !== amountKey && entry != null)
    )
      return null;
    const amount = pricing[amountKey];
    if (typeof amount !== 'string' || !DECIMAL_PATTERN.test(amount)) return null;
    if (flat) {
      result.mode = 'request';
      result.flatFee = amount;
    } else result[field!] = amount;
  }
  return result;
}

export type RetailPriceLeaf = { items: unknown[] };
export type RetailPriceRule = { id: string; label: string; condition: string | Record<string, unknown>; price: RetailPriceLeaf };
export type RetailPriceGroup = { modelGroupID: string; rules: RetailPriceRule[]; base: RetailPriceLeaf | null };
export type RetailPriceDocument = { schema: 'group-conditional-price-v1'; global: RetailPriceLeaf | null; groups: RetailPriceGroup[] };

// Keep leaf JSON intact: tiers, cache variants and dimensions belong to the
// canonical backend parser and must survive editing another branch.
export function readRetailPriceDocument(value: unknown): RetailPriceDocument | null {
  const leaf = (v: unknown): v is RetailPriceLeaf => record(v) && Array.isArray(v.items);
  if (leaf(value)) return { schema: 'group-conditional-price-v1', global: value, groups: [] };
  if (
    !record(value) ||
    !onlyKeys(value, ['schema', 'global', 'groups']) ||
    value.schema !== 'group-conditional-price-v1' ||
    !Array.isArray(value.groups)
  )
    return null;
  if (value.global !== null && !leaf(value.global)) return null;
  const ids = new Set<string>();
  for (const group of value.groups) {
    if (
      !record(group) ||
      !onlyKeys(group, ['modelGroupID', 'rules', 'base']) ||
      typeof group.modelGroupID !== 'string' ||
      ids.has(group.modelGroupID) ||
      !Array.isArray(group.rules) ||
      (group.base !== null && !leaf(group.base))
    )
      return null;
    ids.add(group.modelGroupID);
    const rules = new Set<string>();
    for (const rule of group.rules) {
      if (
        !record(rule) ||
        !onlyKeys(rule, ['id', 'label', 'condition', 'price']) ||
        typeof rule.id !== 'string' ||
        !rule.id ||
        rules.has(rule.id) ||
        typeof rule.label !== 'string' ||
        !(typeof rule.condition === 'string' || record(rule.condition)) ||
        !leaf(rule.price)
      )
        return null;
      rules.add(rule.id);
    }
  }
  return value as RetailPriceDocument;
}

export function writeSimpleRetailPrice(price: SimpleRetailPrice): RetailPriceLeaf {
  if (price.mode === 'request') {
    if (!DECIMAL_PATTERN.test(price.flatFee)) throw new Error('Invalid request price');
    return { items: [{ itemCode: 'request', pricing: { mode: 'flat_fee', flatFee: price.flatFee } }] };
  }
  const items = Object.entries(tokenFields).flatMap(([itemCode, field]) => {
    const amount = price[field];
    if (amount === '') return [];
    if (!DECIMAL_PATTERN.test(amount)) throw new Error('Invalid token price');
    return [{ itemCode, pricing: { mode: 'usage_per_unit', usagePerUnit: amount } }];
  });
  if (!items.length) throw new Error('At least one price is required');
  return { items };
}

export function serializeRetailPriceDocument(document: RetailPriceDocument): RetailPriceDocument | RetailPriceLeaf {
  // Preserve the established legacy shape until a group branch is added.
  return document.groups.length === 0 && document.global !== null ? document.global : document;
}

function hasUnsafeInteger(value: unknown): boolean {
  if (typeof value === 'number') return !Number.isFinite(value) || (Number.isInteger(value) && !Number.isSafeInteger(value));
  if (Array.isArray(value)) return value.some(hasUnsafeInteger);
  return record(value) && Object.values(value).some(hasUnsafeInteger);
}

export function validateEditorDocument(value: unknown): string | null {
  if (hasUnsafeInteger(value)) return 'models.groupPrice.unsafeInteger';
  const doc = readRetailPriceDocument(value);
  if (!doc) return 'models.groupPrice.invalidDocument';
  const leaves = [doc.global, ...doc.groups.flatMap((group) => [group.base, ...group.rules.map((rule) => rule.price)])].filter(
    (leaf): leaf is RetailPriceLeaf => leaf !== null
  );
  if (!leaves.length || doc.groups.some((group) => !group.base && !group.rules.length)) return 'models.groupPrice.invalidDocument';
  for (const leaf of leaves) {
    if (!leaf.items.length) return 'models.groupPrice.invalidLeaf';
    const simple = readSimpleRetailPrice(leaf);
    if (simple) {
      try {
        writeSimpleRetailPrice(simple);
      } catch {
        return 'models.groupPrice.invalidLeaf';
      }
    }
  }
  return null;
}
