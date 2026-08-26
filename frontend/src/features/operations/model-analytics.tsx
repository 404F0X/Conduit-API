import { Fragment, useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { Boxes, Filter, Settings2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Line,
  LineChart,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip,
  XAxis,
  YAxis,
} from 'recharts';
import { DEFAULT_ACCOUNTING_CURRENCY_CODE } from '@/lib/accounting';
import { formatCurrencyValue } from '@/lib/currency-format';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { Skeleton } from '@/components/ui/skeleton';
import { useGeneralSettings } from '@/features/system/data/system';
import {
  MODEL_ANALYTICS_STORAGE_KEY,
  aggregateModels,
  bucketScrollTarget,
  buildModelTimeChart,
  lastNonzeroBucketIndex,
  loadModelAnalyticsPreferences,
  type AnalyticsMetric,
  type ModelAnalysisMode,
  type ModelMainChartMode,
} from './analytics';
import { useOperationsFlow, useOperationsLedger, useOperationsModelSeries } from './data';

const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
const percent = new Intl.NumberFormat(undefined, { style: 'percent', maximumFractionDigits: 1 });
const palette = [
  '#1664ff',
  '#1ac6ff',
  '#ff8a00',
  '#3cc780',
  '#7442d4',
  '#ffc400',
  '#e11d48',
  '#0d9488',
  '#8b5cf6',
  '#64748b',
  '#f97316',
  '#06b6d4',
  '#84cc16',
  '#ec4899',
  '#14b8a6',
  '#6366f1',
];

function Segments<T extends string>({
  value,
  onChange,
  options,
  label,
}: {
  value: T;
  onChange: (value: T) => void;
  options: Array<{ value: T; label: string }>;
  label: string;
}) {
  return (
    <div className='border-border bg-muted/40 inline-flex min-h-10 items-center rounded-md border p-1' role='group' aria-label={label}>
      {options.map((option) => (
        <Button
          key={option.value}
          size='sm'
          variant={value === option.value ? 'secondary' : 'ghost'}
          className='h-8 transition-transform active:scale-[0.96]'
          aria-pressed={value === option.value}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </Button>
      ))}
    </div>
  );
}

function formatMetric(value: number, metric: AnalyticsMetric, currencyCode: string, locale: string) {
  return metric === 'revenue' ? formatCurrencyValue(value, currencyCode, locale, { maximumFractionDigits: 4 }) : compact.format(value);
}
function bucketLabel(value: string, granularity: 'hour' | 'day' | 'week') {
  const date = new Date(value);
  return granularity === 'hour'
    ? date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })
    : date.toLocaleDateString([], { month: '2-digit', day: '2-digit' });
}

function Kpis({
  requests,
  revenue,
  tokens,
  days,
  currencyCode,
  locale,
}: {
  requests: number;
  revenue: number;
  tokens: number;
  days: number;
  currencyCode: string;
  locale: string;
}) {
  const { t } = useTranslation();
  const minutes = Math.max(1, days * 1440);
  const items = [
    [t('operations.analytics.models.kpi.requests'), compact.format(requests), ''],
    [
      t('operations.analytics.models.kpi.revenue'),
      formatCurrencyValue(revenue, currencyCode, locale, { maximumFractionDigits: 4 }),
      'text-emerald-600 dark:text-emerald-400',
    ],
    [t('operations.analytics.models.kpi.tokens'), compact.format(tokens), ''],
    [t('operations.analytics.models.kpi.rpm'), (requests / minutes).toFixed(2), ''],
    [t('operations.analytics.models.kpi.tpm'), compact.format(tokens / minutes), ''],
  ];
  return (
    <section className='border-border bg-card grid overflow-hidden rounded-lg border sm:grid-cols-2 xl:grid-cols-5'>
      {items.map(([label, value, tone], index) => (
        <div
          key={label}
          className={cn(
            'min-w-0 px-4 py-4',
            index > 0 && 'border-border border-t sm:border-l',
            index === 2 && 'sm:border-l-0 xl:border-l',
            index > 1 && 'xl:border-t-0'
          )}
        >
          <div className='text-muted-foreground text-[11px] font-medium tracking-wide uppercase'>{label}</div>
          <div className={cn('mt-2 font-mono text-xl font-semibold tabular-nums', tone)}>{value}</div>
        </div>
      ))}
    </section>
  );
}

