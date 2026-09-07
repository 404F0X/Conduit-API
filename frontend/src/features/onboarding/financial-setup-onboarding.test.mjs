import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';

const provider = readFileSync(join(import.meta.dirname, 'onboarding-provider.tsx'), 'utf8');
const flow = readFileSync(join(import.meta.dirname, 'financial-setup-onboarding.tsx'), 'utf8');
const systemData = readFileSync(join(import.meta.dirname, '..', 'system', 'data', 'system.ts'), 'utf8');
const setupSpec = readFileSync(join(import.meta.dirname, '..', '..', '..', 'tests', 'setup.spec.ts'), 'utf8');

test('owner financial onboarding replaces routed content and takes priority', () => {
  const financialReturn = provider.indexOf('onboardingInfo?.financialSetup?.onboarded === false');
  const routedContent = provider.indexOf('{children}', financialReturn);
  assert.notEqual(financialReturn, -1);
  assert.equal(routedContent > financialReturn, true);
  assert.match(provider, /return <FinancialSetupOnboarding/);
  assert.match(provider, /isOwner && !financialCompletedLocally/);
});

test('owner onboarding loading and errors fail closed with retry UI', () => {
  assert.match(provider, /isOwner && isLoading/);
  assert.match(provider, /isOwner && isError/);
  assert.match(provider, /refetch\(\)/);
  assert.doesNotMatch(systemData, /catch \(_error\)[\s\S]*onboarded: true/);
});

test('financial settings persist before onboarding completion', () => {
  const save = flow.indexOf('await updateSettings.mutateAsync');
  const complete = flow.indexOf('await completeSetup.mutateAsync');
  assert.equal(save >= 0 && complete > save, true);
  assert.match(flow, /currencyCodes\.includes/);
  assert.match(flow, /active:scale-\[0\.96\]/);
  assert.doesNotMatch(flow, /Dialog|backdrop|transition-all/);
});

test('easter-egg defaults are hints and never prefilled form values', () => {
  assert.match(flow, /accountingCurrencyCode: ''/);
  assert.match(flow, /creditDisplayName: ''/);
  assert.match(flow, /creditsPerAccountingUnit: ''/);
  assert.match(flow, /financialOnboarding\.creditNamePlaceholder/);
  assert.match(flow, /financialOnboarding\.creditsPerUnitPlaceholder/);
  assert.doesNotMatch(flow, /setValues\([\s\S]*settingsQuery\.data\.creditDisplayName/);
});

test('e2e setup waits for asynchronous financial onboarding before continuing', () => {
  assert.match(setupSpec, /financialHeading[\s\S]*\.waitFor\(\{ state: 'visible', timeout: 15000 \}\)/);
  assert.doesNotMatch(setupSpec, /financialHeading\.isVisible\(\{ timeout:/);
});
