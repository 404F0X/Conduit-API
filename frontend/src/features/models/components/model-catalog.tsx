import { type ReactNode, useEffect, useMemo, useRef, useState } from 'react';
import { Link, useNavigate, useSearch } from '@tanstack/react-router';
import type { SortingState } from '@tanstack/react-table';
import {
  IconAdjustmentsHorizontal,
  IconAlertTriangle,
  IconArrowsHorizontal,
  IconBox,
  IconCheck,
  IconChevronRight,
  IconDatabase,
  IconEdit,
  IconHeartbeat,
  IconCoin,
  IconPlus,
  IconRoute,
  IconSearch,
  IconServer,
} from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { DEFAULT_ACCOUNTING_CURRENCY_CODE, scaleDisplayAmount } from '@/lib/accounting';
import { cn } from '@/lib/utils';
import { useAdminPriceDisplay } from '@/hooks/use-admin-price-display';
import { usePermissions } from '@/hooks/usePermissions';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { ChannelManagementAdapterBadge } from '@/features/channels/components/channel-management-adapter-badge';
import { useChannels } from '@/features/channels/context/channels-context';
import { useChannelModelPrices, useChannelProbeData, useQueryChannels } from '@/features/channels/data/channels';
import type { Channel, ChannelProbeData, ChannelProbePoint, ChannelStatus, Pricing } from '@/features/channels/data/schema';
import { isNewApiChannelTag } from '@/features/channels/utils/channel-management-adapter';
import { useOperationsLedger } from '@/features/operations/data';
import { useGeneralSettings } from '@/features/system/data/system';
import { useModels } from '../context/models-context';
import { useChannelCatalogPrices } from '../data/catalog';
import {
  type ModelRoute,
  type PriceBookItem,
  type UpstreamModelDeployment,
  useCommercializationCatalog,
  useUpstreamSupplyCatalog,
} from '../data/commercialization';
import type { ProvidersData } from '../data/providers.schema';
import type { Model } from '../data/schema';
import { type ModelsCatalogSearch, validateModelsSearch } from '../model-search';
import { CommercializationPanel, type CommercializationAction } from './commercialization-panel';
import { aggregatePublicModelHealth, aggregateUpstreamModelHealth, getHealth } from './model-catalog-health';
import { priceColumns } from './model-catalog-pricing';
import { createColumns } from './models-columns';
import { ModelsTable } from './models-table';

type CatalogView = 'channels' | 'models';

interface ModelCatalogProps {
  models: Model[];
  modelsLoading: boolean;
  modelsTotalCount?: number;
  providers?: ProvidersData;
}

interface AggregatedModel {
  id: string;
  name: string;
  developer: string;
  channels: Channel[];
  routes: ModelRoute[];
  configuredModel?: Model;
  publishedRetailPrice?: PriceBookItem;
  retailCurrency: string;
}

const VIEW_STORAGE_KEY = 'models-catalog-view';

function statusTone(status: ChannelStatus) {
  if (status === 'enabled') return 'bg-emerald-500';
  if (status === 'disabled') return 'bg-amber-500';
  return 'bg-slate-400';
}

function healthTextClass(state: ReturnType<typeof getHealth>['state']) {
  if (state === 'healthy') return 'text-emerald-600 dark:text-emerald-400';
  if (state === 'warning') return 'text-amber-600 dark:text-amber-400';
  if (state === 'error') return 'text-red-600 dark:text-red-400';
  return 'text-muted-foreground';
}

function healthDotClass(state: ReturnType<typeof getHealth>['state']) {
  if (state === 'healthy') return 'bg-emerald-500';
  if (state === 'warning') return 'bg-amber-500';
  if (state === 'error') return 'bg-red-500';
  return 'bg-muted-foreground/40';
}

function healthEdgeClass(state: ReturnType<typeof getHealth>['state']) {
  if (state === 'healthy') return 'border-l-emerald-500';
  if (state === 'warning') return 'border-l-amber-500';
  if (state === 'error') return 'border-l-red-500';
  return 'border-l-muted-foreground/35';
}

function HealthValue({ health, className }: { health: ReturnType<typeof getHealth>; className?: string }) {
  const { t } = useTranslation();
  return (
    <span
      className={cn(
        'inline-flex min-w-0 items-center justify-center gap-1.5 font-mono tabular-nums',
        healthTextClass(health.state),
        className
      )}
    >
      <span className={cn('size-1.5 shrink-0 rounded-full', healthDotClass(health.state))} />
      <span className='min-w-0 truncate'>{health.rate == null ? t('models.catalog.noSamples') : `${(health.rate * 100).toFixed(1)}%`}</span>
    </span>
  );
}

function statusAccentClass(status: string) {
  switch (status.toLowerCase()) {
    case 'enabled':
      return 'border-emerald-500/30 bg-emerald-500/10 text-emerald-700 dark:text-emerald-400';
    case 'disabled':
      return 'border-amber-500/30 bg-amber-500/10 text-amber-700 dark:text-amber-400';
    default:
      return 'border-muted-foreground/25 bg-muted/60 text-muted-foreground';
  }
}

function statusAccentDotClass(status: string) {
  switch (status.toLowerCase()) {
    case 'enabled':
      return 'bg-emerald-500';
    case 'disabled':
      return 'bg-amber-500';
    default:
      return 'bg-muted-foreground/60';
  }
}

function StatusAccent({ status, children }: { status: string; children: ReactNode }) {
  return (
    <Badge variant='outline' className={cn('gap-1.5 font-medium', statusAccentClass(status))}>
      <span className={cn('size-1.5 rounded-full', statusAccentDotClass(status))} />
      {children}
    </Badge>
  );
}

function modelDetailSearch(modelID: string): ModelsCatalogSearch {
  return { view: 'models', model: modelID };
}

function channelDetailSearch(channelID: string, upstreamModel?: string, deployment?: string): ModelsCatalogSearch {
  return {
    view: 'channels',
    channel: channelID,
    ...(upstreamModel ? { upstreamModel } : {}),
    ...(deployment ? { deployment } : {}),
  };
}

function HealthRhythm({ points }: { points: ChannelProbePoint[] }) {
  const display = points.slice(-24);
  const padded = [...Array(Math.max(0, 24 - display.length)).fill(null), ...display] as Array<ChannelProbePoint | null>;
  return (
    <div className='flex h-8 items-end gap-0.5' aria-hidden='true'>
      {padded.map((point, index) => {
        const total = point?.totalRequestCount || 0;
        const rate = total ? (point?.successRequestCount || 0) / total : null;
        return (
          <span
            key={`${point?.timestamp || 'empty'}-${index}`}
            className={cn(
              'min-w-0 flex-1 rounded-[2px] transition-colors',
              rate === null && 'bg-muted-foreground/15 h-2',
              rate !== null && rate >= 0.9 && 'h-5 bg-emerald-500/80',
              rate !== null && rate >= 0.5 && rate < 0.9 && 'h-6 bg-amber-500/85',
              rate !== null && rate < 0.5 && 'h-8 bg-red-500/85'
            )}
          />
        );
      })}
    </div>
  );
}

function inferDeveloper(modelId: string, configured: Model | undefined, providers?: ProvidersData) {
  if (configured?.developer) return configured.developer;
  if (providers) {
    for (const [providerId, provider] of Object.entries(providers.providers)) {
      if (provider.models?.some((model) => model.id.toLowerCase() === modelId.toLowerCase())) return providerId;
    }
  }
  const value = modelId.toLowerCase();
  if (value.includes('claude')) return 'anthropic';
  if (value.startsWith('gpt-') || value.startsWith('o1') || value.startsWith('o3') || value.startsWith('o4')) return 'openai';
  if (value.includes('gemini')) return 'google';
  if (value.includes('deepseek')) return 'deepseek';
  if (value.includes('qwen')) return 'alibaba';
  if (value.includes('mistral') || value.includes('mixtral')) return 'mistral';
  return 'unknown';
}

