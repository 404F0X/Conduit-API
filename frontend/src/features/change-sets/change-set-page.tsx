import { useMemo, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { usePermissions } from '@/hooks/usePermissions';
import { Header } from '@/components/layout/header';
import { Main } from '@/components/layout/main';
import { ACTIONABLE_CHANGE_SET_STATUSES, getLastRelevantTime, matchesChangeSetSearch } from './change-set-display';
import type { ChangeSetRouteSearch } from './change-set-search';
import type { ChangeSetActionType } from './components/change-set-actions';
import { ChangeSetDetailDialog } from './components/change-set-detail-dialog';
import { ChangeSetReviewDialog, type ChangeSetActionSelection } from './components/change-set-review-dialog';
import { ChangeSetTable } from './components/change-set-table';
import { ChangeSetToolbar, type ChangeSetStatusFilter } from './components/change-set-toolbar';
import { type ChangeSet, type ChangeSetKind, useChangeSets } from './data/change-sets';

type Props = {
  mode: 'workbench' | 'changelog';
  initialFilters?: ChangeSetRouteSearch;
};

export function ChangeSetPage({ mode, initialFilters = {} }: Props) {
  const { t } = useTranslation();
  const { hasSystemScope } = usePermissions();
  const defaultStatus: ChangeSetStatusFilter = mode === 'workbench' ? 'ACTIONABLE' : 'ALL';
  const [query, setQuery] = useState(initialFilters.q ?? '');
  const [kind, setKind] = useState<'ALL' | ChangeSetKind>(initialFilters.kind ?? 'ALL');
  const [status, setStatus] = useState<ChangeSetStatusFilter>(initialFilters.status ?? defaultStatus);
  const [scopeType, setScopeType] = useState(initialFilters.scopeType);
  const [scopeID, setScopeID] = useState(initialFilters.scopeID);
  const [selectedChangeSet, setSelectedChangeSet] = useState<ChangeSet | null>(null);
  const [actionSelection, setActionSelection] = useState<ChangeSetActionSelection | null>(null);
  const changeSetsQuery = useChangeSets({
    kind: kind === 'ALL' ? undefined : kind,
    status: status !== 'ALL' && status !== 'ACTIONABLE' ? status : undefined,
    statuses: status === 'ACTIONABLE' ? ACTIONABLE_CHANGE_SET_STATUSES : undefined,
    scopeType,
    scopeID,
    limit: 500,
  });
  const canManage = mode === 'workbench' && hasSystemScope('write_commercialization');

  const rows = useMemo(() => {
    return [...(changeSetsQuery.data ?? [])]
      .filter((changeSet) => {
        if (kind !== 'ALL' && changeSet.kind !== kind) return false;
        if (
          status === 'ACTIONABLE' &&
          !ACTIONABLE_CHANGE_SET_STATUSES.includes(changeSet.status as (typeof ACTIONABLE_CHANGE_SET_STATUSES)[number])
        ) {
          return false;
        }
        if (status !== 'ALL' && status !== 'ACTIONABLE' && changeSet.status !== status) return false;
        if (scopeType && changeSet.scopeType !== scopeType) return false;
        if (scopeID && changeSet.scopeID !== scopeID) return false;
        return matchesChangeSetSearch(changeSet, query);
      })
      .sort((left, right) => new Date(getLastRelevantTime(right)).getTime() - new Date(getLastRelevantTime(left)).getTime());
  }, [changeSetsQuery.data, kind, query, scopeID, scopeType, status]);

  const hasActiveFilters = Boolean(query || kind !== 'ALL' || status !== defaultStatus || scopeType || scopeID);

  const resetFilters = () => {
    setQuery('');
    setKind('ALL');
    setStatus(defaultStatus);
    setScopeType(undefined);
    setScopeID(undefined);
  };

  const handleAction = (action: ChangeSetActionType, changeSet: ChangeSet) => {
    setSelectedChangeSet(null);
    setActionSelection({ action, changeSet });
  };

  return (
    <>
      <Header fixed>
        <div className='min-w-0'>
          <h2 className='truncate text-xl font-bold'>{t(`changeSets.${mode}.title`)}</h2>
          <p className='text-muted-foreground truncate text-sm'>{t(`changeSets.${mode}.description`)}</p>
        </div>
      </Header>

      <Main fixed className='gap-4'>
        <ChangeSetToolbar
          query={query}
          kind={kind}
          status={status}
          defaultStatus={defaultStatus}
          scopeType={scopeType}
          scopeID={scopeID}
          resultCount={rows.length}
          onQueryChange={setQuery}
          onKindChange={setKind}
          onStatusChange={setStatus}
          onClearScope={() => {
            setScopeType(undefined);
            setScopeID(undefined);
          }}
          onReset={resetFilters}
        />
        <ChangeSetTable
          rows={rows}
          loading={changeSetsQuery.isLoading}
          error={changeSetsQuery.error instanceof Error ? changeSetsQuery.error : null}
          canManage={canManage}
          emptyTitle={t(hasActiveFilters ? 'changeSets.empty.filteredTitle' : `changeSets.empty.${mode}Title`)}
          emptyDescription={t(hasActiveFilters ? 'changeSets.empty.filteredDescription' : `changeSets.empty.${mode}Description`)}
          onRetry={() => changeSetsQuery.refetch()}
          onOpen={setSelectedChangeSet}
          onAction={handleAction}
        />
      </Main>

      <ChangeSetDetailDialog
        changeSet={selectedChangeSet}
        canManage={canManage}
        onOpenChange={(open) => !open && setSelectedChangeSet(null)}
        onAction={handleAction}
      />
      {mode === 'workbench' && <ChangeSetReviewDialog selection={actionSelection} onClose={() => setActionSelection(null)} />}
    </>
  );
}
