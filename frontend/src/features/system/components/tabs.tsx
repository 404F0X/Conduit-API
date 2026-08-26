'use client';

import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { usePermissions } from '@/hooks/usePermissions';
import { Tabs, TabsList, TabsTrigger, TabsContent } from '@/components/ui/tabs';
import { AboutSettings } from './about-settings';
import { BackupSettings } from './backup-settings';
import { BrandSettings } from './brand-settings';
import { DiagnosticsSettings } from './diagnostics-settings';
import { GeneralSettings } from './general-settings';
import { ProxyPresetsSettings } from './proxy-presets-settings';
import { QuotaSettings } from './quota-settings';
import { RetrySettings } from './retry-settings';
import { SecuritySettings } from './security-settings';
import { StorageSettings } from './storage-settings';
import { WebhookSettings } from './webhook-settings';

type SystemTabKey =
  'general' | 'security' | 'brand' | 'storage' | 'retry' | 'webhook' | 'proxy' | 'quota' | 'backup' | 'diagnostics' | 'about';

interface SystemSettingsTabsProps {
  initialTab?: SystemTabKey;
}

export function SystemSettingsTabs({ initialTab }: SystemSettingsTabsProps) {
  const { t } = useTranslation();
  const { isOwner } = usePermissions();
  const [activeTab, setActiveTab] = useState<SystemTabKey>('general');
  const tabsViewportRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!initialTab) {
      return;
    }

    if (!isOwner && (initialTab === 'backup' || initialTab === 'diagnostics')) {
      setActiveTab('general');
      return;
    }

    setActiveTab(initialTab);
  }, [initialTab, isOwner]);

  useEffect(() => {
    const revealActiveTab = () => {
      const tabList = tabsViewportRef.current?.querySelector<HTMLElement>('[data-testid="system-settings-tabs"]');
      const activeTrigger = tabList?.querySelector<HTMLElement>(`[data-value="${activeTab}"]`);
      if (!tabList || !activeTrigger) return;

      const listBounds = tabList.getBoundingClientRect();
      const triggerBounds = activeTrigger.getBoundingClientRect();
      const edgePadding = 4;
      let nextScrollLeft = tabList.scrollLeft;

      if (triggerBounds.left < listBounds.left + edgePadding) {
        nextScrollLeft -= listBounds.left + edgePadding - triggerBounds.left;
      } else if (triggerBounds.right > listBounds.right - edgePadding) {
        nextScrollLeft += triggerBounds.right - (listBounds.right - edgePadding);
      }

      const maxScrollLeft = Math.max(0, tabList.scrollWidth - tabList.clientWidth);
      nextScrollLeft = Math.min(maxScrollLeft, Math.max(0, nextScrollLeft));
      if (Math.abs(nextScrollLeft - tabList.scrollLeft) >= 1) {
        tabList.scrollTo({ left: nextScrollLeft });
      }
    };
    const frame = window.requestAnimationFrame(revealActiveTab);
    window.addEventListener('resize', revealActiveTab);
    return () => {
      window.cancelAnimationFrame(frame);
      window.removeEventListener('resize', revealActiveTab);
    };
  }, [activeTab]);

  return (
    <Tabs
      value={activeTab}
      onValueChange={(value) => {
        const nextTab = value as SystemTabKey;
        if (!isOwner && (nextTab === 'backup' || nextTab === 'diagnostics')) {
          setActiveTab('general');
          return;
        }
        setActiveTab(nextTab);
      }}
      className='w-full'
    >
      <div ref={tabsViewportRef} className='min-w-0'>
        <TabsList
          data-testid='system-settings-tabs'
          className='shadow-soft border-border bg-background scrollbar-hide flex w-full scroll-px-1 justify-start overflow-x-auto rounded-2xl border px-1'
        >
          <TabsTrigger value='general' data-value='general' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.general')}
          </TabsTrigger>
          <TabsTrigger value='security' data-value='security' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.security')}
          </TabsTrigger>
          <TabsTrigger value='brand' data-value='brand' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.brand')}
          </TabsTrigger>
          <TabsTrigger value='retry' data-value='retry' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.retry')}
          </TabsTrigger>
          <TabsTrigger value='webhook' data-value='webhook' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.webhook')}
          </TabsTrigger>
          <TabsTrigger value='storage' data-value='storage' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.storage')}
          </TabsTrigger>
          <TabsTrigger value='proxy' data-value='proxy' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.proxy')}
          </TabsTrigger>
          <TabsTrigger value='quota' data-value='quota' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.quota')}
          </TabsTrigger>
          {isOwner && (
            <TabsTrigger value='diagnostics' data-value='diagnostics' className='flex-none shrink-0 md:flex-1'>
              {t('system.tabs.diagnostics')}
            </TabsTrigger>
          )}
          {isOwner && (
            <TabsTrigger value='backup' data-value='backup' className='flex-none shrink-0 md:flex-1'>
              {t('system.tabs.backup')}
            </TabsTrigger>
          )}
          <TabsTrigger value='about' data-value='about' className='flex-none shrink-0 md:flex-1'>
            {t('system.tabs.about')}
          </TabsTrigger>
        </TabsList>
      </div>
      <div className='shadow-soft border-border bg-card mt-6 rounded-2xl border p-4 sm:p-6'>
        <TabsContent value='general' className='mt-0 p-0'>
          <GeneralSettings />
        </TabsContent>
        <TabsContent value='security' className='mt-0 p-0'>
          <SecuritySettings />
        </TabsContent>
        <TabsContent value='brand' className='mt-0 p-0'>
          <BrandSettings />
        </TabsContent>
        <TabsContent value='storage' className='mt-0 p-0'>
          <StorageSettings />
        </TabsContent>
        <TabsContent value='retry' className='mt-0 p-0'>
          <RetrySettings />
        </TabsContent>
        <TabsContent value='webhook' className='mt-0 p-0'>
          <WebhookSettings />
        </TabsContent>
        <TabsContent value='proxy' className='mt-0 p-0'>
          <ProxyPresetsSettings />
        </TabsContent>
        <TabsContent value='quota' className='mt-0 p-0'>
          <QuotaSettings />
        </TabsContent>
        {isOwner && (
          <TabsContent value='diagnostics' className='mt-0 p-0'>
            <DiagnosticsSettings />
          </TabsContent>
        )}
        {isOwner && (
          <TabsContent value='backup' className='mt-0 p-0'>
            <BackupSettings />
          </TabsContent>
        )}
        <TabsContent value='about' className='mt-0 p-0'>
          <AboutSettings />
        </TabsContent>
      </div>
    </Tabs>
  );
}
