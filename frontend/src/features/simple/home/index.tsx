import type React from 'react';
import { useMemo } from 'react';
import { Link } from '@tanstack/react-router';
import { IconCircleCheck, IconCircleDashed, IconPlus } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { usePermissions } from '@/hooks/usePermissions';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { CopyButton } from '@/components/ui/copy-button';
import { Skeleton } from '@/components/ui/skeleton';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { EmptyState, PageHeading } from '@/components/page-state';
import { useMyProjectBalance } from '@/features/billing/data';
import { StatusLabel } from '../components/status-label';
import { useSimpleTokens } from '../data/tokens';
import { usageOf, useSimpleRequestCounts, useSimpleRequests } from '../data/usage';
import { formatFullTime, formatRelativeTime, startOfToday } from '../lib/format';
import { useMoney } from '../lib/use-money';

function lastSevenDays() {
  const today = startOfToday();
  return Array.from({ length: 7 }, (_, index) => {
    const from = new Date(today);
    from.setDate(today.getDate() - (6 - index));
    const to = new Date(from);
    to.setDate(from.getDate() + 1);
    to.setMilliseconds(-1);
    return { from, to };
  });
}

function StatCard({
  title,
  value,
  sub,
  href,
  loading,
}: {
  title: string;
  value: string;
  sub?: React.ReactNode;
  href?: '/project/wallet' | '/project/requests';
  loading?: boolean;
}) {
  const body = (
    <Card className={cn('h-full', href && 'hover:border-primary/40 transition-colors')}>
      <CardHeader className='pb-2'>
        <CardDescription>{title}</CardDescription>
        {loading ? <Skeleton className='h-8 w-28' /> : <CardTitle className='text-2xl tabular-nums'>{value}</CardTitle>}
      </CardHeader>
      {sub && <CardContent className='text-muted-foreground text-xs'>{sub}</CardContent>}
    </Card>
  );
  return href ? (
    <Link to={href} className='block'>
      {body}
    </Link>
  ) : (
    body
  );
}

