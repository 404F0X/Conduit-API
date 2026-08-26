import assert from 'node:assert/strict';
import test from 'node:test';
import { priceColumns } from './model-catalog-pricing.ts';

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
