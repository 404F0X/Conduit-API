import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import OperationsPage from '@/features/operations';

function ProtectedOperations() {
  return (
    <RouteGuard requiredScopes={['read_dashboard']}>
      <OperationsPage />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/operations/')({ component: ProtectedOperations });
