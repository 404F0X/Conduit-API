import { createFileRoute } from '@tanstack/react-router';
import { ProjectGuard } from '@/components/project-guard';
import { RouteGuard } from '@/components/route-guard';
import ApiKeys from '@/features/apikeys';
import { ModePage } from '@/features/simple/mode-page';
import { SimpleTokens } from '@/features/simple/tokens';

function ProtectedProjectApiKeys() {
  return (
    <ProjectGuard>
      <RouteGuard requiredScopes={['read_api_keys']}>
        <ModePage simple={<SimpleTokens />} enterprise={<ApiKeys />} />
      </RouteGuard>
    </ProjectGuard>
  );
}

export const Route = createFileRoute('/_authenticated/project/api-keys/')({
  component: ProtectedProjectApiKeys,
});
