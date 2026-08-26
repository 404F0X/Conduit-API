import { createFileRoute } from '@tanstack/react-router';
import { RouteGuard } from '@/components/route-guard';
import ModelsManagement from '@/features/models';
import { validateModelsSearch } from '@/features/models/model-search';

function ProtectedModels() {
  return (
    <RouteGuard requiredScopes={['read_channels']}>
      <ModelsManagement />
    </RouteGuard>
  );
}

export const Route = createFileRoute('/_authenticated/models/')({
  component: ProtectedModels,
  validateSearch: validateModelsSearch,
});
