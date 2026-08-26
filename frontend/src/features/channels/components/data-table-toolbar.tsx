import { useMemo, useEffect } from 'react';
import { Cross2Icon } from '@radix-ui/react-icons';
import { Table } from '@tanstack/react-table';
import { useQueryModels } from '@/gql/models';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { DataTableFacetedFilter } from '@/components/data-table-faceted-filter';
import { useAllChannelTags } from '../data/channels';
import { DataTableViewOptions } from './data-table-view-options';

interface DataTableToolbarProps<TData> {
  table: Table<TData>;
  isFiltered?: boolean;
  selectedCount?: number;
  showErrorOnly?: boolean;
  onExitErrorOnlyMode?: () => void;
}

export function DataTableToolbar<TData>({
  table,
  isFiltered: externalIsFiltered,
  showErrorOnly,
  onExitErrorOnlyMode,
}: DataTableToolbarProps<TData>) {
  const { t } = useTranslation();
  const tableState = table.getState();
  const isFiltered = externalIsFiltered ?? tableState.columnFilters.length > 0;

  // Get all channel tags from GraphQL
  const { data: allTags = [] } = useAllChannelTags();

  // Fetch models using the models query
  const { mutate: fetchModels, data: modelsData } = useQueryModels();

  // Fetch models on component mount
  useEffect(() => {
    fetchModels({
      statusIn: ['enabled', 'disabled'],
      includeMapping: true,
      includePrefix: true,
    });
  }, [fetchModels]);

  const tagOptions = useMemo(
    () =>
      allTags.map((tag) => ({
        value: tag,
        label: tag,
      })),
    [allTags]
  );

  const modelOptions = useMemo(() => {
    if (!modelsData) return [];
    return modelsData.map((model) => ({
      value: model.id,
      label: model.id,
    }));
  }, [modelsData]);

  const channelStatuses = useMemo(
    () => [
      {
        value: 'enabled',
        label: t('channels.status.enabled'),
      },
      {
        value: 'disabled',
        label: t('channels.status.disabled'),
      },
      {
        value: 'archived',
        label: t('channels.status.archived'),
      },
    ],
    [t]
  );

  return (
    <div className='flex w-full flex-col gap-2 lg:flex-row lg:items-center'>
      <div className='relative w-full min-w-0 flex-1 lg:min-w-48'>
        <i className='ph ph-magnifying-glass text-muted-foreground absolute top-2.5 left-3'></i>
        <Input
          placeholder={t('channels.filters.filterByName')}
          value={(table.getColumn('name')?.getFilterValue() as string) ?? ''}
          onChange={(event) => table.getColumn('name')?.setFilterValue(event.target.value)}
          className='bg-card border-border focus:ring-primary/20 placeholder-muted-foreground text-foreground w-full rounded-xl border py-2 pr-4 pl-10 text-sm shadow-sm transition-[border-color,box-shadow] focus:ring-2 focus:outline-none'
        />
      </div>
      <div className='hide-scroll bg-muted/20 flex w-full items-center gap-2 overflow-x-auto rounded-lg border px-1 py-1 shadow-[inset_-12px_0_12px_-14px_currentColor] lg:w-auto lg:overflow-visible lg:border-0 lg:bg-transparent lg:p-0 lg:shadow-none'>
        {table.getColumn('status') && (
          <DataTableFacetedFilter column={table.getColumn('status')} title={t('channels.filters.status')} options={channelStatuses} />
        )}
        {table.getColumn('tags') && tagOptions?.length > 0 && (
          <DataTableFacetedFilter column={table.getColumn('tags')} title={t('channels.filters.tags')} options={tagOptions} singleSelect />
        )}
        {table.getColumn('model') && modelOptions?.length > 0 && (
          <DataTableFacetedFilter
            column={table.getColumn('model')}
            title={t('channels.filters.model')}
            options={modelOptions}
            singleSelect
          />
        )}
        {isFiltered && (
          <Button variant='ghost' onClick={() => table.resetColumnFilters()} className='h-8 shrink-0 px-2 lg:px-3'>
            {t('common.filters.reset')}
            <Cross2Icon className='ml-2 h-4 w-4' />
          </Button>
        )}
        {showErrorOnly && onExitErrorOnlyMode && (
          <Button
            variant='outline'
            onClick={onExitErrorOnlyMode}
            className='h-8 shrink-0 border-orange-600 text-orange-600 hover:bg-orange-600 hover:text-white'
          >
            {t('channels.errorBanner.exitErrorOnlyButton')}
          </Button>
        )}
        <div className='shrink-0'>
          <DataTableViewOptions table={table} />
        </div>
      </div>
    </div>
  );
}
