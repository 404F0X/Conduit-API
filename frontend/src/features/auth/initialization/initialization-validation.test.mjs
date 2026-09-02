import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'initialization-validation.ts'), 'utf8');
const transpiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.ESNext,
    target: ts.ScriptTarget.ES2023,
  },
}).outputText;
const moduleUrl = `data:text/javascript;base64,${Buffer.from(transpiled).toString('base64')}`;
const { initializationPasswordsMatch } = await import(moduleUrl);

test('initialization requires the owner password to be entered identically twice', () => {
  assert.equal(
    initializationPasswordsMatch({
      ownerPassword: 'correct-password',
      confirmOwnerPassword: 'correct-password',
    }),
    true
  );
  assert.equal(
    initializationPasswordsMatch({
      ownerPassword: 'correct-password',
      confirmOwnerPassword: 'mistyped-password',
    }),
    false
  );
});

const formSource = readFileSync(join(import.meta.dirname, 'components', 'initialization-form.tsx'), 'utf8');
const autoCompleteSource = readFileSync(join(import.meta.dirname, '..', '..', '..', 'components', 'auto-complete-select.tsx'), 'utf8');

test('account step opens the required financial dialog without issuing initialization', () => {
  const accountSubmit = formSource.slice(formSource.indexOf('function onSubmit'), formSource.indexOf('function onFinancialSubmit'));
  assert.match(accountSubmit, /setFinancialDialogOpen\(true\)/);
  assert.doesNotMatch(accountSubmit, /\.mutate\(/);
});

test('financial dialog has no dismissal bypass and blocks invalid requests', () => {
  assert.match(formSource, /showCloseButton=\{false\}/);
  assert.match(formSource, /onEscapeKeyDown=\{\(event\) => event\.preventDefault\(\)\}/);
  assert.match(formSource, /onInteractOutside=\{\(event\) => event\.preventDefault\(\)\}/);
  assert.match(formSource, /!financialSettingsValid \|\| initializeSystemMutation\.isPending/);
  assert.doesNotMatch(formSource, /skip/i);
});

test('financial dialog opens calmly with a named currency combobox', () => {
  assert.match(formSource, /onOpenAutoFocus=\{\(event\) => \{/);
  assert.match(formSource, /financialDialogTitleRef\.current\?\.focus\(\)/);
  assert.match(formSource, /id='initialization-accounting-currency-label'/);
  assert.match(formSource, /ariaLabelledBy='initialization-accounting-currency-label'/);
  assert.match(autoCompleteSource, /aria-labelledby=\{ariaLabelledBy\}/);
});

test('financial dialog hardens mobile actions, long previews, and conditional descriptions', () => {
  assert.equal((formSource.match(/className='h-11 w-full sm:h-9 sm:w-auto'/g) ?? []).length, 2);
  assert.match(formSource, /\[overflow-wrap:anywhere\]/);
  assert.match(formSource, /initialization-accounting-currency-help\$\{/);
  assert.match(formSource, /initialization-credit-display-name-help\$\{/);
  assert.match(formSource, /initialization-credits-per-unit-help\$\{/);
});

test('final initialization payload contains normalized accounting fields', () => {
  assert.match(formSource, /accountingCurrencyCode: normalizedAccountingCurrency/);
  assert.match(formSource, /creditDisplayName: normalizedCreditDisplayName/);
  assert.match(formSource, /creditsPerAccountingUnit: normalizedCreditsPerAccountingUnit/);
});
