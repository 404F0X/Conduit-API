import { createFileRoute } from '@tanstack/react-router';
import { ProjectGuard } from '@/components/project-guard';
import ModelMarket from '@/features/model-market';
import { ModePage } from '@/features/simple/mode-page';
import { SimpleModels } from '@/features/simple/models';

function ProjectModels() {
  return (
    <ProjectGuard>
      <ModePage simple={<SimpleModels />} enterprise={<ModelMarket />} />
    </ProjectGuard>
  );
}

export const Route = createFileRoute('/_authenticated/project/models/')({ component: ProjectModels });
