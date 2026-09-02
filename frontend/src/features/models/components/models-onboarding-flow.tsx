'use client';

import { useEffect } from 'react';
import { driver } from 'driver.js';
import 'driver.js/dist/driver.css';
import { useTranslation } from 'react-i18next';
import { useCompleteSystemModelSettingOnboarding } from '@/features/system/data/system';

interface ModelsOnboardingFlowProps {
  onComplete?: () => void;
}

export function ModelsOnboardingFlow({ onComplete }: ModelsOnboardingFlowProps) {
  const { t } = useTranslation();
  const { mutate: completeOnboarding } = useCompleteSystemModelSettingOnboarding();

  useEffect(() => {
    const settingsButton = document.querySelector('[data-settings-button]') as HTMLButtonElement;
    if (!settingsButton) {
      return;
    }

    let driverObj: ReturnType<typeof driver> | null = null;
    let animationFrame: number | null = null;
    let completed = false;

    // Register completion before the tour can expose its highlighted target.
    // driver.js calls `onHighlighted` only after its entrance animation, while
    // the target is interactive as soon as the popover is mounted. Installing
    // the listener here keeps that first click atomic: it always tears down the
    // overlay before the button's normal React handler opens model settings.
    const handleSettingsClick = () => {
      if (completed) return;
      completed = true;

      if (animationFrame !== null) {
        window.cancelAnimationFrame(animationFrame);
        animationFrame = null;
      }
      if (driverObj) {
        driverObj.destroy();
        driverObj = null;
      }

      completeOnboarding(undefined, {
        onSuccess: () => {
          onComplete?.();
        },
      });
    };

    settingsButton.addEventListener('click', handleSettingsClick, { once: true });

    // Wait for one paint so driver.js measures the committed button layout.
    // The completion listener above is already active during this boundary.
    animationFrame = window.requestAnimationFrame(() => {
      animationFrame = null;
      if (completed) return;

      driverObj = driver({
        showProgress: false,
        showButtons: [],
        allowClose: false,
        steps: [
          {
            element: '[data-settings-button]',
            popover: {
              title: t('models.onboarding.steps.settingsButton.title'),
              description: t('models.onboarding.steps.settingsButton.description'),
              side: 'bottom',
              align: 'end',
              showButtons: [],
            },
          },
        ],
      });
      driverObj.drive();
    });

    return () => {
      if (animationFrame !== null) {
        window.cancelAnimationFrame(animationFrame);
      }
      settingsButton.removeEventListener('click', handleSettingsClick);
      if (driverObj) {
        driverObj.destroy();
      }
    };
  }, [completeOnboarding, onComplete, t]);

  return null;
}
