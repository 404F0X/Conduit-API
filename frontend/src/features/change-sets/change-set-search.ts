import type { ChangeSetKind, ChangeSetStatus } from './data/change-sets';

const kinds: readonly ChangeSetKind[] = [
  'PROVIDER_PRICE',
  'MODEL_MAPPING',
  'RETAIL_PRICE',
  'CHANNEL_CONFIGURATION',
  'MODEL_SUPPLY',
  'GROUP_ACCESS',
  'IDENTITY_ACCESS',
  'PROJECT_CONFIGURATION',
  'SYSTEM_CONFIGURATION',
  'PROJECT_CREDIT',
];
const statuses: ChangeSetStatus[] = ['DRAFT', 'PENDING_REVIEW', 'APPROVED', 'APPLIED', 'REJECTED', 'SUPERSEDED', 'INVALID'];

export type ChangeSetRouteSearch = {
  q?: string;
  kind?: ChangeSetKind;
  status?: ChangeSetStatus;
  scopeType?: string;
  scopeID?: string;
  after?: string;
  all?: boolean;
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
    after: optionalString(search.after),
    all: search.all === true || search.all === 'true' ? true : undefined,
  };
}

export type PackageDetailSearch = ChangeSetRouteSearch & { list?: 'workbench' | 'changelog' };
export function validatePackageDetailSearch(search: Record<string, unknown>): PackageDetailSearch {
  return { ...validateChangeSetSearch(search), list: search.list === 'changelog' ? 'changelog' : 'workbench' };
}
