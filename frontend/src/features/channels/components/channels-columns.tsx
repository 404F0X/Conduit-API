import { useCallback, useState, memo, useRef, useEffect } from 'react';
import { format } from 'date-fns';
import { ColumnDef, Row, Table } from '@tanstack/react-table';
import {
  IconPlayerPlay,
  IconChevronDown,
  IconChevronRight,
  IconAlertTriangle,
  IconEdit,
  IconLoader2,
  IconKeyOff,
} from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Input } from '@/components/ui/input';
import { Switch } from '@/components/ui/switch';
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip';
import { DataTableColumnHeader } from '@/components/data-table-column-header';
import { useChannels } from '../context/channels-context';
import { useTestChannel, useUpdateChannel } from '../data/channels';
import { CHANNEL_CONFIGS, getProvider } from '../data/config_channels';
import { Channel } from '../data/schema';
import { isNewApiChannelTag } from '../utils/channel-management-adapter';
import { ChannelHealthCell } from './channel-health-cell';
import { ChannelLimiterCell } from './channel-limiter-cell';
import { ChannelManagementAdapterBadge } from './channel-management-adapter-badge';
import { ChannelOverflowMenu } from './channel-overflow-menu';
import { ChannelsStatusDialog } from './channels-status-dialog';

const WEIGHT_PRECISION = 4;
const MIN_WEIGHT = 0;
const MAX_WEIGHT = 100;

const formatWeight = (value: number) => Number(value.toFixed(WEIGHT_PRECISION));
const clampWeight = (value: number) => formatWeight(Math.min(MAX_WEIGHT, Math.max(MIN_WEIGHT, value)));

// Status Switch Cell Component to handle status toggle with confirmation dialog
const StatusSwitchCell = memo(({ row }: { row: Row<Channel> }) => {
  const channel = row.original;
  const [dialogOpen, setDialogOpen] = useState(false);

  const isEnabled = channel.status === 'enabled';
  const isArchived = channel.status === 'archived';

  const handleSwitchClick = useCallback(() => {
    if (!isArchived) {
      setDialogOpen(true);
    }
  }, [isArchived]);

  return (
    <div className='flex justify-center'>
      <Switch checked={isEnabled} onCheckedChange={handleSwitchClick} disabled={isArchived} data-testid='channel-status-switch' />
      {dialogOpen && <ChannelsStatusDialog open={dialogOpen} onOpenChange={setDialogOpen} currentRow={channel} />}
    </div>
  );
});

StatusSwitchCell.displayName = 'StatusSwitchCell';

// Action Cell Component to handle hooks properly
const ActionCell = memo(({ row }: { row: Row<Channel> }) => {
  const channel = row.original;
  const { setOpen, setCurrentRow } = useChannels();
  const testChannel = useTestChannel();

  const handleDefaultTest = async () => {
    try {
      await testChannel.mutateAsync({
        channelID: channel.id,
        modelID: channel.defaultTestModel || undefined,
      });
    } catch (_error) {
      // The mutation hook already reports the provider test failure.
    }
  };

  const handleEdit = useCallback(() => {
    setCurrentRow(channel);
    setOpen('edit');
  }, [channel, setCurrentRow, setOpen]);

  return (
    <div className='flex items-center justify-center gap-1'>
      <Button size='sm' variant='outline' className='h-8 w-8 p-0' onClick={handleEdit}>
        <IconEdit className='h-3 w-3' />
      </Button>
      <Button size='sm' variant='outline' className='h-8 px-3' onClick={handleDefaultTest} disabled={testChannel.isPending}>
        <IconPlayerPlay className='mr-1 h-3 w-3' />
      </Button>
      <ChannelOverflowMenu channel={channel} />
    </div>
  );
});

ActionCell.displayName = 'ActionCell';

const ExpandCell = ({ row }: { row: any }) => (
  <div className='flex justify-center'>
    <Button
      variant='ghost'
      size='sm'
      className='h-6 w-6 p-0'
      onClick={(e) => {
        e.stopPropagation();
        row.toggleExpanded();
      }}
    >
      {row.getIsExpanded() ? <IconChevronDown className='h-4 w-4' /> : <IconChevronRight className='h-4 w-4' />}
    </Button>
  </div>
);

