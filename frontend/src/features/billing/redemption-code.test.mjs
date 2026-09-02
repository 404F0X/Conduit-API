import assert from 'node:assert/strict';
import test from 'node:test';
import {
  canRedeemCode,
  isValidCreditAmount,
  isValidRedemptionQuantity,
  isValidRedemptionUseLimit,
  normalizeRedemptionCode,
} from './redemption-code.ts';

test('redemption code normalization removes outer whitespace without changing case', () => {
  assert.equal(normalizeRedemptionCode('  AbC-123  '), 'AbC-123');
  assert.equal(canRedeemCode(' \n\t '), false);
  assert.equal(canRedeemCode(' code '), true);
  assert.equal(canRedeemCode('x'.repeat(128)), true);
  assert.equal(canRedeemCode('x'.repeat(129)), false);
});

test('redemption-code issuance validates credit precision and batch size', () => {
  assert.equal(isValidCreditAmount('10.123456'), true);
  assert.equal(isValidCreditAmount('10.1234567'), false);
  assert.equal(isValidCreditAmount('0'), false);
  assert.equal(isValidCreditAmount('-1'), false);
  assert.equal(isValidRedemptionQuantity(1), true);
  assert.equal(isValidRedemptionQuantity(1000), true);
  assert.equal(isValidRedemptionQuantity(1001), false);
  assert.equal(isValidRedemptionQuantity(1.5), false);
  assert.equal(isValidRedemptionUseLimit(1), true);
  assert.equal(isValidRedemptionUseLimit(100_000), true);
  assert.equal(isValidRedemptionUseLimit(100_001), false);
  assert.equal(isValidRedemptionUseLimit(1.5), false);
});
