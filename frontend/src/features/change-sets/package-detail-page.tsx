import { useDeferredValue, useEffect, useRef, useState } from 'react';
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { Link, useNavigate } from '@tanstack/react-router';
import { ArrowLeft, Check, ChevronRight, RefreshCw, ShieldCheck } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { usePermissions } from '@/hooks/usePermissions';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Textarea } from '@/components/ui/textarea';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { DiscoveryPanel } from '@/features/channels/components/new-api-discovery';
import { getDiscovery, initializeDiscovery, rememberDiscovery } from '@/features/channels/data/discovery';
import { ExpressionPriceSummary } from '@/features/models/components/expression-price-summary';
import { LazyNewApiExpressionEditor } from '@/features/models/components/lazy-new-api-expression-editor';
import { isNewApiExpressionPrice, validNewApiExpressionDraft } from '@/features/models/components/simple-retail-price';
import {
  getEventPage,
  getItem,
  getItemPage,
  getProviderBase,
  findProviderOperation,
  createProviderPackage,
  saveProviderPackage,
  transitionPackage,
  usePackage,
  type ItemDetail,
  type PackageDetail,
  type ProviderPriceInput,
} from './data/change-sets';
import { eventActor, packageActionAllowed } from './package-state';
import {
  changedSnapshotFields,
  snapshotFieldValues,
  displayAmount,
  objectValue,
  priceRows,
  updatePriceAmount,
  isDefinitePackageRejection,
} from './price-view';
import { RetailDraftEditor } from './retail-draft-editor';
import { SecretReceipt } from './secret-receipt';

