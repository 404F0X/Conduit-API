'use client';

import React, { useEffect, useState } from 'react';
import { CircleAlert, Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { useAuthStore } from '@/stores/authStore';
import { Button } from '@/components/ui/button';
import { Card } from '@/components/ui/card';
import { useOnboardingInfo } from '@/features/system/data/system';
import { AutoDisableChannelOnboardingFlow } from './auto-disable-channel-onboarding-flow';
import { FinancialSetupOnboarding } from './financial-setup-onboarding';
import { OnboardingFlow } from './onboarding-flow';

type OnboardingMode = 'none' | 'financial' | 'main' | 'autoDisableChannel';

interface OnboardingProviderProps {
  children: React.ReactNode;
  showOnboarding?: boolean;
  onComplete?: () => void;
}

export function OnboardingProvider({ children, showOnboarding = true, onComplete }: OnboardingProviderProps) {
  const { t } = useTranslation();
  const { data: onboardingInfo, isLoading, isError, refetch } = useOnboardingInfo();
  const [mode, setMode] = useState<OnboardingMode>('none');
  const [financialCompletedLocally, setFinancialCompletedLocally] = useState(false);
  const user = useAuthStore((state) => state.auth.user);
  const isOwner = user?.isOwner ?? false;

  useEffect(() => {
    if (!isLoading && showOnboarding && isOwner) {
      if (onboardingInfo?.financialSetup?.onboarded === false) {
        setMode('financial');
      } else if (!onboardingInfo || !onboardingInfo.onboarded) {
        setMode('main');
      } else if (!onboardingInfo.autoDisableChannel?.onboarded) {
        setMode('autoDisableChannel');
      } else {
        setMode('none');
      }
    }
  }, [onboardingInfo, isLoading, showOnboarding, isOwner]);

  const handleComplete = () => {
    setMode('none');
    onComplete?.();
  };

  const handleFinancialComplete = () => {
    setFinancialCompletedLocally(true);
    if (!onboardingInfo?.onboarded) setMode('main');
    else if (!onboardingInfo.autoDisableChannel?.onboarded) setMode('autoDisableChannel');
    else setMode('none');
  };

  if (showOnboarding && isOwner && isLoading) {
    return <OnboardingState icon={<Loader2 className='size-5 animate-spin' />} title={t('financialOnboarding.statusLoading')} />;
  }

  if (showOnboarding && isOwner && isError) {
    return (
      <OnboardingState
        icon={<CircleAlert className='size-5' />}
        title={t('financialOnboarding.statusError')}
        description={t('financialOnboarding.statusErrorDescription')}
        action={
          <Button className='h-10' onClick={() => refetch()}>
            {t('financialOnboarding.retry')}
          </Button>
        }
      />
    );
  }

  if (showOnboarding && isOwner && !financialCompletedLocally && onboardingInfo?.financialSetup?.onboarded === false) {
    return <FinancialSetupOnboarding onComplete={handleFinancialComplete} />;
  }

  return (
    <>
      {children}
      {mode === 'main' && <OnboardingFlow onComplete={handleComplete} />}
      {mode === 'autoDisableChannel' && <AutoDisableChannelOnboardingFlow onComplete={handleComplete} />}
    </>
  );
}

function OnboardingState({
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
        {description && <p className='text-muted-foreground mt-2 text-sm leading-6'>{description}</p>}
        {action && <div className='mt-5'>{action}</div>}
      </Card>
    </main>
  );
}
