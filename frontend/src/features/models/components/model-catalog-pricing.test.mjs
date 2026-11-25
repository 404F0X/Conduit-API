import assert from 'node:assert/strict';
import test from 'node:test';
import { priceColumns, retailPriceValues } from './model-catalog-pricing.ts';

test('flat fees are classified as per-request prices rather than token prices', () => {
  const values = priceColumns({
    currencyCode: 'CNY',
    price: {
      items: [
        {
          itemCode: 'prompt_tokens',
          pricing: { mode: 'flat_fee', flatFee: '0.002' },
        },
      ],
    },
  });

  assert.deepEqual(values, {
    currency: 'CNY',
    request: '0.002',
    input: '—',
    output: '—',
    cacheRead: '—',
    cacheWrite: '—',
  });
});

test('usage prices stay in their token columns', () => {
  const values = priceColumns({
    currencyCode: 'CNY',
    price: {
      items: [
        { itemCode: 'prompt_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: '1' } },
        { itemCode: 'completion_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: '2' } },
        { itemCode: 'prompt_cached_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: '0.1' } },
        { itemCode: 'prompt_write_cached_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: '1.25' } },
      ],
    },
  });

  assert.deepEqual(values, {
    currency: 'CNY',
    request: '—',
    input: '1',
    output: '2',
    cacheRead: '0.1',
    cacheWrite: '1.25',
  });
});

const tokenLeaf = (amount = '0') => ({ items: [{ itemCode: 'prompt_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: amount } }] });
const groupDocument = (global = tokenLeaf('9')) => ({
  schema: 'group-conditional-price-v1',
  global,
  groups: [
    {
      modelGroupID: '550e8400-e29b-41d4-a716-446655440000',
      base: tokenLeaf('7'),
      rules: [{ id: 'token-rule', label: 'Rule', condition: 'prompt_tokens >= 100', price: tokenLeaf('2') }],
    },
  ],
});

test('admin summary identifies group pricing and global fallback without choosing a leaf', () => {
  assert.deepEqual(retailPriceValues({ price: groupDocument() }), { mode: 'grouped', globalFallback: true });
  assert.deepEqual(retailPriceValues({ price: groupDocument(null) }), { mode: 'grouped', globalFallback: false });
  assert.deepEqual(retailPriceValues({ price: groupDocument(tokenLeaf('0')) }), { mode: 'grouped', globalFallback: true });
});

test('admin summary retains plain token and request zero prices', () => {
  assert.equal(retailPriceValues({ price: tokenLeaf() }).values[0].value, '0');
  assert.deepEqual(retailPriceValues({ price: { items: [{ itemCode: 'request', pricing: { mode: 'flat_fee', flatFee: '0' } }] } }), {
    mode: 'request',
    flatFee: '0',
  });
  assert.equal(retailPriceValues(), null);
});

test('admin summary keeps complex prices structured instead of projecting a fake scalar', () => {
  for (const mode of ['usage_tiered', 'usage_volume']) {
    assert.deepEqual(
      retailPriceValues({
        price: { items: [{ itemCode: 'prompt_tokens', pricing: { mode, usageTiered: { tiers: [{ pricePerUnit: '2' }] } } }] },
      }),
      { mode: 'structured' }
    );
  }
  const cache = tokenLeaf();
  cache.items[0].promptWriteCacheVariants = [{ variantCode: 'five_min', pricing: { mode: 'usage_per_unit', usagePerUnit: '3' } }];
  assert.deepEqual(retailPriceValues({ price: cache }), { mode: 'structured' });
});

test('admin summary reports malformed documents and leaves unavailable without throwing', () => {
  const brokenGroup = groupDocument();
  brokenGroup.groups[0].rules[0].price = { items: [null] };
  for (const price of [
    null,
    {},
    { schema: 'unknown', groups: [] },
    { items: [] },
    { items: [null] },
    { items: [{ itemCode: 'prompt_tokens', pricing: null }] },
    { items: [{ itemCode: 'prompt_tokens', pricing: { mode: 'usage_tiered' } }] },
    tokenLeaf('-1'),
    brokenGroup,
  ]) {
    assert.deepEqual(retailPriceValues({ price }), { mode: 'unavailable' });
  }
});

test('a published expression is displayed symbolically with its exact multiplier', () => {
  const expression = {
    kind: 'NEW_API_EXPRESSION',
    profile: 'conduit-newapi-token-v1',
    version: 1,
    expression: 'fixed(0.002)',
    multiplier: '1.000000000000000001',
  };
  assert.deepEqual(retailPriceValues({ price: expression }), { mode: 'expression', price: expression });
  assert.deepEqual(retailPriceValues({ price: { ...expression, items: [] } }), { mode: 'unavailable' });
});
