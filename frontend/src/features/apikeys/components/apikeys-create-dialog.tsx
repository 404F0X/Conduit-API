import { useMemo, useState } from 'react';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { IconCheck, IconClock, IconCoins, IconKey, IconLock, IconSearch, IconSparkles } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { useSelectedProjectId } from '@/stores/projectStore';
import { cn, extractNumberIDAsNumber } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '@/components/ui/form';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { ScopesSelect } from '@/components/scopes-select';
import { useMyModelCatalog } from '@/features/model-market/data';
import { useMyProjects } from '@/features/projects/data/projects';
import { useApiKeysContext } from '../context/apikeys-context';
import { useCreateApiKey } from '../data/apikeys';
import { CreateApiKeyInput, createApiKeyInputSchemaFactory } from '../data/schema';

type KeyType = 'user' | 'service_account';
type AccessMode = 'all' | 'models';
type ChannelAccessMode = 'all' | 'channels';
type PeriodType = 'all_time' | 'past_duration' | 'calendar_duration';
type CalendarUnit = 'day' | 'month';

export function ApiKeysCreateDialog() {
  const { t } = useTranslation();
  const { isDialogOpen, closeDialog, openDialog, setSelectedApiKey } = useApiKeysContext();
  const createApiKey = useCreateApiKey();
  const selectedProjectId = useSelectedProjectId();
  const { data: projects } = useMyProjects();
  const catalog = useMyModelCatalog();
  const [keyType, setKeyType] = useState<KeyType>('user');
  const [accessMode, setAccessMode] = useState<AccessMode>('all');
  const [channelAccessMode, setChannelAccessMode] = useState<ChannelAccessMode>('all');
  const [selectedModels, setSelectedModels] = useState<string[]>([]);
  const [selectedChannels, setSelectedChannels] = useState<string[]>([]);
  const [search, setSearch] = useState('');
  const [quotaEnabled, setQuotaEnabled] = useState(false);
  const [requests, setRequests] = useState('');
  const [tokens, setTokens] = useState('');
  const [cost, setCost] = useState('');
  const [periodType, setPeriodType] = useState<PeriodType>('all_time');
  const [periodValue, setPeriodValue] = useState('24');
  const [calendarUnit, setCalendarUnit] = useState<CalendarUnit>('month');
  const [validFrom, setValidFrom] = useState('');
  const [validUntil, setValidUntil] = useState('');
  const [advancedOpen, setAdvancedOpen] = useState(false);
  const [policyError, setPolicyError] = useState('');

  const localizedSchema = useMemo(() => createApiKeyInputSchemaFactory(t), [t]);
  const form = useForm<CreateApiKeyInput>({
    resolver: zodResolver(localizedSchema),
    defaultValues: { name: '', type: 'user', scopes: undefined },
  });
  const projectLabel = projects?.find((project) => project.id === selectedProjectId)?.name || selectedProjectId || '—';
  const models = useMemo(() => {
    const needle = search.trim().toLowerCase();
    return (catalog.data?.models || []).filter(
      (model) =>
        !needle ||
        `${model.name} ${model.modelId} ${model.routes.map((route) => route.channelName).join(' ')}`.toLowerCase().includes(needle)
    );
  }, [catalog.data, search]);
  const channels = useMemo(() => {
    const byID = new Map<string, string>();
    for (const model of catalog.data?.models || []) {
      for (const route of model.routes) byID.set(route.channelID, route.channelName);
    }
    return [...byID].map(([id, name]) => ({ id, name })).sort((a, b) => a.name.localeCompare(b.name));
  }, [catalog.data]);

  const reset = () => {
    form.reset({ name: '', type: 'user', scopes: undefined });
    setKeyType('user');
    setAccessMode('all');
    setChannelAccessMode('all');
    setSelectedModels([]);
    setSelectedChannels([]);
    setSearch('');
    setQuotaEnabled(false);
    setRequests('');
    setTokens('');
    setCost('');
    setPeriodType('all_time');
    setPeriodValue('24');
    setCalendarUnit('month');
    setValidFrom('');
    setValidUntil('');
    setAdvancedOpen(false);
    setPolicyError('');
  };
  const close = () => {
    reset();
    closeDialog('create');
  };
  const toggle = (value: string, current: string[], setter: (next: string[]) => void) =>
    setter(current.includes(value) ? current.filter((item) => item !== value) : [...current, value]);

  const onSubmit = async (data: CreateApiKeyInput) => {
    setPolicyError('');
    if (accessMode === 'models' && selectedModels.length === 0) return setPolicyError(t('apikeys.access.errors.modelRequired'));
    if (channelAccessMode === 'channels' && selectedChannels.length === 0)
      return setPolicyError(t('apikeys.access.errors.channelRequired'));
    if (quotaEnabled && !requests && !tokens && !cost) return setPolicyError(t('apikeys.validation.quotaAtLeastOneLimit'));
    if (validFrom && validUntil && new Date(validUntil) <= new Date(validFrom))
      return setPolicyError(t('apikeys.access.errors.invalidDates'));
    if (validUntil && new Date(validUntil) <= new Date()) return setPolicyError(t('apikeys.access.errors.expiryInPast'));

    const quota = quotaEnabled
      ? {
          requests: requests ? Number(requests) : null,
          totalTokens: tokens ? Number(tokens) : null,
          cost: cost || null,
          period: {
            type: periodType,
            pastDuration: periodType === 'past_duration' ? { value: Number(periodValue), unit: 'hour' } : null,
            calendarDuration: periodType === 'calendar_duration' ? { unit: calendarUnit } : null,
          },
        }
      : null;
    const profile = {
      name: t('apikeys.access.defaultProfileName'),
      modelMappings: [],
      channelIDs: channelAccessMode === 'channels' ? selectedChannels.map(extractNumberIDAsNumber) : [],
      channelTags: [],
      channelTagsMatchMode: 'any',
      modelIDs: accessMode === 'models' ? selectedModels : [],
      validFrom: validFrom ? new Date(validFrom).toISOString() : null,
      validUntil: validUntil ? new Date(validUntil).toISOString() : null,
      quota,
    };
    try {
      const result = await createApiKey.mutateAsync({
        ...data,
        type: keyType,
        scopes: keyType === 'service_account' ? data.scopes || [] : undefined,
        profiles: { activeProfile: profile.name, profiles: [profile] },
      });
      reset();
      closeDialog('create');
      setSelectedApiKey(result.createAPIKey);
      openDialog('view', result.createAPIKey);
    } catch {
      /* mutation owns error feedback */
    }
  };

  const accessSummary =
    accessMode === 'all' ? t('apikeys.access.summary.all') : t('apikeys.access.summary.models', { count: selectedModels.length });
  const channelSummary =
    channelAccessMode === 'all'
      ? t('apikeys.access.summary.allChannels')
      : t('apikeys.access.summary.channels', { count: selectedChannels.length });
  const periodSummary =
    periodType === 'all_time'
      ? t('apikeys.profiles.quotaPeriodAllTime')
      : periodType === 'past_duration'
        ? t('apikeys.access.summary.rollingPeriod', { value: periodValue })
        : t(`apikeys.access.summary.calendar.${calendarUnit}`);
  const quotaSummary = !quotaEnabled
    ? t('apikeys.access.summary.noQuota')
    : [
        requests && t('apikeys.access.summary.requests', { value: requests }),
        tokens && t('apikeys.access.summary.tokens', { value: tokens }),
        cost && t('apikeys.access.summary.cost', { value: cost }),
        periodSummary,
      ]
        .filter(Boolean)
        .join(' · ');
  const validitySummary = t('apikeys.access.summary.validityRange', {
    from: validFrom ? new Date(validFrom).toLocaleString() : t('apikeys.access.summary.immediately'),
    until: validUntil ? new Date(validUntil).toLocaleString() : t('apikeys.access.summary.never'),
  });
  const selectedScopes = form.watch('scopes') || [];

  return (
    <Dialog open={isDialogOpen.create} onOpenChange={(open) => !open && close()}>
      <DialogContent className='flex max-h-[calc(100vh-2rem)] flex-col gap-0 overflow-hidden p-0 sm:max-w-[920px]'>
        <DialogHeader className='border-b px-5 py-4 pr-12 sm:px-6'>
          <DialogTitle className='flex items-center gap-2 text-balance'>
            <IconKey className='text-primary size-5' />
            {t('apikeys.dialogs.create.title')}
          </DialogTitle>
          <p className='text-muted-foreground text-xs text-pretty'>{t('apikeys.access.boundary', { project: projectLabel })}</p>
        </DialogHeader>
        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className='flex min-h-0 flex-1 flex-col'>
            <div className='grid min-h-0 flex-1 overflow-y-auto lg:grid-cols-[minmax(0,1fr)_280px]'>
              <div className='space-y-6 p-5 sm:p-6'>
                <PolicySection icon={IconKey} title={t('apikeys.access.basics')}>
                  <FormField
                    control={form.control}
                    name='name'
                    render={({ field }) => (
                      <FormItem>
                        <FormLabel>{t('apikeys.dialogs.fields.name.label')}</FormLabel>
                        <FormControl>
                          <Input autoComplete='off' {...field} />
                        </FormControl>
                        <FormMessage />
                      </FormItem>
                    )}
                  />
                  <div role='radiogroup' aria-label={t('apikeys.dialogs.fields.type.label')} className='grid min-w-0 grid-cols-2 gap-2'>
                    {(['user', 'service_account'] as const).map((type) => (
                      <button
                        key={type}
                        type='button'
                        role='radio'
                        aria-checked={keyType === type}
                        onClick={() => {
                          setKeyType(type);
                          form.setValue('type', type);
                        }}
                        className={cn(
                          'flex min-h-10 min-w-0 items-center gap-2 rounded-md border px-3 text-left text-sm transition-[border-color,background-color]',
                          keyType === type ? 'border-primary bg-primary/5 font-medium' : 'hover:bg-muted/50'
                        )}
                      >
                        <span
                          className={cn(
                            'size-4 shrink-0 rounded-full border',
                            keyType === type && 'border-primary shadow-[inset_0_0_0_4px_var(--primary)]'
                          )}
                        />
                        <span className='min-w-0'>
                          <span className='block truncate'>
                            {t(`apikeys.dialogs.fields.type.${type === 'user' ? 'user' : 'serviceAccount'}`)}
                          </span>
                          <span className='text-muted-foreground block truncate text-[11px]'>
                            {type === 'user' ? 'SDK / CLI' : 'CI / Backend'}
                          </span>
                        </span>
                      </button>
                    ))}
                  </div>
                </PolicySection>

                <PolicySection
                  icon={IconSparkles}
                  title={t('apikeys.access.modelsTitle')}
                  description={t('apikeys.access.modelsDescription')}
                >
                  <div className='space-y-2'>
                    <p className='text-xs font-medium'>{t('apikeys.access.channelsTitle')}</p>
                    <div role='radiogroup' aria-label={t('apikeys.access.channelsTitle')} className='grid gap-2 sm:grid-cols-2'>
                      {(['all', 'channels'] as const).map((mode) => (
                        <button
                          key={mode}
                          type='button'
                          role='radio'
                          aria-checked={channelAccessMode === mode}
                          onClick={() => setChannelAccessMode(mode)}
                          className={cn(
                            'min-h-10 rounded-md border px-3 py-2 text-left text-xs transition-[border-color,background-color]',
                            channelAccessMode === mode ? 'border-primary bg-primary/5 text-primary font-medium' : 'hover:bg-muted/50'
                          )}
                        >
                          <span className='flex items-center gap-2'>
                            <span
                              className={cn(
                                'size-3.5 rounded-full border',
                                channelAccessMode === mode && 'border-primary shadow-[inset_0_0_0_3px_var(--primary)]'
                              )}
                            />
                            {t(`apikeys.access.channelMode.${mode}`)}
                          </span>
                        </button>
                      ))}
                    </div>
                    {channelAccessMode === 'channels' && (
                      <div className='max-h-40 space-y-1 overflow-y-auto rounded-md border p-1'>
                        {channels.map((channel) => (
                          <Choice
                            key={channel.id}
                            checked={selectedChannels.includes(channel.id)}
                            onClick={() => toggle(channel.id, selectedChannels, setSelectedChannels)}
                            title={channel.name}
                            meta={t('apikeys.access.channelID', { id: channel.id })}
                          />
                        ))}
                        {!catalog.isLoading && channels.length === 0 && (
                          <p className='text-muted-foreground px-3 py-2 text-xs'>{t('apikeys.access.noChannelsFound')}</p>
                        )}
                      </div>
                    )}
                  </div>
                  <div className='border-t pt-3'>
                    <div role='radiogroup' aria-label={t('apikeys.access.modelsTitle')} className='grid gap-2 sm:grid-cols-2'>
                      {(['all', 'models'] as const).map((mode) => (
                        <button
                          key={mode}
                          type='button'
                          role='radio'
                          aria-checked={accessMode === mode}
                          onClick={() => setAccessMode(mode)}
                          className={cn(
                            'min-h-10 rounded-md border px-3 py-2 text-left text-xs transition-[border-color,background-color]',
                            accessMode === mode ? 'border-primary bg-primary/5 text-primary font-medium' : 'hover:bg-muted/50'
                          )}
                        >
                          <span className='flex items-center gap-2'>
                            <span
                              className={cn(
                                'size-3.5 rounded-full border',
                                accessMode === mode && 'border-primary shadow-[inset_0_0_0_3px_var(--primary)]'
                              )}
                            />
                            {t(`apikeys.access.mode.${mode}`)}
                          </span>
                        </button>
                      ))}
                    </div>
                    {accessMode === 'models' && catalog.isLoading && <p className='text-muted-foreground text-xs'>{t('common.loading')}</p>}
                    {accessMode === 'models' && catalog.isError && (
                      <p className='text-destructive text-xs'>{t('apikeys.access.catalogError')}</p>
                    )}
                    {accessMode === 'models' && (
                      <>
                        <div className='relative'>
                          <IconSearch className='text-muted-foreground absolute top-2.5 left-3 size-4' />
                          <Input
                            value={search}
                            onChange={(event) => setSearch(event.target.value)}
                            className='pl-9'
                            placeholder={t('apikeys.access.searchModels')}
                          />
                        </div>
                        <div className='max-h-56 space-y-1 overflow-y-auto rounded-md border p-1'>
                          {models.map((model) => (
                            <Choice
                              key={model.modelId}
                              checked={selectedModels.includes(model.modelId)}
                              onClick={() => toggle(model.modelId, selectedModels, setSelectedModels)}
                              title={model.name}
                              meta={model.routes.map((route) => route.channelName).join(' · ')}
                            />
                          ))}
                        </div>
                      </>
                    )}
                  </div>
                </PolicySection>

                <PolicySection
                  icon={IconCoins}
                  title={t('apikeys.access.quotaTitle')}
                  description={t('apikeys.access.quotaDescription')}
                  trailing={<Switch checked={quotaEnabled} onCheckedChange={setQuotaEnabled} />}
                >
                  {quotaEnabled && (
                    <div className='space-y-3'>
                      <div className='grid gap-2 sm:grid-cols-3'>
                        <NumberInput label={t('apikeys.profiles.quotaRequests')} value={requests} setValue={setRequests} />
                        <NumberInput label={t('apikeys.profiles.quotaTotalTokens')} value={tokens} setValue={setTokens} />
                        <NumberInput label={t('apikeys.profiles.quotaCost')} value={cost} setValue={setCost} step='0.01' />
                      </div>
                      <label className='block text-xs font-medium'>
                        {t('apikeys.profiles.quotaPeriodType')}
                        <select
                          className='border-input bg-background mt-1 min-h-10 w-full rounded-md border px-3'
                          value={periodType}
                          onChange={(e) => setPeriodType(e.target.value as PeriodType)}
                        >
                          <option value='all_time'>{t('apikeys.profiles.quotaPeriodAllTime')}</option>
                          <option value='past_duration'>{t('apikeys.profiles.quotaPeriodPastDuration')}</option>
                          <option value='calendar_duration'>{t('apikeys.profiles.quotaPeriodCalendarDuration')}</option>
                        </select>
                      </label>
                      {periodType === 'past_duration' && (
                        <NumberInput label={t('apikeys.access.rollingHours')} value={periodValue} setValue={setPeriodValue} />
                      )}
                      {periodType === 'calendar_duration' && (
                        <label className='block text-xs font-medium'>
                          {t('apikeys.access.calendarUnit')}
                          <select
                            className='border-input bg-background mt-1 min-h-10 w-full rounded-md border px-3'
                            value={calendarUnit}
                            onChange={(event) => setCalendarUnit(event.target.value as CalendarUnit)}
                          >
                            <option value='day'>{t('apikeys.profiles.quotaUnitDay')}</option>
                            <option value='month'>{t('apikeys.profiles.quotaUnitMonth')}</option>
                          </select>
                        </label>
                      )}
                    </div>
                  )}
                </PolicySection>

                <PolicySection
                  icon={IconClock}
                  title={t('apikeys.access.validityTitle')}
                  description={t('apikeys.access.validityDescription')}
                >
                  <div className='grid gap-3 sm:grid-cols-2'>
                    <DateInput label={t('apikeys.access.validFrom')} value={validFrom} setValue={setValidFrom} />
                    <DateInput label={t('apikeys.access.validUntil')} value={validUntil} setValue={setValidUntil} />
                  </div>
                </PolicySection>

                {keyType === 'service_account' && (
                  <details
                    open={advancedOpen}
                    onToggle={(event) => setAdvancedOpen(event.currentTarget.open)}
                    className='rounded-md border'
                  >
                    <summary className='cursor-pointer px-4 py-3 text-sm font-medium'>{t('apikeys.access.advancedScopes')}</summary>
                    <div className='border-t p-4'>
                      <FormField
                        control={form.control}
                        name='scopes'
                        render={({ field }) => (
                          <FormItem>
                            <FormControl>
                              <ScopesSelect value={field.value || []} onChange={field.onChange} level='system' enablePermissionFilter />
                            </FormControl>
                          </FormItem>
                        )}
                      />
                    </div>
                  </details>
                )}
                {policyError && (
                  <p role='alert' className='text-destructive text-sm'>
                    {policyError}
                  </p>
                )}
              </div>

              <aside className='bg-muted/25 border-t p-5 lg:border-t-0 lg:border-l'>
                <div className='lg:sticky lg:top-5'>
                  <h3 className='flex items-center gap-2 text-sm font-semibold'>
                    <IconLock className='text-primary size-4' />
                    {t('apikeys.access.summaryTitle')}
                  </h3>
                  <dl className='mt-4 space-y-4 text-xs'>
                    <Summary label={t('apikeys.dialogs.create.summary.project')} value={projectLabel} />
                    <Summary label={t('apikeys.access.modelsTitle')} value={accessSummary} />
                    <Summary label={t('apikeys.access.channelsTitle')} value={channelSummary} />
                    <Summary label={t('apikeys.access.quotaTitle')} value={quotaSummary} />
                    <Summary label={t('apikeys.access.validityTitle')} value={validitySummary} />
                    {keyType === 'service_account' && (
                      <Summary
                        label={t('apikeys.dialogs.fields.scopes.label')}
                        value={selectedScopes.length ? selectedScopes.join(', ') : t('apikeys.access.summary.noScopes')}
                      />
                    )}
                  </dl>
                  <p className='text-muted-foreground mt-5 border-t pt-4 text-xs leading-relaxed'>{t('apikeys.access.intersectionNote')}</p>
                </div>
              </aside>
            </div>
            <DialogFooter className='bg-background shrink-0 border-t px-5 py-4 sm:px-6'>
              <Button type='button' variant='outline' onClick={close}>
                {t('common.buttons.cancel')}
              </Button>
              <Button type='submit' disabled={createApiKey.isPending || !selectedProjectId}>
                {createApiKey.isPending ? t('common.buttons.creating') : t('apikeys.createApiKey')}
              </Button>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  );
}

