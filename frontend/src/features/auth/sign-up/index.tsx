import { Link } from '@tanstack/react-router';
import { useTranslation } from 'react-i18next';
import AuthLayout from '../auth-layout';
import TwoColumnAuth from '../components/two-column-auth';
import { SignUpForm } from './components/sign-up-form';

export default function SignUp() {
  const { t } = useTranslation();

  return (
    <AuthLayout>
      <TwoColumnAuth
        title={t('auth.signUp.title')}
        description={
          <>
            {t('auth.signUp.subtitle')}{' '}
            <Link to='/sign-in' className='font-medium text-slate-800 underline underline-offset-4 hover:text-slate-600'>
              {t('auth.signUp.signIn')}
            </Link>
          </>
        }
        rightFooter={<p className='text-xs leading-relaxed text-slate-500 sm:text-sm'>{t('auth.signUp.footer.agreement')}</p>}
      >
        <SignUpForm />
      </TwoColumnAuth>
    </AuthLayout>
  );
}
