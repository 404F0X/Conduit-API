import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Checkbox } from '@/components/ui/checkbox';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet';
import { Switch } from '@/components/ui/switch';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { EmptyState, ErrorState, LoadingRows, PageHeading } from '@/components/page-state';
import { useMyModelCatalog } from '@/features/model-market/data';
import { StatusLabel } from '../components/status-label';
import { useSimpleTokens } from '../data/tokens';
import { type SimpleRequest, type SimpleRequestFilter, usageOf, useSimpleRequestCounts, useSimpleRequests } from '../data/usage';
import { formatCount, formatDuration, formatFullTime, formatRelativeTime, startOfToday } from '../lib/format';
import { useMoney } from '../lib/use-money';

const PAGE_SIZE = 20;
const ALL = '__all__';
type RangeChoice = 'today' | '7d' | '30d' | 'all';

function rangeStart(range: RangeChoice): Date | null {
  if (range === 'all') return null;
  const start = startOfToday();
  if (range === '7d') start.setDate(start.getDate() - 6);
  if (range === '30d') start.setDate(start.getDate() - 29);
  return start;
}

function costItemLabel(code: string, t: (key: string) => string) {
  const known = ['prompt_tokens', 'completion_tokens', 'prompt_cached_tokens', 'prompt_write_cached_tokens'];
  return known.includes(code) ? t(`simple.usage.costItem.${code}`) : code;
}

