import { createFileRoute } from '@tanstack/react-router';
import { ProjectGuard } from '@/components/project-guard';
import { ModePage } from '@/features/simple/mode-page';
import { SimpleWallet } from '@/features/simple/wallet';
import WalletPage from '@/features/wallet';

function ProjectWallet() {
  return (
    <ProjectGuard>
      <ModePage simple={<SimpleWallet />} enterprise={<WalletPage />} />
    </ProjectGuard>
  );
}

export const Route = createFileRoute('/_authenticated/project/wallet/')({ component: ProjectWallet });
