import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import BillingPage from '@/features/billing';

function ProtectedBilling() {
  return (
    <RouteGuard requiredScopes={['read_users', 'read_settings']} requireAll>
      <BillingPage />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/billing/')({ component: ProtectedBilling });