function TimeChart({
  rows,
  models,
  mode,
  metric,
  granularity,
  currencyCode,
  locale,
}: {
  rows: Array<Record<string, string | number>>;
  models: string[];
  mode: ModelMainChartMode | 'trend';
  metric: AnalyticsMetric;
  granularity: 'hour' | 'day' | 'week';
  currencyCode: string;
  locale: string;
}) {
  const { t } = useTranslation();
  const name = (model: string) => (model === '__other__' ? t('operations.analytics.other') : model);
  const scrollRef = useRef<HTMLDivElement>(null);
  const scrollTargetRef = useRef(0);
  const [latestPositioned, setLatestPositioned] = useState(false);
  const lastUsefulBucket = useMemo(() => lastNonzeroBucketIndex(rows, models), [rows, models]);
  useLayoutEffect(() => {
    const container = scrollRef.current;
    if (!container) return;
    let firstFrame = 0;
    let secondFrame = 0;
    let disposed = false;
    const positionLatest = () => {
      if (disposed) return;
      const target = bucketScrollTarget(lastUsefulBucket, rows.length, container.scrollWidth, container.clientWidth);
      scrollTargetRef.current = target;
      container.scrollLeft = target;
      setLatestPositioned(lastUsefulBucket >= 0 && Math.abs(container.scrollLeft - target) <= 1);
    };
    const schedule = () => {
      window.cancelAnimationFrame(firstFrame);
      window.cancelAnimationFrame(secondFrame);
      firstFrame = window.requestAnimationFrame(() => {
        secondFrame = window.requestAnimationFrame(positionLatest);
      });
    };
    const observer = new ResizeObserver(schedule);
    observer.observe(container);
    if (container.firstElementChild) observer.observe(container.firstElementChild);
    schedule();
    void document.fonts?.ready.then(schedule);
    return () => {
      disposed = true;
      observer.disconnect();
      window.cancelAnimationFrame(firstFrame);
      window.cancelAnimationFrame(secondFrame);
    };
  }, [rows, models, mode, metric, lastUsefulBucket]);
  if (!models.length)
    return (
      <div className='border-border text-muted-foreground flex h-72 items-center justify-center rounded-md border border-dashed text-sm'>
        {t('operations.analytics.empty')}
      </div>
    );
  const common = { data: rows, margin: { top: 8, right: 16, left: 0, bottom: 4 } };
  const axes = (
    <>
      <CartesianGrid strokeDasharray='3 3' vertical={false} opacity={0.24} />
      <XAxis
        dataKey='bucketStart'
        tick={{ fontSize: 11 }}
        tickFormatter={(value) => bucketLabel(String(value), granularity)}
        minTickGap={24}
      />
      <YAxis
        tick={{ fontSize: 11 }}
        width={48}
        tickFormatter={(value) =>
          metric === 'revenue'
            ? formatCurrencyValue(value, currencyCode, locale, { notation: 'compact', maximumFractionDigits: 1 })
            : compact.format(value)
        }
      />
      <Tooltip
        contentStyle={{ background: 'var(--card)', border: '1px solid var(--border)', borderRadius: 6 }}
        labelFormatter={(value) => new Date(String(value)).toLocaleString()}
        formatter={(value, nameValue) => [formatMetric(Number(value), metric, currencyCode, locale), name(String(nameValue))]}
      />
    </>
  );
  return (
    <div className='min-w-0 space-y-2'>
      <p className='text-muted-foreground text-[11px] md:hidden'>
        {t(latestPositioned ? 'operations.analytics.models.scrollHint' : 'operations.analytics.models.scrollPending')}
      </p>
      <div
        ref={scrollRef}
        data-testid='model-time-chart-scroll'
        data-latest-positioned={latestPositioned}
        data-last-useful-bucket={lastUsefulBucket}
        className='min-w-0 overflow-x-auto pb-2'
        onScroll={(event) => {
          const container = event.currentTarget;
          setLatestPositioned(lastUsefulBucket >= 0 && Math.abs(container.scrollLeft - scrollTargetRef.current) <= 1);
        }}
      >
        <div className='h-[360px] w-full min-w-[760px]'>
          <ResponsiveContainer width='100%' height='100%' initialDimension={{ width: 760, height: 360 }}>
            {mode === 'bar' ? (
              <BarChart {...common}>
                {axes}
                {models.map((model, index) => (
                  <Bar key={model} dataKey={model} name={model} stackId='models' fill={palette[index % palette.length]} maxBarSize={46} />
                ))}
              </BarChart>
            ) : mode === 'area' ? (
              <AreaChart {...common}>
                {axes}
                {models.map((model, index) => (
                  <Area
                    key={model}
                    type='monotone'
                    dataKey={model}
                    name={model}
                    stroke={palette[index % palette.length]}
                    fill={palette[index % palette.length]}
                    fillOpacity={0.12}
                    strokeWidth={2}
                    connectNulls={false}
                  />
                ))}
              </AreaChart>
            ) : (
              <LineChart {...common}>
                {axes}
                {models.map((model, index) => (
                  <Line
                    key={model}
                    type='monotone'
                    dataKey={model}
                    name={model}
                    stroke={palette[index % palette.length]}
                    strokeWidth={2}
                    dot={false}
                    connectNulls={false}
                  />
                ))}
              </LineChart>
            )}
          </ResponsiveContainer>
        </div>
      </div>
      <div className='flex flex-wrap gap-x-4 gap-y-1 text-[11px]'>
        {models.map((model, index) => (
          <span key={model} className='inline-flex min-w-0 items-center gap-1.5'>
            <span className='size-2.5 shrink-0 rounded-sm' style={{ backgroundColor: palette[index % palette.length] }} />
            <span className='max-w-48 truncate'>{name(model)}</span>
          </span>
        ))}
      </div>
    </div>
  );
}

