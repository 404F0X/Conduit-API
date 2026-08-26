import assert from 'node:assert/strict';
import test from 'node:test';
import {
  aggregateModels,
  aggregateUsers,
  buildFlowPaths,
  buildModelTimeChart,
  buildSankeyGraph,
  bucketScrollTarget,
  lastNonzeroBucketIndex,
  loadModelAnalyticsPreferences,
  maskSensitiveLabel,
  toggleFlowStage,
} from './analytics.ts';

const base = {
  userId: 1,
  userEmail: 'a@example.com',
  projectId: 2,
  projectName: 'P',
  apiKeyId: 3,
  apiKeyName: 'K',
  requestedModel: 'chat',
  actualModel: 'vendor-chat',
  channelId: 4,
  channelName: 'C1',
  meteredRequests: 2,
  totalTokens: 100,
  recordedUpstreamCost: 0.2,
  recognizedUsageRevenue: 0.5,
  settledRequests: 2,
  lastActivityAt: '2026-08-17T00:00:00Z',
};

test('models keep same actual model separate across channels', () => {
  const models = aggregateModels([base, { ...base, channelId: 5, channelName: 'C2' }], []);
  assert.equal(models.length, 1);
  assert.equal(models[0].requests, 4);
  assert.equal(models[0].supplies.length, 2);
});

test('users aggregate unique dimensions and accounting', () => {
  const users = aggregateUsers([base, { ...base, projectId: 8, projectName: 'P2', totalTokens: 50 }]);
  assert.equal(users.length, 1);
  assert.equal(users[0].projects, 2);
  assert.equal(users[0].tokens, 150);
  assert.equal(users[0].revenue, 1);
});

test('flow paths use selected stages and merge omitted paths', () => {
  const paths = buildFlowPaths(
    [base, { ...base, userId: 9, userEmail: 'b@example.com', meteredRequests: 1 }],
    ['user', 'channel'],
    'requests',
    1
  );
  assert.equal(paths.length, 2);
  assert.deepEqual(
    paths[0].values.map((item) => item.stage),
    ['user', 'channel']
  );
  assert.equal(paths[1].other, true);
  assert.equal(paths[1].metric, 1);
});

test('flow paths aggregate identical routes by the selected metric', () => {
  const paths = buildFlowPaths([base, { ...base, totalTokens: 50 }], ['requestedModel', 'actualModel', 'channel'], 'tokens', 10);
  assert.equal(paths.length, 1);
  assert.equal(paths[0].metric, 150);
  assert.equal(paths[0].rows, 2);
});

test('flow overflow can be merged or hidden', () => {
  const rows = [base, { ...base, userId: 9, userEmail: 'b@example.com', meteredRequests: 1 }];
  assert.equal(buildFlowPaths(rows, ['user', 'channel'], 'requests', 1, 'merge').length, 2);
  assert.equal(buildFlowPaths(rows, ['user', 'channel'], 'requests', 1, 'hide').length, 1);
});

test('sankey graph merges adjacent links and excludes zero-value revenue', () => {
  const paths = buildFlowPaths([base, { ...base, totalTokens: 50 }], ['user', 'requestedModel', 'channel'], 'tokens', 10);
  const graph = buildSankeyGraph(paths, false);
  assert.equal(graph.nodes.length, 3);
  assert.deepEqual(
    graph.links.map((link) => link.value),
    [150, 150]
  );
  const empty = buildSankeyGraph(buildFlowPaths([{ ...base, recognizedUsageRevenue: null }], ['user', 'channel'], 'revenue', 10), false);
  assert.equal(empty.links.length, 0);
});

test('sensitive labels can be masked without changing ordinary stages', () => {
  assert.equal(maskSensitiveLabel('alice@example.com', 'user', true), 'al***@example.com');
  assert.equal(maskSensitiveLabel('production-key', 'apiKey', true), '•••• -key');
  assert.equal(maskSensitiveLabel('chat', 'requestedModel', true), 'chat');
});

test('stage toggling preserves a minimum of two selected stages', () => {
  assert.deepEqual(toggleFlowStage(['user', 'channel'], 'user'), ['user', 'channel']);
  assert.deepEqual(toggleFlowStage(['user', 'project', 'channel'], 'project'), ['user', 'channel']);
});

test('model preferences validate persisted values and tolerate broken storage', () => {
  const valid = { getItem: () => JSON.stringify({ periodDays: 14, mainChart: 'area', analysisMode: 'top' }) };
  assert.deepEqual(loadModelAnalyticsPreferences(valid), { periodDays: 14, mainChart: 'area', analysisMode: 'top' });
  assert.deepEqual(loadModelAnalyticsPreferences({ getItem: () => '{' }), { periodDays: 1, mainChart: 'bar', analysisMode: 'trend' });
});

test('model time chart zero pads buckets and merges overflow into Other', () => {
  const chart = buildModelTimeChart(
    {
      generatedAt: '2026-08-18T01:00:00Z',
      periodStart: '2026-08-18T00:00:00Z',
      periodEnd: '2026-08-18T02:00:00Z',
      periodDays: 1,
      granularity: 'hour',
      points: [
        {
          bucketStart: '2026-08-18T00:00:00Z',
          requestedModel: 'a',
          meteredRequests: 4,
          totalTokens: 40,
          recordedUpstreamCost: 1,
          recognizedUsageRevenue: 2,
        },
        {
          bucketStart: '2026-08-18T01:00:00Z',
          requestedModel: 'b',
          meteredRequests: 2,
          totalTokens: 20,
          recordedUpstreamCost: 1,
          recognizedUsageRevenue: 1,
        },
      ],
    },
    'requests',
    1
  );
  assert.deepEqual(chart.models, ['a', '__other__']);
  assert.equal(chart.rows.length, 3);
  assert.equal(chart.rows[1].__other__, 2);
  assert.equal(chart.rows[2].a, 0);
});

test('model chart locates the last bucket with usage across visible series', () => {
  const rows = [
    { bucketStart: '0', a: 2, b: 0 },
    { bucketStart: '1', a: 0, b: 3 },
    { bucketStart: '2', a: 0, b: 0 },
    { bucketStart: '3', a: 0, b: 0 },
  ];
  assert.equal(lastNonzeroBucketIndex(rows, ['a', 'b']), 1);
  assert.equal(lastNonzeroBucketIndex(rows, ['a']), 0);
  assert.equal(lastNonzeroBucketIndex(rows, ['missing']), -1);
});

test('model chart scroll target places useful bucket near 70 percent and clamps', () => {
  assert.ok(Math.abs(bucketScrollTarget(6, 10, 1000, 400) - 376.4) < 0.001);
  assert.equal(bucketScrollTarget(9, 10, 1000, 400), 600);
  assert.equal(bucketScrollTarget(0, 10, 1000, 400), 0);
  assert.equal(bucketScrollTarget(-1, 10, 1000, 400), 0);
});