function developerLabel(developer: string, providers: ProvidersData | undefined, t: ReturnType<typeof useTranslation>['t']) {
  const provider = providers?.providers[developer];
  const key = `models.developers.${developer}`;
  if (provider?.display_name || provider?.name) return provider.display_name || provider.name || developer;
  return t(key, { defaultValue: developer === 'unknown' ? t('models.catalog.unknownDeveloper') : developer });
}

const RETAIL_TOKEN_ITEMS = [
  { code: 'prompt_tokens', label: 'models.catalog.retailInput' },
  { code: 'completion_tokens', label: 'models.catalog.retailOutput' },
  { code: 'prompt_cached_tokens', label: 'models.catalog.retailCacheRead' },
  { code: 'prompt_write_cached_tokens', label: 'models.catalog.retailCacheWrite' },
] as const;

function retailPriceValues(item?: PriceBookItem) {
  if (!item) return null;
  const flatFee = item.price.items.find((entry) => entry.pricing.mode === 'flat_fee')?.pricing.flatFee;
  if (flatFee !== undefined && flatFee !== '') return { mode: 'request' as const, flatFee };
  const values = RETAIL_TOKEN_ITEMS.map(({ code, label }) => {
    const value = item.price.items.find((entry) => entry.itemCode === code)?.pricing.usagePerUnit;
    return { code, label, value: value === '' ? undefined : value };
  });
  return { mode: 'tokens' as const, values };
}

function RetailPriceSummary({ item, currency }: { item?: PriceBookItem; currency: string }) {
  const { t } = useTranslation();
  const display = useAdminPriceDisplay(currency);
  const price = retailPriceValues(item);
  if (!price) {
    return <div className='text-xs font-medium text-amber-600 dark:text-amber-400'>{t('models.catalog.retailMissing')}</div>;
  }
  if (price.mode === 'request') {
    const value = scaleDisplayAmount(price.flatFee, display.factor);
    return (
      <div>
        <div className='text-muted-foreground mb-1 text-[10px]'>
          {display.label} · {t('models.catalog.retailPerRequest')}
        </div>
        <span
          className='block min-w-0 truncate font-mono text-xs font-semibold tabular-nums'
          title={`${t('models.catalog.retailPerRequest')}: ${display.label} ${value ?? '—'}`}
          aria-label={`${t('models.catalog.retailPerRequest')}: ${display.label} ${value ?? '—'}`}
        >
          {value ?? '—'}
        </span>
      </div>
    );
  }
  return (
    <div>
      <div className='text-muted-foreground mb-1 text-[10px]'>
        {display.label} · {t('models.catalog.retailPerMillion')}
      </div>
      <div className='grid grid-cols-2 gap-x-4 gap-y-0.5'>
        {price.values.map(({ code, label, value }) => {
          const displayedValue = scaleDisplayAmount(value, display.factor);
          return (
            <div key={code} className='flex min-w-0 items-baseline justify-between gap-1.5'>
              <span className='text-muted-foreground shrink-0 text-[10px]'>{t(label)}</span>
              <span
                className={cn(
                  'min-w-0 truncate font-mono text-xs font-semibold tabular-nums',
                  displayedValue == null && 'text-amber-600 dark:text-amber-400'
                )}
                title={
                  displayedValue == null
                    ? `${t(label)}: ${t('models.catalog.retailUnset')}`
                    : `${t(label)}: ${display.label} ${displayedValue} / ${t('models.catalog.retailPerMillion')}`
                }
                aria-label={
                  displayedValue == null
                    ? `${t(label)}: ${t('models.catalog.retailUnset')}`
                    : `${t(label)}: ${display.label} ${displayedValue} / ${t('models.catalog.retailPerMillion')}`
                }
              >
                {displayedValue == null ? t('models.catalog.retailUnset') : displayedValue}
              </span>
            </div>
          );
        })}
      </div>
    </div>
  );
}

function ChannelPriceCells({ values }: { values: ReturnType<typeof priceColumns> }) {
  const display = useAdminPriceDisplay(values.currency);
  return (
    <>
      {[values.request, values.input, values.output, values.cacheRead, values.cacheWrite].map((value, index) => {
        const displayedValue = value === '—' ? null : display.amount(value);
        return (
          <TableCell key={index} className='text-right font-mono text-xs tabular-nums'>
            {value === '—' ? '—' : `${display.label} ${displayedValue ?? '—'}`}
          </TableCell>
        );
      })}
    </>
  );
}

function EmptyCatalog({ title, description }: { title: string; description: string }) {
  return (
    <div className='bg-muted/20 flex min-h-64 flex-col items-center justify-center rounded-xl border border-dashed px-6 text-center'>
      <div className='bg-background mb-4 flex size-11 items-center justify-center rounded-xl border shadow-sm'>
        <IconBox className='text-muted-foreground size-5' />
      </div>
      <h3 className='font-medium'>{title}</h3>
      <p className='text-muted-foreground mt-1 max-w-md text-sm'>{description}</p>
    </div>
  );
}

function CatalogSkeleton() {
  return (
    <div className='grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3'>
      {Array.from({ length: 6 }).map((_, index) => (
        <Card key={index} className='rounded-xl'>
          <CardContent className='space-y-5 p-5'>
            <div className='flex justify-between'>
              <Skeleton className='h-5 w-36' />
              <Skeleton className='h-5 w-16' />
            </div>
            <Skeleton className='h-8 w-full' />
            <div className='flex gap-2'>
              <Skeleton className='h-5 w-20' />
              <Skeleton className='h-5 w-24' />
            </div>
          </CardContent>
        </Card>
      ))}
    </div>
  );
}

