import { useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { IconArrowsSort, IconDots, IconForms, IconPlus, IconScale, IconSettings, IconUpload } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { PermissionGuard } from '@/components/permission-guard';
import { useChannels } from '../context/channels-context';
import { NewApiCreateDialog } from './new-api-discovery';

/**
 * 页头只留一个主按钮“新建渠道”（四个字段的快速新建），
 * 完整表单、批量导入、排序、渠道设置、负载均衡策略都收进“更多”。
 */
export function ChannelsPrimaryButtons() {
  const { t } = useTranslation();
  const [newApiOpen, setNewApiOpen] = useState(false);
  const navigate = useNavigate();
  const { setOpen } = useChannels();

  return (
    <div className='flex w-full items-center justify-end gap-2 lg:w-auto'>
      <NewApiCreateDialog key={newApiOpen ? 'open' : 'closed'} open={newApiOpen} onOpenChange={setNewApiOpen} />
      <PermissionGuard requiredSystemScope='write_channels'>
        <Button variant='outline' onClick={() => setNewApiOpen(true)}>
          {t('eventPackages.newApi')}
        </Button>
      </PermissionGuard>
      <PermissionGuard requiredScope='write_channels'>
        <Button className='min-w-0 flex-1 lg:flex-none' onClick={() => setOpen('quickAdd')} data-testid='add-channel-button'>
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
        <DropdownMenuContent align='end' className='w-56'>
          <PermissionGuard requiredScope='write_channels'>
            <>
              <DropdownMenuItem onClick={() => setOpen('add')}>
                <IconForms />
                {t('channels.quickCreate.useFullForm')}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setOpen('bulkImport')}>
                <IconUpload />
                {t('channels.importChannels')}
              </DropdownMenuItem>
              <DropdownMenuItem onClick={() => setOpen('bulkOrdering')}>
                <IconArrowsSort />
                {t('channels.orderChannels')}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onClick={() => setOpen('channelSettings')}>
                <IconSettings />
                {t('channels.actions.settings')}
              </DropdownMenuItem>
            </>
          </PermissionGuard>
          <PermissionGuard requiredSystemScope='read_settings'>
            <DropdownMenuItem onClick={() => navigate({ to: '/system', search: { tab: 'retry' } })}>
              <IconScale />
              {t('channels.loadBalancingStrategy')}
            </DropdownMenuItem>
          </PermissionGuard>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
