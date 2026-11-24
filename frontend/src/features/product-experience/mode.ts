export type ProductMode = 'SIMPLE' | 'ENTERPRISE';

export const DEFAULT_PRODUCT_MODE: ProductMode = 'ENTERPRISE';

export type ProductLandingPath = '/' | '/project/dashboard' | '/project/models';

/**
 * 登录后和“返回”按钮去哪里。
 * - 站长去站点概览；
 * - 简单模式下首页对所有人开放，普通用户去首页；
 * - 企业模式的工作区概览需要 read_requests，没有这个权限的人去模型广场（人人可看），
 *   避免“无权限页的返回按钮又指向一个无权限页”。
 */
export function resolveProductLandingPath(mode: ProductMode, isOwner: boolean, canReadRequests = true): ProductLandingPath {
  if (isOwner) return '/';
  if (mode === 'SIMPLE') return '/project/dashboard';
  return canReadRequests ? '/project/dashboard' : '/project/models';
}

export function isProductModeAllowed(mode: ProductMode, allowedModes?: ProductMode[]): boolean {
  return !allowedModes?.length || allowedModes.includes(mode);
}

export function resolveProjectSelection(
  _mode: ProductMode,
  selectedProjectId: string | null,
  availableProjectIds: string[],
  primaryProjectId: string | null
): string | null {
  if (selectedProjectId && availableProjectIds.includes(selectedProjectId)) {
    return selectedProjectId;
  }
  if (primaryProjectId && availableProjectIds.includes(primaryProjectId)) return primaryProjectId;
  return availableProjectIds.length === 1 ? availableProjectIds[0] : null;
}
