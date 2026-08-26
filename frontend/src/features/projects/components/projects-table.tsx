import React, { useMemo, useState } from 'react';
import {
  ColumnDef,
  ColumnFiltersState,
  RowData,
  SortingState,
  VisibilityState,
  flexRender,
  getCoreRowModel,
  getFacetedRowModel,
  getFacetedUniqueValues,
  useReactTable,
} from '@tanstack/react-table';
import { IconArchive, IconCheck, IconX } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { TableSkeleton } from '@/components/ui/table-skeleton';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { ServerSidePagination } from '@/components/server-side-pagination';
import { useBulkUpdateProjectStatus } from '../data/projects';
import { Project, ProjectConnection } from '../data/schema';
import { DataTableToolbar } from './data-table-toolbar';

declare module '@tanstack/react-table' {
  // eslint-disable-next-line @typescript-eslint/no-unused-vars
  interface ColumnMeta<TData extends RowData, TValue> {
    className: string;
  }
}

interface DataTableProps {
  columns: ColumnDef<Project>[];
  loading?: boolean;
  data: Project[];
  pageInfo?: ProjectConnection['pageInfo'];
  pageSize: number;
  totalCount?: number;
  onNextPage: () => void;
  onPreviousPage: () => void;
  onPageSizeChange: (pageSize: number) => void;
  searchFilter: string;
  onSearchFilterChange: (value: string) => void;
}