function ChannelDetailDialog({
  channel,
  open,
  onOpenChange,
  points,
  highlightedUpstreamModel,
  deployments,
  deploymentsLoading,
}: {
  channel: Channel | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  points: ChannelProbePoint[];
  highlightedUpstreamModel?: string;
  deployments: UpstreamModelDeployment[];
  deploymentsLoading: boolean;
}) {
  const { t } = useTranslation();
  const pricesQuery = useChannelModelPrices(channel?.id || '');
  const prices = useMemo(() => new Map((pricesQuery.data || []).map((price) => [price.modelID, price])), [pricesQuery.data]);
  const health = getHealth(points);
  const deploymentRows = deployments
    .filter((deployment) => deployment.channelID === channel?.id)
    .sort((a, b) => a.upstreamModelID.localeCompare(b.upstreamModelID) || a.variant.localeCompare(b.variant));
  const highlightedRowRef = useRef<HTMLTableRowElement | null>(null);

  useEffect(() => {
    if (!open || !highlightedUpstreamModel) return;
    highlightedRowRef.current?.scrollIntoView({ block: 'center', inline: 'nearest' });
  }, [highlightedUpstreamModel, open, pricesQuery.isLoading]);

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='flex max-h-[88vh] flex-col overflow-hidden sm:max-w-5xl'>
        <DialogHeader className='pr-8'>
          <div className='flex flex-wrap items-center gap-2'>
            <DialogTitle>{channel?.name}</DialogTitle>
            {channel && (
              <Badge variant='outline' className='font-mono text-[11px]'>
                {channel.type}
              </Badge>
            )}
          </div>
          <DialogDescription>{t('models.catalog.channelDetailDescription', { count: deploymentRows.length })}</DialogDescription>
        </DialogHeader>
        <div className='grid grid-cols-2 gap-3 sm:grid-cols-4'>
          <div className='bg-muted/20 rounded-lg border p-3'>
            <div className='text-muted-foreground text-xs'>{t('models.catalog.status')}</div>
            <div className='mt-1 text-sm font-medium'>{channel ? t(`models.catalog.statuses.${channel.status}`) : '—'}</div>
          </div>
          <div className='bg-muted/20 rounded-lg border p-3'>
            <div className='text-muted-foreground text-xs'>{t('models.catalog.health')}</div>
            <div className='mt-1 text-sm font-medium'>
              {health.rate == null ? t('models.catalog.noSamples') : `${(health.rate * 100).toFixed(1)}%`}
            </div>
          </div>
          <div className='bg-muted/20 rounded-lg border p-3'>
            <div className='text-muted-foreground text-xs'>{t('models.catalog.models')}</div>
            <div className='mt-1 font-mono text-sm font-medium'>{deploymentRows.length}</div>
          </div>
          <div className='bg-muted/20 rounded-lg border p-3'>
            <div className='text-muted-foreground text-xs'>{t('models.catalog.priced')}</div>
            <div className='mt-1 font-mono text-sm font-medium'>{pricesQuery.data?.length ?? '—'}</div>
          </div>
        </div>
        <div className='relative flex min-h-0 flex-1 flex-col'>
          <div className='text-muted-foreground mb-2 flex items-center gap-1.5 text-[11px] sm:hidden'>
            <IconArrowsHorizontal className='size-3.5' />
            {t('models.catalog.scrollForPrices')}
          </div>
          <div className='min-h-0 flex-1 overflow-auto rounded-lg border'>
            <div className='min-w-[880px]'>
              <Table>
                <TableHeader className='bg-background sticky top-0 z-10'>
                  <TableRow>
                    <TableHead className='bg-background sticky left-0 z-20 border-r shadow-[8px_0_12px_-12px_rgba(0,0,0,0.45)]'>
                      {t('models.catalog.modelId')}
                    </TableHead>
                    <TableHead className='text-right'>{t('models.catalog.requestPrice')}</TableHead>
                    <TableHead className='text-right'>{t('models.catalog.inputPrice')}</TableHead>
                    <TableHead className='text-right'>{t('models.catalog.outputPrice')}</TableHead>
                    <TableHead className='text-right'>{t('models.catalog.cacheReadPrice')}</TableHead>
                    <TableHead className='text-right'>{t('models.catalog.cacheWritePrice')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {deploymentRows.map((deployment) => {
                    const id = deployment.upstreamModelID;
                    const values = priceColumns(prices.get(id));
                    const highlighted = highlightedUpstreamModel === id;
                    return (
                      <TableRow
                        key={deployment.id}
                        ref={highlighted ? highlightedRowRef : undefined}
                        className={cn(highlighted && 'bg-amber-500/10 hover:bg-amber-500/15')}
                        data-highlighted-upstream-model={highlighted ? 'true' : undefined}
                      >
                        <TableCell className='bg-background sticky left-0 z-[1] border-r font-mono text-xs font-medium shadow-[8px_0_12px_-12px_rgba(0,0,0,0.45)]'>
                          {channel ? (
                            <Link
                              to='/models'
                              search={channelDetailSearch(channel.id, id, deployment.id)}
                              data-testid='upstream-model-link'
                              className='focus-visible:ring-ring inline-flex min-h-10 min-w-0 items-center rounded-sm underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:outline-none'
                            >
                              <span className='min-w-0'>
                                <span className='block truncate'>{id}</span>
                                {deployment.variant && (
                                  <span className='text-muted-foreground block text-[10px]'>{deployment.variant}</span>
                                )}
                              </span>
                            </Link>
                          ) : (
                            id
                          )}
                        </TableCell>
                        <ChannelPriceCells values={values} />
                      </TableRow>
                    );
                  })}
                  {!deploymentsLoading && deploymentRows.length === 0 && (
                    <TableRow>
                      <TableCell colSpan={6} className='text-muted-foreground h-28 text-center'>
                        {t('models.catalog.noChannelModels')}
                      </TableCell>
                    </TableRow>
                  )}
                </TableBody>
              </Table>
            </div>
          </div>
          <div className='from-background/80 pointer-events-none absolute right-0 bottom-0 h-[calc(100%-1.5rem)] w-6 bg-gradient-to-l to-transparent sm:hidden' />
        </div>
        {pricesQuery.isError && <p className='text-xs text-amber-600 dark:text-amber-400'>{t('models.catalog.priceUnavailable')}</p>}
        <DialogFooter>
          <Button variant='outline' onClick={() => onOpenChange(false)}>
            {t('common.buttons.close')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function PricingValue({ pricing, currency }: { pricing?: Pricing; currency: string }) {
  const { t } = useTranslation();
  const display = useAdminPriceDisplay(currency);
  if (!pricing) return <span className='text-muted-foreground'>{t('models.catalog.notConfigured')}</span>;
  if (pricing.mode === 'flat_fee') {
    const value = scaleDisplayAmount(pricing.flatFee, display.factor);
    return (
      <div>
        <div className='font-mono text-sm font-semibold tabular-nums'>
          {display.label} {value ?? '—'}
        </div>
        <div className='text-muted-foreground mt-0.5 text-[10px]'>{t('models.catalog.perRequest')}</div>
      </div>
    );
  }
  if (pricing.mode === 'usage_per_unit') {
    const value = scaleDisplayAmount(pricing.usagePerUnit, display.factor);
    return (
      <div>
        <div className='font-mono text-sm font-semibold tabular-nums'>
          {display.label} {value ?? '—'}
        </div>
        <div className='text-muted-foreground mt-0.5 text-[10px]'>{t('models.catalog.perMillionTokens')}</div>
      </div>
    );
  }
  return (
    <div className='space-y-1.5'>
      <div className='text-muted-foreground text-[10px]'>
        {t(pricing.mode === 'usage_volume' ? 'models.catalog.volumePricing' : 'models.catalog.tieredPricing')}
      </div>
      {(pricing.usageTiered?.tiers || []).map((tier, index) => (
        <div
          key={`${tier.upTo ?? 'infinity'}-${index}`}
          className='flex items-baseline justify-between gap-3 font-mono text-[11px] tabular-nums'
        >
          <span className='text-muted-foreground'>{tier.upTo == null ? t('models.catalog.remainingTokens') : `≤ ${tier.upTo}`}</span>
          <span>
            {t('models.catalog.configuredCostPerMillion', {
              currency: display.label,
              value: scaleDisplayAmount(tier.pricePerUnit, display.factor) ?? '—',
            })}
          </span>
        </div>
      ))}
    </div>
  );
}

function UpstreamModelDetailDialog({
  channel,
  deployment,
  loading,
  loadError,
  open,
  onOpenChange,
  routes,
  publicModels,
  canReadHealth,
  canEditPrice,
  accountingCurrencyCode,
  onEditPrice,
}: {
  channel: Channel | null;
  deployment: UpstreamModelDeployment | null;
  loading: boolean;
  loadError: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  routes: ModelRoute[];
  publicModels: Model[];
  canReadHealth: boolean;
  canEditPrice: boolean;
  accountingCurrencyCode: string;
  onEditPrice: () => void;
}) {
  const { t } = useTranslation();
  const pricesQuery = useChannelModelPrices(channel?.id || '');
  const healthQuery = useOperationsLedger(1, open && canReadHealth && !!deployment);
  const price = pricesQuery.data?.find((item) => item.modelID === deployment?.upstreamModelID);
  const publicModelsByGID = new Map(publicModels.map((model) => [model.id, model]));
  const linkedRoutes = deployment
    ? routes.filter((route) => route.deploymentID === deployment.id && publicModelsByGID.has(route.publicModelID))
    : [];
  const suppliedRoutes = linkedRoutes.filter((route) => {
    const publicModel = publicModelsByGID.get(route.publicModelID);
    return (
      route.status === 'ENABLED' && deployment?.status === 'ENABLED' && channel?.status === 'enabled' && publicModel?.status === 'enabled'
    );
  });
  const health = deployment
    ? aggregateUpstreamModelHealth(healthQuery.data?.routeHealth || [], deployment.channelID, deployment.upstreamModelID)
    : aggregateUpstreamModelHealth([], '', '');
  const healthLoading = canReadHealth && healthQuery.isLoading;
  const healthEdgeTone =
    healthLoading || !canReadHealth
      ? 'border-l-muted-foreground/35'
      : healthQuery.isError
        ? 'border-l-red-500'
        : healthEdgeClass(health.state);
  const priceItems = new Map(price?.price.items.map((item) => [item.itemCode, item]));
  const flatFeePrice = price?.price.items.find((item) => item.pricing.mode === 'flat_fee')?.pricing;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        className='flex max-h-[calc(100dvh-2rem)] flex-col overflow-hidden rounded-lg sm:max-h-[780px] sm:max-w-5xl'
        data-testid='upstream-model-detail'
      >
        {loading ? (
          <div className='space-y-5 py-2' data-testid='upstream-model-detail-loading'>
            <Skeleton className='h-7 w-64' />
            <Skeleton className='h-16 w-full' />
            <Skeleton className='h-32 w-full' />
            <Skeleton className='h-48 w-full' />
          </div>
        ) : !deployment ? (
          <div
            className='flex min-h-64 flex-col items-center justify-center gap-3 text-center'
            data-testid='upstream-model-detail-not-found'
          >
            <IconAlertTriangle className='size-7 text-amber-500' />
            <div>
              <DialogTitle>{loadError ? t('models.catalog.deploymentLoadFailed') : t('models.catalog.deploymentNotFound')}</DialogTitle>
              <DialogDescription className='mt-1'>
                {loadError ? t('models.catalog.deploymentLoadFailedDescription') : t('models.catalog.deploymentNotFoundDescription')}
              </DialogDescription>
            </div>
            <Button variant='outline' onClick={() => onOpenChange(false)}>
              {t('models.catalog.backToChannel')}
            </Button>
          </div>
        ) : (
          <>
            <DialogHeader className='shrink-0 pr-8 text-left'>
              <div className='flex flex-wrap items-center gap-2'>
                <DialogTitle className='font-mono break-words' title={deployment.upstreamModelID}>
                  {deployment.upstreamModelID}
                </DialogTitle>
                <StatusAccent status={deployment.status}>
                  {t(`models.catalog.routeStatuses.${deployment.status.toLowerCase()}`)}
                </StatusAccent>
              </div>
              <DialogDescription className='flex flex-wrap gap-x-4 gap-y-1 text-left'>
                <span>{channel?.name}</span>
                <span className='font-mono'>{channel?.type}</span>
                <span>
                  {t('models.catalog.source')}: {deployment.source || '—'}
                </span>
                <span>
                  {t('models.catalog.variant')}:{' '}
                  <span className='font-mono'>{deployment.variant || t('models.catalog.defaultVariant')}</span>
                </span>
              </DialogDescription>
            </DialogHeader>

            <div className='grid shrink-0 border sm:grid-cols-[minmax(0,1.55fr)_minmax(150px,0.8fr)_minmax(155px,0.9fr)]'>
              <div className={cn('bg-muted/10 border-b border-l-4 p-3 sm:border-r sm:border-b-0', healthEdgeTone)}>
                <div className='text-muted-foreground text-xs'>{t('models.catalog.exactModelHealth')}</div>
                <div className={cn('mt-1 min-h-7 text-lg font-semibold', healthQuery.isError && 'text-red-600 dark:text-red-400')}>
                  {healthLoading ? (
                    <Skeleton className='h-6 w-20 rounded-sm' data-testid='upstream-model-health-loading' />
                  ) : !canReadHealth ? (
                    t('models.catalog.healthNoPermission')
                  ) : healthQuery.isError ? (
                    t('models.catalog.healthLoadFailed')
                  ) : (
                    <HealthValue health={health} className='justify-start gap-2 text-lg' />
                  )}
                </div>
                <div className='text-muted-foreground mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 text-[11px]'>
                  {healthLoading
                    ? t('models.catalog.healthLoading')
                    : canReadHealth && !healthQuery.isError
                      ? t('models.catalog.healthSampleBasis', { attempts: health.attempts, successes: health.successes })
                      : t('models.catalog.healthWindow')}
                </div>
              </div>
              <div className='bg-muted/10 border-b p-3 sm:border-r sm:border-b-0'>
                <div className='text-muted-foreground text-xs'>{t('models.catalog.supplyingPublicModels')}</div>
                <div className='mt-1 font-mono text-base font-semibold tabular-nums'>{suppliedRoutes.length}</div>
                <div className='text-muted-foreground mt-1 text-[11px]'>{t('models.catalog.runtimeEligibleRoutes')}</div>
              </div>
              <div className='bg-muted/10 p-3'>
                <div className='text-muted-foreground text-xs'>{t('models.catalog.purchasePrice')}</div>
                <div className='mt-1 text-base font-semibold'>
                  {pricesQuery.isError
                    ? t('models.catalog.loadFailed')
                    : price
                      ? t('models.catalog.configured')
                      : t('models.catalog.notConfigured')}
                </div>
                <div className='text-muted-foreground mt-1 text-[11px]'>
                  {pricesQuery.isError
                    ? t('models.catalog.priceUnavailable')
                    : price
                      ? t('models.catalog.noCurrencyConfigured', { currency: price.currencyCode })
                      : t('models.catalog.purchasePriceNotConfigured')}
                </div>
              </div>
            </div>

            <div className='min-h-0 flex-1 space-y-3 overflow-auto pr-1'>
              <section className='border' aria-labelledby='upstream-purchase-price'>
                <div className='border-b px-4 py-3'>
                  <h3 id='upstream-purchase-price' className='text-sm font-semibold'>
                    {t('models.catalog.purchasePrice')}
                  </h3>
                  <p className='text-muted-foreground mt-0.5 text-xs'>
                    {t('models.catalog.purchasePriceDescription', {
                      currency: price?.currencyCode || accountingCurrencyCode,
                    })}
                  </p>
                </div>
                {pricesQuery.isLoading ? (
                  <div className='bg-border grid grid-cols-2 gap-px p-px sm:grid-cols-4'>
                    {[0, 1, 2, 3].map((key) => (
                      <Skeleton key={key} className='h-20 rounded-none' />
                    ))}
                  </div>
                ) : pricesQuery.isError ? (
                  <p className='px-4 py-6 text-sm text-amber-600 dark:text-amber-400'>{t('models.catalog.priceUnavailable')}</p>
                ) : !price ? (
                  <p className='text-muted-foreground px-4 py-6 text-sm'>{t('models.catalog.purchasePriceNotConfigured')}</p>
                ) : flatFeePrice ? (
                  <div className='bg-background p-3'>
                    <div className='text-muted-foreground mb-2 text-[11px]'>{t('models.catalog.requestPrice')}</div>
                    <PricingValue pricing={flatFeePrice} currency={price.currencyCode} />
                  </div>
                ) : (
                  <div className='bg-border grid grid-cols-2 gap-px sm:grid-cols-4'>
                    {[
                      ['models.catalog.inputPrice', 'prompt_tokens'],
                      ['models.catalog.outputPrice', 'completion_tokens'],
                      ['models.catalog.cacheReadPrice', 'prompt_cached_tokens'],
                      ['models.catalog.cacheWritePrice', 'prompt_write_cached_tokens'],
                    ].map(([label, code]) => (
                      <div key={code} className='bg-background p-3'>
                        <div className='text-muted-foreground mb-2 text-[11px]'>{t(label)}</div>
                        <PricingValue pricing={priceItems.get(code)?.pricing} currency={price.currencyCode} />
                      </div>
                    ))}
                  </div>
                )}
              </section>

              <section className='border' aria-labelledby='upstream-public-models'>
                <div className='border-b px-4 py-3'>
                  <h3 id='upstream-public-models' className='text-sm font-semibold'>
                    {t('models.catalog.supplyingPublicModels')}
                  </h3>
                </div>
                <div className='text-muted-foreground flex items-center gap-1.5 px-4 pt-3 text-[11px] sm:hidden'>
                  <IconArrowsHorizontal className='size-3.5' />
                  {t('models.catalog.scrollForRoutes')}
                </div>
                <div className='overflow-x-auto'>
                  <div className='min-w-[560px]'>
                    <Table>
                      <TableHeader>
                        <TableRow>
                          <TableHead className='bg-background sticky left-0 z-10 min-w-72 border-r shadow-[8px_0_12px_-12px_rgba(0,0,0,0.45)]'>
                            {t('models.catalog.publicModel')}
                          </TableHead>
                          <TableHead>{t('models.catalog.routeStatus')}</TableHead>
                          <TableHead>{t('models.catalog.publicModelStatus')}</TableHead>
                        </TableRow>
                      </TableHeader>
                      <TableBody>
                        {linkedRoutes.map((route) => {
                          const publicModel = publicModelsByGID.get(route.publicModelID)!;
                          return (
                            <TableRow key={route.id}>
                              <TableCell className='bg-background sticky left-0 z-[1] border-r shadow-[8px_0_12px_-12px_rgba(0,0,0,0.45)]'>
                                <Link
                                  to='/models'
                                  search={modelDetailSearch(publicModel.modelID)}
                                  className='focus-visible:ring-ring inline-flex min-h-10 max-w-64 items-center font-mono text-xs underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:outline-none'
                                >
                                  <span className='min-w-0'>
                                    <span className='block truncate font-sans font-medium' title={publicModel.name}>
                                      {publicModel.name}
                                    </span>
                                    <span className='text-muted-foreground block truncate' title={publicModel.modelID}>
                                      {publicModel.modelID}
                                    </span>
                                  </span>
                                </Link>
                              </TableCell>
                              <TableCell>
                                <StatusAccent status={route.status}>
                                  {t(`models.catalog.routeStatuses.${route.status.toLowerCase()}`)}
                                </StatusAccent>
                              </TableCell>
                              <TableCell>
                                <StatusAccent status={publicModel.status}>
                                  {t(`models.catalog.statuses.${publicModel.status}`)}
                                </StatusAccent>
                              </TableCell>
                            </TableRow>
                          );
                        })}
                        {linkedRoutes.length === 0 && (
                          <TableRow>
                            <TableCell colSpan={3} className='text-muted-foreground h-24 text-center'>
                              {t('models.catalog.noSuppliedPublicModels')}
                            </TableCell>
                          </TableRow>
                        )}
                      </TableBody>
                    </Table>
                  </div>
                </div>
              </section>
            </div>

            <DialogFooter className='shrink-0'>
              <Button variant='outline' onClick={() => onOpenChange(false)}>
                {t('models.catalog.backToChannel')}
              </Button>
              {canEditPrice && (
                <Button onClick={onEditPrice} data-testid='configure-upstream-purchase-price'>
                  <IconCoin className='size-4' />
                  {t('models.catalog.configurePurchasePrice')}
                </Button>
              )}
            </DialogFooter>
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function ModelDetailDialog({
  model,
  open,
  onOpenChange,
  probeMap,
  providers,
  accountingCurrencyCode,
  onCommercializationAction,
}: {
  model: AggregatedModel | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  probeMap: Map<string, ChannelProbePoint[]>;
  providers?: ProvidersData;
  accountingCurrencyCode: string;
  onCommercializationAction: (action: CommercializationAction) => void;
}) {
  const { t } = useTranslation();
  const { modelPermissions, hasSystemScope } = usePermissions();
  const canWriteRoutes = hasSystemScope('write_channels');
  const canWriteCommercialization = hasSystemScope('write_commercialization');
  const { setCurrentRow, setOpen } = useModels();
  const edit = () => {
    if (!model?.configuredModel) return;
    onOpenChange(false);
    setCurrentRow(model.configuredModel);
    setOpen('edit');
  };
  const health = model ? aggregatePublicModelHealth(model.routes, model.channels, probeMap) : getHealth([]);
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='flex h-[min(760px,calc(100dvh-2rem))] flex-col overflow-hidden sm:max-w-5xl'>
        <DialogHeader className='shrink-0 pr-8'>
          <DialogTitle>{model?.name}</DialogTitle>
          <DialogDescription className='font-mono'>{model?.id}</DialogDescription>
        </DialogHeader>
        <div className='grid shrink-0 gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(220px,auto)]'>
          <div className='bg-muted/20 rounded-lg border p-3'>
            <div className='text-muted-foreground text-xs'>{t('models.catalog.publicModelHealth')}</div>
            <div className='mt-1 flex items-center gap-2'>
              <code className='min-w-0 truncate text-sm font-semibold'>{model?.id}</code>
              <IconChevronRight className='text-muted-foreground size-4 shrink-0' />
              <span className='min-w-0 text-sm font-semibold'>
                <HealthValue health={health} />
              </span>
            </div>
            <div className='mt-2 flex flex-wrap gap-2'>
              {model && <Badge variant='secondary'>{developerLabel(model.developer, providers, t)}</Badge>}
            </div>
          </div>
          <div className='bg-muted/20 rounded-lg border p-3'>
            <div className='text-muted-foreground text-xs'>{t('models.catalog.retailPrice')}</div>
            <div className='mt-2'>
              <RetailPriceSummary item={model?.publishedRetailPrice} currency={accountingCurrencyCode} />
            </div>
          </div>
        </div>
        <Alert className='shrink-0'>
          <IconRoute />
          <AlertTitle>{t('models.catalog.routingOrderTitle')}</AlertTitle>
          <AlertDescription>{t('models.catalog.routingOrderDescription')}</AlertDescription>
        </Alert>
        <div className='relative flex min-h-0 flex-1 flex-col'>
          <div className='text-muted-foreground mb-2 flex items-center gap-1.5 text-[11px] sm:hidden'>
            <IconArrowsHorizontal className='size-3.5' />
            {t('models.catalog.scrollForRoutes')}
          </div>
          <div className='min-h-0 flex-1 overflow-auto rounded-lg border'>
            <div className='min-w-[860px]'>
              <Table>
                <TableHeader className='bg-background sticky top-0 z-10'>
                  <TableRow>
                    <TableHead>{t('models.catalog.channel')}</TableHead>
                    <TableHead>{t('models.catalog.upstreamModelId')}</TableHead>
                    <TableHead>{t('models.catalog.routeStatus')}</TableHead>
                    <TableHead>{t('models.catalog.channelStatus')}</TableHead>
                    <TableHead className='text-right'>{t('models.catalog.channelWeight')}</TableHead>
                    <TableHead className='text-right'>{t('models.catalog.health')}</TableHead>
                    <TableHead className='text-right'>{t('models.catalog.actions')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {model?.routes.map((route) => {
                    const channel = model.channels.find((item) => item.id === route.channelID);
                    const health = getHealth(channel ? probeMap.get(channel.id) || [] : []);
                    return (
                      <TableRow key={route.id}>
                        <TableCell className='font-medium'>
                          {channel ? (
                            <Link
                              to='/models'
                              search={channelDetailSearch(channel.id)}
                              className='focus-visible:ring-ring rounded-sm underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:outline-none'
                            >
                              {route.channelName}
                            </Link>
                          ) : (
                            route.channelName
                          )}
                        </TableCell>
                        <TableCell className='font-mono text-xs'>
                          {channel ? (
                            <Link
                              to='/models'
                              search={channelDetailSearch(channel.id, route.upstreamModelID, route.deploymentID)}
                              className='focus-visible:ring-ring rounded-sm underline-offset-4 hover:underline focus-visible:ring-2 focus-visible:outline-none'
                            >
                              {route.upstreamModelID}
                            </Link>
                          ) : (
                            route.upstreamModelID
                          )}
                        </TableCell>
                        <TableCell>
                          <span className='inline-flex items-center gap-2 text-xs'>
                            <span
                              className={cn(
                                'size-1.5 rounded-full',
                                route.status === 'ENABLED'
                                  ? 'bg-emerald-500'
                                  : route.status === 'DISABLED'
                                    ? 'bg-amber-500'
                                    : 'bg-slate-400'
                              )}
                            />
                            {t(`models.catalog.routeStatuses.${route.status.toLowerCase()}`)}
                          </span>
                        </TableCell>
                        <TableCell>{channel ? t(`models.catalog.statuses.${channel.status}`) : '—'}</TableCell>
                        <TableCell className='text-right font-mono text-xs tabular-nums'>{channel?.orderingWeight ?? 0}</TableCell>
                        <TableCell className='text-right font-mono text-xs'>
                          {health.rate == null ? '—' : `${(health.rate * 100).toFixed(1)}%`}
                        </TableCell>
                        <TableCell className='text-right'>
                          <Button
                            size='sm'
                            variant='ghost'
                            disabled={!canWriteRoutes}
                            onClick={() => onCommercializationAction({ kind: 'edit-route', route })}
                          >
                            {t('models.catalog.editRoute')}
                          </Button>
                        </TableCell>
                      </TableRow>
                    );
                  })}
                  {model?.routes.length === 0 && (
                    <TableRow>
                      <TableCell colSpan={7} className='text-muted-foreground h-24 text-center'>
                        {t('models.catalog.noRoutes')}
                      </TableCell>
                    </TableRow>
                  )}
                </TableBody>
              </Table>
            </div>
          </div>
          <div className='from-background/80 pointer-events-none absolute right-0 bottom-0 h-[calc(100%-1.5rem)] w-6 bg-gradient-to-l to-transparent sm:hidden' />
        </div>
        <DialogFooter className='shrink-0 border-t pt-4'>
          {model?.configuredModel && canWriteRoutes && (
            <Button
              variant='outline'
              className='w-full sm:w-auto'
              onClick={() => onCommercializationAction({ kind: 'add-route', publicModelID: model.configuredModel!.id })}
            >
              <IconPlus className='size-4' />
              {t('models.catalog.addRoute')}
            </Button>
          )}
          {model?.configuredModel && canWriteCommercialization && (
            <Button
              variant='outline'
              className='w-full sm:w-auto'
              onClick={() => onCommercializationAction({ kind: 'price', publicModelID: model.configuredModel!.id })}
            >
              <IconCoin className='size-4' />
              {t('models.catalog.setRetailPrice')}
            </Button>
          )}
          {model?.configuredModel && modelPermissions.canWrite && (
            <Button variant='outline' className='w-full sm:w-auto' onClick={edit}>
              <IconEdit className='size-4' />
              {t('models.catalog.editConfiguration')}
            </Button>
          )}
          <Button className='w-full sm:w-auto' onClick={() => onOpenChange(false)}>
            {t('common.buttons.close')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function ModelCatalog({ models, modelsLoading, modelsTotalCount, providers }: ModelCatalogProps) {
  const { t } = useTranslation();
  const { data: generalSettings } = useGeneralSettings();
  const accountingCurrencyCode = generalSettings?.accountingCurrencyCode?.trim() || DEFAULT_ACCOUNTING_CURRENCY_CODE;
  const urlSearch = useSearch({ from: '/_authenticated/models/' }) as ModelsCatalogSearch;
  const navigate = useNavigate();
  const { modelPermissions, hasSystemScope } = usePermissions();
  const { setCurrentRow: setPriceChannel, setOpen: setChannelDialog, setSelectedPriceModelID } = useChannels();
  const canReadCommercialization = hasSystemScope('read_commercialization');
  const canReadDashboard = hasSystemScope('read_dashboard');
  const canEditProviderPrices = hasSystemScope('write_commercialization');
  const [view, setView] = useState<CatalogView>(() => {
    if (urlSearch.view === 'channels' || urlSearch.view === 'models') return urlSearch.view;
    const stored = localStorage.getItem(VIEW_STORAGE_KEY);
    return stored === 'channels' || stored === 'models' ? stored : 'models';
  });
  const [search, setSearch] = useState('');
  const [status, setStatus] = useState('all');
  const [type, setType] = useState('all');
  const [commercializationAction, setCommercializationAction] = useState<CommercializationAction | null>(null);
  const [showManagement, setShowManagement] = useState(false);
  const [managementSearch, setManagementSearch] = useState('');
  const [sorting, setSorting] = useState<SortingState>([{ id: 'name', desc: false }]);
  const channelsQuery = useQueryChannels({ first: 10000, orderBy: { field: 'NAME', direction: 'ASC' } });
  const channels = useMemo(() => channelsQuery.data?.edges.map((edge) => edge.node) || [], [channelsQuery.data]);
  const probeQuery = useChannelProbeData(channels.map((channel) => channel.id));
  const pricesQuery = useChannelCatalogPrices(channels.length > 0);
  const supplyQuery = useUpstreamSupplyCatalog();
  const commercializationQuery = useCommercializationCatalog(canReadCommercialization);
  const probeMap = useMemo(
    () =>
      new Map<string, ChannelProbePoint[]>(((probeQuery.data || []) as ChannelProbeData[]).map((entry) => [entry.channelID, entry.points])),
    [probeQuery.data]
  );
  const configuredById = useMemo(() => new Map(models.map((model) => [model.modelID.toLowerCase(), model])), [models]);

  const updateSearch = (next: ModelsCatalogSearch, replace = false) => {
    void navigate({
      to: '/models',
      search: ((previous: ModelsCatalogSearch) => validateModelsSearch({ ...previous, ...next })) as never,
      replace,
    });
  };

  useEffect(() => {
    localStorage.setItem(VIEW_STORAGE_KEY, view);
  }, [view]);

  useEffect(() => {
    const nextView = urlSearch.view || (urlSearch.model ? 'models' : urlSearch.channel ? 'channels' : undefined);
    if (nextView && nextView !== view) setView(nextView);
  }, [urlSearch.channel, urlSearch.deployment, urlSearch.model, urlSearch.view, view]);

  const types = useMemo(() => [...new Set(channels.map((channel) => channel.type))].sort(), [channels]);
  const filteredChannels = useMemo(() => {
    const query = search.trim().toLowerCase();
    return channels.filter((channel) => {
      const developerMatches = channel.supportedModels.some((id) =>
        inferDeveloper(id, configuredById.get(id.toLowerCase()), providers).includes(query)
      );
      const searchMatches =
        !query ||
        channel.name.toLowerCase().includes(query) ||
        channel.type.toLowerCase().includes(query) ||
        channel.supportedModels.some((id) => id.toLowerCase().includes(query)) ||
        developerMatches;
      return searchMatches && (status === 'all' || channel.status === status) && (type === 'all' || channel.type === type);
    });
  }, [channels, configuredById, providers, search, status, type]);

  const aggregatedModels = useMemo(() => {
    const query = search.trim().toLowerCase();
    const channelById = new Map(channels.map((channel) => [channel.id, channel]));
    const routes = supplyQuery.data?.modelRoutes || [];
    const defaultBook = commercializationQuery.data?.priceBooks.find((book) => book.isDefault);
    const publishedPrices = new Map(
      (defaultBook?.versions.find((version) => version.status === 'published')?.items || []).map((item) => [item.publicModelID, item])
    );
    return models
      .map((model): AggregatedModel | null => {
        const modelRoutes = routes.filter((route) => route.publicModelID === model.id && route.status !== 'ARCHIVED');
        const visibleRoutes = modelRoutes.filter((route) => {
          const channel = channelById.get(route.channelID);
          if (!channel) return false;
          return (status === 'all' || channel.status === status) && (type === 'all' || channel.type === type);
        });
        const modelChannels = [...new Map(visibleRoutes.map((route) => [route.channelID, channelById.get(route.channelID)!])).values()];
        const searchMatches =
          !query ||
          model.modelID.toLowerCase().includes(query) ||
          model.name.toLowerCase().includes(query) ||
          model.developer.toLowerCase().includes(query) ||
          visibleRoutes.some(
            (route) => route.channelName.toLowerCase().includes(query) || route.upstreamModelID.toLowerCase().includes(query)
          );
        if (!searchMatches || ((status !== 'all' || type !== 'all') && modelChannels.length === 0)) return null;
        return {
          id: model.modelID,
          name: model.name,
          developer: model.developer,
          channels: modelChannels,
          routes: visibleRoutes,
          configuredModel: model,
          publishedRetailPrice: publishedPrices.get(model.id),
          retailCurrency: accountingCurrencyCode,
        };
      })
      .filter((model): model is AggregatedModel => model !== null)
      .sort((a, b) => a.name.localeCompare(b.name));
  }, [
    accountingCurrencyCode,
    channels,
    commercializationQuery.data?.priceBooks,
    models,
    search,
    status,
    supplyQuery.data?.modelRoutes,
    type,
  ]);
  const selectedModelID = urlSearch.model || null;
  const selectedChannelID = urlSearch.channel || null;
  const selectedModel = aggregatedModels.find((model) => model.id === selectedModelID) || null;
  const selectedChannel = channels.find((channel) => channel.id === selectedChannelID) || null;
  const upstreamDetailRequested = !!selectedChannel && !!(urlSearch.deployment || urlSearch.upstreamModel);
  const selectedDeployment = useMemo(() => {
    if (!selectedChannel || !supplyQuery.data) return null;
    const channelDeployments = supplyQuery.data.upstreamModelDeployments.filter(
      (deployment) => deployment.channelID === selectedChannel.id
    );
    if (urlSearch.deployment) {
      return channelDeployments.find((deployment) => deployment.id === urlSearch.deployment) || null;
    }
    if (!urlSearch.upstreamModel) return null;
    const matching = channelDeployments.filter((deployment) => deployment.upstreamModelID === urlSearch.upstreamModel);
    return (
      matching.find((deployment) => !deployment.variant && deployment.status === 'ENABLED') ||
      matching.find((deployment) => deployment.status === 'ENABLED') ||
      matching[0] ||
      null
    );
  }, [selectedChannel, supplyQuery.data, urlSearch.deployment, urlSearch.upstreamModel]);

  const editSelectedPurchasePrice = () => {
    if (!selectedChannel || !selectedDeployment) return;
    setPriceChannel(selectedChannel);
    setSelectedPriceModelID(selectedDeployment.upstreamModelID);
    setChannelDialog('price');
    updateSearch({ channel: undefined, upstreamModel: undefined, deployment: undefined }, true);
  };

  const enabledChannels = channels.filter((channel) => channel.status === 'enabled').length;
  const totalCatalogModels = models.length;
  const columns = useMemo(() => createColumns(t, modelPermissions.canWrite), [modelPermissions.canWrite, t]);

  return (
    <div className='flex min-h-0 flex-1 flex-col gap-4 overflow-auto pb-8'>
      <section className='bg-card rounded-xl border p-3 shadow-xs sm:p-4'>
        <div className='flex flex-col gap-3 xl:flex-row xl:items-center xl:justify-between'>
          <div className='bg-muted/40 inline-flex w-fit rounded-lg border p-1'>
            {(['channels', 'models'] as CatalogView[]).map((item) => (
              <Button
                key={item}
                size='sm'
                variant={view === item ? 'secondary' : 'ghost'}
                className={cn('h-8 px-3 shadow-none', view === item && 'bg-background shadow-xs')}
                data-testid={`models-catalog-view-${item}`}
                onClick={() => {
                  setView(item);
                  updateSearch({ view: item, model: undefined, channel: undefined, upstreamModel: undefined, deployment: undefined });
                }}
              >
                {item === 'channels' ? <IconServer className='size-4' /> : <IconDatabase className='size-4' />}
                {t(`models.catalog.views.${item}`)}
              </Button>
            ))}
          </div>
          <div className='flex flex-1 flex-col gap-2 sm:flex-row xl:max-w-3xl'>
            <div className='relative min-w-0 flex-1'>
              <IconSearch className='text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2' />
              <Input
                value={search}
                onChange={(event) => setSearch(event.target.value)}
                placeholder={t(view === 'channels' ? 'models.catalog.searchChannelsPlaceholder' : 'models.catalog.searchModelsPlaceholder')}
                className='pl-9'
              />
            </div>
            <Select value={status} onValueChange={setStatus}>
              <SelectTrigger className='w-full sm:w-36'>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value='all'>{t('models.catalog.allStatuses')}</SelectItem>
                <SelectItem value='enabled'>{t('models.catalog.statuses.enabled')}</SelectItem>
                <SelectItem value='disabled'>{t('models.catalog.statuses.disabled')}</SelectItem>
                <SelectItem value='archived'>{t('models.catalog.statuses.archived')}</SelectItem>
              </SelectContent>
            </Select>
            <Select value={type} onValueChange={setType}>
              <SelectTrigger className='w-full sm:w-44'>
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                <SelectItem value='all'>{t('models.catalog.allTypes')}</SelectItem>
                {types.map((value) => (
                  <SelectItem key={value} value={value}>
                    {value}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>
        <div className='text-muted-foreground mt-3 flex flex-wrap items-center gap-x-5 gap-y-2 border-t pt-3 text-xs'>
          <span className='inline-flex items-center gap-1.5'>
            <IconServer className='size-3.5' />
            {t('models.catalog.channelSummary', { enabled: enabledChannels, total: channels.length })}
          </span>
          <span className='inline-flex items-center gap-1.5'>
            <IconDatabase className='size-3.5' />
            {t('models.catalog.modelSummary', { count: totalCatalogModels })}
          </span>
          {pricesQuery.isError && (
            <span className='inline-flex items-center gap-1.5 text-amber-600 dark:text-amber-400'>
              <IconAlertTriangle className='size-3.5' />
              {t('models.catalog.priceUnavailable')}
            </span>
          )}
        </div>
      </section>

      {channelsQuery.isError && (
        <Alert variant='destructive'>
          <IconAlertTriangle />
          <AlertTitle>{t('models.catalog.loadError')}</AlertTitle>
          <AlertDescription>{t('models.catalog.loadErrorDescription')}</AlertDescription>
        </Alert>
      )}
      {(channelsQuery.isLoading || modelsLoading) && <CatalogSkeleton />}

      {!channelsQuery.isLoading &&
        !channelsQuery.isError &&
        view === 'channels' &&
        (filteredChannels.length ? (
          <div className='grid grid-cols-1 gap-3 lg:grid-cols-2 2xl:grid-cols-3'>
            {filteredChannels.map((channel) => {
              const points = probeMap.get(channel.id) || [];
              const health = getHealth(points);
              const visibleTags = (channel.tags ?? []).filter(
                (tag) => channel.settings?.managementAdapter !== 'new_api' || !isNewApiChannelTag(tag)
              );
              return (
                <Card
                  key={channel.id}
                  data-testid='channel-catalog-card'
                  className='group hover:border-foreground/20 relative cursor-pointer rounded-xl transition-[border-color,box-shadow,transform] hover:-translate-y-0.5 hover:shadow-md'
                >
                  <CardContent className='p-5'>
                    <div className='flex items-start justify-between gap-4'>
                      <div className='min-w-0'>
                        <div className='flex items-center gap-2'>
                          <span className={cn('size-2 rounded-full', statusTone(channel.status))} />
                          <h3 className='truncate text-[16px] font-semibold underline-offset-4 group-hover:underline'>
                            <Link
                              to='/models'
                              search={channelDetailSearch(channel.id)}
                              data-testid='channel-catalog-detail-link'
                              className='focus-visible:after:ring-ring after:absolute after:inset-0 after:rounded-xl focus-visible:outline-none focus-visible:after:ring-2'
                            >
                              {channel.name}
                            </Link>
                          </h3>
                          <ChannelManagementAdapterBadge managementAdapter={channel.settings?.managementAdapter} />
                        </div>
                        <p className='text-muted-foreground mt-1 truncate font-mono text-xs'>{channel.baseURL}</p>
                      </div>
                      <Badge variant='outline' className='shrink-0 font-mono text-[10px]'>
                        {channel.type}
                      </Badge>
                    </div>
                    <div className='bg-muted/15 mt-5 rounded-lg border px-3 py-3'>
                      <div className='mb-2 flex items-center justify-between text-xs'>
                        <span className='text-muted-foreground inline-flex items-center gap-1.5'>
                          <IconHeartbeat className='size-3.5' />
                          {t('models.catalog.lastHour')}
                        </span>
                        <HealthValue health={health} />
                      </div>
                      <HealthRhythm points={points} />
                    </div>
                    <div className='mt-4 flex items-end justify-between gap-3'>
                      <div className='min-w-0'>
                        <p className='text-muted-foreground text-xs'>
                          {t('models.catalog.supportedModels', { count: channel.supportedModels.length })}
                        </p>
                        <div className='mt-2 flex flex-wrap gap-1.5'>
                          {channel.supportedModels.slice(0, 3).map((id) => (
                            <Badge
                              key={id}
                              asChild
                              variant='secondary'
                              className='relative z-10 max-w-40 font-mono text-[10px] font-normal'
                            >
                              <Link
                                to='/models'
                                search={channelDetailSearch(channel.id, id)}
                                data-testid='upstream-model-link'
                                title={id}
                                className='truncate focus-visible:outline-none'
                              >
                                {id}
                              </Link>
                            </Badge>
                          ))}
                          {channel.supportedModels.length > 3 && (
                            <Badge variant='outline' className='text-[10px]'>
                              +{channel.supportedModels.length - 3}
                            </Badge>
                          )}
                        </div>
                      </div>
                      <span className='text-muted-foreground group-hover:text-foreground inline-flex shrink-0 items-center gap-1 text-xs font-medium transition-colors'>
                        {t('models.catalog.viewDetails')}
                        <IconChevronRight className='size-3.5' />
                      </span>
                    </div>
                    {visibleTags.length > 0 && (
                      <div className='mt-4 flex flex-wrap gap-1 border-t pt-3'>
                        {visibleTags.slice(0, 5).map((tag) => (
                          <span key={tag} className='text-muted-foreground text-[10px]'>
                            #{tag}
                          </span>
                        ))}
                      </div>
                    )}
                  </CardContent>
                </Card>
              );
            })}
          </div>
        ) : (
          <EmptyCatalog title={t('models.catalog.noChannels')} description={t('models.catalog.noChannelsDescription')} />
        ))}

      {!channelsQuery.isLoading &&
        !channelsQuery.isError &&
        view === 'models' &&
        (aggregatedModels.length ? (
          <div className='grid grid-cols-1 gap-3 sm:grid-cols-2 xl:grid-cols-3 2xl:grid-cols-4'>
            {aggregatedModels.map((model) => {
              const enabled = model.routes.filter(
                (route) =>
                  route.status === 'ENABLED' &&
                  model.channels.some((channel) => channel.id === route.channelID && channel.status === 'enabled')
              ).length;
              const health = aggregatePublicModelHealth(model.routes, model.channels, probeMap);
              return (
                <Link
                  key={model.id}
                  to='/models'
                  search={modelDetailSearch(model.id)}
                  className='group focus-visible:ring-ring block rounded-xl focus-visible:ring-2 focus-visible:outline-none'
                >
                  <Card className='hover:border-foreground/20 cursor-pointer rounded-xl transition-[border-color,box-shadow,transform] group-hover:-translate-y-0.5 group-hover:shadow-md'>
                    <CardContent className='flex min-h-56 flex-col p-4 sm:min-h-60 sm:p-5'>
                      <div className='flex items-start justify-between gap-3'>
                        <div className='bg-muted/30 flex size-9 shrink-0 items-center justify-center rounded-lg border font-mono text-sm font-semibold'>
                          {model.name.slice(0, 1).toUpperCase()}
                        </div>
                        {model.configuredModel && (
                          <Badge variant='outline' className='gap-1 text-[10px]'>
                            <IconCheck className='size-3 text-emerald-500' />
                            {t('models.catalog.configured')}
                          </Badge>
                        )}
                      </div>
                      <div className='mt-3 min-w-0 sm:mt-4'>
                        <h3 className='truncate text-[15px] font-semibold underline-offset-4 group-hover:underline'>{model.name}</h3>
                        <p className='text-muted-foreground mt-1 truncate font-mono text-xs underline-offset-4 group-hover:underline'>
                          {model.id}
                        </p>
                      </div>
                      <div className='mt-auto pt-4 sm:pt-5'>
                        <Badge variant='secondary' className='font-normal'>
                          {developerLabel(model.developer, providers, t)}
                        </Badge>
                        <div className='mt-4 grid grid-cols-2 gap-2 border-t pt-3 text-center'>
                          <div>
                            <div className='text-sm font-semibold'>
                              <HealthValue health={health} />
                            </div>
                            <div className='text-muted-foreground text-[10px]'>{t('models.catalog.publicModelHealth')}</div>
                          </div>
                          <div>
                            <div className='font-mono text-sm font-semibold text-emerald-600 dark:text-emerald-400'>{enabled}</div>
                            <div className='text-muted-foreground text-[10px]'>{t('models.catalog.enabled')}</div>
                          </div>
                        </div>
                        <div className='mt-3 border-t pt-2.5'>
                          <div className='text-muted-foreground mb-1 text-[10px] font-medium'>{t('models.catalog.retailPrice')}</div>
                          <RetailPriceSummary item={model.publishedRetailPrice} currency={model.retailCurrency} />
                        </div>
                      </div>
                    </CardContent>
                  </Card>
                </Link>
              );
            })}
          </div>
        ) : (
          <EmptyCatalog title={t('models.catalog.noModels')} description={t('models.catalog.noModelsDescription')} />
        ))}

      {view === 'models' && (
        <section className='bg-card mt-2 rounded-xl border'>
          <button
            type='button'
            data-testid='models-enterprise-toggle'
            aria-expanded={showManagement}
            className='hover:bg-muted/30 focus-visible:ring-ring flex w-full items-center justify-between gap-4 p-4 text-left focus-visible:ring-2 focus-visible:outline-none focus-visible:ring-inset'
            onClick={() => setShowManagement((value) => !value)}
          >
            <span>
              <span className='flex items-center gap-2 text-sm font-medium'>
                <IconAdjustmentsHorizontal className='size-4' />
                {t('models.catalog.manageConfigurations')}
              </span>
              <span className='text-muted-foreground mt-1 block text-xs'>
                {t('models.catalog.manageConfigurationsDescription', { count: modelsTotalCount || models.length })}
              </span>
            </span>
            <IconChevronRight className={cn('size-4 shrink-0 transition-transform', showManagement && 'rotate-90')} />
          </button>
          {showManagement && (
            <div className='h-[620px] border-t p-3' data-testid='models-enterprise-panel'>
              <ModelsTable
                data={models}
                columns={columns}
                loading={modelsLoading}
                totalCount={modelsTotalCount}
                nameFilter={managementSearch}
                sorting={sorting}
                onSortingChange={setSorting}
                onNameFilterChange={setManagementSearch}
                canWrite={modelPermissions.canWrite}
              />
            </div>
          )}
        </section>
      )}

      {view === 'models' && canReadCommercialization && (
        <CommercializationPanel models={models} action={commercializationAction} onActionHandled={() => setCommercializationAction(null)} />
      )}

      <ChannelDetailDialog
        channel={selectedChannel}
        open={!!selectedChannel && !upstreamDetailRequested}
        onOpenChange={(open) => {
          if (!open) updateSearch({ channel: undefined, upstreamModel: undefined, deployment: undefined });
        }}
        points={selectedChannel ? probeMap.get(selectedChannel.id) || [] : []}
        highlightedUpstreamModel={urlSearch.upstreamModel}
        deployments={supplyQuery.data?.upstreamModelDeployments || []}
        deploymentsLoading={supplyQuery.isLoading}
      />
      <UpstreamModelDetailDialog
        channel={selectedChannel}
        deployment={selectedDeployment}
        loading={upstreamDetailRequested && (channelsQuery.isLoading || supplyQuery.isLoading)}
        loadError={supplyQuery.isError}
        open={upstreamDetailRequested}
        onOpenChange={(open) => {
          if (!open) updateSearch({ upstreamModel: undefined, deployment: undefined });
        }}
        routes={supplyQuery.data?.modelRoutes || []}
        publicModels={models}
        canReadHealth={canReadDashboard}
        canEditPrice={canEditProviderPrices}
        accountingCurrencyCode={accountingCurrencyCode}
        onEditPrice={editSelectedPurchasePrice}
      />
      <ModelDetailDialog
        model={selectedModel}
        open={!!selectedModel}
        onOpenChange={(open) => {
          if (!open) updateSearch({ model: undefined });
        }}
        probeMap={probeMap}
        providers={providers}
        accountingCurrencyCode={accountingCurrencyCode}
        onCommercializationAction={(action) => {
          updateSearch({ model: undefined }, true);
          setCommercializationAction(action);
        }}
      />
    </div>
  );
}
