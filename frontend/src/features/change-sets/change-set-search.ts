import type { ChangeSetKind, ChangeSetStatus } from './data/change-sets';

const kinds: ChangeSetKind[] = ['PROVIDER_PRICE', 'MODEL_MAPPING', 'RETAIL_PRICE'];
const statuses: ChangeSetStatus[] = ['DRAFT', 'PENDING_REVIEW', 'APPLIED', 'REJECTED', 'SUPERSEDED', 'INVALID'];

export type ChangeSetRouteSearch = {
  q?: string;
  kind?: ChangeSetKind;
  status?: ChangeSetStatus;
  scopeType?: string;
  scopeID?: string;
};

function optionalString(value: unknown): string | undefined {
  return typeof value === 'string' && value.trim() ? value.trim() : undefined;
}

export function validateChangeSetSearch(search: Record<string, unknown>): ChangeSetRouteSearch {
  const kind = optionalString(search.kind);
  const status = optionalString(search.status);

  return {
    q: optionalString(search.q),
    kind: kinds.includes(kind as ChangeSetKind) ? (kind as ChangeSetKind) : undefined,
    status: statuses.includes(status as ChangeSetStatus) ? (status as ChangeSetStatus) : undefined,
    scopeType: optionalString(search.scopeType),
    scopeID: optionalString(search.scopeID),
  };
}
