import { useState } from 'react';
import {
  IconAlertCircle,
  IconArrowDownRight,
  IconCalendarRepeat,
  IconChevronDown,
  IconClockHour4,
  IconCoins,
  IconEqual,
  IconRefresh,
  IconShieldLock,
  IconStack2,
  IconTicket,
  IconWallet,
} from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { useSelectedProjectId } from '@/stores/projectStore';
import { DEFAULT_CREDIT_DISPLAY_NAME } from '@/lib/accounting';
import { cn } from '@/lib/utils';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import {
  type ProjectWalletComparison,
  type SubscriptionAllowanceBucket,
  type UserSubscription,
  useMyProjectBalance,
  useMySubscriptions,
} from '@/features/billing/data';
import { FundingOrderStrip } from '@/features/billing/funding-order-strip';
import { bucketTotalsByClass, spendableAllowanceBuckets } from '@/features/billing/quota-buckets';
import { useGeneralSettings } from '@/features/system/data/system';
import { RedeemCodeDialog } from './components/redeem-code-dialog';

function amount(creditDisplayName: string, value?: string) {
  if (!value) return '—';
  return `${creditDisplayName} ${value.includes('.') ? value.replace(/0+$/, '').replace(/\.$/, '') : value}`;
}

function date(value: string, locale: string) {
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime())
    ? value
    : new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(parsed);
}

function cadence(subscription: UserSubscription, t: ReturnType<typeof useTranslation>['t']) {
  return t(`billing.interval.${subscription.intervalUnit.toLowerCase()}`, { count: subscription.intervalCount });
}

const SUBSCRIPTION_ACCENTS = ['bg-chart-1', 'bg-chart-2', 'bg-chart-3', 'bg-chart-4', 'bg-chart-5', 'bg-chart-6'] as const;

function subscriptionAccent(id: string) {
  const hash = [...id].reduce((value, character) => (value * 31 + character.charCodeAt(0)) >>> 0, 0);
  return SUBSCRIPTION_ACCENTS[hash % SUBSCRIPTION_ACCENTS.length];
}

function allowancePercent(balance: Pick<SubscriptionAllowanceBucket, 'grantedAllowance' | 'remainingAllowance'>) {
  const granted = Number(balance.grantedAllowance);
  const remaining = Number(balance.remainingAllowance);
  if (!Number.isFinite(granted) || granted <= 0 || !Number.isFinite(remaining)) return 0;
  return Math.min(100, Math.max(0, (remaining / granted) * 100));
}

function subscriptionTime(subscription: UserSubscription) {
  const start = new Date(subscription.currentPeriodStart).getTime();
  const end = new Date(subscription.currentPeriodEnd).getTime();
  if (!Number.isFinite(start) || !Number.isFinite(end) || end <= start) {
    return { elapsedPercent: 0, remainingMs: null };
  }
  const now = Date.now();
  return {
    elapsedPercent: Math.min(100, Math.max(0, ((now - start) / (end - start)) * 100)),
    remainingMs: end - now,
  };
}

function remainingTimeLabel(remainingMs: number | null, t: ReturnType<typeof useTranslation>['t']) {
  if (remainingMs == null) return t('billing.subscription.timeUnknown');
  if (remainingMs <= 0) return t('billing.subscription.expired');
  const hours = Math.ceil(remainingMs / (60 * 60 * 1000));
  if (hours < 48) return t('billing.subscription.hoursRemaining', { count: hours });
  return t('billing.subscription.daysRemaining', { count: Math.ceil(hours / 24) });
}

function subscriptionStatus(status: string, t: ReturnType<typeof useTranslation>['t']) {
  return t(`billing.subscription.status.${status}`, { defaultValue: status });
}

function migrationStatus(value: string, t: ReturnType<typeof useTranslation>['t']) {
  const knownStatuses = new Set(['project_wallet_uninitialized', 'different', 'match', 'missing_owner', 'ambiguous_owner']);
  return knownStatuses.has(value) ? t(`wallet.projectMigration.status.${value}`) : value;
}

function projectWalletStatus(value: string, t: ReturnType<typeof useTranslation>['t']) {
  const knownStatuses = new Set(['uninitialized', 'active', 'suspended', 'closed']);
  return knownStatuses.has(value.toLowerCase()) ? t(`wallet.projectMigration.walletStatus.${value.toLowerCase()}`) : value;
}

