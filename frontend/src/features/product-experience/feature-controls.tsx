import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { usePermissions } from '@/hooks/usePermissions';
import { useProductExperience } from './context';
import { type FeatureOverride, useProductExperienceSettings, useUpdateProductExperienceSettings } from './data';

export function FeatureControls() {
  const { t } = useTranslation();
  const { isOwner } = usePermissions();
  const { mode } = useProductExperience();
  const settings = useProductExperienceSettings();
  const update = useUpdateProductExperienceSettings();
  const execution = new Set([
    'operations_reports',
    'automatic_model_sync',
    'automatic_provider_observation',
    'redemption_issuance',
    'subscription_management',
  ]);
  const save = async (id: string, field: 'visibility' | 'execution', value: FeatureOverride) => {
    try {
      await update.mutateAsync({ mode, expectedRevision: settings.data?.revision, overrides: [{ id, [field]: value }] });
      toast.success(t('system.productFeatures.saved'));
    } catch (error) {
      toast.error(error instanceof Error ? error.message : t('system.productFeatures.failed'));
    }
  };
  return (
    <details className='mt-4 rounded-lg border p-3'>
      <summary className='flex min-h-10 cursor-pointer items-center font-medium'>{t('system.productFeatures.title')}</summary>
      <p className='text-muted-foreground py-3 text-sm'>{t('system.productFeatures.description')}</p>
      <div className='space-y-3'>
        {settings.data?.features.map((feature) => (
          <div key={feature.id} className='grid items-center gap-2 sm:grid-cols-[1fr_9rem_9rem]'>
            <label htmlFor={`visibility-${feature.id}`} className='text-sm'>
              {t(`system.productFeatures.labels.${feature.id}`)}
            </label>
            {(['visibility', 'execution'] as const).map((field) =>
              field === 'execution' && !execution.has(feature.id) ? (
                <span key={field} />
              ) : (
                <select
                  key={field}
                  id={`${field}-${feature.id}`}
                  className='bg-background min-h-10 rounded-md border px-2 text-sm'
                  aria-label={`${t(`system.productFeatures.${field}`)}: ${t(`system.productFeatures.labels.${feature.id}`)}`}
                  value={feature[field]}
                  disabled={!isOwner || update.isPending}
                  onChange={(event) => void save(feature.id, field, event.target.value as FeatureOverride)}
                >
                  {(['INHERIT', 'ON', 'OFF'] as const).map((option) => (
                    <option key={option} value={option}>
                      {t(`system.productFeatures.${option}`)}
                    </option>
                  ))}
                </select>
              )
            )}
          </div>
        ))}
      </div>
    </details>
  );
}
