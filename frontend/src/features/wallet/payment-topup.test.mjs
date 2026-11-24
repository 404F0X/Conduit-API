import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';
import ts from 'typescript';

const require = createRequire(import.meta.url);
function compile(file, load = require, storage = globalThis.sessionStorage) {
  const js = ts.transpileModule(readFileSync(new URL(file, import.meta.url), 'utf8'), {
    fileName: file,
    compilerOptions: { module: ts.ModuleKind.CommonJS, jsx: ts.JsxEmit.ReactJSX, target: ts.ScriptTarget.ES2022 },
  }).outputText;
  const module = { exports: {} };
  new Function('require', 'module', 'exports', 'sessionStorage', js)(load, module, module.exports, storage);
  return module.exports;
}
const money = compile('./payment-money.ts');
const receipt = (extra = {}) => ({
  id: '00000000-0000-4000-8000-000000000001',
  projectID: '5',
  status: 'pending',
  channel: 'ALIPAY',
  currency: 'CNY',
  amountMinor: '123',
  creditsMicros: '9007199254740993',
  rechargeMultiplierPPM: '1500000',
  accountingVersion: '9007199254740993',
  createdAt: '2026-11-03T00:00:00Z',
  expiresAt: '2099-11-04T00:00:00Z',
  paidAt: null,
  checkoutURL: 'https://cashier.example.invalid/submit.php?fixture=1',
  reviewReason: null,
  ...extra,
});
function nodes(tree) {
  const all = [];
  const visit = (node) => {
    if (Array.isArray(node)) return node.forEach(visit);
    if (!node || typeof node !== 'object') return;
    all.push(node);
    visit(node.props?.children);
  };
  visit(tree);
  return all;
}
function panel({ enabled = true, fail = false, status = 'pending', createReply, recheckReply } = {}) {
  let cursor = 0;
  let states = [],
    effects = [];
  let payer = 'payer',
    componentPayer = payer;
  let project = 'gid://conduit/Project/5',
    componentProject = project;
  const stored = new Map();
  const storage = {
    getItem: (key) => stored.get(key) ?? null,
    setItem: (key, value) => stored.set(key, value),
    removeItem: (key) => stored.delete(key),
  };
  const authStore = Object.assign((select) => select(authStore.getState()), {
    getState: () => ({ auth: { user: payer ? { id: payer } : null } }),
  });
  const projectStore = { getState: () => ({ selectedProjectId: project }) };
  const calls = [];
  const checks = [];
  const hooks = {
    useEffect: (setup) => {
      const index = cursor++;
      if (!(index in effects)) effects[index] = setup();
    },
    useState: (initial) => {
      const index = cursor++;
      if (!(index in states)) states[index] = typeof initial === 'function' ? initial() : initial;
      return [
        states[index],
        (value) => {
          states[index] = value;
        },
      ];
    },
    useRef: (value) => {
      const [ref] = hooks.useState({ current: value });
      return ref;
    },
  };
  const queries = {
    usePaymentCapabilities: () => ({
      data: { enabled, currency: 'CNY', channels: ['ALIPAY', 'WECHAT'], minAmountMinor: '1', maxAmountMinor: '10000000' },
    }),
    usePaymentOrders: () => ({ data: [], refetch: async () => {} }),
    usePaymentOrder: () => ({ refetch: async () => {} }),
    useCreatePaymentOrder: () => ({
      mutateAsync: async (input) => {
        calls.push({ ...input });
        if (createReply) return createReply(input);
        if (fail) throw new Error('network outcome unknown');
        return receipt({ status });
      },
    }),
    useRecheckPaymentOrder: () => ({
      mutateAsync: async (id) => {
        checks.push(id);
        if (recheckReply) return recheckReply(id);
        return receipt({ status });
      },
    }),
  };
  queries.StalePaymentContextError = class StalePaymentContextError extends Error {};
  const ui = new Proxy({}, { get: (_, key) => key });
  const { PaymentTopupPanel } = compile(
    './components/payment-topup-panel.tsx',
    (id) => {
      if (id === 'react') return hooks;
      if (id === 'react/jsx-runtime') return { jsx: (type, props) => ({ type, props }), jsxs: (type, props) => ({ type, props }) };
      if (id === 'react-i18next') return { useTranslation: () => ({ t: (key) => key, i18n: { language: 'en' } }) };
      if (id.includes('authStore')) return { useAuthStore: authStore };
      if (id.includes('projectStore')) return { useProjectStore: projectStore };
      if (id === '../payment-data') return queries;
      if (id === '../payment-money') return money;
      return ui;
    },
    storage
  );
  return {
    calls,
    checks,
    stored,
    changeContext: (nextPayer, nextProject = project, remount = true) => {
      payer = nextPayer;
      project = nextProject;
      if (remount) {
        effects.forEach((cleanup) => cleanup?.());
        effects = [];
        states = [];
        componentPayer = payer;
        componentProject = project;
      }
    },
    render: () => {
      cursor = 0;
      return PaymentTopupPanel({ payerID: componentPayer, projectID: componentProject, creditDisplayName: 'Credits' });
    },
  };
}
test('disabled capability cannot create or expose a cashier', async () => {
  const app = panel({ enabled: false });
  const tree = app.render();
  const form = nodes(tree).find((node) => node.type === 'form');
  await form.props.onSubmit({ preventDefault() {} });
  assert.equal(app.calls.length, 0);
  assert.equal(nodes(tree).filter((node) => node.type === 'a').length, 0);
});
test('unknown creation retries retain one identity and do not offer a cashier', async () => {
  const app = panel({ fail: true });
  let tree = app.render();
  nodes(tree)
    .find((node) => node.type === 'Input')
    .props.onChange({ target: { value: '1.23' } });
  tree = app.render();
  await nodes(tree)
    .find((node) => node.type === 'form')
    .props.onSubmit({ preventDefault() {} });
  tree = app.render();
  assert.equal(nodes(tree).find((node) => node.type === 'Input').props.disabled, true);
  await nodes(tree)
    .find((node) => node.type === 'form')
    .props.onSubmit({ preventDefault() {} });
  assert.equal(app.calls.length, 2);
  assert.deepEqual(app.calls[0], app.calls[1]);
  assert.equal(app.calls[0].amountMinor, '123');
  assert.equal(nodes(app.render()).filter((node) => node.type === 'a').length, 0);
});
test('created order displays frozen server values before an explicit cashier link; recheck sends only its ID', async () => {
  const app = panel();
  let tree = app.render();
  nodes(tree)
    .find((node) => node.type === 'Input')
    .props.onChange({ target: { value: '1.23' } });
  tree = app.render();
  await nodes(tree)
    .find((node) => node.type === 'form')
    .props.onSubmit({ preventDefault() {} });
  tree = app.render();
  const serialized = JSON.stringify(tree);
  assert.ok(serialized.includes('9007199254.740993'));
  assert.ok(serialized.includes('CNY 1.23'));
  assert.ok(serialized.includes('wallet.payment.status.pending'));
  assert.ok(serialized.includes('wallet.payment.returnHint'));
  const link = nodes(tree).find((node) => node.type === 'a');
  assert.equal(link.props.target, '_blank');
  assert.equal(link.props.rel, 'noopener noreferrer');
  assert.equal(link.props.href, receipt().checkoutURL);
  await nodes(tree)
    .find((node) => node.props?.children === 'wallet.payment.recheck')
    .props.onClick();
  assert.deepEqual(app.checks, [receipt().id]);
  assert.equal(JSON.stringify(app.render()).includes('wallet.payment.status.credited'), false);
});
test('closed or credited orders never reopen a cashier', async () => {
  for (const status of ['closed', 'credited', 'review_required']) {
    const app = panel({ status });
    let tree = app.render();
    nodes(tree)
      .find((node) => node.type === 'Input')
      .props.onChange({ target: { value: '1.23' } });
    tree = app.render();
    await nodes(tree)
      .find((node) => node.type === 'form')
      .props.onSubmit({ preventDefault() {} });
    assert.equal(nodes(app.render()).filter((node) => node.type === 'a').length, 0, status);
  }
});
test('wire schemas retain exact strings and payment calls bind only the selected Project header', async () => {
  const requests = [];
  const invalidations = [];
  const data = compile('./payment-data.ts', (id) => {
    if (id === 'zod') return require('zod');
    if (id === '@tanstack/react-query')
      return {
        useQuery: (options) => options,
        useMutation: (options) => options,
        useQueryClient: () => ({ invalidateQueries: async (key) => invalidations.push(key) }),
      };
    if (id.includes('projectStore'))
      return {
        useSelectedProjectId: () => 'gid://conduit/Project/5',
        useProjectStore: { getState: () => ({ selectedProjectId: 'gid://conduit/Project/5' }) },
      };
    if (id.includes('authStore'))
      return {
        useAuthStore: Object.assign((select) => select({ auth: { user: { id: 'payer' } } }), {
          getState: () => ({ auth: { user: { id: 'payer' } } }),
        }),
      };
    if (id.includes('gql/graphql'))
      return {
        graphqlRequest: async (query, variables, headers) => {
          requests.push({ query, variables, headers });
          return { paymentOrders: [receipt()], createPaymentOrder: receipt(), recheckPaymentOrder: receipt(), paymentOrder: receipt() };
        },
      };
    throw new Error(id);
  });
  assert.equal(data.paymentOrderSchema.parse(receipt()).creditsMicros, '9007199254740993');
  assert.throws(() => data.paymentOrderSchema.parse(receipt({ amountMinor: 123 })));
  await data.usePaymentOrders(10).queryFn();
  assert.deepEqual(requests[0].variables, { limit: 10, offset: 10 });
  assert.deepEqual(requests[0].headers, { 'X-Project-ID': 'gid://conduit/Project/5' });
  const input = { amountMinor: '123', channel: 'ALIPAY', idempotencyKey: 'fixture-idempotency' };
  await data.useCreatePaymentOrder().mutationFn(input);
  assert.deepEqual(requests[1].variables, { input });
  await data.useRecheckPaymentOrder().mutationFn(receipt().id);
  assert.deepEqual(requests[2].variables, { id: receipt().id });
  assert.ok(requests.every((request) => !JSON.stringify(request.variables).includes('payerUserID')));
});

