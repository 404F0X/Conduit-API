import { HTMLAttributes } from 'react';
import { z } from 'zod';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { useTranslation } from 'react-i18next';
import i18n from '@/lib/i18n';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '@/components/ui/form';
import { Input } from '@/components/ui/input';
import { PasswordInput } from '@/components/password-input';
import { useInitializeSystem } from '@/features/auth/data/initialization';
import { initializationPasswordsMatch } from '@/features/auth/initialization/initialization-validation';

type InitializationFormProps = HTMLAttributes<HTMLFormElement>;

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
    initializeSystemMutation.mutate({
      ownerEmail: data.ownerEmail,
      ownerPassword: data.ownerPassword,
      ownerFirstName: data.ownerFirstName,
      ownerLastName: data.ownerLastName,
      brandName: data.brandName,
      preferLanguage: i18n.language,
      deferFinancialSetup: true,
    });
  }

  return (
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
  );
}
