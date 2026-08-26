import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const require = createRequire(import.meta.url);
const sourcePath = join(import.meta.dirname, 'schema.ts');
const source = readFileSync(sourcePath, 'utf8');
const transpiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.CommonJS,
    target: ts.ScriptTarget.ES2023,
  },
}).outputText;

const schemaModule = { exports: {} };
const localRequire = (specifier) => {
  if (specifier === '@/gql/pagination') {
    return { pageInfoSchema: require('zod').z.any() };
  }
  return require(specifier);
};
new Function('require', 'module', 'exports', transpiled)(localRequire, schemaModule, schemaModule.exports);

const {
  capabilityPolicySchema,
  channelModelPriceSchema,
  channelPoliciesSchema,
  channelSchema,
  channelOrderingItemSchema,
  channelSummarySchema,
  saveChannelModelPriceInputSchema,
} = schemaModule.exports;

const graphqlChannel = {
  id: 'channel-1',
  createdAt: '2026-08-17T00:00:00Z',
  updatedAt: '2026-08-17T00:00:00Z',
  type: 'openai',
  baseURL: null,
  name: 'Nullable Rust channel',
  status: 'enabled',
  policies: { stream: null },
  supportedModels: [],
  autoSyncSupportedModels: false,
  defaultTestModel: '',
};

test('accepts and normalizes nullable Channel.baseURL and ChannelPolicies.stream from Rust GraphQL', () => {
  const parsed = channelSchema.parse(graphqlChannel);

  assert.equal(parsed.baseURL, '');
  assert.equal(parsed.policies.stream, undefined);
});

test('normalizes nullable baseURL in the summary and ordering query projections', () => {
  const common = {
    id: 'channel-1',
    name: 'Nullable Rust channel',
    type: 'openai',
    status: 'enabled',
    baseURL: null,
    orderingWeight: 0,
  };

  assert.equal(channelSummarySchema.parse(common).baseURL, '');
  assert.equal(channelOrderingItemSchema.parse(common).baseURL, '');
});

test('accepts the lowercase GraphQL CapabilityPolicy values and rejects other spellings', () => {
  for (const policy of ['unlimited', 'require', 'forbid']) {
    assert.equal(capabilityPolicySchema.parse(policy), policy);
    assert.equal(channelPoliciesSchema.parse({ stream: policy }).stream, policy);
  }

  assert.equal(capabilityPolicySchema.safeParse('UNLIMITED').success, false);
});

const modelPrice = {
  items: [
    {
      itemCode: 'prompt_tokens',
      pricing: { mode: 'usage_per_unit', usagePerUnit: '1.25' },
    },
  ],
};

test('requires and preserves each channel price row currency, including mixed currencies', () => {
  const rows = [
    { id: 'price-cny', modelID: 'model-a', currencyCode: 'CNY', price: modelPrice },
    { id: 'price-usd', modelID: 'model-b', currencyCode: 'USD', price: modelPrice },
  ].map((row) => channelModelPriceSchema.parse(row));

  assert.deepEqual(
    rows.map((row) => row.currencyCode),
    ['CNY', 'USD']
  );
  assert.equal(channelModelPriceSchema.safeParse({ id: 'price-old', modelID: 'model-c', price: modelPrice }).success, false);
});

test('requires a row currency when saving channel model prices', () => {
  assert.equal(saveChannelModelPriceInputSchema.safeParse({ modelId: 'model-a', currencyCode: 'CNY', price: modelPrice }).success, true);
  assert.equal(saveChannelModelPriceInputSchema.safeParse({ modelId: 'model-a', price: modelPrice }).success, false);
});