// ExpandCell.displayName = 'ExpandCell'; // Removed since it's not memoized now, but can keep if desired

function getProxyURLSummary(proxyURL: string): { label: string; detail?: string } {
  try {
    const url = new URL(proxyURL);
    const pathname = url.pathname === '/' ? '' : url.pathname;
    return {
      label: url.host || proxyURL,
      detail: `${url.protocol}//${url.host}${pathname}`,
    };
  } catch {
    return { label: proxyURL };
  }
}

// Memoized cell components to avoid recreating on every render
const NameCell = memo(({ row }: { row: Row<Channel> }) => {
  const { t } = useTranslation();
  const channel = row.original;
  const hasError = !!channel.errorMessage;
  const disabledKeysCount = channel.disabledAPIKeys?.length ?? 0;
  const hasDisabledKeys = disabledKeysCount > 0;
  const websiteURL = channel.websiteURL;

  const nameElement = websiteURL ? (
    <a
      href={websiteURL}
      target='_blank'
      rel='noopener noreferrer'
      className={cn('min-w-0 truncate font-medium hover:underline', hasError ? 'text-destructive' : '')}
      onClick={(e) => e.stopPropagation()}
    >
      {row.getValue('name')}
    </a>
  ) : (
    <div className={cn('min-w-0 truncate font-medium', hasError && 'text-destructive')}>{row.getValue('name')}</div>
  );

  const content = (
    <div className='flex justify-center'>
      <div className='flex max-w-64 items-center gap-2'>
        {hasError && <IconAlertTriangle className='text-destructive h-4 w-4 shrink-0' />}
        {!hasError && hasDisabledKeys && <IconKeyOff className='h-4 w-4 shrink-0 text-amber-500' />}
        {nameElement}
        <ChannelManagementAdapterBadge managementAdapter={channel.settings?.managementAdapter} />
      </div>
    </div>
  );

  if (hasError) {
    return (
      <Tooltip>
        <TooltipTrigger asChild>{content}</TooltipTrigger>
        <TooltipContent>
          <div className='space-y-1'>
            <p className='text-destructive text-sm'>
              {t(`channels.messages.${channel.errorMessage}`, {
                defaultValue: channel.errorMessage,
              })}
            </p>
          </div>
        </TooltipContent>
      </Tooltip>
    );
  }

  if (hasDisabledKeys) {
    return (
      <Tooltip>
        <TooltipTrigger asChild>{content}</TooltipTrigger>
        <TooltipContent>
          <p className='text-sm text-amber-500'>{t('channels.actions.disabledAPIKeys', { count: disabledKeysCount })}</p>
        </TooltipContent>
      </Tooltip>
    );
  }

  return content;
});

NameCell.displayName = 'NameCell';

const ProviderCell = memo(({ row }: { row: Row<Channel> }) => {
  const { t } = useTranslation();
  const type = row.original.type;
  const config = CHANNEL_CONFIGS[type];
  const provider = getProvider(type);
  const IconComponent = config.icon;
  return (
    <div className='flex justify-center'>
      <Badge variant='outline' className={cn('capitalize', config.color)}>
        <div className='flex items-center gap-2'>
          <IconComponent size={16} className='shrink-0' />
          <span>{t(`channels.providers.${provider}`)}</span>
        </div>
      </Badge>
    </div>
  );
});

ProviderCell.displayName = 'ProviderCell';

const TagsCell = memo(({ row }: { row: Row<Channel> }) => {
  const channel = row.original;
  const isNewApi = channel.settings?.managementAdapter === 'new_api';
  const tags = ((row.getValue('tags') as string[]) || []).filter((tag) => !isNewApi || !isNewApiChannelTag(tag));

  if (tags.length === 0 && !isNewApi) {
    return (
      <div className='flex justify-center'>
        <span className='text-muted-foreground text-xs'>-</span>
      </div>
    );
  }
  return (
    <div className='flex max-w-48 flex-wrap justify-center gap-1'>
      <ChannelManagementAdapterBadge managementAdapter={channel.settings?.managementAdapter} />
      {tags.slice(0, isNewApi ? 1 : 2).map((tag) => (
        <Badge key={tag} variant='outline' className='text-xs'>
          {tag}
        </Badge>
      ))}
      {tags.length > (isNewApi ? 1 : 2) && (
        <Badge variant='outline' className='text-xs'>
          +{tags.length - (isNewApi ? 1 : 2)}
        </Badge>
      )}
    </div>
  );
});

