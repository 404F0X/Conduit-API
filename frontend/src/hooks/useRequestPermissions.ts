import { useMemo } from 'react';
import { holdsScope } from '@/lib/permission-core';
import { useScopeSets } from '@/hooks/useScopeSets';

export interface RequestPermissions {
  canViewUsers: boolean;
  canViewApiKeys: boolean;
  canViewChannels: boolean;
  canViewRoles: boolean;
}

/**
 * 决定 GraphQL 查询里要不要带上用户、Key、渠道、角色这些关联字段。
 * 判断规则和 usePermissions 相同，都来自 lib/permission-core.ts。
 */
export function useRequestPermissions(): RequestPermissions {
  const scopeSets = useScopeSets();

  return useMemo(
    () => ({
      canViewUsers: holdsScope(scopeSets, 'read_users'),
      canViewApiKeys: holdsScope(scopeSets, 'read_api_keys'),
      canViewChannels: holdsScope(scopeSets, 'read_channels'),
      canViewRoles: holdsScope(scopeSets, 'read_roles'),
    }),
    [scopeSets]
  );
}
