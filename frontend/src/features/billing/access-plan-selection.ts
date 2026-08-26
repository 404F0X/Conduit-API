export type AccessPlanOption = {
  id: string;
  name: string;
};

export function normalizeAccessPlanIDs(ids: readonly string[]): string[] {
  return [...new Set(ids.filter(Boolean))];
}

export function accessPlanIDsForEdit(accessPlans: readonly AccessPlanOption[]): string[] {
  return normalizeAccessPlanIDs(accessPlans.map((accessPlan) => accessPlan.id));
}

export function toggleAccessPlanID(ids: readonly string[], id: string): string[] {
  const current = normalizeAccessPlanIDs(ids);
  return current.includes(id) ? current.filter((candidate) => candidate !== id) : [...current, id];
}

export function mergeAccessPlanOptions(...groups: ReadonlyArray<readonly AccessPlanOption[]>): AccessPlanOption[] {
  const options = new Map<string, AccessPlanOption>();
  for (const group of groups) {
    for (const option of group) {
      if (!options.has(option.id)) options.set(option.id, option);
    }
  }
  return [...options.values()];
}
