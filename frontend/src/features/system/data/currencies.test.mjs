import assert from 'node:assert/strict';
import test from 'node:test';
import { currencyCodes, ISO_4217_PUBLICATION_DATE } from './currencies.ts';

test('currency selector tracks the complete current ISO 4217 List One', () => {
  assert.equal(ISO_4217_PUBLICATION_DATE, '2026-01-01');
  assert.equal(currencyCodes.length, 178);
  assert.deepEqual(currencyCodes, [...new Set(currencyCodes)].sort());

  for (const code of ['MRU', 'STN', 'BHD', 'GHS', 'KWD', 'OMR', 'SLE', 'VES', 'XCG', 'ZWG']) {
    assert.equal(currencyCodes.includes(code), true, `${code} must be selectable`);
  }

  for (const retiredCode of ['BGN', 'HRK', 'MRO', 'SLL', 'STD', 'ZWL']) {
    assert.equal(currencyCodes.includes(retiredCode), false, `${retiredCode} is retired`);
  }
});
