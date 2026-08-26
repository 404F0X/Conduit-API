import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { Badge } from '@/components/ui/badge';
import type { ChannelSettings } from '../data/schema';

interface ChannelManagementAdapterBadgeProps {
  managementAdapter?: ChannelSettings['managementAdapter'];
  className?: string;
}

export function ChannelManagementAdapterBadge({ managementAdapter, className }: ChannelManagementAdapterBadgeProps) {
  const { t } = useTranslation();

  if (managementAdapter !== 'new_api') return null;

  return (
    <Badge
      variant='outline'
      data-testid='channel-management-adapter-new-api'
      className={cn(
        'shrink-0 border-amber-500/40 bg-amber-500/10 text-[10px] font-semibold tracking-wide text-amber-700 shadow-none dark:text-amber-300',
        className
      )}
    >
      {t('channels.managementAdapter.newApi')}
    </Badge>
  );
}