TagsCell.displayName = 'TagsCell';

const ProxyCell = memo(({ row }: { row: Row<Channel> }) => {
  const { t } = useTranslation();
  const proxy = row.original.settings?.proxy;

  if (!proxy || proxy.type === 'disabled') {
    return (
      <div className='flex justify-center'>
        <span className='text-muted-foreground text-xs'>-</span>
      </div>
    );
  }

  if (proxy.type === 'environment') {
    return (
      <div className='flex justify-center'>
        <span className='text-muted-foreground text-xs'>{t('channels.dialogs.proxy.types.environment')}</span>
      </div>
    );
  }

  const proxyURL = proxy.url?.trim();
  if (!proxyURL) {
    return (
      <div className='flex justify-center'>
        <span className='text-muted-foreground text-xs'>-</span>
      </div>
    );
  }

  const { label, detail } = getProxyURLSummary(proxyURL);
  const content = (
    <div className='flex justify-center'>
      <span className='max-w-40 truncate font-mono text-xs'>{label}</span>
    </div>
  );

  if (detail && detail !== label) {
    return (
      <Tooltip>
        <TooltipTrigger asChild>{content}</TooltipTrigger>
        <TooltipContent>{detail}</TooltipContent>
      </Tooltip>
    );
  }

  return content;
});

ProxyCell.displayName = 'ProxyCell';

const SupportedModelsCell = memo(({ row }: { row: Row<Channel> }) => {
  const { t } = useTranslation();
  const channel = row.original;
  const models = row.getValue('supportedModels') as string[];
  const { setOpen, setCurrentRow } = useChannels();

  const handleOpenModelsDialog = useCallback(() => {
    setCurrentRow(channel);
    setOpen('viewModels');
  }, [channel, setCurrentRow, setOpen]);

  return (
    <div className='flex max-w-full min-w-0 items-center justify-center gap-2 overflow-hidden'>
      <div className='flex max-w-full min-w-0 flex-wrap justify-center gap-1 overflow-hidden'>
        {models.slice(0, 5).map((model) => (
          <Badge key={model} variant='secondary' className='block max-w-48 truncate text-left text-xs'>
            {model}
          </Badge>
        ))}
        {models.length > 5 && (
          <Badge
            variant='secondary'
            className='hover:bg-primary hover:text-primary-foreground cursor-pointer text-xs transition-colors'
            onClick={handleOpenModelsDialog}
            title={t('channels.actions.viewModels')}
          >
            +{models.length - 5}
          </Badge>
        )}
      </div>
    </div>
  );
});

SupportedModelsCell.displayName = 'SupportedModelsCell';

