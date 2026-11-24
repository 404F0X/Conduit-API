import { useEffect, useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { RadioGroup, RadioGroupItem } from '@/components/ui/radio-group';
import { ScrollArea } from '@/components/ui/scroll-area';
import { localExpiryDate, type SimpleTokenForm } from '../data/tokens';

type ExpiryChoice = 'never' | '30' | '90' | 'custom';

/**
 * 新建 / 编辑令牌：只有四个字段（名称、额度、有效期、可用模型）。
 * 新建时明确选模型组；编辑保留未修改的配置，高级配置仅允许改名。
 */
export function TokenFormDialog({
  open,
  onOpenChange,
  mode,
  initial,
  nameOnly = false,
  defaultName,
  models,
  modelGroupID,
  groups,
  onModelGroupChange,
  catalogReady,
  catalogError,
  creditLabel,
  submitting,
  onSubmit,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  mode: 'create' | 'edit';
  initial?: SimpleTokenForm;
  /** 令牌用了完整模式的高级配置时，简单模式只允许改名，避免把高级配置覆盖掉 */
  nameOnly?: boolean;
  defaultName: string;
  models: { modelId: string; name: string }[];
  modelGroupID: string;
  groups: { id: string; name: string }[];
  onModelGroupChange: (id: string) => void;
  catalogReady: boolean;
  catalogError: boolean;
  creditLabel: string;
  submitting: boolean;
  onSubmit: (form: SimpleTokenForm) => void;
}) {
  const { t } = useTranslation();
  const [name, setName] = useState(defaultName);
  const [quotaUnlimited, setQuotaUnlimited] = useState(true);
  const [quota, setQuota] = useState('');
  const [expiryChanged, setExpiryChanged] = useState(false);
  const [expiry, setExpiry] = useState<ExpiryChoice>('never');
  const [customDate, setCustomDate] = useState('');
  const [allModels, setAllModels] = useState(true);
  const [modelIds, setModelIds] = useState<string[]>([]);
  const [modelSearch, setModelSearch] = useState('');

  useEffect(() => {
    if (!open) return;
    setExpiryChanged(false);
    setName(initial?.name ?? defaultName);
    setQuotaUnlimited(initial?.quota == null);
    setQuota(initial?.quota != null ? String(initial.quota) : '');
    if (initial?.expiresAt) {
      setExpiry('custom');
      setCustomDate(localExpiryDate(initial.expiresAt));
    } else {
      setExpiry('never');
      setCustomDate('');
    }
    setAllModels(!initial || initial.modelIds.length === 0);
    setModelIds(initial?.modelIds ?? []);
    setModelSearch('');
  }, [open, initial, defaultName]);

  useEffect(() => {
    if (mode === 'create') {
      setModelIds([]);
      setAllModels(true);
    }
  }, [modelGroupID, mode]);

  const filteredModels = useMemo(() => {
    const keyword = modelSearch.trim().toLowerCase();
    return keyword ? models.filter((m) => m.modelId.toLowerCase().includes(keyword) || m.name.toLowerCase().includes(keyword)) : models;
  }, [models, modelSearch]);

  const quotaNumber = Number(quota);
  const quotaInvalid = !quotaUnlimited && (!quota.trim() || !Number.isFinite(quotaNumber) || quotaNumber < 0);
  const customInvalid =
    (mode === 'create' || expiryChanged) &&
    expiry === 'custom' &&
    (!customDate || new Date(`${customDate}T23:59:59`).getTime() <= Date.now());
  const modelsInvalid = !allModels && modelIds.length === 0;
  const canSubmit =
    name.trim().length > 0 &&
    (nameOnly || (!quotaInvalid && !customInvalid && !modelsInvalid)) &&
    (mode === 'edit' || (!!modelGroupID && catalogReady && groups.some((group) => group.id === modelGroupID))) &&
    !submitting;

  const submit = () => {
    if (!canSubmit) return;
    let expiresAt: Date | null = null;
    if (expiry === '30' || expiry === '90') {
      expiresAt = new Date(Date.now() + Number(expiry) * 24 * 60 * 60 * 1000);
    } else if (expiry === 'custom') {
      expiresAt = new Date(`${customDate}T23:59:59`);
    }
    onSubmit({
      expiryChanged: mode === 'create' || expiryChanged,
      modelGroupID,
      name: name.trim(),
      quota: quotaUnlimited ? null : quotaNumber,
      expiresAt: expiryChanged ? expiresAt : (initial?.expiresAt ?? expiresAt),
      modelIds: allModels ? [] : modelIds,
    });
  };

  const toggleModel = (modelId: string, checked: boolean) =>
    setModelIds((current) => (checked ? [...current, modelId] : current.filter((id) => id !== modelId)));

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='sm:max-w-lg'>
        <DialogHeader>
          <DialogTitle>{mode === 'create' ? t('simple.tokens.form.createTitle') : t('simple.tokens.form.editTitle')}</DialogTitle>
          <DialogDescription>{t('simple.tokens.form.description')}</DialogDescription>
        </DialogHeader>

        <div className='space-y-5'>
          <div className='space-y-2'>
            <Label htmlFor='simple-token-name'>{t('simple.tokens.form.name')}</Label>
            <Input id='simple-token-name' value={name} maxLength={64} onChange={(e) => setName(e.target.value)} />
          </div>

          {mode === 'create' && (
            <div className='space-y-2'>
              <Label htmlFor='simple-token-group'>{t('apikeys.modelGroup', { defaultValue: 'Model group' })}</Label>
              <select
                id='simple-token-group'
                className='border-input h-9 w-full rounded-md border bg-transparent px-3 text-sm'
                value={modelGroupID}
                onChange={(event) => onModelGroupChange(event.target.value)}
              >
                <option value=''>{t('apikeys.access.errors.groupRequired')}</option>
                {groups.map((group) => (
                  <option key={group.id} value={group.id}>
                    {group.name}
                  </option>
                ))}
              </select>
              {(!catalogReady || catalogError) && (
                <p className='text-muted-foreground text-xs'>{t('apikeys.access.errors.groupRequired')}</p>
              )}
            </div>
          )}
          {nameOnly && (
            <p className='text-muted-foreground rounded-md border p-3 text-sm'>
              {t('simple.tokens.advancedNotice')}{' '}
              <a className='underline' href='/settings'>
                {t('common.navigation.settings', { defaultValue: 'Settings' })}
              </a>
            </p>
          )}

          {!nameOnly && (
            <>
              <div className='space-y-2'>
                <Label>{t('simple.tokens.form.quota')}</Label>
                <RadioGroup
                  value={quotaUnlimited ? 'unlimited' : 'limited'}
                  onValueChange={(value) => setQuotaUnlimited(value === 'unlimited')}
                  className='flex flex-wrap gap-4'
                >
                  <label className='flex items-center gap-2 text-sm'>
                    <RadioGroupItem value='unlimited' /> {t('simple.tokens.form.quotaUnlimited')}
                  </label>
                  <label className='flex items-center gap-2 text-sm'>
                    <RadioGroupItem value='limited' /> {t('simple.tokens.form.quotaLimited')}
                  </label>
                </RadioGroup>
                {!quotaUnlimited && (
                  <div className='flex items-center gap-2'>
                    <span className='text-muted-foreground text-sm'>{creditLabel}</span>
                    <Input
                      inputMode='decimal'
                      value={quota}
                      onChange={(e) => setQuota(e.target.value)}
                      placeholder='50'
                      className='max-w-40'
                    />
                  </div>
                )}
                <p className='text-muted-foreground text-xs'>{t('simple.tokens.form.quotaHint')}</p>
              </div>

              <div className='space-y-2'>
                <Label>{t('simple.tokens.form.expiry')}</Label>
                <RadioGroup
                  value={expiry}
                  onValueChange={(value) => {
                    setExpiryChanged(true);
                    setExpiry(value as ExpiryChoice);
                  }}
                  className='flex flex-wrap gap-4'
                >
                  {(['never', '30', '90', 'custom'] as const).map((choice) => (
                    <label key={choice} className='flex items-center gap-2 text-sm'>
                      <RadioGroupItem value={choice} /> {t(`simple.tokens.form.expiry_${choice}`)}
                    </label>
                  ))}
                </RadioGroup>
                {expiry === 'custom' && (
                  <Input
                    aria-label={`${t('simple.tokens.form.expiry')} (${Intl.DateTimeFormat().resolvedOptions().timeZone})`}
                    type='date'
                    value={customDate}
                    onChange={(e) => {
                      setExpiryChanged(true);
                      setCustomDate(e.target.value);
                    }}
                    className='max-w-48'
                  />
                )}
                {customInvalid && customDate && <p className='text-destructive text-xs'>{t('simple.tokens.form.expiryPast')}</p>}
              </div>

              <div className='space-y-2'>
                <Label>{t('simple.tokens.form.models')}</Label>
                <RadioGroup
                  value={allModels ? 'all' : 'some'}
                  onValueChange={(value) => setAllModels(value === 'all')}
                  className='flex flex-wrap gap-4'
                >
                  <label className='flex items-center gap-2 text-sm'>
                    <RadioGroupItem value='all' /> {t('simple.tokens.form.modelsAll')}
                  </label>
                  <label className='flex items-center gap-2 text-sm'>
                    <RadioGroupItem value='some' /> {t('simple.tokens.form.modelsSome')}
                  </label>
                </RadioGroup>
                {!allModels && (
                  <div className='space-y-2 rounded-md border p-2'>
                    <Input
                      value={modelSearch}
                      onChange={(e) => setModelSearch(e.target.value)}
                      placeholder={t('simple.tokens.form.modelSearch')}
                    />
                    <ScrollArea className='h-44'>
                      <div className='space-y-1 pr-3'>
                        {filteredModels.map((model) => (
                          <label key={model.modelId} className='hover:bg-muted flex items-center gap-2 rounded px-1 py-1 text-sm'>
                            <Checkbox
                              checked={modelIds.includes(model.modelId)}
                              onCheckedChange={(checked) => toggleModel(model.modelId, checked === true)}
                            />
                            <span className='truncate font-mono text-xs'>{model.modelId}</span>
                          </label>
                        ))}
                        {filteredModels.length === 0 && (
                          <p className='text-muted-foreground px-1 py-2 text-sm'>{t('simple.models.empty')}</p>
                        )}
                      </div>
                    </ScrollArea>
                    <p className='text-muted-foreground text-xs'>{t('simple.tokens.form.modelsSelected', { count: modelIds.length })}</p>
                  </div>
                )}
              </div>
            </>
          )}
        </div>

        <DialogFooter>
          <Button variant='outline' onClick={() => onOpenChange(false)}>
            {t('common.buttons.cancel')}
          </Button>
          <Button onClick={submit} disabled={!canSubmit}>
            {mode === 'create' ? t('simple.tokens.form.createSubmit') : t('common.buttons.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
