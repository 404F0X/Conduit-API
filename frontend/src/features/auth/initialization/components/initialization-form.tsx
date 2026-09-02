import { FormEvent, HTMLAttributes, useMemo, useRef, useState } from 'react';
import { z } from 'zod';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { useTranslation } from 'react-i18next';
import { validateInitialAccountingSettings } from '@/lib/accounting';
import i18n from '@/lib/i18n';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '@/components/ui/form';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { AutoCompleteSelect } from '@/components/auto-complete-select';
import { PasswordInput } from '@/components/password-input';
import { useInitializeSystem } from '@/features/auth/data/initialization';
import { initializationPasswordsMatch } from '@/features/auth/initialization/initialization-validation';
import { currencyCodes } from '@/features/system/data/currencies';

type InitializationFormProps = HTMLAttributes<HTMLFormElement>;
const supportedCurrencyCodes: readonly string[] = currencyCodes;

// Create form schema factory to support i18n
const createFormSchema = (t: (key: string) => string) =>
  z
    .object({
      ownerEmail: z
        .string()
        .min(1, { message: t('initialization.form.validation.ownerEmailRequired') })
        .email({ message: t('initialization.form.validation.ownerEmailInvalid') }),
      ownerPassword: z
        .string()
        .min(1, {
          message: t('initialization.form.validation.ownerPasswordRequired'),
        })
        .min(8, {
          message: t('initialization.form.validation.ownerPasswordMinLength'),
        }),
      confirmOwnerPassword: z.string().min(1, { message: t('initialization.form.validation.confirmOwnerPasswordRequired') }),
      ownerFirstName: z.string().min(1, { message: t('initialization.form.validation.ownerFirstNameRequired') }),
      ownerLastName: z.string().min(1, { message: t('initialization.form.validation.ownerLastNameRequired') }),
      brandName: z.string().min(1, { message: t('initialization.form.validation.brandNameRequired') }),
    })
    .refine(initializationPasswordsMatch, {
      message: t('initialization.form.validation.ownerPasswordsDoNotMatch'),
      path: ['confirmOwnerPassword'],
    });

