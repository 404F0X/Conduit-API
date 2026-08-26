import { Link } from '@tanstack/react-router';
import {
  Activity,
  ArrowRight,
  CircleAlert,
  CircleCheck,
  Clock3,
  FolderKanban,
  Gauge,
  KeyRound,
  LayoutDashboard,
  Play,
  Radio,
} from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useSelectedProjectId } from '@/stores/projectStore';
import { useRoutePermissions } from '@/hooks/useRoutePermissions';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card';
import { Skeleton } from '@/components/ui/skeleton';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { useApiKeys } from '@/features/apikeys/data/apikeys';
import { useMe } from '@/features/auth/data/auth';
import { useMyProjects } from '@/features/projects/data/projects';
import { useRequests } from '@/features/requests/data/requests';
import type { Request, RequestStatus } from '@/features/requests/data/schema';
import { type PublicChannelHealth, usePublicChannelHealth } from './health-data';

const statusStyles: Record<RequestStatus, string> = {
  completed: 'border-emerald-500/20 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400',
  failed: 'border-destructive/20 bg-destructive/10 text-destructive',
  canceled: 'border-muted-foreground/20 bg-muted text-muted-foreground',
  pending: 'border-amber-500/20 bg-amber-500/10 text-amber-700 dark:text-amber-400',
  processing: 'border-primary/20 bg-primary/10 text-primary',
};

const signalStyles: Record<RequestStatus, string> = {
  completed: 'bg-emerald-500',
  failed: 'bg-destructive',
  canceled: 'bg-muted-foreground',
  pending: 'bg-amber-500',
  processing: 'bg-primary',
};

function StatCard({
  icon: Icon,
  label,
  value,
  hint,
  tone = 'primary',
  loading,
  unavailable = false,
}: {
  icon: typeof Activity;
  label: string;
  value: string;
  hint: string;
  tone?: 'primary' | 'danger' | 'success';
  loading: boolean;
  unavailable?: boolean;
}) {
  const iconTone = {
    primary: 'bg-primary/10 text-primary',
    danger: 'bg-destructive/10 text-destructive',
    success: 'bg-emerald-500/10 text-emerald-600 dark:text-emerald-400',
  }[tone];

  return (
    <Card className='border-border/70 relative overflow-hidden shadow-sm transition-all hover:-translate-y-0.5 hover:shadow-md'>
      <CardContent className='p-5'>
        <div className='flex items-start justify-between gap-4'>
          <div className='min-w-0'>
            <p className='text-muted-foreground text-sm font-medium'>{label}</p>
            {loading ? (
              <Skeleton className='mt-3 h-8 w-20' />
            ) : (
              <p className={`mt-2 text-3xl font-semibold tracking-tight ${unavailable ? 'text-muted-foreground' : ''}`}>
                {unavailable ? '—' : value}
              </p>
            )}
            <p className='text-muted-foreground mt-2 truncate text-xs'>{hint}</p>
          </div>
          <span className={`flex size-10 shrink-0 items-center justify-center rounded-xl ${iconTone}`}>
            <Icon className='size-5' />
          </span>
        </div>
      </CardContent>
    </Card>
  );
}

