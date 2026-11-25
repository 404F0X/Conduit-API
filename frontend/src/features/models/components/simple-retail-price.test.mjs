import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';
import ts from 'typescript';

const require = createRequire(import.meta.url);
const compile = (file, load) => {
  const output = ts.transpileModule(readFileSync(new URL(file, import.meta.url), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function('require', 'module', 'exports', output)(load, module, module.exports);
  return module.exports;
};
const helper = compile('./simple-retail-price.ts', require);
const token = (code = 'prompt_tokens', amount = '0') => ({ itemCode: code, pricing: { mode: 'usage_per_unit', usagePerUnit: amount } });
const flat = { itemCode: 'request', pricing: { mode: 'flat_fee', flatFee: '0' } };
const protectedPrices = {
  extended: { items: [token(), token('image')] },
  variants: {
    items: [{ ...token('prompt_write_cached_tokens'), promptWriteCacheVariants: [{ variantCode: 'one_hour', pricing: token().pricing }] }],
  },
  mixed: { items: [flat, token()] },
  tier: { items: [{ itemCode: 'prompt_tokens', pricing: { mode: 'usage_tiered', usageTiered: { tiers: [{ pricePerUnit: '2' }] } } }] },
  volume: { items: [{ itemCode: 'prompt_tokens', pricing: { mode: 'usage_volume' } }] },
  duplicates: { items: [token(), token()] },
  units: { items: [{ ...token(), unit: 1000 }] },
  unknown: { items: [token()], extra: true },
  malformed: { items: [{ itemCode: 'prompt_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: 0 } }] },
  missing: {},
  null: null,
};
for (const [name, price] of Object.entries(protectedPrices)) {
  test(`rejects ${name} without mutating its structure`, () => {
    const original = JSON.stringify(price);
    assert.equal(helper.readSimpleRetailPrice(price), null);
    assert.equal(JSON.stringify(price), original);
  });
}
test('simple shapes retain zero, decimals, subsets and optional empty variants', () => {
  assert.equal(helper.readSimpleRetailPrice({ items: [flat] }).flatFee, '0');
  assert.equal(
    helper.readSimpleRetailPrice({ items: [{ ...token(), promptWriteCacheVariants: [] }, token('completion_tokens', '2.500000000001')] })
      .completion,
    '2.500000000001'
  );
  assert.equal(helper.readSimpleRetailPrice({ items: [] }).prompt, '');
  assert.equal(
    helper.readSimpleRetailPrice({ items: [{ ...token(), pricing: { ...token().pricing, flatFee: null, usageTiered: null } }] }).prompt,
    '0'
  );
});

async function panel(price, options = {}) {
  const notices = [];
  const modelID = options.modelID ?? 'model';
  const baseModelID = options.baseModelID ?? modelID;
  let states = [],
    cursor = 0,
    effects = [],
    saves = [],
    created = 0;
  const detail = {
    baseRevision: 'published-retail-reference-17',
    currentBaseRevision: Object.hasOwn(options, 'currentBaseRevision') ? options.currentBaseRevision : 'edit-base-digest-D1',
    summary: { id: 'draft', status: 'DRAFT', revision: 'package-revision-3', itemCount: 0 },
  };
  const creates = [];
  const catalog = { data: { priceBooks: [{ id: 'book', isDefault: true }], modelRoutes: [] }, refetch: async () => catalog };
  const hooks = {
    useState: (initial) => {
      const i = cursor++;
      if (!(i in states)) states[i] = typeof initial === 'function' ? initial() : initial;
      return [
        states[i],
        (v) => {
          states[i] = v;
        },
      ];
    },
    useRef: (v) => {
      const [value] = hooks.useState({ current: v });
      return value;
    },
    useCallback: (fn) => fn,
    useEffect: (fn) => effects.push(fn),
  };
  const mutations = new Proxy(
    {},
    {
      get: (_, name) => {
        if (name === 'getRetailBase')
          return async () => ({ revision: 'edit-base-digest-D1', prices: [{ publicModelID: baseModelID, price }] });
        if (name === 'getPackageByOperation') return async () => (options.draftPrice ? detail : null);
        if (name === 'getPackage') return async () => detail;
        if (name === 'getItemPage')
          return async () => ({
            edges: options.draftPrice ? [{ node: { id: 'item', itemKey: modelID.split('/').at(-1) } }] : [],
            pageInfo: { hasNextPage: false },
          });
        if (name === 'getItem') return async () => ({ item: { afterSnapshot: options.draftPrice } });
        return () => ({
          mutateAsync: async (input) => {
            if (name === 'useSaveRetailPriceChangeSetItem') {
              saves.push(input);
              return { item: { validationError: options.validationError ?? null } };
            }
            if (name === 'useCreateRetailPriceChangeSet') {
              created++;
              creates.push(input);
            }
            return detail;
          },
        });
      },
    }
  );
  const { CommercializationPanel } = compile('./commercialization-panel.tsx', (id) => {
    if (id === 'react') return hooks;
    if (id === 'react/jsx-runtime') return require(id);
    if (id === './simple-retail-price') return helper;
    if (id === '@/lib/utils') return compile('../../../lib/utils.ts', require);
    if (id === './group-retail-price-editor')
      return {
        GroupRetailPriceEditor: 'GroupRetailPriceEditor',
        validateEditorDocument: (value) => (helper.readRetailPriceDocument(value) ? null : 'invalid'),
      };
    if (id.includes('change-sets/data')) return mutations;
    if (id.includes('data/commercialization'))
      return {
        useUpsertModelRoute: mutations.useUpsertModelRoute,
        useCreatePriceBook: mutations.useCreatePriceBook,
        useCommercializationCatalog: () => catalog,
        usePricingModelGroups: () => ({ data: [] }),
      };
    if (id === 'react-i18next') return { useTranslation: () => ({ t: (key) => key }) };
    if (id.includes('usePermissions')) return { usePermissions: () => ({ hasSystemScope: () => true }) };
    if (id.includes('system/data')) return { useGeneralSettings: () => ({}) };
    if (id === '@tanstack/react-router') return { useNavigate: () => () => {} };
    if (id === 'sonner')
      return { toast: { success: (message) => notices.push(['success', message]), error: (message) => notices.push(['error', message]) } };
    return new Proxy({}, { get: (_, name) => name });
  });
  const render = (action) => {
    cursor = 0;
    effects = [];
    return CommercializationPanel({ models: [{ id: modelID, modelID: 'model' }], action, onActionHandled() {} });
  };
  render({ kind: 'price', publicModelID: options.unlocked ? '' : modelID });
  effects.forEach((fn) => fn());
  await new Promise((resolve) => setImmediate(resolve));
  const tree = render(null);
  const nodes = [];
  const walk = (value) => {
    if (!value || typeof value !== 'object') return;
    if (Array.isArray(value)) return value.forEach(walk);
    nodes.push(value);
    walk(value.props?.children);
  };
  walk(tree);
  return {
    nodes,
    saves,
    created,
    creates,
    notices,
    rerender() {
      nodes.length = 0;
      walk(render(null));
      return nodes;
    },
  };
}
for (const name of ['extended', 'mixed', 'tier']) {
  test(`panel saves ${name} through draft without partial simplification`, async () => {
    const { nodes, saves, created } = await panel(protectedPrices[name]);
    const save = nodes.find((n) => n.props?.children === '保存变更');
    assert.equal(Boolean(save.props.disabled), false);
    await save.props.onClick();
    assert.deepEqual(saves[0].price, protectedPrices[name]);
    assert.equal(created, 1);
  });
}
for (const [name, price] of [
  ['token', { items: [token()] }],
  ['flat', { items: [flat] }],
]) {
  test(`panel keeps simple ${name} zero prices editable and saves full simple shape`, async () => {
    const { nodes, saves, created } = await panel(price);
    const save = nodes.find((n) => n.props?.children === '保存变更');
    assert.equal(Boolean(save.props.disabled), false);
    save.props.onClick();
    await new Promise((resolve) => setImmediate(resolve));
    assert.equal(created, 1);
    assert.deepEqual(saves[0].price, price);
  });
}

const groupID = '00000000-0000-4000-8000-000000000001';
test('group document editing preserves ordered zero rules, AST, complex leaves and global fallback', () => {
  const ast = { op: 'compare', fact: 'minute', cmp: 'ge', value: 30 };
  const source = {
    schema: 'group-conditional-price-v1',
    global: protectedPrices.mixed,
    groups: [
      {
        modelGroupID: groupID,
        base: protectedPrices.tier,
        rules: [
          { id: 'one', label: 'First', condition: ast, price: { items: [token()] } },
          { id: 'two', label: 'Second', condition: 'total_tokens >= 1000 && month == 11', price: protectedPrices.variants },
        ],
      },
    ],
  };
  const snapshot = JSON.stringify(source);
  const document = helper.readRetailPriceDocument(source);
  const edited = {
    ...document,
    groups: document.groups.map((group) => ({
      ...group,
      base: helper.writeSimpleRetailPrice({ mode: 'request', flatFee: '0', prompt: '', completion: '', cacheRead: '', cacheWrite: '' }),
    })),
  };
  assert.deepEqual(helper.serializeRetailPriceDocument(edited).groups[0].rules, source.groups[0].rules);
  assert.deepEqual(edited.global, source.global);
  assert.equal(JSON.stringify(source), snapshot);
  assert.deepEqual(helper.serializeRetailPriceDocument(helper.readRetailPriceDocument({ items: [token()] })), { items: [token()] });
});
test('unknown document versions and duplicate group/rule IDs cannot be visually simplified', () => {
  assert.equal(helper.readRetailPriceDocument({ schema: 'future', global: null, groups: [] }), null);
  const group = { modelGroupID: groupID, base: { items: [flat] }, rules: [] };
  assert.equal(helper.readRetailPriceDocument({ schema: 'group-conditional-price-v1', global: null, groups: [group, group] }), null);
});

test('unsafe JSON integers block save before any leaf or group can be overwritten', () => {
  const price = {
    items: [
      {
        itemCode: 'prompt_tokens',
        pricing: { mode: 'usage_tiered', usageTiered: { tiers: [{ upTo: 9007199254740992, pricePerUnit: '2' }, { pricePerUnit: '3' }] } },
      },
    ],
  };
  assert.equal(helper.validateEditorDocument(price), 'models.groupPrice.unsafeInteger');
  assert.equal(
    helper.validateEditorDocument({ schema: 'group-conditional-price-v1', global: price, groups: [] }),
    'models.groupPrice.unsafeInteger'
  );
});

function editorModule() {
  let active;
  const jsx = (type, props) => ({ type, props });
  const ui = new Proxy({}, { get: (_, name) => name });
  const component = compile('./group-retail-price-editor.tsx', (id) => {
    if (id === 'react')
      return {
        useState: (initial) => {
          const index = active.cursor++;
          if (!(index in active.states)) active.states[index] = typeof initial === 'function' ? initial() : initial;
          return [
            active.states[index],
            (value) => {
              active.states[index] = value;
            },
          ];
        },
      };
    if (id === 'react/jsx-runtime') return { jsx, jsxs: jsx };
    if (id === './simple-retail-price') return helper;
    if (id === '@/lib/utils') return compile('../../../lib/utils.ts', require);
    if (id === 'react-i18next') return { useTranslation: () => ({ t: (key) => key }) };
    return ui;
  });
  const mount = (type, props) => {
    const instance = { cursor: 0, states: [] };
    return {
      render: (next = props) => {
        active = instance;
        instance.cursor = 0;
        return type(next);
      },
    };
  };
  const nodes = (tree) => {
    const result = [];
    const visit = (node) => {
      if (Array.isArray(node)) return node.forEach(visit);
      if (!node || typeof node !== 'object') return;
      result.push(node);
      visit(node.props?.children);
    };
    visit(tree);
    return result;
  };
  return { component, mount, nodes };
}
test('editor raw/visual switch retains complex leaves; invalid raw blocks parent save', () => {
  const { component, mount, nodes } = editorModule();
  let value = {
    schema: 'group-conditional-price-v1',
    global: protectedPrices.tier,
    groups: [{ modelGroupID: groupID, base: { items: [flat] }, rules: [] }],
  };
  const props = () => ({
    value,
    onChange: (next) => {
      value = next;
    },
    currency: 'CNY',
    groups: [{ id: groupID, name: 'Group', status: 'ENABLED' }],
  });
  const root = mount(component.GroupRetailPriceEditor, props());
  let tree = root.render(props());
  const leaf = nodes(tree).find((node) => typeof node.type === 'function' && node.type.name === 'LeafEditor');
  const child = mount(leaf.type, leaf.props);
  const staticEditor = nodes(child.render()).find((node) => typeof node.type === 'function' && node.type.name === 'StaticLeafEditor');
  const childNodes = nodes(mount(staticEditor.type, staticEditor.props).render());
  assert.equal(childNodes.find((node) => node.props?.children === 'models.groupPrice.visual').props.disabled, true);
  const raw = childNodes.find((node) => node.type === 'Textarea');
  raw.props.onChange({ target: { value: '{invalid' } });
  assert.equal(helper.validateEditorDocument(value), 'models.groupPrice.invalidLeaf');
  assert.equal(value.groups[0].base.items[0].pricing.flatFee, '0');
  // Whole-document raw switches do not rebuild or discard complex leaves.
  value = {
    schema: 'group-conditional-price-v1',
    global: protectedPrices.tier,
    groups: [{ modelGroupID: groupID, base: { items: [flat] }, rules: [] }],
  };
  tree = root.render(props());
  nodes(tree)
    .find((node) => node.props?.children === 'models.groupPrice.rawDocument')
    .props.onClick();
  tree = root.render(props());
  const fullRaw = nodes(tree).find((node) => node.type === 'Textarea');
  assert.deepEqual(JSON.parse(fullRaw.props.value), value);
  nodes(tree)
    .find((node) => node.props?.children === 'models.groupPrice.visual')
    .props.onClick();
  assert.deepEqual(value.global, protectedPrices.tier);
});
test('group selection assigns once and rule reorder keeps each leaf and condition together', () => {
  const { component, mount, nodes } = editorModule();
  let value = {
    schema: 'group-conditional-price-v1',
    global: { items: [flat] },
    groups: [
      {
        modelGroupID: groupID,
        base: null,
        rules: [
          { id: 'one', label: 'One', condition: 'minute >= 30', price: protectedPrices.tier },
          { id: 'two', label: 'Two', condition: { op: 'compare', fact: 'date', cmp: 'eq', value: 20261103 }, price: { items: [flat] } },
        ],
      },
    ],
  };
  const props = () => ({
    value,
    onChange: (next) => {
      value = next;
    },
    currency: 'CNY',
    groups: [{ id: groupID, name: 'Group', status: 'ENABLED' }],
  });
  const root = mount(component.GroupRetailPriceEditor, props());
  const tree = root.render(props());
  assert.equal(nodes(tree).filter((node) => node.type === 'option' && node.props.value === groupID).length, 0);
  const firstDown = nodes(tree).find((node) => node.props?.children === 'models.groupPrice.down');
  firstDown.props.onClick();
  assert.deepEqual(
    value.groups[0].rules.map((rule) => rule.id),
    ['two', 'one']
  );
  assert.deepEqual(value.groups[0].rules[1].price, protectedPrices.tier);
  assert.equal(value.groups[0].rules[1].condition, 'minute >= 30');
  assert.ok(
    nodes(tree)
      .filter((node) => node.type === 'summary')
      .every((node) => typeof node.props.children === 'string')
  );
});

test('reopening a saved draft loads its complete group document instead of the published fallback', async () => {
  const draftPrice = {
    schema: 'group-conditional-price-v1',
    global: { items: [flat] },
    groups: [
      {
        modelGroupID: groupID,
        base: protectedPrices.tier,
        rules: [{ id: 'saved', label: 'Saved rule', condition: 'hour >= 9 && minute < 30', price: protectedPrices.variants }],
      },
    ],
  };
  const { nodes, saves, created } = await panel({ items: [token('prompt_tokens', '99')] }, { draftPrice });
  const editor = nodes.find((node) => node.type === 'GroupRetailPriceEditor');
  assert.deepEqual(editor.props.value, draftPrice);
  await nodes.find((node) => node.props?.children === '保存变更').props.onClick();
  assert.deepEqual(saves[0].price, draftPrice);
  assert.equal(created, 0);
});
test('canonical backend validation errors remain visible and never report a successful price save', async () => {
  const { nodes, notices } = await panel({ items: [flat] }, { validationError: 'unknown condition fact' });
  await nodes.find((node) => node.props?.children === '保存变更').props.onClick();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(notices, [['error', 'unknown condition fact']]);
});

test('a fresh retail draft opens with distinct raw published reference and edit-base digest', async () => {
  const { nodes, creates, saves, notices } = await panel({ items: [token()] });
  assert.ok(nodes.some((node) => node.type === 'Dialog' && node.props.open === true));
  assert.equal(creates[0].expectedBaseRevision, 'edit-base-digest-D1');
  await nodes.find((node) => node.props?.children === '保存变更').props.onClick();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(saves[0].expectedRevision, 'package-revision-3');
  assert.deepEqual(saves[0].price, { items: [token()] });
  assert.deepEqual(notices, [['success', '零售价变更已保存']]);
});

for (const currentBaseRevision of ['edit-base-digest-D2', null, undefined, '']) {
  test(`retail draft refuses changed or missing current edit-base digest: ${String(currentBaseRevision)}`, async () => {
    const { nodes, creates, saves, notices } = await panel({ items: [token()] }, { currentBaseRevision });
    assert.equal(
      nodes.some((node) => node.type === 'Dialog' && node.props.open === true),
      false
    );
    assert.equal(creates[0].expectedBaseRevision, 'edit-base-digest-D1');
    assert.equal(saves.length, 0);
    assert.deepEqual(notices, [['error', 'eventPackages.conflict']]);
  });
}

const completeGroupPrice = () => ({
  schema: 'group-conditional-price-v1',
  global: { items: [token('prompt_tokens', '9')] },
  groups: [
    {
      modelGroupID: groupID,
      base: protectedPrices.tier,
      rules: [
        { id: 'first', label: 'First rule', condition: 'prompt_tokens >= 100', price: protectedPrices.variants },
        { id: 'second', label: 'Second rule', condition: { op: 'compare', fact: 'hour', cmp: 'ge', value: 9 }, price: { items: [flat] } },
      ],
    },
  ],
});

test('configured Model GID loads full published numeric-ID group price without changing save identity', async () => {
  const price = completeGroupPrice();
  const id = 'gid://conduit/Model/42';
  const { nodes, saves } = await panel(price, { modelID: id, baseModelID: '42' });
  assert.deepEqual(nodes.find((node) => node.type === 'GroupRetailPriceEditor').props.value, price);
  await nodes.find((node) => node.props?.children === '保存变更').props.onClick();
  assert.deepEqual(saves[0].price, price);
  assert.equal(saves[0].publicModelID, id);
  assert.equal(saves[0].expectedRevision, 'package-revision-3');
});

test('unlocked model selection loads its numeric-ID published document and preserves every branch', async () => {
  const price = completeGroupPrice();
  const id = 'gid://conduit/Model/42';
  const view = await panel(price, { modelID: id, baseModelID: '42', unlocked: true });
  const select = view.nodes.find((node) => node.type === 'Select' && JSON.stringify(node.props.children).includes('选择模型'));
  await select.props.onValueChange(id);
  const nodes = view.rerender();
  assert.deepEqual(nodes.find((node) => node.type === 'GroupRetailPriceEditor').props.value, price);
  await nodes.find((node) => node.props?.children === '保存变更').props.onClick();
  assert.deepEqual(view.saves[0].price, price);
  assert.equal(view.saves[0].publicModelID, id);
});

test('GID draft restoration matches numeric itemKey and retains complete saved price over published price', async () => {
  const draftPrice = completeGroupPrice();
  const { nodes, created, saves } = await panel(
    { items: [token('prompt_tokens', '99')] },
    {
      modelID: 'gid://conduit/Model/42',
      baseModelID: '42',
      draftPrice,
    }
  );
  assert.equal(created, 0);
  assert.deepEqual(nodes.find((node) => node.type === 'GroupRetailPriceEditor').props.value, draftPrice);
  await nodes.find((node) => node.props?.children === '保存变更').props.onClick();
  assert.deepEqual(saves[0].price, draftPrice);
});

const expressionLeaf = {
  kind: 'NEW_API_EXPRESSION',
  profile: 'conduit-newapi-token-v1',
  version: 1,
  expression: 'p * 2 + c * 7',
  multiplier: '1.000000000000000000000000001',
};

test('retail proposal preserves expression leaf source and decimal multiplier in global, base and ordered rules', async () => {
  const price = {
    schema: 'group-conditional-price-v1',
    global: expressionLeaf,
    groups: [
      {
        modelGroupID: groupID,
        base: { ...expressionLeaf, expression: 'fixed(2)', multiplier: '1' },
        rules: [
          { id: 'one', label: 'first', condition: 'prompt_tokens >= 1000', price: { ...expressionLeaf, expression: 'p * 6' } },
          { id: 'two', label: 'second', condition: 'prompt_tokens >= 5', price: { items: [flat] } },
        ],
      },
    ],
  };
  const { nodes, saves } = await panel(price);
  const save = nodes.find((node) => node.props?.children === '保存变更');
  assert.equal(Boolean(save.props.disabled), false);
  await save.props.onClick();
  assert.deepEqual(saves[0].price, price);
});

test('editor disallows mixed leaves, nonpositive multipliers and oversized expression source', () => {
  assert.equal(helper.readRetailPriceDocument({ ...expressionLeaf, items: [flat] }), null);
  assert.equal(helper.validateEditorDocument({ ...expressionLeaf, multiplier: '0.000' }), 'models.expressionPrice.invalid');
  assert.equal(helper.validateEditorDocument({ ...expressionLeaf, expression: '表'.repeat(1366) }), 'models.expressionPrice.invalid');
  assert.equal(helper.validateEditorDocument(expressionLeaf), null);
});

test('expression preview sends separate cache fields as integer strings and keeps the returned Decimal', async () => {
  const actualTokens = {
    promptTokens: '9007199254740993',
    completionTokens: '12',
    cacheReadTokens: '4',
    cacheWriteTokens: '2',
    cacheWrite1hTokens: '17',
  };
  const amountUSD = '0.000000000000000000000001';
  let variables;
  let response;
  const jsx = (type, props) => ({ type, props });
  const ui = new Proxy({}, { get: (_, name) => name });
  const { NewApiExpressionEditor } = compile('./new-api-expression-editor.tsx', (id) => {
    if (id === 'react') return { useState: (value) => [typeof value === 'object' ? actualTokens : false, () => {}] };
    if (id === 'react/jsx-runtime') return { jsx, jsxs: jsx };
    if (id === './simple-retail-price') return helper;
    if (id === 'react-i18next') return { useTranslation: () => ({ t: (key) => key }) };
    if (id.includes('usePermissions')) return { usePermissions: () => ({ hasSystemScope: () => true }) };
    if (id === '@tanstack/react-query')
      return {
        useMutation: ({ mutationFn }) => ({
          mutate: async (input) => {
            response = await mutationFn(input);
          },
        }),
      };
    if (id === '@/gql/graphql')
      return {
        graphqlRequest: async (query, input) => {
          assert.match(query, /previewNewApiBillingExpression\(input: \$input\)/);
          variables = input;
          return { previewNewApiBillingExpression: { amountUSD } };
        },
      };
    return ui;
  });
  const editor = NewApiExpressionEditor({ value: expressionLeaf, onChange() {} });
  const preview = editor.props.children.find((child) => typeof child?.type === 'function');
  const tree = preview.type(preview.props);
  const button = tree.props.children.find((child) => child?.props?.children === 'models.expressionPrice.evaluate');
  assert.equal(button.props.disabled, false);
  await button.props.onClick();
  assert.deepEqual(variables, { input: { pricing: expressionLeaf, ...actualTokens, inputIncludesCache: false } });
  assert.equal(response, amountUSD);
});
