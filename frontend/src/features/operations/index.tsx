import { useEffect, useState } from 'react';
import {
  AlertTriangle,
  CircleDollarSign,
  Clock3,
  Gauge,
  RefreshCw,
  ReceiptText,
  ServerCog,
  ShieldAlert,
  Sigma,
  SlidersHorizontal,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Area, CartesianGrid, ComposedChart, Legend, Line, ResponsiveContainer, Tooltip, XAxis, YAxis } from 'recharts';
import { DEFAULT_ACCOUNTING_CURRENCY_CODE } from '@/lib/accounting';
import { formatCurrencyValue } from '@/lib/currency-format';
import { cn } from '@/lib/utils';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Skeleton } from '@/components/ui/skeleton';
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { useGeneralSettings } from '@/features/system/data/system';
import { AnalyticsState, FlowAnalytics, ModelAnalytics, UserAnalytics } from './analytics-views';
import {
  DEFAULT_OPERATIONS_TREND_SERIES,
  OPERATIONS_TREND_STORAGE_KEY,
  hasVisibleTrendAxis,
  loadOperationsTrendSeries,
  type OperationsTrendSeriesId,
} from './chart-settings';
import {
  type OperationsChannel,
  type OperationsMoneyMetric,
  type OperationsRouteHealth,
  useOperationsFlow,
  useOperationsLedger,
} from './data';

const integer = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
const percent = new Intl.NumberFormat(undefined, { style: 'percent', maximumFractionDigits: 1 });

const trendSeriesGroups = [
  {
    id: 'traffic',
    series: [
      { id: 'customerRequests', label: 'operations.kpi.requests', color: '#0284c7' },
      { id: 'upstreamAttempts', label: 'operations.attempts', color: 'var(--muted-foreground)' },
      { id: 'retryCount', label: 'operations.performance.retries', color: '#8b5cf6' },
    ],
  },
  {
    id: 'finance',
    series: [
      { id: 'recordedUpstreamCost', label: 'operations.summary.cost', color: '#f59e0b' },
      { id: 'recognizedUsageRevenue', label: 'operations.summary.revenue', color: '#0ea5e9' },
      { id: 'grossProfit', label: 'operations.summary.profit', color: '#10b981' },
    ],
  },
  {
    id: 'reliability',
    series: [
      { id: 'requestFailureRate', label: 'operations.trend.requestFailureRate', color: 'var(--destructive)' },
      { id: 'failureRate', label: 'operations.trend.upstreamFailureRate', color: '#f97316' },
    ],
  },
] as const satisfies ReadonlyArray<{
  id: string;
  series: ReadonlyArray<{ id: OperationsTrendSeriesId; label: string; color: string }>;
}>;

const attemptsSeries: OperationsTrendSeriesId[] = ['customerRequests', 'upstreamAttempts', 'retryCount'];
const moneySeries: OperationsTrendSeriesId[] = ['recordedUpstreamCost', 'recognizedUsageRevenue', 'grossProfit'];
const failureRateSeries: OperationsTrendSeriesId[] = ['requestFailureRate', 'failureRate'];

