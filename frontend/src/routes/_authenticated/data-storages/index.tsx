import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import DataStoragesManagement from '@/features/data-storages';

function ProtectedDataStorages() {
  return (
    <RouteGuard>
      <DataStoragesManagement />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/data-storages/')({
  component: ProtectedDataStorages,
});
