import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'configuration-navigation.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const module = { exports: {} };
new Function('module', 'exports', output)(module, module.exports);
const { getChannelConfigurationTarget, getFirstChannelConfigurationErrorPath } = module.exports;

test('nested credentials and array model errors return the actual first validation path', () => {
  const errors = { credentials: { apiKeys: [{ type: 'custom', message: 'Invalid key' }] }, defaultTestModel: { type: 'required' } };
  const path = getFirstChannelConfigurationErrorPath(errors);
  assert.equal(path, 'credentials.apiKeys.0');
  assert.deepEqual(getChannelConfigurationTarget(path), { category: 'connection', selector: '[data-testid="channel-api-key-input"]' });
  assert.equal(getFirstChannelConfigurationErrorPath({ supportedModels: { root: { message: 'Required' } } }), 'supportedModels.root');
});

test('quota errors target the billing control inside the collapsed snapshot', () => {
  for (const field of ['websiteURL', 'quotaCurrency', 'actualQuotaUsed', 'quotaRemaining']) {
    assert.deepEqual(getChannelConfigurationTarget(field), { category: 'billing', selector: `[name="${field}"]` });
  }
  assert.equal(getChannelConfigurationTarget('billingCurrency').selector, '#channel-billing-currency');
  assert.equal(getChannelConfigurationTarget('settings.rechargeMultiplier').category, 'billing');
});

test('manual retry and nested policy errors reveal request settings rather than connection', () => {
  assert.deepEqual(getChannelConfigurationTarget('retryableStatusCodes'), { category: 'request', selector: '#channel-retry-status-codes' });
  assert.deepEqual(getChannelConfigurationTarget('settings.retryableErrorPatterns.0.pattern'), {
    category: 'request',
    selector: '#channel-retry-error-patterns',
  });
  assert.equal(
    getChannelConfigurationTarget(getFirstChannelConfigurationErrorPath({ policies: { stream: { message: 'Invalid' } } })).category,
    'request'
  );
  for (const field of ['passThroughBody', 'passThroughUserAgent']) assert.equal(getChannelConfigurationTarget(field).category, 'request');
});

test('metadata and pending model selections retain distinct categories', () => {
  assert.deepEqual(getChannelConfigurationTarget('selectedFetchedModels'), {
    category: 'connection',
    selector: '#channel-confirm-fetched-models',
  });
  assert.equal(getChannelConfigurationTarget('tags.0').category, 'other');
  assert.equal(getChannelConfigurationTarget('remark').category, 'other');
  for (const field of ['selectedFetchedModels', 'supportedModels.0', 'autoSyncModelPattern', 'defaultTestModel']) {
    assert.equal(getChannelConfigurationTarget(field).category, 'connection');
  }
});

test('ignores DOM refs and safely falls back for an empty or unknown error', () => {
  const ref = {};
  ref.ref = ref;
  assert.equal(getFirstChannelConfigurationErrorPath({ ref, name: { message: 'Required', ref } }), 'name');
  assert.equal(getFirstChannelConfigurationErrorPath({}), undefined);
  assert.equal(getFirstChannelConfigurationErrorPath(null), undefined);
  assert.deepEqual(getChannelConfigurationTarget('unrenderedField'), {
    category: 'connection',
    selector: '[data-channel-category="connection"]',
  });
});
