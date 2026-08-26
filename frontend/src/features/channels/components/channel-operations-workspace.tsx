import {
  IconActivity,
  IconAlertTriangle,
  IconCheck,
  IconClock,
  IconCoin,
  IconFlask,
  IconHistory,
  IconKey,
  IconServer,
  IconSettings,
  IconWallet,
} from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { ScrollArea } from '@/components/ui/scroll-area';
import { Sheet, SheetContent, SheetDescription, SheetHeader, SheetTitle } from '@/components/ui/sheet';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { useChannels } from '../context/channels-context';
import type { Channel, ChannelProbePoint } from '../data/schema';
import { classifyChannelFailure, type ChannelFailureKind } from '../utils/failure-classifier';

type ChannelWithProbePoints = Channel & { probePoints?: ChannelProbePoint[] };

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  channel: ChannelWithProbePoints;
}

function quotaRecord(value: unknown): Record<string, unknown> {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as Record<string, unknown>) : {};
}

function displayValue(value: unknown): string {
  return value === null || value === undefined || value === '' ? '—' : String(value);
}

function Signal({ label, value, tone }: { label: string; value: string; tone: 'good' | 'warning' | 'bad' | 'neutral' }) {
  return (
    <div
      className={cn(
        'flex min-w-0 flex-1 items-center justify-between gap-3 border-l-4 px-3 py-2.5',
        tone === 'good' && 'border-l-emerald-500 bg-emerald-500/7',
        tone === 'warning' && 'border-l-amber-500 bg-amber-500/8',
        tone === 'bad' && 'border-l-red-500 bg-red-500/7',
        tone === 'neutral' && 'border-l-muted-foreground/35 bg-muted/25'
      )}
      data-tone={tone}
    >
      <div className='min-w-0'>
        <p className='text-muted-foreground text-[11px] font-semibold tracking-wide uppercase'>{label}</p>
        <p className='truncate text-sm font-medium'>{value}</p>
      </div>
      <span
        className={cn(
          'size-3 shrink-0 rounded-full ring-4',
          tone === 'good' && 'bg-emerald-500 ring-emerald-500/15',
          tone === 'warning' && 'bg-amber-500 ring-amber-500/15',
          tone === 'bad' && 'bg-red-500 ring-red-500/15',
          tone === 'neutral' && 'bg-muted-foreground/40 ring-muted-foreground/10'
        )}
      />
    </div>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return (
    <div className='border-border/70 border-b py-3 last:border-0'>
      <dt className='text-muted-foreground text-xs'>{label}</dt>
      <dd className='mt-1 font-mono text-sm font-semibold tabular-nums'>{value}</dd>
    </div>
  );
}

export function ChannelOperationsWorkspace({ open, onOpenChange, channel }: Props) {
  const { t } = useTranslation();
  const { setOpen } = useChannels();
  const points = channel.probePoints ?? [];
  const requestCount = points.reduce((sum, point) => sum + point.totalRequestCount, 0);
  const successCount = points.reduce((sum, point) => sum + point.successRequestCount, 0);
  const successRate = requestCount > 0 ? (successCount / requestCount) * 100 : null;
  const latencyPoints = points.filter((point) => point.avgTimeToFirstTokenMs != null);
  const averageLatency = latencyPoints.length
    ? latencyPoints.reduce((sum, point) => sum + (point.avgTimeToFirstTokenMs ?? 0), 0) / latencyPoints.length
    : null;
  const classifiedRequestFailure = classifyChannelFailure({
    errorMessage: channel.errorMessage,
    providerQuotaStatus: null,
    disabledAPIKeys: channel.disabledAPIKeys,
  });
  const classifiedFailure = classifyChannelFailure(channel);
  const operationalIssue = channel.operationalIssue;
  const requestFailure =
    operationalIssue?.source === 'channel_error'
      ? { kind: operationalIssue.category, evidence: classifiedRequestFailure?.evidence ?? [] }
      : classifiedRequestFailure;
  const failure = operationalIssue ? { kind: operationalIssue.category, evidence: classifiedFailure?.evidence ?? [] } : classifiedFailure;
  const isNewApi = channel.settings?.managementAdapter === 'new_api';
  const quota = quotaRecord(channel.providerQuotaStatus?.quotaData);
  const quotaStatus = channel.providerQuotaStatus?.status;
  const normalizedQuotaStatus = quotaStatus?.trim().toLowerCase();
  const configuredAPIKeys = new Set(
    [channel.credentials?.apiKey, ...(channel.credentials?.apiKeys ?? [])].filter((key): key is string => Boolean(key?.trim()))
  );
  const requestTone = requestFailure ? 'bad' : successRate === null ? 'neutral' : successRate >= 99 ? 'good' : 'warning';
  const accountTone = !isNewApi
    ? 'neutral'
    : operationalIssue?.source === 'provider_quota'
      ? operationalIssue.severity === 'warning'
        ? 'warning'
        : 'bad'
      : normalizedQuotaStatus === 'available'
        ? 'good'
        : normalizedQuotaStatus === 'warning'
          ? 'warning'
          : normalizedQuotaStatus === 'exhausted'
            ? 'bad'
            : normalizedQuotaStatus === 'unknown'
              ? 'neutral'
              : channel.providerQuotaStatus?.probeVerifiedAt
                ? 'good'
                : 'warning';

  const openDialog = (dialog: 'test' | 'testHistory' | 'quotaProbe' | 'price' | 'testAPIKeys' | 'errorResolved' | 'edit') => {
    setOpen(dialog);
  };

  const failureLabel = (kind: ChannelFailureKind) => t(`channels.operations.failureKinds.${kind}`);

  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent className='min-w-0 gap-0 overflow-hidden p-0 max-sm:!inset-auto max-sm:!top-0 max-sm:!right-0 max-sm:!bottom-0 max-sm:!left-0 max-sm:!h-[100dvh] max-sm:!w-[100vw] max-sm:!max-w-none max-sm:!translate-x-0 max-sm:!translate-y-0 max-sm:!transform-none max-sm:!animate-none max-sm:!transition-none sm:w-full sm:max-w-2xl lg:max-w-3xl'>
        <SheetHeader className='min-w-0 border-b px-5 py-4 pr-12'>
          <div className='flex flex-wrap items-center gap-2'>
            <SheetTitle className='text-lg'>{channel.name}</SheetTitle>
            <Badge variant='outline'>{channel.type}</Badge>
            <Badge variant={channel.status === 'enabled' ? 'default' : 'secondary'}>{t(`channels.status.${channel.status}`)}</Badge>
          </div>
          <SheetDescription className='font-mono text-xs'>{channel.baseURL || '—'}</SheetDescription>
          <div className='bg-muted/35 mt-2 grid grid-cols-1 divide-y rounded-lg border sm:grid-cols-2 sm:divide-x sm:divide-y-0'>
            <Signal
              label={t('channels.operations.signals.requestHealth')}
              value={
                requestFailure
                  ? failureLabel(requestFailure.kind)
                  : successRate === null
                    ? t('channels.operations.noRecentData')
                    : `${successRate.toFixed(1)}%`
              }
              tone={requestTone}
            />
            <Signal
              label={t('channels.operations.signals.newApiAccount')}
              value={
                !isNewApi
                  ? t('channels.operations.notConfigured')
                  : operationalIssue?.source === 'provider_quota'
                    ? t(`channels.operations.issueCodes.${operationalIssue.code}`, { defaultValue: operationalIssue.code })
                    : quotaStatus ||
                      (channel.providerQuotaStatus?.probeVerifiedAt
                        ? t('channels.operations.verified')
                        : t('channels.operations.awaitingVerification'))
              }
              tone={accountTone}
            />
          </div>
        </SheetHeader>

        <Tabs defaultValue='overview' className='flex min-h-0 max-w-full min-w-0 flex-1 flex-col overflow-hidden'>
          <TabsList className='hide-scroll mx-5 mt-4 flex h-auto max-w-full min-w-0 justify-start overflow-x-auto sm:grid sm:grid-cols-4'>
            <TabsTrigger className='shrink-0' value='overview'>
              {t('channels.operations.tabs.overview')}
            </TabsTrigger>
            <TabsTrigger className='shrink-0' value='health'>
              {t('channels.operations.tabs.health')}
            </TabsTrigger>
            <TabsTrigger className='shrink-0' value='newApi'>
              {t('channels.operations.tabs.newApi')}
            </TabsTrigger>
            <TabsTrigger className='shrink-0' value='troubleshooting'>
              {t('channels.operations.tabs.troubleshooting')}
            </TabsTrigger>
          </TabsList>
          <ScrollArea className='min-h-0 max-w-full min-w-0 flex-1'>
            <div className='max-w-full min-w-0 px-5 py-5'>
              <TabsContent value='overview' className='mt-0 space-y-6'>
                <section>
                  <h3 className='flex items-center gap-2 text-sm font-semibold'>
                    <IconServer size={17} />
                    {t('channels.operations.identity')}
                  </h3>
                  <dl className='mt-2 grid grid-cols-2 gap-x-6 border-y sm:grid-cols-4'>
                    <Metric label={t('channels.operations.channelId')} value={channel.id} />
                    <Metric label={t('channels.operations.defaultModel')} value={channel.defaultTestModel || '—'} />
                    <Metric label={t('channels.operations.modelCount')} value={String(channel.supportedModels.length)} />
                    <Metric label={t('channels.operations.apiKeyCount')} value={String(configuredAPIKeys.size)} />
                  </dl>
                </section>
                <section>
                  <h3 className='text-sm font-semibold'>{t('channels.operations.quickActions')}</h3>
                  <div className='mt-3 grid grid-cols-2 gap-2 sm:flex sm:flex-wrap'>
                    <Button size='sm' onClick={() => openDialog('test')}>
                      <IconFlask />
                      {t('channels.actions.test')}
                    </Button>
                    <Button size='sm' variant='outline' onClick={() => openDialog('testHistory')}>
                      <IconHistory />
                      {t('channels.actions.testHistory')}
                    </Button>
                    {isNewApi && (
                      <Button size='sm' variant='outline' onClick={() => openDialog('quotaProbe')}>
                        <IconWallet />
                        {t('channels.actions.queryUpstreamQuota')}
                      </Button>
                    )}
                    <Button size='sm' variant='outline' onClick={() => openDialog('edit')}>
                      <IconSettings />
                      {t('common.buttons.edit')}
                    </Button>
                  </div>
                </section>
              </TabsContent>

              <TabsContent value='health' className='mt-0 space-y-6'>
                <section className='flex flex-wrap items-start justify-between gap-3'>
                  <div>
                    <h3 className='flex items-center gap-2 text-sm font-semibold'>
                      <IconActivity size={17} />
                      {t('channels.operations.health.title')}
                    </h3>
                    <p className='text-muted-foreground mt-1 text-sm'>{t('channels.operations.health.description')}</p>
                  </div>
                  <div className='flex gap-2'>
                    <Button size='sm' onClick={() => openDialog('test')}>
                      <IconFlask />
                      {t('channels.operations.health.activeTest')}
                    </Button>
                    <Button size='sm' variant='outline' onClick={() => openDialog('testHistory')}>
                      <IconHistory />
                      {t('channels.actions.testHistory')}
                    </Button>
                  </div>
                </section>
                <dl className='grid grid-cols-2 gap-x-6 border-y sm:grid-cols-4'>
                  <Metric label={t('channels.operations.health.requests')} value={requestCount.toLocaleString()} />
                  <Metric
                    label={t('channels.operations.health.success')}
                    value={successRate === null ? '—' : `${successRate.toFixed(2)}%`}
                  />
                  <Metric
                    label={t('channels.operations.health.avgTtft')}
                    value={averageLatency === null ? '—' : `${Math.round(averageLatency)} ms`}
                  />
                  <Metric label={t('channels.operations.health.points')} value={String(points.length)} />
                </dl>
                {points.length === 0 ? (
                  <div className='text-muted-foreground rounded-lg border border-dashed p-8 text-center text-sm'>
                    {t('channels.operations.health.empty')}
                  </div>
                ) : (
                  <div className='space-y-2'>
                    {points
                      .slice(-6)
                      .reverse()
                      .map((point) => (
                        <div key={point.timestamp} className='flex items-center justify-between gap-4 border-b py-2 text-sm'>
                          <span className='text-muted-foreground flex items-center gap-2'>
                            <IconClock size={15} />
                            {new Date(point.timestamp * 1000).toLocaleString()}
                          </span>
                          <span className='font-mono tabular-nums'>
                            {point.successRequestCount}/{point.totalRequestCount}
                          </span>
                        </div>
                      ))}
                  </div>
                )}
              </TabsContent>

              <TabsContent value='newApi' className='mt-0 space-y-6'>
                {!isNewApi ? (
                  <div className='rounded-lg border border-dashed p-8 text-center'>
                    <IconSettings className='text-muted-foreground mx-auto mb-3' />
                    <h3 className='font-semibold'>{t('channels.operations.newApi.disabledTitle')}</h3>
                    <p className='text-muted-foreground mx-auto mt-1 max-w-md text-sm'>
                      {t('channels.operations.newApi.disabledDescription')}
                    </p>
                    <Button className='mt-4' size='sm' onClick={() => openDialog('edit')}>
                      {t('channels.operations.newApi.configure')}
                    </Button>
                  </div>
                ) : (
                  <>
                    <section className='flex flex-wrap items-start justify-between gap-3'>
                      <div>
                        <h3 className='flex items-center gap-2 text-sm font-semibold'>
                          <IconWallet size={17} />
                          {t('channels.operations.newApi.accountSnapshot')}
                        </h3>
                        <p className='text-muted-foreground mt-1 text-sm'>{t('channels.operations.newApi.secretHint')}</p>
                      </div>
                      <Button size='sm' onClick={() => openDialog('quotaProbe')}>
                        {t('channels.operations.newApi.probeQuota')}
                      </Button>
                    </section>
                    <dl className='grid grid-cols-2 gap-x-6 border-y sm:grid-cols-4'>
                      <Metric label={t('channels.quotaProbe.metrics.total')} value={displayValue(quota.total)} />
                      <Metric label={t('channels.quotaProbe.metrics.used')} value={displayValue(quota.used)} />
                      <Metric label={t('channels.quotaProbe.metrics.remaining')} value={displayValue(quota.remaining)} />
                      <Metric
                        label={t('channels.operations.newApi.lastVerified')}
                        value={
                          channel.providerQuotaStatus?.probeVerifiedAt
                            ? new Date(channel.providerQuotaStatus.probeVerifiedAt).toLocaleString()
                            : '—'
                        }
                      />
                    </dl>
                    <section>
                      <h3 className='flex items-center gap-2 text-sm font-semibold'>
                        <IconCoin size={17} />
                        {t('channels.operations.newApi.pricing')}
                      </h3>
                      <p className='text-muted-foreground mt-1 text-sm'>{t('channels.operations.newApi.pricingDescription')}</p>
                      <Button className='mt-3' size='sm' variant='outline' onClick={() => openDialog('price')}>
                        {t('channels.actions.modelPrice')}
                      </Button>
                    </section>
                    <section>
                      <h3 className='text-sm font-semibold'>{t('channels.operations.newApi.capabilities')}</h3>
                      <div className='mt-3 divide-y rounded-lg border'>
                        {['quota', 'accountBalance', 'pricing'].map((capability) => (
                          <div key={capability} className='flex items-center justify-between px-3 py-2 text-sm'>
                            <span>{t(`channels.operations.newApi.capability.${capability}`)}</span>
                            <Badge variant='outline'>{t('channels.operations.newApi.onDemand')}</Badge>
                          </div>
                        ))}
                      </div>
                    </section>
                  </>
                )}
              </TabsContent>

              <TabsContent value='troubleshooting' className='mt-0 space-y-5'>
                {failure ? (
                  <>
                    <div className='border-destructive/40 bg-destructive/5 rounded-lg border p-4'>
                      <div className='flex flex-wrap items-center gap-2 font-semibold text-red-700 dark:text-red-300'>
                        <IconAlertTriangle size={18} />
                        {failureLabel(failure.kind)}
                        {operationalIssue && (
                          <>
                            <Badge variant='outline'>{t(`channels.operations.severity.${operationalIssue.severity}`)}</Badge>
                            <code className='text-xs'>{operationalIssue.code}</code>
                          </>
                        )}
                      </div>
                      <p className='text-muted-foreground mt-1 text-sm'>{t(`channels.operations.recovery.${failure.kind}`)}</p>
                    </div>
                    <section>
                      <h3 className='text-sm font-semibold'>{t('channels.operations.evidence')}</h3>
                      <div className='mt-2 space-y-2'>
                        {failure.evidence.map((item, index) => (
                          <pre
                            key={index}
                            className='bg-muted/50 overflow-x-auto rounded-md border p-3 font-mono text-xs whitespace-pre-wrap'
                          >
                            {item}
                          </pre>
                        ))}
                      </div>
                    </section>
                    <section>
                      <h3 className='text-sm font-semibold'>{t('channels.operations.recoveryActions')}</h3>
                      <p className='text-muted-foreground mt-1 text-sm'>{t('channels.operations.retestBeforeClear')}</p>
                      <div className='mt-3 flex flex-wrap gap-2'>
                        <Button size='sm' onClick={() => openDialog('test')}>
                          <IconFlask />
                          {t('channels.operations.health.activeTest')}
                        </Button>
                        {(channel.credentials?.apiKeys?.length ?? 0) > 1 && (
                          <Button size='sm' variant='outline' onClick={() => openDialog('testAPIKeys')}>
                            <IconKey />
                            {t('channels.actions.testAPIKeys', { count: channel.credentials?.apiKeys?.length ?? 0 })}
                          </Button>
                        )}
                        {isNewApi && (
                          <Button size='sm' variant='outline' onClick={() => openDialog('quotaProbe')}>
                            <IconWallet />
                            {t('channels.actions.queryUpstreamQuota')}
                          </Button>
                        )}
                        {channel.errorMessage && (
                          <Button size='sm' variant='ghost' onClick={() => openDialog('errorResolved')}>
                            <IconCheck />
                            {t('channels.actions.markErrorResolved')}
                          </Button>
                        )}
                      </div>
                    </section>
                  </>
                ) : (
                  <div className='text-muted-foreground rounded-lg border border-dashed p-8 text-center text-sm'>
                    <IconCheck className='mx-auto mb-3 text-emerald-500' />
                    {t('channels.operations.noFailureEvidence')}
                  </div>
                )}
              </TabsContent>
            </div>
          </ScrollArea>
        </Tabs>
      </SheetContent>
    </Sheet>
  );
}
