import { useDeferredValue, useState } from 'react';
import { Link } from '@tanstack/react-router';
import { ArrowRight, Layers3, RefreshCw, Search } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import type { ChangeSetRouteSearch } from './change-set-search';
import { CHANGE_SET_KINDS, type ChangeSetKind, type ChangeSetStatus, usePackagePage } from './data/change-sets';
import { GovernancePanel } from './governance-panel';
import { packageEmptyState } from './package-state';

export function ChangeSetPage({ mode, initialFilters = {} }: { mode: 'workbench' | 'changelog'; initialFilters?: ChangeSetRouteSearch }) {
  const { t, i18n } = useTranslation();
  const [status, setStatus] = useState<ChangeSetStatus | 'ALL'>(
    initialFilters.all ? 'ALL' : (initialFilters.status ?? (mode === 'workbench' ? 'PENDING_REVIEW' : 'ALL'))
  );
  const [kind, setKind] = useState<ChangeSetKind | 'ALL'>(initialFilters.kind ?? 'ALL');
  const [search, setSearch] = useState(initialFilters.q ?? '');
  const [scope, setScope] = useState({ scopeType: initialFilters.scopeType, scopeID: initialFilters.scopeID });
  const deferredSearch = useDeferredValue(search);
  const [cursors, setCursors] = useState<Array<string | undefined>>(initialFilters.after ? [undefined, initialFilters.after] : [undefined]);
  const query = usePackagePage(
    {
      kind: kind === 'ALL' ? undefined : kind,
      statuses: status === 'ALL' ? [] : [status],
      ...scope,
      search: deferredSearch || undefined,
    },
    cursors.at(-1)
  );
  const counts = new Map(query.data?.statusCounts.map((c) => [c.status, c.count]));
  const resetPage = () => setCursors([undefined]);
  const rows = query.data?.edges.map((e) => e.node) ?? [];
  const emptyState = packageEmptyState(query.data?.statusCounts ?? [], {
    search: deferredSearch,
    kind: kind === 'ALL' ? undefined : kind,
    ...scope,
  });
  const resetFilters = () => {
    setSearch('');
    setKind('ALL');
    setStatus('ALL');
    setScope({ scopeType: undefined, scopeID: undefined });
    resetPage();
  };
  return (
    <>
      <Header className='h-auto min-h-16 shrink-0 py-2'>
        <div className='flex min-w-0 flex-1 items-center gap-3'>
          <div className='bg-primary/10 text-primary grid size-10 shrink-0 place-items-center rounded-xl'>
            <Layers3 className='size-5' />
          </div>
          <div className='min-w-0'>
            <h1 className='text-xl font-semibold tracking-tight break-words'>{t('eventPackages.inbox')}</h1>
            <p className='text-muted-foreground hidden text-sm sm:block'>{t('eventPackages.intro')}</p>
          </div>
        </div>
        <Button variant='outline' size='sm' onClick={() => void query.refetch()} disabled={query.isFetching}>
          <RefreshCw className='size-4' />
          <span className='hidden sm:inline'>{t('eventPackages.refresh')}</span>
        </Button>
      </Header>
      <Main fixed className='[scrollbar-gutter:stable] gap-5 overflow-y-auto'>
        {mode === 'workbench' && <GovernancePanel />}
        <div className='flex shrink-0 flex-col justify-between gap-3 lg:flex-row lg:items-center'>
          <Tabs
            value={status}
            onValueChange={(value) => {
              setStatus(value as typeof status);
              resetPage();
            }}
          >
            <TabsList className='flex h-auto max-w-full flex-wrap gap-1'>
              {' '}
              {(['PENDING_REVIEW', 'APPROVED', 'DRAFT', 'INVALID', 'ALL'] as const).map((value) => (
                <TabsTrigger key={value} value={value} className='gap-2 py-2'>
                  {t(`eventPackages.${value === 'ALL' ? 'all' : value}`)}
                  <span className='bg-background/70 rounded px-1.5 text-xs tabular-nums'>
                    {query.data
                      ? value === 'ALL'
                        ? query.data.statusCounts.reduce((n, c) => n + c.count, 0)
                        : (counts.get(value) ?? 0)
                      : '—'}
                  </span>
                </TabsTrigger>
              ))}
            </TabsList>
          </Tabs>
          <div className='flex min-w-0 flex-1 gap-2 lg:max-w-xl'>
            <div className='relative min-w-0 flex-1'>
              <Search className='text-muted-foreground pointer-events-none absolute top-2.5 left-3 size-4' />
              <Input
                value={search}
                onChange={(e) => {
                  setSearch(e.target.value);
                  resetPage();
                }}
                placeholder={t('eventPackages.search')}
                aria-label={t('eventPackages.search')}
                className='pl-9'
              />
            </div>
            <Select
              value={kind}
              onValueChange={(value) => {
                setKind(value as typeof kind);
                resetPage();
              }}
            >
              <SelectTrigger className='w-40'>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {(['ALL', ...CHANGE_SET_KINDS] as const).map((k) => (
                  <SelectItem key={k} value={k}>
                    {t(`eventPackages.${k === 'ALL' ? 'allKinds' : k}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>
        <div className='bg-card shrink-0 overflow-hidden rounded-xl border shadow-sm'>
          <div className='text-muted-foreground bg-muted/25 hidden grid-cols-[minmax(0,1fr)_10rem_10rem_12rem] gap-4 border-b px-5 py-3 text-xs font-medium tracking-wide uppercase lg:grid'>
            <span>{t('eventPackages.scope')}</span>
            <span>{t('eventPackages.actor')}</span>
            <span>{t('eventPackages.time')}</span>
            <span>{t('eventPackages.changes')}</span>
          </div>
          {query.isLoading ? (
            <p role='status' className='p-10 text-center text-sm'>
              {t('eventPackages.loading')}
            </p>
          ) : query.isError ? (
            <div role='alert' className='space-y-3 p-8'>
              <h2 className='font-medium'>{t('eventPackages.error')}</h2>
              <p className='text-muted-foreground text-sm'>{query.error.message}</p>
              <Button variant='outline' onClick={() => void query.refetch()}>
                {t('eventPackages.retry')}
              </Button>
            </div>
          ) : rows.length === 0 ? (
            <div className='px-6 py-16 text-center'>
              <Layers3 className='text-muted-foreground mx-auto mb-3 size-8' />
              <h2 className='font-medium'>{t(`eventPackages.${emptyState === 'filtered' ? 'filteredEmpty' : 'empty'}`)}</h2>
              <p className='text-muted-foreground mt-2 text-sm'>
                {t(`eventPackages.${emptyState === 'filtered' ? 'filteredEmptyBody' : 'emptyBody'}`)}
              </p>
              {emptyState === 'filtered' && (
                <Button variant='outline' className='mt-4' onClick={resetFilters}>
                  {t('eventPackages.resetFilters')}
                </Button>
              )}
            </div>
          ) : (
            rows.map((row) => (
              <Link
                key={row.id}
                to='/change-sets/$changeSetId'
                search={{
                  list: mode,
                  q: search || undefined,
                  kind: kind === 'ALL' ? undefined : kind,
                  status: status === 'ALL' ? undefined : status,
                  all: status === 'ALL' ? true : undefined,
                  ...scope,
                  after: cursors.at(-1),
                }}
                params={{ changeSetId: row.id }}
                className='group hover:bg-muted/35 focus-visible:outline-primary grid gap-3 border-b px-5 py-4 transition-colors last:border-b-0 focus-visible:outline-2 lg:grid-cols-[minmax(0,1fr)_10rem_10rem_12rem] lg:items-center'
              >
                <div className='min-w-0'>
                  <div className='mb-1.5 flex flex-wrap items-center gap-2'>
                    <Badge
                      variant={row.status === 'INVALID' ? 'destructive' : row.status === 'APPLIED' ? 'default' : 'secondary'}
                      className='text-[11px]'
                    >
                      {t(`eventPackages.${row.status}`)}
                    </Badge>
                    <span className='text-muted-foreground text-xs'>{t(`eventPackages.${row.kind}`)}</span>
                    <span className='text-muted-foreground ml-auto text-xs tabular-nums'>#{row.id.split('/').at(-1)}</span>
                  </div>
                  <h2 className='text-sm font-semibold break-words'>{row.scopeName ?? row.title}</h2>
                  <p className='text-muted-foreground mt-1 text-xs break-words'>
                    {row.title} · {t(`eventPackages.${row.sourceOrigin}`)}
                  </p>
                </div>
                <p className='text-muted-foreground min-w-0 text-xs break-words'>
                  {row.actorName ??
                    (row.createdBy
                      ? t('eventPackages.userFallback', { id: row.createdBy.split('/').at(-1) })
                      : t(row.sourceOrigin === 'AUTOMATIC_SYNC' ? 'eventPackages.system' : 'eventPackages.unknown'))}
                </p>
                <time dateTime={row.updatedAt} className='text-muted-foreground text-xs tabular-nums'>
                  {new Intl.DateTimeFormat(i18n.language, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(row.updatedAt))}
                </time>
                <div className='flex items-center justify-between gap-2'>
                  <div className='min-w-0'>
                    <p className='text-sm font-semibold tabular-nums'>
                      {row.itemCount} <span className='text-muted-foreground font-normal'>{t('eventPackages.items')}</span>
                    </p>
                    <p className='text-muted-foreground mt-1 flex flex-wrap gap-x-2 gap-y-1 text-xs leading-relaxed tabular-nums'>
                      {(
                        [
                          ['addedItems', row.createCount],
                          ['updatedItems', row.updateCount],
                          ['deletedItems', row.deleteCount],
                          ['needsActionItems', row.invalidItemCount],
                        ] as const
                      ).map(([label, count]) => (count ? <span key={label}>{t(`eventPackages.${label}`, { count })}</span> : null))}
                    </p>
                  </div>
                  <ArrowRight className='text-muted-foreground size-4 shrink-0 transition-transform group-hover:translate-x-0.5' />
                </div>
              </Link>
            ))
          )}
        </div>
        <div className='flex shrink-0 flex-wrap items-center justify-between gap-3 pb-4 text-sm'>
          <span className='text-muted-foreground tabular-nums'>
            {query.data ? `${query.data.totalCount} ${t('eventPackages.packages')}` : '—'}
          </span>
          <div className='flex gap-2'>
            <Button
              variant='outline'
              size='sm'
              disabled={cursors.length === 1 || query.isFetching}
              onClick={() => setCursors(cursors.slice(0, -1))}
            >
              {t('eventPackages.previous')}
            </Button>
            <Button
              variant='outline'
              size='sm'
              disabled={!query.data?.pageInfo.hasNextPage || query.isFetching}
              onClick={() => setCursors([...cursors, query.data!.pageInfo.endCursor!])}
            >
              {t('eventPackages.next')}
            </Button>
          </div>
        </div>
      </Main>
    </>
  );
}
