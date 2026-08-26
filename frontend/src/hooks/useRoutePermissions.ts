import { useCallback, useMemo } from 'react';
import { routeConfigs, type RouteConfig, type RouteGroup, type ScopeLevel } from '@/config/route-permission';
import { useAuthStore } from '@/stores/authStore';
import { useSelectedProjectId } from '@/stores/projectStore';
import { type NavGroup, type NavItem } from '@/components/layout/types';
import { useMe } from '@/features/auth/data/auth';
import { isProductModeAllowed, useProductExperience } from '@/features/product-experience';

export function useRoutePermissions() {
  const { user: authUser } = useAuthStore((state) => state.auth);
  const { data: meData } = useMe();
  const selectedProjectId = useSelectedProjectId();
  const { mode: productMode } = useProductExperience();

  // Use data from me query if available, otherwise fall back to auth store
  const user = meData || authUser;
  const systemScopes = useMemo(() => user?.scopes || [], [user?.scopes]);
  const isOwner = user?.isOwner || false;

  // Get project-level scopes for the selected project
  const projectScopes = useMemo(() => {
    if (!selectedProjectId || !user?.projects) {
      return [];
    }
    const project = user.projects.find((p) => p.projectID === selectedProjectId);
    return project?.scopes || [];
  }, [selectedProjectId, user?.projects]);

  // 检查路由权限（根据 scopeLevel 决定检查哪个级别的权限）
  const hasRouteAccess = useCallback(
    (routeConfig: RouteConfig, groupScopeLevel?: ScopeLevel): boolean => {
      if (!isProductModeAllowed(productMode, routeConfig.productModes)) {
        return false;
      }

      if (!routeConfig.requiredScopes || routeConfig.requiredScopes.length === 0) {
        return true;
      }

      if (isOwner) {
        return true;
      }

      const scopeLevel = routeConfig.scopeLevel || groupScopeLevel || 'any';
      let scopesToCheck: string[] = [];

      if (scopeLevel === 'system') {
        scopesToCheck = systemScopes;
      } else if (scopeLevel === 'project') {
        scopesToCheck = projectScopes;
      } else {
        scopesToCheck = [...systemScopes, ...projectScopes];
      }

      if (scopesToCheck.includes('*')) {
        return true;
      }

      const predicate = (scope: string) => scopesToCheck.includes(scope);
      return routeConfig.scopeMatch === 'all' ? routeConfig.requiredScopes.every(predicate) : routeConfig.requiredScopes.some(predicate);
    },
    [isOwner, productMode, projectScopes, systemScopes]
  );

  // 检查路由组权限
  const hasGroupAccess = useCallback(
    (group: RouteGroup): boolean => group.routes.some((route) => hasRouteAccess(route, group.scopeLevel)),
    [hasRouteAccess]
  );

  // 检查单个路由权限
  const checkRouteAccess = useMemo(() => {
    return (path: string): { hasAccess: boolean; mode?: 'hidden' | 'disabled' } => {
      const { routeConfig, groupScopeLevel } = getRouteConfigByPathWithGroup(path);
      if (!routeConfig) {
        return { hasAccess: true };
      }

      const access = hasRouteAccess(routeConfig, groupScopeLevel);
      return {
        hasAccess: access,
        mode: routeConfig.mode,
      };
    };
  }, [hasRouteAccess]);

  // 检查路由组权限
  const checkGroupAccess = useMemo(() => {
    return (group: RouteGroup): boolean => {
      return hasGroupAccess(group);
    };
  }, [hasGroupAccess]);

  // 过滤导航项
  const filterNavItems = useMemo(() => {
    return (items: NavItem[]): NavItem[] => {
      return items
        .filter((item) => {
          if ('url' in item) {
            const access = checkRouteAccess(item.url as string);

            // 如果是隐藏模式且没有权限，则过滤掉
            if (!access.hasAccess && access.mode === 'hidden') {
              return false;
            }
          }

          return true;
        })
        .map((item) => {
          if ('url' in item) {
            const access = checkRouteAccess(item.url as string);

            return {
              ...item,
              isDisabled: !access.hasAccess && access.mode === 'disabled',
            };
          }

          return item;
        });
    };
  }, [checkRouteAccess]);

  // 过滤导航组
  const filterNavGroups = useMemo(() => {
    return (groups: NavGroup[]): NavGroup[] => {
      return groups
        .filter((group) => {
          // 找到对应的路由组配置
          const routeGroup = routeConfigs.find((rg) => rg.title === group.title);
          if (!routeGroup) {
            return true; // 如果没有配置，默认显示
          }

          // 检查组是否有可访问的路由
          return checkGroupAccess(routeGroup);
        })
        .map((group) => ({
          ...group,
          items: filterNavItems(group.items),
        }));
    };
  }, [checkGroupAccess, filterNavItems]);

  return {
    userScopes: [...systemScopes, ...projectScopes],
    isOwner,
    checkRouteAccess,
    checkGroupAccess,
    filterNavItems,
    filterNavGroups,
  };
}

// 辅助函数：根据路径查找路由配置及其所属组的 scopeLevel
function getRouteConfigByPathWithGroup(path: string): {
  routeConfig?: RouteConfig;
  groupScopeLevel?: ScopeLevel;
} {
  for (const group of routeConfigs) {
    for (const route of group.routes) {
      if (route.path === path) {
        return { routeConfig: route, groupScopeLevel: group.scopeLevel };
      }
      if (route.children) {
        const childConfig = route.children.find((child) => child.path === path);
        if (childConfig) {
          return { routeConfig: childConfig, groupScopeLevel: group.scopeLevel };
        }
      }
    }
  }
  return {};
}
