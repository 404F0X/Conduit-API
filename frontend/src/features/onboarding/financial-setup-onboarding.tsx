import { FormEvent, useMemo, useState } from 'react';
import { CircleAlert, Landmark, Loader2, LockKeyhole } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { validateInitialAccountingSettings, type InitialAccountingSettingsInput } from '@/lib/accounting';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { AutoCompleteSelect } from '@/components/auto-complete-select';
import { currencyCodes } from '@/features/system/data/currencies';
import { useCompleteFinancialSetupOnboarding, useGeneralSettings, useUpdateGeneralSettings } from '@/features/system/data/system';

type Props = { onComplete: () => void };
type Touched = Record<keyof InitialAccountingSettingsInput, boolean>;

export function FinancialSetupOnboarding({ onComplete }: Props) {
  const { t } = useTranslation();
  const settingsQuery = useGeneralSettings();
  const updateSettings = useUpdateGeneralSettings();
  const completeSetup = useCompleteFinancialSetupOnboarding();
  const [values, setValues] = useState<InitialAccountingSettingsInput>({
    accountingCurrencyCode: '',
    creditDisplayName: '',
    creditsPerAccountingUnit: '',
  });
  const [touched, setTouched] = useState<Touched>({
    accountingCurrencyCode: false,
    creditDisplayName: false,
    creditsPerAccountingUnit: false,
  });
  const [submitError, setSubmitError] = useState('');

  const currencyItems = useMemo(() => currencyCodes.map((code) => ({ value: code, label: `${code} · ${t(`currencies.${code}`)}` })), [t]);
  const validation = validateInitialAccountingSettings(values);
  const currencyValid = validation.fields.accountingCurrencyCode && currencyCodes.includes(validation.normalized.accountingCurrencyCode);
  const isValid = validation.isValid && currencyValid;
  const pending = updateSettings.isPending || completeSetup.isPending;

  const touch = (field: keyof Touched) => setTouched((current) => ({ ...current, [field]: true }));
  const update = (field: keyof InitialAccountingSettingsInput, value: string) => setValues((current) => ({ ...current, [field]: value }));

  async function handleSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setTouched({ accountingCurrencyCode: true, creditDisplayName: true, creditsPerAccountingUnit: true });
    setSubmitError('');
    if (!isValid || pending || !settingsQuery.data) return;

    try {
      await updateSettings.mutateAsync({
        accountingCurrencyCode: validation.normalized.accountingCurrencyCode,
        timezone: settingsQuery.data.timezone,
        creditDisplayName: validation.normalized.creditDisplayName,
        creditsPerAccountingUnit: validation.normalized.creditsPerAccountingUnit,
        exchangeRates: settingsQuery.data.exchangeRates,
      });
      await completeSetup.mutateAsync();
      onComplete();
    } catch {
      setSubmitError(t('financialOnboarding.saveError'));
    }
  }

  if (settingsQuery.isLoading) {
    return <FinancialState icon={<Loader2 className='size-5 animate-spin' />} title={t('financialOnboarding.loading')} />;
  }

  if (settingsQuery.isError || !settingsQuery.data) {
    return (
      <FinancialState
        icon={<CircleAlert className='size-5' />}
        title={t('financialOnboarding.loadErrorTitle')}
        description={t('financialOnboarding.loadErrorDescription')}
        action={
          <Button className='h-10' onClick={() => settingsQuery.refetch()}>
            {t('financialOnboarding.retry')}
          </Button>
        }
      />
    );
  }

  return (
    <main className='bg-muted/25 flex min-h-full items-center justify-center px-4 py-8 sm:px-6 lg:px-10'>
      <Card className='border-border/80 w-full max-w-5xl overflow-hidden p-0 [box-shadow:0_20px_60px_-32px_rgba(15,23,42,0.28),0_8px_24px_-16px_rgba(15,23,42,0.18)]'>
        <div className='grid lg:grid-cols-[0.72fr_1.28fr]'>
          <aside className='bg-primary/[0.045] flex flex-col justify-between border-b p-6 lg:border-r lg:border-b-0 lg:p-8'>
            <div>
              <div className='border-primary/15 bg-background text-primary mb-7 flex size-11 items-center justify-center rounded-xl border shadow-sm'>
                <Landmark className='size-5' aria-hidden='true' />
              </div>
              <p className='text-primary text-xs font-semibold tracking-[0.16em] uppercase'>{t('financialOnboarding.eyebrow')}</p>
              <h1 className='mt-3 text-2xl leading-tight font-semibold text-balance sm:text-[1.75rem]'>{t('financialOnboarding.title')}</h1>
              <p className='text-muted-foreground mt-4 max-w-md text-sm leading-6 text-pretty'>{t('financialOnboarding.description')}</p>
            </div>
            <div className='border-primary/10 mt-8 flex gap-3 border-t pt-5 text-sm leading-5'>
              <LockKeyhole className='text-primary mt-0.5 size-4 shrink-0' aria-hidden='true' />
              <p className='text-muted-foreground text-pretty'>{t('financialOnboarding.lockNote')}</p>
            </div>
          </aside>

          <form className='bg-card grid gap-6 p-6 sm:p-8' onSubmit={handleSubmit} aria-busy={pending} noValidate>
            {submitError && (
              <Alert variant='destructive' role='alert'>
                <CircleAlert aria-hidden='true' />
                <AlertTitle>{t('financialOnboarding.saveErrorTitle')}</AlertTitle>
                <AlertDescription>{submitError}</AlertDescription>
              </Alert>
            )}

            <Field
              label={t('financialOnboarding.currency')}
              help={t('financialOnboarding.currencyHelp')}
              id='finance-currency'
              error={touched.accountingCurrencyCode && !currencyValid ? t('financialOnboarding.currencyError') : ''}
            >
              <AutoCompleteSelect
                inputId='finance-currency'
                selectedValue={values.accountingCurrencyCode}
                onSelectedValueChange={(value) => {
                  update('accountingCurrencyCode', value);
                  touch('accountingCurrencyCode');
                }}
                items={currencyItems}
                placeholder={t('financialOnboarding.currencyPlaceholder')}
                emptyMessage={t('financialOnboarding.currencyEmpty')}
                inputClassName='h-10'
                disabled={pending}
                ariaInvalid={touched.accountingCurrencyCode && !currencyValid}
                ariaLabelledBy='finance-currency-label'
                ariaDescribedBy={`finance-currency-help${touched.accountingCurrencyCode && !currencyValid ? ' finance-currency-error' : ''}`}
              />
            </Field>

            <Field
              label={t('financialOnboarding.creditName')}
              help={t('financialOnboarding.creditNameHelp')}
              id='finance-credit-name'
              error={touched.creditDisplayName && !validation.fields.creditDisplayName ? t('financialOnboarding.creditNameError') : ''}
            >
              <Input
                id='finance-credit-name'
                className='h-10'
                value={values.creditDisplayName}
                onChange={(event) => update('creditDisplayName', event.target.value)}
                onBlur={() => touch('creditDisplayName')}
                placeholder={t('financialOnboarding.creditNamePlaceholder')}
                disabled={pending}
                aria-invalid={touched.creditDisplayName && !validation.fields.creditDisplayName}
                aria-describedby={`finance-credit-name-help${
                  touched.creditDisplayName && !validation.fields.creditDisplayName ? ' finance-credit-name-error' : ''
                }`}
              />
            </Field>

            <Field
              label={t('financialOnboarding.creditsPerUnit')}
              help={t('financialOnboarding.creditsPerUnitHelp')}
              id='finance-credits-per-unit'
              error={
                touched.creditsPerAccountingUnit && !validation.fields.creditsPerAccountingUnit
                  ? t('financialOnboarding.creditsPerUnitError')
                  : ''
              }
            >
              <Input
                id='finance-credits-per-unit'
                className='h-10'
                inputMode='decimal'
                value={values.creditsPerAccountingUnit}
                onChange={(event) => update('creditsPerAccountingUnit', event.target.value)}
                onBlur={() => touch('creditsPerAccountingUnit')}
                placeholder={t('financialOnboarding.creditsPerUnitPlaceholder')}
                disabled={pending}
                aria-invalid={touched.creditsPerAccountingUnit && !validation.fields.creditsPerAccountingUnit}
                aria-describedby={`finance-credits-per-unit-help${
                  touched.creditsPerAccountingUnit && !validation.fields.creditsPerAccountingUnit ? ' finance-credits-per-unit-error' : ''
                }`}
              />
            </Field>

            <div className='border-primary/15 bg-primary/[0.045] rounded-xl border px-5 py-4' aria-live='polite'>
              <p className='text-muted-foreground text-xs font-medium'>{t('financialOnboarding.preview')}</p>
              <p className='mt-1 text-lg font-semibold [overflow-wrap:anywhere] [font-variant-numeric:tabular-nums]'>
                1 {validation.normalized.accountingCurrencyCode || '—'} = {validation.normalized.creditsPerAccountingUnit || '—'}{' '}
                {validation.normalized.creditDisplayName || '—'}
              </p>
            </div>

            <Button
              type='submit'
              className='h-11 justify-self-stretch transition-[background-color,box-shadow,transform] active:scale-[0.96] sm:justify-self-end sm:px-7'
              disabled={!isValid || pending}
            >
              {pending && <Loader2 className='mr-2 size-4 animate-spin' aria-hidden='true' />}
              {pending ? t('financialOnboarding.saving') : t('financialOnboarding.confirm')}
            </Button>
          </form>
        </div>
      </Card>
    </main>
  );
}

