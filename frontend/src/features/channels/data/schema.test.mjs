import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const require = createRequire(import.meta.url);
function loadSchema(path) {
  const source = readFileSync(join(import.meta.dirname, path), 'utf8');
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
    if (specifier === '@/features/models/components/simple-retail-price') {
      return loadSchema('../../models/components/simple-retail-price.ts');
    }
    if (specifier === '@/features/models/data/schema') {
      return loadSchema('../../models/data/schema.ts');
    }
    return require(specifier);
  };
  new Function('require', 'module', 'exports', transpiled)(localRequire, schemaModule, schemaModule.exports);
  return schemaModule.exports;
}

const {
  capabilityPolicySchema,
  newApiModelPricingProbeSchema,
  channelModelPriceSchema,
  channelPoliciesSchema,
  channelSchema,
  channelOrderingItemSchema,
  updateChannelInputSchema,
  channelSummarySchema,
  saveChannelModelPriceInputSchema,
} = loadSchema('schema.ts');

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

test('channel updates preserve exact trimmed public bindings independently of upstream alias rewrites', () => {
  const publicModelBindings = {
    upserts: [{ upstreamModelID: ' X/Y ', publicModelKey: ' Public/Key ', modelType: 'chat' }],
    removeRouteIDs: ['route-id'],
  };
  const settings = { modelMappings: [{ from: 'X/Y', to: 'upstream-wire-name' }] };
  const result = updateChannelInputSchema.parse({ publicModelBindings, settings });
  assert.deepEqual(result.publicModelBindings, {
    upserts: [{ upstreamModelID: 'X/Y', publicModelKey: 'Public/Key', modelType: 'chat' }],
    removeRouteIDs: ['route-id'],
  });
  assert.deepEqual(result.settings.modelMappings, settings.modelMappings);
  assert.equal(updateChannelInputSchema.parse({}).publicModelBindings, undefined);
  assert.equal(updateChannelInputSchema.parse({ publicModelBindings: null }).publicModelBindings, null);
});

test('approved channel expression is a distinct USD leaf and mixed provider inputs are rejected', () => {
  const billingExpression = {
    kind: 'NEW_API_EXPRESSION',
    profile: 'conduit-newapi-token-v1',
    version: 1,
    expression: 'p * 2 + c * 8',
    multiplier: '1.234567890123456789',
  };
  const row = { id: 'row', modelID: 'upstream', currencyCode: 'USD', price: null, billingExpression };
  assert.deepEqual(channelModelPriceSchema.parse(row), row);
  const input = { modelId: row.modelID, currencyCode: row.currencyCode, billingExpression };
  assert.deepEqual(saveChannelModelPriceInputSchema.parse(input), input);
  assert.equal(channelModelPriceSchema.safeParse({ ...row, billingExpression: null }).success, false);
  assert.equal(saveChannelModelPriceInputSchema.safeParse({ ...input, price: modelPrice }).success, false);
  assert.equal(
    saveChannelModelPriceInputSchema.safeParse({ ...input, billingExpression: { ...billingExpression, items: [] } }).success,
    false
  );
});

test('NEWAPI probe keeps severity-less or unfamiliar diagnostics and its adoption guard', () => {
  const issue = { code: 'UPSTREAM_CONTRACT_UNKNOWN', message: 'Pinned version required' };
  const model = {
    modelId: 'model',
    billingKind: 'tiered_expr',
    quality: 'unsupported',
    provenance: { adoptionBlocked: true, originalEvidence: 'retained' },
    diagnostics: [issue],
  };
  for (const issues of [[], [issue], [{ ...issue, severity: 'BLOCKING' }], [{ ...issue, severity: 'REPORT_ONLY' }]]) {
    const parsed = newApiModelPricingProbeSchema.parse({ ...model, provenance: { ...model.provenance, issues } });
    assert.deepEqual(parsed.diagnostics, [issue]);
    assert.deepEqual(parsed.provenance.issues, issues);
    assert.equal(parsed.provenance.adoptionBlocked, true);
    assert.equal(parsed.provenance.originalEvidence, 'retained');
  }
  assert.equal(newApiModelPricingProbeSchema.safeParse({ ...model, diagnostics: [{ message: issue.message }] }).success, false);
  assert.equal(newApiModelPricingProbeSchema.safeParse({ ...model, provenance: { issues: [{ message: issue.message }] } }).success, false);
});
