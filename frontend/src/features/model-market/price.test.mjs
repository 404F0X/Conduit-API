import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';
import ts from 'typescript';

const messages = JSON.parse(readFileSync(new URL('../../locales/en/model-market.json', import.meta.url), 'utf8'));
const t = (key, values = {}) => {
  const template = messages[key] ?? messages[`${key}_${values.count === 1 ? 'one' : 'other'}`] ?? key;
  return template.replace(/{{(\w+)}}/g, (_, name) => String(values[name]));
};
const jsx = (type, props) => (typeof type === 'function' ? type(props) : { type, props });
const ui = new Proxy({}, { get: (_, name) => (props) => jsx(String(name), props) });
const output = ts.transpileModule(readFileSync(new URL('./index.tsx', import.meta.url), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
}).outputText;
const module = { exports: {} };
new Function('require', 'module', 'exports', output)(
  (name) => {
    if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx };
    if (name === 'react-i18next') return { useTranslation: () => ({ t }) };
    return ui;
  },
  module,
  module.exports
);
const { Price } = module.exports;
const pricing = (mode, amount = null, tiers = []) => ({ mode, amount, tiers });
const item = (itemCode, value, variants = []) => ({ itemCode, pricing: value, promptWriteCacheVariants: variants });
const price = (items, extra = {}) => ({
  price: {
    status: 'PRICED',
    billable: true,
    displayName: 'credits',
    items,
    branches: [],
    source: 'global',
    priceBookVersion: '1',
    inputPerMillion: null,
    outputPerMillion: null,
    cacheReadPerMillion: null,
    cacheWritePerMillion: null,
    ...extra,
  },
});
const render = (model, large = false) => JSON.stringify(Price({ model, large }));

test('zero unit prices stay priced and visible; missing and malformed differ', () => {
  assert.match(render(price([item('prompt_tokens', pricing('usage_per_unit', '0'))], { inputPerMillion: '0' })), /credits 0/);
  assert.match(render(price([], { status: 'UNPRICED' })), /Unpriced/);
  const unavailable = render(price([], { status: 'UNAVAILABLE' }));
  assert.match(unavailable, /Price unavailable/);
  assert.doesNotMatch(unavailable, /Unpriced|credits 0/);
});

test('flat-only and mixed cards announce pricing modes and lead to all conditions', () => {
  const flat = item('request', pricing('flat_fee', '0'));
  assert.match(render(price([flat])), /Fixed request fee/);
  assert.match(render(price([flat])), /1 price component/);
  const mixed = price([flat, item('prompt_tokens', pricing('usage_tiered', null, [{ above: '0', upTo: null, pricePerMillion: '2' }]))]);
  assert.match(render(mixed), /Graduated tiers/);
  assert.match(render(mixed), /2 price components/);
  assert.doesNotMatch(render(mixed), /Unpriced/);
});

test('details retain exact bounds, all rates, modes, variants, repeated components and units', () => {
  const model = price([
    item('request', pricing('flat_fee', '0')),
    item(
      'prompt_tokens',
      pricing('usage_tiered', null, [
        { above: '0', upTo: '9007199254740993', pricePerMillion: '1.000000000001' },
        { above: '9007199254740993', upTo: null, pricePerMillion: '2' },
      ])
    ),
    item('completion_tokens', pricing('usage_volume', null, [{ above: '0', upTo: null, pricePerMillion: '3' }])),
    item('prompt_write_cached_tokens', pricing('usage_per_unit', '4'), [
      { variantCode: 'five_min', pricing: pricing('usage_per_unit', '0') },
      { variantCode: 'one_hour', pricing: pricing('flat_fee', '5') },
    ]),
    item('request', pricing('flat_fee', '6')),
  ]);
  const tree = render(model, true);
  for (const text of [
    '9007199254740993',
    '1.000000000001',
    'credits 0',
    '/ request',
    '/ 1M tokens',
    'Graduated tiers',
    'Volume tiers',
    'Each token segment',
    'all tokens',
    'five_min',
    'one_hour',
    'first matching variant',
  ]) {
    // React children retain separate text nodes.
    assert.ok(tree.includes(text) || tree.replaceAll('\",\"', '').includes(text), text);
  }
  assert.equal((tree.match(/Fixed request fee/g) ?? []).length, 3);
});

