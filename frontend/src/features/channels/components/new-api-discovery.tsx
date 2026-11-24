import { useEffect, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Link } from '@tanstack/react-router';
import { CheckCircle2, Circle, RefreshCw, ExternalLink, AlertCircle, Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { usePermissions } from '@/hooks/usePermissions';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { useCreateChannel } from '../data/channels';
import {
  canonicalNewApiURL,
  getDiscovery,
  initializeDiscovery,
  recalledDiscovery,
  rememberDiscovery,
  retryDiscovery,
  retryableStages,
  type DiscoveryOperation,
} from '../data/discovery';
import type { Channel } from '../data/schema';

export function DiscoveryPanel({
  operationID,
  initial,
  pending = false,
}: {
  operationID: string;
  initial?: DiscoveryOperation;
  pending?: boolean;
}) {
  const { t } = useTranslation();
  const client = useQueryClient();
  const { hasSystemScope } = usePermissions();
  const [busy, setBusy] = useState(false);
  const [pat, setPat] = useState('');
  const [userID, setUserID] = useState('');
  const query = useQuery({
    queryKey: ['channelDiscovery', operationID],
    queryFn: () => getDiscovery(operationID),
    initialData: initial,
    enabled: hasSystemScope('read_channels'),
    refetchInterval: (q) => (q.state.data?.state === 'RUNNING' ? 2000 : false),
  });
  const operation = query.data;
  const retry = async () => {
    if (!operation) return;
    setBusy(true);
    try {
      const result = await retryDiscovery({
        operationID,
        expectedAttempt: operation.attempt,
        stages: retryableStages(operation),
        credentials: pat.trim() ? { pat: pat.trim(), userID: userID.trim() || undefined } : undefined,
      });
      client.setQueryData(['channelDiscovery', operationID], result);
      setPat('');
      setUserID('');
      await client.invalidateQueries({ queryKey: ['channels'] });
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t('eventPackages.error'));
      if (hasSystemScope('read_channels')) await query.refetch();
    } finally {
      setBusy(false);
    }
  };
  const start = async () => {
    if (!operation) return;
    setBusy(true);
    try {
      const result = await initializeDiscovery({ channelID: operation.channelID, operationID });
      client.setQueryData(['channelDiscovery', operationID], result);
    } catch (e) {
      toast.error(e instanceof Error ? e.message : t('eventPackages.error'));
      if (hasSystemScope('read_channels')) await query.refetch();
    } finally {
      setBusy(false);
    }
  };
  if (pending && !operation)
    return (
      <div role='status' className='grid gap-3 sm:grid-cols-2'>
        {['models', 'prices', 'validation'].map((stage) => (
          <section key={stage} className='bg-muted/20 rounded-xl border p-4'>
            <h3 className='text-sm font-medium'>{t(`eventPackages.${stage}`)}</h3>
            <p className='text-muted-foreground mt-3 text-xs'>{t('eventPackages.awaitingReceipt')}</p>
          </section>
        ))}
      </div>
    );
  if (query.isLoading)
    return (
      <p role='status' className='p-6'>
        {t('eventPackages.loading')}
      </p>
    );
  if (query.isError || !operation)
    return (
      <div role='alert' className='space-y-3 rounded-xl border p-5'>
        <p>{query.error?.message ?? t('eventPackages.operationMissing')}</p>
        <Button variant='outline' disabled={!hasSystemScope('read_channels')} onClick={() => void query.refetch()}>
          {t('eventPackages.checkResult')}
        </Button>
        <p className='text-muted-foreground text-xs break-all'>{operationID}</p>
      </div>
    );
  const steps = [
    { label: 'saved', status: 'SUCCEEDED', count: null },
    { label: 'models', status: operation.models.status, count: operation.models.discoveredCount },
    { label: 'prices', status: operation.prices.status, count: operation.prices.discoveredCount },
    { label: 'validation', status: operation.packageValidation.status, count: operation.packageValidation.totalItems },
  ];
  return (
    <div className='space-y-5'>
      <div className='flex items-center justify-between gap-3'>
        <Badge variant={operation.state === 'FAILED' ? 'destructive' : 'secondary'}>{t(`eventPackages.${operation.state}`)}</Badge>
        <Button
          variant='ghost'
          size='sm'
          disabled={query.isFetching || !hasSystemScope('read_channels')}
          onClick={() => void query.refetch()}
        >
          <RefreshCw className='size-4' />
          {t('eventPackages.refresh')}
        </Button>
      </div>
      <div className='grid gap-3 sm:grid-cols-2'>
        {steps.map((step) => (
          <section key={step.label} className='bg-muted/20 rounded-xl border p-4'>
            <div className='flex items-center gap-2'>
              {['SUCCEEDED', 'VALID', 'NO_CHANGE'].includes(step.status) ? (
                <CheckCircle2 className='size-4 text-emerald-600 dark:text-emerald-400' />
              ) : step.status === 'RUNNING' ? (
                <Loader2 className='size-4 animate-spin' />
              ) : ['FAILED', 'BLOCKED', 'INTERRUPTED', 'NEEDS_ACTION'].includes(step.status) ? (
                <AlertCircle className='size-4 text-amber-600' />
              ) : (
                <Circle className='text-muted-foreground size-4' />
              )}
              <h3 className='text-sm font-medium'>{t(`eventPackages.${step.label}`)}</h3>
            </div>
            <p className='mt-3 text-xs'>
              {t(`eventPackages.${step.status}`)}
              {step.count !== null && (
                <span className='text-muted-foreground ml-2 tabular-nums'>
                  {step.count} {t('eventPackages.items')}
                </span>
              )}
            </p>
          </section>
        ))}
      </div>
      {[operation.models, operation.prices]
        .filter((stage) => stage.message)
        .map((stage, index) => (
          <p key={index} role='alert' className='rounded-lg bg-amber-500/10 p-3 text-xs'>
            {stage.errorCode}: {stage.message}
          </p>
        ))}
      {operation.state === 'PARTIAL' && <p className='text-muted-foreground text-sm'>{t('eventPackages.partial')}</p>}
      {hasSystemScope('read_commercialization') && operation.packages.length > 0 && (
        <div className='space-y-2'>
          <h3 className='text-sm font-medium'>{t('eventPackages.packages')}</h3>
          {operation.packages.map((p) => (
            <Link
              key={p.changeSetID}
              to='/change-sets/$changeSetId'
              params={{ changeSetId: p.changeSetID }}
              className='hover:bg-muted/40 flex items-center justify-between gap-3 rounded-lg border p-3 text-sm'
            >
              <span>
                {t(`eventPackages.${p.kind}`)} · {p.itemCount} {t('eventPackages.items')}
              </span>
              <Badge variant='outline'>{t(`eventPackages.${p.status}`)}</Badge>
            </Link>
          ))}
        </div>
      )}
      {hasSystemScope('write_channels') && operation.state === 'NOT_STARTED' && (
        <Button disabled={busy || pending} onClick={() => void start()}>
          {t('eventPackages.initial')}
        </Button>
      )}
      {hasSystemScope('write_channels') && retryableStages(operation).length > 0 && (
        <section className='space-y-3 rounded-xl border p-4'>
          <p className='text-sm'>{t('eventPackages.partial')}</p>
          {operation.prices.retryable && (
            <div className='grid gap-3 sm:grid-cols-2'>
              <div>
                <Label htmlFor='retry-pat'>{t('eventPackages.pat')}</Label>
                <Input id='retry-pat' type='password' autoComplete='off' value={pat} onChange={(e) => setPat(e.target.value)} />
              </div>
              <div>
                <Label htmlFor='retry-user'>{t('eventPackages.userID')}</Label>
                <Input id='retry-user' inputMode='numeric' value={userID} onChange={(e) => setUserID(e.target.value)} />
              </div>
            </div>
          )}
          <Button disabled={busy || pending || operation.state === 'RUNNING'} onClick={() => void retry()}>
            {t('eventPackages.retryStages')}
          </Button>
        </section>
      )}
      <section className='text-muted-foreground space-y-1 border-t pt-4 text-xs'>
        <h3 className='mb-2 font-medium'>{t('eventPackages.recurring')}</h3>
        <p>
          {t('eventPackages.models')}: {t(`eventPackages.${operation.recurringModelSyncEnabled ? 'enabled' : 'disabled'}`)}
        </p>
        <p>
          {t('eventPackages.prices')}:{' '}
          {t(
            `eventPackages.${operation.recurringPriceDiscoveryState === 'ENABLED' ? 'enabled' : operation.recurringPriceDiscoveryState === 'DISABLED' ? 'disabled' : 'awaiting'}`
          )}
        </p>
        <p className='pt-2 break-all'>
          {t('eventPackages.intent')}: {operationID} · attempt {operation.attempt}
        </p>
      </section>
    </div>
  );
}