function ActivitySignal({ requests, loading, error }: { requests: Request[]; loading: boolean; error: boolean }) {
  const { t } = useTranslation();
  const events = [...requests].reverse();
  const maxLatency = Math.max(...events.map((request) => request.metricsLatencyMs ?? 0), 1);

  return (
    <aside className='border-border/60 bg-background/55 relative hidden min-h-40 overflow-hidden rounded-xl border p-4 shadow-sm lg:flex lg:flex-col'>
      <div className='pointer-events-none absolute inset-0 bg-[linear-gradient(to_right,transparent_0,transparent_calc(100%-1px),var(--border)_100%)] bg-[length:16.666%_100%] opacity-35' />
      <div className='relative flex items-start justify-between gap-3'>
        <div>
          <p className='text-xs font-semibold tracking-[0.16em] uppercase'>{t('projectDashboard.signal.title')}</p>
          <p className='text-muted-foreground mt-1 text-[11px]'>
            {error
              ? t('projectDashboard.stats.unavailable')
              : events.length > 0
                ? t('projectDashboard.signal.events', { count: events.length })
                : t('projectDashboard.signal.quiet')}
          </p>
        </div>
        <Radio className='text-primary size-4' />
      </div>
      <div className='relative mt-5 grid flex-1 grid-cols-6 items-end gap-2' aria-label={t('projectDashboard.signal.title')}>
        {loading
          ? Array.from({ length: 6 }).map((_, index) => (
              <Skeleton key={index} className='w-full rounded-sm' style={{ height: `${24 + index * 7}px` }} />
            ))
          : Array.from({ length: 6 }).map((_, index) => {
              const request = events[index];
              const height = request ? 22 + ((request.metricsLatencyMs ?? maxLatency * 0.25) / maxLatency) * 50 : 12;
              return (
                <div
                  key={request?.id ?? `empty-${index}`}
                  className='flex h-full flex-col justify-end gap-2'
                  title={request ? `${request.modelID} · ${request.status}` : undefined}
                >
                  <div
                    className={`bg-primary/12 w-full rounded-sm transition-[height] duration-500 ${request ? '' : 'bg-muted/50'}`}
                    style={{ height: `${Math.min(height, 72)}px` }}
                  />
                  <span className={`mx-auto size-1.5 rounded-full ${request ? signalStyles[request.status] : 'bg-border'}`} />
                </div>
              );
            })}
      </div>
    </aside>
  );
}

function DashboardSkeleton() {
  return (
    <div className='space-y-6'>
      <Skeleton className='h-48 w-full rounded-2xl' />
      <div className='grid gap-4 sm:grid-cols-2 xl:grid-cols-4'>
        {Array.from({ length: 4 }).map((_, index) => (
          <Skeleton key={index} className='h-36 rounded-xl' />
        ))}
      </div>
      <Skeleton className='h-72 w-full rounded-xl' />
    </div>
  );
}

function ServiceHealth({ health, locale }: { health: PublicChannelHealth; locale: string }) {
  const { t } = useTranslation();
  const status = {
    OPERATIONAL: { icon: CircleCheck, tone: 'text-emerald-700 dark:text-emerald-400', badge: 'border-emerald-500/25 bg-emerald-500/10' },
    DEGRADED: { icon: CircleAlert, tone: 'text-amber-700 dark:text-amber-400', badge: 'border-amber-500/25 bg-amber-500/10' },
    DISRUPTED: { icon: CircleAlert, tone: 'text-destructive', badge: 'border-destructive/25 bg-destructive/10' },
    UNKNOWN: { icon: Clock3, tone: 'text-muted-foreground', badge: 'border-border bg-muted/40' },
  }[health.status];
  const StatusIcon = status.icon;
  const updated = health.lastUpdatedAt
    ? new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(new Date(health.lastUpdatedAt))
    : t('projectDashboard.health.noData');

  return (
    <Card className='border-border/70 overflow-hidden shadow-sm'>
      <CardHeader className='border-b py-4 sm:flex-row sm:items-center sm:justify-between'>
        <div>
          <CardTitle className='flex items-center gap-2 text-base'>
            <Gauge className='text-primary size-4' />
            {t('projectDashboard.health.title')}
          </CardTitle>
          <p className='text-muted-foreground mt-1 text-sm'>{t('projectDashboard.health.description')}</p>
        </div>
        <Badge variant='outline' className={`${status.badge} ${status.tone} gap-1.5`}>
          <StatusIcon className='size-3.5' /> {t(`projectDashboard.health.status.${health.status.toLowerCase()}`)}
        </Badge>
      </CardHeader>
      <CardContent className='grid gap-0 p-0 sm:grid-cols-4'>
        {[
          [t('projectDashboard.health.successRate'), health.successRate == null ? '—' : `${health.successRate.toFixed(1)}%`],
          [
            t('projectDashboard.health.ttft'),
            health.avgTimeToFirstTokenMs == null ? '—' : `${Math.round(health.avgTimeToFirstTokenMs)} ms`,
          ],
          [t('projectDashboard.health.tps'), health.avgTokensPerSecond == null ? '—' : health.avgTokensPerSecond.toFixed(1)],
          [t('projectDashboard.health.updated'), updated],
        ].map(([label, value], index) => (
          <div key={String(label)} className={`px-5 py-4 ${index ? 'border-t sm:border-t-0 sm:border-l' : ''}`}>
            <div className='text-muted-foreground text-xs'>{label}</div>
            <div className={`mt-1 font-medium ${index < 3 ? 'font-mono tabular-nums' : 'text-sm'}`}>{value}</div>
          </div>
        ))}
      </CardContent>
    </Card>
  );
}

