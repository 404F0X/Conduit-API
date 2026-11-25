import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';
import { createStore } from 'zustand/vanilla';

async function loadSource(path, replacements = []) {
  let source = readFileSync(join(import.meta.dirname, path), 'utf8');
  for (const [from, to] of replacements) source = source.replace(from, to);
  const output = ts.transpileModule(source, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2023 },
  }).outputText;
  return import(`data:text/javascript;base64,${Buffer.from(output).toString('base64')}`);
}
const prices = await loadSource('price-view.ts');
const state = await loadSource('package-state.ts');
const listSearch = await loadSource('change-set-search.ts');
test('terminal or unavailable review actions cannot be retried after a recovered response', () => {
  for (const status of ['APPLIED', 'REJECTED', 'SUPERSEDED']) {
    for (const action of ['approve', 'reject', 'submit']) {
      assert.equal(
        state.packageActionAllowed({ summary: { status }, availableActions: ['APPROVE', 'PUBLISH', 'REJECT', 'SUBMIT'] }, action),
        false
      );
    }
  }
  assert.equal(state.packageActionAllowed({ summary: { status: 'PENDING_REVIEW' }, availableActions: [] }, 'approve'), false);
  assert.equal(state.packageActionAllowed({ summary: { status: 'INVALID' }, availableActions: ['REJECT'] }, 'reject'), true);
  assert.equal(state.packageActionAllowed({ summary: { status: 'PENDING_REVIEW' }, availableActions: ['APPROVE'] }, 'approve'), true);
});
test('empty queues and server-filtered counts do not imply the database is empty', () => {
  assert.equal(state.packageEmptyState([], {}), 'empty');
  assert.equal(state.packageEmptyState([{ count: 4 }], {}), 'filtered');
  for (const filters of [{ search: 'absent' }, { kind: 'RETAIL_PRICE' }, { scopeID: 'other' }, { scopeType: 'channel' }]) {
    assert.equal(state.packageEmptyState([], filters), 'filtered');
  }
});
test('audit display keeps distinct full actor IDs and uses only an explicitly supplied name', () => {
  const event = { actorType: 'user', detail: {} };
  assert.deepEqual(state.eventActor({ ...event, actorID: 'gid://conduit/User/10' }), {
    type: 'user',
    id: 'gid://conduit/User/10',
    name: undefined,
  });
  assert.notEqual(state.eventActor({ ...event, actorID: 'User/10' }).id, state.eventActor({ ...event, actorID: 'User/11' }).id);
  assert.equal(state.eventActor({ ...event, actorID: 'User/12', detail: { actorName: 'Real name' } }).name, 'Real name');
  assert.equal(state.eventActor({ ...event, actorID: null }).id, undefined);
  assert.equal(
    state.eventActor({ ...event, actorID: 'User/10' }, { createdBy: 'User/10', actorName: 'Known creator' }).name,
    'Known creator'
  );
  assert.equal(state.eventActor({ ...event, actorID: 'User/11' }, { createdBy: 'User/10', actorName: 'Known creator' }).name, undefined);
});
test('cache-variant-only changes and volume/tiered prices retain every threshold and explicit zero', () => {
  const original = {
    items: [
      {
        itemCode: 'prompt_write_cached_tokens',
        pricing: { mode: 'usage_per_unit', usagePerUnit: '1' },
        promptWriteCacheVariants: [
          {
            variantCode: 'five_min',
            pricing: {
              mode: 'usage_volume',
              usageTiered: {
                tiers: [
                  { upTo: 1000000, pricePerUnit: '0' },
                  { upTo: null, pricePerUnit: '2' },
                ],
              },
            },
          },
        ],
      },
    ],
  };
  const proposed = structuredClone(original);
  proposed.items[0].promptWriteCacheVariants[0].pricing.usageTiered.tiers[1].pricePerUnit = '3';
  assert.deepEqual(prices.changedSnapshotFields(original, proposed), ['prompt_write_cached_tokens']);
  const rows = prices.priceRows(proposed);
  assert.equal(rows.length, 2);
  assert.equal(rows[1].variantCode, 'five_min');
  assert.equal(rows[1].mode, 'usage_volume');
  assert.deepEqual(rows[1].tiers, [
    { upTo: 1000000, amount: '0' },
    { upTo: null, amount: '3' },
  ]);
  assert.deepEqual(
    prices.snapshotFieldValues(original).prompt_write_cached_tokens.promptWriteCacheVariants,
    original.items[0].promptWriteCacheVariants
  );
});
test('review action and applied state translations agree in both supported languages', () => {
  const english = JSON.parse(readFileSync(join(import.meta.dirname, '../../locales/en/event-packages.json'), 'utf8')).eventPackages;
  const chinese = JSON.parse(readFileSync(join(import.meta.dirname, '../../locales/zh-CN/event-packages.json'), 'utf8')).eventPackages;
  assert.equal(english.APPLIED, 'Applied');
  assert.equal(english.approve, 'Approve');
  assert.equal(english.publish, 'Publish');
  assert.equal(state.packageActionAllowed({ summary: { status: 'APPROVED' }, availableActions: ['PUBLISH'] }, 'publish'), true);
  assert.equal(state.packageActionAllowed({ summary: { status: 'PENDING_REVIEW' }, availableActions: ['PUBLISH'] }, 'publish'), false);
  assert.equal(chinese.approve, '批准');
  assert.equal(chinese.publish, '发布');
});
test('detail return context retains the exact list filter and server cursor without arbitrary redirects', () => {
  const context = listSearch.validatePackageDetailSearch({
    list: 'changelog',
    after: 'opaque-page-4',
    q: 'cache',
    all: true,
    kind: 'PROVIDER_PRICE',
    scopeType: 'channel',
    scopeID: 'channel-7',
  });
  assert.equal(context.list, 'changelog');
  assert.equal(context.after, 'opaque-page-4');
  assert.equal(context.all, true);
  assert.equal(context.q, 'cache');
  assert.equal(context.scopeID, 'channel-7');
  assert.equal(listSearch.validatePackageDetailSearch({ list: 'https://outside.example', status: 'unsafe' }).list, 'workbench');
  assert.equal(listSearch.validatePackageDetailSearch({ status: 'unsafe' }).status, undefined);
});
test('price diff preserves explicit zero, absence and complete tier boundaries', () => {
  const before = {
    price: {
      items: [
        { itemCode: 'prompt_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: '0' } },
        {
          itemCode: 'completion_tokens',
          pricing: {
            mode: 'usage_tiered',
            usageTiered: {
              tiers: [
                { upTo: 100, pricePerUnit: '1' },
                { upTo: null, pricePerUnit: '2' },
              ],
            },
          },
        },
      ],
    },
  };
  const after = structuredClone(before);
  after.price.items[1].pricing.usageTiered.tiers[0].pricePerUnit = '3';
  assert.deepEqual(prices.changedSnapshotFields(before, after), ['completion_tokens']);
  assert.equal(prices.displayAmount(prices.priceRows(before)[0].amount), '0');
  assert.equal(prices.displayAmount(null), null);
  assert.equal(prices.displayAmount(''), null);
  assert.throws(() => prices.updatePriceAmount(before.price, 'completion_tokens', '4'), /tiered/);
  assert.deepEqual(
    prices.changedSnapshotFields(null, { items: [{ itemCode: 'prompt_tokens', pricing: { usagePerUnit: '0', mode: 'usage_per_unit' } }] }),
    ['prompt_tokens']
  );
});
test('reordered snapshot object keys are not a user-visible change', () => {
  assert.deepEqual(
    prices.changedSnapshotFields(
      { items: [{ itemCode: 'prompt_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: '2' } }] },
      { items: [{ itemCode: 'prompt_tokens', pricing: { usagePerUnit: '2', mode: 'usage_per_unit' } }] }
    ),
    []
  );
  assert.equal(prices.isDefinitePackageRejection(new Error('SOURCE_CHANGED: reload')), true);
  assert.equal(prices.isDefinitePackageRejection(new Error('Network error')), false);
  assert.equal(prices.isDefinitePackageRejection(new Error('Failed to parse server response as JSON')), false);
});

const calls = [];
let response = {};
const invalidations = [];
globalThis.__packageTransport = async (query, variables) => {
  calls.push({ query, variables });
  return response;
};
globalThis.__packageQueryClient = {
  invalidateQueries: async (value) => {
    invalidations.push(value.queryKey);
  },
};
class TestPackageRequestError extends Error {}
globalThis.__packageRequestError = TestPackageRequestError;
const packages = await loadSource('data/change-sets.ts', [
  [
    /import \{ useMutation, useQuery, useQueryClient \} from '@tanstack\/react-query';/,
    `const useMutation=options=>options; const useQuery=options=>options; const useQueryClient=()=>globalThis.__packageQueryClient;`,
  ],
  [
    /import \{ GraphQLRequestError, graphqlRequest \} from '@\/gql\/graphql';/,
    `const GraphQLRequestError=globalThis.__packageRequestError; const graphqlRequest=(...args)=>globalThis.__packageTransport(...args);`,
  ],
]);
test('summary and lazy detail requests bind filters, cursors and server revisions', async () => {
  response = {
    changeSets: {
      edges: [],
      totalCount: 143,
      statusCounts: [{ status: 'PENDING_REVIEW', count: 139 }],
      pageInfo: { hasNextPage: true, endCursor: 'opaque' },
    },
  };
  const page = await packages.getPackagePage({ statuses: ['PENDING_REVIEW'] });
  assert.equal(page.totalCount, 143);
  assert.equal(calls.at(-1).variables.first, 20);
  assert.doesNotMatch(calls.at(-1).query, /beforeSnapshot|afterSnapshot|sourceSnapshot/);
  response = { changeSetItems: { edges: [], totalCount: 103, pageInfo: { hasNextPage: true, endCursor: 'item-page' } } };
  await packages.getItemPage('package-1', 'opaque', 'm', true);
  assert.equal(calls.at(-1).variables.id, 'package-1');
  assert.match(calls.at(-1).query, /changeSetItems\(changeSetID: \$id/);
  assert.match(calls.at(-1).query, /first: 50/);
  assert.equal(calls.at(-1).variables.after, 'opaque');
  assert.doesNotMatch(calls.at(-1).query, /beforeSnapshot|afterSnapshot/);
  response = { approveChangeSet: { summary: { id: 'package-1', revision: 'next', status: 'APPLIED' } } };
  await packages.transitionPackage('approve', { id: 'package-1', expectedRevision: 'event-82', reviewNote: 'all items verified' });
  assert.equal(calls.at(-1).variables.expectedRevision, 'event-82');
  assert.match(calls.at(-1).query, /expectedRevision: \$expectedRevision/);
});
test('failed package mutations invalidate summary, detail and related catalog caches', async () => {
  invalidations.length = 0;
  const mutation = packages.useApproveChangeSet();
  await mutation.onSettled(undefined, new Error('SOURCE_CHANGED: formal price changed'));
  assert.ok(invalidations.some((key) => key[0] === 'changeSets'));
  assert.ok(invalidations.some((key) => key[0] === 'changeSet'));
  assert.ok(invalidations.some((key) => key[0] === 'commercialization-catalog'));
  assert.ok(invalidations.some((key) => key[0] === 'generalSettings'));
});

const discovery = await loadSource('../channels/data/discovery.ts', [
  [/import \{ graphqlRequest \} from '@\/gql\/graphql';/, `const graphqlRequest=(...args)=>globalThis.__packageTransport(...args);`],
]);
test('canonical NEWAPI URL preserves mounted prefix and rejects embedded credentials', () => {
  assert.equal(discovery.canonicalNewApiURL('https://newapi.example/station/v1/?query=secret#x'), 'https://newapi.example/station');
  assert.equal(discovery.canonicalNewApiURL('http://127.0.0.1:3000/'), 'http://127.0.0.1:3000');
  assert.throws(() => discovery.canonicalNewApiURL('https://user:secret@newapi.example'), /credentials/);
  assert.throws(() => discovery.canonicalNewApiURL('file:///tmp/test'), /HTTP/);
});
test('retry reuses operation and attempt and excludes completed discovery stages', async () => {
  const operation = {
    operationID: 'stable',
    attempt: 3,
    models: { status: 'SUCCEEDED', retryable: false },
    prices: { status: 'FAILED', retryable: true },
  };
  assert.deepEqual(discovery.retryableStages(operation), ['PRICES']);
  response = { retryNewApiChannelDiscovery: { ...operation, attempt: 4 } };
  const result = await discovery.retryDiscovery({
    operationID: operation.operationID,
    expectedAttempt: operation.attempt,
    stages: discovery.retryableStages(operation),
  });
  assert.equal(result.attempt, 4);
  assert.equal(calls.at(-1).variables.operationID, 'stable');
  assert.equal(calls.at(-1).variables.expectedAttempt, 3);
  assert.deepEqual(calls.at(-1).variables.stages, ['PRICES']);
});

const formulas = await loadSource('../models/data/price-formulas.ts', [
  [
    /import \{ useMutation, useQuery, useQueryClient \} from '@tanstack\/react-query';/,
    `const useMutation=options=>options; const useQuery=options=>options; const useQueryClient=()=>globalThis.__packageQueryClient;`,
  ],
  [/import \{ graphqlRequest \} from '@\/gql\/graphql';/, `const graphqlRequest=(...args)=>globalThis.__packageTransport(...args);`],
]);
test('frontend operation documents and variables match the schema-validated committed contract', async () => {
  const fixture = JSON.parse(readFileSync(join(import.meta.dirname, '../../../../tests/contracts/change-package-operations.json'), 'utf8'));
  response = {};
  for (const entry of fixture) {
    const api = { packages, discovery, formulas }[entry.source];
    await api[entry.method](...entry.args);
    const actual = JSON.parse(JSON.stringify(calls.at(-1)));
    assert.deepEqual(actual, { query: entry.query, variables: entry.variables }, entry.method);
  }
});

const mutationBridge = await loadSource('mutation-adapter.ts');
test('native governed forms retain nested GraphQL IDs/enums and billing commands lower once', () => {
  const model = { modelID: 'sku-a', developer: 'test', name: 'A', group: 'test', modelCard: {}, settings: {} };
  assert.deepEqual(
    mutationBridge.governedMutation('mutation Add($inputs: [CreateModelInput!]!) { bulkCreateModels(inputs: $inputs) { id } }', {
      inputs: [model],
    }),
    { action: 'model.batch_form', targetIDs: [], payload: { models: [model] } }
  );
  const user = { projectId: 'gid://conduit/Project/3', userId: 'gid://conduit/User/4', isOwner: false, roleIDs: ['5'] };
  assert.deepEqual(
    mutationBridge.governedMutation('mutation Add($input: AddUserToProjectInput!) { addUserToProject(input:$input) { id } }', {
      input: user,
    }),
    { action: 'member.add_form', targetIDs: [], payload: user }
  );
  const settings = { timezone: 'UTC' };
  assert.deepEqual(
    mutationBridge.governedMutation(
      'mutation Settings($input: UpdateSystemGeneralSettingsInput!) { updateSystemGeneralSettings(input:$input) { timezone } }',
      { input: settings }
    ).payload,
    settings
  );
  const plan = {
    name: 'test',
    intervalUnit: 'MONTH',
    intervalCount: 1,
    accessPlanIDs: ['gid://conduit/AccessPlan/7'],
    quotaRules: [{ quotaClass: 'DEDICATED', allowance: '10', rolloverMode: 'NONE', accessPlanIDs: ['7'] }],
  };
  const mapped = mutationBridge.governedMutation(
    'mutation Plan($input: CreateSubscriptionPlanInput!) { createSubscriptionPlan(input:$input) { id } }',
    { input: plan }
  );
  assert.equal(mapped.payload.interval_unit, 'Month');
  assert.deepEqual(mapped.payload.access_plan_ids, ['gid://conduit/AccessPlan/7']);
  assert.equal(mapped.payload.quota_rules[0].quota_class, 'Dedicated');
  assert.equal(mapped.payload.quota_rules[0].rollover_mode, 'None');
  assert.equal(
    mutationBridge.governedMutation('mutation Archive { bulkArchiveChannels(ids:$ids) }', { ids: ['1', '2'] }).payload.status,
    'archived'
  );
  assert.equal(
    mutationBridge.governedMutation('mutation Keys { enableAllChannelAPIKeys(channelID:$channelID) }', { channelID: '3' }).payload
      .operation,
    'enable_all'
  );
  assert.equal(mutationBridge.governedMutation('query Read { channels { id } }'), null);
});
test('lost proposal response retries the same operation and original target revision; edited intent uses a new operation', async () => {
  const calls = [];
  let lost = true;
  let revision = 'before';
  globalThis.__governanceTestRequest = async (query, variables) => {
    calls.push({ query, variables });
    if (query.includes('GovernedBase')) return { governedChangeBase: revision };
    if (lost) {
      lost = false;
      revision = 'published';
      throw new Error('lost response');
    }
    return { proposeGovernedChange: { summary: { id: '11' } } };
  };
  const governance = await loadSource('data/governance.ts', [
    ["import { useQuery } from '@tanstack/react-query';", 'const useQuery=()=>{};'],
    ["import { graphqlRequest } from '@/gql/graphql';", 'const graphqlRequest=(...args)=>globalThis.__governanceTestRequest(...args);'],
    ["import { DETAIL_FIELDS, type ChangeSetKind, type PackageDetail } from './change-sets';", "const DETAIL_FIELDS='summary { id }';"],
  ]);
  const command = { action: 'channel.write', targetIDs: ['1'], payload: { name: 'new' } };
  const intent = crypto.randomUUID();
  await assert.rejects(governance.proposeCommand(command, 'why', intent), /lost response/);
  await governance.proposeCommand(command, 'why', intent);
  const proposals = calls.filter((c) => c.query.includes('GovernedProposal'));
  assert.equal(proposals.length, 2);
  assert.equal(proposals[0].variables.input.operationID, proposals[1].variables.input.operationID);
  assert.equal(proposals[1].variables.input.expectedTargetRevision, 'before');
  assert.equal(calls.filter((c) => c.query.includes('GovernedBase')).length, 1);
  await governance.proposeCommand({ ...command, payload: { name: 'edited' } }, 'why', intent);
  const edited = calls.at(-1).variables.input;
  assert.notEqual(edited.operationID, intent);
  assert.equal(edited.expectedTargetRevision, 'published');
  delete globalThis.__governanceTestRequest;
});

test('review detects an exact expression change and protects it from conversion by a static editor', () => {
  const before = {
    kind: 'NEW_API_EXPRESSION',
    profile: 'conduit-newapi-token-v1',
    version: 1,
    expression: 'p * 2',
    multiplier: '1.00000000001',
  };
  const next = { ...before, multiplier: '1.00000000002' };
  assert.deepEqual(prices.changedSnapshotFields({ billingExpression: before, price: null }, next), ['billingExpression']);
  assert.deepEqual(prices.snapshotFieldValues(next), { billingExpression: next });
  assert.throws(() => prices.updatePriceAmount(next, 'prompt_tokens', '1'), /price editor/);
});

test('provider dialog checks the exact receipt after a definite create rejection and preserves uncertainty', async () => {
  const error = new TestPackageRequestError('invalid change set operation: VALIDATION_FAILED: unsupported cc12h');
  error.providerCreateValidationRejected = true;
  const rejected = async () => {
    throw error;
  };
  const intent = { channelID: 'channel', operationID: 'same-id', expectedBaseRevision: 'base', input: [] };
  const existing = { operationID: intent.operationID, outcome: 'CREATED', changeSetDetail: { summary: { id: '1' } } };

  response = { providerPriceChangeSetOperation: null };
  assert.deepEqual(await packages.createProviderDraftFromDialog(intent, rejected), { validationError: error });
  assert.deepEqual(calls.at(-1).variables, { operationID: intent.operationID });

  const corrected = {
    ...intent,
    operationID: 'corrected-id',
    input: [{ modelId: 'model', currencyCode: 'USD', billingExpression: { expression: 'fixed(0.1)' } }],
  };
  const submitted = [];
  const created = await packages.createProviderDraftFromDialog(corrected, async (input) => {
    submitted.push(input);
    return { ...existing, operationID: input.operationID };
  });
  assert.deepEqual(submitted, [corrected]);
  assert.equal(created.result.changeSetDetail.summary.id, '1');
  assert.notEqual(created.result.operationID, intent.operationID);

  response = { providerPriceChangeSetOperation: existing };
  assert.deepEqual(await packages.createProviderDraftFromDialog(intent, rejected), { result: existing, recovered: true });
  assert.deepEqual(calls.at(-1).variables, { operationID: intent.operationID });
  assert.deepEqual(await packages.lookupProviderDraftReceipt(intent.operationID), existing);

  const noChange = { operationID: intent.operationID, outcome: 'NO_CHANGE', changeSetDetail: null };
  response = { providerPriceChangeSetOperation: noChange };
  assert.deepEqual(await packages.lookupProviderDraftReceipt(intent.operationID), noChange);
  assert.deepEqual(await packages.createProviderDraftFromDialog(intent, async () => noChange), { result: noChange });

  const invalidReceipts = [
    {},
    { ...existing, operationID: 'other' },
    { ...existing, outcome: 'unknown' },
    { ...existing, changeSetDetail: null },
    { operationID: intent.operationID, outcome: 'CREATED' },
    { ...existing, changeSetDetail: {} },
    ...['', 'abc', '0', '9223372036854775808'].map((id) => ({ ...existing, changeSetDetail: { summary: { id } } })),
    { operationID: intent.operationID, outcome: 'NO_CHANGE' },
    { ...noChange, changeSetDetail: existing.changeSetDetail },
  ];
  for (const malformed of [undefined, ...invalidReceipts]) {
    response = { providerPriceChangeSetOperation: malformed };
    await assert.rejects(packages.createProviderDraftFromDialog(intent, rejected), /receipt is incomplete/);
    assert.deepEqual(calls.at(-1).variables, { operationID: intent.operationID });
    await assert.rejects(
      packages.createProviderDraftFromDialog(intent, async () => malformed),
      /receipt is incomplete/
    );
  }
  const oldTransport = globalThis.__packageTransport;
  try {
    for (const message of ['validation during lookup', 'Unauthorized', 'Network error']) {
      const lookupError = new TestPackageRequestError(message);
      globalThis.__packageTransport = async () => {
        throw lookupError;
      };
      await assert.rejects(packages.createProviderDraftFromDialog(intent, rejected), (actual) => actual === lookupError);
    }
  } finally {
    globalThis.__packageTransport = oldTransport;
  }
  const internal = new TestPackageRequestError('STORAGE_FAILED: response detail unavailable');
  const before = calls.length;
  await assert.rejects(
    packages.createProviderDraftFromDialog(intent, async () => {
      throw internal;
    }),
    (actual) => actual === internal
  );
  assert.equal(calls.length, before);
  response = { providerPriceChangeSetOperation: existing };
  assert.deepEqual(await packages.lookupProviderDraftReceipt(intent.operationID), existing);
});

const submissionGuard = await loadSource('../channels/components/provider-draft-submission.ts');
test('provider draft owns the complete lookup and ignores completion from a cancelled session', async () => {
  const submission = submissionGuard.createProviderDraftSubmission();
  let resolveLookup;
  const lookup = new Promise((resolve) => {
    resolveLookup = resolve;
  });
  const current = submission.start();
  let writes = 0;
  let releases = 0;
  const completion = (async () => {
    await lookup;
    if (current.isCurrent()) writes++;
    if (current.finish()) releases++;
  })();
  assert.equal(current.isCurrent(), true);
  assert.equal(submission.start(), null, 'double submit before any UI re-render cannot acquire');
  submission.invalidate();
  assert.equal(current.isCurrent(), false);
  assert.equal(submission.start(), null, 'a reopened dialog waits for the one physical request to finish');
  resolveLookup();
  await completion;
  assert.equal(writes, 0);
  assert.equal(releases, 1);
  const reopened = submission.start();
  assert.equal(current.finish(), false, 'stale finally cannot release the new submission');
  assert.equal(reopened.isCurrent(), true);
  assert.equal(reopened.finish(), true);
});

const testAuth = createStore(() => ({ auth: { user: { id: 'actor-1' }, accessToken: 'test-token' } }));
globalThis.__providerDraftSessionTest = { createStore, testAuth, packages };
const sessions = await loadSource('../channels/components/provider-draft-sessions.ts', [
  [/import \{ createStore \} from 'zustand\/vanilla';/, 'const {createStore}=globalThis.__providerDraftSessionTest;'],
  [/import \{ GRAPHQL_ENDPOINT \} from '@\/gql\/graphql';/, `const GRAPHQL_ENDPOINT='/admin/graphql';`],
  [/import \{ useAuthStore \} from '@\/stores\/authStore';/, 'const useAuthStore=globalThis.__providerDraftSessionTest.testAuth;'],
  [
    /import \{\s*createProviderDraftFromDialog,[\s\S]*?from '@\/features\/change-sets\/data\/change-sets';/,
    'const { createProviderDraftFromDialog, createProviderPackage, lookupProviderDraftReceipt }=globalThis.__providerDraftSessionTest.packages;',
  ],
]);
const channelSlot = { actorID: 'actor-1', endpoint: 'https://conduit.invalid/admin/graphql', channelID: 'channel-1' };
const draftInput = { expectedBaseRevision: 'base', input: [{ modelId: 'example', currencyCode: 'USD', price: { items: [] } }] };
const receipt = (operationID) => ({ operationID, outcome: 'CREATED', changeSetDetail: { summary: { id: '42' } } });
const entryFor = (owner, slot = channelSlot) => owner.store.getState().entries[sessions.providerDraftSlotKey(slot)];

test('pending provider price draft survives teardown, 500 ms row removal, and a soft navigation to Models', async () => {
  let resolveCreate;
  let calls = 0;
  const owner = sessions.createProviderDraftSessions(
    {
      create: () => {
        calls++;
        return new Promise((resolve) => {
          resolveCreate = resolve;
        });
      },
      lookup: async () => {
        throw new Error('busy must not look up a receipt');
      },
    },
    () => true,
    () => 'first-op'
  );
  const channelsView = submissionGuard.createProviderDraftSubmission();
  const ticket = channelsView.start();
  const operation = owner.start(channelSlot, draftInput);
  channelsView.invalidate();
  await new Promise((resolve) => setTimeout(resolve, 510));

  const modelsView = submissionGuard.createProviderDraftSubmission();
  const modelsTicket = modelsView.start();
  assert.equal(modelsTicket.isCurrent(), true);
  assert.equal(entryFor(owner).intent.operationID, 'first-op');
  assert.equal(entryFor(owner).status, 'busy');
  assert.equal(owner.start(channelSlot, draftInput), undefined);
  assert.equal(owner.recover(channelSlot, 'retry'), undefined);
  assert.equal(owner.recover(channelSlot, 'abandon'), undefined);
  assert.equal(calls, 1);
  modelsView.invalidate();

  resolveCreate(receipt('first-op'));
  await operation;
  assert.equal(ticket.isCurrent(), false);
  assert.equal(modelsTicket.isCurrent(), false);
  assert.deepEqual(entryFor(owner).result, { outcome: 'CREATED', packageID: '42' });
  assert.equal(owner.start(channelSlot, draftInput), undefined);
  owner.dismiss(channelSlot);
  assert.equal(entryFor(owner), undefined);
});

test('unknown draft uses same receipt and bytes and can be abandoned only after a successful null lookup', async () => {
  const payloads = [];
  const lookups = [];
  let found = null;
  let lookupError = new Error('offline');
  let seq = 0;
  const owner = sessions.createProviderDraftSessions(
    {
      create: async (intent) => {
        payloads.push(JSON.stringify(intent));
        if (payloads.length < 3) throw new Error('response lost');
        return receipt(intent.operationID);
      },
      lookup: async (id) => {
        lookups.push(id);
        if (lookupError) throw lookupError;
        return found;
      },
    },
    () => true,
    () => `op-${++seq}`
  );

  const input = structuredClone(draftInput);
  await assert.rejects(owner.start(channelSlot, input), /response lost/);
  input.input[0].modelId = 'unsent edit';
  assert.equal(entryFor(owner).status, 'unknown');
  await assert.rejects(owner.recover(channelSlot, 'abandon'), /offline/);
  assert.equal(entryFor(owner).intent.operationID, 'op-1');
  lookupError = null;
  assert.equal(await owner.recover(channelSlot, 'check'), null);
  assert.equal(entryFor(owner).intent.operationID, 'op-1');
  await assert.rejects(owner.recover(channelSlot, 'retry'), /response lost/);
  assert.deepEqual(payloads.slice(0, 2), [payloads[0], payloads[0]]);
  assert.equal(
    lookups.every((id) => id === 'op-1'),
    true
  );

  found = receipt('op-1');
  await owner.recover(channelSlot, 'abandon');
  assert.deepEqual(entryFor(owner).result, { outcome: 'CREATED', packageID: '42' });
  assert.equal(payloads.length, 2);

  const other = { ...channelSlot, channelID: 'channel-2' };
  const owner2 = sessions.createProviderDraftSessions(
    {
      create: async () => {
        throw new Error('response lost');
      },
      lookup: async () => null,
    },
    () => true,
    () => `op-${++seq}`
  );
  await assert.rejects(owner2.start(other, draftInput), /response lost/);
  await owner2.recover(other, 'abandon');
  assert.equal(entryFor(owner2, other), undefined);
  await assert.rejects(owner2.start(other, draftInput), /response lost/);
  assert.equal(entryFor(owner2, other).intent.operationID, 'op-3');
});

test('provider session slots isolate actors, API endpoints and channels and clear on sign-out', async () => {
  let currentActor = 'actor-1';
  let done;
  const payloads = [];
  const owner = sessions.createProviderDraftSessions(
    {
      create: (intent) => {
        payloads.push(intent);
        if (intent.channelID === 'channel-1')
          return new Promise((resolve) => {
            done = resolve;
          });
        return Promise.resolve(receipt(intent.operationID));
      },
      lookup: async () => null,
    },
    (actorID) => actorID === currentActor,
    () => `op-${payloads.length + 1}`
  );
  const first = owner.start(channelSlot, draftInput);
  const secondChannel = { ...channelSlot, channelID: 'channel-2' };
  await owner.start(secondChannel, draftInput);
  assert.equal(entryFor(owner).status, 'busy');
  assert.equal(entryFor(owner, secondChannel).status, 'complete');
  assert.notEqual(
    sessions.providerDraftSlotKey({ ...channelSlot, endpoint: 'https://other.invalid/admin/graphql' }),
    sessions.providerDraftSlotKey(channelSlot)
  );
  currentActor = 'actor-2';
  owner.clear();
  assert.equal(owner.start(channelSlot, draftInput), undefined);
  assert.equal(owner.recover(channelSlot, 'retry'), undefined);
  assert.equal(entryFor(owner, { ...channelSlot, actorID: currentActor }), undefined);
  done(receipt('op-1'));
  assert.equal(await first, undefined);
  assert.deepEqual(owner.store.getState().entries, {});

  response = { createProviderPriceChangeSet: receipt('observer-op') };
  const authOwner = sessions.providerDraftSessions;
  const original = globalThis.__packageTransport;
  globalThis.__packageTransport = async (query, variables) =>
    query.includes('CreateProviderPriceChangeSet')
      ? { createProviderPriceChangeSet: receipt(variables.operationID) }
      : original(query, variables);
  try {
    await authOwner.start(channelSlot, draftInput);
    assert.equal(entryFor(authOwner).status, 'complete');
    assert.doesNotMatch(JSON.stringify(authOwner.store.getState()), /test-token|accessToken|credential/);
    testAuth.setState({ auth: { user: null, accessToken: '' } });
    assert.deepEqual(authOwner.store.getState().entries, {});
    testAuth.setState({ auth: { user: { id: channelSlot.actorID }, accessToken: 'new-login' } });
    assert.equal(entryFor(authOwner), undefined);
  } finally {
    globalThis.__packageTransport = original;
  }
});