/** 首页：余额、今天花了多少、今天调了几次；没上手时先给三步引导。 */
export function SimpleHome() {
  const { t, i18n } = useTranslation();
  const money = useMoney();
  const { apiKeyPermissions } = usePermissions();
  const origin = typeof window !== 'undefined' ? window.location.origin : '';
  const days = useMemo(lastSevenDays, []);
  const today = days[days.length - 1];

  const balance = useMyProjectBalance();
  const tokens = useSimpleTokens({ first: 1 });
  const todayRequests = useSimpleRequests({ from: today.from }, { first: 100 });
  const recent = useSimpleRequests({}, { first: 5 });
  const dayCounts = useSimpleRequestCounts(days.map((day) => ({ from: day.from, to: day.to })));
  const [todayFailed] = useSimpleRequestCounts([{ from: today.from, failedOnly: true }]);

  const todayRows = todayRequests.data?.edges.map((edge) => edge.node) ?? [];
  const todayCost = todayRows.reduce((sum, row) => sum + (Number(usageOf(row)?.totalCost ?? 0) || 0), 0);
  const todayCostTruncated = todayRequests.data?.pageInfo.hasNextPage ?? false;
  const hasToken = (tokens.data?.totalCount ?? 0) > 0;
  const hasRequest = (recent.data?.edges.length ?? 0) > 0;
  const showOnboarding = !tokens.isLoading && !recent.isLoading && (!hasToken || !hasRequest);
  const counts = dayCounts.map((query) => query.data ?? 0);
  const maxCount = Math.max(1, ...counts);

  const newTokenButton = apiKeyPermissions.canWrite && (
    <Button asChild>
      <Link to='/project/api-keys'>
        <IconPlus className='size-4' /> {t('simple.tokens.create')}
      </Link>
    </Button>
  );

  return (
    <>
      <Header fixed>
        <PageHeading title={t('simple.home.title')} description={t('simple.home.description')} action={newTokenButton} />
      </Header>
      <Main className='space-y-4'>
        {showOnboarding && (
          <Card>
            <CardHeader>
              <CardTitle className='text-base'>{t('simple.home.onboarding.title')}</CardTitle>
            </CardHeader>
            <CardContent>
              <ol className='grid gap-3 md:grid-cols-3'>
                {[
                  { done: hasToken, title: t('simple.home.onboarding.step1'), hint: t('simple.home.onboarding.step1Hint') },
                  { done: hasToken, title: t('simple.home.onboarding.step2'), hint: t('simple.home.onboarding.step2Hint') },
                  { done: hasRequest, title: t('simple.home.onboarding.step3'), hint: t('simple.home.onboarding.step3Hint') },
                ].map((step, index) => (
                  <li key={index} className='flex gap-3 rounded-md border p-3'>
                    {step.done ? (
                      <IconCircleCheck className='size-5 shrink-0 text-emerald-600' />
                    ) : (
                      <IconCircleDashed className='text-muted-foreground size-5 shrink-0' />
                    )}
                    <div>
                      <p className='text-sm font-medium'>{step.title}</p>
                      <p className='text-muted-foreground text-xs'>{step.hint}</p>
                    </div>
                  </li>
                ))}
              </ol>
            </CardContent>
          </Card>
        )}

        <div className='grid gap-4 md:grid-cols-3'>
          <StatCard
            title={t('simple.home.cards.balance')}
            value={money.credits(balance.data?.myProjectBalance?.availableBalance ?? 0).text}
            sub={t('simple.home.cards.balanceSub')}
            href='/project/wallet'
            loading={balance.isLoading}
          />
          <StatCard
            title={t('simple.home.cards.todayCost')}
            value={`${todayCostTruncated ? '≥ ' : ''}${money.cost(todayCost).text}`}
            sub={todayCostTruncated ? t('simple.home.cards.todayCostTruncated') : undefined}
            href='/project/requests'
            loading={todayRequests.isLoading}
          />
          <StatCard
            title={t('simple.home.cards.todayRequests')}
            value={String(todayRequests.data?.totalCount ?? 0)}
            sub={
              (todayFailed?.data ?? 0) > 0 ? (
                <span className='text-red-600'>{t('simple.home.cards.failed', { count: todayFailed?.data ?? 0 })}</span>
              ) : (
                t('simple.home.cards.noFailures')
              )
            }
            href='/project/requests'
            loading={todayRequests.isLoading}
          />
        </div>

        <Card>
          <CardHeader className='pb-3'>
            <CardTitle className='text-base'>{t('simple.home.baseUrl.title')}</CardTitle>
            <CardDescription>{t('simple.home.baseUrl.description')}</CardDescription>
          </CardHeader>
          <CardContent className='grid gap-2 md:grid-cols-2'>
            {[
              { label: t('simple.home.baseUrl.openai'), url: `${origin}/v1` },
              { label: t('simple.home.baseUrl.anthropic'), url: `${origin}/anthropic` },
            ].map((item) => (
              <div key={item.url} className='bg-muted flex items-center gap-2 rounded-md px-3 py-2'>
                <span className='text-muted-foreground shrink-0 text-xs'>{item.label}</span>
                <code className='flex-1 truncate font-mono text-sm'>{item.url}</code>
                <CopyButton content={item.url} />
              </div>
            ))}
          </CardContent>
        </Card>

        <div className='grid gap-4 lg:grid-cols-2'>
          <Card>
            <CardHeader className='pb-3'>
              <CardTitle className='text-base'>{t('simple.home.chart.title')}</CardTitle>
            </CardHeader>
            <CardContent>
              <div className='flex h-40 items-end gap-2' role='img' aria-label={t('simple.home.chart.title')}>
                {days.map((day, index) => (
                  <div key={day.from.toISOString()} className='flex flex-1 flex-col items-center gap-1'>
                    <span className='text-muted-foreground text-xs tabular-nums'>{counts[index]}</span>
                    <div
                      className='bg-primary/80 w-full rounded-t'
                      style={{ height: `${Math.max(2, (counts[index] / maxCount) * 112)}px` }}
                      title={`${formatFullTime(day.from, i18n.language)}: ${counts[index]}`}
                    />
                    <span className='text-muted-foreground text-xs'>
                      {new Intl.DateTimeFormat(i18n.language, { weekday: 'short' }).format(day.from)}
                    </span>
                  </div>
                ))}
              </div>
            </CardContent>
          </Card>

          <Card>
            <CardHeader className='flex flex-row items-center justify-between pb-3'>
              <CardTitle className='text-base'>{t('simple.home.recent.title')}</CardTitle>
              <Link to='/project/requests' className='text-primary text-sm underline-offset-4 hover:underline'>
                {t('simple.home.recent.all')}
              </Link>
            </CardHeader>
            <CardContent>
              {recent.isLoading ? (
                <Skeleton className='h-32 w-full' />
              ) : !hasRequest ? (
                <EmptyState title={t('simple.usage.emptyTitle')} description={t('simple.usage.emptyDescription')} className='py-6' />
              ) : (
                <ul className='divide-y'>
                  {recent.data?.edges.map(({ node }) => (
                    <li key={node.id} className='flex items-center justify-between gap-3 py-2 text-sm'>
                      <div className='min-w-0'>
                        <p className='truncate font-mono text-xs'>{node.modelID}</p>
                        <p className='text-muted-foreground text-xs'>{formatRelativeTime(node.createdAt, i18n.language, t)}</p>
                      </div>
                      <div className='flex shrink-0 items-center gap-3'>
                        <span className='tabular-nums'>{money.cost(usageOf(node)?.totalCost).text}</span>
                        <StatusLabel status={node.status} />
                      </div>
                    </li>
                  ))}
                </ul>
              )}
            </CardContent>
          </Card>
        </div>
      </Main>
    </>
  );
}
