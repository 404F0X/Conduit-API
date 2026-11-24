import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const output = ts.transpileModule(readFileSync(new URL('./payment-money.ts', import.meta.url), 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const module = { exports: {} };
new Function('module', 'exports', output)(module, module.exports);
const helper = module.exports;

test('CNY to fen preserves minor units and the full i64 boundary without rounding', () => {
  for (const [input, expected] of [
    ['0.01', '1'],
    ['1', '100'],
    ['1.20', '120'],
    ['90071992547409.93', '9007199254740993'],
    ['92233720368547758.07', '9223372036854775807'],
  ])
    assert.equal(helper.cnyToMinor(input), expected);
  for (const input of ['0', '-1', '+1', '1e3', '1.001', '01', 'Infinity', '92233720368547758.08', '1,000', ''])
    assert.equal(helper.cnyToMinor(input), null, input);
  assert.equal(helper.cnyToMinor('1', '101', '200'), null);
  assert.equal(helper.cnyToMinor('2.01', '100', '200'), null);
  assert.equal(helper.cnyToMinor('2', '100', '200'), '200');
});
test('credits, PPM and fen format exactly without float conversion', () => {
  assert.equal(helper.formatPaymentUnits('1', 2, true), '0.01');
  assert.equal(helper.formatPaymentUnits('100', 2, true), '1.00');
  assert.equal(helper.formatPaymentUnits('9007199254740993', 6), '9007199254.740993');
  assert.equal(helper.formatPaymentUnits('1500000', 6), '1.5');
  assert.equal(helper.formatPaymentUnits('0', 6), '0');
  assert.equal(helper.formatPaymentUnits('1e6', 6), '—');
});
test('checkout links accept HTTPS destinations and reject credentials and active schemes', () => {
  assert.equal(
    helper.paymentCheckoutHref('https://cashier.example.invalid/pay?order=fixture'),
    'https://cashier.example.invalid/pay?order=fixture'
  );
  for (const value of [
    null,
    'javascript:alert(1)',
    'http://cashier.example.invalid/pay',
    'https://user:password@cashier.example.invalid',
    '//cashier.example.invalid',
    'invalid',
  ])
    assert.equal(helper.paymentCheckoutHref(value), null);
});