function enterAmount(app, amount = '1.23') {
  nodes(app.render())
    .find((node) => node.type === 'Input')
    .props.onChange({ target: { value: amount } });
}
function submit(app) {
  return nodes(app.render())
    .find((node) => node.type === 'form')
    .props.onSubmit({ preventDefault() {} });
}

test('same-Project payer change removes the old receipt before remount; next payer can create normally', async () => {
  const app = panel();
  enterAmount(app);
  await submit(app);
  assert.ok(nodes(app.render()).some((node) => node.type === 'a'));
  app.changeContext('next-payer', undefined, false);
  assert.equal(app.render(), null);
  app.changeContext('next-payer');
  assert.equal(nodes(app.render()).filter((node) => node.type === 'a').length, 0);
  assert.equal(nodes(app.render()).find((node) => node.type === 'Input').props.value, '');
  enterAmount(app, '2.00');
  await submit(app);
  assert.equal(app.calls[1].amountMinor, '200');
  assert.notEqual(app.calls[0].idempotencyKey, app.calls[1].idempotencyKey);
});

test('late create cannot attach old cashier or erase recoverable payer intent after identity switches', async () => {
  let complete;
  const response = new Promise((resolve) => {
    complete = resolve;
  });
  const app = panel({ createReply: () => response });
  enterAmount(app);
  const pending = submit(app);
  const firstIntent = { ...app.calls[0] };
  app.changeContext('next-payer');
  assert.equal(nodes(app.render()).find((node) => node.type === 'Input').props.value, '');
  complete(receipt());
  await pending;
  assert.equal(nodes(app.render()).filter((node) => node.type === 'a').length, 0);
  assert.equal(JSON.stringify(app.render()).includes('wallet.payment.createFailed'), false);
  app.changeContext('payer');
  assert.equal(nodes(app.render()).find((node) => node.type === 'Input').props.disabled, true);
  await submit(app);
  assert.deepEqual(app.calls[1], firstIntent);
  assert.ok(nodes(app.render()).some((node) => node.type === 'a'));
});

