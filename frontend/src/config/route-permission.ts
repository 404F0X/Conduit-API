import type { ProductMode } from '@/features/product-experience/mode';

// 路由权限配置
export type ScopeLevel = 'system' | 'project' | 'any';

export interface RouteConfig {
  path: string;
  requiredScopes?: string[];
  scopeMatch?: 'any' | 'all';
  scopeLevel?: ScopeLevel; // 权限级别：system 只检查系统级权限，project 只检查项目级权限，any 检查两者
  mode?: 'hidden' | 'disabled'; // 当没有权限时的处理方式
  productModes?: ProductMode[];
  children?: RouteConfig[];
}

export interface RouteGroup {
  title: string;
  scopeLevel?: ScopeLevel; // 路由组的默认权限级别
  routes: RouteConfig[];
}

// 定义所有路由的权限配置
export const routeConfigs: RouteGroup[] = [
  {
    title: 'Admin',
    scopeLevel: 'system', // Admin 路由组只能通过 system-level 权限访问
    routes: [
      {
        path: '/',
        requiredScopes: ['read_dashboard'],
        mode: 'hidden',
      },
      {
        path: '/operations',
        requiredScopes: ['read_dashboard'],
        mode: 'hidden',
      },
      {
        path: '/projects',
        requiredScopes: ['read_projects'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/users',
        requiredScopes: ['read_users'],
        mode: 'hidden',
      },
      {
        path: '/billing',
        requiredScopes: ['read_billing', 'read_subscriptions'],
        scopeMatch: 'any',
        mode: 'hidden',
      },
      {
        path: '/groups',
        requiredScopes: ['read_groups'],
        mode: 'hidden',
      },
      {
        path: '/roles',
        requiredScopes: ['read_roles'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/channels',
        requiredScopes: ['read_channels'],
        mode: 'hidden',
      },
      {
        path: '/models',
        requiredScopes: ['read_channels'],
        mode: 'hidden',
      },
      {
        path: '/change-sets',
        requiredScopes: ['read_commercialization'],
        mode: 'hidden',
      },
      {
        path: '/changelog',
        requiredScopes: ['read_commercialization'],
        mode: 'hidden',
      },
      {
        path: '/prompt-protection-rules',
        requiredScopes: ['read_channels'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/data-storages',
        requiredScopes: ['read_data_storages'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/api-keys',
        requiredScopes: ['read_api_keys'],
        mode: 'hidden',
      },
      {
        path: '/system',
        requiredScopes: ['read_settings'],
        mode: 'hidden',
      },
      {
        path: '/permission-demo',
        // 权限演示页面所有用户都可以访问
        productModes: ['ENTERPRISE'],
      },
    ],
  },
  {
    title: 'Project',
    scopeLevel: 'any', // Project 路由组可以通过 system-level 或 project-level 权限访问
    routes: [
      {
        path: '/project/dashboard',
        requiredScopes: ['read_requests'],
        scopeLevel: 'any',
        mode: 'hidden',
      },
      {
        path: '/project/api-keys',
        requiredScopes: ['read_api_keys'],
        mode: 'hidden',
      },
      {
        path: '/project/wallet',
        // Self-service wallet is available to every authenticated user.
      },
      {
        path: '/project/models',
        // Personalized catalog is available to every authenticated user.
      },
      {
        path: '/project/prompts',
        requiredScopes: ['read_prompts'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/project/requests',
        requiredScopes: ['read_requests'],
        mode: 'hidden',
      },
      {
        path: '/project/usage-logs',
        requiredScopes: ['read_requests'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/project/traces',
        requiredScopes: ['read_requests'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/project/threads',
        requiredScopes: ['read_requests'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/project/users',
        requiredScopes: ['read_users'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/project/roles',
        requiredScopes: ['read_roles'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
      {
        path: '/project/playground',
        // The playground enumerates upstream channels/models. Keep it hidden
        // from self-service users who only own API-key/request permissions.
        requiredScopes: ['read_channels'],
        mode: 'hidden',
        productModes: ['ENTERPRISE'],
      },
    ],
  },
  {
    title: 'Settings',
    routes: [
      {
        path: '/settings',
        // Profile 设置所有用户都可以访问
      },
      {
        path: '/settings/profile',
        // Profile 设置所有用户都可以访问
      },
      {
        path: '/settings/appearance',
        // Appearance 设置所有用户都可以访问
      },
      {
        path: '/settings/notifications',
        // Notifications 设置所有用户都可以访问
      },
    ],
  },
];

// 获取路由配置的辅助函数
export function getRouteConfig(path: string): RouteConfig | undefined {
  let bestMatch: RouteConfig | undefined;
  for (const group of routeConfigs) {
    for (const route of group.routes) {
      if (route.path === path || (route.path !== '/' && path.startsWith(`${route.path}/`))) {
        if (!bestMatch || route.path.length > bestMatch.path.length) {
          bestMatch = route;
        }
      }
      if (route.children) {
        const childConfig = route.children.find(
          (child) => child.path === path || (child.path !== '/' && path.startsWith(`${child.path}/`))
        );
        if (childConfig && (!bestMatch || childConfig.path.length > bestMatch.path.length)) {
          bestMatch = childConfig;
        }
      }
    }
  }
  return bestMatch;
}

// 检查用户是否有访问路由的权限
export function hasRouteAccess(userScopes: string[], routeConfig: RouteConfig): boolean {
  if (!routeConfig.requiredScopes || routeConfig.requiredScopes.length === 0) {
    return true;
  }

  // 如果用户有通配符权限，则拥有所有权限
  if (userScopes.includes('*')) {
    return true;
  }

  // 检查用户是否拥有所需的任一权限
  const predicate = (scope: string) => userScopes.includes(scope);
  return routeConfig.scopeMatch === 'all' ? routeConfig.requiredScopes.every(predicate) : routeConfig.requiredScopes.some(predicate);
}

// 检查用户是否有访问路由组的权限
export function hasGroupAccess(userScopes: string[], group: RouteGroup): boolean {
  return group.routes.some((route) => hasRouteAccess(userScopes, route));
}