export function ProjectsTable({
  columns,
  data,
  loading,
  pageInfo,
  pageSize,
  totalCount,
  onNextPage,
  onPreviousPage,
  onPageSizeChange,
  searchFilter,
  onSearchFilterChange,
}: DataTableProps) {
  const { t } = useTranslation();
  const [rowSelection, setRowSelection] = useState({});
  const [columnVisibility, setColumnVisibility] = useState<VisibilityState>({});
  const [columnFilters, setColumnFilters] = useState<ColumnFiltersState>([]);
  const [sorting, setSorting] = useState<SortingState>([]);
  const [bulkAction, setBulkAction] = useState<'active' | 'archived' | null>(null);
  const bulkUpdate = useBulkUpdateProjectStatus();

  // Sync server state to local column filters (for UI display)
  React.useEffect(() => {
    const newFilters: ColumnFiltersState = [];
    if (searchFilter) {
      // Use 'search' as a virtual column ID for the combined search
      newFilters.push({ id: 'search', value: searchFilter });
    }
    setColumnFilters(newFilters);
  }, [searchFilter]);

  const handleColumnFiltersChange = (updater: ColumnFiltersState | ((prev: ColumnFiltersState) => ColumnFiltersState)) => {
    const newFilters = typeof updater === 'function' ? updater(columnFilters) : updater;
    setColumnFilters(newFilters);

    // Extract search filter value
    const searchFilterValue = newFilters.find((f) => f.id === 'search')?.value;

    // Only update if values actually change to prevent reset issues
    const newSearchFilter = typeof searchFilterValue === 'string' ? searchFilterValue : '';
    if (newSearchFilter !== searchFilter) {
      onSearchFilterChange(newSearchFilter);
    }
  };

  const table = useReactTable({
    data,
    columns,
    state: {
      sorting,
      columnVisibility,
      rowSelection,
      columnFilters,
    },
    enableRowSelection: true,
    onRowSelectionChange: setRowSelection,
    onSortingChange: setSorting,
    onColumnFiltersChange: handleColumnFiltersChange,
    onColumnVisibilityChange: setColumnVisibility,
    getCoreRowModel: getCoreRowModel(),
    manualFiltering: true,
    manualPagination: true,
    getFacetedRowModel: getFacetedRowModel(),
    getFacetedUniqueValues: getFacetedUniqueValues(),
    getRowId: (row) => row.id,
  });

  const selectedRows = useMemo(() => table.getFilteredSelectedRowModel().rows, [table, rowSelection, data]);
  const selectedCount = selectedRows.length;

  React.useEffect(() => {
    const dataIds = new Set(data.map((project) => project.id));
    if (Object.keys(rowSelection).some((id) => !dataIds.has(id))) {
      setRowSelection({});
    }
  }, [data, rowSelection]);

  const confirmBulkAction = () => {
    if (!bulkAction || selectedCount === 0) return;
    bulkUpdate.mutate(
      { ids: selectedRows.map((row) => row.original.id), status: bulkAction },
      {
        onSuccess: () => {
          setRowSelection({});
          setBulkAction(null);
        },
      }
    );
  };

  return (
    <div className='flex flex-1 flex-col overflow-hidden' data-testid='projects-table'>
      <DataTableToolbar table={table} />
      <div className='shadow-soft relative mt-4 flex-1 overflow-auto rounded-2xl border border-[var(--table-border)] md:overflow-x-hidden'>
        <Table data-testid='projects-table' className='border-separate border-spacing-0 rounded-2xl bg-[var(--table-background)]'>
          <TableHeader className='sticky top-0 z-20 bg-[var(--table-header)] shadow-sm'>
            {table.getHeaderGroups().map((headerGroup) => (
              <TableRow key={headerGroup.id} className='group/row border-0'>
                {headerGroup.headers.map((header) => {
                  return (
                    <TableHead
                      key={header.id}
                      colSpan={header.colSpan}
                      className={`${header.column.columnDef.meta?.className ?? ''} text-muted-foreground border-0 text-xs font-semibold tracking-wider uppercase`}
                    >
                      {header.isPlaceholder ? null : flexRender(header.column.columnDef.header, header.getContext())}
                    </TableHead>
                  );
                })}
              </TableRow>
            ))}
          </TableHeader>
          <TableBody className='space-y-1 !bg-[var(--table-background)] p-2'>
            {loading ? (
              <TableSkeleton rows={pageSize} columns={columns.length} />
            ) : table.getRowModel().rows?.length ? (
              table.getRowModel().rows.map((row) => (
                <TableRow
                  key={row.id}
                  data-state={row.getIsSelected() && 'selected'}
                  className='group/row table-row-hover rounded-xl border-0 !bg-[var(--table-background)] transition-all duration-200 ease-in-out'
                >
                  {row.getVisibleCells().map((cell) => (
                    <TableCell key={cell.id} className={`${cell.column.columnDef.meta?.className ?? ''} border-0 bg-inherit px-4 py-3`}>
                      {flexRender(cell.column.columnDef.cell, cell.getContext())}
                    </TableCell>
                  ))}
                </TableRow>
              ))
            ) : (
              <TableRow className='!bg-[var(--table-background)]'>
                <TableCell colSpan={columns.length} className='h-24 !bg-[var(--table-background)] text-center'>
                  {t('common.noData')}
                </TableCell>
              </TableRow>
            )}
          </TableBody>
        </Table>
      </div>
      <div className='mt-4 flex-shrink-0'>
        <ServerSidePagination
          pageInfo={pageInfo}
          pageSize={pageSize}
          dataLength={data.length}
          totalCount={totalCount}
          selectedRows={Object.keys(rowSelection).length}
          onNextPage={onNextPage}
          onPreviousPage={onPreviousPage}
          onPageSizeChange={onPageSizeChange}
          data-testid='pagination'
        />
      </div>
      {selectedCount > 0 && (
        <div className='fixed bottom-6 left-1/2 z-50 -translate-x-1/2' data-testid='projects-bulk-actions'>
          <div className='bg-background flex items-center gap-2 rounded-lg border px-4 py-2 shadow-lg'>
            <Button
              variant='ghost'
              size='icon'
              className='h-8 w-8'
              onClick={() => setRowSelection({})}
              aria-label={t('projects.bulk.clearSelection')}
            >
              <IconX className='h-4 w-4' />
            </Button>
            <div className='flex items-center gap-1.5 px-2'>
              <span className='bg-primary text-primary-foreground flex h-6 min-w-6 items-center justify-center rounded px-1.5 text-xs font-medium'>
                {selectedCount}
              </span>
              <span className='text-muted-foreground text-sm'>{t('common.selected')}</span>
            </div>
            <div className='bg-border mx-2 h-6 w-px' />
            <Button
              variant='ghost'
              size='icon'
              className='h-8 w-8 text-green-600 hover:bg-green-100 hover:text-green-700'
              onClick={() => setBulkAction('active')}
              title={t('common.buttons.activate')}
            >
              <IconCheck className='h-4 w-4' />
            </Button>
            <Button
              variant='ghost'
              size='icon'
              className='h-8 w-8 text-orange-600 hover:bg-orange-100 hover:text-orange-700'
              onClick={() => setBulkAction('archived')}
              title={t('common.buttons.archive')}
            >
              <IconArchive className='h-4 w-4' />
            </Button>
          </div>
        </div>
      )}
      <ConfirmDialog
        open={bulkAction !== null}
        onOpenChange={(open) => !open && setBulkAction(null)}
        title={t(bulkAction === 'archived' ? 'projects.dialogs.bulkArchive.title' : 'projects.dialogs.bulkActivate.title')}
        desc={t(bulkAction === 'archived' ? 'projects.dialogs.bulkArchive.description' : 'projects.dialogs.bulkActivate.description', {
          count: selectedCount,
        })}
        cancelBtnText={t('common.buttons.cancel')}
        confirmText={t(
          bulkUpdate.isPending
            ? 'projects.bulk.processing'
            : bulkAction === 'archived'
              ? 'common.buttons.archive'
              : 'common.buttons.activate'
        )}
        destructive={bulkAction === 'archived'}
        isLoading={bulkUpdate.isPending}
        handleConfirm={confirmBulkAction}
      />
    </div>
  );
}