export function InitializationForm({ className, ...props }: InitializationFormProps) {
  const { t } = useTranslation();
  const initializeSystemMutation = useInitializeSystem();

  const formSchema = createFormSchema(t);
  type FormData = z.infer<typeof formSchema>;
  type AccountDetails = Omit<FormData, 'confirmOwnerPassword'>;

  const [financialDialogOpen, setFinancialDialogOpen] = useState(false);
  const financialDialogTitleRef = useRef<HTMLHeadingElement>(null);
  const [dialogContent, setDialogContent] = useState<HTMLDivElement | null>(null);
  const [accountDetails, setAccountDetails] = useState<AccountDetails | null>(null);
  const [accountingCurrencyCode, setAccountingCurrencyCode] = useState('');
  const [creditDisplayName, setCreditDisplayName] = useState('');
  const [creditsPerAccountingUnit, setCreditsPerAccountingUnit] = useState('');
  const [financialTouched, setFinancialTouched] = useState({
    accountingCurrencyCode: false,
    creditDisplayName: false,
    creditsPerAccountingUnit: false,
  });
  const currencyItems = useMemo(() => currencyCodes.map((code) => ({ value: code, label: code })), []);

  const financialValidation = validateInitialAccountingSettings({
    accountingCurrencyCode,
    creditDisplayName,
    creditsPerAccountingUnit,
  });
  const {
    accountingCurrencyCode: normalizedAccountingCurrency,
    creditDisplayName: normalizedCreditDisplayName,
    creditsPerAccountingUnit: normalizedCreditsPerAccountingUnit,
  } = financialValidation.normalized;
  const accountingCurrencyValid =
    financialValidation.fields.accountingCurrencyCode && supportedCurrencyCodes.includes(normalizedAccountingCurrency);
  const creditDisplayNameValid = financialValidation.fields.creditDisplayName;
  const creditsPerAccountingUnitValid = financialValidation.fields.creditsPerAccountingUnit;
  const financialSettingsValid = financialValidation.isValid && supportedCurrencyCodes.includes(normalizedAccountingCurrency);

  const form = useForm<FormData>({
    resolver: zodResolver(formSchema),
    mode: 'onChange',
    defaultValues: {
      ownerEmail: '',
      ownerPassword: '',
      confirmOwnerPassword: '',
      ownerFirstName: '',
      ownerLastName: '',
      brandName: '',
    },
  });

  function onSubmit(data: FormData) {
    setAccountDetails({
      ownerEmail: data.ownerEmail,
      ownerPassword: data.ownerPassword,
      ownerFirstName: data.ownerFirstName,
      ownerLastName: data.ownerLastName,
      brandName: data.brandName,
    });
    setFinancialDialogOpen(true);
  }

  function onFinancialSubmit(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setFinancialTouched({
      accountingCurrencyCode: true,
      creditDisplayName: true,
      creditsPerAccountingUnit: true,
    });
    if (!accountDetails || !financialSettingsValid || initializeSystemMutation.isPending) return;

    initializeSystemMutation.mutate({
      ...accountDetails,
      preferLanguage: i18n.language,
      accountingCurrencyCode: normalizedAccountingCurrency,
      creditDisplayName: normalizedCreditDisplayName,
      creditsPerAccountingUnit: normalizedCreditsPerAccountingUnit,
    });
  }

  return (
    <>
      <Form {...form}>
        <form onSubmit={form.handleSubmit(onSubmit)} className={cn('grid gap-4', className)} {...props}>
          <FormField
            control={form.control}
            name='ownerFirstName'
            render={({ field }) => (
              <FormItem>
                <FormLabel>{t('initialization.form.ownerFirstName')}</FormLabel>
                <FormControl>
                  <Input
                    placeholder={t('initialization.form.placeholders.ownerFirstName')}
                    className='border-slate-300 !bg-white text-slate-800 transition-[border-color,box-shadow,background-color] duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:!bg-white focus:ring-2 focus:ring-slate-200'
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name='ownerLastName'
            render={({ field }) => (
              <FormItem>
                <FormLabel>{t('initialization.form.ownerLastName')}</FormLabel>
                <FormControl>
                  <Input
                    placeholder={t('initialization.form.placeholders.ownerLastName')}
                    className='border-slate-300 !bg-white text-slate-800 transition-[border-color,box-shadow,background-color] duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:!bg-white focus:ring-2 focus:ring-slate-200'
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name='ownerEmail'
            render={({ field }) => (
              <FormItem>
                <FormLabel>{t('initialization.form.ownerEmail')}</FormLabel>
                <FormControl>
                  <Input
                    placeholder={t('initialization.form.placeholders.ownerEmail')}
                    className='border-slate-300 !bg-white text-slate-800 transition-[border-color,box-shadow,background-color] duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:!bg-white focus:ring-2 focus:ring-slate-200'
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name='ownerPassword'
            render={({ field }) => (
              <FormItem>
                <FormLabel>{t('initialization.form.ownerPassword')}</FormLabel>
                <FormControl>
                  <PasswordInput
                    placeholder={t('initialization.form.placeholders.ownerPassword')}
                    autoComplete='new-password'
                    className='border-slate-300 bg-white text-slate-800 backdrop-blur-sm transition-[border-color,box-shadow,background-color] duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:bg-white focus:ring-2 focus:ring-slate-200'
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name='confirmOwnerPassword'
            render={({ field }) => (
              <FormItem>
                <FormLabel>{t('initialization.form.confirmOwnerPassword')}</FormLabel>
                <FormControl>
                  <PasswordInput
                    placeholder={t('initialization.form.placeholders.confirmOwnerPassword')}
                    autoComplete='new-password'
                    className='border-slate-300 bg-white text-slate-800 backdrop-blur-sm transition-[border-color,box-shadow,background-color] duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:bg-white focus:ring-2 focus:ring-slate-200'
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <FormField
            control={form.control}
            name='brandName'
            render={({ field }) => (
              <FormItem>
                <FormLabel>{t('initialization.form.brandName')}</FormLabel>
                <FormControl>
                  <Input
                    placeholder={t('initialization.form.placeholders.brandName')}
                    className='border-slate-300 !bg-white text-slate-800 transition-[border-color,box-shadow,background-color] duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:!bg-white focus:ring-2 focus:ring-slate-200'
                    {...field}
                  />
                </FormControl>
                <FormMessage />
              </FormItem>
            )}
          />
          <Button
            type='submit'
            className='mt-6 w-full rounded-lg bg-slate-800 px-6 py-3 font-medium text-white shadow-lg transition-[background-color,box-shadow,opacity,transform] duration-300 hover:bg-slate-700 hover:shadow-xl focus:ring-2 focus:ring-slate-500 focus:ring-offset-2 active:scale-[0.96] disabled:opacity-50'
            disabled={initializeSystemMutation.isPending || !form.formState.isValid}
          >
            {t('initialization.form.continue')}
          </Button>
        </form>
      </Form>

      <Dialog
        open={financialDialogOpen}
        onOpenChange={(open) => {
          if (open) setFinancialDialogOpen(true);
        }}
      >
        <DialogContent
          ref={setDialogContent}
          showCloseButton={false}
          className='max-h-[calc(100svh-2rem)] overflow-y-auto border-slate-200 sm:max-w-lg'
          aria-busy={initializeSystemMutation.isPending}
          onOpenAutoFocus={(event) => {
            event.preventDefault();
            financialDialogTitleRef.current?.focus();
          }}
          onEscapeKeyDown={(event) => event.preventDefault()}
          onInteractOutside={(event) => event.preventDefault()}
        >
          <DialogHeader>
            <DialogTitle ref={financialDialogTitleRef} tabIndex={-1} className='outline-none'>
              {t('initialization.financial.title')}
            </DialogTitle>
            <DialogDescription>{t('initialization.financial.description')}</DialogDescription>
          </DialogHeader>

          <form onSubmit={onFinancialSubmit} className='grid gap-5' aria-busy={initializeSystemMutation.isPending}>
            <div className='grid gap-2'>
              <Label id='initialization-accounting-currency-label' htmlFor='initialization-accounting-currency'>
                {t('initialization.financial.accountingCurrency')}
              </Label>
              <AutoCompleteSelect
                inputId='initialization-accounting-currency'
                selectedValue={accountingCurrencyCode}
                onSelectedValueChange={(value) => {
                  setAccountingCurrencyCode(value);
                  setFinancialTouched((current) => ({ ...current, accountingCurrencyCode: true }));
                }}
                items={currencyItems}
                placeholder={t('initialization.financial.accountingCurrencyPlaceholder')}
                emptyMessage={t('initialization.financial.currencyEmpty')}
                portalContainer={dialogContent}
                disabled={initializeSystemMutation.isPending}
                ariaInvalid={financialTouched.accountingCurrencyCode && !accountingCurrencyValid}
                ariaLabelledBy='initialization-accounting-currency-label'
                ariaDescribedBy={`initialization-accounting-currency-help${
                  financialTouched.accountingCurrencyCode && !accountingCurrencyValid ? ' initialization-accounting-currency-error' : ''
                }`}
              />
              <p id='initialization-accounting-currency-help' className='text-muted-foreground text-xs'>
                {t('initialization.financial.accountingCurrencyHelp')}
              </p>
              {financialTouched.accountingCurrencyCode && !accountingCurrencyValid && (
                <p id='initialization-accounting-currency-error' className='text-destructive text-xs' role='alert'>
                  {t('initialization.financial.validation.currency')}
                </p>
              )}
            </div>

            <div className='grid gap-2'>
              <Label htmlFor='initialization-credit-display-name'>{t('initialization.financial.creditDisplayName')}</Label>
              <Input
                id='initialization-credit-display-name'
                value={creditDisplayName}
                onChange={(event) => setCreditDisplayName(event.target.value)}
                onBlur={() => setFinancialTouched((current) => ({ ...current, creditDisplayName: true }))}
                placeholder={t('initialization.financial.creditDisplayNamePlaceholder')}
                disabled={initializeSystemMutation.isPending}
                aria-invalid={financialTouched.creditDisplayName && !creditDisplayNameValid}
                aria-describedby={`initialization-credit-display-name-help${
                  financialTouched.creditDisplayName && !creditDisplayNameValid ? ' initialization-credit-display-name-error' : ''
                }`}
              />
              <p id='initialization-credit-display-name-help' className='text-muted-foreground text-xs'>
                {t('initialization.financial.creditDisplayNameHelp')}
              </p>
              {financialTouched.creditDisplayName && !creditDisplayNameValid && (
                <p id='initialization-credit-display-name-error' className='text-destructive text-xs' role='alert'>
                  {t('initialization.financial.validation.creditDisplayName')}
                </p>
              )}
            </div>

            <div className='grid gap-2'>
              <Label htmlFor='initialization-credits-per-unit'>{t('initialization.financial.creditsPerUnit')}</Label>
              <Input
                id='initialization-credits-per-unit'
                value={creditsPerAccountingUnit}
                onChange={(event) => setCreditsPerAccountingUnit(event.target.value)}
                onBlur={() => setFinancialTouched((current) => ({ ...current, creditsPerAccountingUnit: true }))}
                placeholder={t('initialization.financial.creditsPerUnitPlaceholder')}
                inputMode='decimal'
                disabled={initializeSystemMutation.isPending}
                aria-invalid={financialTouched.creditsPerAccountingUnit && !creditsPerAccountingUnitValid}
                aria-describedby={`initialization-credits-per-unit-help${
                  financialTouched.creditsPerAccountingUnit && !creditsPerAccountingUnitValid
                    ? ' initialization-credits-per-unit-error'
                    : ''
                }`}
              />
              <p id='initialization-credits-per-unit-help' className='text-muted-foreground text-xs'>
                {t('initialization.financial.creditsPerUnitHelp')}
              </p>
              {financialTouched.creditsPerAccountingUnit && !creditsPerAccountingUnitValid && (
                <p id='initialization-credits-per-unit-error' className='text-destructive text-xs' role='alert'>
                  {t('initialization.financial.validation.creditsPerUnit')}
                </p>
              )}
            </div>

            {financialSettingsValid && (
              <div className='rounded-md border border-slate-200 bg-slate-50 px-4 py-3 text-center' aria-live='polite'>
                <p className='text-xs font-medium tracking-wide text-slate-500 uppercase'>
                  {t('initialization.financial.conversionPreview')}
                </p>
                <p className='mx-auto mt-1 max-w-full font-mono text-base font-semibold [overflow-wrap:anywhere] break-words whitespace-normal text-slate-800'>
                  1 {normalizedAccountingCurrency} = {normalizedCreditsPerAccountingUnit} {normalizedCreditDisplayName}
                </p>
              </div>
            )}

            <p className='rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-sm leading-5 text-amber-900'>
              {t('initialization.financial.currencyLockNote')}
            </p>

            <DialogFooter>
              <Button
                type='button'
                variant='outline'
                className='h-11 w-full sm:h-9 sm:w-auto'
                disabled={initializeSystemMutation.isPending}
                onClick={() => setFinancialDialogOpen(false)}
              >
                {t('initialization.financial.back')}
              </Button>
              <Button
                type='submit'
                className='h-11 w-full sm:h-9 sm:w-auto'
                disabled={initializeSystemMutation.isPending || !financialSettingsValid}
              >
                {initializeSystemMutation.isPending ? t('initialization.form.submitting') : t('initialization.financial.confirm')}
              </Button>
            </DialogFooter>
          </form>
        </DialogContent>
      </Dialog>
    </>
  );
}
