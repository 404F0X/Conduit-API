import type { ChangeSet } from './data/change-sets';

export const ACTIONABLE_CHANGE_SET_STATUSES = ['DRAFT', 'PENDING_REVIEW', 'INVALID'] as const;

export function getLastRelevantTime(changeSet: ChangeSet): string {
  return changeSet.appliedAt ?? changeSet.reviewedAt ?? changeSet.submittedAt ?? changeSet.updatedAt ?? changeSet.createdAt;
}

export function formatChangeSetTime(value: string | null | undefined, locale: string): string {
  if (!value) return '-';

  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;

  return new Intl.DateTimeFormat(locale, {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date);
}

export function matchesChangeSetSearch(changeSet: ChangeSet, query: string): boolean {
  const normalizedQuery = query.trim().toLocaleLowerCase();
  if (!normalizedQuery) return true;

  const values = [
    changeSet.id,
    changeSet.title,
    changeSet.scopeType,
    changeSet.scopeID,
    changeSet.validationError,
    changeSet.reviewNote,
    ...changeSet.items.flatMap((item) => [item.itemKey, item.validationError]),
  ];

  return values.some((value) =>
    String(value ?? '')
      .toLocaleLowerCase()
      .includes(normalizedQuery)
  );
}

export function hasJsonContent(value: unknown): boolean {
  if (value === null || value === undefined) return false;
  if (Array.isArray(value)) return value.length > 0;
  if (typeof value === 'object') return Object.keys(value).length > 0;
  return String(value).length > 0;
}
