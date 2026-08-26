import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import SimpleGroupsPage from '@/features/user-groups';

function ProtectedGroups() {
  return (
    <RouteGuard requiredScopes={['read_groups']}>
      <SimpleGroupsPage />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/groups/')({ component: ProtectedGroups });
