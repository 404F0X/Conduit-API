import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';
import ts from 'typescript';

const require = createRequire(import.meta.url);
function compile(file, load = require) {
  const js = ts.transpileModule(readFileSync(new URL(file, import.meta.url), 'utf8'), {
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function('require', 'module', 'exports', js)(load, module, module.exports);
  return module.exports;
}
const permissions = compile('../../lib/permission-core.ts');
const routes = compile('../../config/app-routes.ts', () => new Proxy({}, { get: () => 'icon' }));
test('central routes retain groups and governance writers; nested channel paths inherit system authority', () => {
  const channel = routes.findAppRoute('/channels/channel-42');
  assert.equal(channel.path, '/channels');
  assert.equal(channel.scopeLevel, 'system');
  const projectOnly = { isOwner: false, systemScopes: [], projectScopes: ['read_channels'] };
  assert.equal(permissions.satisfiesScopes(projectOnly, channel.requiredScopes, 'any', channel.scopeLevel), false);
  assert.equal(routes.findAppRoute('/groups').requiredScopes.includes('read_groups'), true);
  const governance = routes.findAppRoute('/change-sets');
  assert.equal(
    permissions.satisfiesScopes(
      { isOwner: false, systemScopes: ['write_groups'], projectScopes: [] },
      governance.requiredScopes,
      'any',
      'system'
    ),
    true
  );
});
test('simple home is accessible without requests while enterprise home keeps its request scope', () => {
  const home = routes.findAppRoute('/project/dashboard');
  const empty = { isOwner: false, systemScopes: [], projectScopes: [] };
  assert.equal(permissions.satisfiesScopes(empty, routes.requiredScopesFor(home, 'SIMPLE')), true);
  assert.equal(permissions.satisfiesScopes(empty, routes.requiredScopesFor(home, 'ENTERPRISE')), false);
});
test('simple wallet mounts the existing server-confirmed payment panel bound to payer and project', () => {
  const jsx = (type, props, key) => ({ type, props, key });
  const PaymentTopupPanel = () => null;
  let payer = 'payer-a',
    project = 'project-a';
  const { SimpleWallet } = compile('./wallet/index.tsx', (id) => {
    if (id === 'react/jsx-runtime') return { jsx, jsxs: jsx, Fragment: 'fragment' };
    if (id === 'react') return { useState: (value) => [value, () => {}] };
    if (id === 'react-i18next') return { useTranslation: () => ({ t: (value) => value, i18n: { language: 'en' } }) };
    if (id.includes('authStore')) return { useAuthStore: (select) => select({ auth: { user: { id: payer } } }) };
    if (id.includes('projectStore')) return { useSelectedProjectId: () => project };
    if (id.includes('payment-topup-panel')) return { PaymentTopupPanel };
    if (id.includes('billing/data')) return { useMyProjectBalance: () => ({ data: null }), useMySubscriptions: () => ({ data: null }) };
    if (id.includes('use-money')) return { useMoney: () => ({ label: 'Credits', credits: () => ({ text: '0', exact: '0' }) }) };
    return new Proxy({}, { get: (_, name) => name });
  });
  const panel = () => {
    const found = [];
    const visit = (node) => {
      if (Array.isArray(node)) return node.forEach(visit);
      if (!node || typeof node !== 'object') return;
      if (node.type === PaymentTopupPanel) found.push(node);
      visit(node.props?.children);
    };
    visit(SimpleWallet());
    return found;
  };
  assert.deepEqual(panel()[0].props, { payerID: payer, projectID: project, creditDisplayName: 'Credits' });
  const firstKey = panel()[0].key;
  payer = 'payer-b';
  project = 'project-b';
  assert.notEqual(panel()[0].key, firstKey);
  project = null;
  assert.equal(panel().length, 0);
});
