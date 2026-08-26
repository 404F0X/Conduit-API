import { useEffect, useMemo, useState } from 'react';
import { AlertCircle, ChevronLeft, ChevronRight, Inbox } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { formatChangeSetTime, getLastRelevantTime } from '../change-set-display';
import type { ChangeSet } from '../data/change-sets';
import { ChangeSetActions, type ChangeSetActionType } from './change-set-actions';
import { ChangeSetKindBadge, ChangeSetStatusBadge } from './change-set-badges';

type Props = {
  rows: ChangeSet[];
  loading: boolean;
  error: Error | null;
  canManage: boolean;
  emptyTitle: string;
  emptyDescription: string;
  onRetry: () => void;
  onOpen: (changeSet: ChangeSet) => void;
  onAction: (action: ChangeSetActionType, changeSet: ChangeSet) => void;
};

const pageSizes = [20, 50, 100];

function stopActionPropagation(event: React.MouseEvent<HTMLDivElement>) {
  event.stopPropagation();
}

export function ChangeSetTable({ rows, loading, error, canManage, emptyTitle, emptyDescription, onRetry, onOpen, onAction }: Props) {
  const { t, i18n } = useTranslation();
  const [pageIndex, setPageIndex] = useState(0);
  const [pageSize, setPageSize] = useState(20);

  useEffect(() => {
    setPageIndex(0);
  }, [rows]);

  const pageCount = Math.max(1, Math.ceil(rows.length / pageSize));
  const safePageIndex = Math.min(pageIndex, pageCount - 1);
  const pageRows = useMemo(
    () => rows.slice(safePageIndex * pageSize, safePageIndex * pageSize + pageSize),
    [pageSize, rows, safePageIndex]
  );

  if (error) {
    return (
      <div className='border-border flex min-h-72 flex-col items-center justify-center gap-3 rounded-md border px-6 text-center'>
        <AlertCircle className='text-destructive size-6' />
        <div>
          <p className='font-medium'>{t('changeSets.error.title')}</p>
          <p className='text-muted-foreground mt-1 max-w-xl text-sm break-words'>{error.message}</p>
        </div>
        <Button variant='outline' size='sm' onClick={onRetry}>
          {t('common.buttons.retry')}
        </Button>
      </div>
    );
  }

  if (!loading && rows.length === 0) {
    return (
      <div className='border-border flex min-h-72 flex-col items-center justify-center rounded-md border px-6 text-center'>
        <Inbox className='text-muted-foreground size-6' />
        <p className='mt-3 font-medium'>{emptyTitle}</p>
        <p className='text-muted-foreground mt-1 max-w-md text-sm'>{emptyDescription}</p>
      </div>
    );
  }

  return (
    <div className='flex min-h-0 flex-1 flex-col gap-3'>
      <div className='border-border hidden min-h-0 flex-1 overflow-auto rounded-md border md:block'>
        <Table>
          <TableHeader className='bg-muted/45 sticky top-0 z-10'>
            <TableRow className='hover:bg-transparent'>
              <TableHead className='min-w-64'>{t('changeSets.columns.change')}</TableHead>
              <TableHead className='w-40'>{t('changeSets.columns.kind')}</TableHead>
              <TableHead className='w-40'>{t('changeSets.columns.status')}</TableHead>
              <TableHead className='min-w-48'>{t('changeSets.columns.scope')}</TableHead>
              <TableHead className='w-24 text-right'>{t('changeSets.columns.items')}</TableHead>
              <TableHead className='w-44'>{t('changeSets.columns.updated')}</TableHead>
              {canManage && <TableHead className='w-56 text-right'>{t('changeSets.columns.actions')}</TableHead>}
            </TableRow>
          </TableHeader>
          <TableBody>
            {loading
              ? Array.from({ length: 8 }, (_, index) => (
                  <TableRow key={index}>
                    {Array.from({ length: canManage ? 7 : 6 }, (_cell, cellIndex) => (
                      <TableCell key={cellIndex}>
                        <Skeleton className='h-5 w-full max-w-36 rounded-sm' />
                      </TableCell>
                    ))}
                  </TableRow>
                ))
              : pageRows.map((changeSet) => (
                  <TableRow
                    key={changeSet.id}
                    tabIndex={0}
                    className='focus-visible:bg-muted/50 cursor-pointer focus-visible:outline-none'
                    aria-label={t('changeSets.openDetails', { title: changeSet.title })}
                    onClick={() => onOpen(changeSet)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter' || event.key === ' ') {
                        event.preventDefault();
                        onOpen(changeSet);
                      }
                    }}
                  >
                    <TableCell>
                      <p className='max-w-md truncate font-medium'>{changeSet.title}</p>
                      <p className='text-muted-foreground mt-1 max-w-md truncate font-mono text-xs'>{changeSet.id}</p>
                    </TableCell>
                    <TableCell>
                      <ChangeSetKindBadge kind={changeSet.kind} />
                    </TableCell>
                    <TableCell>
                      <ChangeSetStatusBadge status={changeSet.status} />
                    </TableCell>
                    <TableCell>
                      <p className='text-xs font-medium'>{changeSet.scopeType}</p>
                      <p className='text-muted-foreground mt-1 max-w-48 truncate font-mono text-xs'>{changeSet.scopeID}</p>
                    </TableCell>
                    <TableCell className='text-right font-mono text-sm tabular-nums'>{changeSet.items.length}</TableCell>
                    <TableCell className='text-muted-foreground text-xs'>
                      {formatChangeSetTime(getLastRelevantTime(changeSet), i18n.language)}
                    </TableCell>
                    {canManage && (
                      <TableCell>
                        <div onClick={stopActionPropagation} onKeyDown={(event) => event.stopPropagation()}>
                          <ChangeSetActions changeSet={changeSet} onAction={onAction} className='flex justify-end gap-2' />
                        </div>
                      </TableCell>
                    )}
                  </TableRow>
                ))}
          </TableBody>
        </Table>
      </div>

      <div className='border-border min-h-0 flex-1 overflow-auto border-y md:hidden'>
        {loading
          ? Array.from({ length: 6 }, (_, index) => (
              <div key={index} className='border-border space-y-3 border-b px-1 py-4 last:border-b-0'>
                <Skeleton className='h-5 w-2/3 rounded-sm' />
                <div className='flex gap-2'>
                  <Skeleton className='h-5 w-24 rounded-sm' />
                  <Skeleton className='h-5 w-20 rounded-sm' />
                </div>
                <Skeleton className='h-4 w-full rounded-sm' />
              </div>
            ))
          : pageRows.map((changeSet) => (
              <article key={changeSet.id} className='border-border border-b py-4 last:border-b-0'>
                <button type='button' className='w-full text-left focus-visible:outline-none' onClick={() => onOpen(changeSet)}>
                  <div className='flex min-w-0 items-start justify-between gap-3'>
                    <div className='min-w-0 flex-1'>
                      <p className='font-medium break-words'>{changeSet.title}</p>
                      <p className='text-muted-foreground mt-1 truncate font-mono text-xs'>{changeSet.id}</p>
                    </div>
                    <ChangeSetStatusBadge status={changeSet.status} className='shrink-0' />
                  </div>
                  <div className='mt-3 flex flex-wrap items-center gap-2'>
                    <ChangeSetKindBadge kind={changeSet.kind} />
                    <span className='text-muted-foreground max-w-full truncate font-mono text-xs'>
                      {changeSet.scopeType} / {changeSet.scopeID}
                    </span>
                  </div>
                  <div className='text-muted-foreground mt-3 flex flex-wrap justify-between gap-x-4 gap-y-1 text-xs'>
                    <span>{t('changeSets.itemCount', { count: changeSet.items.length })}</span>
                    <span>{formatChangeSetTime(getLastRelevantTime(changeSet), i18n.language)}</span>
                  </div>
                </button>
                {canManage && <ChangeSetActions changeSet={changeSet} onAction={onAction} className='mt-3 flex flex-wrap gap-2' />}
              </article>
            ))}
      </div>

      {!loading && rows.length > 0 && (
        <div className='flex flex-wrap items-center justify-between gap-3 px-1'>
          <div className='text-muted-foreground text-xs tabular-nums'>
            {t('changeSets.pagination.range', {
              start: safePageIndex * pageSize + 1,
              end: Math.min((safePageIndex + 1) * pageSize, rows.length),
              total: rows.length,
            })}
          </div>
          <div className='flex items-center gap-2'>
            <Select
              value={String(pageSize)}
              onValueChange={(value) => {
                setPageSize(Number(value));
                setPageIndex(0);
              }}
            >
              <SelectTrigger className='h-8 w-20' aria-label={t('pagination.rowsPerPage')}>
                <SelectValue />
              </SelectTrigger>
              <SelectContent side='top'>
                {pageSizes.map((value) => (
                  <SelectItem key={value} value={String(value)}>
                    {value}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
            <span className='min-w-20 text-center text-xs font-medium tabular-nums'>
              {t('pagination.currentPage', { current: safePageIndex + 1, total: pageCount })}
            </span>
            <Button
              variant='outline'
              size='icon'
              className='size-8'
              disabled={safePageIndex === 0}
              onClick={() => setPageIndex((value) => Math.max(0, value - 1))}
            >
              <ChevronLeft className='size-4' />
              <span className='sr-only'>{t('pagination.previousPage')}</span>
            </Button>
            <Button
              variant='outline'
              size='icon'
              className='size-8'
              disabled={safePageIndex >= pageCount - 1}
              onClick={() => setPageIndex((value) => Math.min(pageCount - 1, value + 1))}
            >
              <ChevronRight className='size-4' />
              <span className='sr-only'>{t('pagination.nextPage')}</span>
            </Button>
          </div>
        </div>
      )}
    </div>
  );
}