/** 使用日志：一条请求一行，固定 6 列；点开看这次请求的扣费明细。 */
export function SimpleUsageLogs() {
  const { t, i18n } = useTranslation();
  const money = useMoney();
  const [range, setRange] = useState<RangeChoice>('7d');
  const [apiKeyId, setApiKeyId] = useState(ALL);
  const [modelId, setModelId] = useState(ALL);
  const [failedOnly, setFailedOnly] = useState(false);
  const [autoRefresh, setAutoRefresh] = useState(false);
  const [cursors, setCursors] = useState<string[]>([]);
  const [detail, setDetail] = useState<SimpleRequest | null>(null);

  const filter: SimpleRequestFilter = useMemo(
    () => ({
      from: rangeStart(range),
      apiKeyId: apiKeyId === ALL ? null : apiKeyId,
      modelId: modelId === ALL ? null : modelId,
      failedOnly,
    }),
    [range, apiKeyId, modelId, failedOnly]
  );
  const resetPage = () => setCursors([]);

  const requests = useSimpleRequests(
    filter,
    { first: PAGE_SIZE, after: cursors[cursors.length - 1] },
    { refetchInterval: autoRefresh ? 10_000 : false }
  );
  const [failedCount] = useSimpleRequestCounts([{ ...filter, failedOnly: true }]);
  const tokens = useSimpleTokens({ first: 100 });
  const catalog = useMyModelCatalog();

  const rows = requests.data?.edges.map((edge) => edge.node) ?? [];
  const pageTotals = rows.reduce(
    (acc, row) => {
      const usage = usageOf(row);
      acc.tokens += usage?.totalTokens ?? 0;
      acc.cost += Number(usage?.totalCost ?? 0) || 0;
      return acc;
    },
    { tokens: 0, cost: 0 }
  );

  const update =
    <T,>(setter: (value: T) => void) =>
    (value: T) => {
      setter(value);
      resetPage();
    };

  return (
    <>
      <Header fixed>
        <PageHeading title={t('simple.usage.title')} description={t('simple.usage.description')} />
      </Header>
      <Main>
        <Card>
          <CardContent className='space-y-4 pt-6'>
            <div className='flex flex-wrap items-center gap-2'>
              <Select value={range} onValueChange={update((v: string) => setRange(v as RangeChoice))}>
                <SelectTrigger className='w-32'>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  {(['today', '7d', '30d', 'all'] as const).map((choice) => (
                    <SelectItem key={choice} value={choice}>
                      {t(`simple.usage.range.${choice}`)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Select value={apiKeyId} onValueChange={update(setApiKeyId)}>
                <SelectTrigger className='w-40'>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={ALL}>{t('simple.usage.allTokens')}</SelectItem>
                  {(tokens.data?.edges ?? []).map(({ node }) => (
                    <SelectItem key={node.id} value={node.id}>
                      {node.name}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <Select value={modelId} onValueChange={update(setModelId)}>
                <SelectTrigger className='w-48'>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value={ALL}>{t('simple.usage.allModels')}</SelectItem>
                  {(catalog.data?.models ?? []).map((model) => (
                    <SelectItem key={model.modelId} value={model.modelId}>
                      {model.modelId}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <label className='flex items-center gap-2 text-sm'>
                <Checkbox checked={failedOnly} onCheckedChange={(checked) => update(setFailedOnly)(checked === true)} />
                {t('simple.usage.failedOnly')}
              </label>
              <label className='ml-auto flex items-center gap-2 text-sm'>
                <Switch checked={autoRefresh} onCheckedChange={setAutoRefresh} />
                {t('simple.usage.autoRefresh')}
              </label>
            </div>

            {requests.data && (
              <div className='bg-muted/50 text-muted-foreground flex flex-wrap gap-x-6 gap-y-1 rounded-md px-3 py-2 text-sm'>
                <span>{t('simple.usage.summary.count', { count: requests.data.totalCount })}</span>
                {failedCount?.data != null && failedCount.data > 0 && !failedOnly && (
                  <span className='text-red-600'>{t('simple.usage.summary.failed', { count: failedCount.data })}</span>
                )}
                {rows.length > 0 && (
                  <span>
                    {t('simple.usage.summary.page', {
                      cost: money.cost(pageTotals.cost).text,
                      tokens: formatCount(pageTotals.tokens, i18n.language),
                    })}
                  </span>
                )}
              </div>
            )}

            {requests.isLoading ? (
              <LoadingRows />
            ) : requests.isError ? (
              <ErrorState onRetry={() => requests.refetch()} />
            ) : rows.length === 0 ? (
              <EmptyState title={t('simple.usage.emptyTitle')} description={t('simple.usage.emptyDescription')} />
            ) : (
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t('simple.usage.columns.time')}</TableHead>
                    <TableHead>{t('simple.usage.columns.token')}</TableHead>
                    <TableHead>{t('simple.usage.columns.model')}</TableHead>
                    <TableHead>{t('simple.usage.columns.tokens')}</TableHead>
                    <TableHead>{t('simple.usage.columns.cost')}</TableHead>
                    <TableHead>{t('simple.usage.columns.status')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {rows.map((row) => {
                    const usage = usageOf(row);
                    const cost = money.cost(usage?.totalCost);
                    return (
                      <TableRow key={row.id} className='cursor-pointer' onClick={() => setDetail(row)}>
                        <TableCell className='whitespace-nowrap' title={formatFullTime(row.createdAt, i18n.language)}>
                          {formatRelativeTime(row.createdAt, i18n.language, t)}
                        </TableCell>
                        <TableCell className='max-w-36 truncate'>{row.apiKey?.name ?? '—'}</TableCell>
                        <TableCell className='max-w-48 truncate font-mono text-xs'>{row.modelID}</TableCell>
                        <TableCell className='whitespace-nowrap tabular-nums'>
                          {usage
                            ? `${formatCount(usage.promptTokens, i18n.language)} / ${formatCount(usage.completionTokens, i18n.language)}`
                            : '—'}
                        </TableCell>
                        <TableCell className='whitespace-nowrap tabular-nums' title={cost.exact}>
                          {cost.text}
                        </TableCell>
                        <TableCell>
                          <StatusLabel status={row.status} />
                        </TableCell>
                      </TableRow>
                    );
                  })}
                </TableBody>
              </Table>
            )}

            {(cursors.length > 0 || requests.data?.pageInfo.hasNextPage) && (
              <div className='flex items-center justify-end gap-2'>
                <Button variant='outline' size='sm' disabled={cursors.length === 0} onClick={() => setCursors((c) => c.slice(0, -1))}>
                  {t('simple.pager.previous')}
                </Button>
                <Button
                  variant='outline'
                  size='sm'
                  disabled={!requests.data?.pageInfo.hasNextPage}
                  onClick={() => {
                    const end = requests.data?.pageInfo.endCursor;
                    if (end) setCursors((c) => [...c, end]);
                  }}
                >
                  {t('simple.pager.next')}
                </Button>
              </div>
            )}
          </CardContent>
        </Card>
      </Main>

      <Sheet open={!!detail} onOpenChange={(open) => !open && setDetail(null)}>
        <SheetContent className='w-full overflow-y-auto sm:max-w-lg'>{detail && <RequestDetail request={detail} />}</SheetContent>
      </Sheet>
    </>
  );
}

function RequestDetail({ request }: { request: SimpleRequest }) {
  const { t, i18n } = useTranslation();
  const money = useMoney();
  const usage = usageOf(request);
  const items = usage?.costItems ?? [];
  return (
    <>
      <SheetHeader>
        <SheetTitle className='font-mono text-base'>{request.modelID}</SheetTitle>
        <SheetDescription>{formatFullTime(request.createdAt, i18n.language)}</SheetDescription>
      </SheetHeader>
      <div className='space-y-6 px-4 pb-6 text-sm'>
        <dl className='grid grid-cols-2 gap-4'>
          <div>
            <dt className='text-muted-foreground'>{t('simple.usage.columns.status')}</dt>
            <dd className='mt-1'>
              <StatusLabel status={request.status} />
            </dd>
          </div>
          <div>
            <dt className='text-muted-foreground'>{t('simple.usage.columns.token')}</dt>
            <dd className='mt-1'>{request.apiKey?.name ?? '—'}</dd>
          </div>
          <div>
            <dt className='text-muted-foreground'>{t('simple.usage.detail.latency')}</dt>
            <dd className='mt-1'>{formatDuration(request.metricsLatencyMs, t)}</dd>
          </div>
          <div>
            <dt className='text-muted-foreground'>{t('simple.usage.detail.firstToken')}</dt>
            <dd className='mt-1'>
              {request.stream ? formatDuration(request.metricsFirstTokenLatencyMs, t) : t('simple.usage.detail.notStream')}
            </dd>
          </div>
        </dl>

        <div className='space-y-2'>
          <h3 className='font-medium'>{t('simple.usage.detail.costTitle')}</h3>
          {!usage ? (
            <p className='text-muted-foreground'>{t('simple.usage.detail.noUsage')}</p>
          ) : (
            <div className='rounded-md border'>
              {items.length > 0 ? (
                items.map((item) => (
                  <div key={item.itemCode} className='flex items-center justify-between border-b px-3 py-2 last:border-b-0'>
                    <span>
                      {costItemLabel(item.itemCode, t)}
                      <span className='text-muted-foreground ml-2 tabular-nums'>{formatCount(item.quantity, i18n.language)}</span>
                    </span>
                    <span className='tabular-nums'>{money.cost(item.subtotal).text}</span>
                  </div>
                ))
              ) : (
                <div className='text-muted-foreground border-b px-3 py-2'>
                  {t('simple.usage.detail.tokens', {
                    input: formatCount(usage.promptTokens, i18n.language),
                    output: formatCount(usage.completionTokens, i18n.language),
                  })}
                </div>
              )}
              <div className='bg-muted/50 flex items-center justify-between px-3 py-2 font-medium'>
                <span>{t('simple.usage.detail.total')}</span>
                <span className='tabular-nums' title={money.cost(usage.totalCost).exact}>
                  {money.cost(usage.totalCost).text}
                </span>
              </div>
            </div>
          )}
          {request.status === 'failed' && <p className='text-muted-foreground text-xs'>{t('simple.usage.detail.failedHint')}</p>}
        </div>
      </div>
    </>
  );
}
