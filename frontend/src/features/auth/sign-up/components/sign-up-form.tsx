import { HTMLAttributes } from 'react';
import { z } from 'zod';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { passwordSchema } from '@/lib/validation';
import { Button } from '@/components/ui/button';
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '@/components/ui/form';
import { Input } from '@/components/ui/input';
import { PasswordInput } from '@/components/password-input';
import { useSignUp } from '@/features/auth/data/auth';

type SignUpFormProps = HTMLAttributes<HTMLFormElement>;

export function SignUpForm({ className, ...props }: SignUpFormProps) {
  const { t } = useTranslation();
  const signUpMutation = useSignUp();
  const formSchema = z
    .object({
      email: z.email({ message: t('auth.signUp.validation.invalidEmail') }).min(1, { message: t('auth.signUp.validation.emailRequired') }),
      password: passwordSchema(t),
      confirmPassword: z.string(),
    })
    .refine((data) => data.password === data.confirmPassword, {
      message: t('users.validation.passwordsNotMatch'),
      path: ['confirmPassword'],
    });

  const form = useForm<z.infer<typeof formSchema>>({
    resolver: zodResolver(formSchema),
    defaultValues: {
      email: '',
      password: '',
      confirmPassword: '',
    },
  });

  function onSubmit(data: z.infer<typeof formSchema>) {
    signUpMutation.mutate({ email: data.email, password: data.password });
  }

  return (
    <Form {...form}>
      <form onSubmit={form.handleSubmit(onSubmit)} className={cn('grid gap-5', className)} {...props}>
        <FormField
          control={form.control}
          name='email'
          render={({ field }) => (
            <FormItem>
              <FormLabel className='text-sm font-medium text-slate-700'>{t('auth.signUp.email')}</FormLabel>
              <FormControl>
                <Input
                  type='email'
                  placeholder='name@example.com'
                  className='border-slate-300 !bg-white text-slate-800 transition-all duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:!bg-white focus:ring-2 focus:ring-slate-200'
                  {...field}
                />
              </FormControl>
              <FormMessage className='text-red-600' />
            </FormItem>
          )}
        />
        <FormField
          control={form.control}
          name='password'
          render={({ field }) => (
            <FormItem>
              <FormLabel className='text-sm font-medium text-slate-700'>{t('auth.signUp.password')}</FormLabel>
              <FormControl>
                <PasswordInput
                  placeholder='********'
                  className='border-slate-300 bg-white text-slate-800 transition-all duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:bg-white focus:ring-2 focus:ring-slate-200'
                  {...field}
                />
              </FormControl>
              <FormMessage className='text-red-600' />
            </FormItem>
          )}
        />
        <FormField
          control={form.control}
          name='confirmPassword'
          render={({ field }) => (
            <FormItem>
              <FormLabel className='text-sm font-medium text-slate-700'>{t('users.form.confirmPassword')}</FormLabel>
              <FormControl>
                <PasswordInput
                  placeholder='********'
                  className='border-slate-300 bg-white text-slate-800 transition-all duration-300 placeholder:text-slate-400 focus:border-slate-500 focus:bg-white focus:ring-2 focus:ring-slate-200'
                  {...field}
                />
              </FormControl>
              <FormMessage className='text-red-600' />
            </FormItem>
          )}
        />
        <Button
          className='mt-3 w-full rounded-lg bg-slate-800 px-6 py-3 font-medium text-white shadow-lg transition-all duration-300 hover:bg-slate-700 hover:shadow-xl focus:ring-2 focus:ring-slate-500 focus:ring-offset-2 disabled:opacity-50'
          disabled={signUpMutation.isPending}
          data-testid='sign-up-submit'
        >
          {signUpMutation.isPending ? t('auth.signUp.submitting') : t('auth.signUp.submit')}
        </Button>

        {/* <div className='relative my-2'>
          <div className='absolute inset-0 flex items-center'>
            <span className='w-full border-t' />
          </div>
          <div className='relative flex justify-center text-xs uppercase'>
            <span className='bg-background text-muted-foreground px-2'>
              Or continue with
            </span>
          </div>
        </div>

        <div className='grid grid-cols-2 gap-2'>
          <Button
            variant='outline'
            className='w-full'
            type='button'
            disabled={isLoading}
          >
            <IconBrandGithub className='h-4 w-4' /> GitHub
          </Button>
          <Button
            variant='outline'
            className='w-full'
            type='button'
            disabled={isLoading}
          >
            <IconBrandFacebook className='h-4 w-4' /> Facebook
          </Button>
        </div> */}
      </form>
    </Form>
  );
}
