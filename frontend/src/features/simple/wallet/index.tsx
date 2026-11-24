import { useState } from 'react';
import { Link } from '@tanstack/react-router';
import { IconTicket } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { useAuthStore } from '@/stores/authStore';
import { useSelectedProjectId } from '@/stores/projectStore';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Progress } from '@/components/ui/progress';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { EmptyState, ErrorState, LoadingRows, PageHeading } from '@/components/page-state';
import { useMyProjectBalance, useMySubscriptions } from '@/features/billing/data';
import { PaymentTopupPanel } from '@/features/wallet/components/payment-topup-panel';
import { RedeemCodeDialog } from '@/features/wallet/components/redeem-code-dialog';
import { formatFullTime, formatRelativeTime } from '../lib/format';
import { useMoney } from '../lib/use-money';

const KNOWN_ENTRY_TYPES = ['admin_grant', 'redemption', 'usage_charge', 'refund', 'adjustment', 'subscription'];
const ACTIVE_SUBSCRIPTION = ['active', 'cancel_pending', 'paused'];

function toNumber(value: string | number | null | undefined) {
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : 0;
}

/** 钱包：一个大数字（可用余额），一个兑换码按钮，下面是流水和套餐。 */
export function SimpleWallet() {
  const { t, i18n } = useTranslation();
  const money = useMoney();
  const payerID = useAuthStore((s) => s.auth.user?.id);
  const projectID = useSelectedProjectId();
  const [redeemOpen, setRedeemOpen] = useState(false);
  const balanceQuery = useMyProjectBalance();
  const subscriptionsQuery = useMySubscriptions();
  const balance = balanceQuery.data?.myProjectBalance ?? null;
  const subscriptions = (subscriptionsQuery.data?.mySubscriptions ?? []).filter((s) =>
    ACTIVE_SUBSCRIPTION.includes(s.status.toLowerCase())
  );
  const ledger = balance?.ledgerEntries ?? [];

  return (
    <>
      <Header fixed>
        <PageHeading
          title={t('simple.wallet.title')}
          description={t('simple.wallet.description')}
          action={
            <Button onClick={() => setRedeemOpen(true)}>
              <IconTicket className='size-4' /> {t('simple.wallet.redeem')}
            </Button>
          }
        />
      </Header>
      <Main className='space-y-4'>
        {payerID && projectID && (
          <PaymentTopupPanel key={`${payerID}:${projectID}`} payerID={payerID} projectID={projectID} creditDisplayName={money.label} />
        )}
        <Card>
          <CardContent className='pt-6'>
            {balanceQuery.isLoading ? (
              <LoadingRows rows={2} />
            ) : balanceQuery.isError ? (
              <ErrorState onRetry={() => balanceQuery.refetch()} />
            ) : (
              <div className='space-y-2'>
                <p className='text-muted-foreground text-sm'>{t('simple.wallet.available')}</p>
                <p className='text-4xl font-semibold tracking-tight tabular-nums' title={money.credits(balance?.availableBalance).exact}>
                  {money.credits(balance?.availableBalance ?? 0).text}
                </p>
                <p className='text-muted-foreground text-sm'>
                  {t('simple.wallet.sources', {
                    credit: money.credits(balance?.creditBalance ?? 0).text,
                    subscription: money.credits(balance?.subscriptionBalance ?? 0).text,
                  })}
                  {toNumber(balance?.reservedBalance) > 0 &&
                    ` ${t('simple.wallet.reserved', { amount: money.credits(balance?.reservedBalance).text })}`}
                </p>
                {balance && balance.walletStatus.toLowerCase() !== 'active' && (
                  <p className='text-sm text-amber-600'>{t('simple.wallet.inactive')}</p>
                )}
                <p className='text-muted-foreground text-xs'>{t('simple.wallet.topUpHint')}</p>
              </div>
            )}
          </CardContent>
        </Card>

        <Tabs defaultValue='ledger'>
          <TabsList>
            <TabsTrigger value='ledger'>{t('simple.wallet.tabs.ledger')}</TabsTrigger>
            <TabsTrigger value='plans'>{t('simple.wallet.tabs.plans', { count: subscriptions.length })}</TabsTrigger>
          </TabsList>

          <TabsContent value='ledger'>
            <Card>
              <CardHeader>
                <CardDescription>
                  {t('simple.wallet.ledgerHint')}{' '}
                  <Link to='/project/requests' className='text-primary underline-offset-4 hover:underline'>
                    {t('simple.wallet.toUsageLogs')}
                  </Link>
                </CardDescription>
              </CardHeader>
              <CardContent>
                {balanceQuery.isLoading ? (
                  <LoadingRows />
                ) : ledger.length === 0 ? (
                  <EmptyState title={t('simple.wallet.ledgerEmpty')} />
                ) : (
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead>{t('simple.wallet.columns.time')}</TableHead>
                        <TableHead>{t('simple.wallet.columns.type')}</TableHead>
                        <TableHead>{t('simple.wallet.columns.note')}</TableHead>
                        <TableHead className='text-right'>{t('simple.wallet.columns.amount')}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      {ledger.map((entry) => {
                        const amount = toNumber(entry.amount);
                        return (
                          <TableRow key={entry.id}>
                            <TableCell className='whitespace-nowrap' title={formatFullTime(entry.createdAt, i18n.language)}>
                              {formatRelativeTime(entry.createdAt, i18n.language, t)}
                            </TableCell>
                            <TableCell>
                              {KNOWN_ENTRY_TYPES.includes(entry.entryType)
                                ? t(`simple.wallet.entryType.${entry.entryType}`)
                                : entry.entryType}
                            </TableCell>
                            <TableCell className='text-muted-foreground max-w-64 truncate'>{entry.description || '—'}</TableCell>
                            <TableCell className={cn('text-right tabular-nums', amount > 0 ? 'text-emerald-600' : '')}>
                              {amount > 0 ? '+' : ''}
                              {money.credits(entry.amount).text}
                            </TableCell>
                          </TableRow>
                        );
                      })}
                    </TableBody>
                  </Table>
                )}
                {ledger.length >= 50 && <p className='text-muted-foreground mt-3 text-xs'>{t('simple.wallet.ledgerLimit')}</p>}
              </CardContent>
            </Card>
          </TabsContent>

          <TabsContent value='plans'>
            {subscriptionsQuery.isLoading ? (
              <LoadingRows />
            ) : subscriptions.length === 0 ? (
              <Card>
                <CardContent className='pt-6'>
                  <EmptyState title={t('simple.wallet.plansEmpty')} description={t('simple.wallet.plansEmptyDescription')} />
                </CardContent>
              </Card>
            ) : (
              <div className='grid gap-4 md:grid-cols-2'>
                {subscriptions.map((sub) => {
                  const granted = toNumber(sub.grantedAllowance);
                  const remaining = toNumber(sub.remainingAllowance);
                  const percent = granted > 0 ? Math.max(0, Math.min(100, (remaining / granted) * 100)) : 0;
                  return (
                    <Card key={sub.id}>
                      <CardHeader>
                        <CardTitle className='text-base'>{sub.plan.name}</CardTitle>
                        <CardDescription>
                          {sub.autoRenew
                            ? t('simple.wallet.plan.renews', { date: formatFullTime(sub.currentPeriodEnd, i18n.language) })
                            : t('simple.wallet.plan.ends', { date: formatFullTime(sub.currentPeriodEnd, i18n.language) })}
                        </CardDescription>
                      </CardHeader>
                      <CardContent className='space-y-2'>
                        <div className='flex items-baseline justify-between text-sm'>
                          <span className='text-muted-foreground'>{t('simple.wallet.plan.remaining')}</span>
                          <span className='tabular-nums'>
                            {money.credits(sub.remainingAllowance).text} / {money.credits(sub.grantedAllowance).text}
                          </span>
                        </div>
                        <Progress value={percent} className='h-1.5' />
                        {sub.status.toLowerCase() === 'paused' && (
                          <p className='text-xs text-amber-600'>{t('simple.wallet.plan.paused')}</p>
                        )}
                        {sub.grantedModelIDs.length > 0 && (
                          <p className='text-muted-foreground text-xs'>
                            {t('simple.wallet.plan.models', { count: sub.grantedModelIDs.length })}
                          </p>
                        )}
                      </CardContent>
                    </Card>
                  );
                })}
              </div>
            )}
          </TabsContent>
        </Tabs>
      </Main>

      <RedeemCodeDialog open={redeemOpen} onOpenChange={setRedeemOpen} creditDisplayName={money.label} />
    </>
  );
}
