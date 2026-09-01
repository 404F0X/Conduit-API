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
  normalizeCurrencyCode,
  validateInitialAccountingSettings,
} = module.exports;

test('ships the product accounting defaults', () => {
  assert.equal(DEFAULT_ACCOUNTING_CURRENCY_CODE, 'CNY');
  assert.equal(DEFAULT_CREDIT_DISPLAY_NAME, '神社塞钱');
  assert.equal(DEFAULT_CREDITS_PER_ACCOUNTING_UNIT, '10000');
});

test('validates and normalizes mandatory first-run accounting settings', () => {
  for (const input of [
    { accountingCurrencyCode: '', creditDisplayName: 'Credits', creditsPerAccountingUnit: '1' },
    { accountingCurrencyCode: 'US', creditDisplayName: 'Credits', creditsPerAccountingUnit: '1' },
    { accountingCurrencyCode: 'US1', creditDisplayName: 'Credits', creditsPerAccountingUnit: '1' },
    { accountingCurrencyCode: 'USD', creditDisplayName: '   ', creditsPerAccountingUnit: '1' },
    { accountingCurrencyCode: 'USD', creditDisplayName: 'Credits', creditsPerAccountingUnit: '0' },
    { accountingCurrencyCode: 'USD', creditDisplayName: 'Credits', creditsPerAccountingUnit: '-1' },
    { accountingCurrencyCode: 'USD', creditDisplayName: 'Credits', creditsPerAccountingUnit: 'nope' },
  ]) {
    assert.equal(validateInitialAccountingSettings(input).isValid, false);
  }

  assert.deepEqual(
    validateInitialAccountingSettings({
      accountingCurrencyCode: ' usd ',
      creditDisplayName: ' API credits ',
      creditsPerAccountingUnit: ' 2500.50 ',
    }),
    {
      normalized: {
        accountingCurrencyCode: 'USD',
        creditDisplayName: 'API credits',
        creditsPerAccountingUnit: '2500.50',
      },
      fields: {
        accountingCurrencyCode: true,
        creditDisplayName: true,
        creditsPerAccountingUnit: true,
      },
      isValid: true,
    }
  );
});

test('validates accounting inputs without treating the display name as a currency key', () => {
  assert.equal(isCurrencyCode('cny'), true);
  assert.equal(normalizeCurrencyCode(' usd '), 'USD');
  assert.equal(isCurrencyCode(''), false);
  assert.equal(isCurrencyCode('US'), false);
  assert.equal(isCurrencyCode('US1'), false);
  assert.equal(isCurrencyCode('神社塞钱'), false);
  assert.equal(isPositiveDecimal('10000'), true);
  assert.equal(isPositiveDecimal('0.125'), true);
  assert.equal(isPositiveDecimal('0'), false);
  assert.equal(isPositiveDecimal('-1'), false);
  assert.equal(isPositiveDecimal('not-a-number'), false);
  assert.equal(isPositiveDecimal('0.0000000000000000000000000001'), true);
  assert.equal(isPositiveDecimal('0.00000000000000000000000000001'), false);
  assert.equal(isPositiveDecimal('79228162514264337593543950335'), true);
  assert.equal(isPositiveDecimal('79228162514264337593543950336'), false);
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
