import { useEffect, useMemo, useState, type FormEvent } from 'react';
import {
  IconAlertCircle,
  IconArrowDownRight,
  IconCalendarRepeat,
  IconCashBanknote,
  IconChevronDown,
  IconCoins,
  IconEqual,
  IconMinus,
  IconPencil,
  IconPlus,
  IconRefresh,
  IconReceipt2,
  IconShieldLock,
  IconStack2,
  IconTrash,
  IconWallet,
} from '@tabler/icons-react';
import { Check, ChevronsUpDown, Loader2 } from 'lucide-react';
import { nanoid } from 'nanoid';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { DEFAULT_CREDIT_DISPLAY_NAME } from '@/lib/accounting';
import { cn } from '@/lib/utils';
import { usePermissions } from '@/hooks/usePermissions';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Collapsible, CollapsibleContent, CollapsibleTrigger } from '@/components/ui/collapsible';
import { Command, CommandEmpty, CommandGroup, CommandInput, CommandItem, CommandList } from '@/components/ui/command';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Switch } from '@/components/ui/switch';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { useGeneralSettings } from '@/features/system/data/system';
import type { User } from '@/features/users/data/schema';
import { useUsers } from '@/features/users/data/users';
import { accessPlanIDsForEdit, mergeAccessPlanOptions, normalizeAccessPlanIDs, toggleAccessPlanID } from './access-plan-selection';
import {
  type ProjectBalance,
  type ProjectWalletComparison,
  type QuotaClass,
  type SubscriptionAllowanceBucket,
  type SubscriptionAccessPlan,
  type SubscriptionPlan,
  type SubscriptionQuotaRuleInput,
  type UserSubscription,
  useAssignUserSubscription,
  useBillingAccessBundles,
  useCreateSubscriptionPlan,
  useGrantProjectCredit,
  useProjectBalance,
  useProjectWalletComparison,
  useRefreshSubscriptionAllowance,
  useSetSubscriptionAutoRenew,
  useSubscriptionLifecycle,
  useSubscriptionPlans,
  useSubscriptionProjects,
  useUpdateSubscriptionPlan,
  useUserSubscriptions,
} from './data';
import { FundingOrderStrip } from './funding-order-strip';
import { activeAllowanceBuckets, bucketTotalsByClass, planAllowance, planTotalsByClass, quotaRulesForPlan } from './quota-buckets';

const MONEY_PATTERN = /^\d+(?:\.\d{1,6})?$/;

function displayAmount(creditDisplayName: string, value: string | undefined) {
  if (!value) return '—';
  const normalized = value.includes('.') ? value.replace(/0+$/, '').replace(/\.$/, '') : value;
  return `${creditDisplayName} ${normalized}`;
}

function displayDate(value: string, locale: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(date);
}

function errorMessage(error: unknown, fallback: string) {
  return error instanceof Error ? error.message : fallback;
}

function AccessPlanList({ accessPlans, emptyLabel }: { accessPlans: SubscriptionAccessPlan[]; emptyLabel: string }) {
  if (!accessPlans.length) return <span>{emptyLabel}</span>;
  return (
    <div className='flex flex-wrap gap-1'>
      {accessPlans.map((accessPlan) => (
        <Badge key={accessPlan.id} variant='outline' className='font-normal'>
          {accessPlan.name}
        </Badge>
      ))}
    </div>
  );
}