export default function ProjectDashboard() {
  const { t, i18n } = useTranslation();
  const selectedProjectId = useSelectedProjectId();
  const { data: me } = useMe();
  const { data: projects, isLoading: projectsLoading } = useMyProjects();
  const { checkRouteAccess } = useRoutePermissions();
  const canAccessApiKeys = checkRouteAccess('/project/api-keys').hasAccess;
  const canAccessPlayground = checkRouteAccess('/project/playground').hasAccess;
  const requestsQuery = useRequests({ first: 1 }, { enabled: !!selectedProjectId });
  const failedRequestsQuery = useRequests({ first: 1, where: { status: 'failed' } }, { enabled: !!selectedProjectId });
  const recentRequestsQuery = useRequests(
    { first: 6, orderBy: { field: 'CREATED_AT', direction: 'DESC' } },
    { enabled: !!selectedProjectId }
  );
  const apiKeysQuery = useApiKeys({ first: 1 }, { disableAutoFetch: !canAccessApiKeys || !selectedProjectId });
  const publicHealthQuery = usePublicChannelHealth();

  const workspace = projects?.find((project) => project.id === selectedProjectId);
  const recentRequests = recentRequestsQuery.data?.edges.map((edge) => edge.node) ?? [];
  const isInitialLoading = projectsLoading || (requestsQuery.isLoading && !requestsQuery.data);
  const hasError = requestsQuery.isError || failedRequestsQuery.isError || recentRequestsQuery.isError || apiKeysQuery.isError;
  const hasApiKeys = (apiKeysQuery.data?.totalCount ?? 0) > 0;
  const hasRequests = (requestsQuery.data?.totalCount ?? 0) > 0;
  const displayName = me?.firstName || me?.email?.split('@')[0] || t('projectDashboard.userFallback');
  const numberFormat = new Intl.NumberFormat();
  const dateTimeFormat = new Intl.DateTimeFormat(i18n.resolvedLanguage || i18n.language, {
    month: 'short',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
  });

  if (isInitialLoading)
    return (
      <Main>
        <DashboardSkeleton />
      </Main>
    );

  return (
    <>
      <Header fixed>
        <div className='flex flex-1 items-center gap-3'>
          <div className='bg-primary/10 text-primary flex size-9 items-center justify-center rounded-lg'>
            <LayoutDashboard className='size-4' />
          </div>
          <div>
            <h1 className='text-base font-semibold'>{t('projectDashboard.title')}</h1>
            <p className='text-muted-foreground hidden text-xs sm:block'>{workspace?.name ?? t('projectDashboard.workspaceFallback')}</p>
          </div>
        </div>
      </Header>

      <Main className='space-y-6 pb-10'>
        <section className='border-border/70 bg-card relative grid overflow-hidden rounded-2xl border px-6 py-7 shadow-sm sm:px-8 lg:grid-cols-[minmax(0,1fr)_18rem] lg:items-stretch lg:gap-8'>
          <div className='relative max-w-3xl'>
            <div className='text-muted-foreground mb-5 flex items-center gap-2 text-xs font-medium'>
              <span className='relative flex size-2.5'>
                <span className='absolute inline-flex size-full animate-ping rounded-full bg-emerald-500 opacity-40' />
                <span className='relative inline-flex size-2.5 rounded-full bg-emerald-500' />
              </span>
              {t('projectDashboard.operational')}
              <span aria-hidden='true'>·</span>
              <span className='truncate'>{workspace?.name ?? t('projectDashboard.workspaceFallback')}</span>
            </div>
            <h2 className='text-2xl font-semibold tracking-tight sm:text-3xl'>{t('projectDashboard.welcome', { name: displayName })}</h2>
            <p className='text-muted-foreground mt-2 max-w-xl text-sm leading-6 sm:text-base'>{t('projectDashboard.description')}</p>
            <div className='mt-6 flex flex-wrap gap-3'>
              {canAccessApiKeys && (
                <Button asChild>
                  <Link to='/project/api-keys'>
                    <KeyRound />
                    {t('projectDashboard.actions.apiKeys')}
                  </Link>
                </Button>
              )}
              <Button asChild variant={canAccessApiKeys ? 'outline' : 'default'}>
                <Link to='/project/requests'>
                  <Activity />
                  {t('projectDashboard.actions.requests')}
                </Link>
              </Button>
              {canAccessPlayground && (
                <Button asChild variant='ghost'>
                  <Link to='/project/playground'>
                    <Play />
                    {t('projectDashboard.actions.playground')}
                  </Link>
                </Button>
              )}
            </div>
          </div>
          <ActivitySignal requests={recentRequests} loading={recentRequestsQuery.isLoading} error={recentRequestsQuery.isError} />
        </section>

        {hasError && (
          <div className='border-destructive/20 bg-destructive/5 text-destructive flex items-center gap-3 rounded-xl border px-4 py-3 text-sm'>
            <CircleAlert className='size-4 shrink-0' />
            <span className='flex-1'>{t('projectDashboard.error')}</span>
            <Button
              variant='outline'
              size='sm'
              onClick={() =>
                void Promise.all([
                  requestsQuery.refetch(),
                  failedRequestsQuery.refetch(),
                  recentRequestsQuery.refetch(),
                  ...(canAccessApiKeys ? [apiKeysQuery.refetch()] : []),
                ])
              }
            >
              {t('common.buttons.retry')}
            </Button>
          </div>
        )}

        <section className='grid gap-4 sm:grid-cols-2 xl:grid-cols-4'>
          <StatCard
            icon={Activity}
            label={t('projectDashboard.stats.totalRequests')}
            value={numberFormat.format(requestsQuery.data?.totalCount ?? 0)}
            hint={
              requestsQuery.isError
                ? t('projectDashboard.stats.unavailable')
                : hasRequests
                  ? t('projectDashboard.stats.totalRequestsHint')
                  : t('projectDashboard.stats.emptyHint')
            }
            loading={requestsQuery.isLoading}
            unavailable={requestsQuery.isError}
          />
          <StatCard
            icon={CircleAlert}
            label={t('projectDashboard.stats.failedRequests')}
            value={numberFormat.format(failedRequestsQuery.data?.totalCount ?? 0)}
            hint={
              failedRequestsQuery.isError
                ? t('projectDashboard.stats.unavailable')
                : hasRequests
                  ? t('projectDashboard.stats.failedRequestsHint')
                  : t('projectDashboard.stats.emptyHint')
            }
            tone='danger'
            loading={failedRequestsQuery.isLoading}
            unavailable={failedRequestsQuery.isError}
          />
          <StatCard
            icon={KeyRound}
            label={t('projectDashboard.stats.apiKeys')}
            value={canAccessApiKeys ? numberFormat.format(apiKeysQuery.data?.totalCount ?? 0) : '—'}
            hint={
              apiKeysQuery.isError
                ? t('projectDashboard.stats.unavailable')
                : canAccessApiKeys
                  ? t('projectDashboard.stats.apiKeysHint')
                  : t('projectDashboard.stats.restricted')
            }
            loading={canAccessApiKeys && apiKeysQuery.isLoading}
            unavailable={apiKeysQuery.isError}
          />
          <StatCard
            icon={workspace?.status === 'active' ? CircleCheck : FolderKanban}
            label={t('projectDashboard.stats.workspace')}
            value={
              workspace?.status === 'active'
                ? t('projectDashboard.status.active')
                : workspace?.status === 'archived'
                  ? t('projectDashboard.status.archived')
                  : '—'
            }
            hint={workspace ? workspace.name : t('projectDashboard.stats.unavailable')}
            tone={workspace?.status === 'active' ? 'success' : 'primary'}
            loading={projectsLoading}
            unavailable={!projectsLoading && !workspace}
          />
        </section>

        {publicHealthQuery.data && <ServiceHealth health={publicHealthQuery.data} locale={i18n.resolvedLanguage || i18n.language} />}

        {canAccessApiKeys && apiKeysQuery.isSuccess && !hasApiKeys && (
          <section className='border-primary/20 bg-primary/5 flex flex-col gap-4 rounded-xl border p-5 sm:flex-row sm:items-center sm:justify-between'>
            <div className='flex gap-4'>
              <span className='bg-primary/10 text-primary flex size-10 shrink-0 items-center justify-center rounded-lg'>
                <KeyRound className='size-5' />
              </span>
              <div>
                <h3 className='font-semibold'>{t('projectDashboard.onboarding.noKeysTitle')}</h3>
                <p className='text-muted-foreground mt-1 text-sm'>{t('projectDashboard.onboarding.noKeysDescription')}</p>
              </div>
            </div>
            <Button asChild size='sm'>
              <Link to='/project/api-keys'>
                {t('projectDashboard.onboarding.createKey')}
                <ArrowRight />
              </Link>
            </Button>
          </section>
        )}

        <Card className='border-border/70 overflow-hidden shadow-sm'>
          <CardHeader className='bg-muted/20 border-b sm:flex-row sm:items-center sm:justify-between'>
            <div>
              <CardTitle className='flex items-center gap-2 text-base'>
                <Radio className='text-primary size-4' />
                {t('projectDashboard.recent.title')}
              </CardTitle>
              <p className='text-muted-foreground mt-1 text-sm'>{t('projectDashboard.recent.description')}</p>
            </div>
            {hasRequests && (
              <Button asChild variant='ghost' size='sm'>
                <Link to='/project/requests'>
                  {t('projectDashboard.recent.viewAll')}
                  <ArrowRight />
                </Link>
              </Button>
            )}
          </CardHeader>
          <CardContent className='p-0'>
            {recentRequestsQuery.isLoading ? (
              <div className='space-y-3 p-5'>
                {Array.from({ length: 4 }).map((_, index) => (
                  <Skeleton key={index} className='h-12 w-full' />
                ))}
              </div>
            ) : recentRequestsQuery.isError ? (
              <div className='text-muted-foreground flex items-center justify-center gap-2 px-6 py-12 text-sm'>
                <CircleAlert className='size-4' />
                {t('projectDashboard.recent.unavailable')}
              </div>
            ) : recentRequests.length === 0 ? (
              <div className='flex flex-col items-center px-6 py-12 text-center'>
                <span className='bg-muted text-muted-foreground flex size-12 items-center justify-center rounded-xl'>
                  <Clock3 className='size-5' />
                </span>
                <h3 className='mt-4 font-semibold'>{t('projectDashboard.onboarding.noRequestsTitle')}</h3>
                <p className='text-muted-foreground mt-1 max-w-md text-sm'>{t('projectDashboard.onboarding.noRequestsDescription')}</p>
                {canAccessPlayground && (
                  <Button asChild variant='outline' size='sm' className='mt-4'>
                    <Link to='/project/playground'>
                      {t('projectDashboard.actions.playground')}
                      <ArrowRight />
                    </Link>
                  </Button>
                )}
              </div>
            ) : (
              <div className='divide-y'>
                {recentRequests.map((request) => (
                  <Link
                    key={request.id}
                    to='/project/requests/$requestId'
                    params={{ requestId: request.id }}
                    className='group hover:bg-muted/40 grid gap-3 px-5 py-4 transition-colors sm:grid-cols-[minmax(0,1fr)_auto_auto] sm:items-center'
                  >
                    <div className='min-w-0'>
                      <p className='truncate text-sm font-medium'>{request.modelID}</p>
                      <p className='text-muted-foreground mt-1 truncate text-xs'>
                        {request.apiKey?.name ?? t('projectDashboard.recent.unknownKey')} · {request.source}
                      </p>
                    </div>
                    <Badge variant='outline' className={statusStyles[request.status]}>
                      {t(`projectDashboard.requestStatus.${request.status}`)}
                    </Badge>
                    <div className='text-muted-foreground flex items-center gap-3 text-xs sm:min-w-36 sm:justify-end'>
                      <span>{dateTimeFormat.format(request.createdAt)}</span>
                      <ArrowRight className='size-3.5 transition-transform group-hover:translate-x-0.5' />
                    </div>
                  </Link>
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </Main>
    </>
  );
}
