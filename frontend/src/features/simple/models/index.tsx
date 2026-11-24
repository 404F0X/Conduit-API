import { Fragment, useMemo, useState } from 'react';
import { IconChevronDown, IconChevronRight, IconCopy } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Card, CardContent } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { EmptyState, ErrorState, LoadingRows, PageHeading } from '@/components/page-state';
import { Price } from '@/features/model-market';
import { type CatalogModel, useMyModelCatalog } from '@/features/model-market/data';
import { formatCount } from '../lib/format';

const ALL = '__all__';

function PriceCell({ model, value }: { model: CatalogModel; value: string | null }) {
  if (!value) return <span className='text-muted-foreground'>—</span>;
  return (
    <span className='tabular-nums'>
      {model.price.displayName} {value}
    </span>
  );
}

function StatusDot({ status }: { status?: string | null }) {
  const { t } = useTranslation();
  const key = (status || 'unknown').toLowerCase();
  const tone =
    key === 'operational'
      ? 'bg-emerald-500'
      : key === 'degraded'
        ? 'bg-amber-500'
        : key === 'disrupted'
          ? 'bg-red-500'
          : 'bg-muted-foreground/40';
  return (
    <span className='inline-flex items-center gap-1.5 text-sm'>
      <span className={cn('size-2 rounded-full', tone)} />
      {t(`simple.models.status.${['operational', 'degraded', 'disrupted'].includes(key) ? key : 'unknown'}`)}
    </span>
  );
}

