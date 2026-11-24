import { createFileRoute } from '@tanstack/react-router';
import { GOVERNANCE_ROUTE_SCOPES } from '@/config/route-permission';
import { RouteGuard } from '@/components/route-guard';
import { validatePackageDetailSearch } from '@/features/change-sets/change-set-search';
import { PackageDetailPage } from '@/features/change-sets/package-detail-page';

export const Route = createFileRoute('/_authenticated/change-sets/$changeSetId')({
  component: Page,
  validateSearch: validatePackageDetailSearch,
});
function Page() {
  const { changeSetId } = Route.useParams();
  const search = Route.useSearch();
  return (
    <RouteGuard requiredScopes={GOVERNANCE_ROUTE_SCOPES}>
      <PackageDetailPage key={changeSetId} id={changeSetId} returnContext={search} />
    </RouteGuard>
  );
}
