import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(join(import.meta.dirname, 'currency-format.ts'), 'utf8');
const output = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2023 },
}).outputText;
const module = { exports: {} };
new Function('require', 'module', 'exports', output)(() => ({}), module, module.exports);

const { formatCurrencyValue, getCurrencyInputAffix } = module.exports;

test('formats a configurable non-ISO unit without passing it to Intl currency formatting', () => {
  assert.equal(formatCurrencyValue(1.25, 'CUSTOM_UNIT', 'zh-CN'), '1.25 CUSTOM_UNIT');
  assert.equal(formatCurrencyValue(1.25, 'CUSTOM_UNIT', 'en-US'), '1.25 CUSTOM_UNIT');
  assert.equal(getCurrencyInputAffix('CUSTOM_UNIT', 'zh-CN'), 'CUSTOM_UNIT');
});

test('keeps Intl currency formatting for real-world currency codes', () => {
  assert.equal(formatCurrencyValue(12.5, 'USD', 'en-US'), '$12.50');
  assert.equal(getCurrencyInputAffix('USD', 'en-US'), '$');
});

test('safely formats other configurable unit names', () => {
  assert.equal(formatCurrencyValue(1234.5, 'CREDIT_UNIT', 'en-US'), '1,234.5 CREDIT_UNIT');
  assert.equal(getCurrencyInputAffix('CREDIT_UNIT', 'en-US'), 'CREDIT_UNIT');
  assert.equal(formatCurrencyValue(12.5, 'Shrine credits', 'en-US'), '12.5 Shrine credits');
  assert.equal(getCurrencyInputAffix('Shrine credits', 'en-US'), 'Shrine credits');
});