const OrderingWeightCell = memo(({ row }: { row: Row<Channel> }) => {
  const channel = row.original;
  const initialWeight = row.getValue('orderingWeight') as number | null;
  const [isEditing, setIsEditing] = useState(false);
  const [weight, setWeight] = useState<string>(initialWeight?.toString() || '1');
  const updateChannel = useUpdateChannel();
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (isEditing && inputRef.current) {
      inputRef.current.focus();
      inputRef.current.select();
    }
  }, [isEditing]);

  const handleDoubleClick = useCallback(() => {
    setIsEditing(true);
    setWeight(initialWeight?.toString() || '1');
  }, [initialWeight]);

  const handleSave = useCallback(async () => {
    const weightValue = clampWeight(Number(weight));
    if (weightValue === initialWeight) {
      setIsEditing(false);
      return;
    }

    try {
      await updateChannel.mutateAsync({
        id: channel.id,
        input: { orderingWeight: weightValue },
      });
      setIsEditing(false);
    } catch (_error) {
      // Error handled by mutation hook
    }
  }, [channel.id, weight, initialWeight, updateChannel]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === 'Enter') {
        handleSave();
      } else if (e.key === 'Escape') {
        setIsEditing(false);
        setWeight(initialWeight?.toString() || '1');
      }
    },
    [handleSave, initialWeight]
  );

  if (isEditing) {
    return (
      <div className='flex justify-center px-2'>
        <Input
          ref={inputRef}
          type='number'
          inputMode='decimal'
          step='any'
          min={MIN_WEIGHT}
          max={MAX_WEIGHT}
          value={weight}
          onChange={(e) => setWeight(e.target.value)}
          onBlur={handleSave}
          onKeyDown={handleKeyDown}
          className='h-7 w-20 text-center font-mono text-sm'
          disabled={updateChannel.isPending}
        />
      </div>
    );
  }

  return (
    <div className='group flex cursor-pointer items-center justify-center gap-2' onDoubleClick={handleDoubleClick}>
      <span className={cn('font-mono text-sm', initialWeight == null && 'text-muted-foreground')}>{initialWeight ?? '-'}</span>
      {updateChannel.isPending && <IconLoader2 className='text-muted-foreground h-3 w-3 animate-spin' />}
    </div>
  );
});

OrderingWeightCell.displayName = 'OrderingWeightCell';

const CreatedAtCell = memo(({ row }: { row: Row<Channel> }) => {
  const raw = row.getValue('createdAt') as unknown;
  const date = raw instanceof Date ? raw : new Date(raw as string);

  if (Number.isNaN(date.getTime())) {
    return (
      <div className='flex justify-center'>
        <span className='text-muted-foreground text-xs'>-</span>
      </div>
    );
  }

  return (
    <div className='flex justify-center'>
      <Tooltip>
        <TooltipTrigger asChild>
          <div className='text-muted-foreground cursor-help text-sm'>{format(date, 'yyyy-MM-dd')}</div>
        </TooltipTrigger>
        <TooltipContent>{format(date, 'yyyy-MM-dd HH:mm:ss')}</TooltipContent>
      </Tooltip>
    </div>
  );
});

CreatedAtCell.displayName = 'CreatedAtCell';

