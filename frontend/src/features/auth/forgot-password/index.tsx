import { Link } from '@tanstack/react-router';
import { useTranslation } from 'react-i18next';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import AuthLayout from '../auth-layout';

export default function ForgotPassword() {
  const { t } = useTranslation();
  return (
    <AuthLayout>
      <Card className='gap-4'>
        <CardHeader>
          <CardTitle className='text-lg tracking-tight'>{t('auth.passwordRecovery.title')}</CardTitle>
          <CardDescription>{t('auth.passwordRecovery.unavailable')}</CardDescription>
        </CardHeader>
        <CardContent>
          <p className='text-muted-foreground mb-4 text-sm'>{t('auth.passwordRecovery.contactAdmin')}</p>
          <Link to='/sign-in' className='hover:text-primary underline underline-offset-4'>
            {t('auth.passwordRecovery.backToSignIn')}
          </Link>
        </CardContent>
      </Card>
    </AuthLayout>
  );
}