/** 模型与价格：一张表，每 100 万 Token 的价格；没定价的模型不展示（调了也会被拒绝）。 */
export function SimpleModels() {
  const { t, i18n } = useTranslation();
  const [scope, setScope] = useState<{ modelGroupID?: string; apiKeyID?: string }>();
  const catalog = useMyModelCatalog(scope);
  const [search, setSearch] = useState('');
  const [vendor, setVendor] = useState(ALL);
  const [expanded, setExpanded] = useState<string | null>(null);

  const models = useMemo(() => (catalog.data?.models ?? []).filter((m) => m.price.billable), [catalog.data]);
  const vendors = useMemo(() => Array.from(new Set(models.map((m) => m.developer).filter(Boolean))).sort(), [models]);
  const visible = useMemo(() => {
    const keyword = search.trim().toLowerCase();
    return models
      .filter((m) => vendor === ALL || m.developer === vendor)
      .filter((m) => !keyword || m.modelId.toLowerCase().includes(keyword) || m.name.toLowerCase().includes(keyword))
      .sort((a, b) => a.modelId.localeCompare(b.modelId));
  }, [models, search, vendor]);
  const healthVisible = catalog.data?.healthVisible ?? false;

  const copy = (modelId: string) =>
    navigator.clipboard
      .writeText(modelId)
      .then(() => toast.success(t('simple.models.copied', { model: modelId })))
      .catch(() => toast.error(t('common.errors.copyFailed')));

  return (
    <>
      <Header fixed>
        <PageHeading title={t('simple.models.title')} description={t('simple.models.description')} />
      </Header>
      <Main>
        <Card>
          <CardContent className='space-y-4 pt-6'>
            <label className='grid gap-2 text-sm'>
              {t('modelMarket.scope.label')}
              <select
                className='bg-background h-9 rounded-md border px-2'
                value={catalog.data?.selectedPriceScope ?? ''}
                onChange={(event) => {
                  const selected = catalog.data?.priceScopes.find((item) => item.id === event.target.value);
                  setExpanded(null);
                  setScope(
                    selected ? (selected.apiKeyID ? { apiKeyID: selected.apiKeyID } : { modelGroupID: selected.modelGroupID }) : undefined
                  );
                }}
              >
                <option value=''>{t('modelMarket.scope.select')}</option>
                {(catalog.data?.priceScopes ?? []).map((item) => (
                  <option key={item.id} value={item.id}>
                    {item.label}
                  </option>
                ))}
              </select>
            </label>
            <p className='text-muted-foreground text-xs'>{t('modelMarket.scope.hint')}</p>
            <div className='flex flex-wrap items-center gap-2'>
              <Input
                value={search}
                onChange={(e) => setSearch(e.target.value)}
                placeholder={t('simple.models.search')}
                className='max-w-xs'
              />
              {vendors.length > 1 && (
                <Select value={vendor} onValueChange={setVendor}>
                  <SelectTrigger className='w-44'>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    <SelectItem value={ALL}>{t('simple.models.allVendors')}</SelectItem>
                    {vendors.map((v) => (
                      <SelectItem key={v} value={v}>
                        {v}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
              <span className='text-muted-foreground ml-auto text-xs'>{t('simple.models.unitHint')}</span>
            </div>

            {catalog.isLoading ? (
              <LoadingRows />
            ) : catalog.isError ? (
              <ErrorState onRetry={() => catalog.refetch()} />
            ) : models.length === 0 ? (
              <EmptyState title={t('simple.models.empty')} description={t('simple.models.emptyDescription')} />
            ) : visible.length === 0 ? (
              <EmptyState title={t('simple.models.noMatch')} />
            ) : (
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>{t('simple.models.columns.model')}</TableHead>
                    <TableHead>{t('simple.models.columns.input')}</TableHead>
                    <TableHead>{t('simple.models.columns.output')}</TableHead>
                    <TableHead>{t('simple.models.columns.cacheRead')}</TableHead>
                    {healthVisible && <TableHead>{t('simple.models.columns.status')}</TableHead>}
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {visible.map((model) => {
                    const open = expanded === model.id;
                    return (
                      <Fragment key={model.id}>
                        <TableRow className='cursor-pointer' onClick={() => setExpanded(open ? null : model.id)}>
                          <TableCell>
                            <div className='flex items-center gap-2'>
                              {open ? <IconChevronDown className='size-4' /> : <IconChevronRight className='size-4' />}
                              <div className='min-w-0'>
                                <div className='font-mono text-sm'>{model.modelId}</div>
                                {model.developer && <div className='text-muted-foreground text-xs'>{model.developer}</div>}
                              </div>
                              <Button
                                variant='ghost'
                                size='icon'
                                className='size-7'
                                aria-label={t('simple.models.copy')}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  copy(model.modelId);
                                }}
                              >
                                <IconCopy className='size-3.5' />
                              </Button>
                            </div>
                          </TableCell>
                          <TableCell>
                            <PriceCell model={model} value={model.price.inputPerMillion} />
                          </TableCell>
                          <TableCell>
                            <PriceCell model={model} value={model.price.outputPerMillion} />
                          </TableCell>
                          <TableCell>
                            <PriceCell model={model} value={model.price.cacheReadPerMillion} />
                          </TableCell>
                          {healthVisible && (
                            <TableCell>
                              <StatusDot status={model.health?.status} />
                            </TableCell>
                          )}
                        </TableRow>
                        {open && (
                          <TableRow className='bg-muted/40 hover:bg-muted/40'>
                            <TableCell colSpan={healthVisible ? 5 : 4}>
                              <Price model={model} large />
                              <dl className='grid grid-cols-2 gap-3 py-1 text-sm sm:grid-cols-4'>
                                <div>
                                  <dt className='text-muted-foreground text-xs'>{t('simple.models.detail.context')}</dt>
                                  <dd>{model.contextLimit ? formatCount(model.contextLimit, i18n.language) : '—'}</dd>
                                </div>
                                <div>
                                  <dt className='text-muted-foreground text-xs'>{t('simple.models.detail.output')}</dt>
                                  <dd>{model.outputLimit ? formatCount(model.outputLimit, i18n.language) : '—'}</dd>
                                </div>
                                <div>
                                  <dt className='text-muted-foreground text-xs'>{t('simple.models.detail.cacheWrite')}</dt>
                                  <dd>
                                    <PriceCell model={model} value={model.price.cacheWritePerMillion} />
                                  </dd>
                                </div>
                                <div>
                                  <dt className='text-muted-foreground text-xs'>{t('simple.models.detail.capabilities')}</dt>
                                  <dd className='truncate'>{model.capabilities.join(' · ') || '—'}</dd>
                                </div>
                              </dl>
                            </TableCell>
                          </TableRow>
                        )}
                      </Fragment>
                    );
                  })}
                </TableBody>
              </Table>
            )}
          </CardContent>
        </Card>
      </Main>
    </>
  );
}
