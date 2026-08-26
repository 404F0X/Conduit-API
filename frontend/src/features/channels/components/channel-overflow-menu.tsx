import { DotsHorizontalIcon } from '@radix-ui/react-icons';
import { useNavigate } from '@tanstack/react-router';
import {
  IconActivity,
  IconAdjustments,
  IconArchive,
  IconBraces,
  IconCheck,
  IconCoin,
  IconCopy,
  IconGauge,
  IconHistory,
  IconKeyOff,
  IconNetwork,
  IconPlayerPlay,
  IconPlugConnected,
  IconRoute,
  IconTransform,
  IconTrash,
  IconWallet,
} from '@tabler/icons-react';
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
  const apiKeysCount = channel.credentials?.apiKeys?.filter((key) => key.trim().length > 0).length ?? 0;
  const hasMultipleAPIKeys = channelPermissions.canWrite && apiKeysCount > 1;

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
      <DropdownMenuContent align='end' className='w-[220px]'>
        <DropdownMenuItem onClick={() => openDialog('operations')}>
          <IconActivity size={16} className='mr-2' />
          {t('channels.operations.action')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('test')}>
          <IconPlayerPlay size={16} className='mr-2' />
          {t('channels.actions.test')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('testHistory')}>
          <IconHistory size={16} className='mr-2' />
          {t('channels.actions.testHistory')}
        </DropdownMenuItem>
        <DropdownMenuSeparator />

        <DropdownMenuItem onClick={() => openDialog('duplicate')}>
          <IconCopy size={16} className='mr-2' />
          {t('common.actions.duplicate')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('modelMapping')}>
          <IconRoute size={16} className='mr-2' />
          {t('channels.dialogs.settings.modelMapping.title')}
        </DropdownMenuItem>
        {hasSystemScope('write_commercialization') && (
          <DropdownMenuItem onClick={() => openDialog('price')}>
            <IconCoin size={16} className='mr-2' />
            {t('channels.actions.modelPrice')}
          </DropdownMenuItem>
        )}
        {hasSystemScope('read_commercialization') && (
          <DropdownMenuItem
            onClick={() =>
              navigate({
                to: '/change-sets',
                search: { scopeType: 'channel', scopeID: channel.id },
              })
            }
          >
            <IconHistory size={16} className='mr-2' />
            {t('channels.changeSets.action')}
          </DropdownMenuItem>
        )}
        <DropdownMenuItem onClick={() => openDialog('overrides')}>
          <IconAdjustments size={16} className='mr-2' />
          {t('channels.dialogs.settings.overrides.action')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('proxy')}>
          <IconNetwork size={16} className='mr-2' />
          {t('channels.dialogs.proxy.action')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('transformOptions')}>
          <IconTransform size={16} className='mr-2' />
          {t('channels.dialogs.transformOptions.action')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('rateLimit')}>
          <IconGauge size={16} className='mr-2' />
          {t('channels.dialogs.rateLimit.action')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('automation')}>
          <IconBraces size={16} className='mr-2' />
          {t('channels.dialogs.automation.action')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('endpoints')}>
          <IconPlugConnected size={16} className='mr-2' />
          {t('channels.endpoints.title')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('quotaProbe')}>
          <IconWallet size={16} className='mr-2' />
          {t('channels.actions.queryUpstreamQuota')}
        </DropdownMenuItem>
        {hasMultipleAPIKeys && (
          <DropdownMenuItem onClick={() => openDialog('testAPIKeys')}>
            <IconPlayerPlay size={16} className='mr-2' />
            {t('channels.actions.testAPIKeys', { count: apiKeysCount })}
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
        <DropdownMenuSeparator />
        <DropdownMenuItem onClick={() => openDialog('archive')} className={isArchived ? 'text-green-600!' : 'text-orange-500!'}>
          {isArchived ? <IconCheck size={16} className='mr-2' /> : <IconArchive size={16} className='mr-2' />}
          {t(isArchived ? 'common.buttons.restore' : 'common.buttons.archive')}
        </DropdownMenuItem>
        <DropdownMenuItem onClick={() => openDialog('delete')} className='text-red-500!'>
          <IconTrash size={16} className='mr-2' />
          {t('common.buttons.delete')}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  );
}
