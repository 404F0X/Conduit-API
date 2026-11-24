import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

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
const packages = await loadSource('data/change-sets.ts', [
  [
    /import \{ useMutation, useQuery, useQueryClient \} from '@tanstack\/react-query';/,
    `const useMutation=options=>options; const useQuery=options=>options; const useQueryClient=()=>globalThis.__packageQueryClient;`,
  ],
  [/import \{ graphqlRequest \} from '@\/gql\/graphql';/, `const graphqlRequest=(...args)=>globalThis.__packageTransport(...args);`],
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
