import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'accounting.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const module = { exports: {} };
new Function('require', 'module', 'exports', output)(() => ({}), module, module.exports);

const {
  DEFAULT_ACCOUNTING_CURRENCY_CODE,
  DEFAULT_CREDIT_DISPLAY_NAME,
  DEFAULT_CREDITS_PER_ACCOUNTING_UNIT,
  currencyConversionFactor,
  currencyToAccountingFactor,
  isCurrencyCode,
  isPositiveDecimal,
} = module.exports;

test('ships the product accounting defaults', () => {
  assert.equal(DEFAULT_ACCOUNTING_CURRENCY_CODE, 'CNY');
  assert.equal(DEFAULT_CREDIT_DISPLAY_NAME, '神社塞钱');
  assert.equal(DEFAULT_CREDITS_PER_ACCOUNTING_UNIT, '10000');
});

test('validates accounting inputs without treating the display name as a currency key', () => {
  assert.equal(isCurrencyCode('cny'), true);
  assert.equal(isCurrencyCode('神社塞钱'), false);
  assert.equal(isPositiveDecimal('10000'), true);
  assert.equal(isPositiveDecimal('0.125'), true);
  assert.equal(isPositiveDecimal('0'), false);
  assert.equal(isPositiveDecimal('-1'), false);
});

test('converts quote currency into accounting currency using an explicit directional rate', () => {
  const settings = {
    accountingCurrencyCode: 'CNY',
    exchangeRates: [{ currencyCode: 'USD', quotePerAccountingUnit: '0.125' }],
  };
  assert.equal(currencyToAccountingFactor(settings, 'CNY'), 1);
  assert.equal(currencyToAccountingFactor(settings, 'USD'), 8);
  assert.equal(currencyToAccountingFactor(settings, 'EUR'), null);
});

test('converts between row currencies without relabelling the stored amount', () => {
  const settings = {
    accountingCurrencyCode: 'CNY',
    exchangeRates: [
      { currencyCode: 'USD', quotePerAccountingUnit: '0.125' },
      { currencyCode: 'EUR', quotePerAccountingUnit: '0.1' },
    ],
  };
  assert.equal(currencyConversionFactor(settings, 'USD', 'CNY'), 8);
  assert.equal(currencyConversionFactor(settings, 'USD', 'EUR'), 0.8);
  assert.equal(currencyConversionFactor(settings, 'USD', 'JPY'), null);
});
