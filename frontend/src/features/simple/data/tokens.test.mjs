import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import test from 'node:test';
import ts from 'typescript';

const require = createRequire(import.meta.url);
const source = readFileSync(new URL('./tokens.ts', import.meta.url), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const loaded = { exports: {} };
new Function('require', 'module', 'exports', output)(
  (id) => (id.startsWith('@/') || id === 'react-i18next' || id === '@tanstack/react-query' ? {} : require(id)),
  loaded,
  loaded.exports
);
const { planSimpleTokenUpdate, hasAdvancedSettings, buildSimpleTokenCreation, localExpiryDate } = loaded.exports;
const expiry = '2031-08-16T00:12:34.123456+09:00';
const profile = {
  name: 'existing',
  modelMappings: [],
  channelIDs: [],
  channelTags: [],
  channelTagsMatchMode: 'any',
  modelIDs: ['model-a'],
  validFrom: null,
  validUntil: expiry,
  loadBalanceStrategy: null,
  maxConcurrentRequests: 0,
  quota: { cost: 10, requests: null, totalTokens: null, period: { type: 'all_time', pastDuration: null, calendarDuration: null } },
};
const token = { id: 'key', name: 'Original', modelGroupID: 'group-a', profiles: { activeProfile: 'existing', profiles: [profile] } };
const form = { name: 'Renamed', quota: 10, modelIds: ['model-a'], expiresAt: new Date(expiry), expiryChanged: false };

test('rename omits profile replacement and preserves the original group and exact expiry', () => {
  const before = structuredClone(token);
  assert.deepEqual(planSimpleTokenUpdate(token, form, 'new'), { name: { name: 'Renamed' }, profiles: undefined });
  assert.deepEqual(token, before);
  assert.equal(token.profiles.profiles[0].validUntil, expiry);
});

test('advanced constraints remain read only even when form settings differ', () => {
  for (const changes of [
    { validFrom: '2030-01-01T00:00:00Z' },
    { maxConcurrentRequests: 2 },
    { loadBalanceStrategy: 'weighted' },
    { loadBalanceStrategy: 'adaptive' },
    { quota: { ...profile.quota, period: { type: 'calendar_duration', calendarDuration: { unit: 'month' } } } },
  ]) {
    const advanced = { ...token, profiles: { ...token.profiles, profiles: [{ ...profile, ...changes }] } };
    const before = structuredClone(advanced);
    assert.equal(hasAdvancedSettings(advanced), true);
    assert.deepEqual(planSimpleTokenUpdate(advanced, { ...form, quota: null, modelIds: [], expiryChanged: true, expiresAt: null }, 'new'), {
      name: { name: 'Renamed' },
      profiles: undefined,
    });
    assert.deepEqual(advanced, before);
  }
  assert.equal(
    hasAdvancedSettings({ ...token, profiles: { ...token.profiles, profiles: [profile, { ...profile, name: 'second' }] } }),
    true
  );
  assert.equal(hasAdvancedSettings(token), false);
  assert.equal(
    hasAdvancedSettings({ ...token, profiles: { ...token.profiles, profiles: [{ ...profile, loadBalanceStrategy: '' }] } }),
    false
  );
});

test('explicit quota edit merges full original profile and preserves precise untouched expiry', () => {
  const result = planSimpleTokenUpdate(token, { ...form, name: token.name, quota: 20 }, 'new');
  assert.equal(result.name, undefined);
  assert.deepEqual(result.profiles, { ...token.profiles, profiles: [{ ...profile, quota: { ...profile.quota, cost: 20 } }] });
  assert.equal(result.profiles.profiles[0].validUntil, expiry);
  assert.equal(token.profiles.profiles[0].quota.cost, 10);
});

test('explicit expiry edit uses its instant, while the local date control is timezone aware', () => {
  const previousTZ = process.env.TZ;
  process.env.TZ = 'America/New_York';
  try {
    const date = new Date('2031-08-16T00:12:34+09:00');
    assert.equal(localExpiryDate(date), '2031-08-15'); // test runner uses America/New_York
    assert.equal(
      planSimpleTokenUpdate(token, { ...form, expiryChanged: true, expiresAt: date }, 'new').profiles.profiles[0].validUntil,
      date.toISOString()
    );
    assert.equal(planSimpleTokenUpdate(token, form, 'new').profiles, undefined);
  } finally {
    if (previousTZ === undefined) delete process.env.TZ;
    else process.env.TZ = previousTZ;
  }
});

test('creation requires an authorized configured group and only that catalog models', () => {
  const create = { ...form, modelGroupID: 'group-a' };
  const catalog = { configured: true, modelGroupID: 'group-a', models: [{ id: 'model-a' }] };
  const payload = buildSimpleTokenCreation(create, 'default', [{ id: 'group-a' }], catalog);
  assert.equal(payload.modelGroupID, 'group-a');
  assert.deepEqual(payload.profiles.profiles[0].modelIDs, ['model-a']);
  assert.throws(() => buildSimpleTokenCreation(create, 'default', [], catalog));
  assert.throws(() => buildSimpleTokenCreation(create, 'default', [{ id: 'group-a' }], { ...catalog, configured: false }));
  assert.throws(() => buildSimpleTokenCreation(create, 'default', [{ id: 'group-a' }], { ...catalog, modelGroupID: 'other' }));
  assert.throws(() => buildSimpleTokenCreation({ ...create, modelIds: ['outside-group'] }, 'default', [{ id: 'group-a' }], catalog));
});