export const createColumns = (t: ReturnType<typeof useTranslation>['t'], canWrite: boolean = true): ColumnDef<Channel>[] => {
  return [
    {
      id: 'expand',
      header: () => null,
      meta: {
        className: 'w-8 min-w-8 text-center',
      },
      cell: ExpandCell,
      enableSorting: false,
      enableHiding: false,
    },
    ...(canWrite
      ? [
          {
            id: 'select',
            header: ({ table }: { table: Table<Channel> }) => (
              <div className='flex justify-center'>
                <Checkbox
                  checked={table.getIsAllPageRowsSelected() || (table.getIsSomePageRowsSelected() && 'indeterminate')}
                  onCheckedChange={(value) => table.toggleAllPageRowsSelected(!!value)}
                  aria-label={t('common.columns.selectAll')}
                  className='translate-y-[2px]'
                />
              </div>
            ),
            cell: ({ row }: { row: Row<Channel> }) => (
              <div className='flex justify-center'>
                <Checkbox
                  checked={row.getIsSelected()}
                  onCheckedChange={(value) => row.toggleSelected(!!value)}
                  aria-label={t('common.columns.selectRow')}
                  className='translate-y-[2px]'
                />
              </div>
            ),
            meta: {
              className: 'w-10 min-w-10 text-center',
            },
            enableSorting: false,
            enableHiding: false,
          },
        ]
      : []),
    {
      accessorKey: 'name',
      header: ({ column }) => <DataTableColumnHeader column={column} title={t('common.columns.name')} className='justify-center' />,
      cell: NameCell,
      meta: {
        className: 'w-[24%] min-w-48 text-center',
      },
      enableHiding: false,
      enableSorting: true,
    },
    {
      id: 'provider',
      accessorKey: 'type',
      header: ({ column }) => <DataTableColumnHeader column={column} title={t('channels.columns.provider')} className='justify-center' />,
      cell: ProviderCell,
      meta: {
        className: 'w-32 min-w-32 text-center',
      },
      filterFn: (row, _id, value) => {
        return value.includes(row.original.type);
      },
      enableSorting: true,
      enableHiding: false,
    },
    {
      accessorKey: 'status',
      header: ({ column }) => <DataTableColumnHeader column={column} title={t('common.columns.status')} className='justify-center' />,
      cell: StatusSwitchCell,
      meta: {
        className: 'w-20 min-w-20 text-center',
      },
      enableSorting: true,
      enableHiding: false,
    },

    {
      accessorKey: 'tags',
      header: ({ column }) => <DataTableColumnHeader column={column} title={t('channels.columns.tags')} className='justify-center' />,
      cell: TagsCell,
      meta: {
        className: 'w-36 min-w-36 text-center',
      },
      filterFn: (row, id, value) => {
        const tags = (row.getValue(id) as string[]) || [];
        // Single select: value is a string, not an array
        return tags.includes(value as string);
      },
      enableSorting: false,
      enableHiding: true,
    },
    {
      id: 'model',
      accessorFn: () => '', // Virtual column for filtering only
      header: () => null,
      cell: () => null,
      filterFn: () => true, // Server-side filtering, always return true
      enableSorting: false,
      enableHiding: true,
      enableColumnFilter: false,
      enableGlobalFilter: false,
    },
    {
      accessorKey: 'supportedModels',
      header: ({ column }) => (
        <DataTableColumnHeader column={column} title={t('channels.columns.supportedModels')} className='justify-center' />
      ),
      cell: SupportedModelsCell,
      meta: {
        className: 'w-auto min-w-64 text-center',
      },
      enableSorting: false,
    },
    {
      id: 'proxy',
      accessorFn: (row) => row.settings?.proxy?.url ?? row.settings?.proxy?.type ?? '',
      header: ({ column }) => <DataTableColumnHeader column={column} title={t('channels.columns.proxy')} className='justify-center' />,
      cell: ProxyCell,
      meta: {
        className: 'w-32 min-w-32 text-center',
      },
      enableSorting: false,
      enableHiding: true,
    },
    {
      id: 'health',
      accessorKey: 'health',
      header: ({ column }) => <DataTableColumnHeader column={column} title={t('channels.columns.health')} className='justify-center' />,
      cell: ({ row }: { row: Row<Channel> }) => {
        const probePoints = (row.original as any).probePoints || [];
        const limiterStats = row.original.liveLimiterStats;
        return (
          <div className='flex flex-col items-center gap-1'>
            <ChannelHealthCell points={probePoints} />
            {limiterStats ? <ChannelLimiterCell stats={limiterStats} /> : null}
          </div>
        );
      },
      meta: {
        className: 'hidden w-36 min-w-36 text-center 2xl:table-cell',
      },
      enableSorting: false,
      enableHiding: true,
    },
    {
      accessorKey: 'orderingWeight',
      header: ({ column }) => (
        <DataTableColumnHeader column={column} title={t('channels.columns.orderingWeight')} className='justify-center' />
      ),
      cell: OrderingWeightCell,
      meta: {
        className: 'w-16 min-w-16 text-center',
      },
      sortingFn: 'alphanumeric',
      enableSorting: true,
      enableHiding: true,
    },
    {
      accessorKey: 'createdAt',
      header: ({ column }) => <DataTableColumnHeader column={column} title={t('common.columns.createdAt')} className='justify-center' />,
      cell: CreatedAtCell,
      meta: {
        className: 'w-28 min-w-28 text-center',
      },
      enableSorting: true,
      enableHiding: false,
    },
    ...(canWrite
      ? [
          {
            id: 'action',
            header: ({ column }: { column: any }) => (
              <DataTableColumnHeader column={column} title={t('common.columns.actions')} className='justify-center' />
            ),
            cell: ActionCell,
            meta: {
              className: 'w-36 min-w-36 text-center',
            },
            enableSorting: false,
            enableHiding: false,
          },
        ]
      : []),
  ];
};
