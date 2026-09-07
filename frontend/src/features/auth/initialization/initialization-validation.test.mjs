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
test('public initialization defers financial setup without rendering financial controls', () => {
  assert.match(formSource, /deferFinancialSetup: true/);
  assert.doesNotMatch(formSource, /DialogContent|accountingCurrencyCode|creditsPerAccountingUnit/);
  assert.match(formSource, /initializeSystemMutation\.mutate/);
});
