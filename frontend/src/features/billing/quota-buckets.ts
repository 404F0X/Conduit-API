import type { QuotaClass, SubscriptionAllowanceBucket, SubscriptionPlan, SubscriptionQuotaRule, UserSubscription } from './data';

const MICROS_PER_UNIT = 1_000_000n;
const TERMINAL_BUCKET_STATUSES = new Set(['CANCELLED', 'DEPLETED', 'EXPIRED', 'VOID']);

function parseMicros(value: string | null | undefined): bigint {
  if (!value) return 0n;
  const match = value.trim().match(/^(\d+)(?:\.(\d{0,6}))?$/);
  if (!match) return 0n;
  return BigInt(match[1]) * MICROS_PER_UNIT + BigInt((match[2] || '').padEnd(6, '0'));
}

function formatMicros(value: bigint): string {
  const whole = value / MICROS_PER_UNIT;
  const fraction = String(value % MICROS_PER_UNIT)
    .padStart(6, '0')
    .replace(/0+$/, '');
  return fraction ? `${whole}.${fraction}` : String(whole);
}

export function sumQuotaAmounts(values: ReadonlyArray<string | null | undefined>): string {
  return formatMicros(values.reduce((total, value) => total + parseMicros(value), 0n));
}

export function quotaRulesForPlan(plan: SubscriptionPlan): SubscriptionQuotaRule[] {
  if (plan.quotaRules?.length) return plan.quotaRules;
  if (!plan.allowance) return [];

  return [
    {
      id: `legacy-${plan.id}`,
      name: plan.name,
      quotaClass: plan.accessPlans?.length ? 'DEDICATED' : 'GENERAL',
      allowance: plan.allowance,
      rolloverMode: plan.rolloverMode || 'NONE',
      rolloverCap: plan.rolloverCap,
      carryoverDays: null,
      accessPlans: plan.accessPlans || [],
    },
  ];
}

export function allowanceBucketsForSubscription(subscription: UserSubscription): SubscriptionAllowanceBucket[] {
  if (subscription.allowanceBuckets?.length) return subscription.allowanceBuckets;
  if (!subscription.grantedAllowance) return [];

  const fallbackRule = quotaRulesForPlan(subscription.plan)[0];
  return [
    {
      id: `legacy-${subscription.id}`,
      name: fallbackRule?.name || subscription.plan.name,
      quotaClass: fallbackRule?.quotaClass || 'GENERAL',
      sourceType: 'CURRENT',
      periodStart: subscription.currentPeriodStart,
      periodEnd: subscription.currentPeriodEnd,
      expiresAt: subscription.currentPeriodEnd,
      grantedAllowance: subscription.grantedAllowance,
      consumedAllowance: subscription.consumedAllowance,
      reservedAllowance: subscription.reservedAllowance,
      remainingAllowance: subscription.remainingAllowance,
      status: subscription.status,
      accessPlans: fallbackRule?.accessPlans || [],
      modelIDs: fallbackRule?.quotaClass === 'DEDICATED' ? subscription.grantedModelIDs || [] : [],
      sourceBucketID: null,
    },
  ];
}

export function activeAllowanceBuckets(subscription: UserSubscription): SubscriptionAllowanceBucket[] {
  return allowanceBucketsForSubscription(subscription)
    .filter((bucket) => {
      if (TERMINAL_BUCKET_STATUSES.has(bucket.status.toUpperCase())) return false;
      return parseMicros(bucket.remainingAllowance) > 0n || parseMicros(bucket.reservedAllowance) > 0n;
    })
    .sort((left, right) => {
      const classOrder = Number(right.quotaClass === 'DEDICATED') - Number(left.quotaClass === 'DEDICATED');
      if (classOrder) return classOrder;
      const expiryOrder = new Date(left.expiresAt).getTime() - new Date(right.expiresAt).getTime();
      return expiryOrder || left.id.localeCompare(right.id);
    });
}

export function spendableAllowanceBuckets(subscription: UserSubscription): SubscriptionAllowanceBucket[] {
  return subscription.status.toUpperCase() === 'ACTIVE'
    ? activeAllowanceBuckets(subscription).filter((bucket) => bucket.status.toUpperCase() === 'ACTIVE')
    : [];
}

export function bucketTotalsByClass(buckets: readonly SubscriptionAllowanceBucket[]): Record<QuotaClass, string> {
  return {
    GENERAL: sumQuotaAmounts(buckets.filter((bucket) => bucket.quotaClass === 'GENERAL').map((bucket) => bucket.remainingAllowance)),
    DEDICATED: sumQuotaAmounts(buckets.filter((bucket) => bucket.quotaClass === 'DEDICATED').map((bucket) => bucket.remainingAllowance)),
  };
}

export function planTotalsByClass(plan: SubscriptionPlan): Record<QuotaClass, string> {
  const rules = quotaRulesForPlan(plan);
  return {
    GENERAL: sumQuotaAmounts(rules.filter((rule) => rule.quotaClass === 'GENERAL').map((rule) => rule.allowance)),
    DEDICATED: sumQuotaAmounts(rules.filter((rule) => rule.quotaClass === 'DEDICATED').map((rule) => rule.allowance)),
  };
}

export function planAllowance(plan: SubscriptionPlan): string {
  return sumQuotaAmounts(quotaRulesForPlan(plan).map((rule) => rule.allowance));
}
