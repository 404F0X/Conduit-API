import { Fragment, useMemo, useState } from 'react';
import { AlertCircle, ArrowRight, Eye, EyeOff, GitBranch, Users } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Sankey, Tooltip, type SankeyLinkProps, type SankeyNodeProps } from 'recharts';
import { DEFAULT_ACCOUNTING_CURRENCY_CODE } from '@/lib/accounting';
import { formatCurrencyValue } from '@/lib/currency-format';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { useGeneralSettings } from '@/features/system/data/system';
import {
  FLOW_STAGES,
  aggregateUsers,
  buildFlowPaths,
  buildSankeyGraph,
  maskSensitiveLabel,
  stageValue,
  toggleFlowStage,
  type AnalyticsMetric,
  type FlowOverflowMode,
  type FlowStage,
} from './analytics';
import type { OperationsFlow } from './data';

const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });

const controlClass = 'min-h-10 transition-transform active:scale-[0.96]';

function Controls({
  metric,
  setMetric,
  top,
  setTop,
  topOptions,
}: {
  metric: AnalyticsMetric;
  setMetric: (metric: AnalyticsMetric) => void;
  top: number;
  setTop: (top: number) => void;
  topOptions: number[];
}) {
  const { t } = useTranslation();
  return (
    <div className='flex flex-wrap items-center gap-2' aria-label={t('operations.analytics.controls')}>
      <Select value={metric} onValueChange={(value) => setMetric(value as AnalyticsMetric)}>
        <SelectTrigger size='sm' className='min-h-10' aria-label={t('operations.analytics.metric')}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {(['requests', 'tokens', 'revenue'] as const).map((value) => (
            <SelectItem key={value} value={value}>
              {t(`operations.analytics.metrics.${value}`)}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
      <Select value={String(top)} onValueChange={(value) => setTop(Number(value))}>
        <SelectTrigger size='sm' className='min-h-10' aria-label={t('operations.analytics.topN')}>
          <SelectValue />
        </SelectTrigger>
        <SelectContent>
          {topOptions.map((value) => (
            <SelectItem key={value} value={String(value)}>
              Top {value}
            </SelectItem>
          ))}
        </SelectContent>
      </Select>
    </div>
  );
}

function metricLabel(value: number, metric: AnalyticsMetric, currencyCode: string, locale: string) {
  return metric === 'revenue' ? formatCurrencyValue(value, currencyCode, locale, { maximumFractionDigits: 4 }) : compact.format(value);
}

function RankedBars({
  rows,
  metric,
  empty,
  currencyCode,
  locale,
}: {
  rows: Array<{ key: string; value: number; detail?: string }>;
  metric: AnalyticsMetric;
  empty: string;
  currencyCode: string;
  locale: string;
}) {
  const max = Math.max(0, ...rows.map((row) => row.value));
  if (!rows.length) return <EmptyState title={empty} />;
  return (
    <div className='space-y-3'>
      {rows.map((row, index) => (
        <div key={row.key} className='grid grid-cols-[minmax(0,1fr)_auto] items-end gap-x-4 gap-y-1'>
          <div className='min-w-0'>
            <div className='flex items-baseline gap-2'>
              <span className='text-muted-foreground w-5 font-mono text-xs'>{index + 1}</span>
              <span className='truncate text-sm font-medium'>{row.key}</span>
            </div>
            {row.detail && <div className='text-muted-foreground ml-7 truncate text-[11px]'>{row.detail}</div>}
          </div>
          <span className='font-mono text-sm font-semibold tabular-nums'>{metricLabel(row.value, metric, currencyCode, locale)}</span>
          <div className='bg-muted col-span-2 ml-7 h-2 overflow-hidden rounded-sm'>
            <div
              className='h-full bg-sky-500 transition-[width]'
              style={{ width: max ? `${Math.max(1.5, (row.value / max) * 100)}%` : '0%' }}
            />
          </div>
        </div>
      ))}
    </div>
  );
}

function EmptyState({ title }: { title: string }) {
  return (
    <div className='border-border text-muted-foreground flex min-h-44 items-center justify-center rounded-md border border-dashed px-6 text-center text-sm'>
      {title}
    </div>
  );
}

export function AnalyticsState({ loading, error, children }: { loading: boolean; error: Error | null; children: React.ReactNode }) {
  const { t } = useTranslation();
  if (loading)
    return (
      <div className='grid gap-4'>
        <Skeleton className='h-72' />
        <Skeleton className='h-80' />
      </div>
    );
  if (error)
    return (
      <div className='border-destructive/40 bg-destructive/5 text-destructive flex items-start gap-3 rounded-lg border p-5 text-sm'>
        <AlertCircle className='mt-0.5 size-4 shrink-0' />
        <div>
          <div className='font-medium'>{t('operations.analytics.error')}</div>
          <div className='mt-1 opacity-80'>{error.message}</div>
        </div>
      </div>
    );
  return <>{children}</>;
}

export { ModelAnalytics } from './model-analytics';

const stageColor: Record<FlowStage, string> = {
  user: '#1664ff',
  project: '#1ac6ff',
  apiKey: '#7442d4',
  requestedModel: '#ff8a00',
  actualModel: '#3cc780',
  channel: '#e11d48',
};

function SankeyNodeShape({ x, y, width, height, payload }: SankeyNodeProps) {
  const node = payload as typeof payload & { stage?: FlowStage; name: string };
  const stage = node.stage ?? 'user';
  const labelRight = x < 720;
  return (
    <g tabIndex={0} role='graphics-symbol' aria-label={node.name}>
      <rect
        x={x}
        y={y}
        width={width}
        height={Math.max(2, height)}
        rx={2}
        fill={stageColor[stage]}
        stroke='var(--background)'
        strokeWidth={1}
      />
      <text
        x={labelRight ? x + width + 5 : x - 5}
        y={y + Math.max(10, height / 2)}
        textAnchor={labelRight ? 'start' : 'end'}
        fill='var(--foreground)'
        fontSize={10}
      >
        {node.name.length > 18 ? `${node.name.slice(0, 16)}…` : node.name}
      </text>
      <title>{node.name}</title>
    </g>
  );
}

function SankeyLinkShape(props: SankeyLinkProps) {
  const { sourceX, targetX, sourceY, targetY, sourceControlX, targetControlX, sourceRelativeY, targetRelativeY, linkWidth, payload } =
    props;
  const source = payload.source as typeof payload.source & { name?: string; stage?: FlowStage };
  const target = payload.target as typeof payload.target & { name?: string };
  const path = `M${sourceX},${sourceY + sourceRelativeY} C${sourceControlX},${sourceY + sourceRelativeY} ${targetControlX},${targetY + targetRelativeY} ${targetX},${targetY + targetRelativeY}`;
  return (
    <path
      d={path}
      fill='none'
      stroke={stageColor[source.stage ?? 'user']}
      strokeWidth={Math.max(1, linkWidth)}
      strokeOpacity={0.34}
      className='hover:stroke-opacity-70 transition-[stroke-opacity]'
    >
      <title>
        {source.name} → {target.name}: {payload.value}
      </title>
    </path>
  );
}

export function FlowAnalytics({ flow }: { flow: OperationsFlow }) {
  const { i18n, t } = useTranslation();
  const { data: generalSettings } = useGeneralSettings();
  const accountingCurrencyCode = generalSettings?.accountingCurrencyCode?.trim() || DEFAULT_ACCOUNTING_CURRENCY_CODE;
  const [metric, setMetric] = useState<AnalyticsMetric>('requests');
  const [top, setTop] = useState(20);
  const [stages, setStages] = useState<FlowStage[]>(FLOW_STAGES);
  const [overflowMode, setOverflowMode] = useState<FlowOverflowMode>('merge');
  const [maskSensitive, setMaskSensitive] = useState(true);
  const [userFilter, setUserFilter] = useState('__all__');
  const [nodeFilter, setNodeFilter] = useState('');
  const userOptions = useMemo(() => [...new Set(flow.rows.map((row) => stageValue(row, 'user')))].sort(), [flow.rows]);
  const filteredRows = useMemo(
    () =>
      flow.rows.filter(
        (row) =>
          (userFilter === '__all__' ||
            (userFilter === '__unattributed__' ? !stageValue(row, 'user') : stageValue(row, 'user') === userFilter)) &&
          (!nodeFilter.trim() ||
            FLOW_STAGES.some((stage) => stageValue(row, stage).toLocaleLowerCase().includes(nodeFilter.trim().toLocaleLowerCase())))
      ),
    [flow.rows, userFilter, nodeFilter]
  );
  const paths = useMemo(
    () => buildFlowPaths(filteredRows, stages, metric, top, overflowMode),
    [filteredRows, stages, metric, top, overflowMode]
  );
  const graph = useMemo(() => buildSankeyGraph(paths, maskSensitive), [paths, maskSensitive]);
  const fallback = (value: string, stage: FlowStage) =>
    value === '__other__' ? t('operations.analytics.other') : value || t(`operations.analytics.fallback.${stage}`);
  return (
    <div className='space-y-5'>
      <div className='flex flex-col justify-between gap-3 lg:flex-row lg:items-end'>
        <div>
          <h2 className='flex items-center gap-2 text-lg font-semibold'>
            <GitBranch className='size-5 text-violet-500' />
            {t('operations.analytics.flow.title')}
          </h2>
          <p className='text-muted-foreground mt-1 max-w-3xl text-xs'>{t('operations.analytics.flow.description')}</p>
        </div>
        <Controls metric={metric} setMetric={setMetric} top={top} setTop={setTop} topOptions={[10, 20, 50, 100]} />
      </div>
      <div
        className='border-border bg-card flex flex-wrap items-end gap-3 rounded-xl border p-3'
        role='group'
        aria-label={t('operations.analytics.flow.stages')}
      >
        {FLOW_STAGES.map((stage) => (
          <Button
            key={stage}
            size='sm'
            variant={stages.includes(stage) ? 'secondary' : 'outline'}
            className={controlClass}
            aria-pressed={stages.includes(stage)}
            onClick={() => setStages((current) => toggleFlowStage(current, stage))}
          >
            {t(`operations.analytics.stages.${stage}`)}
          </Button>
        ))}
        <div className='border-border hidden h-8 border-l lg:block' />
        <Select value={overflowMode} onValueChange={(value) => setOverflowMode(value as FlowOverflowMode)}>
          <SelectTrigger className='min-h-10' aria-label={t('operations.analytics.flow.overflow')}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value='merge'>{t('operations.analytics.flow.merge')}</SelectItem>
            <SelectItem value='hide'>{t('operations.analytics.flow.hide')}</SelectItem>
          </SelectContent>
        </Select>
        <Select value={userFilter} onValueChange={setUserFilter}>
          <SelectTrigger className='min-h-10 max-w-56' aria-label={t('operations.analytics.flow.userFilter')}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value='__all__'>{t('operations.analytics.flow.allUsers')}</SelectItem>
            {userOptions.map((user) => (
              <SelectItem key={user || '__unattributed__'} value={user || '__unattributed__'}>
                {maskSensitiveLabel(user, 'user', maskSensitive) || t('operations.analytics.fallback.user')}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Input
          className='min-h-10 w-full sm:w-56'
          value={nodeFilter}
          onChange={(event) => setNodeFilter(event.target.value)}
          placeholder={t('operations.analytics.flow.nodeFilter')}
          aria-label={t('operations.analytics.flow.nodeFilter')}
        />
        <Button variant='outline' className={controlClass} aria-pressed={maskSensitive} onClick={() => setMaskSensitive((value) => !value)}>
          {maskSensitive ? <EyeOff /> : <Eye />}
          {t('operations.analytics.flow.mask')}
        </Button>
      </div>
      <div className='border-border bg-card rounded-xl border'>
        <div className='border-border flex flex-wrap items-center justify-between gap-2 border-b p-4'>
          <div className='text-sm font-medium'>{t('operations.analytics.flow.pathTitle')}</div>
          <div className='text-muted-foreground text-xs'>
            {t('operations.analytics.flow.coverage', {
              returned: compact.format(flow.rows.reduce((sum, row) => sum + row.meteredRequests, 0)),
              usage: compact.format(flow.usageRows),
              settled: compact.format(flow.settledUsageRows),
            })}
          </div>
        </div>
        {!paths.length || graph.links.length === 0 ? (
          <div className='p-5'>
            <EmptyState title={t('operations.analytics.empty')} />
          </div>
        ) : (
          <>
            <div className='overflow-x-auto'>
              <div className='h-[560px] min-w-[1040px] p-3'>
                <Sankey
                  width={1040}
                  height={540}
                  data={graph}
                  nodePadding={18}
                  nodeWidth={11}
                  linkCurvature={0.55}
                  node={SankeyNodeShape}
                  link={SankeyLinkShape}
                  margin={{ top: 24, right: 100, bottom: 24, left: 80 }}
                >
                  <Tooltip
                    formatter={(value) => metricLabel(Number(value), metric, accountingCurrencyCode, i18n.language)}
                    contentStyle={{ background: 'var(--card)', border: '1px solid var(--border)', borderRadius: 6 }}
                  />
                </Sankey>
              </div>
            </div>
            <details className='border-border border-t p-4 text-xs'>
              <summary className='text-muted-foreground focus-visible:ring-ring hover:text-foreground inline-flex min-h-10 cursor-pointer items-center rounded-md px-2 focus-visible:ring-2 focus-visible:outline-none'>
                {t('operations.analytics.flow.inspectPath')} · {paths.length}
              </summary>
              <div className='mt-2 max-h-72 overflow-auto rounded-md border'>
                {paths.map((path) => (
                  <div key={path.key} className='border-border flex min-w-[760px] items-center gap-1.5 border-b px-3 py-2 last:border-b-0'>
                    {path.values.map((item, index) => (
                      <Fragment key={item.stage}>
                        <span>
                          <span className='text-muted-foreground'>{t(`operations.analytics.stages.${item.stage}`)}:</span>{' '}
                          {fallback(maskSensitiveLabel(item.value, item.stage, maskSensitive), item.stage)}
                        </span>
                        {index < path.values.length - 1 && <ArrowRight className='text-muted-foreground size-3.5 shrink-0' />}
                      </Fragment>
                    ))}
                    <span className='ml-auto font-mono font-semibold'>
                      {metricLabel(path.metric, metric, accountingCurrencyCode, i18n.language)}
                    </span>
                  </div>
                ))}
              </div>
            </details>
          </>
        )}
      </div>
      <div className='rounded-lg border border-amber-500/30 bg-amber-500/5 p-4 text-xs leading-relaxed'>
        <strong>{t('operations.analytics.flow.entitlementTitle')}</strong> {t('operations.analytics.flow.entitlementNote')}
      </div>
    </div>
  );
}

function UserChannelDistribution({
  flow,
  metric,
  top,
  fallbackUser,
  fallbackChannel,
  currencyCode,
  locale,
}: {
  flow: OperationsFlow;
  metric: AnalyticsMetric;
  top: number;
  fallbackUser: string;
  fallbackChannel: string;
  currencyCode: string;
  locale: string;
}) {
  const palette = ['#1664ff', '#1ac6ff', '#ff8a00', '#3cc780', '#7442d4', '#e11d48', '#64748b'];
  const rows = useMemo(() => {
    const users = new Map<string, { channels: Map<string, number>; total: number }>();
    for (const row of flow.rows) {
      const user = stageValue(row, 'user') || fallbackUser;
      const channel = stageValue(row, 'channel') || fallbackChannel;
      const value = metric === 'requests' ? row.meteredRequests : metric === 'tokens' ? row.totalTokens : (row.recognizedUsageRevenue ?? 0);
      const entry = users.get(user) ?? { channels: new Map(), total: 0 };
      entry.channels.set(channel, (entry.channels.get(channel) ?? 0) + value);
      entry.total += value;
      users.set(user, entry);
    }
    return [...users.entries()]
      .map(([user, value]) => ({ user, ...value }))
      .sort((a, b) => b.total - a.total)
      .slice(0, top);
  }, [flow.rows, metric, top, fallbackUser, fallbackChannel]);
  const channels = [...new Set(rows.flatMap((row) => [...row.channels.keys()]))];
  if (!rows.length) return <EmptyState title='—' />;
  return (
    <div className='space-y-3'>
      {rows.map((row) => (
        <div key={row.user} className='grid grid-cols-[minmax(7rem,12rem)_minmax(0,1fr)_5rem] items-center gap-3'>
          <span className='truncate text-xs font-medium' title={row.user}>
            {row.user}
          </span>
          <div className='bg-muted flex h-6 overflow-hidden rounded-sm'>
            {channels.map((channel, index) => {
              const value = row.channels.get(channel) ?? 0;
              return value > 0 ? (
                <div
                  key={channel}
                  style={{ width: `${(value / Math.max(1, row.total)) * 100}%`, backgroundColor: palette[index % palette.length] }}
                  title={`${channel}: ${metricLabel(value, metric, currencyCode, locale)}`}
                />
              ) : null;
            })}
          </div>
          <span className='text-right font-mono text-xs font-semibold'>{metricLabel(row.total, metric, currencyCode, locale)}</span>
        </div>
      ))}
      <div className='flex flex-wrap gap-x-4 gap-y-2 pt-2'>
        {channels.map((channel, index) => (
          <span key={channel} className='flex items-center gap-1.5 text-[11px]'>
            <span className='size-2.5 rounded-[2px]' style={{ backgroundColor: palette[index % palette.length] }} />
            {channel}
          </span>
        ))}
      </div>
    </div>
  );
}

export function UserAnalytics({ flow }: { flow: OperationsFlow }) {
  const { i18n, t } = useTranslation();
  const { data: generalSettings } = useGeneralSettings();
  const accountingCurrencyCode = generalSettings?.accountingCurrencyCode?.trim() || DEFAULT_ACCOUNTING_CURRENCY_CODE;
  const [metric, setMetric] = useState<AnalyticsMetric>('requests');
  const [top, setTop] = useState(10);
  const users = useMemo(() => aggregateUsers(flow.rows), [flow.rows]);
  const ranked = users
    .map((user) => ({
      key: user.email || t('operations.analytics.fallback.user'),
      value: metric === 'requests' ? user.requests : metric === 'tokens' ? user.tokens : user.revenue,
    }))
    .sort((a, b) => b.value - a.value)
    .slice(0, top);
  return (
    <div className='space-y-5'>
      <div className='flex flex-col justify-between gap-3 sm:flex-row sm:items-end'>
        <div>
          <h2 className='flex items-center gap-2 text-lg font-semibold'>
            <Users className='size-5 text-cyan-500' />
            {t('operations.analytics.users.title')}
          </h2>
          <p className='text-muted-foreground mt-1 max-w-3xl text-xs'>{t('operations.analytics.users.description')}</p>
        </div>
        <Controls metric={metric} setMetric={setMetric} top={top} setTop={setTop} topOptions={[5, 10, 20]} />
      </div>
      <Card className='rounded-lg'>
        <CardHeader>
          <CardTitle>{t('operations.analytics.distribution')}</CardTitle>
        </CardHeader>
        <CardContent>
          <RankedBars
            rows={ranked}
            metric={metric}
            empty={t('operations.analytics.empty')}
            currencyCode={accountingCurrencyCode}
            locale={i18n.language}
          />
        </CardContent>
      </Card>
      <Card className='rounded-xl'>
        <CardHeader>
          <CardTitle>{t('operations.analytics.users.channelContribution')}</CardTitle>
          <p className='text-muted-foreground text-xs'>{t('operations.analytics.users.channelContributionDescription')}</p>
        </CardHeader>
        <CardContent>
          <UserChannelDistribution
            flow={flow}
            metric={metric}
            top={top}
            fallbackUser={t('operations.analytics.fallback.user')}
            fallbackChannel={t('operations.analytics.fallback.channel')}
            currencyCode={accountingCurrencyCode}
            locale={i18n.language}
          />
        </CardContent>
      </Card>
      <div>
        <h3 className='text-sm font-semibold'>{t('operations.analytics.users.detail')}</h3>
        <p className='text-muted-foreground mt-1 text-xs'>{t('operations.analytics.users.detailDescription')}</p>
      </div>
      <div className='border-border overflow-x-auto rounded-lg border'>
        <table className='w-full min-w-[980px] text-xs xl:table-fixed'>
          <thead className='bg-muted/50 text-muted-foreground text-left text-[11px] uppercase'>
            <tr>
              {['user', 'scope', 'requests', 'tokens', 'cost', 'revenue', 'profit', 'activity'].map((key) => (
                <th
                  key={key}
                  className={cn(
                    'px-2.5 py-3',
                    key !== 'user' && key !== 'scope' && 'text-right',
                    key === 'user' && 'xl:w-[18%]',
                    key === 'scope' && 'xl:w-[18%]',
                    key === 'activity' && 'xl:w-[17%]'
                  )}
                >
                  {t(`operations.analytics.users.columns.${key}`)}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {users.map((user) => (
              <tr key={user.key} className='border-border border-t'>
                <td className='px-2.5 py-3'>
                  <div className='truncate font-medium' title={user.email || t('operations.analytics.fallback.user')}>
                    {user.email || t('operations.analytics.fallback.user')}
                  </div>
                  {user.userId != null && <div className='text-muted-foreground font-mono text-[10px]'>#{user.userId}</div>}
                </td>
                <td className='px-2.5 py-3'>
                  <div className='grid grid-cols-2 gap-x-2 gap-y-1'>
                    {(['projects', 'apiKeys', 'models', 'channels'] as const).map((key) => (
                      <span key={key} className='whitespace-nowrap'>
                        <span className='text-muted-foreground'>{t(`operations.analytics.users.scope.${key}`)}</span>{' '}
                        <span className='font-mono'>{compact.format(user[key])}</span>
                      </span>
                    ))}
                  </div>
                </td>
                {[user.requests, user.tokens].map((value, index) => (
                  <td key={index} className='px-2.5 py-3 text-right font-mono'>
                    {compact.format(value)}
                  </td>
                ))}
                <td className='px-2.5 py-3 text-right font-mono'>
                  {user.costComplete
                    ? formatCurrencyValue(user.cost, accountingCurrencyCode, i18n.language, { maximumFractionDigits: 4 })
                    : `≈ ${formatCurrencyValue(user.cost, accountingCurrencyCode, i18n.language, { maximumFractionDigits: 4 })}`}
                </td>
                <td className='px-2.5 py-3 text-right font-mono'>
                  {user.revenueComplete
                    ? formatCurrencyValue(user.revenue, accountingCurrencyCode, i18n.language, { maximumFractionDigits: 4 })
                    : `≈ ${formatCurrencyValue(user.revenue, accountingCurrencyCode, i18n.language, { maximumFractionDigits: 4 })}`}
                </td>
                <td className='px-2.5 py-3 text-right font-mono'>
                  {user.costComplete && user.revenueComplete
                    ? formatCurrencyValue(user.revenue - user.cost, accountingCurrencyCode, i18n.language, {
                        maximumFractionDigits: 4,
                      })
                    : '—'}
                </td>
                <td className='px-2.5 py-3 text-right text-[11px] whitespace-nowrap'>
                  {user.lastActivityAt ? new Date(user.lastActivityAt).toLocaleString() : '—'}
                </td>
              </tr>
            ))}
            {!users.length && (
              <tr>
                <td colSpan={8} className='text-muted-foreground px-4 py-12 text-center'>
                  {t('operations.analytics.empty')}
                </td>
              </tr>
            )}
          </tbody>
        </table>
      </div>
    </div>
  );
}
