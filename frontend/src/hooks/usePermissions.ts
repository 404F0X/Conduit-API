import { useCallback, useMemo } from 'react';
import { holdsScope, satisfiesScopes } from '@/lib/permission-core';
import { useScopeSets } from '@/hooks/useScopeSets';

/**
 * Hook for checking user permissions based on scopes
 * Provides utilities to check if user has specific permissions for actions
 * Supports both system-level and project-level scopes.
 *
 * 判断规则统一在 lib/permission-core.ts（站长全权、'*' 全权、其余完全相等）。
 */
export function usePermissions() {
  const scopeSets = useScopeSets();
  const { user, isOwner } = scopeSets;

  // Check if user has a specific scope at system level only
  const hasSystemScope = useCallback((requiredScope: string): boolean => holdsScope(scopeSets, requiredScope, 'system'), [scopeSets]);

  // Check if user has a specific scope at project level only (system-level '*' applies to every project)
  const hasProjectScope = useCallback((requiredScope: string): boolean => holdsScope(scopeSets, requiredScope, 'project'), [scopeSets]);

  // Check if user has a specific scope (system-level or project-level)
  const hasScope = useCallback((requiredScope: string): boolean => holdsScope(scopeSets, requiredScope, 'any'), [scopeSets]);

  // Check if user has any of the required scopes
  const hasAnyScope = useCallback((requiredScopes: string[]): boolean => satisfiesScopes(scopeSets, requiredScopes, 'any'), [scopeSets]);

  // Check if user has all of the required scopes
  const hasAllScopes = useCallback((requiredScopes: string[]): boolean => satisfiesScopes(scopeSets, requiredScopes, 'all'), [scopeSets]);

  // Common permission checks for channel operations
  const channelPermissions = useMemo(
    () => ({
      canRead: hasScope('read_channels'),
      canWrite: hasScope('write_channels'),
      canBulkImport: hasScope('write_channels'), // Bulk import requires write permission
      canCreate: hasScope('write_channels'),
      canEdit: hasScope('write_channels'),
      canDelete: hasScope('write_channels'),
      canTest: hasScope('read_channels'), // Test requires at least read permission
      canOrder: hasScope('write_channels'), // Ordering requires write permission
    }),
    [hasScope]
  );

  // Common permission checks for user operations
  const userPermissions = useMemo(
    () => ({
      canRead: hasScope('read_users'),
      canWrite: hasScope('write_users'),
      canCreate: hasScope('write_users'),
      canEdit: hasScope('write_users'),
      canDelete: isOwner, // Only system owners can delete users
    }),
    [hasScope, isOwner]
  );

  // Common permission checks for role operations
  const rolePermissions = useMemo(
    () => ({
      canRead: hasScope('read_roles'),
      canWrite: hasScope('write_roles'),
      canCreate: hasScope('write_roles'),
      canEdit: hasScope('write_roles'),
      canDelete: hasScope('write_roles'),
    }),
    [hasScope]
  );

  // Common permission checks for API key operations
  const apiKeyPermissions = useMemo(
    () => ({
      canRead: hasScope('read_api_keys'),
      canWrite: hasScope('write_api_keys'),
      canCreate: hasScope('write_api_keys'),
      canEdit: hasScope('write_api_keys'),
      canDelete: hasScope('write_api_keys'),
    }),
    [hasScope]
  );

  // Common permission checks for model operations
  const modelPermissions = useMemo(
    () => ({
      canRead: hasScope('read_channels'),
      canWrite: hasScope('write_channels'),
      canCreate: hasScope('write_channels'),
      canEdit: hasScope('write_channels'),
      canDelete: hasScope('write_channels'),
    }),
    [hasScope]
  );

  // Common permission checks for project operations
  const projectPermissions = useMemo(
    () => ({
      canRead: hasScope('read_projects'),
      canWrite: hasScope('write_projects'),
      canCreate: hasScope('write_projects'),
      canEdit: hasScope('write_projects'),
      canDelete: isOwner, // Only system owners can delete projects
    }),
    [hasScope, isOwner]
  );

  return {
    user,
    isOwner,
    hasScope,
    hasSystemScope,
    hasProjectScope,
    hasAnyScope,
    hasAllScopes,
    channelPermissions,
    userPermissions,
    rolePermissions,
    apiKeyPermissions,
    modelPermissions,
    projectPermissions,
  };
}
