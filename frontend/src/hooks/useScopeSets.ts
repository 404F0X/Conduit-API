import { useMemo } from 'react';
import { useAuthStore, type AuthUser } from '@/stores/authStore';
import { useSelectedProjectId } from '@/stores/projectStore';
import { collectScopeSets, type ScopeSets } from '@/lib/permission-core';
import { useMe } from '@/features/auth/data/auth';

/**
 * 当前登录用户在当前选中项目下的权限集合。
 * usePermissions / useRoutePermissions / useRequestPermissions / RouteGuard 都从这里取，
 * 判断规则统一在 lib/permission-core.ts。
 */
export function useScopeSets(): ScopeSets & { user: AuthUser | null } {
  const { user: authUser } = useAuthStore((state) => state.auth);
  const { data: meData } = useMe();
  const selectedProjectId = useSelectedProjectId();
  const user = meData || authUser;

  return useMemo(() => ({ ...collectScopeSets(user, selectedProjectId), user: user ?? null }), [user, selectedProjectId]);
}
