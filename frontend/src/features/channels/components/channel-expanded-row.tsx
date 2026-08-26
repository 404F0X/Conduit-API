import { memo } from 'react';
import { format } from 'date-fns';
import { useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';
import { CHANNEL_CONFIGS } from '../data/config_channels';
import { Channel } from '../data/schema';
import { isNewApiChannelTag } from '../utils/channel-management-adapter';
import { ChannelManagementAdapterBadge } from './channel-management-adapter-badge';

interface ChannelExpandedRowProps {
  channel: Channel;
  getApiFormatLabel: (apiFormat?: string) => string;
}

function quotaDataRecord(value: unknown): Record<string, unknown> | null {
  if (value && typeof value === 'object' && !Array.isArray(value)) return value as Record<string, unknown>;
  if (typeof value !== 'string') return null;
  try {
    const parsed: unknown = JSON.parse(value);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? (parsed as Record<string, unknown>) : null;
  } catch {
    return null;
  }
}

function displayQuotaValue(value: unknown): string | null {
  return typeof value === 'string' || typeof value === 'number' ? String(value) : null;
}

export const ChannelExpandedRow = memo(({ channel, getApiFormatLabel }: ChannelExpandedRowProps) => {
  const { t } = useTranslation();
  const config = CHANNEL_CONFIGS[channel.type];
  const automaticData = quotaDataRecord(channel.providerQuotaStatus?.quotaData);
  const hasNewApiSnapshot = channel.providerQuotaStatus?.probeAdapter === 'new_api' && automaticData != null;
  const quotaCurrency = hasNewApiSnapshot ? (displayQuotaValue(automaticData.currency) ?? channel.quotaCurrency) : channel.quotaCurrency;
  const quotaUsed = hasNewApiSnapshot ? displayQuotaValue(automaticData.used) : channel.actualQuotaUsed;
  const quotaRemaining = hasNewApiSnapshot ? displayQuotaValue(automaticData.remaining) : channel.quotaRemaining;
  const quotaTotal = hasNewApiSnapshot ? displayQuotaValue(automaticData.total) : null;
  const isNewApi = channel.settings?.managementAdapter === 'new_api';
  const visibleTags = (channel.tags ?? []).filter((tag) => !isNewApi || !isNewApiChannelTag(tag));

  return (
    <div className='bg-muted/30 sticky left-0 box-border w-[100cqw] min-w-0 p-3 sm:p-4'>
      <section className='bg-card w-full min-w-0 rounded-xl p-4 shadow-sm ring-1 ring-black/5 sm:p-6 dark:ring-white/10'>
        <div className='space-y-6'>
          <div className='grid grid-cols-1 gap-6 lg:grid-cols-2'>
            <div className='space-y-3'>
              <h4 className='text-sm font-semibold'>{t('channels.expandedRow.basic')}</h4>
              <div className='space-y-2 text-sm'>
                <div className='flex items-start gap-2'>
                  <span className='text-muted-foreground shrink-0'>{t('channels.columns.baseURL')}:</span>
                  <span className='min-w-0 flex-1 text-right font-mono text-xs break-all'>{channel.baseURL}</span>
                </div>
                <div className='flex items-start gap-2'>
                  <span className='text-muted-foreground shrink-0'>{t('channels.expandedRow.websiteURL')}:</span>
                  {channel.websiteURL ? (
                    <a
                      href={channel.websiteURL}
                      target='_blank'
                      rel='noopener noreferrer'
                      className='min-w-0 flex-1 truncate text-right text-xs underline-offset-4 hover:underline'
                    >
                      {channel.websiteURL}
                    </a>
                  ) : (
                    <span className='text-muted-foreground flex-1 text-right'>{t('channels.expandedRow.notRecorded')}</span>
                  )}
                </div>
                <div className='flex items-center justify-between'>
                  <span className='text-muted-foreground'>{t('channels.columns.type')}:</span>
                  <Badge variant='outline' className={config?.color}>
                    {t(`channels.types.${channel.type}`)}
                  </Badge>
                </div>
                <div className='flex items-center justify-between'>
                  <span className='text-muted-foreground'>{t('channels.expandedRow.apiFormat')}:</span>
                  <span className='font-mono text-xs'>{getApiFormatLabel(config?.apiFormat)}</span>
                </div>
                <div className='flex justify-between'>
                  <span className='text-muted-foreground'>{t('common.columns.createdAt')}:</span>
                  <span className='tabular-nums'>{format(channel.createdAt, 'yyyy-MM-dd HH:mm')}</span>
                </div>
                <div className='flex justify-between'>
                  <span className='text-muted-foreground'>{t('common.columns.updatedAt')}:</span>
                  <span className='tabular-nums'>{format(channel.updatedAt, 'yyyy-MM-dd HH:mm')}</span>
                </div>
              </div>
            </div>

            <div className='space-y-3'>
              <div className='space-y-3'>
                <h4 className='text-sm font-semibold'>{t('channels.expandedRow.additional')}</h4>
                <div className='space-y-2 text-sm'>
                  <div className='flex items-center justify-between'>
                    <span className='text-muted-foreground'>{t('channels.columns.orderingWeight')}:</span>
                    <span className='font-mono text-xs tabular-nums'>{channel.orderingWeight ?? 0}</span>
                  </div>
                  <div className='flex justify-between'>
                    <span className='text-muted-foreground'>{t('channels.expandedRow.remark')}:</span>
                    <span className='max-w-[200px] truncate text-right' title={channel.remark || undefined}>
                      {channel.remark || '-'}
                    </span>
                  </div>
                  <div className='flex items-start justify-between'>
                    <span className='text-muted-foreground shrink-0'>{t('channels.expandedRow.tags')}:</span>
                    <div className='flex max-w-[200px] flex-wrap justify-end gap-1'>
                      <ChannelManagementAdapterBadge managementAdapter={channel.settings?.managementAdapter} />
                      {visibleTags.length > 0 ? (
                        visibleTags.map((tag) => (
                          <Badge key={tag} variant='outline' className='text-xs'>
                            {tag}
                          </Badge>
                        ))
                      ) : !isNewApi ? (
                        <span>-</span>
                      ) : null}
                    </div>
                  </div>
                </div>
              </div>
            </div>
          </div>

          <section className='space-y-3 border-t pt-4'>
            <div className='flex flex-wrap items-start justify-between gap-2'>
              <div>
                <h4 className='text-sm font-semibold'>{t('channels.expandedRow.quotaSnapshot')}</h4>
                <p className='text-muted-foreground mt-1 text-xs'>
                  {t(hasNewApiSnapshot ? 'channels.expandedRow.newApiQuotaHint' : 'channels.expandedRow.quotaSnapshotHint')}
                </p>
              </div>
              {hasNewApiSnapshot ? (
                <Badge
                  variant='outline'
                  className={
                    channel.providerQuotaStatus?.probeVerifiedAt
                      ? 'border-emerald-500/40 text-emerald-700 dark:text-emerald-300'
                      : 'border-amber-500/40 text-amber-700 dark:text-amber-300'
                  }
                >
                  {t(
                    channel.providerQuotaStatus?.probeVerifiedAt
                      ? 'channels.expandedRow.newApiVerifiedSource'
                      : 'channels.expandedRow.newApiProbeSource'
                  )}
                </Badge>
              ) : null}
            </div>
            <div className={`grid grid-cols-1 gap-2 ${quotaTotal == null ? 'sm:grid-cols-2' : 'sm:grid-cols-3'}`}>
              {quotaTotal != null ? (
                <div className='bg-background/40 flex min-w-0 items-center justify-between gap-4 rounded-md border px-3 py-2 text-sm'>
                  <span className='text-muted-foreground shrink-0'>{t('channels.expandedRow.quotaTotal')}</span>
                  <span className='min-w-0 text-right font-mono break-all tabular-nums'>{`${quotaCurrency} ${quotaTotal}`}</span>
                </div>
              ) : null}
              <div className='bg-background/40 flex min-w-0 items-center justify-between gap-4 rounded-md border px-3 py-2 text-sm'>
                <span className='text-muted-foreground shrink-0'>{t('channels.expandedRow.actualQuotaUsed')}</span>
                <span className='min-w-0 text-right font-mono break-all tabular-nums'>
                  {quotaUsed == null ? t('channels.expandedRow.notRecorded') : `${quotaCurrency} ${quotaUsed}`}
                </span>
              </div>
              <div className='bg-background/40 flex min-w-0 items-center justify-between gap-4 rounded-md border px-3 py-2 text-sm'>
                <span className='text-muted-foreground shrink-0'>{t('channels.expandedRow.quotaRemaining')}</span>
                <span className='min-w-0 text-right font-mono break-all tabular-nums'>
                  {quotaRemaining == null ? t('channels.expandedRow.notRecorded') : `${quotaCurrency} ${quotaRemaining}`}
                </span>
              </div>
            </div>
          </section>

          {channel.supportedModels && channel.supportedModels.length > 0 && (
            <div className='space-y-3'>
              <h4 className='text-sm font-semibold'>{t('channels.expandedRow.supportedModels')}</h4>
              <div className='flex flex-wrap gap-2'>
                {channel.supportedModels.slice(0, 5).map((model) => (
                  <Badge key={model} variant='secondary' className='font-mono text-xs'>
                    {model}
                  </Badge>
                ))}
                {channel.supportedModels.length > 5 && (
                  <span className='text-muted-foreground flex items-center text-xs italic'>
                    {t('channels.expandedRow.moreModels', { count: channel.supportedModels.length - 5 })}
                  </span>
                )}
              </div>
            </div>
          )}
        </div>
      </section>
    </div>
  );
});

ChannelExpandedRow.displayName = 'ChannelExpandedRow';
