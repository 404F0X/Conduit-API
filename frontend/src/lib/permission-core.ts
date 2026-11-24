import type { AuthUser } from '@/stores/authStore';

/**
 * 全站唯一的权限判断规则。
 *
 * 以前有三份各自的实现（usePermissions、useRoutePermissions、useRequestPermissions），
 * 另外 route-guard 和 permission-utils 又各写了一份，规则不一致：
 * 有的认 '*'、有的不认；有的按前缀匹配路由、有的只认完全相等。
 * 现在所有地方都调用这里的函数：
 *  - 站长（isOwner）拥有全部权限；
 *  - 拥有 '*' 的人拥有全部权限（系统级的 '*' 对所有项目生效，项目级的 '*' 只对该项目生效）；
 *  - 其余按 scope 字符串完全相等判断，不做前缀匹配。
 */

export type ScopeLevel = 'system' | 'project' | 'any';
export type ScopeMatch = 'any' | 'all';

export const WILDCARD_SCOPE = '*';

export interface ScopeSets {
  isOwner: boolean;
  systemScopes: string[];
  projectScopes: string[];
}

type ScopeUser = Pick<AuthUser, 'isOwner' | 'scopes' | 'projects'> | null | undefined;

export function collectScopeSets(user: ScopeUser, projectId?: string | null): ScopeSets {
  if (!user) {
    return { isOwner: false, systemScopes: [], projectScopes: [] };
  }
  const project = projectId ? user.projects?.find((p) => p.projectID === projectId) : undefined;
  return {
    isOwner: !!user.isOwner,
    systemScopes: user.scopes ?? [],
    projectScopes: project?.scopes ?? [],
  };
}

function scopesForLevel(sets: ScopeSets, level: ScopeLevel): string[] {
  if (level === 'system') return sets.systemScopes;
  if (level === 'project') {
    // 系统级的 '*' 对所有项目生效
    return sets.systemScopes.includes(WILDCARD_SCOPE) ? [WILDCARD_SCOPE, ...sets.projectScopes] : sets.projectScopes;
  }
  return [...sets.systemScopes, ...sets.projectScopes];
}

/** 判断一个 scope 是否被持有（含站长与通配符）。 */
export function holdsScope(sets: ScopeSets, scope: string, level: ScopeLevel = 'any'): boolean {
  if (sets.isOwner) return true;
  const scopes = scopesForLevel(sets, level);
  return scopes.includes(WILDCARD_SCOPE) || scopes.includes(scope);
}

/** 判断一组 scope 是否满足；空列表视为不需要权限。 */
export function satisfiesScopes(
  sets: ScopeSets,
  required: readonly string[] | undefined,
  match: ScopeMatch = 'any',
  level: ScopeLevel = 'any'
): boolean {
  if (!required || required.length === 0) return true;
  if (sets.isOwner) return true;
  const predicate = (scope: string) => holdsScope(sets, scope, level);
  return match === 'all' ? required.every(predicate) : required.some(predicate);
}

/** 不依赖 React 的便捷写法，供非 hook 场景（例如 permission-utils）使用。 */
export function userHoldsScope(user: ScopeUser, scope: string, projectId?: string | null): boolean {
  return holdsScope(collectScopeSets(user, projectId), scope);
}
