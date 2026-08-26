export interface ChannelFilterState {
  nameFilter: string;
  typeFilter: string[];
  tabFilteredTypes: string[];
  statusFilter: string[];
  showErrorOnly: boolean;
}

export function buildChannelWhereClause(state: ChannelFilterState): Record<string, string | string[] | boolean> {
  const where: Record<string, string | string[] | boolean> = {};

  if (state.nameFilter) where.nameContainsFold = state.nameFilter;

  const combinedTypes = Array.from(new Set([...state.typeFilter, ...state.tabFilteredTypes]));
  if (combinedTypes.length > 0) where.typeIn = combinedTypes;

  where.statusIn = state.statusFilter.length > 0 ? state.statusFilter : ['enabled', 'disabled'];
  if (state.showErrorOnly) where.errorMessageNotNil = true;

  return where;
}
