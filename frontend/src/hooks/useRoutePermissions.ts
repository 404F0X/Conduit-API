import { useCallback, useMemo } from 'react';
import { findAppRoute, requiredScopesFor, type AppRoute } from '@/config/app-routes';
import { satisfiesScopes } from '@/lib/permission-core';
import { useScopeSets } from '@/hooks/useScopeSets';
import { type NavGroup, type NavItem } from '@/components/layout/types';
import { isProductModeAllowed, useProductExperience } from '@/features/product-experience';

/**
 * 路由级权限：某个地址当前用户在当前模式下能不能打开。
 * 路由数据来自 config/app-routes.ts，判断规则来自 lib/permission-core.ts。
 */
export function useRoutePermissions() {
  const scopeSets = useScopeSets();
  const { mode: productMode } = useProductExperience();

  const canAccessRoute = useCallback(
    (route: AppRoute): boolean => {
      if (!isProductModeAllowed(productMode, route.modes)) return false;
      return satisfiesScopes(scopeSets, requiredScopesFor(route, productMode), route.scopeMatch ?? 'any', route.scopeLevel ?? 'any');
    },
    [productMode, scopeSets]
  );

  // 检查单个地址（按最长前缀匹配注册表；没登记的地址默认放行，由页面自己的守卫决定）
  const checkRouteAccess = useCallback(
    (path: string): { hasAccess: boolean; mode?: 'hidden' | 'disabled'; route?: AppRoute } => {
      const route = findAppRoute(path);
      if (!route) return { hasAccess: true };
      return { hasAccess: canAccessRoute(route), mode: route.deniedBehavior ?? 'hidden', route };
    },
    [canAccessRoute]
  );

  // 过滤导航项：没权限且是 hidden 的去掉，disabled 的置灰
  const filterNavItems = useCallback(
    (items: NavItem[]): NavItem[] =>
      items
        .filter((item) => {
          if (!('url' in item) || !item.url) return true;
          const access = checkRouteAccess(item.url as string);
          return access.hasAccess || access.mode !== 'hidden';
        })
        .map((item) => {
          if (!('url' in item) || !item.url) return item;
          const access = checkRouteAccess(item.url as string);
          return { ...item, isDisabled: !access.hasAccess && access.mode === 'disabled' };
        }),
    [checkRouteAccess]
  );

  // 过滤导航组：组里一个都看不到时整组去掉（以前按组名匹配，界面是中文时匹配不上，等于没过滤）
  const filterNavGroups = useCallback(
    (groups: NavGroup[]): NavGroup[] =>
      groups.map((group) => ({ ...group, items: filterNavItems(group.items) })).filter((group) => group.items.length > 0),
    [filterNavItems]
  );

  const userScopes = useMemo(() => [...scopeSets.systemScopes, ...scopeSets.projectScopes], [scopeSets]);

  return {
    userScopes,
    isOwner: scopeSets.isOwner,
    scopeSets,
    canAccessRoute,
    checkRouteAccess,
    filterNavItems,
    filterNavGroups,
  };
}