function SnapshotView({ value, title, fields }: { value: unknown; title: string; fields?: string[] }) {
  const { t } = useTranslation();
  const record = objectValue(value);
  const prices = priceRows(value).filter((row) => !fields || fields.includes(row.code));
  const model = objectValue(record.publicModel ?? record);
  const priceDocument = record.billingExpression ?? record.price ?? value;
  const grouped = objectValue(priceDocument).schema === 'group-conditional-price-v1';
  return (
    <section className='bg-background min-w-0 rounded-lg border p-4'>
      <h3 className='text-muted-foreground mb-3 text-xs font-medium'>{title}</h3>
      {fields?.length === 0 ? (
        <p className='text-muted-foreground text-sm'>{t('eventPackages.noDifference')}</p>
      ) : value == null ? (
        <p className='text-sm'>{t('eventPackages.noValue')}</p>
      ) : isNewApiExpressionPrice(priceDocument) && (!fields || fields.includes('billingExpression')) ? (
        <ExpressionPriceSummary price={priceDocument} />
      ) : grouped && (!fields || fields.includes('conditionalPrice')) ? (
        <pre className='max-h-64 overflow-auto text-xs break-all whitespace-pre-wrap'>{JSON.stringify(priceDocument, null, 2)}</pre>
      ) : prices.length ? (
        <dl className='space-y-2'>
          {Boolean(record.currencyCode) && <div className='mb-3 text-xs font-medium'>{String(record.currencyCode)}</div>}
          {prices.map((p, i) => (
            <div key={`${p.code}-${i}`} className='space-y-2 border-b pb-2 text-xs last:border-b-0'>
              <dt className='text-muted-foreground'>
                {t(`eventPackages.fields.${p.code}`, { defaultValue: p.code })}
                {p.variantCode && (
                  <span className='ml-2'>{t(`eventPackages.variants.${p.variantCode}`, { defaultValue: p.variantCode })}</span>
                )}
                <span className='block text-[10px]'>
                  {t(`eventPackages.pricingModes.${p.mode}`, { defaultValue: p.mode })} ·{' '}
                  {t(p.mode === 'flat_fee' || p.code === 'request' ? 'eventPackages.perRequest' : 'eventPackages.perMillion')}
                </span>
              </dt>
              <dd>
                {['usage_tiered', 'usage_volume'].includes(p.mode) && p.tiers.length ? (
                  <ol className='space-y-1'>
                    {p.tiers.map((tier, index) => (
                      <li key={index} className='flex flex-wrap justify-between gap-2'>
                        <span className='text-muted-foreground'>
                          {tier.upTo === null
                            ? t('eventPackages.tierUnlimited')
                            : t('eventPackages.tierUpTo', { threshold: displayAmount(tier.upTo) ?? t('eventPackages.unknown') })}
                        </span>
                        <span className='font-mono tabular-nums'>{displayAmount(tier.amount) ?? t('eventPackages.unknown')}</span>
                      </li>
                    ))}
                  </ol>
                ) : (
                  <span className='font-mono tabular-nums'>{displayAmount(p.amount) ?? t('eventPackages.unknown')}</span>
                )}
              </dd>
            </div>
          ))}
        </dl>
      ) : (
        <dl className='space-y-2 text-xs'>
          {['modelID', 'name', 'developer', 'type', 'group', 'deploymentID', 'routeStatus', 'status']
            .filter((k) => (!fields || fields.includes(k)) && (model[k] != null || record[k] != null))
            .map((k) => (
              <div key={k} className='flex justify-between gap-2'>
                <dt className='text-muted-foreground'>{t(`eventPackages.fields.${k}`, { defaultValue: k })}</dt>
                <dd className='break-all'>{String(model[k] ?? record[k])}</dd>
              </div>
            ))}
        </dl>
      )}
    </section>
  );
}
function ItemInspection({ detail }: { detail: ItemDetail }) {
  const { t } = useTranslation();
  const source = objectValue(detail.item.sourceSnapshot);
  const provenance = objectValue(source.provenance);
  const changed = changedSnapshotFields(detail.item.beforeSnapshot, detail.item.afterSnapshot);
  const unchanged = Object.keys({
    ...snapshotFieldValues(detail.item.beforeSnapshot),
    ...snapshotFieldValues(detail.item.afterSnapshot),
  }).filter((key) => !changed.includes(key));
  return (
    <div className='space-y-4'>
      {!detail.currentMatchesBefore && (
        <p role='alert' className='rounded-lg border border-amber-500/30 bg-amber-500/10 p-3 text-sm'>
          {t('eventPackages.drift')}
        </p>
      )}
      <div className='grid gap-3 lg:grid-cols-3'>
        <SnapshotView title={t('eventPackages.before')} value={detail.item.beforeSnapshot} fields={changed} />
        <SnapshotView title={t('eventPackages.proposed')} value={detail.item.afterSnapshot} fields={changed} />
        <SnapshotView title={t('eventPackages.current')} value={detail.currentSnapshot} fields={changed} />
      </div>
      {unchanged.length > 0 && (
        <details className='rounded-lg border px-4 py-3'>
          <summary className='text-muted-foreground cursor-pointer text-xs'>
            {t('eventPackages.unchanged', { count: unchanged.length })}
          </summary>
          <div className='mt-3 grid gap-3 lg:grid-cols-3'>
            <SnapshotView title={t('eventPackages.before')} value={detail.item.beforeSnapshot} fields={unchanged} />
            <SnapshotView title={t('eventPackages.proposed')} value={detail.item.afterSnapshot} fields={unchanged} />
            <SnapshotView title={t('eventPackages.current')} value={detail.currentSnapshot} fields={unchanged} />
          </div>
        </details>
      )}
      <section className='bg-muted/20 rounded-lg border p-4'>
        <h3 className='mb-3 text-sm font-medium'>{t('eventPackages.source')}</h3>
        <dl className='grid gap-3 text-xs sm:grid-cols-2'>
          {['sourceUnit', 'billingCurrency', 'rechargeMultiplier', 'accountingCurrency'].map((key) => (
            <div key={key}>
              <dt className='text-muted-foreground'>{t(`eventPackages.fields.${key}`, { defaultValue: key })}</dt>
              <dd className='mt-1 font-mono break-all'>{displayAmount(source[key]) ?? t('eventPackages.unknown')}</dd>
            </div>
          ))}
          {['upstreamVersion', 'quotaPerUnit', 'quotaDisplayType', 'referenceCurrency', 'referenceUnit']
            .filter((k) => provenance[k] != null)
            .map((key) => (
              <div key={key}>
                <dt className='text-muted-foreground'>{t(`eventPackages.fields.${key}`, { defaultValue: key })}</dt>
                <dd className='mt-1 break-all'>{String(provenance[key])}</dd>
              </div>
            ))}
        </dl>
        {Array.isArray(source.warnings) &&
          source.warnings.map((raw, index) => {
            const warning = objectValue(raw);
            const excludedChannelIDs = Array.isArray(warning.excludedChannelIDs)
              ? warning.excludedChannelIDs.filter((id) => typeof id === 'string')
              : [];
            if (typeof warning.message !== 'string') return null;
            return (
              <div key={index} className='mt-3 rounded border border-amber-500/30 p-3 text-sm'>
                <p>{warning.message}</p>
                {typeof warning.code === 'string' && <p className='text-muted-foreground mt-1 text-xs'>{warning.code}</p>}
                {excludedChannelIDs.length > 0 && (
                  <p className='mt-1 font-mono text-xs break-all'>
                    {t('models.expressionPrice.excludedChannels', { channels: excludedChannelIDs.join(', ') })}
                  </p>
                )}
              </div>
            );
          })}
        {typeof provenance.selectedGroup === 'string' && (
          <p className='mt-3 text-xs'>{t('models.expressionPrice.sourceGroup', { group: provenance.selectedGroup })}</p>
        )}
        {Object.keys(objectValue(provenance.fieldOrigins)).length > 0 && (
          <dl className='mt-4 grid gap-3 text-xs sm:grid-cols-2'>
            {Object.entries(objectValue(provenance.fieldOrigins)).map(([key, value]) => (
              <div key={key}>
                <dt className='text-muted-foreground'>{t(`eventPackages.fields.${key}`, { defaultValue: key })}</dt>
                <dd className='mt-1'>{t(`eventPackages.${String(value)}`, { defaultValue: String(value) })}</dd>
              </div>
            ))}
          </dl>
        )}
        {Array.isArray(provenance.issues) &&
          provenance.issues.map((raw, i) => {
            const issue = objectValue(raw);
            return (
              <p key={i} className='mt-3 text-xs'>
                {t(`eventPackages.issueCodes.${String(issue.code ?? '')}`, { defaultValue: String(issue.message ?? '') })}
              </p>
            );
          })}
        <details className='mt-4'>
          <summary className='text-muted-foreground cursor-pointer text-xs'>{t('eventPackages.technical')}</summary>
          <pre className='bg-muted mt-3 max-h-72 overflow-auto rounded-md p-3 text-[11px]'>
            {JSON.stringify(
              {
                before: detail.item.beforeSnapshot,
                proposed: detail.item.afterSnapshot,
                current: detail.currentSnapshot,
                source: detail.item.sourceSnapshot,
              },
              null,
              2
            )}
          </pre>
        </details>
      </section>
      {detail.issues.map((issue, i) => (
        <p key={i} role='alert' className='text-destructive text-xs'>
          {issue.message}
        </p>
      ))}
    </div>
  );
}

