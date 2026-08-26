import { Search, Tag, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import type { ChangeSetKind, ChangeSetStatus } from '../data/change-sets';

export type ChangeSetStatusFilter = 'ACTIONABLE' | 'ALL' | ChangeSetStatus;

type Props = {
  query: string;
  kind: 'ALL' | ChangeSetKind;
  status: ChangeSetStatusFilter;
  defaultStatus: ChangeSetStatusFilter;
  scopeType?: string;
  scopeID?: string;
  resultCount: number;
  onQueryChange: (value: string) => void;
  onKindChange: (value: 'ALL' | ChangeSetKind) => void;
  onStatusChange: (value: ChangeSetStatusFilter) => void;
  onClearScope: () => void;
  onReset: () => void;
};

const kinds: ChangeSetKind[] = ['PROVIDER_PRICE', 'MODEL_MAPPING', 'RETAIL_PRICE'];
const statuses: ChangeSetStatus[] = ['DRAFT', 'PENDING_REVIEW', 'APPLIED', 'REJECTED', 'SUPERSEDED', 'INVALID'];

export function ChangeSetToolbar({
  query,
  kind,
  status,
  defaultStatus,
  scopeType,
  scopeID,
  resultCount,
  onQueryChange,
  onKindChange,
  onStatusChange,
  onClearScope,
  onReset,
}: Props) {
  const { t } = useTranslation();
  const hasScope = Boolean(scopeType || scopeID);
  const hasFilters = Boolean(query || kind !== 'ALL' || status !== defaultStatus || hasScope);

  return (
    <div className='flex flex-col gap-3'>
      <div className='flex flex-wrap items-center gap-2'>
        <div className='relative min-w-0 flex-1 basis-64 sm:max-w-md'>
          <Search className='text-muted-foreground pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2' />
          <Input
            value={query}
            onChange={(event) => onQueryChange(event.target.value)}
            placeholder={t('changeSets.filters.searchPlaceholder')}
            aria-label={t('changeSets.filters.searchLabel')}
            className='h-9 pl-9'
          />
        </div>

        <Select value={kind} onValueChange={(value) => onKindChange(value as 'ALL' | ChangeSetKind)}>
          <SelectTrigger className='h-9 min-w-0 flex-1 basis-36 sm:w-44 sm:flex-none' aria-label={t('changeSets.filters.kind')}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value='ALL'>{t('changeSets.filters.allKinds')}</SelectItem>
            {kinds.map((value) => (
              <SelectItem key={value} value={value}>
                {t(`changeSets.kind.${value}`)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        <Select value={status} onValueChange={(value) => onStatusChange(value as ChangeSetStatusFilter)}>
          <SelectTrigger className='h-9 min-w-0 flex-1 basis-36 sm:w-44 sm:flex-none' aria-label={t('changeSets.filters.status')}>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value='ALL'>{t('changeSets.filters.allStatuses')}</SelectItem>
            <SelectItem value='ACTIONABLE'>{t('changeSets.filters.actionable')}</SelectItem>
            {statuses.map((value) => (
              <SelectItem key={value} value={value}>
                {t(`changeSets.status.${value}`)}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>

        {hasFilters && (
          <Button variant='ghost' size='sm' className='h-9 px-2' onClick={onReset}>
            <X className='size-4' />
            {t('changeSets.filters.reset')}
          </Button>
        )}
      </div>

      <div className='flex min-h-6 flex-wrap items-center justify-between gap-2'>
        <div className='min-w-0'>
          {hasScope && (
            <Button variant='outline' size='sm' className='h-7 max-w-full gap-1.5 rounded-md px-2' onClick={onClearScope}>
              <Tag className='size-3.5 shrink-0' />
              <span className='truncate font-mono text-xs'>
                {scopeType || '*'} / {scopeID || '*'}
              </span>
              <X className='size-3.5 shrink-0' />
            </Button>
          )}
        </div>
        <span className='text-muted-foreground text-xs tabular-nums'>{t('changeSets.resultCount', { count: resultCount })}</span>
      </div>
    </div>
  );
}
