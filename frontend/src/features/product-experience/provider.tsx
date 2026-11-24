import { useMemo, type ReactNode } from 'react';
import { useAuthStore } from '@/stores/authStore';
import { useSelectedProjectId } from '@/stores/projectStore';
import { holdsScope, collectScopeSets } from '@/lib/permission-core';
import { ProductExperienceContext } from './context';
import { useProductExperienceSettings } from './data';
import { DEFAULT_PRODUCT_MODE, resolveProductLandingPath } from './mode';

export function ProductExperienceProvider({ children }: { children: ReactNode }) {
  const user = useAuthStore((state) => state.auth.user);
  const selectedProjectId = useSelectedProjectId();
  const query = useProductExperienceSettings();
  const mode = query.data?.mode ?? DEFAULT_PRODUCT_MODE;
  const canReadRequests = holdsScope(collectScopeSets(user, selectedProjectId), 'read_requests');
  const value = useMemo(
    () => ({
      mode,
      features: query.data?.features ?? [],
      isLoading: query.isLoading,
      homePath: resolveProductLandingPath(mode, user?.isOwner ?? false, canReadRequests),
    }),
    [mode, query.isLoading, user?.isOwner, canReadRequests, query.data?.features]
  );

  return <ProductExperienceContext.Provider value={value}>{children}</ProductExperienceContext.Provider>;
}