export default function BillingPage() {
  const { t, i18n } = useTranslation();
  const generalSettingsQuery = useGeneralSettings();
  const creditDisplayName = generalSettingsQuery.data?.creditDisplayName?.trim() || DEFAULT_CREDIT_DISPLAY_NAME;
  const { hasSystemScope } = usePermissions();
  const canReadUsers = hasSystemScope('read_users');
  const canReadBilling = hasSystemScope('read_billing');
  const canReadSubscriptions = hasSystemScope('read_subscriptions');
  const canGrantCredit = hasSystemScope('grant_credit');
  const canWriteSubscriptions = hasSystemScope('write_subscriptions');
  const [userSearch, setUserSearch] = useState('');
  const [debouncedUserSearch, setDebouncedUserSearch] = useState('');
  const [userPickerOpen, setUserPickerOpen] = useState(false);
  const [selectedUser, setSelectedUser] = useState<User>();
  useEffect(() => {
    const timer = window.setTimeout(() => setDebouncedUserSearch(userSearch.trim()), 300);
    return () => window.clearTimeout(timer);
  }, [userSearch]);
  const usersQuery = useUsers(
    {
      first: 20,
      where: debouncedUserSearch
        ? {
            or: [
              { emailContainsFold: debouncedUserSearch },
              { firstNameContainsFold: debouncedUserSearch },
              { lastNameContainsFold: debouncedUserSearch },
            ],
          }
        : undefined,
    },
    { disableAutoFetch: !canReadUsers }
  );
  const plansQuery = useSubscriptionPlans(canReadSubscriptions);
  const users = useMemo(() => usersQuery.data?.edges.map((edge) => edge.node) || [], [usersQuery.data]);
  const [userID, setUserID] = useState('');
  const projectsQuery = useSubscriptionProjects(userID);
  const projects = useMemo(
    () => (projectsQuery.data?.subscriptionProjects || []).filter((project) => project.status.toLowerCase() === 'active'),
    [projectsQuery.data]
  );
  const [projectID, setProjectID] = useState('');
  useEffect(() => {
    if (projects.length === 1) setProjectID(projects[0].id);
    else if (!projects.some((project) => project.id === projectID)) setProjectID('');
  }, [projectID, projects, userID]);
  const balanceQuery = useProjectBalance(projectID, canReadBilling);
  const subscriptionsQuery = useUserSubscriptions(userID, canReadSubscriptions);
  const balance = balanceQuery.data?.projectBalance || undefined;
  const subscriptions = (subscriptionsQuery.data?.userSubscriptions || []).filter(
    (subscription) => !projectID || subscription.projectID === projectID
  );
  const currentSubscription = subscriptions.find((item) => item.status.toLowerCase() === 'active') || subscriptions[0];

  const isLoading = (canReadUsers && usersQuery.isLoading) || (canReadSubscriptions && plansQuery.isLoading);
  const loadError = (canReadUsers && usersQuery.error) || (canReadSubscriptions && plansQuery.error);

  return (
    <>
      <Header fixed>
        <div className='flex min-w-0 flex-1 items-center justify-between gap-4'>
          <div className='min-w-0'>
            <h2 className='flex items-center gap-2 text-xl font-bold tracking-tight'>
              <IconWallet className='text-emerald-600' size={22} />
              {t('billing.title')}
            </h2>
            <p className='text-muted-foreground truncate text-sm'>{t('billing.description')}</p>
          </div>
        </div>
      </Header>

      <Main className='space-y-5 pb-10'>
        {loadError && (
          <Alert variant='destructive'>
            <IconAlertCircle />
            <AlertTitle>{t('billing.errors.loadTitle')}</AlertTitle>
            <AlertDescription>
              <p>{t('billing.errors.load')}</p>
              <Button
                size='sm'
                variant='outline'
                className='mt-2'
                onClick={() => {
                  usersQuery.refetch();
                  plansQuery.refetch();
                }}
              >
                <IconRefresh />
                {t('billing.retry')}
              </Button>
            </AlertDescription>
          </Alert>
        )}

        {canReadUsers ? (
          <section className='flex flex-col gap-3 border-b pb-5 xl:flex-row xl:items-end xl:justify-between'>
            <div className='min-w-0 space-y-1.5'>
              <Label htmlFor='billing-user'>{t('billing.user.label')}</Label>
              <Popover open={userPickerOpen} onOpenChange={setUserPickerOpen}>
                <PopoverTrigger asChild>
                  <Button
                    id='billing-user'
                    variant='outline'
                    role='combobox'
                    aria-expanded={userPickerOpen}
                    className='h-auto min-h-10 w-full justify-between px-3 font-normal sm:w-[460px]'
                  >
                    {selectedUser ? (
                      <span className='min-w-0 text-left'>
                        <span className='block truncate font-medium'>{selectedUser.email}</span>
                        <span className='text-muted-foreground block truncate text-xs'>
                          {[selectedUser.firstName, selectedUser.lastName].filter(Boolean).join(' ') || selectedUser.id}
                        </span>
                      </span>
                    ) : (
                      <span className='text-muted-foreground'>{t('billing.user.placeholder')}</span>
                    )}
                    <ChevronsUpDown className='ml-2 size-4 shrink-0 opacity-50' />
                  </Button>
                </PopoverTrigger>
                <PopoverContent className='w-[min(460px,calc(100vw-2rem))] p-0' align='start'>
                  <Command shouldFilter={false}>
                    <CommandInput value={userSearch} onValueChange={setUserSearch} placeholder={t('billing.user.searchPlaceholder')} />
                    <CommandList>
                      {usersQuery.isFetching ? (
                        <div className='text-muted-foreground flex items-center justify-center gap-2 py-6 text-sm'>
                          <Loader2 className='size-4 animate-spin' /> {t('billing.loadingUsers')}
                        </div>
                      ) : (
                        <>
                          <CommandEmpty>{t('billing.user.noResults')}</CommandEmpty>
                          <CommandGroup>
                            {users.map((user) => {
                              const name = [user.firstName, user.lastName].filter(Boolean).join(' ');
                              return (
                                <CommandItem
                                  key={user.id}
                                  value={user.id}
                                  onSelect={() => {
                                    setSelectedUser(user);
                                    setUserID(user.id);
                                    setUserPickerOpen(false);
                                  }}
                                >
                                  <Check className={cn('size-4', user.id === userID ? 'opacity-100' : 'opacity-0')} />
                                  <span className='min-w-0'>
                                    <span className='block truncate font-medium'>{user.email}</span>
                                    <span className='text-muted-foreground block truncate text-xs'>{name || user.id}</span>
                                  </span>
                                </CommandItem>
                              );
                            })}
                          </CommandGroup>
                        </>
                      )}
                    </CommandList>
                  </Command>
                </PopoverContent>
              </Popover>
              {selectedUser && <p className='text-foreground text-sm font-medium break-all'>{selectedUser.email}</p>}
            </div>
            {selectedUser && (
              <p className='text-muted-foreground text-xs break-all xl:max-w-[42%] xl:text-right'>
                {t('billing.user.account')}: <span className='text-foreground font-mono'>{selectedUser.id}</span>
              </p>
            )}
            {selectedUser && (
              <div className='min-w-0 space-y-1.5'>
                <Label>{t('billing.projectWallet.project')}</Label>
                <Select value={projectID} onValueChange={setProjectID} disabled={projectsQuery.isLoading || !projects.length}>
                  <SelectTrigger className='w-full sm:w-[360px]'>
                    <SelectValue placeholder={t('billing.projectWallet.projectPlaceholder')} />
                  </SelectTrigger>
                  <SelectContent>
                    {projects.map((project) => (
                      <SelectItem key={project.id} value={project.id}>
                        {project.name}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </div>
            )}
          </section>
        ) : (
          <Alert>
            <IconShieldLock />
            <AlertTitle>{t('billing.permissions.userDirectoryTitle')}</AlertTitle>
            <AlertDescription>{t('billing.permissions.userDirectoryDescription')}</AlertDescription>
          </Alert>
        )}

        {canReadUsers &&
          (isLoading ? (
            <BillingSkeleton />
          ) : !userID ? (
            <EmptyState icon={<IconWallet />} title={t('billing.selectUser.title')} description={t('billing.selectUser.description')} />
          ) : !projectID ? (
            <EmptyState
              icon={<IconWallet />}
              title={t('billing.projectWallet.project')}
              description={t('billing.projectWallet.chooseProject')}
            />
          ) : (
            <>
              {canReadBilling && (
                <BalanceSummary balance={balance} loading={balanceQuery.isLoading} creditDisplayName={creditDisplayName} />
              )}

              {((canReadBilling && balanceQuery.error) || (canReadSubscriptions && subscriptionsQuery.error)) && (
                <Alert variant='destructive'>
                  <IconAlertCircle />
                  <AlertTitle>{t('billing.errors.accountTitle')}</AlertTitle>
                  <AlertDescription>
                    <p>{t('billing.errors.account')}</p>
                    <Button
                      size='sm'
                      variant='outline'
                      className='mt-2'
                      onClick={() => {
                        if (canReadBilling) balanceQuery.refetch();
                        if (canReadSubscriptions) subscriptionsQuery.refetch();
                      }}
                    >
                      <IconRefresh />
                      {t('billing.retry')}
                    </Button>
                  </AlertDescription>
                </Alert>
              )}

              <div className='grid items-start gap-5 xl:grid-cols-[minmax(0,1.5fr)_minmax(340px,0.8fr)]'>
                <div className='space-y-5'>
                  {canReadSubscriptions &&
                    (subscriptionsQuery.isLoading ? (
                      <CurrentSubscription
                        loading
                        locale={i18n.language}
                        userID={userID}
                        canWrite={canWriteSubscriptions}
                        creditDisplayName={creditDisplayName}
                      />
                    ) : subscriptions.length ? (
                      <div className='space-y-4'>
                        {subscriptions.map((subscription) => (
                          <CurrentSubscription
                            key={subscription.id}
                            subscription={subscription}
                            loading={false}
                            locale={i18n.language}
                            userID={userID}
                            canWrite={canWriteSubscriptions}
                            creditDisplayName={creditDisplayName}
                          />
                        ))}
                      </div>
                    ) : (
                      <CurrentSubscription
                        loading={false}
                        locale={i18n.language}
                        userID={userID}
                        canWrite={canWriteSubscriptions}
                        creditDisplayName={creditDisplayName}
                      />
                    ))}
                  {canReadBilling && (
                    <Ledger
                      balance={balance}
                      loading={balanceQuery.isLoading}
                      locale={i18n.language}
                      creditDisplayName={creditDisplayName}
                    />
                  )}
                </div>
                <OperatorActions
                  userID={userID}
                  creditDisplayName={creditDisplayName}
                  plans={plansQuery.data?.subscriptionPlans || []}
                  currentSubscription={currentSubscription}
                  projectID={projectID}
                  canGrantCredit={canGrantCredit}
                  canWriteSubscriptions={canWriteSubscriptions}
                />
              </div>
            </>
          ))}

        {canReadSubscriptions && !plansQuery.isLoading && (
          <PlansSection
            plans={plansQuery.data?.subscriptionPlans || []}
            canWrite={canWriteSubscriptions}
            creditDisplayName={creditDisplayName}
          />
        )}
      </Main>
    </>
  );
}

function BalanceSummary({
  balance,
  loading,
  creditDisplayName,
}: {
  balance?: ProjectBalance;
  loading: boolean;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const items = [
    {
      label: t('billing.summary.generalQuota'),
      value: balance?.generalSubscriptionBalance ?? balance?.subscriptionBalance,
      icon: IconCalendarRepeat,
      tone: 'text-foreground',
    },
    {
      label: t('billing.summary.dedicatedQuota'),
      value: balance?.dedicatedSubscriptionBalance ?? '0',
      icon: IconStack2,
      tone: 'text-foreground',
    },
    {
      label: t('billing.summary.stationCredit', { name: creditDisplayName }),
      value: balance?.creditBalance,
      icon: IconCoins,
      tone: 'text-foreground',
    },
  ];

  return (
    <section className='space-y-3'>
      <div className='grid gap-3 sm:grid-cols-2 xl:grid-cols-4'>
        <Card className='border-foreground/20 gap-3 py-4 shadow-none md:col-span-1'>
          <CardHeader className='gap-1 px-4'>
            <CardDescription className='flex items-center gap-2 text-xs font-medium tracking-wide uppercase'>
              <IconWallet size={16} className='text-foreground' />
              {t('billing.summary.available')}
            </CardDescription>
            {loading ? (
              <Skeleton className='mt-1 h-8 w-36' />
            ) : (
              <CardTitle className='font-mono text-2xl tracking-tight tabular-nums'>
                {displayAmount(creditDisplayName, balance?.availableBalance)}
              </CardTitle>
            )}
          </CardHeader>
          <CardContent className='px-4'>
            <div className='text-muted-foreground flex items-center gap-1.5 border-t border-dashed pt-3 text-[11px]'>
              <IconShieldLock size={13} />
              <span>{t('billing.summary.reserved')}</span>
              <span className='ml-auto font-mono tabular-nums'>{displayAmount(creditDisplayName, balance?.reservedBalance)}</span>
            </div>
          </CardContent>
        </Card>
        {items.map((item) => (
          <Card key={item.label} className='gap-3 py-4 shadow-none'>
            <CardHeader className='gap-1 px-4'>
              <CardDescription className='flex items-center gap-2 text-xs font-medium tracking-wide uppercase'>
                <item.icon size={16} className={item.tone} />
                {item.label}
              </CardDescription>
              {loading ? (
                <Skeleton className='mt-1 h-8 w-32' />
              ) : (
                <CardTitle className='font-mono text-xl tracking-tight tabular-nums'>
                  {displayAmount(creditDisplayName, item.value)}
                </CardTitle>
              )}
            </CardHeader>
          </Card>
        ))}
      </div>
      <FundingOrderStrip creditDisplayName={creditDisplayName} />
    </section>
  );
}

function CurrentSubscription({
  subscription,
  loading,
  locale,
  userID,
  canWrite,
  creditDisplayName,
}: {
  subscription?: UserSubscription;
  loading: boolean;
  locale: string;
  userID: string;
  canWrite: boolean;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const lifecycle = useSubscriptionLifecycle();
  const autoRenewMutation = useSetSubscriptionAutoRenew();
  const status = subscription?.status.toLowerCase();

  const runLifecycle = async (action: 'pause' | 'resume' | 'cancel' | 'renew') => {
    if (!subscription) return;
    if (action === 'cancel' && !window.confirm(t('billing.lifecycle.cancelConfirm'))) return;
    try {
      await lifecycle.mutateAsync({ action, subscriptionID: subscription.id, userID });
      toast.success(t(`billing.lifecycle.${action}Success`));
    } catch (error) {
      toast.error(errorMessage(error, t('billing.lifecycle.error')));
    }
  };
  return (
    <Card className='gap-4 py-5 shadow-none'>
      <CardHeader className='px-5'>
        <div className='flex items-start justify-between gap-3'>
          <div>
            <CardTitle>{t('billing.subscription.title')}</CardTitle>
            <CardDescription className='mt-1'>{t('billing.subscription.description')}</CardDescription>
          </div>
          {subscription && <Badge variant='secondary'>{subscription.status}</Badge>}
        </div>
      </CardHeader>
      <CardContent className='px-5'>
        {loading ? (
          <div className='grid gap-3 sm:grid-cols-3'>
            {[1, 2, 3].map((item) => (
              <Skeleton key={item} className='h-16' />
            ))}
          </div>
        ) : !subscription ? (
          <EmptyState
            icon={<IconCalendarRepeat />}
            title={t('billing.subscription.empty')}
            description={t('billing.subscription.emptyDescription')}
            compact
          />
        ) : (
          <div className='space-y-4'>
            <div className='flex flex-col gap-2 border-b pb-4 sm:flex-row sm:items-center sm:justify-between'>
              <div>
                <div className='font-semibold'>{subscription.plan.name}</div>
                <div className='text-muted-foreground mt-1 text-xs'>
                  {t('billing.subscription.renews', { date: displayDate(subscription.currentPeriodEnd, locale) })}
                </div>
                <div className='text-muted-foreground mt-1 text-xs'>
                  {t(`billing.interval.${subscription.intervalUnit.toLowerCase()}`, { count: subscription.intervalCount })}
                </div>
              </div>
              <div className='font-mono text-lg font-semibold tabular-nums'>
                {displayAmount(creditDisplayName, subscription.remainingAllowance)}
              </div>
            </div>
            <AllowanceStrip subscription={subscription} creditDisplayName={creditDisplayName} />
            <SubscriptionBucketSection subscription={subscription} locale={locale} creditDisplayName={creditDisplayName} />
            <div className='bg-muted/15 rounded-md border p-3'>
              <div className='flex items-start gap-2'>
                <IconShieldLock size={16} className='text-muted-foreground mt-0.5 shrink-0' />
                <div className='min-w-0 space-y-1.5'>
                  <p className='text-sm font-medium'>{t('billing.subscription.authorizationGroupTitle')}</p>
                  <AccessPlanList accessPlans={subscription.grantedAccessPlans} emptyLabel={t('billing.subscription.noGrantedGroup')} />
                  <p className='text-muted-foreground text-xs text-pretty'>{t('billing.subscription.authorizationGroupHint')}</p>
                </div>
              </div>
              {subscription.projectID ? (
                <p className='text-muted-foreground mt-1 font-mono text-xs break-all'>
                  {t('billing.subscription.boundProject')}: {subscription.projectID}
                </p>
              ) : (
                <p className='text-muted-foreground mt-1 text-xs'>{t('billing.subscription.noBoundProject')}</p>
              )}
            </div>
            {canWrite && (
              <div className='flex flex-wrap items-center gap-2 border-t pt-4'>
                {(status === 'active' || status === 'paused') && (
                  <div className='mr-auto flex items-center gap-2'>
                    <Switch
                      id={`auto-renew-${subscription.id}`}
                      checked={subscription.autoRenew}
                      disabled={autoRenewMutation.isPending}
                      onCheckedChange={async (autoRenew) => {
                        try {
                          await autoRenewMutation.mutateAsync({ subscriptionID: subscription.id, autoRenew, userID });
                          toast.success(t('billing.lifecycle.autoRenewSuccess'));
                        } catch (error) {
                          toast.error(errorMessage(error, t('billing.lifecycle.error')));
                        }
                      }}
                    />
                    <Label htmlFor={`auto-renew-${subscription.id}`} className='text-xs'>
                      {t('billing.assign.autoRenew')}
                    </Label>
                  </div>
                )}
                {status === 'active' && (
                  <Button type='button' size='sm' variant='outline' disabled={lifecycle.isPending} onClick={() => runLifecycle('pause')}>
                    {t('billing.lifecycle.pause')}
                  </Button>
                )}
                {status === 'paused' && (
                  <Button type='button' size='sm' variant='outline' disabled={lifecycle.isPending} onClick={() => runLifecycle('resume')}>
                    {t('billing.lifecycle.resume')}
                  </Button>
                )}
                {status === 'expired' && (
                  <Button type='button' size='sm' disabled={lifecycle.isPending} onClick={() => runLifecycle('renew')}>
                    {t('billing.lifecycle.renew')}
                  </Button>
                )}
                {(status === 'active' || status === 'paused') && (
                  <Button
                    type='button'
                    size='sm'
                    variant='destructive'
                    disabled={lifecycle.isPending}
                    onClick={() => runLifecycle('cancel')}
                  >
                    {t('billing.lifecycle.cancel')}
                  </Button>
                )}
              </div>
            )}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function SubscriptionBucketSection({
  subscription,
  locale,
  creditDisplayName,
}: {
  subscription: UserSubscription;
  locale: string;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const buckets = activeAllowanceBuckets(subscription);
  const dedicated = buckets.filter((bucket) => bucket.quotaClass === 'DEDICATED');
  const general = buckets.filter((bucket) => bucket.quotaClass === 'GENERAL');
  const totals = bucketTotalsByClass(buckets);

  return (
    <Collapsible open={open} onOpenChange={setOpen} className='overflow-hidden rounded-md border'>
      <CollapsibleTrigger asChild>
        <Button
          type='button'
          variant='ghost'
          className='h-auto min-h-10 w-full rounded-none px-3 py-2.5 text-left active:scale-[0.99]'
          aria-label={t('billing.buckets.toggle', { count: buckets.length })}
        >
          <span className='flex min-w-0 flex-1 items-center gap-2'>
            <IconStack2 size={17} className='text-muted-foreground shrink-0' />
            <span className='min-w-0'>
              <span className='block text-sm font-medium'>{t('billing.buckets.activeTitle')}</span>
              <span className='text-muted-foreground block text-xs'>
                {t('billing.buckets.summary', {
                  count: buckets.length,
                  dedicated: displayAmount(creditDisplayName, totals.DEDICATED),
                  general: displayAmount(creditDisplayName, totals.GENERAL),
                })}
              </span>
            </span>
          </span>
          <Badge variant='secondary' className='ml-2 shrink-0 font-mono tabular-nums'>
            {buckets.length}
          </Badge>
          <IconChevronDown
            size={16}
            className={cn(
              'text-muted-foreground ml-1 shrink-0 transition-transform duration-200 motion-reduce:transition-none',
              open && 'rotate-180'
            )}
          />
        </Button>
      </CollapsibleTrigger>
      <CollapsibleContent className='border-t'>
        {!buckets.length ? (
          <div className='text-muted-foreground px-4 py-6 text-center text-sm'>{t('billing.buckets.empty')}</div>
        ) : (
          <div className='space-y-4 p-3'>
            {[
              { quotaClass: 'DEDICATED' as const, buckets: dedicated },
              { quotaClass: 'GENERAL' as const, buckets: general },
            ].map(
              (group) =>
                !!group.buckets.length && (
                  <section key={group.quotaClass} aria-labelledby={`bucket-group-${subscription.id}-${group.quotaClass}`}>
                    <div className='mb-2 flex items-center justify-between gap-3 px-1'>
                      <h4
                        id={`bucket-group-${subscription.id}-${group.quotaClass}`}
                        className='text-xs font-medium tracking-wide uppercase'
                      >
                        {t(`billing.quotaClass.${group.quotaClass.toLowerCase()}`)}
                      </h4>
                      <span className='text-muted-foreground font-mono text-xs tabular-nums'>
                        {displayAmount(creditDisplayName, totals[group.quotaClass])}
                      </span>
                    </div>
                    <div className='space-y-2'>
                      {group.buckets.map((bucket) => (
                        <AdminBucketCard key={bucket.id} bucket={bucket} locale={locale} creditDisplayName={creditDisplayName} />
                      ))}
                    </div>
                  </section>
                )
            )}
          </div>
        )}
      </CollapsibleContent>
    </Collapsible>
  );
}

function AdminBucketCard({
  bucket,
  locale,
  creditDisplayName,
}: {
  bucket: SubscriptionAllowanceBucket;
  locale: string;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const isDedicated = bucket.quotaClass === 'DEDICATED';
  const sourceLabel = t(`billing.bucket.source.${bucket.sourceType.toLowerCase()}`, { defaultValue: bucket.sourceType });
  const statusLabel = t(`billing.bucket.status.${bucket.status.toLowerCase()}`, { defaultValue: bucket.status });

  return (
    <article className='bg-card rounded-sm border px-3 py-3'>
      <div className='grid gap-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-start'>
        <div className='min-w-0'>
          <div className='flex flex-wrap items-center gap-1.5'>
            <h5 className='text-sm font-semibold text-pretty'>{bucket.name}</h5>
            <Badge variant={isDedicated ? 'secondary' : 'outline'} className='text-[10px] font-normal'>
              {t(`billing.quotaClass.${bucket.quotaClass.toLowerCase()}`)}
            </Badge>
            <Badge variant='outline' className='text-[10px] font-normal'>
              {sourceLabel}
            </Badge>
            <Badge variant='outline' className='text-[10px] font-normal'>
              {statusLabel}
            </Badge>
          </div>
          <p className='text-muted-foreground mt-1 text-xs text-pretty'>
            {isDedicated
              ? t('billing.bucket.dedicatedScope', {
                  scopes: bucket.accessPlans?.map((accessPlan) => accessPlan.name).join(', ') || t('billing.bucket.scopeSnapshot'),
                  count: bucket.modelIDs?.length || 0,
                })
              : t('billing.bucket.generalScope')}
          </p>
        </div>
        <div className='sm:text-right'>
          <p className='text-muted-foreground text-[10px] font-medium tracking-wide uppercase'>{t('billing.bucket.remaining')}</p>
          <p className='font-mono text-base font-semibold tabular-nums'>
            {displayAmount(creditDisplayName, bucket.remainingAllowance)}
            <span className='text-muted-foreground font-normal'> / {displayAmount(creditDisplayName, bucket.grantedAllowance)}</span>
          </p>
        </div>
      </div>
      <dl className='mt-3 grid gap-x-4 gap-y-2 border-t border-dashed pt-2.5 text-xs sm:grid-cols-3'>
        <div>
          <dt className='text-muted-foreground'>{t('billing.bucket.reserved')}</dt>
          <dd className='mt-0.5 font-mono tabular-nums'>{displayAmount(creditDisplayName, bucket.reservedAllowance)}</dd>
        </div>
        <div>
          <dt className='text-muted-foreground'>{t('billing.bucket.expires')}</dt>
          <dd className='mt-0.5 tabular-nums'>{displayDate(bucket.expiresAt, locale)}</dd>
        </div>
        <div>
          <dt className='text-muted-foreground'>{t('billing.bucket.period')}</dt>
          <dd className='mt-0.5 tabular-nums'>
            {t('billing.bucket.periodRange', {
              start: displayDate(bucket.periodStart, locale),
              end: displayDate(bucket.periodEnd, locale),
            })}
          </dd>
        </div>
      </dl>
    </article>
  );
}

function Ledger({
  balance,
  loading,
  locale,
  creditDisplayName,
}: {
  balance?: ProjectBalance;
  loading: boolean;
  locale: string;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  return (
    <Card className='gap-4 py-5 shadow-none'>
      <CardHeader className='px-5'>
        <CardTitle>{t('billing.ledger.title')}</CardTitle>
        <CardDescription>{t('billing.ledger.description')}</CardDescription>
      </CardHeader>
      <CardContent className='px-0'>
        {loading ? (
          <div className='space-y-2 px-5'>
            {[1, 2, 3].map((item) => (
              <Skeleton key={item} className='h-10 w-full' />
            ))}
          </div>
        ) : !balance?.ledgerEntries.length ? (
          <EmptyState
            icon={<IconReceipt2 />}
            title={t('billing.ledger.empty')}
            description={t('billing.ledger.emptyDescription')}
            compact
          />
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
                    <TableCell className='text-muted-foreground max-w-[260px] truncate'>{entry.description || '—'}</TableCell>
                    <TableCell className='text-muted-foreground text-xs'>{displayDate(entry.createdAt, locale)}</TableCell>
                    <TableCell className='pr-5 text-right font-mono font-medium tabular-nums'>
                      {displayAmount(creditDisplayName, entry.amount)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

export function ProjectWalletShadow({
  userID,
  canGrantCredit,
  creditDisplayName,
}: {
  userID: string;
  canGrantCredit: boolean;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const projectsQuery = useSubscriptionProjects(userID);
  const projects = useMemo(() => projectsQuery.data?.subscriptionProjects || [], [projectsQuery.data]);
  const [projectID, setProjectID] = useState('');
  const [grantOpen, setGrantOpen] = useState(false);
  const comparisonQuery = useProjectWalletComparison(projectID);
  const projectBalanceQuery = useProjectBalance(projectID);

  useEffect(() => {
    if (projects.length === 1) setProjectID(projects[0].id);
    else if (!projects.some((project) => project.id === projectID)) setProjectID('');
  }, [projectID, projects, userID]);

  const comparison = comparisonQuery.data?.projectWalletComparison;
  const projectBalance = projectBalanceQuery.data?.projectBalance;

  return (
    <section className='rounded-lg border border-dashed border-amber-500/45 bg-amber-500/[0.035] p-4 sm:p-5'>
      <div className='flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between'>
        <div className='min-w-0'>
          <div className='flex flex-wrap items-center gap-2'>
            <h3 className='font-semibold'>{t('billing.projectWallet.title')}</h3>
            <Badge variant='outline' className='border-amber-500/50 text-amber-800 dark:text-amber-300'>
              {t('billing.projectWallet.shadowBadge')}
            </Badge>
          </div>
          <p className='text-muted-foreground mt-1 max-w-3xl text-sm'>{t('billing.projectWallet.description')}</p>
        </div>
        {canGrantCredit && projectID && (
          <Button variant='outline' className='shrink-0 border-amber-500/40' onClick={() => setGrantOpen(true)}>
            <IconPlus /> {t('billing.projectWallet.grantAction')}
          </Button>
        )}
      </div>

      <div className='mt-4 space-y-3'>
        {projectsQuery.isLoading ? (
          <Skeleton className='h-10 w-full sm:w-80' />
        ) : projectsQuery.error ? (
          <Alert variant='destructive'>
            <IconAlertCircle />
            <AlertTitle>{t('billing.projectWallet.projectsErrorTitle')}</AlertTitle>
            <AlertDescription className='flex flex-wrap items-center justify-between gap-2'>
              <span>{t('billing.projectWallet.projectsError')}</span>
              <Button size='sm' variant='outline' onClick={() => projectsQuery.refetch()}>
                {t('billing.retry')}
              </Button>
            </AlertDescription>
          </Alert>
        ) : !projects.length ? (
          <div className='text-muted-foreground rounded-md border border-dashed px-4 py-5 text-sm'>
            {t('billing.projectWallet.noProjects')}
          </div>
        ) : (
          <div className='max-w-md space-y-2'>
            <Label>{t('billing.projectWallet.project')}</Label>
            <Select value={projectID} onValueChange={setProjectID}>
              <SelectTrigger>
                <SelectValue placeholder={t('billing.projectWallet.projectPlaceholder')} />
              </SelectTrigger>
              <SelectContent>
                {projects.map((project) => (
                  <SelectItem key={project.id} value={project.id}>
                    {project.name}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            {projects.length > 1 && !projectID && (
              <p className='text-muted-foreground text-xs'>{t('billing.projectWallet.chooseProject')}</p>
            )}
          </div>
        )}

        {projectID && (comparisonQuery.isLoading || projectBalanceQuery.isLoading) && <Skeleton className='h-28 w-full' />}
        {projectID && (comparisonQuery.error || projectBalanceQuery.error) && (
          <Alert variant='destructive'>
            <IconAlertCircle />
            <AlertTitle>{t('billing.projectWallet.comparisonErrorTitle')}</AlertTitle>
            <AlertDescription className='flex flex-wrap items-center justify-between gap-2'>
              <span>{t('billing.projectWallet.comparisonError')}</span>
              <Button
                size='sm'
                variant='outline'
                onClick={() => {
                  comparisonQuery.refetch();
                  projectBalanceQuery.refetch();
                }}
              >
                {t('billing.retry')}
              </Button>
            </AlertDescription>
          </Alert>
        )}
        {projectID &&
          !comparisonQuery.isLoading &&
          !projectBalanceQuery.isLoading &&
          !comparisonQuery.error &&
          !projectBalanceQuery.error &&
          !comparison && (
            <Alert className='border-amber-500/40 bg-amber-500/5'>
              <IconAlertCircle className='text-amber-700 dark:text-amber-400' />
              <AlertTitle>{t('billing.projectWallet.uninitializedTitle')}</AlertTitle>
              <AlertDescription>{t('billing.projectWallet.uninitialized')}</AlertDescription>
            </Alert>
          )}
        {projectID && comparison && (
          <ReconciliationStrip
            comparison={comparison}
            creditDisplayName={creditDisplayName}
            walletStatus={projectBalance?.walletStatus || t('billing.projectWallet.uninitializedStatus')}
          />
        )}
      </div>

      <ProjectCreditDialog
        open={grantOpen}
        onOpenChange={setGrantOpen}
        projectID={projectID}
        projectName={projects.find((project) => project.id === projectID)?.name}
        creditDisplayName={creditDisplayName}
      />
    </section>
  );
}

function ReconciliationStrip({
  comparison,
  creditDisplayName,
  walletStatus,
}: {
  comparison: ProjectWalletComparison;
  creditDisplayName: string;
  walletStatus: string;
}) {
  const { t } = useTranslation();
  const delta = Number(comparison.availableDelta);
  const matches = Number.isFinite(delta) && delta === 0;
  return (
    <div className='bg-background/75 overflow-hidden rounded-md border'>
      <div className='grid sm:grid-cols-[1fr_auto_1fr]'>
        <div className='border-b p-4 sm:border-r sm:border-b-0'>
          <p className='text-muted-foreground text-xs font-medium tracking-wide uppercase'>
            {t('billing.projectWallet.officialAvailable')}
          </p>
          <p className='mt-1 font-mono text-xl font-semibold text-emerald-700 tabular-nums dark:text-emerald-400'>
            {displayAmount(creditDisplayName, comparison.legacyAvailableBalance)}
          </p>
          <p className='text-muted-foreground mt-1 text-xs'>{t('billing.projectWallet.enforcing')}</p>
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
              {displayAmount(creditDisplayName, comparison.availableDelta)}
            </div>
            <p className='text-muted-foreground mt-0.5 text-[10px] font-medium tracking-wide uppercase'>
              {t('billing.projectWallet.delta')}
            </p>
          </div>
        </div>
        <div className='p-4'>
          <p className='text-muted-foreground text-xs font-medium tracking-wide uppercase'>{t('billing.projectWallet.shadowAvailable')}</p>
          <p className='mt-1 font-mono text-xl font-semibold text-amber-800 tabular-nums dark:text-amber-300'>
            {displayAmount(creditDisplayName, comparison.projectAvailableBalance)}
          </p>
          <p className='text-muted-foreground mt-1 text-xs'>{t('billing.projectWallet.notSpendable')}</p>
        </div>
      </div>
      <div className='flex flex-wrap items-center justify-between gap-2 border-t border-dashed px-4 py-2 text-xs'>
        <span className='text-muted-foreground'>{t('billing.projectWallet.statusExplanation')}</span>
        <span className='flex flex-wrap gap-1.5'>
          <Badge variant='outline'>{comparison.status}</Badge>
          <Badge variant='outline'>{walletStatus}</Badge>
          <span className='text-muted-foreground font-mono break-all'>{comparison.projectID}</span>
        </span>
      </div>
    </div>
  );
}

function ProjectCreditDialog({
  open,
  onOpenChange,
  projectID,
  projectName,
  creditDisplayName,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  projectID: string;
  projectName?: string;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const grant = useGrantProjectCredit();
  const [amount, setAmount] = useState('');
  const [description, setDescription] = useState('');
  const [idempotencyKey, setIdempotencyKey] = useState(() => nanoid());

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!projectID || !MONEY_PATTERN.test(amount) || /^0(?:\.0+)?$/.test(amount)) {
      toast.error(t('billing.projectWallet.invalidAmount'));
      return;
    }
    try {
      await grant.mutateAsync({ projectID, amount, description: description.trim() || undefined, idempotencyKey });
      toast.success(t('billing.projectWallet.grantSuccess'));
      setAmount('');
      setDescription('');
      setIdempotencyKey(nanoid());
      onOpenChange(false);
    } catch (error) {
      toast.error(errorMessage(error, t('billing.projectWallet.grantError')));
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='sm:max-w-md'>
        <form onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t('billing.projectWallet.grantTitle')}</DialogTitle>
            <DialogDescription>{t('billing.projectWallet.grantDescription')}</DialogDescription>
          </DialogHeader>
          <Alert className='mt-4 border-amber-500/40 bg-amber-500/5'>
            <IconAlertCircle className='text-amber-700 dark:text-amber-400' />
            <AlertTitle>{t('billing.projectWallet.noCurrentFundsTitle')}</AlertTitle>
            <AlertDescription>{t('billing.projectWallet.noCurrentFunds')}</AlertDescription>
          </Alert>
          <div className='space-y-4 py-5'>
            <div className='space-y-1 rounded-md border border-dashed p-3'>
              <p className='text-sm font-medium'>{projectName || projectID}</p>
              <p className='text-muted-foreground font-mono text-xs break-all'>{projectID}</p>
            </div>
            <div className='space-y-2'>
              <Label htmlFor='project-grant-amount'>{t('billing.projectWallet.amount')}</Label>
              <div className='flex'>
                <span className='bg-muted text-muted-foreground flex items-center rounded-l-md border border-r-0 px-3 font-mono text-xs'>
                  {creditDisplayName}
                </span>
                <Input
                  id='project-grant-amount'
                  inputMode='decimal'
                  autoComplete='off'
                  className='rounded-l-none font-mono tabular-nums'
                  placeholder='100.00'
                  value={amount}
                  onChange={(event) => setAmount(event.target.value.trim())}
                />
              </div>
              <p className='text-muted-foreground text-xs'>{t('billing.projectWallet.idempotencyHint')}</p>
            </div>
            <div className='space-y-2'>
              <Label htmlFor='project-grant-description'>{t('billing.projectWallet.note')}</Label>
              <Input
                id='project-grant-description'
                value={description}
                onChange={(event) => setDescription(event.target.value)}
                placeholder={t('billing.projectWallet.notePlaceholder')}
              />
            </div>
          </div>
          <DialogFooter>
            <Button type='button' variant='outline' onClick={() => onOpenChange(false)}>
              {t('billing.cancel')}
            </Button>
            <Button type='submit' disabled={grant.isPending || !projectID}>
              {grant.isPending ? <IconRefresh className='animate-spin' /> : <IconPlus />}
              {t('billing.projectWallet.grantSubmit')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function OperatorActions({
  userID,
  projectID,
  creditDisplayName,
  plans,
  currentSubscription,
  canGrantCredit,
  canWriteSubscriptions,
}: {
  userID: string;
  projectID: string;
  creditDisplayName: string;
  plans: SubscriptionPlan[];
  currentSubscription?: UserSubscription;
  canGrantCredit: boolean;
  canWriteSubscriptions: boolean;
}) {
  const { t } = useTranslation();
  const grant = useGrantProjectCredit();
  const assign = useAssignUserSubscription();
  const refresh = useRefreshSubscriptionAllowance();
  const [amount, setAmount] = useState('');
  const [description, setDescription] = useState('');
  const [idempotencyKey, setIdempotencyKey] = useState(() => nanoid());
  const [assignmentIdempotencyKey, setAssignmentIdempotencyKey] = useState(() => nanoid());
  const [planID, setPlanID] = useState('');
  const [autoRenew, setAutoRenew] = useState(true);
  const [intervalUnit, setIntervalUnit] = useState<SubscriptionPlan['intervalUnit']>('MONTH');
  const [intervalCount, setIntervalCount] = useState(1);
  const selectedPlan = plans.find((plan) => plan.id === planID);

  if (!canGrantCredit && !canWriteSubscriptions) {
    return (
      <Alert className='border-amber-600/25 bg-amber-500/5'>
        <IconShieldLock className='text-amber-700 dark:text-amber-400' />
        <AlertTitle>{t('billing.permissions.actionsTitle')}</AlertTitle>
        <AlertDescription>{t('billing.permissions.actionsDescription')}</AlertDescription>
      </Alert>
    );
  }

  const submitGrant = async (event: FormEvent) => {
    event.preventDefault();
    if (!projectID) {
      toast.error(t('billing.projectWallet.chooseProject'));
      return;
    }
    if (!MONEY_PATTERN.test(amount) || /^0(?:\.0+)?$/.test(amount)) {
      toast.error(t('billing.grant.invalidAmount'));
      return;
    }
    try {
      await grant.mutateAsync({
        projectID,
        amount,
        description: description.trim() || undefined,
        idempotencyKey,
      });
      toast.success(t('billing.grant.success'));
      setAmount('');
      setDescription('');
      setIdempotencyKey(nanoid());
    } catch (error) {
      toast.error(errorMessage(error, t('billing.grant.error')));
    }
  };

  const submitAssignment = async (event: FormEvent) => {
    event.preventDefault();
    if (!planID || !projectID) return;
    try {
      await assign.mutateAsync({
        userID,
        planID,
        projectID,
        idempotencyKey: assignmentIdempotencyKey,
        autoRenew,
        intervalUnit,
        intervalCount,
      });
      toast.success(t('billing.assign.success'));
      setAssignmentIdempotencyKey(nanoid());
    } catch (error) {
      toast.error(errorMessage(error, t('billing.assign.error')));
    }
  };

  const submitRefresh = async () => {
    if (!currentSubscription) return;
    try {
      await refresh.mutateAsync({ subscriptionID: currentSubscription.id, userID });
      toast.success(t('billing.refresh.success'));
    } catch (error) {
      toast.error(errorMessage(error, t('billing.refresh.error')));
    }
  };

  return (
    <Card className='gap-4 py-5 shadow-none xl:sticky xl:top-20'>
      <CardHeader className='px-5'>
        <CardTitle>{t('billing.actions.title')}</CardTitle>
        <CardDescription>{t('billing.actions.description')}</CardDescription>
      </CardHeader>
      <CardContent className='px-5'>
        <Tabs defaultValue={canGrantCredit ? 'credit' : 'subscription'}>
          <TabsList className={cn('grid w-full', canGrantCredit && canWriteSubscriptions ? 'grid-cols-2' : 'grid-cols-1')}>
            {canGrantCredit && (
              <TabsTrigger value='credit'>
                <IconCashBanknote />
                {t('billing.grant.tab')}
              </TabsTrigger>
            )}
            {canWriteSubscriptions && (
              <TabsTrigger value='subscription'>
                <IconCalendarRepeat />
                {t('billing.assign.tab')}
              </TabsTrigger>
            )}
          </TabsList>
          {canGrantCredit && (
            <TabsContent value='credit' className='pt-3'>
              <form className='space-y-4' onSubmit={submitGrant}>
                <div className='space-y-2'>
                  <Label htmlFor='grant-amount'>{t('billing.grant.amount')}</Label>
                  <div className='flex'>
                    <span className='bg-muted text-muted-foreground flex items-center rounded-l-md border border-r-0 px-3 font-mono text-xs'>
                      {creditDisplayName}
                    </span>
                    <Input
                      id='grant-amount'
                      inputMode='decimal'
                      autoComplete='off'
                      className='rounded-l-none font-mono tabular-nums'
                      placeholder='100.00'
                      value={amount}
                      onChange={(event) => setAmount(event.target.value.trim())}
                    />
                  </div>
                  <p className='text-muted-foreground text-xs'>{t('billing.grant.precision')}</p>
                </div>
                <div className='space-y-2'>
                  <Label htmlFor='grant-description'>{t('billing.grant.description')}</Label>
                  <Input
                    id='grant-description'
                    value={description}
                    onChange={(event) => setDescription(event.target.value)}
                    placeholder={t('billing.grant.descriptionPlaceholder')}
                  />
                </div>
                <Button type='submit' className='w-full' disabled={grant.isPending || !userID}>
                  {grant.isPending ? <IconRefresh className='animate-spin' /> : <IconArrowDownRight />}
                  {t('billing.grant.submit')}
                </Button>
              </form>
            </TabsContent>
          )}
          {canWriteSubscriptions && (
            <TabsContent value='subscription' className='pt-3'>
              <form className='space-y-4' onSubmit={submitAssignment}>
                <div className='space-y-2'>
                  <Label>{t('billing.assign.plan')}</Label>
                  <Select
                    value={planID}
                    onValueChange={(value) => {
                      setPlanID(value);
                      const plan = plans.find((item) => item.id === value);
                      if (plan) {
                        setIntervalUnit(plan.intervalUnit);
                        setIntervalCount(plan.intervalCount);
                      }
                    }}
                  >
                    <SelectTrigger>
                      <SelectValue placeholder={t('billing.assign.planPlaceholder')} />
                    </SelectTrigger>
                    <SelectContent>
                      {plans
                        .filter((plan) => plan.status === 'ENABLED')
                        .map((plan) => (
                          <SelectItem key={plan.id} value={plan.id}>
                            {plan.name} · {displayAmount(creditDisplayName, planAllowance(plan))}
                          </SelectItem>
                        ))}
                    </SelectContent>
                  </Select>
                </div>
                {selectedPlan && (
                  <div className='space-y-3 rounded-md border p-3'>
                    <div className='grid gap-2'>
                      <div className='rounded-md bg-emerald-500/8 px-3 py-2'>
                        <div className='text-xs font-semibold text-emerald-800 dark:text-emerald-300'>
                          {t('billing.assign.allowancePromise')}
                        </div>
                        <div className='mt-0.5 text-sm'>
                          {displayAmount(creditDisplayName, planAllowance(selectedPlan))} ·{' '}
                          {t(`billing.interval.${intervalUnit.toLowerCase()}`, { count: intervalCount })}
                        </div>
                      </div>
                      <div className='bg-muted/40 rounded-md px-3 py-2'>
                        <div className='text-muted-foreground text-xs font-semibold'>{t('billing.assign.entitlementPromise')}</div>
                        <div className='mt-1 text-sm'>
                          <AccessPlanList accessPlans={selectedPlan.accessPlans} emptyLabel={t('billing.subscription.allowanceOnly')} />
                        </div>
                      </div>
                    </div>
                    <div className='grid grid-cols-[1fr_90px] gap-3'>
                      <div className='space-y-2'>
                        <Label>{t('billing.assign.cadenceUnit')}</Label>
                        <Select value={intervalUnit} onValueChange={(value: SubscriptionPlan['intervalUnit']) => setIntervalUnit(value)}>
                          <SelectTrigger>
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            <SelectItem value='DAY'>{t('billing.units.day')}</SelectItem>
                            <SelectItem value='MONTH'>{t('billing.units.month')}</SelectItem>
                            <SelectItem value='YEAR'>{t('billing.units.year')}</SelectItem>
                          </SelectContent>
                        </Select>
                      </div>
                      <div className='space-y-2'>
                        <Label htmlFor='assign-count'>{t('billing.assign.cadenceCount')}</Label>
                        <Input
                          id='assign-count'
                          type='number'
                          min={1}
                          max={120}
                          value={intervalCount}
                          onChange={(event) => setIntervalCount(Math.max(1, Number(event.target.value) || 1))}
                        />
                      </div>
                    </div>
                  </div>
                )}
                <div className='flex items-center justify-between rounded-md border p-3'>
                  <div>
                    <Label htmlFor='assign-renew'>{t('billing.assign.autoRenew')}</Label>
                    <p className='text-muted-foreground mt-0.5 text-xs'>{t('billing.assign.autoRenewDescription')}</p>
                  </div>
                  <Switch id='assign-renew' checked={autoRenew} onCheckedChange={setAutoRenew} />
                </div>
                <Button type='submit' className='w-full' disabled={assign.isPending || !planID || !userID || !projectID}>
                  {assign.isPending ? <IconRefresh className='animate-spin' /> : <IconPlus />}
                  {t('billing.assign.submit')}
                </Button>
                <div className='border-t pt-4'>
                  <Button
                    type='button'
                    variant='outline'
                    className='w-full'
                    disabled={!currentSubscription || refresh.isPending}
                    onClick={submitRefresh}
                  >
                    <IconRefresh className={refresh.isPending ? 'animate-spin' : ''} />
                    {t('billing.refresh.submit')}
                  </Button>
                  <p className='text-muted-foreground mt-2 text-center text-xs'>{t('billing.refresh.description')}</p>
                </div>
              </form>
            </TabsContent>
          )}
        </Tabs>
      </CardContent>
    </Card>
  );
}

function PlansSection({ plans, canWrite, creditDisplayName }: { plans: SubscriptionPlan[]; canWrite: boolean; creditDisplayName: string }) {
  const { t } = useTranslation();
  const [dialogOpen, setDialogOpen] = useState(false);
  const [editingPlan, setEditingPlan] = useState<SubscriptionPlan>();

  const openCreate = () => {
    setEditingPlan(undefined);
    setDialogOpen(true);
  };

  const openEdit = (plan: SubscriptionPlan) => {
    setEditingPlan(plan);
    setDialogOpen(true);
  };
  return (
    <section className='space-y-3 border-t pt-5'>
      <div className='flex items-center justify-between gap-3'>
        <div>
          <h3 className='font-semibold'>{t('billing.plans.title')}</h3>
          <p className='text-muted-foreground text-sm'>{t('billing.plans.description')}</p>
        </div>
        {canWrite && (
          <Button variant='outline' onClick={openCreate}>
            <IconPlus />
            {t('billing.plans.create')}
          </Button>
        )}
      </div>
      {plans.length === 0 ? (
        <EmptyState icon={<IconCalendarRepeat />} title={t('billing.plans.empty')} description={t('billing.plans.emptyDescription')} />
      ) : (
        <div className='overflow-x-auto rounded-lg border'>
          <Table>
            <TableHeader>
              <TableRow className='bg-muted/40'>
                <TableHead className='pl-4'>{t('billing.plans.name')}</TableHead>
                <TableHead>{t('billing.plans.quotaRules')}</TableHead>
                <TableHead>{t('billing.plans.interval')}</TableHead>
                <TableHead>{t('billing.plans.rollover')}</TableHead>
                <TableHead>{t('billing.plans.modelGrants')}</TableHead>
                <TableHead>{t('billing.plans.status')}</TableHead>
                <TableHead className='w-14 pr-4 text-right'>{t('billing.plans.actions')}</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {plans.map((plan) => (
                <PlanRow
                  key={plan.id}
                  plan={plan}
                  canWrite={canWrite}
                  creditDisplayName={creditDisplayName}
                  onEdit={() => openEdit(plan)}
                />
              ))}
            </TableBody>
          </Table>
        </div>
      )}
      <PlanDialog open={dialogOpen} onOpenChange={setDialogOpen} plan={editingPlan} creditDisplayName={creditDisplayName} />
    </section>
  );
}

function PlanRow({
  plan,
  canWrite,
  creditDisplayName,
  onEdit,
}: {
  plan: SubscriptionPlan;
  canWrite: boolean;
  creditDisplayName: string;
  onEdit: () => void;
}) {
  const { t } = useTranslation();
  const rules = quotaRulesForPlan(plan);
  const totals = planTotalsByClass(plan);
  const rolloverRuleCount = rules.filter((rule) => rule.rolloverMode === 'CAPPED').length;
  return (
    <TableRow>
      <TableCell className='pl-4 font-medium'>{plan.name}</TableCell>
      <TableCell>
        <div className='min-w-48 space-y-1.5'>
          <div className='font-mono font-medium tabular-nums'>{displayAmount(creditDisplayName, planAllowance(plan))}</div>
          <div className='flex flex-wrap gap-1'>
            {!!Number(totals.DEDICATED) && (
              <Badge variant='secondary' className='font-normal'>
                {t('billing.quotaClass.dedicated')} · {displayAmount(creditDisplayName, totals.DEDICATED)}
              </Badge>
            )}
            {!!Number(totals.GENERAL) && (
              <Badge variant='outline' className='font-normal'>
                {t('billing.quotaClass.general')} · {displayAmount(creditDisplayName, totals.GENERAL)}
              </Badge>
            )}
          </div>
          <p className='text-muted-foreground text-xs'>{t('billing.plans.ruleCount', { count: rules.length })}</p>
        </div>
      </TableCell>
      <TableCell>{t(`billing.interval.${plan.intervalUnit.toLowerCase()}`, { count: plan.intervalCount })}</TableCell>
      <TableCell>
        {rolloverRuleCount ? t('billing.rollover.ruleCount', { count: rolloverRuleCount }) : t('billing.rollover.none')}
      </TableCell>
      <TableCell>
        <div className='flex min-w-32 flex-col items-start gap-1'>
          <AccessPlanList accessPlans={plan.accessPlans} emptyLabel={t('billing.plans.noAccessPermissions')} />
        </div>
      </TableCell>
      <TableCell>
        <Badge variant={plan.status === 'ENABLED' ? 'secondary' : 'outline'}>{plan.status}</Badge>
      </TableCell>
      <TableCell className='pr-4 text-right'>
        {canWrite && (
          <Button type='button' size='icon' variant='ghost' aria-label={t('billing.plans.edit')} onClick={onEdit}>
            <IconPencil />
          </Button>
        )}
      </TableCell>
    </TableRow>
  );
}

type QuotaRuleForm = {
  clientID: string;
  id?: string;
  name: string;
  quotaClass: QuotaClass;
  allowance: string;
  rolloverMode: 'NONE' | 'CAPPED';
  rolloverCap: string;
  carryoverDays: string;
  accessPlanIDs: string[];
};

type SubscriptionPlanForm = {
  name: string;
  intervalUnit: SubscriptionPlan['intervalUnit'];
  intervalCount: number;
  accessPlanIDs: string[];
  quotaRules: QuotaRuleForm[];
  status: SubscriptionPlan['status'];
};

const EMPTY_PLAN_FORM: SubscriptionPlanForm = {
  name: '',
  intervalUnit: 'MONTH',
  intervalCount: 1,
  accessPlanIDs: [],
  quotaRules: [],
  status: 'ENABLED',
};

function emptyQuotaRule(name = ''): QuotaRuleForm {
  return {
    clientID: nanoid(),
    name,
    quotaClass: 'GENERAL',
    allowance: '',
    rolloverMode: 'NONE',
    rolloverCap: '',
    carryoverDays: '30',
    accessPlanIDs: [],
  };
}

function PlanDialog({
  open,
  onOpenChange,
  plan,
  creditDisplayName,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  plan?: SubscriptionPlan;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const { hasSystemScope } = usePermissions();
  const canReadGroups = hasSystemScope('read_groups');
  const createPlan = useCreateSubscriptionPlan();
  const updatePlan = useUpdateSubscriptionPlan();
  const bundlesQuery = useBillingAccessBundles(open && canReadGroups);
  const [form, setForm] = useState(EMPTY_PLAN_FORM);
  const [accessPlanPickerOpen, setAccessPlanPickerOpen] = useState(false);
  const [scopePickerOpen, setScopePickerOpen] = useState<string | null>(null);
  const [validationError, setValidationError] = useState<string>();
  const isEditing = !!plan;
  const pending = createPlan.isPending || updatePlan.isPending;
  const selectedAccessPlanIDs = new Set([...form.accessPlanIDs, ...form.quotaRules.flatMap((rule) => rule.accessPlanIDs)]);
  const bundles = (bundlesQuery.data?.simpleGroups || []).filter(
    (bundle) => bundle.status === 'ENABLED' || selectedAccessPlanIDs.has(bundle.accessPlanID)
  );
  const accessPlanOptions = mergeAccessPlanOptions(
    plan?.accessPlans || [],
    ...(plan ? quotaRulesForPlan(plan).map((rule) => rule.accessPlans) : []),
    bundles.map((bundle) => ({ id: bundle.accessPlanID, name: bundle.name }))
  );
  const selectedAccessPlans = form.accessPlanIDs.map(
    (id) => accessPlanOptions.find((accessPlan) => accessPlan.id === id) || { id, name: id }
  );

  useEffect(() => {
    if (!open) return;
    setAccessPlanPickerOpen(false);
    setScopePickerOpen(null);
    setValidationError(undefined);
    if (plan) {
      const rules = quotaRulesForPlan(plan);
      setForm({
        name: plan.name,
        intervalUnit: plan.intervalUnit,
        intervalCount: plan.intervalCount,
        accessPlanIDs: accessPlanIDsForEdit(plan.accessPlans),
        quotaRules: rules.map((rule) => ({
          clientID: rule.id || nanoid(),
          id: rule.id.startsWith('legacy-') ? undefined : rule.id,
          name: rule.name,
          quotaClass: rule.quotaClass,
          allowance: rule.allowance,
          rolloverMode: rule.rolloverMode,
          rolloverCap: rule.rolloverCap || '',
          carryoverDays: rule.carryoverDays ? String(rule.carryoverDays) : '30',
          accessPlanIDs: accessPlanIDsForEdit(rule.accessPlans),
        })),
        status: plan.status,
      });
    } else {
      setForm({
        ...EMPTY_PLAN_FORM,
        accessPlanIDs: [],
        quotaRules: [emptyQuotaRule(t('billing.quotaRule.defaultGeneralName'))],
      });
    }
  }, [open, plan, t]);

  const updateRule = (clientID: string, update: (rule: QuotaRuleForm) => QuotaRuleForm) => {
    setForm((current) => ({
      ...current,
      quotaRules: current.quotaRules.map((rule) => (rule.clientID === clientID ? update(rule) : rule)),
    }));
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setValidationError(undefined);
    if (!form.name.trim() || !form.quotaRules.length) {
      const message = t('billing.createPlan.invalid');
      setValidationError(message);
      toast.error(message);
      return;
    }
    const invalidRule = form.quotaRules.find(
      (rule) => !rule.name.trim() || !MONEY_PATTERN.test(rule.allowance) || /^0(?:\.0+)?$/.test(rule.allowance)
    );
    if (invalidRule) {
      const message = t('billing.quotaRule.invalid', { name: invalidRule.name || t('billing.quotaRule.unnamed') });
      setValidationError(message);
      toast.error(message);
      return;
    }
    const invalidScope = form.quotaRules.find((rule) => rule.quotaClass === 'DEDICATED' && !rule.accessPlanIDs.length);
    if (invalidScope) {
      const message = t('billing.quotaRule.scopeRequired', { name: invalidScope.name });
      setValidationError(message);
      toast.error(message);
      return;
    }
    const invalidRollover = form.quotaRules.find(
      (rule) =>
        rule.rolloverMode === 'CAPPED' &&
        (!MONEY_PATTERN.test(rule.rolloverCap) ||
          /^0(?:\.0+)?$/.test(rule.rolloverCap) ||
          !/^\d+$/.test(rule.carryoverDays) ||
          Number(rule.carryoverDays) < 1 ||
          Number(rule.carryoverDays) > 3650)
    );
    if (invalidRollover) {
      const message = t('billing.quotaRule.invalidRollover', { name: invalidRollover.name });
      setValidationError(message);
      toast.error(message);
      return;
    }
    try {
      const quotaRules: SubscriptionQuotaRuleInput[] = form.quotaRules.map((rule) => ({
        ...(rule.id ? { id: rule.id } : {}),
        name: rule.name.trim(),
        quotaClass: rule.quotaClass,
        allowance: rule.allowance,
        rolloverMode: rule.rolloverMode,
        ...(rule.rolloverMode === 'CAPPED' ? { rolloverCap: rule.rolloverCap, carryoverDays: Number(rule.carryoverDays) } : {}),
        accessPlanIDs: rule.quotaClass === 'DEDICATED' ? normalizeAccessPlanIDs(rule.accessPlanIDs) : [],
      }));
      const normalized = {
        name: form.name.trim(),
        intervalUnit: form.intervalUnit,
        intervalCount: form.intervalCount,
        accessPlanIDs: normalizeAccessPlanIDs(form.accessPlanIDs),
        quotaRules,
      };
      if (plan) {
        await updatePlan.mutateAsync({
          ...normalized,
          id: plan.id,
          status: form.status,
        });
        toast.success(t('billing.editPlan.success'));
      } else {
        await createPlan.mutateAsync(normalized);
        toast.success(t('billing.createPlan.success'));
      }
      onOpenChange(false);
    } catch (error) {
      toast.error(errorMessage(error, t(isEditing ? 'billing.editPlan.error' : 'billing.createPlan.error')));
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-3xl'>
        <form onSubmit={submit}>
          <DialogHeader>
            <DialogTitle>{t(isEditing ? 'billing.editPlan.title' : 'billing.createPlan.title')}</DialogTitle>
            <DialogDescription>{t(isEditing ? 'billing.editPlan.description' : 'billing.createPlan.description')}</DialogDescription>
          </DialogHeader>
          {isEditing && (
            <Alert className='mt-4 border-emerald-500/30 bg-emerald-500/5'>
              <IconCalendarRepeat />
              <AlertTitle>{t('billing.editPlan.futureOnlyTitle')}</AlertTitle>
              <AlertDescription>{t('billing.editPlan.futureOnlyDescription')}</AlertDescription>
            </Alert>
          )}
          {validationError && (
            <Alert variant='destructive' className='mt-4' aria-live='polite'>
              <IconAlertCircle />
              <AlertTitle>{t('billing.quotaRule.validationTitle')}</AlertTitle>
              <AlertDescription>{validationError}</AlertDescription>
            </Alert>
          )}
          <div className='grid gap-4 py-5'>
            <div className='space-y-2'>
              <Label htmlFor='plan-name'>{t('billing.createPlan.name')}</Label>
              <Input id='plan-name' value={form.name} onChange={(event) => setForm({ ...form, name: event.target.value })} />
            </div>
            <div className='grid gap-4 sm:grid-cols-2'>
              <div className='space-y-2'>
                <Label htmlFor='plan-interval'>{t('billing.createPlan.interval')}</Label>
                <Select
                  value={form.intervalUnit}
                  onValueChange={(value: SubscriptionPlan['intervalUnit']) => setForm({ ...form, intervalUnit: value })}
                >
                  <SelectTrigger id='plan-interval'>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value='DAY'>{t('billing.units.day')}</SelectItem>
                    <SelectItem value='MONTH'>{t('billing.units.month')}</SelectItem>
                    <SelectItem value='YEAR'>{t('billing.units.year')}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
              <div className='space-y-2'>
                <Label htmlFor='plan-count'>{t('billing.createPlan.intervalCount')}</Label>
                <Input
                  id='plan-count'
                  type='number'
                  min={1}
                  max={120}
                  value={form.intervalCount}
                  onChange={(event) => setForm({ ...form, intervalCount: Math.max(1, Number(event.target.value) || 1) })}
                />
              </div>
            </div>
            <section className='space-y-3 border-t pt-5' aria-labelledby='quota-rules-heading'>
              <div className='flex flex-wrap items-start justify-between gap-3'>
                <div>
                  <h3 id='quota-rules-heading' className='text-sm font-semibold'>
                    {t('billing.quotaRule.title')}
                  </h3>
                  <p className='text-muted-foreground mt-0.5 max-w-2xl text-xs text-pretty'>{t('billing.quotaRule.description')}</p>
                </div>
                <Button
                  type='button'
                  variant='outline'
                  className='min-h-10 active:scale-[0.96]'
                  onClick={() =>
                    setForm((current) => ({
                      ...current,
                      quotaRules: [
                        ...current.quotaRules,
                        emptyQuotaRule(t('billing.quotaRule.defaultName', { count: current.quotaRules.length + 1 })),
                      ],
                    }))
                  }
                >
                  <IconPlus />
                  {t('billing.quotaRule.add')}
                </Button>
              </div>
              <FundingOrderStrip creditDisplayName={creditDisplayName} />
              <div className='space-y-3'>
                {form.quotaRules.map((rule, index) => {
                  const selectedScopes = rule.accessPlanIDs.map(
                    (id) => accessPlanOptions.find((accessPlan) => accessPlan.id === id) || { id, name: id }
                  );
                  const rulePickerOpen = scopePickerOpen === rule.clientID;
                  return (
                    <fieldset key={rule.clientID} className='bg-muted/10 rounded-md border p-4'>
                      <legend className='sr-only'>{t('billing.quotaRule.legend', { count: index + 1 })}</legend>
                      <div className='mb-4 flex items-start justify-between gap-3'>
                        <div className='flex min-w-0 items-center gap-2'>
                          <span className='bg-muted flex size-7 shrink-0 items-center justify-center rounded-sm font-mono text-xs font-semibold tabular-nums'>
                            {index + 1}
                          </span>
                          <div className='min-w-0'>
                            <p className='truncate text-sm font-medium'>{rule.name || t('billing.quotaRule.unnamed')}</p>
                            <p className='text-muted-foreground text-xs'>{t(`billing.quotaClass.${rule.quotaClass.toLowerCase()}`)}</p>
                          </div>
                        </div>
                        <Button
                          type='button'
                          size='icon'
                          variant='ghost'
                          className='size-10 shrink-0 active:scale-[0.96]'
                          disabled={form.quotaRules.length === 1}
                          aria-label={t('billing.quotaRule.remove', { name: rule.name || index + 1 })}
                          title={
                            form.quotaRules.length === 1
                              ? t('billing.quotaRule.keepOne')
                              : t('billing.quotaRule.remove', { name: rule.name })
                          }
                          onClick={() =>
                            setForm((current) => ({
                              ...current,
                              quotaRules: current.quotaRules.filter((candidate) => candidate.clientID !== rule.clientID),
                            }))
                          }
                        >
                          <IconTrash />
                        </Button>
                      </div>

                      <div className='grid gap-4 md:grid-cols-[minmax(0,1.4fr)_minmax(10rem,.8fr)_minmax(11rem,.8fr)]'>
                        <div className='space-y-2'>
                          <Label htmlFor={`quota-rule-name-${rule.clientID}`}>{t('billing.quotaRule.name')}</Label>
                          <Input
                            id={`quota-rule-name-${rule.clientID}`}
                            value={rule.name}
                            onChange={(event) => updateRule(rule.clientID, (current) => ({ ...current, name: event.target.value }))}
                          />
                        </div>
                        <div className='space-y-2'>
                          <Label htmlFor={`quota-rule-class-${rule.clientID}`}>{t('billing.quotaRule.class')}</Label>
                          <Select
                            value={rule.quotaClass}
                            onValueChange={(quotaClass: QuotaClass) =>
                              updateRule(rule.clientID, (current) => ({
                                ...current,
                                quotaClass,
                                accessPlanIDs: quotaClass === 'GENERAL' ? [] : current.accessPlanIDs,
                              }))
                            }
                          >
                            <SelectTrigger id={`quota-rule-class-${rule.clientID}`}>
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              <SelectItem value='GENERAL'>{t('billing.quotaClass.general')}</SelectItem>
                              <SelectItem value='DEDICATED'>{t('billing.quotaClass.dedicated')}</SelectItem>
                            </SelectContent>
                          </Select>
                        </div>
                        <div className='space-y-2'>
                          <Label htmlFor={`quota-rule-amount-${rule.clientID}`}>{t('billing.quotaRule.amount')}</Label>
                          <div className='flex'>
                            <span
                              className='bg-muted text-muted-foreground flex max-w-28 items-center truncate rounded-l-md border border-r-0 px-2.5 font-mono text-xs'
                              title={creditDisplayName}
                            >
                              {creditDisplayName}
                            </span>
                            <Input
                              id={`quota-rule-amount-${rule.clientID}`}
                              inputMode='decimal'
                              className='rounded-l-none font-mono'
                              placeholder='100.00'
                              value={rule.allowance}
                              onChange={(event) =>
                                updateRule(rule.clientID, (current) => ({ ...current, allowance: event.target.value.trim() }))
                              }
                            />
                          </div>
                        </div>
                      </div>

                      {rule.quotaClass === 'DEDICATED' && (
                        <div className='mt-4 space-y-2'>
                          <Label htmlFor={`quota-rule-scope-${rule.clientID}`}>{t('billing.quotaRule.scope')}</Label>
                          <Popover open={rulePickerOpen} onOpenChange={(nextOpen) => setScopePickerOpen(nextOpen ? rule.clientID : null)}>
                            <PopoverTrigger asChild>
                              <Button
                                id={`quota-rule-scope-${rule.clientID}`}
                                type='button'
                                variant='outline'
                                role='combobox'
                                aria-expanded={rulePickerOpen}
                                className='min-h-10 w-full justify-between font-normal'
                              >
                                <span className='truncate'>
                                  {selectedScopes.length
                                    ? selectedScopes.map((accessPlan) => accessPlan.name).join(', ')
                                    : t('billing.quotaRule.scopePlaceholder')}
                                </span>
                                <ChevronsUpDown className='ml-2 size-4 shrink-0 opacity-50' />
                              </Button>
                            </PopoverTrigger>
                            <PopoverContent className='w-[var(--radix-popover-trigger-width)] p-0' align='start'>
                              <Command>
                                <CommandInput placeholder={t('billing.createPlan.groupsSearch')} />
                                <CommandList>
                                  <CommandEmpty>{t('billing.createPlan.noGrantedGroup')}</CommandEmpty>
                                  <CommandGroup>
                                    {accessPlanOptions.map((accessPlan) => {
                                      const selected = rule.accessPlanIDs.includes(accessPlan.id);
                                      return (
                                        <CommandItem
                                          key={accessPlan.id}
                                          value={`${accessPlan.name} ${accessPlan.id}`}
                                          onSelect={() =>
                                            updateRule(rule.clientID, (current) => ({
                                              ...current,
                                              accessPlanIDs: toggleAccessPlanID(current.accessPlanIDs, accessPlan.id),
                                            }))
                                          }
                                        >
                                          <Check className={cn('size-4', selected ? 'opacity-100' : 'opacity-0')} />
                                          <span className='min-w-0 flex-1 truncate'>{accessPlan.name}</span>
                                        </CommandItem>
                                      );
                                    })}
                                  </CommandGroup>
                                </CommandList>
                              </Command>
                            </PopoverContent>
                          </Popover>
                          <p className='text-muted-foreground text-xs text-pretty'>{t('billing.quotaRule.scopeHint')}</p>
                        </div>
                      )}

                      <div className='mt-4 grid gap-4 border-t border-dashed pt-4 sm:grid-cols-3'>
                        <div className='space-y-2'>
                          <Label htmlFor={`quota-rule-rollover-${rule.clientID}`}>{t('billing.quotaRule.rollover')}</Label>
                          <Select
                            value={rule.rolloverMode}
                            onValueChange={(rolloverMode: 'NONE' | 'CAPPED') =>
                              updateRule(rule.clientID, (current) => ({ ...current, rolloverMode }))
                            }
                          >
                            <SelectTrigger id={`quota-rule-rollover-${rule.clientID}`}>
                              <SelectValue />
                            </SelectTrigger>
                            <SelectContent>
                              <SelectItem value='NONE'>{t('billing.rollover.none')}</SelectItem>
                              <SelectItem value='CAPPED'>{t('billing.rollover.cappedShort')}</SelectItem>
                            </SelectContent>
                          </Select>
                        </div>
                        {rule.rolloverMode === 'CAPPED' && (
                          <>
                            <div className='space-y-2'>
                              <Label htmlFor={`quota-rule-cap-${rule.clientID}`}>{t('billing.quotaRule.rolloverCap')}</Label>
                              <div className='flex'>
                                <span
                                  className='bg-muted text-muted-foreground flex max-w-24 items-center truncate rounded-l-md border border-r-0 px-2.5 font-mono text-xs'
                                  title={creditDisplayName}
                                >
                                  {creditDisplayName}
                                </span>
                                <Input
                                  id={`quota-rule-cap-${rule.clientID}`}
                                  inputMode='decimal'
                                  className='rounded-l-none font-mono'
                                  value={rule.rolloverCap}
                                  onChange={(event) =>
                                    updateRule(rule.clientID, (current) => ({ ...current, rolloverCap: event.target.value.trim() }))
                                  }
                                />
                              </div>
                            </div>
                            <div className='space-y-2'>
                              <Label htmlFor={`quota-rule-days-${rule.clientID}`}>{t('billing.quotaRule.carryoverDays')}</Label>
                              <Input
                                id={`quota-rule-days-${rule.clientID}`}
                                type='number'
                                min={1}
                                max={3650}
                                className='font-mono tabular-nums'
                                value={rule.carryoverDays}
                                onChange={(event) =>
                                  updateRule(rule.clientID, (current) => ({ ...current, carryoverDays: event.target.value }))
                                }
                              />
                            </div>
                          </>
                        )}
                      </div>
                    </fieldset>
                  );
                })}
              </div>
            </section>
            <section className='space-y-3 border-t pt-5' aria-labelledby='access-permissions-heading'>
              <div>
                <h3 id='access-permissions-heading' className='flex items-center gap-2 text-sm font-semibold'>
                  <IconShieldLock size={16} className='text-muted-foreground' />
                  {t('billing.accessPermissions.title')}
                </h3>
                <p className='text-muted-foreground mt-0.5 text-xs text-pretty'>{t('billing.accessPermissions.description')}</p>
              </div>
              <div className='space-y-2'>
                <Label htmlFor='plan-granted-group'>{t('billing.accessPermissions.label')}</Label>
                <Popover open={accessPlanPickerOpen} onOpenChange={setAccessPlanPickerOpen}>
                  <PopoverTrigger asChild>
                    <Button
                      id='plan-granted-group'
                      type='button'
                      variant='outline'
                      role='combobox'
                      aria-expanded={accessPlanPickerOpen}
                      className='w-full justify-between font-normal'
                    >
                      <span className='truncate'>
                        {selectedAccessPlans.length
                          ? selectedAccessPlans.map((accessPlan) => accessPlan.name).join(', ')
                          : t('billing.accessPermissions.none')}
                      </span>
                      <ChevronsUpDown className='ml-2 size-4 shrink-0 opacity-50' />
                    </Button>
                  </PopoverTrigger>
                  <PopoverContent className='w-[var(--radix-popover-trigger-width)] p-0' align='start'>
                    <Command>
                      <CommandInput placeholder={t('billing.createPlan.groupsSearch')} />
                      <CommandList>
                        <CommandEmpty>{t('billing.createPlan.noGrantedGroup')}</CommandEmpty>
                        <CommandGroup>
                          <CommandItem
                            value={`no-access-permissions ${t('billing.accessPermissions.none')}`}
                            onSelect={() => setForm((current) => ({ ...current, accessPlanIDs: [] }))}
                          >
                            <Check className={cn('size-4', form.accessPlanIDs.length === 0 ? 'opacity-100' : 'opacity-0')} />
                            <span>{t('billing.accessPermissions.none')}</span>
                          </CommandItem>
                          {accessPlanOptions.map((accessPlan) => {
                            const selected = form.accessPlanIDs.includes(accessPlan.id);
                            return (
                              <CommandItem
                                key={accessPlan.id}
                                value={`${accessPlan.name} ${accessPlan.id}`}
                                onSelect={() =>
                                  setForm((current) => ({
                                    ...current,
                                    accessPlanIDs: toggleAccessPlanID(current.accessPlanIDs, accessPlan.id),
                                  }))
                                }
                              >
                                <Check className={cn('size-4', selected ? 'opacity-100' : 'opacity-0')} />
                                <span className='min-w-0 flex-1 truncate'>{accessPlan.name}</span>
                              </CommandItem>
                            );
                          })}
                        </CommandGroup>
                      </CommandList>
                    </Command>
                  </PopoverContent>
                </Popover>
                {bundlesQuery.isLoading && <p className='text-muted-foreground text-xs'>{t('billing.createPlan.groupsLoading')}</p>}
                {bundlesQuery.isError && (
                  <div className='flex items-center justify-between gap-3'>
                    <p className='text-destructive text-xs'>{t('billing.createPlan.groupsError')}</p>
                    <Button type='button' variant='ghost' size='sm' onClick={() => bundlesQuery.refetch()}>
                      <IconRefresh />
                      {t('billing.retry')}
                    </Button>
                  </div>
                )}
                <p className='text-muted-foreground text-xs text-pretty'>{t('billing.accessPermissions.hint')}</p>
              </div>
            </section>
            {isEditing && (
              <div className='space-y-2'>
                <Label htmlFor='plan-status'>{t('billing.editPlan.status')}</Label>
                <Select value={form.status} onValueChange={(value: SubscriptionPlan['status']) => setForm({ ...form, status: value })}>
                  <SelectTrigger id='plan-status'>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value='ENABLED'>{t('billing.editPlan.statusEnabled')}</SelectItem>
                    <SelectItem value='DISABLED'>{t('billing.editPlan.statusDisabled')}</SelectItem>
                    <SelectItem value='ARCHIVED'>{t('billing.editPlan.statusArchived')}</SelectItem>
                  </SelectContent>
                </Select>
              </div>
            )}
          </div>
          <DialogFooter>
            <Button type='button' variant='outline' onClick={() => onOpenChange(false)}>
              {t('billing.cancel')}
            </Button>
            <Button type='submit' disabled={pending}>
              {pending && <IconRefresh className='animate-spin' />}
              {t(isEditing ? 'billing.editPlan.submit' : 'billing.createPlan.submit')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function AllowanceStrip({ subscription, creditDisplayName }: { subscription: UserSubscription; creditDisplayName: string }) {
  const { t } = useTranslation();
  const items = [
    {
      label: t('billing.subscription.granted'),
      value: displayAmount(creditDisplayName, subscription.grantedAllowance),
      tone: 'text-foreground',
    },
    {
      label: t('billing.subscription.consumed'),
      value: displayAmount(creditDisplayName, subscription.consumedAllowance),
      tone: 'text-muted-foreground',
    },
    {
      label: t('billing.subscription.reserved'),
      value: displayAmount(creditDisplayName, subscription.reservedAllowance),
      tone: 'text-amber-700 dark:text-amber-400',
    },
    {
      label: t('billing.subscription.remaining'),
      value: displayAmount(creditDisplayName, subscription.remainingAllowance),
      tone: 'text-emerald-700 dark:text-emerald-400',
    },
  ];

  return (
    <div className='bg-muted/20 rounded-md border'>
      <div className='text-muted-foreground flex items-center gap-1 border-b border-dashed px-3 py-2 font-mono text-[10px] tracking-wide uppercase'>
        <span>{t('billing.subscription.allowanceFlow')}</span>
        <span className='ml-auto hidden items-center gap-1 sm:flex'>
          {t('billing.subscription.grantedShort')}
          <IconMinus size={11} />
          {t('billing.subscription.usedShort')}
          <IconMinus size={11} />
          {t('billing.subscription.reservedShort')}
          <IconEqual size={11} />
          {t('billing.subscription.remainingShort')}
        </span>
      </div>
      <dl className='grid grid-cols-2 divide-x divide-y sm:grid-cols-4 sm:divide-y-0'>
        {items.map((item) => (
          <div key={item.label} className='min-w-0 px-3 py-3'>
            <dt className='text-muted-foreground truncate text-[11px]'>{item.label}</dt>
            <dd className={`mt-1 truncate font-mono text-sm font-semibold tabular-nums ${item.tone}`} title={item.value}>
              {item.value}
            </dd>
          </div>
        ))}
      </dl>
    </div>
  );
}

function EmptyState({
  icon,
  title,
  description,
  compact = false,
}: {
  icon: React.ReactNode;
  title: string;
  description: string;
  compact?: boolean;
}) {
  return (
    <div
      className={`text-muted-foreground flex flex-col items-center justify-center text-center ${compact ? 'min-h-32 px-5' : 'min-h-52 rounded-lg border border-dashed px-6'}`}
    >
      <div className='mb-3 opacity-60 [&>svg]:size-7'>{icon}</div>
      <h3 className='text-foreground text-sm font-medium'>{title}</h3>
      <p className='mt-1 max-w-md text-xs'>{description}</p>
    </div>
  );
}

function BillingSkeleton() {
  return (
    <div className='space-y-5'>
      <div className='grid gap-3 sm:grid-cols-2 xl:grid-cols-4'>
        {[1, 2, 3, 4].map((item) => (
          <Skeleton key={item} className='h-32' />
        ))}
      </div>
      <div className='grid gap-5 xl:grid-cols-[1.5fr_0.8fr]'>
        <div className='space-y-5'>
          <Skeleton className='h-56' />
          <Skeleton className='h-64' />
        </div>
        <Skeleton className='h-96' />
      </div>
    </div>
  );
}
