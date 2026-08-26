import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import { validateChangeSetSearch } from '@/features/change-sets/change-set-search';
import { ChangeSetWorkbenchPage } from '@/features/change-sets/workbench-page';

function ProtectedChangeSets() {
  const search = Route.useSearch();

  return (
    <RouteGuard requiredScopes={['read_commercialization']}>
      <ChangeSetWorkbenchPage key={JSON.stringify(search)} initialFilters={search} />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/change-sets/')({
  component: ProtectedChangeSets,
  validateSearch: validateChangeSetSearch,
});
