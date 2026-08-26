import { useMemo, useState } from 'react';
import { Link } from '@tanstack/react-router';
import {
  IconAlertTriangle,
  IconArrowLeft,
  IconBolt,
  IconBrain,
  IconCheck,
  IconCopy,
  IconKey,
  IconSearch,
  IconSparkles,
} from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { type CatalogModel, useMyModelCatalog } from './data';

function compact(value: number | null) {
  if (!value) return '—';
  return new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 }).format(value);
}

function Health({ health }: { health: CatalogModel['health'] }) {
  const { t } = useTranslation();
  const status = health?.status.toLowerCase() || 'unknown';
  const tone =
    status === 'operational'
      ? 'text-emerald-600'
      : status === 'degraded'
        ? 'text-amber-600'
        : status === 'disrupted'
          ? 'text-destructive'
          : 'text-muted-foreground';
  return (
    <span className={`inline-flex items-center gap-1.5 text-xs font-medium ${tone}`}>
      {status === 'operational' ? <IconCheck size={14} /> : <IconAlertTriangle size={14} />}
      {t(`modelMarket.health.${status}`)}
    </span>
  );
}

function healthFreshness(value: string | null | undefined, locale: string) {
  if (!value) return null;
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return null;
  return new Intl.DateTimeFormat(locale, { dateStyle: 'short', timeStyle: 'short' }).format(date);
}

function Price({ model, large = false }: { model: CatalogModel; large?: boolean }) {
  const { t } = useTranslation();
  if (!model.price.billable) return <span className='text-muted-foreground text-sm'>{t('modelMarket.price.unpriced')}</span>;
  return (
    <div className={large ? 'grid gap-3 sm:grid-cols-2' : 'grid grid-cols-2 gap-3'}>
      {[
        [t('modelMarket.price.input'), model.price.inputPerMillion],
        [t('modelMarket.price.output'), model.price.outputPerMillion],
        [t('modelMarket.price.cacheRead'), model.price.cacheReadPerMillion],
        [t('modelMarket.price.cacheWrite'), model.price.cacheWritePerMillion],
      ].map(([label, value]) => (
        <div key={label}>
          <div className='text-muted-foreground text-[11px] font-medium tracking-wide uppercase'>{label}</div>
          <div className={`font-mono font-semibold tabular-nums ${large ? 'text-xl' : 'text-sm'}`}>
            {value ? `${model.price.displayName} ${value}` : '—'}
          </div>
        </div>
      ))}
    </div>
  );
}

function ModelDetail({ model, healthVisible, onBack }: { model: CatalogModel; healthVisible: boolean; onBack: () => void }) {
  const { t } = useTranslation();
  return (
    <div className='space-y-5'>
      <Button variant='ghost' className='-ml-2' onClick={onBack}>
        <IconArrowLeft /> {t('modelMarket.back')}
      </Button>
      <section className='bg-card relative overflow-hidden rounded-xl border p-6 shadow-sm md:p-8'>
        <div className='absolute inset-y-0 left-0 w-1 bg-sky-500' />
        <div className='grid gap-7 xl:grid-cols-[minmax(0,1fr)_360px] xl:items-end'>
          <div>
            <div className='bg-muted mb-4 flex size-12 items-center justify-center rounded-lg border font-mono text-lg font-bold'>
              {model.name.slice(0, 1).toUpperCase()}
            </div>
            <h1 className='text-3xl font-bold tracking-tight'>{model.name}</h1>
            <div className='text-muted-foreground mt-2 flex flex-wrap items-center gap-2 font-mono text-sm'>
              <span>{model.modelId}</span>
              <button
                aria-label={t('modelMarket.copy')}
                onClick={() => void navigator.clipboard.writeText(model.modelId).then(() => toast.success(t('modelMarket.copied')))}
              >
                <IconCopy size={15} />
              </button>
            </div>
            <div className='mt-4 flex flex-wrap gap-2'>
              <Badge variant='secondary'>{model.developer}</Badge>
              <Badge variant='outline'>{model.modelType}</Badge>
              {model.capabilities.map((item) => (
                <Badge key={item} variant='outline'>
                  {item}
                </Badge>
              ))}
            </div>
          </div>
          <div className='border-t pt-5 xl:border-t-0 xl:border-l xl:pt-0 xl:pl-7'>
            <Price model={model} large />
            <p className='text-muted-foreground mt-3 text-xs'>
              {t('modelMarket.price.effectiveHint', { multiplier: model.price.effectiveMultiplier })}
            </p>
          </div>
        </div>
      </section>
      <div className='grid gap-5 lg:grid-cols-[minmax(0,1fr)_300px]'>
        <section className='space-y-3'>
          <Card className='py-5 shadow-none'>
            <CardHeader className='px-5'>
              <CardTitle className='flex items-center gap-2 text-base'>
                <IconBolt className='text-sky-600' size={18} /> {t('modelMarket.detail.automaticRouting')}
              </CardTitle>
              <CardDescription>{t('modelMarket.detail.automaticRoutingHint')}</CardDescription>
            </CardHeader>
            {healthVisible && (
              <CardContent className='px-5'>
                <Health health={model.health} />
              </CardContent>
            )}
          </Card>
        </section>
        <aside className='space-y-3'>
          <Card className='py-5 shadow-none'>
            <CardHeader className='px-5'>
              <CardTitle className='text-base'>{t('modelMarket.detail.limits')}</CardTitle>
            </CardHeader>
            <CardContent className='grid grid-cols-2 gap-4 px-5'>
              <div>
                <div className='text-muted-foreground text-xs'>{t('modelMarket.detail.context')}</div>
                <div className='mt-1 font-mono font-semibold'>{compact(model.contextLimit)}</div>
              </div>
              <div>
                <div className='text-muted-foreground text-xs'>{t('modelMarket.detail.output')}</div>
                <div className='mt-1 font-mono font-semibold'>{compact(model.outputLimit)}</div>
              </div>
            </CardContent>
          </Card>
          <Button asChild className='w-full'>
            <Link to='/project/api-keys'>
              <IconKey /> {t('modelMarket.apiKeys')}
            </Link>
          </Button>
        </aside>
      </div>
    </div>
  );
}

