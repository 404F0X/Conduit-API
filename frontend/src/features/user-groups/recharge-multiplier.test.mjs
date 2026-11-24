import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const output = ts.transpileModule(readFileSync(new URL('./recharge-multiplier.ts', import.meta.url), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const module = { exports: {} };
new Function('module', 'exports', output)(module, module.exports);
const helper = module.exports;

test('recharge multiplier default, exact microstep and upper bound use integer PPM', () => {
  for (const [value, ppm] of [
    ['1', '1000000'],
    ['1.5', '1500000'],
    ['0.000001', '1'],
    ['999.999999', '999999999'],
    ['1000', '1000000000'],
  ]) {
    assert.equal(helper.rechargeMultiplierToPPM(value), ppm);
    assert.equal(helper.rechargePPMToMultiplier(ppm), value);
  }
  for (const value of ['0', '-1', '1e3', '1.0000001', '1000.000001', '01', ''])
    assert.equal(helper.rechargeMultiplierToPPM(value), null, value);
  for (const value of ['0', '-1', '1000000001', '1.5', '01']) assert.equal(helper.rechargePPMToMultiplier(value), null, value);
});

test('group edit sends exact PPM and captured revision while ModelGroup and Project panels remain separate', async () => {
  let cursor = 0,
    mutation;
  const states = [],
    requests = [];
  const user = {
    id: 'group',
    name: 'Users',
    description: null,
    status: 'ENABLED',
    memberUserIDs: ['user'],
    modelGroupIDs: ['model-group'],
    rechargeMultiplierPPM: '1500000',
    rechargeRevision: '9007199254740993',
  };
  const model = { id: 'model-group', name: 'Models', description: null, status: 'ENABLED', revision: 1, modelIDs: ['public'] };
  const js = ts.transpileModule(readFileSync(new URL('./index.tsx', import.meta.url), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  const ui = new Proxy({}, { get: (_, key) => key });
  new Function('require', 'module', 'exports', js)(
    (id) => {
      if (id === 'react')
        return {
          useState: (initial) => {
            const i = cursor++;
            if (!(i in states)) states[i] = typeof initial === 'function' ? initial() : initial;
            return [
              states[i],
              (value) => {
                states[i] = value;
              },
            ];
          },
        };
      if (id === 'react/jsx-runtime') return { jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }) };
      if (id === 'react-i18next') return { useTranslation: () => ({ t: (key) => key }) };
      if (id === '@tanstack/react-query')
        return {
          useQuery: () => ({ data: { userGroups: [user], modelGroups: [model] } }),
          useQueryClient: () => ({ invalidateQueries: async () => {} }),
          useMutation: (options) => {
            mutation = options;
            return { mutate: () => options.mutationFn() };
          },
        };
      if (id.includes('gql/graphql'))
        return {
          graphqlRequest: async (query, variables) => {
            requests.push({ query, variables });
          },
        };
      if (id.includes('usePermissions')) return { usePermissions: () => ({ hasSystemScope: () => true }) };
      if (id === './data')
        return {
          useSimpleGroupModelsCatalog: () => ({ data: { models: { edges: [] } } }),
          useSimpleGroupUsersCatalog: () => ({ data: { users: { edges: [] } } }),
        };
      if (id === './recharge-multiplier') return helper;
      if (id === './project-rules-panel') return { default: 'ProjectRulesPanel' };
      return ui;
    },
    module,
    module.exports
  );
  const render = () => {
    cursor = 0;
    return module.exports.default();
  };
  const nodes = (tree) => {
    const all = [];
    const visit = (node) => {
      if (Array.isArray(node)) return node.forEach(visit);
      if (!node || typeof node !== 'object') return;
      all.push(node);
      visit(node.props?.children);
    };
    visit(tree);
    return all;
  };
  let tree = render();
  nodes(tree)
    .find((node) => node.props?.children === '编辑')
    .props.onClick();
  tree = render();
  const label = nodes(tree).find((node) => node.type === 'label' && node.props.children?.[0] === 'users.groups.recharge.label');
  nodes(label)
    .find((node) => node.type === 'Input')
    .props.onChange({ target: { value: '2.000001' } });
  render();
  await mutation.mutationFn();
  assert.equal(requests[0].variables.input.rechargeMultiplierPPM, '2000001');
  assert.equal(requests[0].variables.input.expectedRechargeRevision, '9007199254740993');
  assert.deepEqual(requests[0].variables.input.memberUserIDs, ['user']);
  assert.deepEqual(requests[0].variables.input.modelGroupIDs, ['model-group']);
  tree = render();
  nodes(tree)
    .find((node) => node.props.role === 'tab' && node.props.children === '模型组')
    .props.onClick();
  tree = render();
  nodes(tree)
    .find((node) => node.props?.children === '编辑')
    .props.onClick();
  render();
  await mutation.mutationFn();
  assert.equal('rechargeMultiplierPPM' in requests[1].variables.input, false);
  assert.deepEqual(requests[1].variables.input.modelIDs, ['public']);
  tree = render();
  nodes(tree)
    .find((node) => node.props.role === 'tab' && node.props.children === 'Project 规则')
    .props.onClick();
  tree = render();
  assert.ok(nodes(tree).some((node) => node.type === 'ProjectRulesPanel'));
});