function Ranking({
  values,
  metric,
  currencyCode,
  locale,
}: {
  values: Array<[string, number]>;
  metric: AnalyticsMetric;
  currencyCode: string;
  locale: string;
}) {
  const max = Math.max(0, ...values.map(([, value]) => value));
  return (
    <div className='space-y-3'>
      {values.map(([model, value], index) => (
        <div key={model} className='grid grid-cols-[minmax(0,1fr)_auto] gap-x-4 gap-y-1'>
          <div className='truncate text-sm'>
            <span className='text-muted-foreground mr-2 font-mono text-xs'>{index + 1}</span>
            {model}
          </div>
          <span className='font-mono text-sm font-semibold'>{formatMetric(value, metric, currencyCode, locale)}</span>
          <div className='bg-muted col-span-2 ml-6 h-2 overflow-hidden rounded-sm'>
            <div className='h-full bg-sky-500' style={{ width: max ? `${Math.max(1, (value / max) * 100)}%` : '0%' }} />
          </div>
        </div>
      ))}
    </div>
  );
}

export function ModelAnalytics() {
  const { i18n, t } = useTranslation();
  const { data: generalSettings } = useGeneralSettings();
  const accountingCurrencyCode = generalSettings?.accountingCurrencyCode?.trim() || DEFAULT_ACCOUNTING_CURRENCY_CODE;
  const [preferences, setPreferences] = useState(() =>
    loadModelAnalyticsPreferences(typeof window === 'undefined' ? undefined : window.localStorage)
  );
  const [metric, setMetric] = useState<AnalyticsMetric>('requests');
  const seriesQuery = useOperationsModelSeries(preferences.periodDays);
  const flowQuery = useOperationsFlow(preferences.periodDays);
  const ledgerQuery = useOperationsLedger(preferences.periodDays);
  useEffect(() => {
    try {
      window.localStorage.setItem(MODEL_ANALYTICS_STORAGE_KEY, JSON.stringify(preferences));
    } catch {
      /* browser storage is optional */
    }
  }, [preferences]);
  const series = seriesQuery.data;
  const flow = flowQuery.data;
  const ledger = ledgerQuery.data;
  const chart = useMemo(() => (series ? buildModelTimeChart(series, metric, 15) : null), [series, metric]);
  const trend = useMemo(() => (series ? buildModelTimeChart(series, 'requests', 20) : null), [series]);
  const models = useMemo(() => (flow && ledger ? aggregateModels(flow.rows, ledger.routeHealth) : []), [flow, ledger]);
  const totals = useMemo(
    () => ({
      requests: series?.points.reduce((sum, p) => sum + p.meteredRequests, 0) ?? 0,
      tokens: series?.points.reduce((sum, p) => sum + p.totalTokens, 0) ?? 0,
      revenue: series?.points.reduce((sum, p) => sum + (p.recognizedUsageRevenue ?? 0), 0) ?? 0,
    }),
    [series]
  );
  const ranking = [...(trend?.totals.entries() ?? [])].sort((a, b) => b[1] - a[1]);
  const pieData = ranking.slice(0, 20).map(([name, value]) => ({ name, value }));
  const pieTotal = pieData.reduce((sum, item) => sum + item.value, 0);
  const health = (ledger?.routeHealth ?? [])
    .filter((row) => row.upstreamAttempts > 0)
    .sort((a, b) => b.upstreamAttempts - a.upstreamAttempts)
    .slice(0, 8);
  const update = (patch: Partial<typeof preferences>) => setPreferences((current) => ({ ...current, ...patch }));
  return (
    <div className='space-y-5'>
      <div className='flex flex-col justify-between gap-3 sm:flex-row sm:items-end'>
        <div>
          <h2 className='flex items-center gap-2 text-lg font-semibold'>
            <Boxes className='size-5 text-sky-500' />
            {t('operations.analytics.models.title')}
          </h2>
          <p className='text-muted-foreground mt-1 text-xs'>{t('operations.analytics.models.description')}</p>
        </div>
        <div className='flex flex-wrap gap-2'>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant='outline' className='min-h-10 transition-transform active:scale-[0.96]'>
                <Settings2 />
                {t('operations.analytics.models.preferences')}
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align='end' className='w-64'>
              <DropdownMenuLabel>{t('operations.analytics.models.mainChart')}</DropdownMenuLabel>
              <DropdownMenuRadioGroup
                value={preferences.mainChart}
                onValueChange={(value) => update({ mainChart: value as ModelMainChartMode })}
              >
                {(['bar', 'area'] as const).map((mode) => (
                  <DropdownMenuRadioItem key={mode} value={mode}>
                    {t(`operations.analytics.models.${mode}`)}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
              <DropdownMenuSeparator />
              <DropdownMenuLabel>{t('operations.analytics.models.analysis')}</DropdownMenuLabel>
              <DropdownMenuRadioGroup
                value={preferences.analysisMode}
                onValueChange={(value) => update({ analysisMode: value as ModelAnalysisMode })}
              >
                {(['trend', 'proportion', 'top'] as const).map((mode) => (
                  <DropdownMenuRadioItem key={mode} value={mode}>
                    {t(`operations.analytics.models.${mode}`)}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button variant='outline' className='min-h-10 transition-transform active:scale-[0.96]'>
                <Filter />
                {t('operations.analytics.models.filter')} · {preferences.periodDays}d
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align='end'>
              <DropdownMenuLabel>{t('operations.analytics.models.period')}</DropdownMenuLabel>
              <DropdownMenuRadioGroup
                value={String(preferences.periodDays)}
                onValueChange={(value) => {
                  const days = Number(value);
                  if (days === 1 || days === 7 || days === 14 || days === 29) update({ periodDays: days });
                }}
              >
                {([1, 7, 14, 29] as const).map((days) => (
                  <DropdownMenuRadioItem key={days} value={String(days)}>
                    {days} {t('operations.analytics.models.days')}
                  </DropdownMenuRadioItem>
                ))}
              </DropdownMenuRadioGroup>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>
      {(seriesQuery.isLoading || flowQuery.isLoading || ledgerQuery.isLoading) && (
        <>
          <Skeleton className='h-24' />
          <Skeleton className='h-[430px]' />
        </>
      )}
      {(seriesQuery.error || flowQuery.error || ledgerQuery.error) && (
        <div className='border-destructive/40 bg-destructive/5 text-destructive rounded-lg border p-4 text-sm'>
          {seriesQuery.error?.message ?? flowQuery.error?.message ?? ledgerQuery.error?.message}
        </div>
      )}
      {series && flow && ledger && (
        <>
          <Kpis
            requests={totals.requests}
            revenue={totals.revenue}
            tokens={totals.tokens}
            days={preferences.periodDays}
            currencyCode={accountingCurrencyCode}
            locale={i18n.language}
          />
          <section className='border-border bg-card rounded-lg border p-4'>
            <div className='flex flex-wrap items-center gap-x-5 gap-y-3'>
              <strong className='text-sm'>{t('operations.analytics.models.health')}</strong>
              <span className='text-xs'>
                <span className='text-muted-foreground'>{t('operations.analytics.models.success')}</span>{' '}
                <b className='font-mono'>{ledger.summary.successRate == null ? '—' : percent.format(ledger.summary.successRate)}</b>
              </span>
              <span className='text-xs'>
                <span className='text-muted-foreground'>TTFT</span>{' '}
                <b className='font-mono'>{ledger.summary.averageTtftMs == null ? '—' : `${Math.round(ledger.summary.averageTtftMs)} ms`}</b>
              </span>
              <span className='text-xs'>
                <span className='text-muted-foreground'>TPS</span>{' '}
                <b className='font-mono'>{ledger.summary.averageTps == null ? '—' : ledger.summary.averageTps.toFixed(1)}</b>
              </span>
              {health.map((row) => (
                <span
                  key={`${row.channelId}:${row.actualModel}`}
                  className={cn(
                    'inline-flex min-h-7 items-center gap-1 rounded-md border px-2 text-[11px]',
                    row.healthStatus === 'healthy'
                      ? 'border-emerald-500/30 text-emerald-500'
                      : row.healthStatus === 'unhealthy'
                        ? 'border-destructive/40 text-destructive'
                        : 'border-amber-500/40 text-amber-500'
                  )}
                >
                  <span className='max-w-40 truncate'>
                    {row.actualModel} · {row.channelName}
                  </span>
                  <span className='font-mono'>{row.successRate == null ? '—' : percent.format(row.successRate)}</span>
                </span>
              ))}
              {health.length === 0 && <span className='text-muted-foreground text-xs'>{t('operations.analytics.models.noHealth')}</span>}
            </div>
          </section>
          <Card className='rounded-lg'>
            <CardHeader>
              <div className='grid gap-3 md:grid-cols-[minmax(0,1fr)_auto] md:items-start'>
                <div>
                  <CardTitle>{t('operations.analytics.models.distribution')}</CardTitle>
                  <p className='text-muted-foreground mt-1 font-mono text-xs'>
                    {t('operations.analytics.models.total')} {formatMetric(totals[metric], metric, accountingCurrencyCode, i18n.language)} ·{' '}
                    {t(`operations.analytics.models.granularity.${series.granularity}`)}
                  </p>
                </div>
                <Segments
                  value={preferences.mainChart}
                  onChange={(mainChart: ModelMainChartMode) => update({ mainChart })}
                  label={t('operations.analytics.models.chartMode')}
                  options={[
                    { value: 'bar', label: t('operations.analytics.models.bar') },
                    { value: 'area', label: t('operations.analytics.models.area') },
                  ]}
                />
              </div>
              <div className='mt-3 flex flex-wrap gap-2'>
                <Segments
                  value={metric}
                  onChange={setMetric}
                  label={t('operations.analytics.metric')}
                  options={(['requests', 'tokens', 'revenue'] as const).map((value) => ({
                    value,
                    label: t(`operations.analytics.metrics.${value}`),
                  }))}
                />
              </div>
            </CardHeader>
            <CardContent className='min-w-0'>
              <TimeChart
                rows={chart?.rows ?? []}
                models={chart?.models ?? []}
                mode={preferences.mainChart}
                metric={metric}
                granularity={series.granularity}
                currencyCode={accountingCurrencyCode}
                locale={i18n.language}
              />
            </CardContent>
          </Card>
          <Card className='rounded-lg'>
            <CardHeader>
              <div className='grid gap-3 md:grid-cols-[minmax(0,1fr)_auto] md:items-start'>
                <div>
                  <CardTitle>{t('operations.analytics.models.analysis')}</CardTitle>
                  <p className='text-muted-foreground mt-1 text-xs'>{t('operations.analytics.models.realBuckets')}</p>
                </div>
                <Segments
                  value={preferences.analysisMode}
                  onChange={(analysisMode: ModelAnalysisMode) => update({ analysisMode })}
                  label={t('operations.analytics.models.analysisMode')}
                  options={[
                    { value: 'trend', label: t('operations.analytics.models.trend') },
                    { value: 'proportion', label: t('operations.analytics.models.proportion') },
                    { value: 'top', label: t('operations.analytics.models.ranking') },
                  ]}
                />
              </div>
            </CardHeader>
            <CardContent className='min-w-0'>
              {preferences.analysisMode === 'trend' ? (
                <TimeChart
                  rows={trend?.rows ?? []}
                  models={trend?.models ?? []}
                  mode='trend'
                  metric='requests'
                  granularity={series.granularity}
                  currencyCode={accountingCurrencyCode}
                  locale={i18n.language}
                />
              ) : preferences.analysisMode === 'proportion' ? (
                <div className='mx-auto w-full max-w-3xl min-w-0 space-y-3'>
                  <div className='h-[260px] min-h-[260px] w-full min-w-0 sm:h-[340px]'>
                    <ResponsiveContainer width='100%' height='100%' initialDimension={{ width: 320, height: 260 }}>
                      <PieChart>
                        <Pie data={pieData} dataKey='value' nameKey='name' innerRadius='48%' outerRadius='78%' paddingAngle={1}>
                          {pieData.map((entry, index) => (
                            <Cell key={entry.name} fill={palette[index % palette.length]} />
                          ))}
                        </Pie>
                        <Tooltip formatter={(value) => compact.format(Number(value))} />
                      </PieChart>
                    </ResponsiveContainer>
                  </div>
                  <div className='grid gap-x-5 gap-y-2 sm:grid-cols-2'>
                    {pieData.map((item, index) => (
                      <div key={item.name} className='grid min-w-0 grid-cols-[auto_minmax(0,1fr)_auto] items-center gap-2 text-xs'>
                        <span className='size-2.5 rounded-sm' style={{ backgroundColor: palette[index % palette.length] }} />
                        <span className='truncate'>{item.name}</span>
                        <span className='text-muted-foreground font-mono tabular-nums'>
                          {compact.format(item.value)} · {percent.format(pieTotal ? item.value / pieTotal : 0)}
                        </span>
                      </div>
                    ))}
                  </div>
                </div>
              ) : (
                <Ranking values={ranking.slice(0, 20)} metric='requests' currencyCode={accountingCurrencyCode} locale={i18n.language} />
              )}
            </CardContent>
          </Card>
          <div>
            <h3 className='text-sm font-semibold'>{t('operations.analytics.models.detail')}</h3>
            <p className='text-muted-foreground mt-1 text-xs'>{t('operations.analytics.models.detailDescription')}</p>
          </div>
          <div className='border-border overflow-x-auto rounded-lg border'>
            <table className='w-full min-w-[980px] text-sm'>
              <thead className='bg-muted/50 text-muted-foreground text-left text-[11px] uppercase'>
                <tr>
                  <th className='px-4 py-3'>{t('operations.analytics.models.publicModel')}</th>
                  <th className='px-3 py-3'>{t('operations.analytics.models.actualModel')}</th>
                  <th className='px-3 py-3'>{t('operations.analytics.models.channel')}</th>
                  <th className='px-3 py-3 text-right'>{t('operations.analytics.metrics.requests')}</th>
                  <th className='px-3 py-3 text-right'>{t('operations.analytics.metrics.tokens')}</th>
                  <th className='px-3 py-3 text-right'>{t('operations.table.cost')}</th>
                  <th className='px-3 py-3 text-right'>{t('operations.table.revenue')}</th>
                  <th className='px-3 py-3 text-right'>{t('operations.table.profit')}</th>
                  <th className='px-3 py-3 text-right'>{t('operations.analytics.models.success')}</th>
                  <th className='px-4 py-3 text-right'>{t('operations.analytics.models.attempts')}</th>
                </tr>
              </thead>
              <tbody>
                {models.map((model) => (
                  <Fragment key={model.requestedModel}>
                    <tr className='border-border bg-muted/35 border-t'>
                      <td colSpan={3} className='px-4 py-3'>
                        <div className='font-semibold'>{model.requestedModel || t('operations.analytics.fallback.model')}</div>
                        <div className='text-muted-foreground mt-0.5 text-[11px]'>
                          {t('operations.analytics.models.groupTotal', { count: model.supplies.length })}
                        </div>
                      </td>
                      <td className='px-3 py-3 text-right font-mono font-semibold'>{compact.format(model.requests)}</td>
                      <td className='px-3 py-3 text-right font-mono font-semibold'>{compact.format(model.tokens)}</td>
                      <td className='px-3 py-3 text-right font-mono font-semibold'>
                        {model.costComplete
                          ? formatCurrencyValue(model.cost, accountingCurrencyCode, i18n.language, { maximumFractionDigits: 4 })
                          : `≈ ${formatCurrencyValue(model.cost, accountingCurrencyCode, i18n.language, { maximumFractionDigits: 4 })}`}
                      </td>
                      <td className='px-3 py-3 text-right font-mono font-semibold'>
                        {model.revenueComplete
                          ? formatCurrencyValue(model.revenue, accountingCurrencyCode, i18n.language, { maximumFractionDigits: 4 })
                          : `≈ ${formatCurrencyValue(model.revenue, accountingCurrencyCode, i18n.language, { maximumFractionDigits: 4 })}`}
                      </td>
                      <td className='px-3 py-3 text-right font-mono font-semibold'>
                        {model.costComplete && model.revenueComplete
                          ? formatCurrencyValue(model.revenue - model.cost, accountingCurrencyCode, i18n.language, {
                              maximumFractionDigits: 4,
                            })
                          : '—'}
                      </td>
                      <td colSpan={2} className='text-muted-foreground px-4 py-3 text-right text-[11px]'>
                        {t('operations.analytics.models.modelTotal')}
                      </td>
                    </tr>
                    {model.supplies.map((supply) => (
                      <tr key={`${model.requestedModel}:${supply.channelId}:${supply.actualModel}`} className='border-border border-t'>
                        <td className='text-muted-foreground px-4 py-3 pl-7 text-xs'>{t('operations.analytics.models.supply')}</td>
                        <td className='px-3 py-3 font-mono text-xs'>
                          {supply.actualModel || t('operations.analytics.fallback.actualModel')}
                        </td>
                        <td className='px-3 py-3'>{supply.channelName || t('operations.analytics.fallback.channel')}</td>
                        <td className='px-3 py-3 text-right font-mono'>{compact.format(supply.requests)}</td>
                        <td className='px-3 py-3 text-right font-mono'>{compact.format(supply.tokens)}</td>
                        <td colSpan={3} className='text-muted-foreground px-3 py-3 text-center text-[11px]'>
                          {t('operations.analytics.models.financeAtModelLevel')}
                        </td>
                        <td className='px-3 py-3 text-right font-mono'>
                          {supply.successRate == null ? '—' : percent.format(supply.successRate)}
                        </td>
                        <td className='px-4 py-3 text-right font-mono'>
                          {supply.attempts == null ? '—' : compact.format(supply.attempts)}
                        </td>
                      </tr>
                    ))}
                  </Fragment>
                ))}
              </tbody>
            </table>
          </div>
        </>
      )}
    </div>
  );
}