export default function ModelMarket() {
  const { t, i18n } = useTranslation();
  const query = useMyModelCatalog();
  const [selected, setSelected] = useState<CatalogModel | null>(null);
  const [search, setSearch] = useState('');
  const [type, setType] = useState('all');
  const [sort, setSort] = useState('name');
  const models = useMemo(() => {
    const term = search.trim().toLowerCase();
    const filtered = (query.data?.models || []).filter(
      (model) =>
        (type === 'all' || model.modelType === type) &&
        (!term || [model.name, model.modelId, model.developer].some((value) => value.toLowerCase().includes(term)))
    );
    return filtered.sort((a, b) =>
      sort === 'price'
        ? Number(a.price.inputPerMillion || Infinity) - Number(b.price.inputPerMillion || Infinity)
        : a.name.localeCompare(b.name)
    );
  }, [query.data?.models, search, sort, type]);
  const types = [...new Set((query.data?.models || []).map((model) => model.modelType))];

  return (
    <>
      <Header fixed>
        <div>
          <h2 className='flex items-center gap-2 text-xl font-bold tracking-tight'>
            <IconSparkles className='text-sky-600' size={22} />
            {t('modelMarket.title')}
          </h2>
          <p className='text-muted-foreground text-sm'>{t('modelMarket.description')}</p>
        </div>
      </Header>
      <Main className='space-y-5 pb-10'>
        {selected && query.data ? (
          <ModelDetail model={selected} healthVisible={query.data.healthVisible} onBack={() => setSelected(null)} />
        ) : (
          <>
            <Alert className='border-sky-500/20 bg-sky-500/5'>
              <IconBolt className='text-sky-600' />
              <AlertTitle>{t('modelMarket.access.title')}</AlertTitle>
              <AlertDescription>{t('modelMarket.access.description')}</AlertDescription>
            </Alert>
            {query.isError ? (
              <Alert variant='destructive'>
                <IconAlertTriangle />
                <AlertTitle>{t('modelMarket.error.title')}</AlertTitle>
                <AlertDescription className='flex items-center justify-between gap-3'>
                  {t('modelMarket.error.description')}
                  <Button size='sm' variant='outline' onClick={() => query.refetch()}>
                    {t('common.buttons.retry')}
                  </Button>
                </AlertDescription>
              </Alert>
            ) : (
              <section className='space-y-4'>
                <div className='bg-card grid gap-3 rounded-xl border p-4 md:grid-cols-[minmax(220px,1fr)_180px_180px_auto] md:items-end'>
                  <label className='space-y-1.5'>
                    <span className='text-muted-foreground text-xs font-medium'>{t('modelMarket.filters.search')}</span>
                    <div className='relative'>
                      <IconSearch className='text-muted-foreground absolute top-2.5 left-3' size={17} />
                      <Input
                        className='pl-9'
                        value={search}
                        onChange={(event) => setSearch(event.target.value)}
                        placeholder={t('modelMarket.filters.placeholder')}
                      />
                    </div>
                  </label>
                  <label className='space-y-1.5'>
                    <span className='text-muted-foreground text-xs font-medium'>{t('modelMarket.filters.type')}</span>
                    <Select value={type} onValueChange={setType}>
                      <SelectTrigger className='w-full'>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value='all'>{t('modelMarket.filters.all')}</SelectItem>
                        {types.map((value) => (
                          <SelectItem key={value} value={value}>
                            {value}
                          </SelectItem>
                        ))}
                      </SelectContent>
                    </Select>
                  </label>
                  <label className='space-y-1.5'>
                    <span className='text-muted-foreground text-xs font-medium'>{t('modelMarket.filters.sort')}</span>
                    <Select value={sort} onValueChange={setSort}>
                      <SelectTrigger className='w-full'>
                        <SelectValue />
                      </SelectTrigger>
                      <SelectContent>
                        <SelectItem value='name'>{t('modelMarket.sort.name')}</SelectItem>
                        <SelectItem value='price'>{t('modelMarket.sort.price')}</SelectItem>
                      </SelectContent>
                    </Select>
                  </label>
                  <div className='text-muted-foreground pb-2 text-right text-sm'>{t('modelMarket.count', { count: models.length })}</div>
                </div>
                {query.isLoading ? (
                  <div className='grid gap-4 md:grid-cols-2 xl:grid-cols-3'>
                    {[1, 2, 3, 4, 5, 6].map((item) => (
                      <Skeleton key={item} className='h-64 rounded-xl' />
                    ))}
                  </div>
                ) : models.length ? (
                  <div className='grid gap-4 md:grid-cols-2 xl:grid-cols-3'>
                    {models.map((model) => (
                      <Card
                        key={model.id}
                        role='button'
                        tabIndex={0}
                        aria-label={t('modelMarket.openModel', { name: model.name })}
                        className='focus-visible:ring-ring group gap-4 py-5 shadow-none transition-all hover:-translate-y-0.5 hover:border-sky-500/40 hover:shadow-md focus-visible:ring-2 focus-visible:ring-offset-2 focus-visible:outline-none'
                        onClick={() => setSelected(model)}
                        onKeyDown={(event) => {
                          if (event.key === 'Enter' || event.key === ' ') {
                            event.preventDefault();
                            setSelected(model);
                          }
                        }}
                      >
                        <CardHeader className='px-5'>
                          <div className='flex items-start justify-between gap-4'>
                            <div className='min-w-0'>
                              <CardTitle className='truncate'>{model.name}</CardTitle>
                              <CardDescription className='mt-1 truncate font-mono'>{model.modelId}</CardDescription>
                            </div>
                          </div>
                          <div className='flex flex-wrap gap-1.5 pt-2'>
                            <Badge variant='outline'>{model.developer}</Badge>
                            <Badge variant='outline'>{model.modelType}</Badge>
                            {model.capabilities.slice(0, 2).map((item) => (
                              <Badge key={item} variant='outline'>
                                {item}
                              </Badge>
                            ))}
                          </div>
                        </CardHeader>
                        <CardContent className='space-y-4 px-5'>
                          <div className='border-y py-4'>
                            <Price model={model} />
                          </div>
                          <div className='flex items-end justify-between gap-3'>
                            {query.data?.healthVisible ? (
                              <div>
                                <Health health={model.health} />
                                {healthFreshness(model.health?.lastUpdatedAt, i18n.language) && (
                                  <div className='text-muted-foreground mt-1 text-[10px]'>
                                    {t('modelMarket.health.updated', {
                                      date: healthFreshness(model.health?.lastUpdatedAt, i18n.language),
                                    })}
                                  </div>
                                )}
                              </div>
                            ) : (
                              <span className='text-muted-foreground text-xs'>{t('modelMarket.route.healthHidden')}</span>
                            )}
                            <span className='text-muted-foreground group-hover:text-foreground text-xs'>{t('modelMarket.view')}</span>
                          </div>
                        </CardContent>
                      </Card>
                    ))}
                  </div>
                ) : (
                  <div className='rounded-xl border border-dashed py-20 text-center'>
                    <IconBrain className='text-muted-foreground mx-auto mb-3' size={32} />
                    <h3 className='font-semibold'>{t('modelMarket.empty.title')}</h3>
                    <p className='text-muted-foreground mx-auto mt-1 max-w-md text-sm'>{t('modelMarket.empty.description')}</p>
                  </div>
                )}
              </section>
            )}
          </>
        )}
      </Main>
    </>
  );
}
