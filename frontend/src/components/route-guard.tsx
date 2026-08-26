import { useEffect } from 'react';
import { useLocation, useRouter } from '@tanstack/react-router';
import { IconShieldX, IconArrowLeft } from '@tabler/icons-react';
import { getRouteConfig } from '@/config/route-permission';
import { useTranslation } from 'react-i18next';
import { useRoutePermissions } from '@/hooks/useRoutePermissions';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { useProductExperience, isProductModeAllowed, type ProductMode } from '@/features/product-experience';

interface RouteGuardProps {
  children: React.ReactNode;
  requiredScopes?: string[];
  requireAll?: boolean;
  fallbackPath?: string;
  showForbidden?: boolean;
  productModes?: ProductMode[];
}

export function RouteGuard({
  children,
  requiredScopes = [],
  requireAll = false,
  fallbackPath,
  showForbidden = true,
  productModes,
}: RouteGuardProps) {
  const router = useRouter();
  const location = useLocation();
  const { userScopes, isOwner } = useRoutePermissions();
  const { mode: productMode, isLoading: isProductModeLoading, homePath } = useProductExperience();
  const configuredProductModes = productModes ?? getRouteConfig(location.pathname)?.productModes;
  const isModeAllowed = isProductModeAllowed(productMode, configuredProductModes);
  const resolvedFallbackPath = fallbackPath ?? homePath;

  // 检查用户是否有所需权限
  const hasRequiredScopes = requireAll
    ? requiredScopes.every((scope) => userScopes.includes(scope))
    : requiredScopes.some((scope) => userScopes.includes(scope));
  const hasAccess = isOwner || userScopes.includes('*') || requiredScopes.length === 0 || hasRequiredScopes;

  useEffect(() => {
    if (!isProductModeLoading && !isModeAllowed) {
      router.navigate({ to: homePath, replace: true });
      return;
    }
    if (!hasAccess && !showForbidden) {
      // 如果没有权限且不显示禁止页面，则重定向
      router.navigate({ to: resolvedFallbackPath });
    }
  }, [hasAccess, homePath, isModeAllowed, isProductModeLoading, resolvedFallbackPath, router, showForbidden]);

  if ((configuredProductModes?.length && isProductModeLoading) || !isModeAllowed) {
    return null;
  }

  if (!hasAccess) {
    if (showForbidden) {
      return <ForbiddenPage onGoBack={() => router.navigate({ to: resolvedFallbackPath })} />;
    }
    return null; // 重定向中，不显示任何内容
  }

  return <>{children}</>;
}

function ForbiddenPage({ onGoBack }: { onGoBack: () => void }) {
  const { t } = useTranslation();

  return (
    <div className='flex h-screen items-center justify-center'>
      <div className='max-w-md text-center'>
        <div className='mb-6'>
          <IconShieldX className='mx-auto h-16 w-16 text-red-500' />
        </div>

        <Alert className='mb-6'>
          <IconShieldX className='h-4 w-4' />
          <AlertTitle>{t('common.routeGuard.accessDenied')}</AlertTitle>
          <AlertDescription>{t('common.routeGuard.noPermission')}</AlertDescription>
        </Alert>

        <Button onClick={onGoBack} variant='outline' className='gap-2'>
          <IconArrowLeft className='h-4 w-4' />
          {t('common.routeGuard.goBack')}
        </Button>
      </div>
    </div>
  );
}
