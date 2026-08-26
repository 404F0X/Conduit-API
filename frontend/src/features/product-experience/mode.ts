export type ProductMode = 'SIMPLE' | 'ENTERPRISE';

export const DEFAULT_PRODUCT_MODE: ProductMode = 'ENTERPRISE';

export function resolveProductLandingPath(mode: ProductMode, isOwner: boolean): '/' | '/project/dashboard' {
  if (mode === 'SIMPLE') {
    return isOwner ? '/' : '/project/dashboard';
  }

  return isOwner ? '/' : '/project/dashboard';
}

export function isProductModeAllowed(mode: ProductMode, allowedModes?: ProductMode[]): boolean {
  return !allowedModes?.length || allowedModes.includes(mode);
}

export function resolveProjectSelection(
  mode: ProductMode,
  selectedProjectId: string | null,
  availableProjectIds: string[],
  primaryProjectId: string | null
): string | null {
  if (mode === 'SIMPLE') {
    return primaryProjectId && availableProjectIds.includes(primaryProjectId) ? primaryProjectId : null;
  }
  if (selectedProjectId && availableProjectIds.includes(selectedProjectId)) {
    return selectedProjectId;
  }
  return availableProjectIds[0] ?? null;
}
