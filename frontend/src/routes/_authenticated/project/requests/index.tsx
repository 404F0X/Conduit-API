import { createFileRoute } from '@tanstack/react-router';
import { ProjectGuard } from '@/components/project-guard';
import { RouteGuard } from '@/components/route-guard';
import RequestsManagement from '@/features/requests';
import { ModePage } from '@/features/simple/mode-page';
import { SimpleUsageLogs } from '@/features/simple/usage';

function ProtectedProjectRequests() {
  return (
    <ProjectGuard>
      <RouteGuard requiredScopes={['read_requests']}>
        <ModePage simple={<SimpleUsageLogs />} enterprise={<RequestsManagement />} />
      </RouteGuard>
    </ProjectGuard>
  );
}

export const Route = createFileRoute('/_authenticated/project/requests/')({
  validateSearch: (search: Record<string, unknown>) => search,
  component: ProtectedProjectRequests,
});
