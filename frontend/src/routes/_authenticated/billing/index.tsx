import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import BillingPage from '@/features/billing';

function ProtectedBilling() {
  return (
    <RouteGuard requiredScopes={['read_billing', 'read_subscriptions']}>
      <BillingPage />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/billing/')({ component: ProtectedBilling });
