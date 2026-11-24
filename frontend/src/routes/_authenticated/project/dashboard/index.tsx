import { createFileRoute } from '@tanstack/react-router';
import { ProjectGuard } from '@/components/project-guard';
import { RouteGuard } from '@/components/route-guard';
import ProjectDashboard from '@/features/project-dashboard';
import { SimpleHome } from '@/features/simple/home';
import { ModePage } from '@/features/simple/mode-page';

function ProtectedProjectDashboard() {
  return (
    <ProjectGuard>
      {/* 权限要求按模式取自路由注册表：完整模式需要 read_requests，简单模式首页人人可进 */}
      <RouteGuard>
        <ModePage simple={<SimpleHome />} enterprise={<ProjectDashboard />} />
      </RouteGuard>
    </ProjectGuard>
  );
}

export const Route = createFileRoute('/_authenticated/project/dashboard/')({
  component: ProtectedProjectDashboard,
});