test('late recheck cannot restore old receipt after Project change', async () => {
  let complete;
  const response = new Promise((resolve) => {
    complete = resolve;
  });
  const app = panel({ recheckReply: () => response });
  enterAmount(app);
  await submit(app);
  const pending = nodes(app.render())
    .find((node) => node.props?.children === 'wallet.payment.recheck')
    .props.onClick();
  app.changeContext('payer', 'gid://conduit/Project/6');
  complete(receipt({ status: 'credited' }));
  await pending;
  assert.equal(nodes(app.render()).filter((node) => node.type === 'a').length, 0);
  assert.equal(JSON.stringify(app.render()).includes('wallet.payment.status.credited'), false);
});

test('payment queries and mutations reject stale context before dispatch and after actual deferred completion', async () => {
  let payer = 'payer',
    project = 'gid://conduit/Project/5',
    complete;
  let sends = 0;
  const response = new Promise((resolve) => {
    complete = resolve;
  });
  const authStore = Object.assign((select) => select(authStore.getState()), { getState: () => ({ auth: { user: { id: payer } } }) });
  const data = compile('./payment-data.ts', (id) => {
    if (id === 'zod') return require('zod');
    if (id === '@tanstack/react-query')
      return {
        useQuery: (options) => options,
        useMutation: (options) => options,
        useQueryClient: () => ({ invalidateQueries: async () => {} }),
      };
    if (id.includes('authStore')) return { useAuthStore: authStore };
    if (id.includes('projectStore'))
      return { useSelectedProjectId: () => project, useProjectStore: { getState: () => ({ selectedProjectId: project }) } };
    if (id.includes('gql/graphql'))
      return {
        graphqlRequest: async () => {
          sends++;
          return response;
        },
      };
    throw new Error(id);
  });
  const oldQuery = data.usePaymentOrder(receipt().id);
  const oldMutation = data.useCreatePaymentOrder();
  assert.deepEqual(oldQuery.queryKey.slice(0, 3), ['payments', 'payer', 'gid://conduit/Project/5']);
  const pending = oldQuery.queryFn();
  payer = 'next-payer';
  await assert.rejects(
    oldMutation.mutationFn({ amountMinor: '123', channel: 'ALIPAY', idempotencyKey: 'old' }),
    data.StalePaymentContextError
  );
  assert.equal(sends, 1);
  complete({ paymentOrder: receipt(), createPaymentOrder: receipt() });
  await assert.rejects(pending, data.StalePaymentContextError);
  const newQuery = data.usePaymentOrder(receipt().id);
  assert.deepEqual(newQuery.queryKey.slice(0, 3), ['payments', 'next-payer', 'gid://conduit/Project/5']);
  assert.equal((await newQuery.queryFn()).id, receipt().id);
  project = 'gid://conduit/Project/6';
  await assert.rejects(newQuery.queryFn(), data.StalePaymentContextError);
  assert.equal(sends, 2);
});

test('an unmounted generation stays stale when the same payer returns before the old create resolves', async () => {
  let complete,
    sends = 0;
  const oldResponse = new Promise((resolve) => {
    complete = resolve;
  });
  const currentID = '00000000-0000-4000-8000-000000000002';
  const app = panel({ createReply: () => (++sends === 1 ? oldResponse : receipt({ id: currentID })) });
  enterAmount(app);
  const oldPending = submit(app);
  app.changeContext('next-payer');
  app.render();
  app.changeContext('payer');
  await submit(app);
  assert.ok(JSON.stringify(app.render()).includes(currentID));
  complete(receipt({ amountMinor: '999' }));
  await oldPending;
  const current = JSON.stringify(app.render());
  assert.ok(current.includes(currentID));
  assert.equal(current.includes('CNY 9.99'), false);
});
