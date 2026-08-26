import { useEffect, useMemo, useState } from 'react';
import { IconAlertTriangle, IconCheck, IconKey, IconLoader2, IconRefresh, IconServerBolt, IconShieldLock } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { useConfirmChannelQuotaProbe, useProbeChannelQuota } from '../data/channels';
import { Channel, ChannelQuotaProbeResult } from '../data/schema';

interface ChannelsQuotaProbeDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  channel: Channel;
}

type QuotaValues = {
  currency?: string;
  total?: string;
  used?: string;
  remaining?: string;
  unlimited?: boolean;
  unlimitedKeyCount?: number;
  keyCount?: number;
  balanceSource?: 'key' | 'account';
};

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

function stringValue(value: unknown): string | undefined {
  return typeof value === 'string' || typeof value === 'number' ? String(value) : undefined;
}

function numberValue(value: unknown): number | undefined {
  const number = typeof value === 'number' ? value : typeof value === 'string' ? Number(value) : Number.NaN;
  return Number.isFinite(number) && number >= 0 ? number : undefined;
}

function persistedQuota(channel: Channel): QuotaValues | null {
  const data = quotaDataRecord(channel.providerQuotaStatus?.quotaData);
  if (!data) return null;

  return {
    currency: stringValue(data.currency),
    total: stringValue(data.total),
    used: stringValue(data.used),
    remaining: stringValue(data.remaining),
    unlimited: data.unlimited === true,
    unlimitedKeyCount: numberValue(data.unlimited_key_count ?? data.unlimitedKeyCount),
    keyCount: numberValue(data.key_count ?? data.keyCount),
    balanceSource:
      data.balance_source === 'account' || data.balanceSource === 'account'
        ? 'account'
        : data.balance_source === 'key' || data.balanceSource === 'key'
          ? 'key'
          : undefined,
  };
}

function resultQuota(result: ChannelQuotaProbeResult | null): QuotaValues | null {
  if (!result?.success) return null;
  return {
    currency: result.currency ?? undefined,
    total: result.total ?? undefined,
    used: result.used ?? undefined,
    remaining: result.remaining ?? undefined,
    unlimited: result.unlimited,
    unlimitedKeyCount: result.unlimitedKeyCount,
    keyCount: result.keyCount,
    balanceSource: result.balanceSource ?? undefined,
  };
}

