import { useNavigate } from '@tanstack/react-router';
import { IconPlus, IconUpload, IconArrowsSort, IconSettings, IconScale, IconDots } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu';
import { PermissionGuard } from '@/components/permission-guard';
import { useChannels } from '../context/channels-context';

export function ChannelsPrimaryButtons() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { setOpen } = useChannels();

  return (
    <div className='w-full lg:w-auto'>
      <div className='hidden flex-wrap justify-end gap-2 xl:flex'>
        <PermissionGuard requiredSystemScope='read_settings'>
          {/* Load Balancing Strategy - navigate to system retry configuration */}
          <Button variant='outline' className='shrink-0 space-x-1' onClick={() => navigate({ to: '/system', search: { tab: 'retry' } })}>
            <span>{t('channels.loadBalancingStrategy')}</span> <IconScale size={18} />
          </Button>
        </PermissionGuard>

        <PermissionGuard requiredScope='write_channels'>
          <>
            {/* Settings - requires write_channels permission */}
            <Button variant='outline' className='shrink-0 space-x-1' onClick={() => setOpen('channelSettings')}>
              <span>{t('channels.actions.settings')}</span> <IconSettings size={18} />
            </Button>

            {/* Bulk Import - requires write_channels permission */}
            <Button variant='outline' className='shrink-0 space-x-1' onClick={() => setOpen('bulkImport')}>
              <span>{t('channels.importChannels', '批量导入')}</span> <IconUpload size={18} />
            </Button>

            {/* Bulk Ordering - requires write_channels permission */}
            <Button variant='outline' className='shrink-0 space-x-1' onClick={() => setOpen('bulkOrdering')}>
              <span>{t('channels.orderChannels')}</span> <IconArrowsSort size={18} />
            </Button>

            {/* Add Channel - requires write_channels permission */}
            <Button className='shrink-0 space-x-1' onClick={() => setOpen('add')} data-testid='add-channel-button'>
              <span>{t('channels.addChannel')}</span> <IconPlus size={18} />
            </Button>
          </>
        </PermissionGuard>
      </div>
      <div className='flex items-center justify-end gap-2 xl:hidden'>
        <PermissionGuard requiredScope='write_channels'>
          <Button className='min-w-0 flex-1' onClick={() => setOpen('add')} data-testid='add-channel-button-mobile'>
            <IconPlus size={18} />
            <span className='truncate'>{t('channels.addChannel')}</span>
          </Button>
        </PermissionGuard>
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button variant='outline' size='icon' aria-label={t('channels.actions.more')}>
              <IconDots size={18} />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align='end' className='w-52'>
            <PermissionGuard requiredSystemScope='read_settings'>
              <DropdownMenuItem onClick={() => navigate({ to: '/system', search: { tab: 'retry' } })}>
                <IconScale />
                {t('channels.loadBalancingStrategy')}
              </DropdownMenuItem>
            </PermissionGuard>
            <PermissionGuard requiredScope='write_channels'>
              <>
                <DropdownMenuItem onClick={() => setOpen('channelSettings')}>
                  <IconSettings />
                  {t('channels.actions.settings')}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => setOpen('bulkImport')}>
                  <IconUpload />
                  {t('channels.importChannels')}
                </DropdownMenuItem>
                <DropdownMenuItem onClick={() => setOpen('bulkOrdering')}>
                  <IconArrowsSort />
                  {t('channels.orderChannels')}
                </DropdownMenuItem>
              </>
            </PermissionGuard>
          </DropdownMenuContent>
        </DropdownMenu>
      </div>
    </div>
  );
}
