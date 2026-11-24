import { DotsHorizontalIcon } from '@radix-ui/react-icons';
import { useNavigate } from '@tanstack/react-router';
import { IconArchive, IconCheck, IconCopy, IconHistory, IconKeyOff, IconListDetails, IconTrash } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { usePermissions } from '@/hooks/usePermissions';
import { Button } from '@/components/ui/button';
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu';
import { useChannels } from '../context/channels-context';
import type { Channel } from '../data/schema';

type ChannelOverflowMenuProps = {
  channel: Channel;
  className?: string;
  showLabel?: boolean;
  testId?: string;
};

export function ChannelOverflowMenu({ channel, className, showLabel = false, testId = 'row-actions' }: ChannelOverflowMenuProps) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { setOpen, setCurrentRow, setSelectedPriceModelID } = useChannels();
  const { channelPermissions, hasSystemScope } = usePermissions();
  const isArchived = channel.status === 'archived';
  const hasError = !!channel.errorMessage;
  const hasDisabledAPIKeys = channelPermissions.canWrite && (channel.disabledAPIKeys?.length ?? 0) > 0;

  const openDialog = (dialog: Parameters<typeof setOpen>[0]) => {
    setCurrentRow(channel);
    if (dialog === 'price') setSelectedPriceModelID(null);
    setOpen(dialog);
  };

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          size='sm'
          variant='outline'
          className={cn('h-8', showLabel ? 'gap-2 px-3' : 'w-8 p-0', className)}
          data-testid={testId}
          aria-label={t('channels.actions.more')}
          title={t('channels.actions.more')}
        >
          <DotsHorizontalIcon className='h-3 w-3' />
          {showLabel && <span>{t('common.columns.actions')}</span>}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align='end' className='w-[200px]'>
        {/* 以前这里有 19 项；按用途分到渠道详情页的标签里，行菜单只留最常用的几项 */}
        <DropdownMenuItem onClick={() => navigate({ to: '/channels/$channelId', params: { channelId: channel.id } })}>
          <IconListDetails size={16} className='mr-2' />
          {t('channels.detail.openDetail')}
        </DropdownMenuItem>
        {hasSystemScope('write_commercialization') && (
          <DropdownMenuItem onClick={() => openDialog('price')}>{t('channels.actions.modelPrice')}</DropdownMenuItem>
        )}
        <DropdownMenuItem onClick={() => openDialog('testHistory')}>
          <IconHistory size={16} className='mr-2' />
          {t('channels.actions.testHistory')}
        </DropdownMenuItem>
        {channelPermissions.canWrite && (
          <DropdownMenuItem onClick={() => openDialog('duplicate')}>
            <IconCopy size={16} className='mr-2' />
            {t('common.actions.duplicate')}
          </DropdownMenuItem>
        )}
        {hasDisabledAPIKeys && (
          <DropdownMenuItem onClick={() => openDialog('disabledAPIKeys')} className='text-orange-500!'>
            <IconKeyOff size={16} className='mr-2' />
            {t('channels.actions.disabledAPIKeys', { count: channel.disabledAPIKeys?.length ?? 0 })}
          </DropdownMenuItem>
        )}
        {hasError && (
          <DropdownMenuItem onClick={() => openDialog('errorResolved')} className='text-green-600!'>
            <IconCheck size={16} className='mr-2' />
            {t('channels.actions.markErrorResolved')}
          </DropdownMenuItem>
        )}
        {channelPermissions.canWrite && (
          <>
            <DropdownMenuSeparator />
            <DropdownMenuItem onClick={() => openDialog('archive')} className={isArchived ? 'text-green-600!' : 'text-orange-500!'}>
              {isArchived ? <IconCheck size={16} className='mr-2' /> : <IconArchive size={16} className='mr-2' />}
              {t(isArchived ? 'common.buttons.restore' : 'common.buttons.archive')}
            </DropdownMenuItem>
            <DropdownMenuItem onClick={() => openDialog('delete')} className='text-red-500!'>
              <IconTrash size={16} className='mr-2' />
              {t('common.buttons.delete')}
            </DropdownMenuItem>
          </>
        )}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