function PolicySection({
  icon: Icon,
  title,
  description,
  trailing,
  children,
}: {
  icon: typeof IconKey;
  title: string;
  description?: string;
  trailing?: React.ReactNode;
  children: React.ReactNode;
}) {
  return (
    <section className='space-y-3 border-b pb-6 last:border-0 last:pb-0'>
      <div className='flex items-start gap-2'>
        <Icon className='text-primary mt-0.5 size-4 shrink-0' />
        <div className='min-w-0 flex-1'>
          <h3 className='text-sm font-semibold'>{title}</h3>
          {description && <p className='text-muted-foreground mt-0.5 text-xs text-pretty'>{description}</p>}
        </div>
        {trailing}
      </div>
      {children}
    </section>
  );
}
function Choice({ checked, onClick, title, meta }: { checked: boolean; onClick: () => void; title: string; meta: string }) {
  return (
    <button
      type='button'
      aria-pressed={checked}
      aria-label={title}
      onClick={onClick}
      className={cn(
        'flex min-h-10 w-full items-center gap-3 rounded-md border px-3 py-2 text-left transition-[border-color,background-color]',
        checked ? 'border-primary bg-primary/5' : 'hover:bg-muted/50'
      )}
    >
      <span
        aria-hidden='true'
        className={cn('flex size-4 shrink-0 items-center justify-center rounded-sm border', checked && 'border-primary bg-primary')}
      >
        {checked && <IconCheck className='text-primary-foreground size-3' />}
      </span>
      <span className='min-w-0'>
        <span className='block truncate text-xs font-medium'>{title}</span>
        <span className='text-muted-foreground block truncate font-mono text-[10px]'>{meta}</span>
      </span>
    </button>
  );
}
function NumberInput({
  label,
  value,
  setValue,
  step = '1',
}: {
  label: string;
  value: string;
  setValue: (value: string) => void;
  step?: string;
}) {
  return (
    <label className='block text-xs font-medium'>
      {label}
      <Input type='number' min='0' step={step} value={value} onChange={(e) => setValue(e.target.value)} className='mt-1 tabular-nums' />
    </label>
  );
}
function DateInput({ label, value, setValue }: { label: string; value: string; setValue: (value: string) => void }) {
  return (
    <label className='block text-xs font-medium'>
      {label}
      <Input type='datetime-local' value={value} onChange={(e) => setValue(e.target.value)} className='mt-1 tabular-nums' />
    </label>
  );
}
function Summary({ label, value }: { label: string; value: string }) {
  return (
    <div>
      <dt className='text-muted-foreground'>{label}</dt>
      <dd className='mt-1 font-medium break-words'>{value || '—'}</dd>
    </div>
  );
}
