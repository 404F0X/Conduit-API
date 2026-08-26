import { useState, useCallback, useEffect } from 'react';
import { IconPlus, IconSettings, IconAlertCircle } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { PermissionGuard } from '@/components/permission-guard';
import { PricingDisplayToggle } from '@/components/pricing-display-toggle';
import { ChannelsModelPriceDialog } from '@/features/channels/components/channels-model-price-dialog';
import ChannelsProvider from '@/features/channels/context/channels-context';
import { useOnboardingInfo } from '@/features/system/data/system';
import { ModelCatalog } from './components/model-catalog';
import { ModelsDialogs } from './components/models-dialogs';
import { ModelsOnboardingFlow } from './components/models-onboarding-flow';
import ModelsProvider, { useModels } from './context/models-context';
import { useQueryAllModels } from './data/models';
import { useDevelopersData } from './data/providers';

function ModelsContent() {
  const { data: providers } = useDevelopersData();
  const { data, isLoading } = useQueryAllModels({});

  return (
    <ModelCatalog
      models={data?.edges?.map((edge) => edge.node) || []}
      modelsLoading={isLoading}
      modelsTotalCount={data?.totalCount}
      providers={providers}
    />
  );
}

function CreateButton() {
  const { t } = useTranslation();
  const { setOpen } = useModels();

  return (
    <Button className='order-first' onClick={() => setOpen('create')}>
      <IconPlus className='mr-2 h-4 w-4' />
      {t('models.actions.create')}
    </Button>
  );
}

function BulkAddButton() {
  const { t } = useTranslation();
  const { setOpen } = useModels();

  return (
    <Button variant='outline' onClick={() => setOpen('batchCreate')}>
      <IconPlus className='mr-2 h-4 w-4' />
      {t('models.actions.bulkAdd')}
    </Button>
  );
}

function SettingsButton() {
  const { t } = useTranslation();
  const { setOpen } = useModels();

  return (
    <Button variant='outline' onClick={() => setOpen('settings')} data-settings-button>
      <IconSettings className='mr-2 h-4 w-4' />
      {t('models.actions.settings')}
    </Button>
  );
}

function DetectUnassociatedButton() {
  const { t } = useTranslation();
  const { setOpen } = useModels();

  return (
    <Button variant='outline' onClick={() => setOpen('unassociated')}>
      <IconAlertCircle className='mr-2 h-4 w-4' />
      {t('models.actions.detectUnassociated')}
    </Button>
  );
}

function ActionButtons() {
  return (
    <div className='flex w-full flex-wrap items-center justify-end gap-2 md:w-auto'>
      <PricingDisplayToggle />
      <PermissionGuard requiredScope='write_channels'>
        <div className='grid flex-1 grid-cols-2 gap-2 sm:grid-cols-4 md:flex md:flex-none md:flex-wrap'>
          <DetectUnassociatedButton />
          <SettingsButton />
          <BulkAddButton />
          <CreateButton />
        </div>
      </PermissionGuard>
    </div>
  );
}

export default function ModelsManagement() {
  const { t } = useTranslation();
  const { data: onboardingInfo } = useOnboardingInfo();
  const [showOnboarding, setShowOnboarding] = useState(false);

  const shouldShowOnboarding = onboardingInfo && !onboardingInfo.systemModelSetting?.onboarded;

  useEffect(() => {
    if (shouldShowOnboarding) {
      setShowOnboarding(true);
    }
  }, [shouldShowOnboarding]);

  const handleOnboardingComplete = useCallback(() => {
    setShowOnboarding(false);
  }, []);

  return (
    <ChannelsProvider>
      <ModelsProvider>
        <Header className='h-auto shrink-0 items-start xl:items-center'>
          <div className='flex w-full flex-1 flex-col gap-3 md:flex-row md:items-start md:justify-between md:gap-4 xl:items-center'>
            <div className='min-w-0'>
              <h2 className='text-xl font-bold tracking-tight'>{t('models.title')}</h2>
              <p className='text-muted-foreground text-sm'>{t('models.description')}</p>
            </div>
            <ActionButtons />
          </div>
        </Header>

        <Main fixed className='overflow-hidden'>
          <ModelsContent />
        </Main>
        <ModelsDialogs />
        <ChannelsModelPriceDialog />
        {showOnboarding && <ModelsOnboardingFlow onComplete={handleOnboardingComplete} />}
      </ModelsProvider>
    </ChannelsProvider>
  );
}
