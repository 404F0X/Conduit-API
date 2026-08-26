import { useEffect } from 'react';
import { QueryClient } from '@tanstack/react-query';
import { createRootRouteWithContext, Outlet } from '@tanstack/react-router';
import { useAuthStore } from '@/stores/authStore';
import { Toaster } from '@/components/ui/sonner';
import { CommandMenu } from '@/components/command-menu';
import { InitializationGuard } from '@/components/initialization-guard';
import { NavigationProgress } from '@/components/navigation-progress';
import GeneralError from '@/features/errors/general-error';
import NotFoundError from '@/features/errors/not-found-error';
import { ProductExperienceProvider } from '@/features/product-experience';
import { useBrandSettings } from '@/features/system/data/system';

function DocumentTitleSync() {
  const accessToken = useAuthStore((state) => state.auth.accessToken);
  const hasToken = Boolean(accessToken);
  const { data: brandSettings } = useBrandSettings({ enabled: hasToken });

  useEffect(() => {
    document.title = (hasToken && brandSettings?.title) || 'Conduit API';
  }, [brandSettings?.title, hasToken]);

  return null;
}

export const Route = createRootRouteWithContext<{
  queryClient: QueryClient;
}>()({
  component: () => {
    return (
      <>
        <NavigationProgress />
        <InitializationGuard>
          <ProductExperienceProvider>
            <DocumentTitleSync />
            <Outlet />
            <CommandMenu />
          </ProductExperienceProvider>
        </InitializationGuard>
        <Toaster duration={3000} />
        {/* {import.meta.env.MODE === 'development' && (
          <>
            <ReactQueryDevtools buttonPosition='bottom-left' />
            <TanStackRouterDevtools position='bottom-right' />
          </>
        )} */}
      </>
    );
  },
  notFoundComponent: NotFoundError,
  errorComponent: GeneralError,
});
