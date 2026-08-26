import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'model-catalog-health.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const module = { exports: {} };
new Function('require', 'module', 'exports', output)(() => ({}), module, module.exports);

const { aggregatePublicModelHealth, aggregateUpstreamModelHealth, channelNumericID } = module.exports;

test('upstream model health aggregates credential rows by attempts rather than averaging percentages', () => {
  const health = aggregateUpstreamModelHealth(
    [
      { channelId: 47, actualModel: 'claude-sonnet-5', upstreamAttempts: 90, successfulAttempts: 90, failedAttempts: 0 },
      { channelId: 47, actualModel: 'claude-sonnet-5', upstreamAttempts: 10, successfulAttempts: 0, failedAttempts: 10 },
      { channelId: 47, actualModel: 'other-model', upstreamAttempts: 100, successfulAttempts: 100, failedAttempts: 0 },
    ],
    'gid://conduit/Channel/47',
    'claude-sonnet-5'
  );

  assert.deepEqual(health, {
    state: 'warning',
    rate: 0.9,
    attempts: 100,
    successes: 90,
    failures: 10,
    credentialCount: 2,
  });
});

test('upstream model health retains permanent failure evidence while aggregating credential rows', () => {
  const health = aggregateUpstreamModelHealth(
    [
      {
        channelId: 47,
        actualModel: 'claude-sonnet-5',
        upstreamAttempts: 9,
        successfulAttempts: 8,
        failedAttempts: 1,
        errorBreakdown: [{ category: 'auth', count: 1 }],
      },
      { channelId: 47, actualModel: 'claude-sonnet-5', upstreamAttempts: 1, successfulAttempts: 1, failedAttempts: 0 },
    ],
    'gid://conduit/Channel/47',
    'claude-sonnet-5'
  );

  assert.equal(health.state, 'error');
  assert.equal(health.rate, 0.9);
});

test('channel numeric ID accepts GraphQL GIDs and rejects nonnumeric IDs', () => {
  assert.equal(channelNumericID('gid://conduit/Channel/47'), 47);
  assert.equal(channelNumericID('47'), 47);
  assert.equal(channelNumericID('channel-a'), null);
});

test('public model health aggregates only enabled routes on enabled channels and deduplicates channel probes', () => {
  const health = aggregatePublicModelHealth(
    [
      { status: 'ENABLED', channelID: 'channel-a' },
      { status: 'ENABLED', channelID: 'channel-a' },
      { status: 'ENABLED', channelID: 'channel-b' },
      { status: 'DISABLED', channelID: 'channel-c' },
      { status: 'ENABLED', channelID: 'channel-d' },
    ],
    [
      { id: 'channel-a', status: 'enabled' },
      { id: 'channel-b', status: 'enabled' },
      { id: 'channel-c', status: 'enabled' },
      { id: 'channel-d', status: 'disabled' },
    ],
    new Map([
      ['channel-a', [{ totalRequestCount: 10, successRequestCount: 9 }]],
      ['channel-b', [{ totalRequestCount: 10, successRequestCount: 5 }]],
      ['channel-c', [{ totalRequestCount: 100, successRequestCount: 100 }]],
      ['channel-d', [{ totalRequestCount: 100, successRequestCount: 0 }]],
    ])
  );

  assert.equal(health.state, 'warning');
  assert.equal(health.rate, 0.7);
});

test('public model health reports empty when eligible routes have no samples', () => {
  const health = aggregatePublicModelHealth(
    [{ status: 'ENABLED', channelID: 'channel-a' }],
    [{ id: 'channel-a', status: 'enabled' }],
    new Map([['channel-a', [{ totalRequestCount: 0, successRequestCount: 0 }]]])
  );

  assert.deepEqual(health, { state: 'empty', rate: null });
});