function ProviderDraftEditor({ detail, onSaved, onClose }: { detail: PackageDetail; onSaved: (id: string) => void; onClose: () => void }) {
  const { t } = useTranslation();
  const [rows, setRows] = useState<ProviderPriceInput[] | null>(null);
  const [base, setBase] = useState('');
  const [excluded, setExcluded] = useState<string[]>([]);
  const [reason, setReason] = useState('');
  const [confirmExclusions, setConfirmExclusions] = useState(false);
  const [operationID, setOperationID] = useState(() => crypto.randomUUID());
  const pendingIntent = useRef<Parameters<typeof createProviderPackage>[0] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string>();
  const load = async () => {
    setBusy(true);
    setError(undefined);
    try {
      const current = await getProviderBase(detail.summary.scopeID);
      const target = new Map<string, ProviderPriceInput>(
        current.prices.map((p) => [
          p.modelID,
          p.billingExpression
            ? { modelId: p.modelID, currencyCode: p.currencyCode, billingExpression: p.billingExpression }
            : { modelId: p.modelID, currencyCode: p.currencyCode, price: p.price },
        ])
      );
      const rowFromSnapshot = (modelId: string, snapshot: unknown): ProviderPriceInput => {
        const expression = objectValue(snapshot).billingExpression ?? snapshot;
        return isNewApiExpressionPrice(expression)
          ? { modelId, currencyCode: 'USD', billingExpression: expression }
          : { modelId, currencyCode: current.accountingCurrency, price: snapshot };
      };
      let after: string | undefined;
      const omitted: string[] = [];
      do {
        const page = await getItemPage(detail.summary.id, after);
        for (const edge of page.edges) {
          const item = await getItem(detail.summary.id, edge.node.id);
          if (!item) throw new Error(t('eventPackages.error'));
          if (detail.summary.status === 'DRAFT') {
            if (item.item.action === 'DELETE') target.delete(item.item.itemKey);
            else target.set(item.item.itemKey, rowFromSnapshot(item.item.itemKey, item.item.afterSnapshot));
          } else if (item.issues.length || item.item.validationError) {
            omitted.push(item.item.itemKey);
          } else if (item.item.action === 'DELETE') target.delete(item.item.itemKey);
          else target.set(item.item.itemKey, rowFromSnapshot(item.item.itemKey, item.item.afterSnapshot));
        }
        after = page.pageInfo.hasNextPage ? page.pageInfo.endCursor! : undefined;
      } while (after);
      setExcluded(omitted);
      setBase(current.revision);
      setRows([...target.values()]);
    } catch (e) {
      setError(e instanceof Error ? e.message : t('eventPackages.error'));
    } finally {
      setBusy(false);
    }
  };
  const invalidExpression = rows?.some((row) => row.billingExpression && !validNewApiExpressionDraft(row.billingExpression));
  const save = async () => {
    if (!rows || invalidExpression) return;
    setBusy(true);
    setError(undefined);
    try {
      if (detail.summary.status === 'DRAFT') {
        const result = await saveProviderPackage({ id: detail.summary.id, expectedRevision: detail.summary.revision, input: rows });
        onSaved(result.summary.id);
      } else {
        if (!reason.trim() || (excluded.length > 0 && !confirmExclusions)) throw new Error(t('eventPackages.reason'));
        const intent = pendingIntent.current ?? {
          channelID: detail.summary.scopeID,
          operationID,
          expectedBaseRevision: base,
          input: rows,
          correction: {
            sourceChangeSetID: detail.summary.id,
            expectedSourcePackageRevision: detail.summary.revision,
            excludedItemKeys: excluded,
            reason: reason.trim(),
          },
        };
        const recovered = pendingIntent.current ? await findProviderOperation(intent.operationID) : null;
        pendingIntent.current = intent;
        if (!recovered && (JSON.stringify(rows) !== JSON.stringify(intent.input) || reason.trim() !== intent.correction?.reason))
          throw new Error(t('eventPackages.pendingIntent'));
        const result = recovered ?? (await createProviderPackage(intent));
        if (recovered) toast.info(t('eventPackages.recovered'));
        pendingIntent.current = null;
        if (result.changeSetDetail) onSaved(result.changeSetDetail.summary.id);
        else toast.info(t('eventPackages.noChange'));
      }
      onClose();
    } catch (e) {
      if (isDefinitePackageRejection(e)) {
        pendingIntent.current = null;
        setOperationID(crypto.randomUUID());
      }
      setError(e instanceof Error ? e.message : t('eventPackages.error'));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent className='max-h-[85svh] max-w-5xl overflow-y-auto'>
        <DialogHeader>
          <DialogTitle>{t(`eventPackages.${detail.summary.status === 'DRAFT' ? 'edit' : 'correct'}`)}</DialogTitle>
          <DialogDescription>{t('eventPackages.intro')}</DialogDescription>
        </DialogHeader>
        {!rows ? (
          <Button onClick={() => void load()} disabled={busy}>
            {t('eventPackages.refresh')}
          </Button>
        ) : (
          <>
            <p className='text-muted-foreground text-xs'>{t('eventPackages.priceUnit')}</p>
            <div className='overflow-x-auto'>
              <table className='w-full text-xs'>
                <thead>
                  <tr>
                    <th className='p-2 text-left'>Model</th>
                    <th>{t('eventPackages.currency')}</th>
                    {['prompt_tokens', 'completion_tokens', 'prompt_cached_tokens', 'prompt_write_cached_tokens'].map((code) => (
                      <th key={code} className='min-w-28 p-2'>
                        {code}
                      </th>
                    ))}
                  </tr>
                </thead>
                <tbody>
                  {rows.map((row, index) => (
                    <tr key={row.modelId} className='border-t'>
                      <td className='max-w-56 p-2 break-all'>{row.modelId}</td>
                      <td className='p-2'>{row.currencyCode}</td>
                      {row.billingExpression ? (
                        <td colSpan={4} className='p-2'>
                          <LazyNewApiExpressionEditor
                            value={row.billingExpression}
                            onChange={(billingExpression) =>
                              setRows(rows.map((item, i) => (i === index ? { ...item, billingExpression } : item)))
                            }
                          />
                        </td>
                      ) : (
                        ['prompt_tokens', 'completion_tokens', 'prompt_cached_tokens', 'prompt_write_cached_tokens'].map((code) => {
                          const field = priceRows(row.price).find((p) => p.code === code);
                          return (
                            <td key={code} className='p-2'>
                              <Input
                                aria-label={`${row.modelId} ${code}`}
                                inputMode='decimal'
                                value={displayAmount(field?.amount) ?? ''}
                                placeholder={t('eventPackages.unknown')}
                                disabled={field != null && field.mode !== 'usage_per_unit'}
                                onChange={(e) => {
                                  try {
                                    setRows(
                                      rows.map((r, i) =>
                                        i === index ? { ...r, price: updatePriceAmount(r.price, code, e.target.value) } : r
                                      )
                                    );
                                  } catch (e) {
                                    setError(String(e));
                                  }
                                }}
                              />
                            </td>
                          );
                        })
                      )}
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {excluded.length > 0 && (
              <div className='rounded-lg border border-amber-500/30 p-3'>
                <p className='text-sm'>{t('eventPackages.exclude')}</p>
                <p className='mt-2 text-xs'>{excluded.join(', ')}</p>
                <label className='mt-3 flex items-center gap-2 text-xs'>
                  <Checkbox checked={confirmExclusions} onCheckedChange={(v) => setConfirmExclusions(v === true)} />
                  {t('eventPackages.excludeConfirm')}
                </label>
              </div>
            )}
            {detail.summary.status !== 'DRAFT' && (
              <Textarea
                value={reason}
                onChange={(e) => setReason(e.target.value)}
                placeholder={t('eventPackages.reason')}
                aria-label={t('eventPackages.reason')}
              />
            )}
          </>
        )}
        {error && (
          <p role='alert' className='text-destructive text-sm'>
            {error}
          </p>
        )}
        <DialogFooter>
          <Button variant='outline' onClick={onClose} disabled={busy}>
            {t('eventPackages.cancel')}
          </Button>
          <Button onClick={() => void save()} disabled={busy || !rows || invalidExpression}>
            {t('eventPackages.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

export function PackageDetailPage({
  id,
  returnContext = {},
}: {
  id: string;
  returnContext?: import('./change-set-search').PackageDetailSearch;
}) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const permissions = usePermissions();
  const client = useQueryClient();
  const query = usePackage(id);
  const [tab, setTab] = useState('overview');
  const [search, setSearch] = useState('');
  const deferred = useDeferredValue(search);
  const [problems, setProblems] = useState(false);
  const [itemCursors, setItemCursors] = useState<Array<string | undefined>>([undefined]);
  const [eventCursors, setEventCursors] = useState<Array<string | undefined>>([undefined]);
  const [itemID, setItemID] = useState<string>();
  const [action, setAction] = useState<'submit' | 'approve' | 'publish' | 'reject'>();
  const [note, setNote] = useState('');
  const [outcome, setOutcome] = useState<string>();
  const [busy, setBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [rebuildConfirm, setRebuildConfirm] = useState(false);
  const rebuildIntent = useRef<Parameters<typeof initializeDiscovery>[0] | null>(null);
  const [rebuildResult, setRebuildResult] = useState<import('@/features/channels/data/discovery').DiscoveryOperation>();
  const items = useQuery({
    queryKey: ['packageItems', id, itemCursors.at(-1), deferred, problems],
    queryFn: () => getItemPage(id, itemCursors.at(-1), deferred || undefined, problems),
    enabled: tab === 'changes' && Boolean(query.data),
  });
  const item = useQuery({
    queryKey: ['packageItem', id, itemID],
    queryFn: () => getItem(id, itemID!),
    enabled: tab === 'changes' && Boolean(itemID),
  });
  const events = useQuery({
    queryKey: ['packageEvents', id, eventCursors.at(-1)],
    queryFn: () => getEventPage(id, eventCursors.at(-1)),
    enabled: tab === 'activity' && Boolean(query.data),
  });
  const fresh = Boolean(query.data) && !query.isFetching && !query.isError;
  const detail = query.data;
  const canWrite = Boolean(detail?.availableActions.length);
  const canWritePrices = permissions.hasSystemScope('write_commercialization');
  useEffect(() => {
    if (action && detail && (!canWrite || !packageActionAllowed(detail, action))) {
      setAction(undefined);
      setOutcome(t('eventPackages.actionUnavailable', { status: t(`eventPackages.${detail.summary.status}`) }));
    }
  }, [action, detail, canWrite, t]);
  const reload = () =>
    Promise.all(
      ['changeSets', 'changeSet', 'packageItems', 'packageItem', 'packageEvents', 'commercialization-catalog', 'channels'].map((k) =>
        client.invalidateQueries({ queryKey: [k] })
      )
    );
  const execute = async () => {
    if (!detail || !action || !fresh || !canWrite || busy || !packageActionAllowed(detail, action)) return;
    setBusy(true);
    setOutcome(undefined);
    try {
      await transitionPackage(action, { id, expectedRevision: detail.summary.revision, reviewNote: note.trim() || undefined });
      toast.success(t('eventPackages.success'));
      setAction(undefined);
      setNote('');
    } catch (e) {
      const saved = await query.refetch();
      if (saved.isSuccess && saved.data) {
        const message = t('eventPackages.recoveredStatus', { status: t(`eventPackages.${saved.data.summary.status}`) });
        setOutcome(message);
        if (!packageActionAllowed(saved.data, action)) setAction(undefined);
        toast.info(message);
      } else {
        setOutcome(t('eventPackages.actionUnknown'));
      }
      toast.error(e instanceof Error ? e.message : t('eventPackages.applyError'));
    } finally {
      await reload();
      setBusy(false);
    }
  };
  const rebuild = async () => {
    if (!detail) return;
    setBusy(true);
    const intent = rebuildIntent.current ?? {
      channelID: detail.summary.scopeID,
      operationID: crypto.randomUUID(),
      stages: ['PRICES' as const],
      rebuild: { sources: [{ id, expectedRevision: detail.summary.revision }], supersedeSources: true },
    };
    rebuildIntent.current = intent;
    rememberDiscovery(detail.summary.scopeID, intent.operationID);
    try {
      const saved = permissions.hasSystemScope('read_channels') ? await getDiscovery(intent.operationID) : null;
      const result = saved ?? (await initializeDiscovery(intent));
      setRebuildResult(result);
      toast.info(t(`eventPackages.${result.state}`));
      if (!['NOT_STARTED', 'RUNNING', 'INTERRUPTED'].includes(result.state)) rebuildIntent.current = null;
      setRebuildConfirm(false);
    } catch (e) {
      if (isDefinitePackageRejection(e)) rebuildIntent.current = null;
      toast.error(e instanceof Error ? e.message : t('eventPackages.error'));
    } finally {
      await reload();
      setBusy(false);
    }
  };
  return (
    <>
      <Header fixed>
        <Link
          to={returnContext.list === 'changelog' ? '/changelog' : '/change-sets'}
          search={returnContext}
          className='text-muted-foreground flex items-center gap-2 text-sm'
        >
          <ArrowLeft className='size-4' />
          <span>{t('eventPackages.back')}</span>
        </Link>
        <Button size='sm' variant='outline' onClick={() => void reload()} disabled={query.isFetching}>
          <RefreshCw className='size-4' />
          {t('eventPackages.refresh')}
        </Button>
      </Header>
      <Main className='space-y-6 pb-12'>
        {query.isLoading ? (
          <p role='status'>{t('eventPackages.loading')}</p>
        ) : query.isError || !detail ? (
          <div role='alert' className='rounded-xl border p-8'>
            <p>{query.error?.message ?? t('eventPackages.error')}</p>
            <Button className='mt-4' onClick={() => void query.refetch()}>
              {t('eventPackages.retry')}
            </Button>
          </div>
        ) : (
          <>
            <div className='flex flex-col justify-between gap-4 lg:flex-row lg:items-start'>
              <div className='min-w-0'>
                <div className='mb-3 flex flex-wrap gap-2'>
                  <Badge variant={detail.summary.status === 'INVALID' ? 'destructive' : 'secondary'}>
                    {t(`eventPackages.${detail.summary.status}`)}
                  </Badge>
                  <Badge variant='outline'>{t(`eventPackages.${detail.summary.kind}`)}</Badge>
                  <span className='text-muted-foreground text-xs'>#{id.split('/').at(-1)}</span>
                </div>
                <h1 className='text-2xl font-semibold tracking-tight'>{detail.summary.scopeName ?? detail.summary.title}</h1>
                <p className='text-muted-foreground mt-2 text-sm'>{detail.summary.title}</p>
                <p className='text-muted-foreground mt-2 text-xs'>
                  {t(`eventPackages.${detail.summary.sourceOrigin}`)} ·{' '}
                  {detail.summary.actorName ??
                    (detail.summary.createdBy
                      ? t('eventPackages.userFallback', { id: detail.summary.createdBy.split('/').at(-1) })
                      : t(detail.summary.sourceOrigin === 'AUTOMATIC_SYNC' ? 'eventPackages.system' : 'eventPackages.unknown'))}{' '}
                  · {new Date(detail.summary.createdAt).toLocaleString()}
                </p>
              </div>
              <div className='bg-muted/35 flex gap-6 rounded-xl border px-5 py-4'>
                <div>
                  <p className='text-muted-foreground text-xs'>{t('eventPackages.items')}</p>
                  <p className='mt-1 text-2xl font-semibold tabular-nums'>{detail.summary.itemCount}</p>
                </div>
                <div>
                  <p className='text-muted-foreground text-xs'>{t('eventPackages.issues')}</p>
                  <p className='mt-1 text-2xl font-semibold tabular-nums'>{detail.summary.invalidItemCount}</p>
                </div>
              </div>
            </div>
            {rebuildResult && <DiscoveryPanel operationID={rebuildResult.operationID} initial={rebuildResult} />}
            {outcome && (
              <section role='status' className='bg-muted/20 rounded-xl border p-4 text-sm'>
                <p>{outcome}</p>
                {note && (
                  <p className='text-muted-foreground mt-2 whitespace-pre-wrap'>
                    {t('eventPackages.retainedNote')}: {note}
                  </p>
                )}
              </section>
            )}
            <Tabs value={tab} onValueChange={setTab}>
              <TabsList>
                {['overview', 'changes', 'activity'].map((value) => (
                  <TabsTrigger key={value} value={value}>
                    {t(`eventPackages.${value}`)}
                  </TabsTrigger>
                ))}
              </TabsList>
              <TabsContent value='overview' className='mt-4 space-y-5'>
                <div className='grid gap-5 lg:grid-cols-[minmax(0,1fr)_20rem]'>
                  <section className='bg-card rounded-xl border p-5'>
                    <h2 className='mb-4 flex items-center gap-2 text-sm font-semibold'>
                      <ShieldCheck className='size-4' />
                      {t('eventPackages.issues')}
                    </h2>
                    {detail.issues.length ? (
                      detail.issues.map((issue, i) => (
                        <div key={i} className='bg-muted/35 mb-3 rounded-lg p-3 text-sm'>
                          <p className='font-medium'>{issue.itemKey ?? t('eventPackages.packages')}</p>
                          <p className='text-muted-foreground mt-1'>{issue.message}</p>
                        </div>
                      ))
                    ) : (
                      <p className='text-muted-foreground text-sm'>{t('eventPackages.noIssues')}</p>
                    )}
                    <p className='text-muted-foreground mt-5 text-xs'>
                      {t('eventPackages.wholeCount', { count: detail.summary.itemCount })}
                    </p>
                  </section>
                  <section className='rounded-xl border p-5'>
                    <p className='text-sm'>{detail.summary.scopeName ?? detail.summary.scopeID}</p>
                    <details className='mt-4'>
                      <summary className='text-muted-foreground cursor-pointer text-xs'>{t('eventPackages.technical')}</summary>{' '}
                      <dl className='space-y-4 text-xs'>
                        {[
                          [t('eventPackages.revision'), detail.summary.revision],
                          [t('eventPackages.sourceRevision'), detail.sourceRevision],
                          [t('eventPackages.intent'), detail.summary.operationID ?? t('eventPackages.unknown')],
                        ].map(([key, value]) => (
                          <div key={key}>
                            <dt className='text-muted-foreground'>{key}</dt>
                            <dd className='mt-1 font-mono break-all'>{value}</dd>
                          </div>
                        ))}
                      </dl>
                    </details>
                  </section>
                </div>
                {detail.summary.kind === 'MODEL_MAPPING' && ['INVALID', 'SUPERSEDED'].includes(detail.summary.status) && (
                  <section className='rounded-xl border border-amber-500/30 p-5'>
                    <p className='mb-3 text-sm'>{t('eventPackages.mappingCorrection')}</p>
                    <Button variant='outline' asChild>
                      <Link to='/channels'>{t('eventPackages.console')}</Link>
                    </Button>
                  </section>
                )}
                {detail.appliedTargets.length > 0 && (
                  <section className='rounded-xl border border-emerald-500/25 bg-emerald-500/5 p-5'>
                    <h2 className='mb-3 flex items-center gap-2 text-sm font-medium'>
                      <Check className='size-4' />
                      {t('eventPackages.published')}
                    </h2>
                    <div className='flex flex-wrap gap-2'>
                      {detail.appliedTargets.map((target) => (
                        <Link
                          key={`${target.type}:${target.id}`}
                          to='/models'
                          search={
                            target.type === 'retail_price'
                              ? { view: 'models', model: target.itemKey }
                              : { view: 'channels', channel: detail.summary.scopeID, upstreamModel: target.itemKey.split(':')[0] }
                          }
                          className='bg-background rounded-md border px-3 py-2 text-xs'
                        >
                          {target.itemKey} · {target.type} #{target.id} <ChevronRight className='inline size-3' />
                        </Link>
                      ))}
                    </div>
                  </section>
                )}
              </TabsContent>
              <TabsContent value='changes' className='mt-4 space-y-4'>
                <div className='flex flex-wrap items-center justify-between gap-3'>
                  <Input
                    className='max-w-sm'
                    value={search}
                    onChange={(e) => {
                      setSearch(e.target.value);
                      setItemCursors([undefined]);
                    }}
                    placeholder={t('eventPackages.findItem')}
                    aria-label={t('eventPackages.findItem')}
                  />
                  <label className='flex items-center gap-2 text-xs'>
                    <Checkbox
                      checked={problems}
                      onCheckedChange={(value) => {
                        setProblems(value === true);
                        setItemCursors([undefined]);
                      }}
                    />
                    {t('eventPackages.onlyProblems')}
                  </label>
                </div>
                <div className='grid gap-4 xl:grid-cols-[20rem_minmax(0,1fr)]'>
                  <div className='self-start overflow-hidden rounded-xl border'>
                    {items.isLoading ? (
                      <p className='p-5'>{t('eventPackages.loading')}</p>
                    ) : items.isError ? (
                      <p role='alert' className='p-5'>
                        {items.error.message}
                      </p>
                    ) : (
                      items.data?.edges.map((edge) => (
                        <button
                          key={edge.node.id}
                          className={`hover:bg-muted/40 flex w-full items-start justify-between gap-3 border-b p-4 text-left text-sm transition-colors last:border-0 ${itemID === edge.node.id ? 'bg-muted/60' : ''}`}
                          onClick={() => setItemID(edge.node.id)}
                        >
                          <span className='min-w-0 break-all'>
                            <span className='font-medium'>{edge.node.modelLabel ?? edge.node.itemKey}</span>
                            <span className='text-muted-foreground mt-1 block text-xs'>
                              {t(`eventPackages.${edge.node.action}`)} · Δ{edge.node.changedFieldCount}
                            </span>
                          </span>
                          {edge.node.issues.length > 0 && <Badge variant='destructive'>!</Badge>}
                        </button>
                      ))
                    )}
                    {items.data && (
                      <div className='flex items-center justify-between gap-1 border-t p-3 text-xs'>
                        <span className='tabular-nums'>{items.data.totalCount}</span>
                        <Button
                          size='sm'
                          variant='ghost'
                          disabled={itemCursors.length === 1 || items.isFetching}
                          onClick={() => setItemCursors(itemCursors.slice(0, -1))}
                        >
                          {t('eventPackages.previous')}
                        </Button>
                        <Button
                          size='sm'
                          variant='ghost'
                          disabled={!items.data.pageInfo.hasNextPage || items.isFetching}
                          onClick={() => setItemCursors([...itemCursors, items.data!.pageInfo.endCursor!])}
                        >
                          {t('eventPackages.next')}
                        </Button>
                      </div>
                    )}
                  </div>
                  <div className='min-w-0'>
                    {item.isLoading ? (
                      <p role='status'>{t('eventPackages.loading')}</p>
                    ) : item.isError ? (
                      <p role='alert'>{item.error.message}</p>
                    ) : item.data ? (
                      <ItemInspection detail={item.data} />
                    ) : (
                      <p className='text-muted-foreground rounded-xl border border-dashed p-10 text-center text-sm'>
                        {t('eventPackages.changes')}
                      </p>
                    )}
                  </div>
                </div>
              </TabsContent>
              <TabsContent value='activity' className='mt-4 space-y-4'>
                {events.isLoading ? (
                  <p>{t('eventPackages.loading')}</p>
                ) : events.isError ? (
                  <p role='alert'>{events.error.message}</p>
                ) : (
                  events.data?.edges.map((edge) => {
                    const actor = eventActor(edge.node, detail.summary);
                    return (
                      <article key={edge.node.id} className='flex gap-4 rounded-xl border p-4'>
                        <span className='bg-muted mt-1 size-2 shrink-0 rounded-full' />
                        <div className='min-w-0'>
                          <p className='text-sm font-medium'>
                            {t(`eventPackages.events.${edge.node.eventType}`, { defaultValue: edge.node.eventType })}
                          </p>
                          <p className='text-muted-foreground mt-1 text-xs'>
                            {actor.name ??
                              (actor.id
                                ? t('eventPackages.userFallback', { id: actor.id })
                                : t(actor.type === 'system' ? 'eventPackages.system' : 'eventPackages.unknown'))}
                            {actor.name && actor.id && <span className='ml-1'>({actor.id})</span>}
                            {' · '}
                            <time dateTime={edge.node.createdAt}>{new Date(edge.node.createdAt).toLocaleString()}</time>
                          </p>
                          <details className='mt-3'>
                            <summary className='text-muted-foreground cursor-pointer text-xs'>{t('eventPackages.technical')}</summary>
                            <pre className='mt-2 max-w-full overflow-auto text-[11px]'>{JSON.stringify(edge.node.detail, null, 2)}</pre>
                          </details>
                        </div>
                      </article>
                    );
                  })
                )}
                <div className='flex justify-end gap-2'>
                  <Button
                    variant='outline'
                    size='sm'
                    disabled={eventCursors.length === 1 || events.isFetching}
                    onClick={() => setEventCursors(eventCursors.slice(0, -1))}
                  >
                    {t('eventPackages.previous')}
                  </Button>
                  <Button
                    variant='outline'
                    size='sm'
                    disabled={!events.data?.pageInfo.hasNextPage || events.isFetching}
                    onClick={() => setEventCursors([...eventCursors, events.data!.pageInfo.endCursor!])}
                  >
                    {t('eventPackages.next')}
                  </Button>
                </div>
              </TabsContent>
            </Tabs>
            <section className='bg-background/95 sticky bottom-3 z-10 flex flex-wrap items-center justify-between gap-3 rounded-xl border p-4 shadow-lg backdrop-blur-sm'>
              <p className='text-muted-foreground text-xs'>{t('eventPackages.wholeCount', { count: detail.summary.itemCount })}</p>
              <div className='flex flex-wrap gap-2'>
                {canWrite &&
                  ['PROVIDER_PRICE', 'RETAIL_PRICE'].includes(detail.summary.kind) &&
                  detail.availableActions.includes('EDIT') && (
                    <Button variant='outline' onClick={() => setEditing(true)} disabled={!fresh || busy}>
                      {t('eventPackages.edit')}
                    </Button>
                  )}
                {canWritePrices &&
                  detail.summary.kind === 'PROVIDER_PRICE' &&
                  ['INVALID', 'SUPERSEDED'].includes(detail.summary.status) && (
                    <>
                      <Button variant='outline' onClick={() => setEditing(true)} disabled={!fresh || busy}>
                        {t('eventPackages.correct')}
                      </Button>
                      {permissions.hasSystemScope('write_channels') && detail.summary.sourceOrigin !== 'MANUAL_EDIT' && (
                        <Button variant='outline' onClick={() => setRebuildConfirm(true)} disabled={!fresh || busy}>
                          {t('eventPackages.rebuild')}
                        </Button>
                      )}
                    </>
                  )}
                {detail.summary.scopeType === 'governed' && detail.summary.status === 'APPLIED' && <SecretReceipt id={id} />}
                {canWrite && detail.availableActions.includes('SUBMIT') && (
                  <Button onClick={() => setAction('submit')} disabled={!fresh || busy}>
                    {t('eventPackages.submit')}
                  </Button>
                )}
                {canWrite && detail.availableActions.includes('APPROVE') && (
                  <Button onClick={() => setAction('approve')} disabled={!fresh || busy}>
                    {t('eventPackages.approve')}
                  </Button>
                )}
                {canWrite && detail.availableActions.includes('PUBLISH') && (
                  <Button onClick={() => setAction('publish')} disabled={!fresh || busy}>
                    {t('eventPackages.publish')}
                  </Button>
                )}
                {canWrite && detail.availableActions.includes('REJECT') && (
                  <Button variant='outline' onClick={() => setAction('reject')} disabled={!fresh || busy}>
                    {t('eventPackages.reject')}
                  </Button>
                )}
                {['APPLIED', 'REJECTED', 'SUPERSEDED'].includes(detail.summary.status) && (
                  <span className='text-muted-foreground text-xs'>{t('eventPackages.terminal')}</span>
                )}
              </div>
            </section>
            {editing && detail.summary.kind === 'PROVIDER_PRICE' && (
              <ProviderDraftEditor
                detail={detail}
                onClose={() => setEditing(false)}
                onSaved={(savedID) => {
                  void reload();
                  if (savedID !== id) void navigate({ to: '/change-sets/$changeSetId', params: { changeSetId: savedID } });
                }}
              />
            )}
            {editing && detail.summary.kind === 'RETAIL_PRICE' && (
              <RetailDraftEditor detail={detail} onClose={() => setEditing(false)} onSaved={() => void reload()} />
            )}
            <Dialog
              open={Boolean(action) || rebuildConfirm}
              onOpenChange={(open) => {
                if (!open && !busy) {
                  setAction(undefined);
                  setRebuildConfirm(false);
                }
              }}
            >
              <DialogContent>
                <DialogHeader>
                  <DialogTitle>{t('eventPackages.confirm')}</DialogTitle>
                  <DialogDescription>{t(rebuildConfirm ? 'eventPackages.selectSources' : 'eventPackages.confirmBody')}</DialogDescription>
                </DialogHeader>
                <p className='text-sm'>
                  {detail.summary.title} · {detail.summary.itemCount} {t('eventPackages.items')}
                </p>
                {(action === 'approve' || action === 'reject') && !rebuildConfirm && (
                  <Textarea
                    value={note}
                    onChange={(e) => setNote(e.target.value)}
                    maxLength={2000}
                    placeholder={t('eventPackages.note')}
                    aria-label={t('eventPackages.note')}
                  />
                )}
                <DialogFooter>
                  <Button
                    variant='outline'
                    disabled={busy}
                    onClick={() => {
                      setAction(undefined);
                      setRebuildConfirm(false);
                    }}
                  >
                    {t('eventPackages.cancel')}
                  </Button>
                  <Button
                    disabled={busy || !fresh || !canWrite || (!rebuildConfirm && (!action || !packageActionAllowed(detail, action)))}
                    onClick={() => void (rebuildConfirm ? rebuild() : execute())}
                  >
                    {t(`eventPackages.${rebuildConfirm ? 'rebuild' : (action ?? 'confirm')}`)}
                  </Button>
                </DialogFooter>
              </DialogContent>
            </Dialog>
          </>
        )}
      </Main>
    </>
  );
}
