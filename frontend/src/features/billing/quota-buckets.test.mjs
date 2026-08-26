import assert from 'node:assert/strict';
import test from 'node:test';
import {
  activeAllowanceBuckets,
  bucketTotalsByClass,
  planAllowance,
  quotaRulesForPlan,
  spendableAllowanceBuckets,
  sumQuotaAmounts,
} from './quota-buckets.ts';

const basePlan = {
  id: 'plan-1',
  name: 'Builder',
  intervalUnit: 'MONTH',
  intervalCount: 1,
  status: 'ENABLED',
  accessPlans: [],
};

test('quota amount sums retain six-decimal precision', () => {
  assert.equal(sumQuotaAmounts(['0.000001', '1.25', '2.000009']), '3.25001');
});

test('new plan quota rules remain separate and aggregate for compact summaries', () => {
  const plan = {
    ...basePlan,
    quotaRules: [
      {
        id: 'general',
        name: 'General',
        quotaClass: 'GENERAL',
        allowance: '10',
        rolloverMode: 'NONE',
        accessPlans: [],
      },
      {
        id: 'claude',
        name: 'Claude pack',
        quotaClass: 'DEDICATED',
        allowance: '5.5',
        rolloverMode: 'CAPPED',
        rolloverCap: '2',
        carryoverDays: 30,
        accessPlans: [{ id: 'premium', name: 'Premium' }],
      },
    ],
  };

  assert.equal(quotaRulesForPlan(plan).length, 2);
  assert.equal(planAllowance(plan), '15.5');
});

test('legacy plans fall back to one read-only rule without conflating new plans', () => {
  const rules = quotaRulesForPlan({
    ...basePlan,
    allowance: '8',
    rolloverMode: 'NONE',
    accessPlans: [{ id: 'legacy-scope', name: 'Legacy scope' }],
    quotaRules: [],
  });

  assert.equal(rules.length, 1);
  assert.equal(rules[0].quotaClass, 'DEDICATED');
  assert.deepEqual(rules[0].accessPlans, [{ id: 'legacy-scope', name: 'Legacy scope' }]);
});

test('active buckets use dedicated-first FEFO order and exclude terminal or empty buckets', () => {
  const subscription = {
    id: 'subscription-1',
    plan: { ...basePlan, quotaRules: [] },
    currentPeriodStart: '2026-08-01T00:00:00Z',
    currentPeriodEnd: '2026-09-01T00:00:00Z',
    grantedAllowance: '12',
    consumedAllowance: '0',
    reservedAllowance: '0',
    remainingAllowance: '12',
    allowanceBuckets: [
      {
        id: 'd-late',
        quotaClass: 'DEDICATED',
        status: 'ACTIVE',
        expiresAt: '2026-09-15T00:00:00Z',
        remainingAllowance: '1.5',
        reservedAllowance: '0.5',
      },
      {
        id: 'g',
        quotaClass: 'GENERAL',
        status: 'ACTIVE',
        expiresAt: '2026-09-01T00:00:00Z',
        remainingAllowance: '4',
        reservedAllowance: '0',
      },
      {
        id: 'd-early',
        quotaClass: 'DEDICATED',
        status: 'ACTIVE',
        expiresAt: '2026-08-31T00:00:00Z',
        remainingAllowance: '2',
        reservedAllowance: '0',
      },
      { id: 'done', quotaClass: 'GENERAL', status: 'DEPLETED', remainingAllowance: '9', reservedAllowance: '0' },
      { id: 'empty', quotaClass: 'GENERAL', status: 'ACTIVE', remainingAllowance: '0', reservedAllowance: '0' },
    ],
  };

  const active = activeAllowanceBuckets(subscription);
  assert.deepEqual(
    active.map((bucket) => bucket.id),
    ['d-early', 'd-late', 'g']
  );
  assert.deepEqual(bucketTotalsByClass(active), { GENERAL: '4', DEDICATED: '3.5' });
  assert.deepEqual(spendableAllowanceBuckets({ ...subscription, status: 'PAUSED' }), []);
});