test('catalog query validates structured prices and uses the selected Project', async () => {
  const require = createRequire(import.meta.url);
  let request;
  const model = {
    id: 'm1',
    modelId: 'public',
    name: 'Public',
    group: '',
    developer: 'test',
    modelType: 'chat',
    capabilities: [],
    contextLimit: null,
    outputLimit: null,
    routes: [],
    health: null,
    ...price([item('request', pricing('flat_fee', '0'))], { currency: 'STATION_CREDIT', effectiveMultiplier: '1.5' }),
  };
  const compiled = ts.transpileModule(readFileSync(new URL('./data.ts', import.meta.url), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const dataModule = { exports: {} };
  new Function('require', 'module', 'exports', compiled)(
    (name) => {
      if (name === 'zod') return require('zod');
      if (name === '@tanstack/react-query') return { useQuery: (options) => options };
      if (name === '@/stores/projectStore') return { useSelectedProjectId: () => 'project-current' };
      if (name === '@/gql/graphql')
        return {
          graphqlRequest: async (query, _variables, headers) => {
            request = { query, headers, variables: _variables };
            return {
              myModelCatalog: {
                healthVisible: false,
                models: [model],
                priceScopes: [{ id: 'group:g1', label: 'Group', modelGroupID: 'g1', modelGroupRevision: 1, apiKeyID: null }],
                selectedPriceScope: 'group:g1',
              },
            };
          },
        };
      throw new Error(name);
    },
    dataModule,
    dataModule.exports
  );
  const query = dataModule.exports.useMyModelCatalog({ modelGroupID: 'g1' });
  const response = await query.queryFn();
  assert.equal(request.headers['X-Project-ID'], 'project-current');
  assert.deepEqual(query.queryKey, ['myModelCatalog', 'project-current', { modelGroupID: 'g1' }]);
  assert.deepEqual(request.variables, { modelGroupID: 'g1' });
  for (const field of [
    'status',
    'items',
    'itemCode',
    'pricing',
    'mode',
    'amount',
    'tiers',
    'above',
    'upTo',
    'pricePerMillion',
    'promptWriteCacheVariants',
    'variantCode',
  ]) {
    assert.ok(request.query.includes(field), field);
  }
  assert.equal(response.models[0].price.items[0].pricing.amount, '0');
  assert.equal(response.models[0].price.effectiveMultiplier, '1.5');
});

test('open model details follow refreshed current-Project catalog prices', () => {
  let current = {
    id: 'm1',
    modelId: 'public',
    name: 'Refreshed model',
    developer: 'test',
    modelType: 'chat',
    capabilities: [],
    contextLimit: null,
    outputLimit: null,
    routes: [],
    health: null,
    ...price([item('request', pricing('flat_fee', '12'))], { effectiveMultiplier: '2' }),
  };
  let stateCalls = 0;
  const featureModule = { exports: {} };
  new Function('require', 'module', 'exports', output)(
    (name) => {
      if (name === 'react/jsx-runtime') return { jsx, jsxs: jsx };
      if (name === 'react')
        return {
          useMemo: (fn) => fn(),
          useState: (initial) => [stateCalls++ === 0 ? 'm1' : initial, () => {}],
        };
      if (name === 'react-i18next') return { useTranslation: () => ({ t, i18n: { language: 'en' } }) };
      if (name === './data') return { useMyModelCatalog: () => ({ data: { models: current ? [current] : [], healthVisible: false } }) };
      return ui;
    },
    featureModule,
    featureModule.exports
  );
  const draw = () => {
    stateCalls = 0;
    return JSON.stringify(featureModule.exports.default());
  };
  assert.match(draw(), /Refreshed model/);
  current = {
    ...current,
    name: 'Next Project model',
    ...price([item('request', pricing('flat_fee', '24'))], { effectiveMultiplier: '4' }),
  };
  const refreshed = draw();
  assert.match(refreshed, /Next Project model/);
  assert.doesNotMatch(refreshed, /Refreshed model/);
  current = null;
  assert.doesNotMatch(draw(), /Next Project model/);
});

test('conditional prices retain every ordered rule and zero without claiming one precise price', () => {
  const model = price([], {
    source: 'conditional',
    branches: [
      { source: 'rule', label: 'Zero first', condition: '"minute >= 30"', items: [item('request', pricing('flat_fee', '0'))] },
      {
        source: 'rule',
        label: 'Token second',
        condition: '{"op":"compare","fact":"total_tokens","cmp":"ge","value":1000}',
        items: [item('request', pricing('flat_fee', '7'))],
      },
      { source: 'global', label: '', condition: null, items: [item('request', pricing('flat_fee', '9'))] },
    ],
  });
  const card = render(model);
  assert.match(card, /Conditional group pricing/);
  assert.doesNotMatch(card, /credits 0|credits 7|credits 9/);
  const detail = render(model, true);
  for (const part of ['Zero first', 'minute', 'Token second', 'total_tokens', 'credits 0', 'credits 7', 'credits 9', 'Global fallback'])
    assert.ok(detail.includes(part) || detail.replaceAll('\",\"', '').includes(part), part);
  assert.ok(detail.indexOf('Zero first') < detail.indexOf('Token second'));
});
