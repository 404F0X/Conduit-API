import { useEffect } from 'react';
import { useQueryClient } from '@tanstack/react-query';
import { useSearch } from '@tanstack/react-router';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { useOIDCProviders } from '@/features/auth/data/auth';
import { AppearanceForm } from '../appearance/appearance-form';
import ContentSection from '../components/content-section';
import OidcManagement from '../security/oidc-management';
import SecurityForm from '../security/security-form';
import ProfileForm from './profile-form';

export default function SettingsProfile() {
  const { t } = useTranslation();
  const { data: providers = [] } = useOIDCProviders();
  // 这个页面同时挂在 /settings 和 /settings/profile 上，按固定路由取参数会在 /settings 上报错
  const search = useSearch({ strict: false }) as { oidc_link?: string };
  const queryClient = useQueryClient();

  useEffect(() => {
    if (search.oidc_link === 'success') {
      toast.success(t('security.oidc.linkSuccess', 'Successfully linked provider!'));
      queryClient.invalidateQueries({ queryKey: ['oidc-providers'] });
      queryClient.invalidateQueries({ queryKey: ['me'] });
    }
  }, [search, queryClient, t]);

  return (
    <ContentSection title={t('profile.title')} desc={t('profile.description')}>
      <Tabs defaultValue='profile' className='w-full'>
        <TabsList className='mb-4'>
          <TabsTrigger value='profile'>{t('profile.title')}</TabsTrigger>
          <TabsTrigger value='security'>{t('security.title', 'Security')}</TabsTrigger>
          {providers.length > 0 && <TabsTrigger value='oidc'>{t('simple.settings.oidc')}</TabsTrigger>}
          <TabsTrigger value='appearance'>{t('simple.settings.appearance')}</TabsTrigger>
        </TabsList>
        <TabsContent value='profile'>
          <ProfileForm />
        </TabsContent>
        <TabsContent value='security'>
          <SecurityForm />
        </TabsContent>
        {providers.length > 0 && (
          <TabsContent value='oidc'>
            <OidcManagement providers={providers} />
          </TabsContent>
        )}
        <TabsContent value='appearance'>
          <AppearanceForm />
        </TabsContent>
      </Tabs>
    </ContentSection>
  );
}
