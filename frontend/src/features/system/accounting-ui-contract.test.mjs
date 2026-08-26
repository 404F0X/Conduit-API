import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';

const sourceRoot = join(import.meta.dirname, '..', '..');
const readSource = (...parts) => readFileSync(join(sourceRoot, ...parts), 'utf8');

test('general accounting settings are readable without the read_settings client gate', () => {
  const source = readSource('features', 'system', 'data', 'system.ts');
  const hook = source.slice(
    source.indexOf('export function useGeneralSettings()'),
    source.indexOf('export function useUpdateGeneralSettings()')
  );

  assert.match(source, /accountingCurrencyLocked:\s*boolean/);
  assert.match(source, /systemGeneralSettings\s*\{[\s\S]*accountingCurrencyLocked/);
  assert.doesNotMatch(hook, /read_settings|enabled\s*:/);
});

test('the accounting currency selector alone presents the backend locked state', () => {
  const source = readSource('features', 'system', 'components', 'general-settings.tsx');

  assert.match(source, /data-testid='accounting-currency-select'/);
  assert.match(source, /disabled=\{settings\?\.accountingCurrencyLocked === true\}/);
  assert.match(source, /data-testid='accounting-currency-lock-notice'/);
  assert.match(source, /currencyLockedDescription/);
});

test('channel price queries and editor preserve required row currencies', () => {
  const channelData = readSource('features', 'channels', 'data', 'channels.ts');
  const catalogData = readSource('features', 'models', 'data', 'catalog.ts');
  const dialog = readSource('features', 'channels', 'components', 'channels-model-price-dialog.tsx');
  const catalog = readSource('features', 'models', 'components', 'model-catalog.tsx');
  const channelMenu = readSource('features', 'channels', 'components', 'channel-overflow-menu.tsx');

  assert.match(channelData, /channelModelPrices\s*\{\s*id\s*modelID\s*currencyCode/);
  assert.match(channelData, /createProviderPriceChangeSet\(channelID: \$channelID, input: \$input\)/);
  assert.match(catalogData, /channelModelPrices\s*\{\s*id\s*modelID\s*currencyCode/);
  assert.match(dialog, /modelId:\s*p\.modelID,\s*currencyCode:\s*p\.currencyCode/);
  assert.match(dialog, /modelId:\s*p\.modelId,\s*currencyCode:\s*p\.currencyCode/);
  assert.match(catalog, /currency=\{price\.currencyCode\}/);
  assert.match(catalog, /useAdminPriceDisplay\(values\.currency\)/);
  assert.match(catalog, /display\.amount\(value\)/);
  assert.match(catalog, /canEditProviderPrices = hasSystemScope\('write_commercialization'\)/);
  assert.match(channelMenu, /hasSystemScope\('write_commercialization'\)[\s\S]*?openDialog\('price'\)/);
});

test('the review queue fetches every actionable status before client-side search', () => {
  const page = readSource('features', 'change-sets', 'change-set-page.tsx');
  const data = readSource('features', 'change-sets', 'data', 'change-sets.ts');

  assert.match(page, /statuses:\s*status === 'ACTIONABLE' \? ACTIONABLE_CHANGE_SET_STATUSES : undefined/);
  assert.match(page, /status:\s*status !== 'ALL' && status !== 'ACTIONABLE' \? status : undefined/);
  assert.match(data, /Promise\.all\([\s\S]*?new Set\(statuses\)[\s\S]*?\{ \.\.\.variables, status \}/);
});

test('retail approval is only exposed by the unified review workbench', () => {
  const panel = readSource('features', 'models', 'components', 'commercialization-panel.tsx');

  assert.doesNotMatch(panel, /useApproveChangeSet|approveChangeSet\.mutateAsync/);
  assert.match(panel, /to: '\/change-sets'/);
  assert.match(panel, /status: 'PENDING_REVIEW'/);
});

test('duplicating a channel refreshes its staged procurement price change set', () => {
  const channelData = readSource('features', 'channels', 'data', 'channels.ts');
  const duplicateChannel = channelData.slice(
    channelData.indexOf('export function useDuplicateChannel()'),
    channelData.indexOf('export interface BulkCreateChannelsInput')
  );

  assert.match(duplicateChannel, /invalidateQueries\(\{ queryKey: \['channels'\] \}\)/);
  assert.match(duplicateChannel, /invalidateQueries\(\{ queryKey: \['changeSets'\] \}\)/);
});

test('procurement drafts refresh the review queue and approved pricing refreshes the accounting lock', () => {
  const channelData = readSource('features', 'channels', 'data', 'channels.ts');
  const commercialization = readSource('features', 'models', 'data', 'commercialization.ts');
  const changeSets = readSource('features', 'change-sets', 'data', 'change-sets.ts');
  const createProviderPriceDraft = channelData.slice(
    channelData.indexOf('export function useCreateProviderPriceChangeSet()'),
    channelData.indexOf('// Use this hook to query channels with pagination')
  );

  assert.match(createProviderPriceDraft, /invalidateQueries\(\{ queryKey: \['changeSets'\] \}\)/);
  assert.doesNotMatch(createProviderPriceDraft, /queryKey: \['generalSettings'\]/);
  assert.match(commercialization, /invalidatesAccountingCurrency/);
  assert.match(commercialization, /export function useCreatePriceBook\(\)[\s\S]*?true\s*\n\s*\);/);
  assert.match(changeSets, /invalidateQueries\(\{ queryKey: \['generalSettings'\] \}\)/);
  for (const hook of ['useCreateRetailPriceChangeSet', 'useSaveRetailPriceChangeSetItem', 'useSubmitChangeSet', 'useApproveChangeSet']) {
    assert.match(changeSets, new RegExp(`export function ${hook}\\(\\)`));
  }
  assert.doesNotMatch(changeSets, /createPriceBookDraft|publishPriceBookVersion/);
});

test('operations money displays use the configured accounting currency', () => {
  for (const file of ['index.tsx', 'model-analytics.tsx', 'analytics-views.tsx']) {
    const source = readSource('features', 'operations', file);
    assert.match(source, /useGeneralSettings/);
    assert.match(source, /accountingCurrencyCode/);
    assert.match(source, /formatCurrencyValue/);
    assert.doesNotMatch(source, /currency:\s*['"]USD['"]/);
    assert.doesNotMatch(source, /\$\$\{(?:value|compact\.format)/);
  }
});
