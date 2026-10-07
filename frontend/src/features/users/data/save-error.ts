export function classifyUserSaveError(error: unknown): 'duplicate-email' | 'forbidden' | 'other' {
  if (!error || typeof error !== 'object') return 'other';
  const value = error as { message?: string; status?: number; extensions?: { code?: string } };
  if (value.extensions?.code === 'DUPLICATE_EMAIL' || /^email '.+' already exists$/i.test(value.message ?? '')) return 'duplicate-email';
  if (
    value.status === 403 ||
    value.extensions?.code === 'FORBIDDEN' ||
    /permission denied|authz:|only .*owner may/i.test(value.message ?? '')
  )
    return 'forbidden';
  return 'other';
}
