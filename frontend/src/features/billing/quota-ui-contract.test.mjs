import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import test from 'node:test';

const sourceRoot = join(import.meta.dirname, '..', '..');
const readSource = (...parts) => readFileSync(join(sourceRoot, ...parts), 'utf8');

test('billing queries request quota rules, bucket snapshots, and split project totals', () => {
  const source = readSource('features', 'billing', 'data.ts');

  assert.match(source, /quotaRules \{ \$\{QUOTA_RULE_FIELDS\} \}/);
  assert.match(source, /allowanceBuckets \{ \$\{ALLOWANCE_BUCKET_FIELDS\} \}/);
  assert.match(source, /generalRemainingAllowance dedicatedRemainingAllowance/);
  assert.match(source, /generalSubscriptionBalance dedicatedSubscriptionBalance/);
});

test('plan mutation inputs use repeatable quota rules instead of one mutable allowance', () => {
  const source = readSource('features', 'billing', 'data.ts');
  const createInput = source.slice(
    source.indexOf('export type CreateSubscriptionPlanInput'),
    source.indexOf('export type UpdateSubscriptionPlanInput')
  );
  const updateInput = source.slice(
    source.indexOf('export type UpdateSubscriptionPlanInput'),
    source.indexOf('export type BillingAccessBundle')
  );

  assert.match(createInput, /quotaRules: SubscriptionQuotaRuleInput\[\]/);
  assert.match(updateInput, /quotaRules: SubscriptionQuotaRuleInput\[\]/);
  assert.doesNotMatch(createInput, /\n\s*allowance:/);
  assert.doesNotMatch(updateInput, /\n\s*allowance:/);
});

test('plan editor keeps funding scopes visibly separate from access permissions', () => {
  const source = readSource('features', 'billing', 'index.tsx');

  assert.match(source, /billing\.quotaRule\.title/);
  assert.match(source, /billing\.quotaRule\.scope/);
  assert.match(source, /billing\.accessPermissions\.title/);
  assert.match(source, /quotaClass === 'DEDICATED'/);
});

test('wallet renders active bucket snapshots and the funding-order strip', () => {
  const source = readSource('features', 'wallet', 'index.tsx');

  assert.match(source, /spendableAllowanceBuckets\(subscription\)/);
  assert.match(source, /<WalletBucketCard/);
  assert.match(source, /<FundingOrderStrip/);
  assert.match(source, /bucket\.modelIDs/);
});
