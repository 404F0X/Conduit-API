'use client';

import React, { useState, useEffect } from 'react';
import { CircleAlert, Loader2, LockKeyhole, Plus, Save, Trash2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import {
  DEFAULT_ACCOUNTING_CURRENCY_CODE,
  DEFAULT_CREDIT_DISPLAY_NAME,
  DEFAULT_CREDITS_PER_ACCOUNTING_UNIT,
  isCurrencyCode,
  isPositiveDecimal,
  normalizeCurrencyCode,
  type AccountingExchangeRate,
} from '@/lib/accounting';
import { usePermissions } from '@/hooks/usePermissions';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Switch } from '@/components/ui/switch';
import { AutoCompleteSelect } from '@/components/auto-complete-select';
import { useProductExperience, useUpdateProductExperienceSettings } from '@/features/product-experience';
import { useSystemContext } from '../context/system-context';
import { currencyCodes } from '../data/currencies';
import {
  useGeneralSettings,
  useUpdateGeneralSettings,
  useUserAgentPassThroughSettings,
  useUpdateUserAgentPassThroughSettings,
  usePassThroughSettings,
  useUpdatePassThroughSettings,
} from '../data/system';
import { GMTTimeZoneOptions } from '../data/timezones';

export function GeneralSettings() {
  const { t } = useTranslation();
  const { data: settings, isLoading: isLoadingSettings } = useGeneralSettings();
  const updateSettings = useUpdateGeneralSettings();
  const { isLoading, setIsLoading } = useSystemContext();
  const { isOwner } = usePermissions();
  const { mode: productMode, isLoading: isLoadingProductMode } = useProductExperience();
  const updateProductMode = useUpdateProductExperienceSettings();

  // User-Agent Pass-Through settings
  const { data: uaSettings, isLoading: isLoadingUASettings } = useUserAgentPassThroughSettings();
  const updateUASettings = useUpdateUserAgentPassThroughSettings();
  const [uaPassThroughEnabled, setUaPassThroughEnabled] = useState(false);

  // Pass-Through (request/response body) settings
  const { data: ptSettings, isLoading: isLoadingPTSettings } = usePassThroughSettings();
  const updatePTSettings = useUpdatePassThroughSettings();
  const [passThroughEnabled, setPassThroughEnabled] = useState(false);

  const [accountingCurrencyCode, setAccountingCurrencyCode] = useState(DEFAULT_ACCOUNTING_CURRENCY_CODE);
  const [timezone, setTimezone] = useState('UTC');
  const [creditDisplayName, setCreditDisplayName] = useState(DEFAULT_CREDIT_DISPLAY_NAME);
  const [creditsPerAccountingUnit, setCreditsPerAccountingUnit] = useState(DEFAULT_CREDITS_PER_ACCOUNTING_UNIT);
  const [exchangeRates, setExchangeRates] = useState<AccountingExchangeRate[]>([]);
  const [accountingErrors, setAccountingErrors] = useState<Record<string, string>>({});

  const currencyItems = React.useMemo(
    () =>
      currencyCodes.map((code) => ({
        value: code,
        label: t(`currencies.${code}`),
      })),
    [t]
  );

  const timezoneItems = React.useMemo(() => GMTTimeZoneOptions, []);

  // Update local state when settings are loaded
  useEffect(() => {
    if (settings) {
      setAccountingCurrencyCode(settings.accountingCurrencyCode);
      setTimezone(settings.timezone || 'UTC');
      setCreditDisplayName(settings.creditDisplayName);
      setCreditsPerAccountingUnit(settings.creditsPerAccountingUnit);
      setExchangeRates(settings.exchangeRates);
      setAccountingErrors({});
    }
  }, [settings]);

  // Update UA pass-through state when loaded
  useEffect(() => {
    if (uaSettings) {
      setUaPassThroughEnabled(uaSettings.enabled);
    }
  }, [uaSettings]);

  // Update pass-through state when loaded
  useEffect(() => {
    if (ptSettings) {
      setPassThroughEnabled(ptSettings.enabled);
    }
  }, [ptSettings]);

  const handleSave = async () => {
    const errors: Record<string, string> = {};
    const normalizedAccountingCurrency = normalizeCurrencyCode(accountingCurrencyCode);
    if (!isCurrencyCode(normalizedAccountingCurrency)) {
      errors.accountingCurrencyCode = t('system.accounting.validation.currencyCode');
    }
    if (!creditDisplayName.trim()) {
      errors.creditDisplayName = t('system.accounting.validation.creditDisplayName');
    }
    if (!isPositiveDecimal(creditsPerAccountingUnit)) {
      errors.creditsPerAccountingUnit = t('system.accounting.validation.creditsPerUnit');
    }

    const seenCurrencies = new Set<string>();
    if (isCurrencyCode(normalizedAccountingCurrency)) seenCurrencies.add(normalizedAccountingCurrency);
    exchangeRates.forEach((rate, index) => {
      const currency = normalizeCurrencyCode(rate.currencyCode);
      if (!isCurrencyCode(currency)) {
        errors[`exchangeRates.${index}.currencyCode`] = t('system.accounting.validation.rateCurrency');
      } else if (seenCurrencies.has(currency)) {
        errors[`exchangeRates.${index}.currencyCode`] = t('system.accounting.validation.duplicateCurrency');
      } else {
        seenCurrencies.add(currency);
      }
      if (!isPositiveDecimal(rate.quotePerAccountingUnit)) {
        errors[`exchangeRates.${index}.quotePerAccountingUnit`] = t('system.accounting.validation.rateValue');
      }
    });

    if (Object.keys(errors).length > 0) {
      setAccountingErrors(errors);
      toast.error(t('system.accounting.validation.fixErrors'));
      return;
    }

    setAccountingErrors({});
    setIsLoading(true);
    try {
      await updateSettings.mutateAsync({
        ...(settings?.accountingCurrencyLocked ? {} : { accountingCurrencyCode: normalizedAccountingCurrency }),
        timezone: timezone.trim(),
        creditDisplayName: creditDisplayName.trim(),
        creditsPerAccountingUnit: creditsPerAccountingUnit.trim(),
        exchangeRates: exchangeRates.map((rate) => ({
          currencyCode: normalizeCurrencyCode(rate.currencyCode),
          quotePerAccountingUnit: rate.quotePerAccountingUnit.trim(),
        })),
      });
    } finally {
      setIsLoading(false);
    }
  };

  const handleUAPassThroughChange = async (enabled: boolean) => {
    const previousValue = uaPassThroughEnabled;
    setUaPassThroughEnabled(enabled);
    try {
      await updateUASettings.mutateAsync({ enabled });
    } catch {
      // Revert state on error
      setUaPassThroughEnabled(previousValue);
    }
  };

  const handlePassThroughChange = async (enabled: boolean) => {
    const previousValue = passThroughEnabled;
    setPassThroughEnabled(enabled);
    try {
      await updatePTSettings.mutateAsync({ enabled });
    } catch {
      // Revert state on error
      setPassThroughEnabled(previousValue);
    }
  };

  const handleProductModeChange = async (simpleModeEnabled: boolean) => {
    try {
      await updateProductMode.mutateAsync(simpleModeEnabled ? 'SIMPLE' : 'ENTERPRISE');
      toast.success(t('common.success.systemUpdated'));
    } catch {
      toast.error(t('common.errors.systemUpdateFailed'));
    }
  };

  const hasChanges = settings
    ? settings.accountingCurrencyCode !== accountingCurrencyCode ||
      settings.timezone !== timezone ||
      settings.creditDisplayName !== creditDisplayName ||
      settings.creditsPerAccountingUnit !== creditsPerAccountingUnit ||
      JSON.stringify(settings.exchangeRates || []) !== JSON.stringify(exchangeRates)
    : false;

  if (isLoadingSettings) {
    return (
      <div className='flex h-32 items-center justify-center'>
        <Loader2 className='h-6 w-6 animate-spin' />
        <span className='text-muted-foreground ml-2'>{t('common.loading')}</span>
      </div>
    );
  }

  return (
    <div className='space-y-6'>
      {isOwner && (
        <Card>
          <CardHeader>
            <CardTitle>{t('system.productMode.title')}</CardTitle>
            <CardDescription>{t('system.productMode.description')}</CardDescription>
          </CardHeader>
          <CardContent>
            <div className='flex items-center justify-between gap-6'>
              <div className='space-y-1'>
                <Label htmlFor='simple-product-mode'>
                  {productMode === 'SIMPLE' ? t('system.productMode.simple') : t('system.productMode.enterprise')}
                </Label>
                <div className='text-muted-foreground text-sm'>
                  {productMode === 'SIMPLE' ? t('system.productMode.simpleDescription') : t('system.productMode.enterpriseDescription')}
                </div>
              </div>
              <Switch
                id='simple-product-mode'
                checked={productMode === 'SIMPLE'}
                onCheckedChange={handleProductModeChange}
                disabled={isLoadingProductMode || updateProductMode.isPending}
                aria-label={t('system.productMode.simple')}
              />
            </div>
            <div className='bg-muted/50 text-muted-foreground mt-4 rounded-lg px-3 py-2 text-sm'>{t('system.productMode.safetyNote')}</div>
          </CardContent>
        </Card>
      )}

      <Card>
        <CardHeader>
          <CardTitle>{t('system.general.title')}</CardTitle>
          <CardDescription>{t('system.general.description')}</CardDescription>
        </CardHeader>
        <CardContent className='space-y-6'>
          <div className='space-y-2'>
            <Label htmlFor='timezone'>{t('system.general.timezone.label')}</Label>
            <div className='max-w-md'>
              <AutoCompleteSelect
                selectedValue={timezone}
                onSelectedValueChange={setTimezone}
                items={timezoneItems}
                placeholder={t('system.general.timezone.placeholder')}
                isLoading={isLoadingSettings}
              />
            </div>
            <div className='text-muted-foreground text-sm'>{t('system.general.timezone.description')}</div>
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t('system.accounting.title')}</CardTitle>
          <CardDescription>{t('system.accounting.description')}</CardDescription>
        </CardHeader>
        <CardContent className='space-y-6'>
          <Alert className='bg-muted/30 text-foreground'>
            <CircleAlert className='text-muted-foreground' />
            <AlertTitle>{t('system.accounting.defaultNoticeTitle')}</AlertTitle>
            <AlertDescription>{t('system.accounting.defaultNoticeDescription')}</AlertDescription>
          </Alert>

          <div className='grid gap-4 lg:grid-cols-3'>
            <div className='space-y-2'>
              <div className='flex min-h-5 items-center justify-between gap-2'>
                <Label htmlFor='accounting-currency'>{t('system.accounting.currency')}</Label>
                {settings?.accountingCurrencyLocked && (
                  <Badge variant='outline' className='border-amber-500/40 bg-amber-500/5 text-amber-700 dark:text-amber-300'>
                    <LockKeyhole className='size-3' />
                    {t('system.accounting.currencyLockedBadge')}
                  </Badge>
                )}
              </div>
              <div data-testid='accounting-currency-select'>
                <AutoCompleteSelect
                  inputId='accounting-currency'
                  selectedValue={accountingCurrencyCode}
                  onSelectedValueChange={(value) => {
                    setAccountingCurrencyCode(value);
                    setAccountingErrors((current) => ({ ...current, accountingCurrencyCode: '' }));
                  }}
                  items={currencyItems}
                  inputClassName='font-mono uppercase'
                  disabled={settings?.accountingCurrencyLocked === true}
                />
              </div>
              <p className='text-muted-foreground text-xs'>{t('system.accounting.currencyDescription')}</p>
              {settings?.accountingCurrencyLocked && (
                <div
                  className='flex items-start gap-2 border-l-2 border-amber-500 bg-amber-500/[0.06] px-3 py-2 text-xs text-amber-900 dark:text-amber-100'
                  data-testid='accounting-currency-lock-notice'
                >
                  <LockKeyhole className='mt-0.5 size-3.5 shrink-0 text-amber-600 dark:text-amber-400' />
                  <span>{t('system.accounting.currencyLockedDescription')}</span>
                </div>
              )}
              {accountingErrors.accountingCurrencyCode && (
                <p className='text-destructive text-xs' role='alert'>
                  {accountingErrors.accountingCurrencyCode}
                </p>
              )}
            </div>
            <div className='space-y-2'>
              <Label htmlFor='credit-display-name'>{t('system.accounting.creditDisplayName')}</Label>
              <Input
                id='credit-display-name'
                value={creditDisplayName}
                aria-invalid={!!accountingErrors.creditDisplayName}
                onChange={(event) => {
                  setCreditDisplayName(event.target.value);
                  setAccountingErrors((current) => ({ ...current, creditDisplayName: '' }));
                }}
              />
              <p className='text-muted-foreground text-xs'>{t('system.accounting.creditDisplayNameDescription')}</p>
              {accountingErrors.creditDisplayName && (
                <p className='text-destructive text-xs' role='alert'>
                  {accountingErrors.creditDisplayName}
                </p>
              )}
            </div>
            <div className='space-y-2'>
              <Label htmlFor='credits-per-accounting-unit'>{t('system.accounting.creditsPerUnit')}</Label>
              <Input
                id='credits-per-accounting-unit'
                inputMode='decimal'
                value={creditsPerAccountingUnit}
                aria-invalid={!!accountingErrors.creditsPerAccountingUnit}
                className='font-mono tabular-nums'
                onChange={(event) => {
                  setCreditsPerAccountingUnit(event.target.value);
                  setAccountingErrors((current) => ({ ...current, creditsPerAccountingUnit: '' }));
                }}
              />
              <p className='text-muted-foreground text-xs'>{t('system.accounting.creditsPerUnitDescription')}</p>
              {accountingErrors.creditsPerAccountingUnit && (
                <p className='text-destructive text-xs' role='alert'>
                  {accountingErrors.creditsPerAccountingUnit}
                </p>
              )}
            </div>
          </div>

          <div className='border-primary/40 bg-muted/25 border-l-2 px-4 py-3'>
            <p className='text-muted-foreground text-xs font-medium'>{t('system.accounting.formulaLabel')}</p>
            <p className='mt-1 font-mono text-base font-semibold tabular-nums sm:text-lg'>
              1 {accountingCurrencyCode || '—'} = {creditsPerAccountingUnit || '—'} {creditDisplayName.trim() || '—'}
            </p>
          </div>

          <div className='space-y-3 border-t pt-5'>
            <div className='flex items-center justify-between gap-3'>
              <div>
                <Label>{t('system.accounting.fxRates')}</Label>
                <p className='text-muted-foreground text-xs'>
                  {t('system.accounting.fxRatesDescription', { currency: accountingCurrencyCode || DEFAULT_ACCOUNTING_CURRENCY_CODE })}
                </p>
              </div>
              <Button
                type='button'
                variant='outline'
                size='sm'
                onClick={() => {
                  const used = new Set(exchangeRates.map((rate) => normalizeCurrencyCode(rate.currencyCode)));
                  const nextCurrency =
                    currencyCodes.find((code) => code !== normalizeCurrencyCode(accountingCurrencyCode) && !used.has(code)) || '';
                  setExchangeRates((rates) => [...rates, { currencyCode: nextCurrency, quotePerAccountingUnit: '' }]);
                }}
              >
                <Plus className='mr-1 h-4 w-4' />
                {t('system.accounting.addRate')}
              </Button>
            </div>
            {exchangeRates.map((rate, index) => (
              <div
                key={index}
                className='grid gap-2 border-b pb-3 last:border-b-0 last:pb-0 sm:grid-cols-[minmax(8rem,0.7fr)_minmax(0,1.4fr)_auto]'
              >
                <div className='space-y-1'>
                  <Input
                    aria-label={t('system.accounting.rateCurrency')}
                    value={rate.currencyCode}
                    maxLength={3}
                    className='font-mono uppercase'
                    aria-invalid={!!accountingErrors[`exchangeRates.${index}.currencyCode`]}
                    onChange={(event) => {
                      setExchangeRates((rates) =>
                        rates.map((item, itemIndex) =>
                          itemIndex === index ? { ...item, currencyCode: event.target.value.toUpperCase() } : item
                        )
                      );
                      setAccountingErrors((current) => ({ ...current, [`exchangeRates.${index}.currencyCode`]: '' }));
                    }}
                  />
                  {accountingErrors[`exchangeRates.${index}.currencyCode`] && (
                    <p className='text-destructive text-xs' role='alert'>
                      {accountingErrors[`exchangeRates.${index}.currencyCode`]}
                    </p>
                  )}
                </div>
                <div className='space-y-1'>
                  <div className='flex items-center gap-2'>
                    <span className='text-muted-foreground shrink-0 font-mono text-xs'>1 {accountingCurrencyCode || '—'} =</span>
                    <Input
                      aria-label={t('system.accounting.rateValue')}
                      inputMode='decimal'
                      value={rate.quotePerAccountingUnit}
                      className='font-mono tabular-nums'
                      aria-invalid={!!accountingErrors[`exchangeRates.${index}.quotePerAccountingUnit`]}
                      onChange={(event) => {
                        setExchangeRates((rates) =>
                          rates.map((item, itemIndex) =>
                            itemIndex === index ? { ...item, quotePerAccountingUnit: event.target.value } : item
                          )
                        );
                        setAccountingErrors((current) => ({ ...current, [`exchangeRates.${index}.quotePerAccountingUnit`]: '' }));
                      }}
                    />
                    <span className='text-muted-foreground w-10 shrink-0 font-mono text-xs'>{rate.currencyCode || '—'}</span>
                  </div>
                  {accountingErrors[`exchangeRates.${index}.quotePerAccountingUnit`] && (
                    <p className='text-destructive text-xs' role='alert'>
                      {accountingErrors[`exchangeRates.${index}.quotePerAccountingUnit`]}
                    </p>
                  )}
                </div>
                <Button
                  type='button'
                  variant='ghost'
                  size='icon'
                  onClick={() => setExchangeRates((rates) => rates.filter((_, itemIndex) => itemIndex !== index))}
                  aria-label={t('system.accounting.removeRate')}
                >
                  <Trash2 className='h-4 w-4' />
                </Button>
              </div>
            ))}
            {exchangeRates.length === 0 && (
              <p className='text-muted-foreground border border-dashed px-3 py-4 text-center text-xs'>{t('system.accounting.noRates')}</p>
            )}
          </div>
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>{t('system.passThroughGroup.title')}</CardTitle>
          <CardDescription>{t('system.passThroughGroup.description')}</CardDescription>
        </CardHeader>
        <CardContent className='space-y-4'>
          <div className='flex items-center justify-between'>
            <div className='space-y-0.5'>
              <Label htmlFor='ua-pass-through'>{t('system.userAgentPassThrough.label')}</Label>
              <div className='text-muted-foreground text-sm'>{t('system.userAgentPassThrough.helpText')}</div>
            </div>
            <Switch
              id='ua-pass-through'
              checked={uaPassThroughEnabled}
              onCheckedChange={handleUAPassThroughChange}
              disabled={isLoadingUASettings || updateUASettings.isPending}
            />
          </div>
          <div className='flex items-center justify-between'>
            <div className='space-y-0.5'>
              <Label htmlFor='pass-through'>{t('system.passThrough.label')}</Label>
              <div className='text-muted-foreground text-sm'>{t('system.passThrough.helpText')}</div>
            </div>
            <Switch
              id='pass-through'
              checked={passThroughEnabled}
              onCheckedChange={handlePassThroughChange}
              disabled={isLoadingPTSettings || updatePTSettings.isPending}
            />
          </div>
        </CardContent>
      </Card>

      {hasChanges && (
        <div className='flex justify-end'>
          <Button onClick={handleSave} disabled={isLoading || updateSettings.isPending} className='min-w-[100px]'>
            {isLoading || updateSettings.isPending ? (
              <>
                <Loader2 className='mr-2 h-4 w-4 animate-spin' />
                {t('system.buttons.saving')}
              </>
            ) : (
              <>
                <Save className='mr-2 h-4 w-4' />
                {t('system.buttons.save')}
              </>
            )}
          </Button>
        </div>
      )}
    </div>
  );
}