function TrendSeriesMenu({
  visibleSeries,
  onToggle,
  onShowAll,
  onReset,
}: {
  visibleSeries: ReadonlySet<OperationsTrendSeriesId>;
  onToggle: (series: OperationsTrendSeriesId) => void;
  onShowAll: () => void;
  onReset: () => void;
}) {
  const { t } = useTranslation();

  return (
    <DropdownMenu modal={false}>
      <DropdownMenuTrigger asChild>
        <Button variant='outline' size='sm' className='shrink-0' aria-label={t('operations.trend.metricsLabel')}>
          <SlidersHorizontal className='size-4' />
          {t('operations.trend.metricsCount', {
            visible: visibleSeries.size,
            total: DEFAULT_OPERATIONS_TREND_SERIES.length,
          })}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align='end' className='w-[min(19rem,calc(100vw-2rem))]'>
        {trendSeriesGroups.map((group, groupIndex) => (
          <div key={group.id}>
            {groupIndex > 0 && <DropdownMenuSeparator />}
            <DropdownMenuLabel className='text-muted-foreground text-[11px] tracking-wide uppercase'>
              {t(`operations.trend.groups.${group.id}`)}
            </DropdownMenuLabel>
            {group.series.map((series) => (
              <DropdownMenuCheckboxItem
                key={series.id}
                checked={visibleSeries.has(series.id)}
                onCheckedChange={() => onToggle(series.id)}
                onSelect={(event) => event.preventDefault()}
              >
                <span className='size-2.5 shrink-0 rounded-[2px]' style={{ backgroundColor: series.color }} aria-hidden='true' />
                <span>{t(series.label)}</span>
              </DropdownMenuCheckboxItem>
            ))}
          </div>
        ))}
        <DropdownMenuSeparator />
        <div className='flex items-center justify-between gap-2 p-1'>
          <Button variant='ghost' size='sm' className='h-7 px-2 text-xs' onClick={onShowAll}>
            {t('operations.trend.showAll')}
          </Button>
          <Button variant='ghost' size='sm' className='h-7 px-2 text-xs' onClick={onReset}>
            {t('operations.trend.reset')}
          </Button>
        </div>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function QualityBadge({ metric }: { metric: OperationsMoneyMetric }) {
  const { t } = useTranslation();
  return (
    <Badge
      variant='outline'
      className={cn(
        'font-mono text-[10px] tracking-wide',
        metric.quality === 'EXACT' && 'border-emerald-500/30 text-emerald-600 dark:text-emerald-400',
        metric.quality === 'PARTIAL' && 'border-amber-500/40 text-amber-700 dark:text-amber-400',
        metric.quality === 'UNAVAILABLE' && 'text-muted-foreground'
      )}
    >
      {t(`operations.quality.${metric.quality.toLowerCase()}`)}
    </Badge>
  );
}

function MoneyValue({
  metric,
  currencyCode,
  locale,
  tone,
}: {
  metric: OperationsMoneyMetric;
  currencyCode: string;
  locale: string;
  tone?: 'profit' | 'cost';
}) {
  const { t } = useTranslation();
  return (
    <div className='space-y-1.5'>
      <div
        className={cn(
          'font-mono text-2xl font-semibold tabular-nums',
          tone === 'profit' && metric.amount != null && metric.amount >= 0 && 'text-emerald-600 dark:text-emerald-400',
          tone === 'profit' && metric.amount != null && metric.amount < 0 && 'text-destructive',
          tone === 'cost' && 'text-amber-700 dark:text-amber-400'
        )}
      >
        {metric.amount == null ? '—' : formatCurrencyValue(metric.amount, currencyCode, locale, { maximumFractionDigits: 4 })}
      </div>
      <div className='flex items-center gap-2'>
        <QualityBadge metric={metric} />
        {metric.quality === 'PARTIAL' && metric.coverageRate != null && (
          <span className='text-muted-foreground text-xs'>{percent.format(metric.coverageRate)}</span>
        )}
      </div>
      {metric.reason && <span className='sr-only'>{t(`operations.reasons.${metric.reason}`)}</span>}
    </div>
  );
}

function LedgerSummary({
  cost,
  revenue,
  profit,
  currencyCode,
  locale,
}: {
  cost: OperationsMoneyMetric;
  revenue: OperationsMoneyMetric;
  profit: OperationsMoneyMetric;
  currencyCode: string;
  locale: string;
}) {
  const { t } = useTranslation();
  return (
    <section className='border-border bg-card grid overflow-hidden rounded-lg border lg:grid-cols-3'>
      {[
        { label: t('operations.summary.revenue'), metric: revenue, icon: ReceiptText },
        { label: t('operations.summary.cost'), metric: cost, icon: CircleDollarSign, tone: 'cost' as const },
        { label: t('operations.summary.profit'), metric: profit, icon: Sigma, tone: 'profit' as const },
      ].map((item, index) => (
        <div key={item.label} className={cn('p-5', index > 0 && 'border-border border-t lg:border-t-0 lg:border-l')}>
          <div className='text-muted-foreground mb-4 flex items-center gap-2 text-xs font-medium tracking-wide uppercase'>
            <item.icon className='size-4' />
            {item.label}
          </div>
          <MoneyValue metric={item.metric} currencyCode={currencyCode} locale={locale} tone={item.tone} />
          <p className='text-muted-foreground mt-3 min-h-8 text-xs leading-relaxed'>
            {item.metric.reason ? t(`operations.reasons.${item.metric.reason}`) : t('operations.reasons.COMPLETE_RECORDED_COST')}
          </p>
        </div>
      ))}
    </section>
  );
}

function HealthRail({ channel }: { channel: OperationsChannel }) {
  const success = channel.successRate ?? 0;
  const costCoverage = channel.costCoverageRate;
  return (
    <div
      className='flex w-20 flex-col gap-1'
      aria-label={`success ${channel.successRate == null ? 'N/A' : percent.format(success)}, cost coverage ${costCoverage == null ? 'N/A' : percent.format(costCoverage)}`}
    >
      <div className='bg-muted h-1 overflow-hidden rounded-sm'>
        {channel.upstreamAttempts > 0 && (
          <div
            className={cn('h-full', success >= 0.95 ? 'bg-emerald-500' : success >= 0.8 ? 'bg-amber-500' : 'bg-destructive')}
            style={{ width: `${success * 100}%` }}
          />
        )}
      </div>
      <div className='bg-muted h-1 overflow-hidden rounded-sm'>
        {costCoverage != null && (
          <div className={cn('h-full', costCoverage === 1 ? 'bg-sky-500' : 'bg-amber-500')} style={{ width: `${costCoverage * 100}%` }} />
        )}
      </div>
    </div>
  );
}

function isFresh(timestamp: string | null) {
  return timestamp != null && Date.now() - new Date(timestamp).getTime() <= 24 * 60 * 60 * 1000;
}

function ChannelTable({ channels, currencyCode, locale }: { channels: OperationsChannel[]; currencyCode: string; locale: string }) {
  const { t } = useTranslation();
  if (channels.length === 0) {
    return (
      <div className='text-muted-foreground border-border rounded-lg border border-dashed py-16 text-center text-sm'>
        {t('operations.empty')}
      </div>
    );
  }
  return (
    <div className='border-border overflow-x-auto rounded-lg border'>
      <table className='w-full min-w-[1720px] border-collapse text-sm'>
        <thead className='bg-muted/50 text-muted-foreground text-left text-[11px] tracking-wide uppercase'>
          <tr>
            <th className='bg-muted/80 sticky left-0 z-10 px-4 py-3'>{t('operations.table.channel')}</th>
            <th className='px-3 py-3'>{t('operations.table.rail')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.attempts')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.retries')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.requests')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.success')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.tokens')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.latency')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.cost')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.revenue')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.profit')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.coverage')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.quota')}</th>
            <th className='px-4 py-3 text-right'>{t('operations.table.activity')}</th>
          </tr>
        </thead>
        <tbody>
          {channels.map((channel) => (
            <tr key={channel.channelId} className='border-border hover:bg-muted/25 border-t transition-colors'>
              <td className='bg-background sticky left-0 z-10 px-4 py-3'>
                <div className='flex items-center gap-2'>
                  <div>
                    <div className='max-w-44 truncate font-medium'>{channel.channelName}</div>
                    <div className='mt-1 flex items-center gap-1.5'>
                      <Badge
                        variant='outline'
                        className={cn(
                          'px-1.5 py-0 text-[10px]',
                          channel.channelStatus === 'enabled' && 'border-emerald-500/30 text-emerald-600 dark:text-emerald-400'
                        )}
                      >
                        {t(`operations.status.${channel.channelStatus}`)}
                      </Badge>
                      <span className='text-muted-foreground font-mono text-[10px]'>
                        {channel.channelType} · #{channel.channelId}
                      </span>
                    </div>
                  </div>
                </div>
              </td>
              <td className='px-3 py-3'>
                <HealthRail channel={channel} />
              </td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>{integer.format(channel.upstreamAttempts)}</td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>{integer.format(channel.retryCount)}</td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>{integer.format(channel.customerRequests)}</td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>
                {channel.successRate == null ? '—' : percent.format(channel.successRate)}
              </td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>{integer.format(channel.totalTokens)}</td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>
                <div>{channel.averageLatencyMs == null ? '—' : `${Math.round(channel.averageLatencyMs)} ms`}</div>
                <div className='text-muted-foreground text-[10px]'>
                  TTFT {channel.averageTtftMs == null ? '—' : `${Math.round(channel.averageTtftMs)} ms`}
                </div>
                <div className='text-muted-foreground text-[10px]'>
                  TPS {channel.averageTps == null ? '—' : channel.averageTps.toFixed(1)}
                </div>
              </td>
              <td className='px-3 py-3 text-right'>
                <div className='font-mono tabular-nums'>
                  {channel.recordedUpstreamCost.amount == null
                    ? '—'
                    : formatCurrencyValue(channel.recordedUpstreamCost.amount, currencyCode, locale, { maximumFractionDigits: 4 })}
                </div>
                <QualityBadge metric={channel.recordedUpstreamCost} />
                <div className='text-muted-foreground mt-1 text-[10px]'>
                  {t('operations.table.perAttempt')}{' '}
                  {channel.costPerAttempt == null
                    ? '—'
                    : formatCurrencyValue(channel.costPerAttempt, currencyCode, locale, { maximumFractionDigits: 4 })}
                </div>
              </td>
              <td className='px-3 py-3 text-right'>
                <div className='font-mono tabular-nums'>
                  {channel.recognizedUsageRevenue.amount == null
                    ? '—'
                    : formatCurrencyValue(channel.recognizedUsageRevenue.amount, currencyCode, locale, { maximumFractionDigits: 4 })}
                </div>
                <span
                  title={
                    channel.recognizedUsageRevenue.reason ? t(`operations.reasons.${channel.recognizedUsageRevenue.reason}`) : undefined
                  }
                >
                  <QualityBadge metric={channel.recognizedUsageRevenue} />
                </span>
              </td>
              <td className='px-3 py-3 text-right'>
                <div
                  className={cn(
                    'font-mono tabular-nums',
                    channel.grossProfit.amount != null && channel.grossProfit.amount >= 0 && 'text-emerald-600 dark:text-emerald-400',
                    channel.grossProfit.amount != null && channel.grossProfit.amount < 0 && 'text-destructive'
                  )}
                >
                  {channel.grossProfit.amount == null
                    ? '—'
                    : formatCurrencyValue(channel.grossProfit.amount, currencyCode, locale, { maximumFractionDigits: 4 })}
                </div>
                <span title={channel.grossProfit.reason ? t(`operations.reasons.${channel.grossProfit.reason}`) : undefined}>
                  <QualityBadge metric={channel.grossProfit} />
                </span>
              </td>
              <td className='px-3 py-3 text-right font-mono text-xs tabular-nums'>
                <div>
                  {t('operations.table.costShort')}{' '}
                  {channel.costCoverageRate == null ? t('operations.na') : percent.format(channel.costCoverageRate)}
                </div>
                <div className='text-muted-foreground'>
                  {t('operations.table.billingShort')}{' '}
                  {channel.billingCoverageRate == null ? t('operations.na') : percent.format(channel.billingCoverageRate)}
                </div>
              </td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>
                <div>{channel.quotaRemaining == null ? '—' : `${channel.quotaRemaining} ${channel.quotaCurrency ?? ''}`}</div>
                <div
                  className={cn('mt-1 text-[10px]', isFresh(channel.quotaSnapshotAt) ? 'text-emerald-600' : 'text-amber-600')}
                  title={channel.quotaSnapshotAt ? new Date(channel.quotaSnapshotAt).toLocaleString() : undefined}
                >
                  {channel.quotaSnapshotAt
                    ? isFresh(channel.quotaSnapshotAt)
                      ? t('operations.freshness.fresh')
                      : t('operations.freshness.stale')
                    : t('operations.freshness.unknown')}
                </div>
                <div
                  className={cn('mt-1 text-[10px]', channel.observedPriceChangeCount > 0 && 'text-amber-600')}
                  title={channel.observedPricingAt ? new Date(channel.observedPricingAt).toLocaleString() : undefined}
                >
                  {t('operations.table.observedPrice')}: {channel.observedPricingSource ?? '—'}
                  {channel.observedPriceChangeCount > 0 ? ` · ${channel.observedPriceChangeCount} ${t('operations.table.changes')}` : ''}
                </div>
              </td>
              <td className='text-muted-foreground px-4 py-3 text-right text-xs'>
                <div>{channel.lastActivityAt ? new Date(channel.lastActivityAt).toLocaleString() : t('operations.never')}</div>
                <div className='mt-1 text-[10px]'>
                  {t('operations.table.probe')}:{' '}
                  {channel.lastProbeAt
                    ? isFresh(channel.lastProbeAt)
                      ? t('operations.freshness.fresh')
                      : t('operations.freshness.stale')
                    : t('operations.freshness.unknown')}
                </div>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function credentialLabel(identity: string | null) {
  if (!identity) return '—';
  const digest = identity.replace(/^sha256:/, '');
  return `sha256:…${digest.slice(-10)}`;
}

function RouteHealthTable({ rows }: { rows: OperationsRouteHealth[] }) {
  const { t } = useTranslation();
  return (
    <div className='border-border overflow-x-auto rounded-lg border'>
      <table className='w-full min-w-[920px] border-collapse text-sm'>
        <thead className='bg-muted/50 text-muted-foreground text-left text-[11px] tracking-wide uppercase'>
          <tr>
            <th className='px-4 py-3'>{t('operations.routeHealth.target')}</th>
            <th className='px-3 py-3'>{t('operations.routeHealth.credential')}</th>
            <th className='px-3 py-3'>{t('operations.routeHealth.status')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.attempts')}</th>
            <th className='px-3 py-3 text-right'>{t('operations.table.success')}</th>
            <th className='px-3 py-3'>{t('operations.performance.errors')}</th>
            <th className='px-4 py-3 text-right'>{t('operations.table.activity')}</th>
          </tr>
        </thead>
        <tbody>
          {rows.map((row) => (
            <tr
              key={`${row.channelId}:${row.actualModel}:${row.credentialIdentity ?? 'unattributed'}`}
              className='border-border hover:bg-muted/25 border-t transition-colors'
            >
              <td className='px-4 py-3'>
                <div className='font-medium'>{row.actualModel}</div>
                <div className='text-muted-foreground mt-0.5 text-xs'>
                  {row.channelName} · #{row.channelId}
                </div>
              </td>
              <td className='px-3 py-3 font-mono text-xs' title={row.credentialIdentity ?? t('operations.routeHealth.unattributed')}>
                {credentialLabel(row.credentialIdentity)}
              </td>
              <td className='px-3 py-3'>
                <Badge
                  variant='outline'
                  className={cn(
                    'font-mono text-[10px]',
                    row.healthStatus === 'healthy' && 'border-emerald-500/30 text-emerald-600 dark:text-emerald-400',
                    row.healthStatus === 'degraded' && 'border-amber-500/40 text-amber-700 dark:text-amber-400',
                    row.healthStatus === 'unhealthy' && 'border-destructive/40 text-destructive'
                  )}
                >
                  {t(`operations.routeHealth.statuses.${row.healthStatus}`)}
                </Badge>
              </td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>{integer.format(row.upstreamAttempts)}</td>
              <td className='px-3 py-3 text-right font-mono tabular-nums'>
                {row.successRate == null ? '—' : percent.format(row.successRate)}
              </td>
              <td className='px-3 py-3'>
                <div className='flex flex-wrap gap-1'>
                  {row.errorBreakdown.length === 0 ? (
                    <span className='text-muted-foreground'>—</span>
                  ) : (
                    row.errorBreakdown.map((bucket) => (
                      <Badge key={bucket.category} variant='secondary' className='font-mono text-[10px]'>
                        {t(`operations.errorCategories.${bucket.category}`)} {bucket.count}
                      </Badge>
                    ))
                  )}
                </div>
              </td>
              <td className='text-muted-foreground px-4 py-3 text-right text-xs'>
                {row.lastActivityAt ? new Date(row.lastActivityAt).toLocaleString() : t('operations.never')}
              </td>
            </tr>
          ))}
          {rows.length === 0 && (
            <tr>
              <td colSpan={7} className='text-muted-foreground px-4 py-10 text-center'>
                {t('operations.routeHealth.empty')}
              </td>
            </tr>
          )}
        </tbody>
      </table>
    </div>
  );
}

export default function OperationsPage() {
  const { i18n, t } = useTranslation();
  const { data: generalSettings } = useGeneralSettings();
  const accountingCurrencyCode = generalSettings?.accountingCurrencyCode?.trim() || DEFAULT_ACCOUNTING_CURRENCY_CODE;
  const [periodDays, setPeriodDays] = useState<1 | 7 | 30>(7);
  const [activeTab, setActiveTab] = useState('overview');
  const [visibleTrendSeries, setVisibleTrendSeries] = useState<OperationsTrendSeriesId[]>(() =>
    loadOperationsTrendSeries(typeof window === 'undefined' ? undefined : window.localStorage)
  );
  const { data, error, isLoading, isFetching, refetch } = useOperationsLedger(periodDays);
  const flowQuery = useOperationsFlow(periodDays);

  useEffect(() => {
    try {
      window.localStorage.setItem(OPERATIONS_TREND_STORAGE_KEY, JSON.stringify(visibleTrendSeries));
    } catch {
      // Browser privacy modes and quota limits must not break the operations view.
    }
  }, [visibleTrendSeries]);

  if (isLoading) {
    return (
      <main className='space-y-4 p-4 md:p-8'>
        {[64, 132, 280, 360].map((height) => (
          <Skeleton key={height} style={{ height }} />
        ))}
      </main>
    );
  }
  if (error || !data) {
    return (
      <main className='p-4 md:p-8'>
        <Alert variant='destructive'>
          <ShieldAlert />
          <AlertTitle>{t('operations.error.title')}</AlertTitle>
          <AlertDescription>{error?.message ?? t('operations.error.description')}</AlertDescription>
        </Alert>
      </main>
    );
  }

  const criticalCount = data.risks.filter((risk) => risk.severity === 'critical').length;
  const visibleTrendSeriesSet = new Set(visibleTrendSeries);
  const showAttemptsAxis = hasVisibleTrendAxis(visibleTrendSeriesSet, attemptsSeries);
  const showMoneyAxis = hasVisibleTrendAxis(visibleTrendSeriesSet, moneySeries);
  const showFailureRateAxis = hasVisibleTrendAxis(visibleTrendSeriesSet, failureRateSeries);
  const toggleTrendSeries = (series: OperationsTrendSeriesId) => {
    setVisibleTrendSeries((current) => (current.includes(series) ? current.filter((item) => item !== series) : [...current, series]));
  };
  // Preserve null as "unavailable" in the ledger while drawing empty chart
  // buckets from the same zero baseline as request counts.
  const chartTrend = data.trend.map((point) => ({
    ...point,
    requestFailureRate: point.requestFailureRate ?? 0,
    failureRate: point.failureRate ?? 0,
    recordedUpstreamCost: point.recordedUpstreamCost ?? 0,
    recognizedUsageRevenue: point.recognizedUsageRevenue ?? 0,
    grossProfit: point.grossProfit ?? 0,
  }));
  return (
    <main className='mx-auto w-full max-w-[1800px] space-y-5 p-4 md:p-8'>
      <header className='flex flex-col justify-between gap-4 border-b pb-5 sm:flex-row sm:items-end'>
        <div>
          <div className='text-muted-foreground mb-1 flex items-center gap-2 text-xs font-medium tracking-[0.16em] uppercase'>
            <ServerCog className='size-4' />
            {t('operations.eyebrow')}
          </div>
          <h1 className='text-2xl font-semibold tracking-tight'>{t('operations.title')}</h1>
          <p className='text-muted-foreground mt-1 max-w-3xl text-sm'>{t('operations.description')}</p>
        </div>
        {activeTab !== 'models' && (
          <div className='flex flex-wrap items-center gap-2'>
            <span className='text-muted-foreground font-mono text-[11px]'>
              {t('operations.updated')} {new Date(data.generatedAt).toLocaleString()}
            </span>
            <div className='border-border flex rounded-md border p-0.5'>
              {([1, 7, 30] as const).map((days) => (
                <Button key={days} size='sm' variant={periodDays === days ? 'secondary' : 'ghost'} onClick={() => setPeriodDays(days)}>
                  {days === 1 ? '24h' : `${days}d`}
                </Button>
              ))}
            </div>
            <Button
              variant='outline'
              size='sm'
              onClick={() => {
                void refetch();
                void flowQuery.refetch();
              }}
              disabled={isFetching || flowQuery.isFetching}
            >
              <RefreshCw className={cn((isFetching || flowQuery.isFetching) && 'animate-spin')} />
              {t('operations.refresh')}
            </Button>
          </div>
        )}
      </header>

      <Tabs value={activeTab} onValueChange={setActiveTab} className='min-w-0'>
        <div className='overflow-x-auto pb-1'>
          <TabsList className='min-w-max'>
            {(['overview', 'models', 'flow', 'users'] as const).map((tab) => (
              <TabsTrigger key={tab} value={tab}>
                {t(`operations.tabs.${tab}`)}
              </TabsTrigger>
            ))}
          </TabsList>
        </div>
      </Tabs>

      {activeTab === 'overview' ? (
        <>
          <section className='border-border bg-muted/25 grid gap-px overflow-hidden rounded-lg border sm:grid-cols-2 xl:grid-cols-[1fr_auto_auto]'>
            <div className='bg-background p-4 sm:col-span-2 xl:col-span-1'>
              <div className='flex items-center gap-2 text-sm font-medium'>
                <Gauge className='size-4 text-sky-500' />
                {t('operations.coverage.title')}
              </div>
              <p className='text-muted-foreground mt-1 text-xs'>{t(`operations.reasons.${data.accountingScopeNote}`)}</p>
            </div>
            <div className='bg-background min-w-44 p-4'>
              <div className='text-muted-foreground text-[11px] uppercase'>{t('operations.coverage.cost')}</div>
              <div className='mt-1 flex items-baseline gap-2 font-mono text-lg tabular-nums'>
                {data.coverage.costCoverageRate == null ? t('operations.na') : percent.format(data.coverage.costCoverageRate)}
                <span className='text-muted-foreground text-xs'>
                  {data.coverage.costedUsageRows}/{data.coverage.usageRows}
                </span>
              </div>
            </div>
            <div className='bg-background min-w-44 p-4'>
              <div className='text-muted-foreground text-[11px] uppercase'>{t('operations.coverage.billing')}</div>
              <div className='mt-2'>
                <QualityBadge metric={data.summary.recognizedUsageRevenue} />
              </div>
            </div>
          </section>

          <LedgerSummary
            cost={data.summary.recordedUpstreamCost}
            revenue={data.summary.recognizedUsageRevenue}
            profit={data.summary.grossProfit}
            currencyCode={accountingCurrencyCode}
            locale={i18n.language}
          />

          <section className='grid gap-3 sm:grid-cols-2 xl:grid-cols-4'>
            <div className='border-border rounded-lg border p-4'>
              <div className='text-muted-foreground text-xs uppercase'>{t('operations.performance.ttft')}</div>
              <div className='mt-2 font-mono text-xl font-semibold tabular-nums'>
                {data.summary.averageTtftMs == null ? '—' : `${Math.round(data.summary.averageTtftMs)} ms`}
              </div>
              <div className='text-muted-foreground mt-1 text-xs'>
                {t('operations.performance.samples', { count: data.summary.ttftSampleCount })}
              </div>
            </div>
            <div className='border-border rounded-lg border p-4'>
              <div className='text-muted-foreground text-xs uppercase'>{t('operations.performance.tps')}</div>
              <div className='mt-2 font-mono text-xl font-semibold tabular-nums'>
                {data.summary.averageTps == null ? '—' : data.summary.averageTps.toFixed(1)}
              </div>
              <div className='text-muted-foreground mt-1 text-xs'>
                {t('operations.performance.samples', { count: data.summary.tpsSampleCount })}
              </div>
            </div>
            <div className='border-border rounded-lg border p-4'>
              <div className='text-muted-foreground text-xs uppercase'>{t('operations.performance.retries')}</div>
              <div className='mt-2 font-mono text-xl font-semibold tabular-nums'>{integer.format(data.summary.retryCount)}</div>
              <div className='text-muted-foreground mt-1 text-xs'>{t('operations.performance.retryDefinition')}</div>
            </div>
            <div className='border-border rounded-lg border p-4'>
              <div className='text-muted-foreground text-xs uppercase'>{t('operations.performance.errors')}</div>
              <div className='mt-2 flex flex-wrap gap-1.5'>
                {data.summary.errorBreakdown.length === 0 ? (
                  <span className='font-mono text-xl font-semibold tabular-nums'>0</span>
                ) : (
                  data.summary.errorBreakdown.map((bucket) => (
                    <Badge key={bucket.category} variant='outline' className='font-mono text-[10px]'>
                      {t(`operations.errorCategories.${bucket.category}`)} {integer.format(bucket.count)}
                    </Badge>
                  ))
                )}
              </div>
            </div>
          </section>

          <section className='grid gap-5 xl:grid-cols-[minmax(0,1fr)_380px]'>
            <Card className='rounded-lg'>
              <CardHeader className='flex-row flex-wrap items-start justify-between gap-3'>
                <div className='min-w-0 flex-1'>
                  <CardTitle>{t('operations.trend.title')}</CardTitle>
                  <p className='text-muted-foreground mt-1 text-xs'>{t('operations.trend.description')}</p>
                </div>
                <div className='flex flex-wrap items-center justify-end gap-2'>
                  <Badge variant='outline' className='font-mono'>
                    {integer.format(data.summary.customerRequests)} {t('operations.kpi.requests')} ·{' '}
                    {integer.format(data.summary.upstreamAttempts)} {t('operations.attempts')}
                  </Badge>
                  <TrendSeriesMenu
                    visibleSeries={visibleTrendSeriesSet}
                    onToggle={toggleTrendSeries}
                    onShowAll={() => setVisibleTrendSeries([...DEFAULT_OPERATIONS_TREND_SERIES])}
                    onReset={() => setVisibleTrendSeries([...DEFAULT_OPERATIONS_TREND_SERIES])}
                  />
                </div>
              </CardHeader>
              <CardContent className='overflow-x-auto'>
                {visibleTrendSeries.length === 0 ? (
                  <div className='border-border flex h-[300px] min-w-[300px] flex-col items-center justify-center rounded-md border border-dashed px-6 text-center'>
                    <SlidersHorizontal className='text-muted-foreground mb-3 size-6' />
                    <p className='text-sm font-medium'>{t('operations.trend.emptyTitle')}</p>
                    <p className='text-muted-foreground mt-1 max-w-sm text-xs'>{t('operations.trend.emptyDescription')}</p>
                    <Button
                      variant='outline'
                      size='sm'
                      className='mt-4'
                      onClick={() => setVisibleTrendSeries([...DEFAULT_OPERATIONS_TREND_SERIES])}
                    >
                      {t('operations.trend.showAll')}
                    </Button>
                  </div>
                ) : (
                  <div className='h-[300px] min-h-[300px] w-[520px] min-w-[520px] md:w-full'>
                    <ResponsiveContainer width='100%' height={300} minWidth={520} minHeight={300}>
                      <ComposedChart data={chartTrend} margin={{ top: 8, right: 38, left: 0, bottom: 0 }}>
                        <CartesianGrid strokeDasharray='3 3' vertical={false} opacity={0.25} />
                        <XAxis dataKey='date' tick={{ fontSize: 11 }} tickFormatter={(value) => value.slice(5)} />
                        {showAttemptsAxis && <YAxis yAxisId='attempts' tick={{ fontSize: 11 }} width={36} />}
                        {showMoneyAxis && (
                          <YAxis
                            yAxisId='money'
                            orientation='right'
                            tick={{ fontSize: 11 }}
                            tickFormatter={(value) =>
                              formatCurrencyValue(value, accountingCurrencyCode, i18n.language, {
                                notation: 'compact',
                                maximumFractionDigits: 1,
                              })
                            }
                            width={44}
                          />
                        )}
                        {showFailureRateAxis && (
                          <YAxis
                            yAxisId='failureRate'
                            orientation='right'
                            domain={[0, 1]}
                            tick={{ fontSize: 11, fill: 'var(--destructive)' }}
                            tickFormatter={(value) => percent.format(value)}
                            width={48}
                            axisLine={false}
                            tickLine={false}
                          />
                        )}
                        <Tooltip
                          contentStyle={{ background: 'var(--card)', border: '1px solid var(--border)', borderRadius: 6 }}
                          formatter={(value, name) => {
                            const numericValue = typeof value === 'number' ? value : Number(value);
                            if (name === t('operations.trend.requestFailureRate') || name === t('operations.trend.upstreamFailureRate'))
                              return [percent.format(numericValue), name];
                            if (
                              name === t('operations.summary.cost') ||
                              name === t('operations.summary.revenue') ||
                              name === t('operations.summary.profit')
                            ) {
                              return [
                                formatCurrencyValue(numericValue, accountingCurrencyCode, i18n.language, {
                                  maximumFractionDigits: 4,
                                }),
                                name,
                              ];
                            }
                            return [integer.format(numericValue), name];
                          }}
                        />
                        <Legend wrapperStyle={{ fontSize: 11, paddingTop: 8 }} />
                        {visibleTrendSeriesSet.has('customerRequests') && (
                          <Area
                            yAxisId='attempts'
                            type='monotone'
                            dataKey='customerRequests'
                            name={t('operations.kpi.requests')}
                            fill='#0ea5e9'
                            stroke='#0284c7'
                            fillOpacity={0.2}
                          />
                        )}
                        {visibleTrendSeriesSet.has('upstreamAttempts') && (
                          <Line
                            yAxisId='attempts'
                            type='monotone'
                            dataKey='upstreamAttempts'
                            name={t('operations.attempts')}
                            stroke='var(--muted-foreground)'
                            strokeWidth={2}
                            dot={false}
                          />
                        )}
                        {visibleTrendSeriesSet.has('retryCount') && (
                          <Line
                            yAxisId='attempts'
                            type='monotone'
                            dataKey='retryCount'
                            name={t('operations.performance.retries')}
                            stroke='#8b5cf6'
                            strokeWidth={1.5}
                            strokeDasharray='3 3'
                            dot={false}
                          />
                        )}
                        {visibleTrendSeriesSet.has('recordedUpstreamCost') && (
                          <Line
                            yAxisId='money'
                            type='monotone'
                            dataKey='recordedUpstreamCost'
                            name={t('operations.summary.cost')}
                            stroke='#f59e0b'
                            strokeWidth={2}
                            connectNulls={false}
                            dot={false}
                          />
                        )}
                        {visibleTrendSeriesSet.has('recognizedUsageRevenue') && (
                          <Line
                            yAxisId='money'
                            type='monotone'
                            dataKey='recognizedUsageRevenue'
                            name={t('operations.summary.revenue')}
                            stroke='#0ea5e9'
                            strokeWidth={2}
                            connectNulls={false}
                            dot={false}
                          />
                        )}
                        {visibleTrendSeriesSet.has('grossProfit') && (
                          <Line
                            yAxisId='money'
                            type='monotone'
                            dataKey='grossProfit'
                            name={t('operations.summary.profit')}
                            stroke='#10b981'
                            strokeWidth={2.5}
                            connectNulls={false}
                            dot={false}
                          />
                        )}
                        {visibleTrendSeriesSet.has('requestFailureRate') && (
                          <Line
                            yAxisId='failureRate'
                            type='monotone'
                            dataKey='requestFailureRate'
                            name={t('operations.trend.requestFailureRate')}
                            stroke='var(--destructive)'
                            strokeWidth={2.5}
                            connectNulls={false}
                            dot={{ r: 2 }}
                          />
                        )}
                        {visibleTrendSeriesSet.has('failureRate') && (
                          <Line
                            yAxisId='failureRate'
                            type='monotone'
                            dataKey='failureRate'
                            name={t('operations.trend.upstreamFailureRate')}
                            stroke='#f97316'
                            strokeWidth={1.5}
                            strokeDasharray='5 3'
                            connectNulls={false}
                            dot={false}
                          />
                        )}
                      </ComposedChart>
                    </ResponsiveContainer>
                  </div>
                )}
              </CardContent>
            </Card>

            <aside className='border-border bg-card rounded-lg border'>
              <div className='border-border flex items-center justify-between border-b p-4'>
                <div className='flex items-center gap-2 font-medium'>
                  <AlertTriangle className='size-4 text-amber-500' />
                  {t('operations.risks.title')}
                </div>
                <div className='flex items-center gap-1.5 text-xs'>
                  <Badge variant={criticalCount ? 'destructive' : 'secondary'}>
                    {criticalCount} {t('operations.risks.critical')}
                  </Badge>
                  <span className='text-muted-foreground'>
                    {data.risks.length - criticalCount} {t('operations.risks.other')}
                  </span>
                </div>
              </div>
              <div className='max-h-[350px] divide-y overflow-y-auto'>
                {data.risks.length === 0 && <div className='p-8 text-center text-sm text-emerald-600'>{t('operations.risks.empty')}</div>}
                {data.risks.slice(0, 12).map((risk, index) => (
                  <div key={`${risk.code}-${risk.channelId ?? 'global'}-${index}`} className='p-4'>
                    <div className='flex items-start gap-2'>
                      <span
                        className={cn(
                          'mt-1.5 size-1.5 shrink-0 rounded-full',
                          risk.severity === 'critical' ? 'bg-destructive' : risk.severity === 'warning' ? 'bg-amber-500' : 'bg-sky-500'
                        )}
                      />
                      <div>
                        <div className='text-sm font-medium'>{t(`operations.riskCodes.${risk.code}.title`)}</div>
                        {risk.channelName && <div className='text-muted-foreground mt-0.5 font-mono text-[11px]'>{risk.channelName}</div>}
                        <p className='text-muted-foreground mt-1 text-xs leading-relaxed'>
                          {t(`operations.riskCodes.${risk.code}.detail`, {
                            affected: risk.affectedCount ?? 0,
                            total: risk.totalCount ?? 0,
                            value: risk.observedValue ?? 0,
                            threshold: risk.thresholdValue ?? 0,
                            days: risk.periodDays ?? data.periodDays,
                          })}
                        </p>
                      </div>
                    </div>
                  </div>
                ))}
              </div>
            </aside>
          </section>

          <section className='grid gap-3 sm:grid-cols-3'>
            <div className='border-border rounded-lg border p-4'>
              <div className='text-muted-foreground flex items-center gap-2 text-xs uppercase'>
                <ServerCog className='size-4' />
                {t('operations.kpi.requests')}
              </div>
              <div className='mt-2 font-mono text-xl font-semibold tabular-nums'>{integer.format(data.summary.customerRequests)}</div>
              <div className='text-muted-foreground mt-1 text-xs'>
                {integer.format(data.summary.upstreamAttempts)} {t('operations.kpi.attempts')} ·{' '}
                {integer.format(data.summary.failedAttempts)} {t('operations.kpi.failed')}
              </div>
            </div>
            <div className='border-border rounded-lg border p-4'>
              <div className='text-muted-foreground flex items-center gap-2 text-xs uppercase'>
                <Gauge className='size-4' />
                {t('operations.kpi.success')}
              </div>
              <div className='mt-2 font-mono text-xl font-semibold tabular-nums'>
                {data.summary.successRate == null ? t('operations.na') : percent.format(data.summary.successRate)}
              </div>
            </div>
            <div className='border-border rounded-lg border p-4'>
              <div className='text-muted-foreground flex items-center gap-2 text-xs uppercase'>
                <Clock3 className='size-4' />
                {t('operations.kpi.tokens')}
              </div>
              <div className='mt-2 font-mono text-xl font-semibold tabular-nums'>{integer.format(data.summary.totalTokens)}</div>
              <div className='text-muted-foreground mt-1 text-xs'>
                {integer.format(data.summary.cachedTokens)} {t('operations.kpi.cached')}
              </div>
            </div>
          </section>

          <section className='space-y-3'>
            <div>
              <h2 className='text-lg font-semibold'>{t('operations.routeHealth.title')}</h2>
              <p className='text-muted-foreground text-xs'>{t('operations.routeHealth.description')}</p>
            </div>
            <RouteHealthTable rows={data.routeHealth} />
          </section>

          <section className='space-y-3'>
            <div className='flex flex-col justify-between gap-1 sm:flex-row sm:items-end'>
              <div>
                <h2 className='text-lg font-semibold'>{t('operations.table.title')}</h2>
                <p className='text-muted-foreground text-xs'>{t('operations.table.description')}</p>
              </div>
            </div>
            <ChannelTable channels={data.channels} currencyCode={accountingCurrencyCode} locale={i18n.language} />
          </section>
        </>
      ) : activeTab === 'models' ? (
        <ModelAnalytics />
      ) : (
        <AnalyticsState loading={flowQuery.isLoading} error={flowQuery.error}>
          {flowQuery.data && activeTab === 'flow' && <FlowAnalytics flow={flowQuery.data} />}
          {flowQuery.data && activeTab === 'users' && <UserAnalytics flow={flowQuery.data} />}
        </AnalyticsState>
      )}
    </main>
  );
}
