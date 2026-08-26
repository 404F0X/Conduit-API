import { useMemo, useRef, useState } from 'react';
import { ArrowRight, Check, ChevronDown, Database, Loader2, Search, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Switch } from '@/components/ui/switch';
import { Textarea } from '@/components/ui/textarea';
import { useModels } from '../context/models-context';
import { type UpstreamModelDeployment, useCommercializationCatalog, useCreatePublicModelWithRoutes } from '../data/commercialization';
import { DEVELOPER_ICONS, DEVELOPER_IDS } from '../data/constants';
import { useDevelopersData } from '../data/providers';
import { type Provider, type ProviderModel, resolveVision } from '../data/providers.schema';
import { modelAssociationSchema, type CreateModelInput, type ModelAssociation, type ModelType } from '../data/schema';

const MODEL_TYPES: ModelType[] = ['chat', 'embedding', 'rerank', 'image_generation', 'video_generation'];
type FieldErrors = Partial<Record<'publicID' | 'name' | 'developer' | 'associations', string>>;

export function ModelsCreateDialog() {
  const { t } = useTranslation();
  const { open, setOpen } = useModels();
  const catalog = useCommercializationCatalog(open === 'create');
  const create = useCreatePublicModelWithRoutes();
  const { data: developersData } = useDevelopersData();
  const [search, setSearch] = useState('');
  const [selectedIDs, setSelectedIDs] = useState<string[]>([]);
  const [publicID, setPublicID] = useState('');
  const [name, setName] = useState('');
  const [developer, setDeveloper] = useState('');
  const [modelType, setModelType] = useState<ModelType>('chat');
  const [enabled, setEnabled] = useState(true);
  const [confirmed, setConfirmed] = useState(false);
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [icon, setIcon] = useState('');
  const [group, setGroup] = useState('');
  const [remark, setRemark] = useState('');
  const [toolCall, setToolCall] = useState(false);
  const [vision, setVision] = useState(false);
  const [temperature, setTemperature] = useState(false);
  const [reasoning, setReasoning] = useState(false);
  const [reasoningDefault, setReasoningDefault] = useState(false);
  const [inputModalities, setInputModalities] = useState<string[]>(['text']);
  const [outputModalities, setOutputModalities] = useState<string[]>(['text']);
  const [costInput, setCostInput] = useState('0');
  const [costOutput, setCostOutput] = useState('0');
  const [cacheRead, setCacheRead] = useState('0');
  const [cacheWrite, setCacheWrite] = useState('0');
  const [contextLimit, setContextLimit] = useState('0');
  const [outputLimit, setOutputLimit] = useState('0');
  const [knowledge, setKnowledge] = useState('');
  const [releaseDate, setReleaseDate] = useState('');
  const [lastUpdated, setLastUpdated] = useState('');
  const [associationJSON, setAssociationJSON] = useState('[]');
  const [errors, setErrors] = useState<FieldErrors>({});
  const publicIDRef = useRef<HTMLInputElement>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  const developerRef = useRef<HTMLInputElement>(null);

  const deployments = catalog.data?.upstreamModelDeployments ?? [];
  const selected = selectedIDs
    .map((id) => deployments.find((deployment) => deployment.id === id))
    .filter((deployment): deployment is UpstreamModelDeployment => Boolean(deployment));
  const filtered = deployments.filter((deployment) => {
    const needle = search.trim().toLowerCase();
    return (
      !needle ||
      [deployment.upstreamModelID, deployment.channelName, deployment.variant, deployment.source].some((value) =>
        value.toLowerCase().includes(needle)
      )
    );
  });

  const providerModels = useMemo(() => {
    if (!developersData) return [];
    return Object.entries(developersData.providers).flatMap(([providerID, provider]) =>
      ((provider as Provider).models ?? []).map((model) => ({ providerID, model }))
    );
  }, [developersData]);

  const applyMetadata = (upstreamModelID: string) => {
    const matches = providerModels.filter(({ model }) => model.id === upstreamModelID);
    if (matches.length !== 1) return;
    const { providerID, model } = matches[0] as { providerID: string; model: ProviderModel };
    setDeveloper(providerID);
    setIcon(DEVELOPER_ICONS[providerID] || providerID);
    setGroup(model.family || providerID);
    setName(model.display_name || model.name || upstreamModelID);
    const normalizedType = model.type?.replace(/-/g, '_') as ModelType | undefined;
    if (normalizedType && MODEL_TYPES.includes(normalizedType)) setModelType(normalizedType);
    setToolCall(Boolean(model.tool_call));
    setVision(resolveVision(model));
    setTemperature(Boolean(model.temperature));
    setReasoning(Boolean(model.reasoning?.supported));
    setReasoningDefault(Boolean(model.reasoning?.default));
    setInputModalities(model.modalities?.input?.length ? model.modalities.input : ['text']);
    setOutputModalities(model.modalities?.output?.length ? model.modalities.output : ['text']);
    setCostInput(String(model.cost?.input ?? 0));
    setCostOutput(String(model.cost?.output ?? 0));
    setCacheRead(String(model.cost?.cache_read ?? 0));
    setCacheWrite(String(model.cost?.cache_write ?? 0));
    setContextLimit(String(model.limit?.context ?? 0));
    setOutputLimit(String(model.limit?.output ?? 0));
    setKnowledge(model.knowledge ?? '');
    setReleaseDate(model.release_date ?? '');
    setLastUpdated(model.last_updated ?? '');
  };

  const toggleDeployment = (deployment: UpstreamModelDeployment) => {
    if (deployment.status !== 'ENABLED') return;
    if (selectedIDs.includes(deployment.id)) {
      setConfirmed(false);
      setSelectedIDs((ids) => ids.filter((id) => id !== deployment.id));
      return;
    }
    if (selected.some((item) => item.channelID === deployment.channelID)) {
      toast.error(t('models.createFlow.sameChannelError'));
      return;
    }
    setConfirmed(false);
    setSelectedIDs((ids) => [...ids, deployment.id]);
    if (selectedIDs.length === 0) {
      setPublicID(deployment.upstreamModelID);
      setName(deployment.upstreamModelID);
      applyMetadata(deployment.upstreamModelID);
    }
  };

  const resetForm = () => {
    setSearch('');
    setSelectedIDs([]);
    setPublicID('');
    setName('');
    setDeveloper('');
    setModelType('chat');
    setEnabled(true);
    setConfirmed(false);
    setAdvancedOpen(false);
    setIcon('');
    setGroup('');
    setRemark('');
    setToolCall(false);
    setVision(false);
    setTemperature(false);
    setReasoning(false);
    setReasoningDefault(false);
    setInputModalities(['text']);
    setOutputModalities(['text']);
    setCostInput('0');
    setCostOutput('0');
    setCacheRead('0');
    setCacheWrite('0');
    setContextLimit('0');
    setOutputLimit('0');
    setKnowledge('');
    setReleaseDate('');
    setLastUpdated('');
    setAssociationJSON('[]');
    setErrors({});
  };

  const close = () => {
    if (!create.isPending) {
      setOpen(null);
      resetForm();
    }
  };

  const submit = async () => {
    const nextErrors: FieldErrors = {};
    if (!publicID.trim()) nextErrors.publicID = t('models.createFlow.publicIDRequired');
    if (!name.trim()) nextErrors.name = t('models.createFlow.nameRequired');
    if (!developer.trim()) nextErrors.developer = t('models.createFlow.developerRequired');
    let associations: ModelAssociation[] = [];
    try {
      const value: unknown = JSON.parse(associationJSON);
      if (!Array.isArray(value)) throw new Error('not an array');
      associations = value.map((association) => modelAssociationSchema.parse(association));
    } catch {
      nextErrors.associations = t('models.createFlow.associationsInvalid');
    }
    setErrors(nextErrors);
    if (!selected.length) {
      toast.error(t('models.createFlow.selectUpstreamRequired'));
      return;
    }
    if (Object.keys(nextErrors).length) {
      if (nextErrors.publicID) publicIDRef.current?.focus();
      else if (nextErrors.name) nameRef.current?.focus();
      else if (nextErrors.developer) developerRef.current?.focus();
      else setAdvancedOpen(true);
      return;
    }
    if (selected.length > 1 && !confirmed) {
      toast.error(t('models.createFlow.compatibilityRequired'));
      return;
    }
    const model: CreateModelInput = {
      developer,
      modelID: publicID.trim(),
      type: modelType,
      name: name.trim(),
      icon: icon || DEVELOPER_ICONS[developer] || developer,
      group: group || developer,
      remark: remark || undefined,
      modelCard: {
        toolCall,
        vision,
        temperature,
        reasoning: { supported: reasoning, default: reasoningDefault },
        modalities: { input: inputModalities, output: outputModalities },
        cost: {
          input: Number(costInput) || 0,
          output: Number(costOutput) || 0,
          cacheRead: Number(cacheRead) || 0,
          cacheWrite: Number(cacheWrite) || 0,
        },
        limit: { context: Number(contextLimit) || 0, output: Number(outputLimit) || 0 },
        knowledge: knowledge || undefined,
        releaseDate: releaseDate || undefined,
        lastUpdated: lastUpdated || undefined,
      },
      settings: { associations },
    };
    try {
      await create.mutateAsync({ model, deploymentIDs: selectedIDs, enabled, confirmCompatibility: confirmed });
      toast.success(t('models.messages.createSuccess'));
      setOpen(null);
      resetForm();
    } catch (error) {
      const message = error instanceof Error ? error.message : t('models.createFlow.createError');
      if (/model id already exists/i.test(message)) {
        setErrors((current) => ({ ...current, publicID: t('models.createFlow.publicIDConflict') }));
        publicIDRef.current?.focus();
      } else if (/display name already exists/i.test(message)) {
        setErrors((current) => ({ ...current, name: t('models.createFlow.nameConflict') }));
        nameRef.current?.focus();
      } else {
        toast.error(message);
      }
    }
  };

  return (
    <Dialog open={open === 'create'} onOpenChange={(value) => !value && close()}>
      <DialogContent className='flex max-h-[92vh] flex-col overflow-hidden sm:max-w-5xl'>
        <DialogHeader className='text-left'>
          <DialogTitle>{t('models.dialogs.create.title')}</DialogTitle>
          <DialogDescription>{t('models.createFlow.description')}</DialogDescription>
        </DialogHeader>
        <div className='min-h-0 flex-1 space-y-6 overflow-y-auto pr-2'>
          <section className='grid gap-4 border-t pt-5 lg:grid-cols-[minmax(0,1.35fr)_minmax(280px,.65fr)]'>
            <div className='space-y-3'>
              <Step number='1' title={t('models.createFlow.upstreamTitle')} description={t('models.createFlow.upstreamDescription')} />
              <div className='relative'>
                <Search className='text-muted-foreground absolute top-2.5 left-3 size-4' />
                <Input
                  value={search}
                  onChange={(event) => setSearch(event.target.value)}
                  className='pl-9'
                  placeholder={t('models.createFlow.searchPlaceholder')}
                />
              </div>
              <div className='max-h-64 divide-y overflow-y-auto rounded-md border'>
                {catalog.isLoading && (
                  <div className='text-muted-foreground flex items-center gap-2 p-4 text-sm'>
                    <Loader2 className='size-4 animate-spin' />
                    {t('models.createFlow.loading')}
                  </div>
                )}
                {catalog.isError && <div className='text-destructive p-4 text-sm'>{t('models.createFlow.loadError')}</div>}
                {!catalog.isLoading && !filtered.length && (
                  <div className='text-muted-foreground p-4 text-sm'>{t('models.createFlow.empty')}</div>
                )}
                {filtered.map((deployment) => {
                  const checked = selectedIDs.includes(deployment.id);
                  const channelSelection = selected.find((item) => item.channelID === deployment.channelID && item.id !== deployment.id);
                  const unavailable = deployment.status !== 'ENABLED';
                  const disabled = unavailable || Boolean(channelSelection);
                  return (
                    <button
                      key={deployment.id}
                      type='button'
                      disabled={disabled}
                      aria-pressed={checked}
                      aria-label={`${deployment.channelName}, ${deployment.upstreamModelID}, ${checked ? t('models.createFlow.selected') : t('models.createFlow.notSelected')}`}
                      onClick={() => toggleDeployment(deployment)}
                      className='hover:bg-muted/50 focus-visible:ring-ring flex w-full items-start gap-3 px-3 py-3 text-left transition-colors focus-visible:ring-2 focus-visible:outline-none disabled:cursor-not-allowed disabled:opacity-50'
                    >
                      <span
                        className={`mt-0.5 flex size-5 items-center justify-center rounded border ${checked ? 'bg-primary border-primary text-primary-foreground' : 'border-input'}`}
                      >
                        {checked && <Check className='size-3.5' />}
                      </span>
                      <span className='min-w-0 flex-1'>
                        <span className='block truncate font-mono text-sm font-medium'>{deployment.upstreamModelID}</span>
                        <span className='text-muted-foreground mt-1 flex flex-wrap items-center gap-1.5 text-xs'>
                          <Badge variant='outline'>{deployment.channelName}</Badge>
                          <span>{deployment.source}</span>
                          {deployment.variant && <span>· {deployment.variant}</span>}
                          {channelSelection && (
                            <span className='text-amber-700 dark:text-amber-400'>
                              {t('models.createFlow.channelAlreadySelected', { model: channelSelection.upstreamModelID })}
                            </span>
                          )}
                        </span>
                      </span>
                      {unavailable && <Badge variant='secondary'>{t('models.createFlow.disabled')}</Badge>}
                    </button>
                  );
                })}
              </div>
            </div>
            <div className='bg-muted/30 rounded-md border p-4'>
              <p className='text-sm font-medium'>{t('models.createFlow.selectedRoutes')}</p>
              <div className='mt-3 space-y-2'>
                {!selected.length && <p className='text-muted-foreground text-sm'>{t('models.createFlow.noneSelected')}</p>}
                {selected.map((deployment) => (
                  <div key={deployment.id} className='bg-background flex min-w-0 items-center gap-2 rounded border px-2.5 py-2 text-xs'>
                    <Badge variant='outline'>{deployment.channelName}</Badge>
                    <span className='min-w-0 flex-1 truncate font-mono'>{deployment.upstreamModelID}</span>
                    <Button
                      type='button'
                      variant='ghost'
                      size='icon'
                      className='size-6 shrink-0'
                      onClick={() => toggleDeployment(deployment)}
                      aria-label={t('models.createFlow.removeRoute', {
                        channel: deployment.channelName,
                        model: deployment.upstreamModelID,
                      })}
                    >
                      <X className='size-3.5' />
                    </Button>
                  </div>
                ))}
              </div>
            </div>
          </section>

          <section className='space-y-4 border-t pt-5'>
            <Step number='2' title={t('models.createFlow.publicTitle')} description={t('models.createFlow.publicDescription')} />
            <div className='grid gap-4 sm:grid-cols-2'>
              <Field label={t('models.createFlow.publicID')} htmlFor='public-model-id' error={errors.publicID}>
                <Input
                  id='public-model-id'
                  ref={publicIDRef}
                  className='font-mono'
                  value={publicID}
                  aria-invalid={Boolean(errors.publicID)}
                  onChange={(event) => {
                    setPublicID(event.target.value);
                    setErrors((current) => ({ ...current, publicID: undefined }));
                  }}
                />
              </Field>
              <Field label={t('models.fields.name')} htmlFor='public-model-name' error={errors.name}>
                <Input
                  id='public-model-name'
                  ref={nameRef}
                  value={name}
                  aria-invalid={Boolean(errors.name)}
                  onChange={(event) => {
                    setName(event.target.value);
                    setErrors((current) => ({ ...current, name: undefined }));
                  }}
                />
              </Field>
              <Field label={t('models.fields.developer')} htmlFor='public-model-developer' error={errors.developer}>
                <Input
                  id='public-model-developer'
                  ref={developerRef}
                  list='public-model-developers'
                  value={developer}
                  aria-invalid={Boolean(errors.developer)}
                  placeholder={t('models.createFlow.developerPlaceholder')}
                  onChange={(event) => {
                    const value = event.target.value;
                    setDeveloper(value);
                    if (!icon) setIcon(DEVELOPER_ICONS[value] || value);
                    if (!group) setGroup(value);
                    setErrors((current) => ({ ...current, developer: undefined }));
                  }}
                />
                <datalist id='public-model-developers'>
                  {DEVELOPER_IDS.map((id) => (
                    <option key={id} value={id} />
                  ))}
                </datalist>
              </Field>
              <Field label={t('models.fields.type')}>
                <Select value={modelType} onValueChange={(value) => setModelType(value as ModelType)}>
                  <SelectTrigger>
                    <SelectValue />
                  </SelectTrigger>
                  <SelectContent>
                    {MODEL_TYPES.map((type) => (
                      <SelectItem key={type} value={type}>
                        {t(`models.types.${type}`)}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              </Field>
            </div>
            <div className='flex items-center justify-between rounded-md border px-3 py-3'>
              <div>
                <Label htmlFor='model-enabled'>{t('models.createFlow.enableNow')}</Label>
                <p className='text-muted-foreground mt-0.5 text-xs'>{t('models.createFlow.enableNowHint')}</p>
              </div>
              <Switch id='model-enabled' checked={enabled} onCheckedChange={setEnabled} />
            </div>
            {selected.length > 1 && (
              <label className='flex items-start gap-3 rounded-md border border-amber-500/50 bg-amber-500/10 p-3 text-sm'>
                <Checkbox checked={confirmed} onCheckedChange={(value) => setConfirmed(value === true)} className='mt-0.5' />
                <span>
                  <span className='font-medium'>{t('models.createFlow.compatibilityTitle')}</span>
                  <span className='text-muted-foreground mt-1 block'>{t('models.createFlow.compatibilityDescription')}</span>
                </span>
              </label>
            )}
          </section>

          <section className='space-y-3 border-t pt-5'>
            <Step number='3' title={t('models.createFlow.previewTitle')} description={t('models.createFlow.previewDescription')} />
            <div className='bg-muted/30 overflow-hidden rounded-md border font-mono text-sm'>
              <div className='flex flex-wrap items-center gap-2 border-b px-4 py-3'>
                <span className='text-muted-foreground'>{t('models.createFlow.clientRequest')}</span>
                <Badge className='font-mono'>{publicID || 'public-model-id'}</Badge>
                <ArrowRight className='text-muted-foreground size-4' />
                <span>{t('models.createFlow.runtimeRoutes')}</span>
              </div>
              <div className='space-y-2 p-4'>
                {selected.length ? (
                  selected.map((deployment) => (
                    <div key={deployment.id} className='flex min-w-0 items-center gap-2'>
                      <Database className='text-primary size-4 shrink-0' />
                      <Badge variant='outline'>{deployment.channelName}</Badge>
                      <span className='truncate'>{deployment.upstreamModelID}</span>
                    </div>
                  ))
                ) : (
                  <span className='text-muted-foreground'>{t('models.createFlow.previewEmpty')}</span>
                )}
              </div>
            </div>
            <p className='text-muted-foreground text-xs'>{t('models.createFlow.authorizationHint')}</p>
          </section>

          <details open={advancedOpen} onToggle={(event) => setAdvancedOpen(event.currentTarget.open)} className='group border-t pt-5'>
            <summary className='focus-visible:ring-ring flex cursor-pointer list-none items-center justify-between rounded-sm py-1 text-sm font-medium focus-visible:ring-2 focus-visible:outline-none'>
              <span>{t('models.createFlow.advancedTitle')}</span>
              <ChevronDown className='size-4 transition-transform group-open:rotate-180' />
            </summary>
            <p className='text-muted-foreground mt-1 text-xs'>{t('models.createFlow.advancedDescription')}</p>
            <div className='mt-4 grid gap-4 sm:grid-cols-2'>
              <Field label={t('models.fields.icon')}>
                <Input value={icon} onChange={(event) => setIcon(event.target.value)} />
              </Field>
              <Field label={t('models.fields.group')}>
                <Input value={group} onChange={(event) => setGroup(event.target.value)} />
              </Field>
              <Toggle label={t('models.modelCard.toolCall')} checked={toolCall} onChange={setToolCall} />
              <Toggle label={t('models.modelCard.vision')} checked={vision} onChange={setVision} />
              <Toggle label={t('models.modelCard.temperature')} checked={temperature} onChange={setTemperature} />
              <Toggle label={t('models.modelCard.reasoningSupported')} checked={reasoning} onChange={setReasoning} />
              <Toggle label={t('models.createFlow.reasoningDefault')} checked={reasoningDefault} onChange={setReasoningDefault} />
              <ModalityField label={t('models.createFlow.inputModalities')} value={inputModalities} onChange={setInputModalities} />
              <ModalityField label={t('models.createFlow.outputModalities')} value={outputModalities} onChange={setOutputModalities} />
              <div className='sm:col-span-2'>
                <p className='text-sm font-medium'>{t('models.createFlow.displayCost')}</p>
                <p className='text-muted-foreground text-xs'>{t('models.modelCard.costHint')}</p>
              </div>
              <NumberField label={t('models.modelCard.input')} value={costInput} onChange={setCostInput} />
              <NumberField label={t('models.modelCard.output')} value={costOutput} onChange={setCostOutput} />
              <NumberField label={t('models.modelCard.cacheRead')} value={cacheRead} onChange={setCacheRead} />
              <NumberField label={t('models.modelCard.cacheWrite')} value={cacheWrite} onChange={setCacheWrite} />
              <NumberField label={t('models.modelCard.context')} value={contextLimit} onChange={setContextLimit} />
              <NumberField label={t('models.columns.maxOutput')} value={outputLimit} onChange={setOutputLimit} />
              <Field label={t('models.modelCard.knowledge')}>
                <Input value={knowledge} onChange={(event) => setKnowledge(event.target.value)} />
              </Field>
              <Field label={t('models.modelCard.releaseDate')}>
                <Input type='date' value={releaseDate} onChange={(event) => setReleaseDate(event.target.value)} />
              </Field>
              <Field label={t('models.modelCard.lastUpdated')}>
                <Input type='date' value={lastUpdated} onChange={(event) => setLastUpdated(event.target.value)} />
              </Field>
              <div className='sm:col-span-2'>
                <Field label={t('models.fields.remark')}>
                  <Textarea value={remark} onChange={(event) => setRemark(event.target.value)} />
                </Field>
              </div>
              <div className='space-y-2 border-t pt-4 sm:col-span-2'>
                <div className='flex items-center justify-between gap-3'>
                  <div>
                    <Label htmlFor='model-associations'>{t('models.createFlow.associationsTitle')}</Label>
                    <p className='text-muted-foreground mt-0.5 text-xs'>{t('models.createFlow.associationsDescription')}</p>
                  </div>
                  <Badge variant='outline'>
                    {t('models.createFlow.associationCount', {
                      count: (() => {
                        try {
                          const parsed: unknown = JSON.parse(associationJSON);
                          return Array.isArray(parsed) ? parsed.length : 0;
                        } catch {
                          return 0;
                        }
                      })(),
                    })}
                  </Badge>
                </div>
                <Textarea
                  id='model-associations'
                  className='min-h-28 font-mono text-xs'
                  value={associationJSON}
                  aria-invalid={Boolean(errors.associations)}
                  onChange={(event) => {
                    setAssociationJSON(event.target.value);
                    setErrors((current) => ({ ...current, associations: undefined }));
                  }}
                />
                {errors.associations && <p className='text-destructive text-xs'>{errors.associations}</p>}
                <p className='text-muted-foreground text-xs'>{t('models.createFlow.associationsExample')}</p>
              </div>
            </div>
          </details>
        </div>
        <DialogFooter className='border-t pt-4'>
          <Button type='button' variant='outline' onClick={close}>
            {t('common.buttons.cancel')}
          </Button>
          <Button type='button' onClick={submit} disabled={create.isPending}>
            {create.isPending && <Loader2 className='mr-2 size-4 animate-spin' />}
            {t('models.createFlow.create')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

function Step({ number, title, description }: { number: string; title: string; description: string }) {
  return (
    <div className='flex gap-3'>
      <span className='bg-primary text-primary-foreground flex size-6 shrink-0 items-center justify-center rounded text-xs font-semibold'>
        {number}
      </span>
      <div>
        <h3 className='text-sm font-semibold'>{title}</h3>
        <p className='text-muted-foreground mt-0.5 text-xs'>{description}</p>
      </div>
    </div>
  );
}

function Field({ label, children, htmlFor, error }: { label: string; children: React.ReactNode; htmlFor?: string; error?: string }) {
  return (
    <div className='space-y-1.5'>
      <Label htmlFor={htmlFor}>{label}</Label>
      {children}
      {error && <p className='text-destructive text-xs'>{error}</p>}
    </div>
  );
}

function NumberField({ label, value, onChange }: { label: string; value: string; onChange: (value: string) => void }) {
  return (
    <Field label={label}>
      <Input type='number' min='0' step='any' value={value} onChange={(event) => onChange(event.target.value)} />
    </Field>
  );
}

function Toggle({ label, checked, onChange }: { label: string; checked: boolean; onChange: (checked: boolean) => void }) {
  return (
    <div className='flex items-center justify-between rounded-md border px-3 py-2'>
      <Label>{label}</Label>
      <Switch checked={checked} onCheckedChange={onChange} />
    </div>
  );
}

function ModalityField({ label, value, onChange }: { label: string; value: string[]; onChange: (value: string[]) => void }) {
  return (
    <div className='space-y-2'>
      <Label>{label}</Label>
      <div className='flex flex-wrap gap-3'>
        {['text', 'image', 'audio', 'video'].map((item) => (
          <label key={item} className='flex items-center gap-1.5 text-sm'>
            <Checkbox
              checked={value.includes(item)}
              onCheckedChange={(checked) => onChange(checked ? [...value, item] : value.filter((entry) => entry !== item))}
            />
            {item}
          </label>
        ))}
      </div>
    </div>
  );
}
