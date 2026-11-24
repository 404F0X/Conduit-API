import { createFileRoute } from '@tanstack/react-router';
import { GOVERNANCE_ROUTE_SCOPES } from '@/config/route-permission';
import { RouteGuard } from '@/components/route-guard';
import { validateChangeSetSearch } from '@/features/change-sets/change-set-search';
import { ChangelogPage } from '@/features/change-sets/changelog-page';

function ProtectedChangelog() {
  const search = Route.useSearch();

  return (
    <RouteGuard requiredScopes={GOVERNANCE_ROUTE_SCOPES}>
      <ChangelogPage key={JSON.stringify(search)} initialFilters={search} />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/changelog/')({
  component: ProtectedChangelog,
  validateSearch: validateChangeSetSearch,
});
