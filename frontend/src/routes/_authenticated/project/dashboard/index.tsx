import { createFileRoute } from '@tanstack/react-router';
import { ProjectGuard } from '@/components/project-guard';
import { RouteGuard } from '@/components/route-guard';
import ProjectDashboard from '@/features/project-dashboard';

function ProtectedProjectDashboard() {
  return (
    <ProjectGuard>
      <RouteGuard requiredScopes={['read_requests']}>
        <ProjectDashboard />
      </RouteGuard>
    </ProjectGuard>
  );
}

export const Route = createFileRoute('/_authenticated/project/dashboard/')({
  component: ProtectedProjectDashboard,
});