function Field({
  label,
  help,
  id,
  error,
  children,
}: {
  label: string;
  help: string;
  id: string;
  error: string | false;
  children: React.ReactNode;
}) {
  return (
    <div className='grid gap-2'>
      <Label id={`${id}-label`} htmlFor={id}>
        {label}
      </Label>
      {children}
      <p id={`${id}-help`} className='text-muted-foreground text-xs leading-5'>
        {help}
      </p>
      {error && (
        <p id={`${id}-error`} className='text-destructive text-xs' role='alert'>
          {error}
        </p>
      )}
    </div>
  );
}

function FinancialState({
  icon,
  title,
  description,
  action,
}: {
  icon: React.ReactNode;
  title: string;
  description?: string;
  action?: React.ReactNode;
}) {
  return (
    <main className='bg-muted/25 flex min-h-full items-center justify-center p-6'>
      <Card className='w-full max-w-md items-center p-8 text-center shadow-lg'>
        <div className='text-primary bg-primary/10 flex size-11 items-center justify-center rounded-xl'>{icon}</div>
        <h1 className='mt-4 text-lg font-semibold'>{title}</h1>
        {description && <p className='text-muted-foreground mt-2 text-sm leading-6 text-pretty'>{description}</p>}
        {action && <div className='mt-5'>{action}</div>}
      </Card>
    </main>
  );
}
