import { IconCash, IconCoin } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { usePricingDisplayStore, type PricingDisplayMode } from '@/stores/pricingDisplayStore';
import { cn } from '@/lib/utils';
import { usePermissions } from '@/hooks/usePermissions';
import { useGeneralSettings } from '@/features/system/data/system';

export function PricingDisplayToggle() {
  const { t } = useTranslation();
  const { data: settings } = useGeneralSettings();
  const { hasSystemScope } = usePermissions();
  const mode = usePricingDisplayStore((state) => state.mode);
  const setMode = usePricingDisplayStore((state) => state.setMode);
  const options: Array<{ value: PricingDisplayMode; label: string; icon: typeof IconCash }> = [
    {
      value: 'accounting',
      label: settings?.accountingCurrencyCode || t('system.accounting.displayMode.accounting'),
      icon: IconCash,
    },
    {
      value: 'credits',
      label: settings?.creditDisplayName || t('system.accounting.displayMode.credits'),
      icon: IconCoin,
    },
  ];

  if (!hasSystemScope('read_commercialization')) return null;

  return (
    <div
      className='bg-muted/50 inline-flex h-9 max-w-full items-center rounded-md border p-0.5'
      role='group'
      aria-label={t('system.accounting.displayMode.label')}
    >
      {options.map((option) => {
        const Icon = option.icon;
        const selected = mode === option.value;
        return (
          <button
            key={option.value}
            type='button'
            aria-pressed={selected}
            onClick={() => setMode(option.value)}
            className={cn(
              'focus-visible:ring-ring inline-flex h-7 min-w-0 items-center gap-1.5 rounded-sm px-2 text-xs font-medium transition-colors focus-visible:ring-2 focus-visible:outline-none',
              selected ? 'bg-background text-foreground shadow-sm' : 'text-muted-foreground hover:text-foreground'
            )}
          >
            <Icon className='size-3.5 shrink-0' />
            <span className='max-w-28 truncate'>{option.label}</span>
          </button>
        );
      })}
    </div>
  );
}