export function NewApiCreateDialog({ open, onOpenChange }: { open: boolean; onOpenChange: (open: boolean) => void }) {
  const { t } = useTranslation();
  const { hasSystemScope } = usePermissions();
  const client = useQueryClient();
  const create = useCreateChannel();
  const [operationID, setOperationID] = useState(() => localStorage.getItem('conduit:pending-new-api') ?? crypto.randomUUID());
  const [name, setName] = useState('');
  const [url, setURL] = useState('');
  const [key, setKey] = useState('');
  const [pat, setPat] = useState('');
  const [userID, setUserID] = useState('');
  const [currency, setCurrency] = useState('');
  const [multiplier, setMultiplier] = useState('');
  const [result, setResult] = useState<DiscoveryOperation>();
  const [savedChannelID, setSavedChannelID] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  useEffect(() => {
    if (!open && result && !['NOT_STARTED', 'RUNNING'].includes(result.state)) {
      setResult(undefined);
      setSavedChannelID(undefined);
      setName('');
      setURL('');
      setKey('');
      setPat('');
      setUserID('');
      setCurrency('');
      setMultiplier('');
      setError(undefined);
      setOperationID(crypto.randomUUID());
      localStorage.removeItem('conduit:pending-new-api');
    }
  }, [open, result]);
  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(undefined);
    localStorage.setItem('conduit:pending-new-api', operationID);
    try {
      if (!name.trim() || !key.trim()) throw new Error(t('eventPackages.name') + ' / ' + t('eventPackages.key'));
      if (userID.trim() && !/^[1-9]\d*$/.test(userID.trim())) throw new Error(t('eventPackages.userID'));
      if (Boolean(currency.trim()) !== Boolean(multiplier.trim())) throw new Error(t('eventPackages.financialHint'));
      const root = canonicalNewApiURL(url);
      let existing = hasSystemScope('read_channels') ? await getDiscovery(operationID) : null;
      if (!existing) {
        const channel = await create.mutateAsync({
          operationID,
          name: name.trim(),
          type: 'openai',
          baseURL: root,
          websiteURL: root,
          quotaCurrency: 'USD',
          credentials: {
            apiKeys: [key.trim()],
            ...(pat.trim() ? { newApiPat: pat.trim() } : {}),
            ...(userID.trim() ? { newApiUserID: userID.trim() } : {}),
          },
          supportedModels: [],
          defaultTestModel: '',
          autoSyncSupportedModels: false,
          autoSyncModelPattern: '',
          tags: [],
          settings: {
            modelMappings: null,
            managementAdapter: 'new_api',
            ...(currency.trim() ? { billingCurrency: currency.trim().toUpperCase(), rechargeMultiplier: multiplier.trim() } : {}),
          },
        });
        rememberDiscovery(channel.id, operationID);
        setSavedChannelID(channel.id);
        setKey('');
        setPat('');
        existing = await initializeDiscovery({ channelID: channel.id, operationID });
      } else if (existing.state === 'NOT_STARTED') {
        setSavedChannelID(existing.channelID);
        setResult(existing);
        client.setQueryData(['channelDiscovery', operationID], existing);
        existing = await initializeDiscovery({ channelID: existing.channelID, operationID });
      }
      rememberDiscovery(existing.channelID, operationID);
      setSavedChannelID(existing.channelID);
      client.setQueryData(['channelDiscovery', operationID], existing);
      setResult(existing);
      localStorage.removeItem('conduit:pending-new-api');
      setKey('');
      setPat('');
      await client.invalidateQueries({ queryKey: ['channels'] });
    } catch (e) {
      setError(e instanceof Error ? e.message : t('eventPackages.error'));
      try {
        const saved = hasSystemScope('read_channels') ? await getDiscovery(operationID) : null;
        if (saved) {
          setResult(saved);
          setSavedChannelID(saved.channelID);
          client.setQueryData(['channelDiscovery', operationID], saved);
          rememberDiscovery(saved.channelID, operationID);
        }
      } catch {
        /* Keep the same operation and fields when the response is unknown. */
      }
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open={open} onOpenChange={(state) => !busy && onOpenChange(state)}>
      <DialogContent className='max-h-[90svh] max-w-2xl overflow-y-auto'>
        <DialogHeader>
          <DialogTitle>{t('eventPackages.newApi')}</DialogTitle>
          <DialogDescription>{t('eventPackages.newApiIntro')}</DialogDescription>
        </DialogHeader>
        {result || savedChannelID ? (
          <div className='space-y-4'>
            <section className='rounded-xl border border-emerald-500/30 bg-emerald-500/5 p-4'>
              <h3 className='text-sm font-medium'>{t('eventPackages.saved')}</h3>
              <p className='text-muted-foreground mt-2 text-xs break-all'>
                {name} · {savedChannelID ?? result?.channelID}
              </p>
              <p className='text-muted-foreground mt-1 text-xs break-all'>
                {t('eventPackages.intent')}: {operationID}
              </p>
              {hasSystemScope('read_channels') && (
                <Button variant='link' asChild className='mt-2 h-auto p-0'>
                  <Link to='/channels'>{t('eventPackages.viewSavedChannel')}</Link>
                </Button>
              )}
            </section>
            {busy && (
              <p role='status' className='flex items-center gap-2 text-sm'>
                <Loader2 className='size-4 animate-spin' />
                {t('eventPackages.initializing')}
              </p>
            )}
            <DiscoveryPanel operationID={operationID} initial={result} pending={busy} />
            {error && (
              <p role='alert' className='bg-destructive/10 rounded-lg p-3 text-sm'>
                {error}
              </p>
            )}
          </div>
        ) : (
          <form onSubmit={(e) => void save(e)} className='space-y-4'>
            <div className='grid gap-4 sm:grid-cols-2'>
              {[
                { id: 'new-name', label: 'name', value: name, set: setName, type: 'text' },
                { id: 'new-url', label: 'url', value: url, set: setURL, type: 'url' },
              ].map((field) => (
                <div key={field.id}>
                  <Label htmlFor={field.id}>{t(`eventPackages.${field.label}`)}</Label>
                  <Input id={field.id} type={field.type} required value={field.value} onChange={(e) => field.set(e.target.value)} />
                </div>
              ))}
            </div>
            <div>
              <Label htmlFor='new-key'>{t('eventPackages.key')}</Label>
              <Input id='new-key' type='password' autoComplete='off' required value={key} onChange={(e) => setKey(e.target.value)} />
            </div>
            <div className='grid gap-4 sm:grid-cols-2'>
              <div>
                <Label htmlFor='new-pat'>{t('eventPackages.pat')}</Label>
                <Input id='new-pat' type='password' autoComplete='off' value={pat} onChange={(e) => setPat(e.target.value)} />
              </div>
              <div>
                <Label htmlFor='new-user'>{t('eventPackages.userID')}</Label>
                <Input id='new-user' inputMode='numeric' value={userID} onChange={(e) => setUserID(e.target.value)} />
              </div>
            </div>
            <p className='text-muted-foreground text-xs'>{t('eventPackages.patHint')}</p>
            <div className='grid gap-4 border-t pt-4 sm:grid-cols-2'>
              <div>
                <Label htmlFor='new-currency'>{t('eventPackages.billingCurrency')}</Label>
                <Input id='new-currency' value={currency} onChange={(e) => setCurrency(e.target.value)} maxLength={8} />
              </div>
              <div>
                <Label htmlFor='new-multiplier'>{t('eventPackages.multiplier')}</Label>
                <Input id='new-multiplier' inputMode='decimal' value={multiplier} onChange={(e) => setMultiplier(e.target.value)} />
              </div>
            </div>
            <p className='text-muted-foreground text-xs'>{t('eventPackages.financialHint')}</p>
            {error && (
              <p role='alert' className='bg-destructive/10 rounded-lg p-3 text-sm'>
                {error}
              </p>
            )}
            <div className='flex flex-wrap justify-end gap-2'>
              <Button
                type='button'
                variant='outline'
                disabled={busy || !hasSystemScope('read_channels')}
                onClick={async () => {
                  try {
                    const saved = await getDiscovery(operationID);
                    if (saved) setResult(saved);
                    else toast.info(t('eventPackages.operationMissing'));
                  } catch (e) {
                    setError(String(e));
                  }
                }}
              >
                {t('eventPackages.checkResult')}
              </Button>
              <Button disabled={busy || create.isPending}>
                {busy && <Loader2 className='size-4 animate-spin' />}
                {t('eventPackages.create')}
              </Button>
            </div>
            <p className='text-muted-foreground text-[10px] break-all'>{operationID}</p>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}

export function NewApiChannelActions({ channel }: { channel: Channel }) {
  const { t } = useTranslation();
  const { hasSystemScope } = usePermissions();
  const [open, setOpen] = useState(false);
  const [operationID, setOperationID] = useState(() => recalledDiscovery(channel.id));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const [initial, setInitial] = useState<DiscoveryOperation>();
  const [unknown, setUnknown] = useState(false);
  const sync = async () => {
    const id = unknown && operationID ? operationID : crypto.randomUUID();
    setOperationID(id);
    rememberDiscovery(channel.id, id);
    setOpen(true);
    setBusy(true);
    setError(undefined);
    try {
      const result = await initializeDiscovery({ channelID: channel.id, operationID: id });
      setInitial(result);
      setUnknown(false);
    } catch (e) {
      setUnknown(true);
      setError(e instanceof Error ? e.message : t('eventPackages.error'));
    } finally {
      setBusy(false);
    }
  };
  let consoleURL: string | undefined;
  try {
    consoleURL = canonicalNewApiURL(channel.baseURL ?? channel.websiteURL ?? '');
  } catch {
    /* Invalid legacy configuration is shown by discovery, not linked. */
  }
  return (
    <>
      <div className='flex flex-wrap gap-1'>
        {consoleURL && (
          <Button size='sm' variant='outline' asChild>
            <a href={consoleURL} target='_blank' rel='noopener noreferrer'>
              <ExternalLink className='size-3' />
              {t('eventPackages.console')}
            </a>
          </Button>
        )}
        {hasSystemScope('write_channels') && (
          <Button size='sm' variant='outline' disabled={busy} onClick={() => void sync()}>
            <RefreshCw className='size-3' />
            {t(unknown ? 'eventPackages.checkResult' : 'eventPackages.sync')}
          </Button>
        )}
        {operationID && hasSystemScope('read_channels') && (
          <Button size='sm' variant='outline' onClick={() => setOpen(true)}>
            {t('eventPackages.result')}
          </Button>
        )}
        {hasSystemScope('read_commercialization') && (
          <Button size='sm' variant='ghost' asChild>
            <Link to='/change-sets' search={{ scopeType: 'channel', scopeID: channel.id }}>
              {t('eventPackages.packages')}
            </Link>
          </Button>
        )}
      </div>
      <Dialog open={open} onOpenChange={setOpen}>
        <DialogContent className='max-h-[90svh] max-w-2xl overflow-y-auto'>
          <DialogHeader>
            <DialogTitle>
              {channel.name} · {t('eventPackages.result')}
            </DialogTitle>
            <DialogDescription>{t('eventPackages.newApiIntro')}</DialogDescription>
          </DialogHeader>
          {busy ? (
            <p role='status'>{t('eventPackages.RUNNING')}</p>
          ) : (
            operationID && <DiscoveryPanel key={operationID} operationID={operationID} initial={initial} />
          )}{' '}
          {error && (
            <p role='alert' className='text-destructive text-sm'>
              {error}
            </p>
          )}
        </DialogContent>
      </Dialog>
    </>
  );
}
