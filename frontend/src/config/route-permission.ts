import type { ScopeLevel } from '@/lib/permission-core';
import { holdsScope } from '@/lib/permission-core';
import type { ProductMode } from '@/features/product-experience/mode';
import { APP_ROUTES, findAppRoute, type AppRoute } from './app-routes';

export { GOVERNANCE_ROUTE_SCOPES } from './app-routes';

/**
 * 兼容层：老代码按 RouteConfig / RouteGroup 读路由权限。
 * 数据全部来自 config/app-routes.ts 的路由注册表，这里不再单独维护列表。
 */
export type { ScopeLevel };

export interface RouteConfig {
  path: string;
  requiredScopes?: string[];
  scopeMatch?: 'any' | 'all';
  scopeLevel?: ScopeLevel;
  mode?: 'hidden' | 'disabled';
  productModes?: ProductMode[];
  children?: RouteConfig[];
}

export interface RouteGroup {
  title: string;
  scopeLevel?: ScopeLevel;
  routes: RouteConfig[];
}

function toRouteConfig(route: AppRoute): RouteConfig {
  return {
    path: route.path,
    requiredScopes: route.requiredScopes,
    scopeMatch: route.scopeMatch,
    scopeLevel: route.scopeLevel,
    mode: route.deniedBehavior ?? 'hidden',
    productModes: route.modes,
  };
}

export const routeConfigs: RouteGroup[] = [
  {
    title: 'Admin',
    scopeLevel: 'system',
    routes: APP_ROUTES.filter((route) => route.scopeLevel === 'system').map(toRouteConfig),
  },
  {
    title: 'Project',
    scopeLevel: 'any',
    routes: APP_ROUTES.filter((route) => route.scopeLevel !== 'system').map(toRouteConfig),
  },
];

export function getRouteConfig(path: string): RouteConfig | undefined {
  const route = findAppRoute(path);
  return route ? toRouteConfig(route) : undefined;
}

/** 只按 scope 判断（不看站长身份和产品模式）；需要完整判断请用 useRoutePermissions。 */
export function hasRouteAccess(userScopes: string[], routeConfig: RouteConfig): boolean {
  const sets = { isOwner: false, systemScopes: userScopes, projectScopes: [] };
  const required = routeConfig.requiredScopes;
  if (!required || required.length === 0) return true;
  const predicate = (scope: string) => holdsScope(sets, scope);
  return routeConfig.scopeMatch === 'all' ? required.every(predicate) : required.some(predicate);
}

export function hasGroupAccess(userScopes: string[], group: RouteGroup): boolean {
  return group.routes.some((route) => hasRouteAccess(userScopes, route));
}