export function ChannelsQuotaProbeDialog({ open, onOpenChange, channel }: ChannelsQuotaProbeDialogProps) {
  const { t, i18n } = useTranslation();
  const probe = useProbeChannelQuota();
  const confirm = useConfirmChannelQuotaProbe();
  const [probeResult, setProbeResult] = useState<ChannelQuotaProbeResult | null>(null);
  const [confirmedResult, setConfirmedResult] = useState<ChannelQuotaProbeResult | null>(null);
  const [newApiPAT, setNewApiPAT] = useState('');
  const [newApiUserID, setNewApiUserID] = useState('');
  const [patRequired, setPatRequired] = useState(false);

  useEffect(() => {
    if (open) {
      setProbeResult(null);
      setConfirmedResult(null);
      setNewApiPAT('');
      setNewApiUserID('');
      setPatRequired(false);
    } else {
      setNewApiPAT('');
      setNewApiUserID('');
    }
  }, [open, channel.id]);

  const persisted = useMemo(() => persistedQuota(channel), [channel]);
  const hasFailedAttempt = confirmedResult?.success === false || probeResult?.success === false;
  const values = hasFailedAttempt ? null : (resultQuota(confirmedResult) ?? resultQuota(probeResult) ?? persisted);
  const persistedVerifiedAt = channel.providerQuotaStatus?.probeVerifiedAt ?? null;
  const activeResult = confirmedResult ?? probeResult;
  const verifiedAt = activeResult ? activeResult.verifiedAt : persistedVerifiedAt;
  const isVerified = !hasFailedAttempt && Boolean(activeResult ? activeResult.success && activeResult.verified : persistedVerifiedAt);
  const accountBalanceReady = probeResult?.success === true && probeResult.balanceSource === 'account';
  const hasUnlimitedKeys = Boolean(probeResult?.unlimitedKeyCount);
  const canConfirm =
    probeResult?.success === true &&
    (!hasUnlimitedKeys || accountBalanceReady) &&
    !patRequired &&
    confirmedResult?.verified !== true &&
    !confirm.isPending;
  const isBusy = probe.isPending || confirm.isPending;

  const handleProbe = async () => {
    setConfirmedResult(null);
    try {
      const result = await probe.mutateAsync({ channelID: channel.id });
      setProbeResult(result);
      setPatRequired(result.requiresPat);
    } catch {
      // Transport and GraphQL errors are reported by the mutation hook.
    }
  };

  const handlePATProbe = async () => {
    const pat = newApiPAT.trim();
    const userID = newApiUserID.trim();
    if (!pat || !/^[1-9]\d*$/.test(userID)) return;

    setConfirmedResult(null);
    try {
      const result = await probe.mutateAsync({
        channelID: channel.id,
        newApiPAT: pat,
        newApiUserID: userID,
      });
      setProbeResult(result);
      if (result.success) {
        setNewApiPAT('');
        setNewApiUserID('');
        setPatRequired(result.balanceSource !== 'account' && result.requiresPat);
      } else if (result.requiresPat) {
        setPatRequired(true);
      }
    } catch {
      // Transport and GraphQL errors are reported without rendering the credential.
    }
  };

  const handleConfirm = async () => {
    try {
      const result = await confirm.mutateAsync(channel.id);
      setConfirmedResult(result);
      if (!result.success) setProbeResult(result);
    } catch {
      // Transport and GraphQL errors are reported by the mutation hook.
    }
  };

  const formatVerifiedAt = (value: string | null | undefined) => {
    if (!value) return null;
    const date = new Date(value);
    return Number.isNaN(date.getTime())
      ? value
      : new Intl.DateTimeFormat(i18n.language, { dateStyle: 'medium', timeStyle: 'medium' }).format(date);
  };

  const failure =
    confirmedResult?.success === false ? confirmedResult.message : probeResult?.success === false ? probeResult.message : null;

  const metricValue = (key: 'total' | 'used' | 'remaining') => {
    if (values?.unlimited && values.balanceSource !== 'account' && key !== 'used') {
      return t('channels.quotaProbe.metrics.unlimited');
    }
    return `${values?.currency ?? channel.quotaCurrency} ${values?.[key] ?? '—'}`;
  };

  const handleOpenChange = (nextOpen: boolean) => {
    if (!nextOpen) setNewApiPAT('');
    onOpenChange(nextOpen);
  };

  return (
    <Dialog open={open} onOpenChange={handleOpenChange}>
      <DialogContent className='max-h-[90vh] overflow-y-auto sm:max-w-xl'>
        <DialogHeader className='text-left'>
          <div className='flex items-center gap-2'>
            <div className='bg-muted flex h-8 w-8 items-center justify-center rounded-md border'>
              <IconServerBolt className='h-4 w-4' />
            </div>
            <DialogTitle>{t('channels.quotaProbe.title')}</DialogTitle>
          </div>
          <DialogDescription>{t('channels.quotaProbe.description', { name: channel.name })}</DialogDescription>
        </DialogHeader>

        <div className='space-y-4'>
          <div className='bg-muted/40 text-muted-foreground rounded-md border px-3 py-2.5 text-sm leading-relaxed'>
            {t('channels.quotaProbe.compatibilityHint')}
          </div>

          {failure ? (
            <div
              role='alert'
              className='border-destructive/40 bg-destructive/5 text-destructive flex items-start gap-2 rounded-md border px-3 py-2.5 text-sm'
            >
              <IconAlertTriangle className='mt-0.5 h-4 w-4 shrink-0' />
              <span>{failure}</span>
            </div>
          ) : null}

          {values ? (
            <section
              role='status'
              aria-live='polite'
              aria-label={t('channels.quotaProbe.metrics.label')}
              className='overflow-hidden rounded-md border'
            >
              <div className='bg-muted/30 flex flex-wrap items-center gap-2 border-b px-3 py-2 text-xs'>
                <Badge
                  variant='outline'
                  className={
                    values.balanceSource === 'account'
                      ? 'border-emerald-500/40 text-emerald-700 dark:text-emerald-300'
                      : 'text-muted-foreground'
                  }
                >
                  {t(values.balanceSource === 'account' ? 'channels.quotaProbe.source.account' : 'channels.quotaProbe.source.key')}
                </Badge>
                <span className='text-muted-foreground'>
                  {t(values.balanceSource === 'account' ? 'channels.quotaProbe.source.accountHint' : 'channels.quotaProbe.source.keyHint')}
                </span>
              </div>
              <div className='grid grid-cols-1 divide-y sm:grid-cols-3 sm:divide-x sm:divide-y-0'>
                {(['total', 'used', 'remaining'] as const).map((key) => (
                  <div key={key} className='min-w-0 px-3 py-3'>
                    <div className='text-muted-foreground text-xs'>{t(`channels.quotaProbe.metrics.${key}`)}</div>
                    <div
                      className={
                        key === 'remaining'
                          ? 'mt-1 font-mono text-base font-semibold break-all text-emerald-600 tabular-nums dark:text-emerald-400'
                          : 'mt-1 font-mono text-base font-semibold break-all tabular-nums'
                      }
                    >
                      {metricValue(key)}
                    </div>
                  </div>
                ))}
              </div>
              <div className='bg-muted/30 flex items-center justify-between border-t px-3 py-2 text-xs'>
                <span className='text-muted-foreground'>{t('channels.quotaProbe.keyCount')}</span>
                <span className='font-mono font-medium tabular-nums'>
                  {values.keyCount ?? '—'}
                  {values.unlimitedKeyCount ? ` · ${t('channels.quotaProbe.unlimitedKeyCount', { count: values.unlimitedKeyCount })}` : ''}
                </span>
              </div>
            </section>
          ) : null}

          {accountBalanceReady ? (
            <div className='flex items-start gap-2 rounded-md border border-emerald-500/40 bg-emerald-500/5 px-3 py-2.5 text-sm text-emerald-700 dark:text-emerald-300'>
              <IconCheck className='mt-0.5 h-4 w-4 shrink-0' />
              <p className='leading-relaxed'>{t('channels.quotaProbe.pat.accountRetrieved')}</p>
            </div>
          ) : null}

          {isVerified ? (
            <div className='flex items-start gap-2 rounded-md border border-emerald-500/40 bg-emerald-500/5 px-3 py-2.5 text-sm text-emerald-700 dark:text-emerald-300'>
              <IconCheck className='mt-0.5 h-4 w-4 shrink-0' />
              <div className='min-w-0'>
                <div className='flex flex-wrap items-center gap-2 font-medium'>
                  {t('channels.quotaProbe.verified.title')}
                  <Badge variant='outline' className='border-emerald-500/40 text-emerald-700 dark:text-emerald-300'>
                    NEW API
                  </Badge>
                </div>
                {verifiedAt ? (
                  <p className='mt-1 text-xs opacity-80'>{t('channels.quotaProbe.verified.at', { time: formatVerifiedAt(verifiedAt) })}</p>
                ) : null}
              </div>
            </div>
          ) : null}

          {patRequired ? (
            <section className='rounded-md border border-amber-500/40 bg-amber-500/5 px-3 py-3 text-sm text-amber-950 dark:text-amber-100'>
              <div className='flex items-start gap-2'>
                <IconAlertTriangle className='mt-0.5 h-4 w-4 shrink-0 text-amber-600 dark:text-amber-400' />
                <div className='min-w-0'>
                  <h3 className='font-medium'>{t('channels.quotaProbe.pat.title')}</h3>
                  <p className='mt-1 text-xs leading-relaxed opacity-90'>{t('channels.quotaProbe.pat.description')}</p>
                </div>
              </div>

              <div className='bg-background/70 mt-3 space-y-2 rounded-md border border-amber-500/25 p-3'>
                <Label htmlFor={`new-api-pat-${channel.id}`} className='flex items-center gap-1.5 text-xs font-medium'>
                  <IconKey className='h-3.5 w-3.5' />
                  {t('channels.quotaProbe.pat.label')}
                </Label>
                <Input
                  id={`new-api-pat-${channel.id}`}
                  type='password'
                  autoComplete='new-password'
                  spellCheck={false}
                  value={newApiPAT}
                  onChange={(event) => setNewApiPAT(event.target.value)}
                  placeholder={t('channels.quotaProbe.pat.placeholder')}
                  disabled={isBusy}
                />
                <Label htmlFor={`new-api-user-id-${channel.id}`} className='text-xs font-medium'>
                  {t('channels.quotaProbe.pat.userIDLabel')}
                </Label>
                <Input
                  id={`new-api-user-id-${channel.id}`}
                  type='text'
                  inputMode='numeric'
                  autoComplete='off'
                  spellCheck={false}
                  value={newApiUserID}
                  onChange={(event) => setNewApiUserID(event.target.value.replace(/\D/g, ''))}
                  placeholder={t('channels.quotaProbe.pat.userIDPlaceholder')}
                  disabled={isBusy}
                />
                <p className='text-muted-foreground flex items-start gap-1.5 text-xs leading-relaxed'>
                  <IconShieldLock className='mt-0.5 h-3.5 w-3.5 shrink-0' />
                  {t('channels.quotaProbe.pat.securityHint')}
                </p>

                <details className='group border-t pt-2 text-xs'>
                  <summary className='focus-visible:ring-ring cursor-pointer font-medium select-none focus-visible:ring-2 focus-visible:outline-none'>
                    {t('channels.quotaProbe.pat.tutorial.title')}
                  </summary>
                  <ol className='text-muted-foreground mt-2 list-decimal space-y-1.5 pl-5 leading-relaxed'>
                    <li>{t('channels.quotaProbe.pat.tutorial.step1')}</li>
                    <li>{t('channels.quotaProbe.pat.tutorial.step2')}</li>
                    <li>{t('channels.quotaProbe.pat.tutorial.step3')}</li>
                  </ol>
                  <p className='text-muted-foreground mt-2 border-t pt-2 leading-relaxed'>{t('channels.quotaProbe.pat.tutorial.note')}</p>
                </details>

                <Button
                  type='button'
                  className='disabled:bg-muted disabled:text-muted-foreground w-full disabled:border sm:w-auto'
                  onClick={handlePATProbe}
                  disabled={!newApiPAT.trim() || !/^[1-9]\d*$/.test(newApiUserID.trim()) || isBusy}
                >
                  {probe.isPending ? <IconLoader2 className='mr-2 h-4 w-4 animate-spin' /> : <IconShieldLock className='mr-2 h-4 w-4' />}
                  {probe.isPending ? t('channels.quotaProbe.pat.actions.probing') : t('channels.quotaProbe.pat.actions.probe')}
                </Button>
              </div>
            </section>
          ) : probeResult?.success && !confirmedResult?.verified ? (
            <div className='rounded-md border border-amber-500/40 bg-amber-500/5 px-3 py-3 text-sm text-amber-900 dark:text-amber-200'>
              <div className='flex items-start gap-2 font-medium'>
                <IconAlertTriangle className='mt-0.5 h-4 w-4 shrink-0' />
                <span>{t('channels.quotaProbe.checkpoint.title')}</span>
              </div>
              <p className='mt-1.5 pl-6 text-xs leading-relaxed opacity-90'>{t('channels.quotaProbe.checkpoint.description')}</p>
            </div>
          ) : null}
        </div>

        <DialogFooter className='gap-2 sm:justify-between'>
          <Button type='button' variant='outline' onClick={handleProbe} disabled={isBusy}>
            {probe.isPending ? <IconLoader2 className='mr-2 h-4 w-4 animate-spin' /> : <IconRefresh className='mr-2 h-4 w-4' />}
            {probe.isPending ? t('channels.quotaProbe.actions.probing') : t('channels.quotaProbe.actions.probe')}
          </Button>
          <Button
            type='button'
            className='disabled:bg-muted disabled:text-muted-foreground disabled:border'
            onClick={handleConfirm}
            disabled={!canConfirm || isBusy}
          >
            {confirm.isPending ? <IconLoader2 className='mr-2 h-4 w-4 animate-spin' /> : <IconCheck className='mr-2 h-4 w-4' />}
            {confirm.isPending ? t('channels.quotaProbe.actions.rechecking') : t('channels.quotaProbe.actions.confirm')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
