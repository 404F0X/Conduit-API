import { useMemo, type ReactNode } from 'react';
import { useAuthStore } from '@/stores/authStore';
import { ProductExperienceContext } from './context';
import { useProductExperienceSettings } from './data';
import { DEFAULT_PRODUCT_MODE, resolveProductLandingPath } from './mode';

export function ProductExperienceProvider({ children }: { children: ReactNode }) {
  const user = useAuthStore((state) => state.auth.user);
  const query = useProductExperienceSettings();
  const mode = query.data?.mode ?? DEFAULT_PRODUCT_MODE;
  const isAdministrator =
    !!user?.isOwner ||
    !!user?.scopes?.some(
      (scope) => scope === '*' || ['read_dashboard', 'read_channels', 'read_users', 'read_settings', 'read_billing'].includes(scope)
    );
  const value = useMemo(
    () => ({
      mode,
      isLoading: query.isLoading,
      homePath: resolveProductLandingPath(mode, isAdministrator),
      features: query.data?.features ?? [],
    }),
    [mode, query.isLoading, isAdministrator, query.data?.features]
  );

  return <ProductExperienceContext.Provider value={value}>{children}</ProductExperienceContext.Provider>;
}
