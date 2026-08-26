import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import { validateChangeSetSearch } from '@/features/change-sets/change-set-search';
import { ChangelogPage } from '@/features/change-sets/changelog-page';

function ProtectedChangelog() {
  const search = Route.useSearch();

  return (
    <RouteGuard requiredScopes={['read_commercialization']}>
      <ChangelogPage key={JSON.stringify(search)} initialFilters={search} />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/changelog/')({
  component: ProtectedChangelog,
  validateSearch: validateChangeSetSearch,
});
