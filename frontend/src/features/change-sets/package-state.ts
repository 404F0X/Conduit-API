export type PackageAction = 'submit' | 'approve' | 'publish' | 'reject';

export function packageActionAllowed(detail: { summary: { status: string }; availableActions: string[] }, action: PackageAction): boolean {
  if (['APPLIED', 'REJECTED', 'SUPERSEDED'].includes(detail.summary.status)) return false;
  if (action === 'approve' && detail.summary.status !== 'PENDING_REVIEW') return false;
  if (action === 'publish' && !['APPROVED', 'DRAFT'].includes(detail.summary.status)) return false;
  if (action === 'submit' && detail.summary.status !== 'DRAFT') return false;
  return detail.availableActions.includes({ submit: 'SUBMIT', approve: 'APPROVE', publish: 'PUBLISH', reject: 'REJECT' }[action]);
}

export function packageEmptyState(
  counts: Array<{ count: number }>,
  filters: { search?: string; kind?: string; scopeType?: string; scopeID?: string }
) {
  // Counts span all statuses, but retain the server's kind, scope and search filters.
  return counts.some((entry) => entry.count > 0) || Boolean(filters.search?.trim() || filters.kind || filters.scopeType || filters.scopeID)
    ? 'filtered'
    : 'empty';
}

export function eventActor(
  event: { actorType: string; actorID?: string | null; detail: unknown },
  creator?: { createdBy?: string | null; actorName?: string | null }
) {
  const detail = event.detail !== null && typeof event.detail === 'object' ? (event.detail as Record<string, unknown>) : {};
  const knownCreator = event.actorID && event.actorID === creator?.createdBy ? creator.actorName : undefined;
  const name = typeof detail.actorName === 'string' ? detail.actorName.trim() : knownCreator?.trim();
  return { name: name || undefined, id: event.actorID || undefined, type: event.actorType };
}
