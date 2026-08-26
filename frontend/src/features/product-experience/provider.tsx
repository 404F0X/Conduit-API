import { useMemo, type ReactNode } from 'react';
import { useAuthStore } from '@/stores/authStore';
import { ProductExperienceContext } from './context';
import { useProductExperienceSettings } from './data';
import { DEFAULT_PRODUCT_MODE, resolveProductLandingPath } from './mode';

export function ProductExperienceProvider({ children }: { children: ReactNode }) {
  const user = useAuthStore((state) => state.auth.user);
  const query = useProductExperienceSettings();
  const mode = query.data?.mode ?? DEFAULT_PRODUCT_MODE;
  const value = useMemo(
    () => ({
      mode,
      isLoading: query.isLoading,
      homePath: resolveProductLandingPath(mode, user?.isOwner ?? false),
    }),
    [mode, query.isLoading, user?.isOwner]
  );

  return <ProductExperienceContext.Provider value={value}>{children}</ProductExperienceContext.Provider>;
}