export default function WalletPage() {
  const { t, i18n } = useTranslation();
  const selectedProjectID = useSelectedProjectId();
  const generalSettingsQuery = useGeneralSettings();
  const creditDisplayName = generalSettingsQuery.data?.creditDisplayName?.trim() || DEFAULT_CREDIT_DISPLAY_NAME;
  const [redeemOpen, setRedeemOpen] = useState(false);
  const [expandedBuckets, setExpandedBuckets] = useState<Set<string>>(() => new Set());
  const balanceQuery = useMyProjectBalance();
  const subscriptionsQuery = useMySubscriptions();
  const balance = balanceQuery.data?.myProjectBalance;
  const subscriptions = subscriptionsQuery.data?.mySubscriptions || [];
  const visibleSubscriptionStatuses = new Set(['active', 'paused', 'expired']);
  const visibleSubscriptions = subscriptions.filter((item) => visibleSubscriptionStatuses.has(item.status.toLowerCase()));
  const bucketsBySubscription = visibleSubscriptions
    .map((subscription) => ({ subscription, buckets: spendableAllowanceBuckets(subscription) }))
    .filter((group) => group.buckets.length > 0);
  const activeBuckets = bucketsBySubscription.flatMap((group) => group.buckets);
  const bucketTotals = bucketTotalsByClass(activeBuckets);
  const loading = balanceQuery.isLoading || subscriptionsQuery.isLoading;
  const error = balanceQuery.error || subscriptionsQuery.error;

  return (
    <>
      <Header fixed>
        <div className='flex min-w-0 flex-1 items-center justify-between gap-4'>
          <div className='min-w-0'>
            <h2 className='flex items-center gap-2 text-xl font-bold tracking-tight'>
              <IconWallet className='text-emerald-600' size={22} />
              {t('wallet.title')}
            </h2>
            <p className='text-muted-foreground truncate text-sm'>{t('wallet.description')}</p>
          </div>
          <Button
            className='shrink-0'
            aria-label={t('wallet.redeem.action')}
            onClick={() => setRedeemOpen(true)}
            disabled={!selectedProjectID}
          >
            <IconTicket />
            <span className='hidden sm:inline'>{t('wallet.redeem.action')}</span>
          </Button>
        </div>
      </Header>
      <Main className='space-y-5 pb-10'>
        {error && (
          <Alert variant='destructive'>
            <IconAlertCircle />
            <AlertTitle>{t('wallet.errorTitle')}</AlertTitle>
            <AlertDescription className='flex flex-wrap items-center justify-between gap-3'>
              <span>{t('wallet.error')}</span>
              <Button
                variant='outline'
                size='sm'
                onClick={() => {
                  balanceQuery.refetch();
                  subscriptionsQuery.refetch();
                }}
              >
                <IconRefresh /> {t('billing.retry')}
              </Button>
            </AlertDescription>
          </Alert>
        )}

        <section className='grid gap-3 sm:grid-cols-2 xl:grid-cols-4'>
          {[
            {
              label: t('billing.summary.available'),
              value: balance?.availableBalance,
              Icon: IconWallet,
              tone: 'text-foreground',
              reserved: balance?.reservedBalance,
            },
            {
              label: t('billing.summary.stationCredit', { name: creditDisplayName }),
              value: balance?.creditBalance,
              Icon: IconCoins,
              tone: 'text-foreground',
            },
            {
              label: t('billing.summary.generalQuota'),
              value: balance?.generalSubscriptionBalance ?? bucketTotals.GENERAL,
              Icon: IconCalendarRepeat,
              tone: 'text-foreground',
            },
            {
              label: t('billing.summary.dedicatedQuota'),
              value: balance?.dedicatedSubscriptionBalance ?? bucketTotals.DEDICATED,
              Icon: IconStack2,
              tone: 'text-foreground',
            },
          ].map(({ label, value, Icon, tone, reserved }) => (
            <Card key={label} className='gap-2 py-4 shadow-none'>
              <CardHeader className='px-4'>
                <CardDescription className='flex items-center gap-2 text-xs font-medium tracking-wide uppercase'>
                  <Icon className={tone} size={16} /> {label}
                </CardDescription>
                {loading ? (
                  <Skeleton className='h-7 w-32' />
                ) : (
                  <CardTitle className='font-mono text-xl tabular-nums'>{amount(creditDisplayName, String(value || ''))}</CardTitle>
                )}
              </CardHeader>
              {reserved != null && (
                <CardContent className='px-4'>
                  <div className='text-muted-foreground flex items-center gap-1.5 border-t border-dashed pt-2.5 text-[11px]'>
                    <IconShieldLock size={13} />
                    <span>{t('billing.summary.reserved')}</span>
                    <span className='ml-auto font-mono tabular-nums'>{amount(creditDisplayName, reserved)}</span>
                  </div>
                </CardContent>
              )}
            </Card>
          ))}
        </section>

        <FundingOrderStrip creditDisplayName={creditDisplayName} />

        <div className='space-y-5'>
          <Card className='gap-4 py-5 shadow-none'>
            <CardHeader className='px-5'>
              <div className='flex items-start justify-between gap-3'>
                <div>
                  <CardTitle>{t('wallet.quotaBucketsTitle')}</CardTitle>
                  <CardDescription className='text-pretty'>{t('wallet.quotaBucketsDescription')}</CardDescription>
                </div>
                {!!activeBuckets.length && <Badge variant='secondary'>{activeBuckets.length}</Badge>}
              </div>
            </CardHeader>
            <CardContent className='space-y-4 px-5'>
              {loading ? (
                <Skeleton className='h-36 w-full' />
              ) : bucketsBySubscription.length ? (
                <div className='space-y-5'>
                  {bucketsBySubscription.map(({ subscription, buckets }) => {
                    const accent = subscriptionAccent(subscription.id);
                    const remainingPercent = allowancePercent(subscription);
                    const time = subscriptionTime(subscription);
                    const modelsExpanded = expandedBuckets.has(subscription.id);
                    const status = subscription.status.toLowerCase();
                    const grantedAccessPlanNames = subscription.grantedAccessPlans.map((accessPlan) => accessPlan.name);
                    const grantedScopeNames = grantedAccessPlanNames.length ? grantedAccessPlanNames : subscription.grantedGroupNames;

                    return (
                      <section key={subscription.id} className='space-y-2.5' aria-labelledby={`subscription-${subscription.id}`}>
                        <article className='bg-muted/15 relative grid min-h-24 grid-cols-1 items-start gap-x-4 gap-y-3 overflow-hidden rounded-md border border-dashed px-4 py-3 sm:grid-cols-[minmax(0,1fr)_auto] lg:grid-cols-[minmax(10rem,.75fr)_minmax(15rem,1.4fr)_auto] lg:items-center lg:gap-x-5'>
                          <div className='flex min-w-0 items-center gap-2'>
                            <h3 id={`subscription-${subscription.id}`} className='min-w-0 font-semibold break-words sm:truncate'>
                              {subscription.plan.name}
                            </h3>
                            <Badge
                              variant={status === 'active' ? 'secondary' : 'outline'}
                              className={cn(
                                'shrink-0 text-[10px] uppercase',
                                status === 'paused' && 'border-amber-500/45 text-amber-700 dark:text-amber-300',
                                status === 'expired' && 'text-muted-foreground'
                              )}
                            >
                              {subscriptionStatus(status, t)}
                            </Badge>
                          </div>

                          <div className='min-w-0 space-y-1 sm:col-span-2 lg:col-span-1 lg:col-start-2 lg:row-start-1'>
                            <p className='text-sm break-words' title={grantedScopeNames.join('、')}>
                              <span className='text-muted-foreground'>{t('billing.plans.grantedGroup')}: </span>
                              {grantedScopeNames.length ? grantedScopeNames.join('、') : t('billing.subscription.noGrantedGroup')}
                              {' · '}
                              <button
                                type='button'
                                aria-expanded={modelsExpanded}
                                aria-controls={`subscription-models-${subscription.id}`}
                                className='text-muted-foreground hover:text-foreground focus-visible:ring-ring inline-flex items-center gap-1 rounded-sm underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:outline-none'
                                onClick={() =>
                                  setExpandedBuckets((current) => {
                                    const next = new Set(current);
                                    if (next.has(subscription.id)) next.delete(subscription.id);
                                    else next.add(subscription.id);
                                    return next;
                                  })
                                }
                              >
                                {t('billing.subscription.modelGrants', { count: subscription.grantedModelIDs.length })}
                                <IconChevronDown className={cn('size-3.5 transition-transform', modelsExpanded && 'rotate-180')} />
                              </button>
                            </p>
                          </div>

                          <dl className='text-left sm:col-start-2 sm:row-start-1 sm:text-right lg:col-start-3'>
                            <div>
                              <dt className='text-muted-foreground text-[10px] font-medium tracking-wide uppercase'>
                                {t('billing.subscription.remaining')}
                              </dt>
                              <dd className='font-mono text-lg font-semibold tabular-nums'>
                                {amount(creditDisplayName, subscription.remainingAllowance)}
                              </dd>
                            </div>
                            <div className='text-muted-foreground mt-0.5 font-mono text-[11px] tabular-nums'>
                              {amount(creditDisplayName, subscription.consumedAllowance)} /{' '}
                              {amount(creditDisplayName, subscription.grantedAllowance)}
                            </div>
                          </dl>

                          <div className='space-y-1.5 sm:col-span-2 lg:col-span-3'>
                            <div className='grid grid-cols-1 gap-1 text-[11px] sm:grid-cols-[minmax(0,1fr)_auto_minmax(0,1fr)] sm:items-center sm:gap-3'>
                              <span className='text-muted-foreground flex min-w-0 items-center gap-1.5 whitespace-nowrap'>
                                <IconClockHour4 className='size-3.5 shrink-0' />
                                <span>{date(subscription.currentPeriodStart, i18n.language)}</span>
                              </span>
                              <span className='text-foreground font-medium sm:text-center'>
                                {remainingTimeLabel(time.remainingMs, t)} · {cadence(subscription, t)}
                              </span>
                              <span className='text-muted-foreground whitespace-nowrap sm:text-right'>
                                {date(subscription.currentPeriodEnd, i18n.language)}
                              </span>
                            </div>
                            <div
                              role='progressbar'
                              aria-label={t('billing.subscription.timeProgress')}
                              aria-valuemin={0}
                              aria-valuemax={100}
                              aria-valuenow={Math.round(time.elapsedPercent)}
                              className='bg-muted h-1.5 overflow-hidden rounded-sm'
                            >
                              <span
                                className='block h-full bg-sky-500 transition-[width] duration-300'
                                style={{ width: `${time.elapsedPercent}%` }}
                              />
                            </div>
                          </div>

                          {modelsExpanded && (
                            <div
                              id={`subscription-models-${subscription.id}`}
                              className='bg-muted/35 flex flex-wrap gap-1.5 rounded-md border border-dashed p-2.5 sm:col-span-2 lg:col-span-3'
                            >
                              {subscription.grantedModelIDs.length ? (
                                subscription.grantedModelIDs.map((modelID) => (
                                  <Badge key={modelID} variant='outline' className='bg-background font-mono text-[10px] font-normal'>
                                    {modelID}
                                  </Badge>
                                ))
                              ) : (
                                <span className='text-muted-foreground text-xs'>{t('billing.subscription.noGrantedModels')}</span>
                              )}
                            </div>
                          )}

                          <div
                            role='progressbar'
                            aria-label={`${subscription.plan.name} ${t('billing.subscription.remaining')}`}
                            aria-valuemin={0}
                            aria-valuemax={100}
                            aria-valuenow={Math.round(remainingPercent)}
                            className='bg-muted absolute right-0 bottom-0 left-0 h-1'
                          >
                            <span
                              className={cn('block h-full transition-[width] duration-300', accent)}
                              style={{ width: `${remainingPercent}%` }}
                            />
                          </div>
                        </article>
                        <div className='space-y-2'>
                          {buckets.map((bucket) => (
                            <WalletBucketCard
                              key={bucket.id}
                              bucket={bucket}
                              locale={i18n.language}
                              creditDisplayName={creditDisplayName}
                              modelsExpanded={expandedBuckets.has(bucket.id)}
                              onToggleModels={() =>
                                setExpandedBuckets((current) => {
                                  const next = new Set(current);
                                  if (next.has(bucket.id)) next.delete(bucket.id);
                                  else next.add(bucket.id);
                                  return next;
                                })
                              }
                            />
                          ))}
                        </div>
                      </section>
                    );
                  })}
                </div>
              ) : (
                <div className='text-muted-foreground py-8 text-center text-sm'>{t('wallet.noQuotaBuckets')}</div>
              )}
            </CardContent>
          </Card>

          <Card className='gap-4 py-5 shadow-none'>
            <CardHeader className='px-5'>
              <CardTitle>{t('billing.ledger.title')}</CardTitle>
              <CardDescription>{t('wallet.ledgerDescription')}</CardDescription>
            </CardHeader>
            <CardContent className='px-0'>
              {loading ? (
                <div className='space-y-2 px-5'>
                  {[1, 2, 3].map((item) => (
                    <Skeleton key={item} className='h-10 w-full' />
                  ))}
                </div>
              ) : !balance?.ledgerEntries.length ? (
                <div className='text-muted-foreground py-12 text-center text-sm'>{t('billing.ledger.empty')}</div>
              ) : (
                <div className='overflow-x-auto'>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead className='pl-5'>{t('billing.ledger.type')}</TableHead>
                        <TableHead>{t('billing.ledger.descriptionColumn')}</TableHead>
                        <TableHead>{t('billing.ledger.date')}</TableHead>
                        <TableHead className='pr-5 text-right'>{t('billing.ledger.amount')}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {balance.ledgerEntries.map((entry) => (
                        <TableRow key={entry.id}>
                          <TableCell className='pl-5'>
                            <Badge variant='outline'>{entry.entryType}</Badge>
                          </TableCell>
                          <TableCell>{entry.description || '—'}</TableCell>
                          <TableCell className='text-muted-foreground text-xs whitespace-nowrap'>
                            {date(entry.createdAt, i18n.language)}
                          </TableCell>
                          <TableCell className='pr-5 text-right font-mono font-medium tabular-nums'>
                            {amount(creditDisplayName, entry.amount)}
                          </TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                </div>
              )}
            </CardContent>
          </Card>
        </div>
      </Main>
      <RedeemCodeDialog open={redeemOpen} onOpenChange={setRedeemOpen} creditDisplayName={creditDisplayName} />
    </>
  );
}

function WalletBucketCard({
  bucket,
  locale,
  creditDisplayName,
  modelsExpanded,
  onToggleModels,
}: {
  bucket: SubscriptionAllowanceBucket;
  locale: string;
  creditDisplayName: string;
  modelsExpanded: boolean;
  onToggleModels: () => void;
}) {
  const { t } = useTranslation();
  const isDedicated = bucket.quotaClass === 'DEDICATED';
  const remainingPercent = allowancePercent(bucket);
  const sourceLabel = t(`billing.bucket.source.${bucket.sourceType.toLowerCase()}`, { defaultValue: bucket.sourceType });
  const statusLabel = t(`billing.bucket.status.${bucket.status.toLowerCase()}`, { defaultValue: bucket.status });
  const modelIDs = bucket.modelIDs || [];
  const scopeNames = bucket.accessPlans?.map((accessPlan) => accessPlan.name) || [];
  const expiryTimestamp = new Date(bucket.expiresAt).getTime();
  const remainingUntilExpiry = expiryTimestamp - Date.now();
  const expiringSoon = Number.isFinite(expiryTimestamp) && remainingUntilExpiry > 0 && remainingUntilExpiry <= 3 * 24 * 60 * 60 * 1000;
  const accent = subscriptionAccent(bucket.id);

  return (
    <article className='bg-card relative overflow-hidden rounded-md border px-4 py-3.5'>
      <div className='grid gap-3 md:grid-cols-[minmax(0,1fr)_auto] md:items-start'>
        <div className='min-w-0'>
          <div className='flex flex-wrap items-center gap-1.5'>
            <h4 className='font-semibold text-balance'>{bucket.name}</h4>
            <Badge variant={isDedicated ? 'secondary' : 'outline'} className='text-[10px] font-normal'>
              {t(`billing.quotaClass.${bucket.quotaClass.toLowerCase()}`)}
            </Badge>
            <Badge variant='outline' className='text-[10px] font-normal'>
              {sourceLabel}
            </Badge>
            <Badge variant='outline' className='text-[10px] font-normal'>
              {statusLabel}
            </Badge>
            {expiringSoon && (
              <Badge variant='outline' className='text-destructive border-destructive/40 text-[10px] font-normal'>
                {t('wallet.bucket.expiringSoon')}
              </Badge>
            )}
          </div>
          <p className='text-muted-foreground mt-1.5 max-w-3xl text-xs text-pretty'>
            {isDedicated
              ? t('wallet.bucket.dedicatedScope', {
                  scopes: scopeNames.join(', ') || t('billing.bucket.scopeSnapshot'),
                  count: modelIDs.length,
                })
              : t('wallet.bucket.generalScope')}
          </p>
        </div>

        <div className='md:min-w-48 md:text-right'>
          <p className='text-muted-foreground text-[10px] font-medium tracking-wide uppercase'>{t('billing.bucket.remaining')}</p>
          <p className='font-mono text-lg font-semibold tabular-nums'>{amount(creditDisplayName, bucket.remainingAllowance)}</p>
          <p className='text-muted-foreground font-mono text-[11px] tabular-nums'>
            {t('wallet.bucket.ofGranted', { granted: amount(creditDisplayName, bucket.grantedAllowance) })}
          </p>
        </div>
      </div>

      <dl className='mt-3 grid gap-2 border-t border-dashed pt-3 text-xs sm:grid-cols-3'>
        <div>
          <dt className='text-muted-foreground'>{t('billing.bucket.reserved')}</dt>
          <dd className='mt-0.5 font-mono tabular-nums'>{amount(creditDisplayName, bucket.reservedAllowance)}</dd>
        </div>
        <div>
          <dt className='text-muted-foreground'>{t('billing.bucket.expires')}</dt>
          <dd className='mt-0.5 flex items-center gap-1.5 tabular-nums sm:justify-start'>
            <IconClockHour4 className='text-muted-foreground size-3.5 shrink-0' />
            {date(bucket.expiresAt, locale)}
          </dd>
        </div>
        <div>
          <dt className='text-muted-foreground'>{t('billing.bucket.period')}</dt>
          <dd className='mt-0.5 tabular-nums'>{date(bucket.periodStart, locale)}</dd>
        </div>
      </dl>

      {isDedicated && (
        <div className='mt-3 border-t border-dashed pt-2'>
          <Button
            type='button'
            variant='ghost'
            className='-ml-2 min-h-10 px-2 text-xs active:scale-[0.96]'
            aria-expanded={modelsExpanded}
            aria-controls={`bucket-models-${bucket.id}`}
            onClick={onToggleModels}
          >
            {t('wallet.bucket.models', { count: modelIDs.length })}
            <IconChevronDown
              className={cn('size-3.5 transition-transform duration-200 motion-reduce:transition-none', modelsExpanded && 'rotate-180')}
            />
          </Button>
          {modelsExpanded && (
            <div id={`bucket-models-${bucket.id}`} className='bg-muted/25 flex flex-wrap gap-1.5 rounded-sm border border-dashed p-2.5'>
              {modelIDs.length ? (
                modelIDs.map((modelID) => (
                  <Badge key={modelID} variant='outline' className='bg-background font-mono text-[10px] font-normal'>
                    {modelID}
                  </Badge>
                ))
              ) : (
                <span className='text-muted-foreground text-xs'>{t('wallet.bucket.noModels')}</span>
              )}
            </div>
          )}
        </div>
      )}

      <div
        role='progressbar'
        aria-label={`${bucket.name} ${t('billing.bucket.remaining')}`}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(remainingPercent)}
        className='bg-muted absolute right-0 bottom-0 left-0 h-1'
      >
        <span
          className={cn('block h-full transition-[width] duration-300 motion-reduce:transition-none', accent)}
          style={{ width: `${remainingPercent}%` }}
        />
      </div>
    </article>
  );
}

export function UserProjectMigration({
  comparison,
  creditDisplayName,
  walletStatus,
  loading,
  error,
  retry,
}: {
  comparison?: ProjectWalletComparison;
  creditDisplayName: string;
  walletStatus?: string;
  loading: boolean;
  error: unknown;
  retry: () => void;
}) {
  const { t } = useTranslation();
  const delta = Number(comparison?.availableDelta || '0');
  const matches = Number.isFinite(delta) && delta === 0;

  return (
    <section className='rounded-lg border border-dashed border-amber-500/45 bg-amber-500/[0.035] p-4 sm:p-5'>
      <div className='flex flex-wrap items-start justify-between gap-3'>
        <div>
          <h3 className='font-semibold'>{t('wallet.projectMigration.title')}</h3>
          <p className='text-muted-foreground mt-1 max-w-3xl text-sm'>{t('wallet.projectMigration.description')}</p>
        </div>
        <Badge variant='outline' className='border-amber-500/50 text-amber-800 dark:text-amber-300'>
          {t('wallet.projectMigration.badge')}
        </Badge>
      </div>

      <div className='mt-4'>
        {loading ? (
          <Skeleton className='h-28 w-full' />
        ) : error ? (
          <Alert variant='destructive'>
            <IconAlertCircle />
            <AlertTitle>{t('wallet.projectMigration.errorTitle')}</AlertTitle>
            <AlertDescription className='flex flex-wrap items-center justify-between gap-2'>
              <span>{t('wallet.projectMigration.error')}</span>
              <Button size='sm' variant='outline' onClick={retry}>
                <IconRefresh /> {t('billing.retry')}
              </Button>
            </AlertDescription>
          </Alert>
        ) : !comparison ? (
          <div className='text-muted-foreground rounded-md border border-dashed px-4 py-5 text-sm'>
            {t('wallet.projectMigration.unavailable')}
          </div>
        ) : (
          <div className='bg-background/75 overflow-hidden rounded-md border'>
            <div className='grid sm:grid-cols-[1fr_auto_1fr]'>
              <div className='border-b p-4 sm:border-r sm:border-b-0'>
                <p className='text-muted-foreground text-xs font-medium tracking-wide uppercase'>{t('wallet.projectMigration.official')}</p>
                <p className='mt-1 font-mono text-xl font-semibold text-emerald-700 tabular-nums dark:text-emerald-400'>
                  {amount(creditDisplayName, comparison.legacyAvailableBalance)}
                </p>
                <p className='text-muted-foreground mt-1 text-xs'>{t('wallet.projectMigration.spendable')}</p>
              </div>
              <div className='bg-muted/30 flex items-center justify-center border-b px-4 py-2 sm:border-r sm:border-b-0'>
                <div className='text-center'>
                  <div
                    className={cn(
                      'flex items-center justify-center gap-1 font-mono text-sm font-semibold tabular-nums',
                      matches ? 'text-emerald-700 dark:text-emerald-400' : 'text-amber-700 dark:text-amber-400'
                    )}
                  >
                    {matches ? <IconEqual size={16} /> : <IconArrowDownRight size={16} />}
                    {amount(creditDisplayName, comparison.availableDelta)}
                  </div>
                  <p className='text-muted-foreground mt-0.5 text-[10px] font-medium tracking-wide uppercase'>
                    {t('wallet.projectMigration.delta')}
                  </p>
                </div>
              </div>
              <div className='p-4'>
                <p className='text-muted-foreground text-xs font-medium tracking-wide uppercase'>{t('wallet.projectMigration.shadow')}</p>
                <p className='mt-1 font-mono text-xl font-semibold text-amber-800 tabular-nums dark:text-amber-300'>
                  {amount(creditDisplayName, comparison.projectAvailableBalance)}
                </p>
                <p className='text-muted-foreground mt-1 text-xs'>{t('wallet.projectMigration.observationOnly')}</p>
              </div>
            </div>
            <div className='grid gap-2 border-t border-dashed px-4 py-3 text-xs sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center'>
              <span className='text-muted-foreground'>{t('wallet.projectMigration.noFundsMoved')}</span>
              <div className='flex min-w-0 flex-wrap items-center gap-1.5 sm:justify-end'>
                <Badge variant='outline'>{migrationStatus(comparison.status, t)}</Badge>
                {walletStatus && <Badge variant='outline'>{projectWalletStatus(walletStatus, t)}</Badge>}
                <span className='text-muted-foreground min-w-0 basis-full sm:basis-auto'>
                  {t('wallet.projectMigration.projectID')}: <span className='font-mono break-all'>{comparison.projectID}</span>
                </span>
              </div>
            </div>
          </div>
        )}
      </div>
    </section>
  );
}
