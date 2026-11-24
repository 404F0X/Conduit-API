export type ConfigurationCategory = 'connection' | 'billing' | 'request' | 'other';

export function getChannelConfigurationTarget(path = ''): { category: ConfigurationCategory; selector: string } {
  const field = path.replace(/^settings\./, '');
  if (field === 'billingCurrency') return { category: 'billing', selector: '#channel-billing-currency' };
  if (field === 'rechargeMultiplier') return { category: 'billing', selector: '#channel-recharge-multiplier' };
  if (['websiteURL', 'quotaCurrency', 'actualQuotaUsed', 'quotaRemaining'].includes(field)) {
    return { category: 'billing', selector: `[name="${field}"]` };
  }
  if (field.startsWith('retryableStatusCodes')) return { category: 'request', selector: '#channel-retry-status-codes' };
  if (field.startsWith('retryableErrorPatterns')) return { category: 'request', selector: '#channel-retry-error-patterns' };
  if (field === 'policies.stream') return { category: 'request', selector: '[data-testid="channel-stream-policy-select"]' };
  if (field === 'passThroughUserAgent') return { category: 'request', selector: '#channel-user-agent-passthrough' };
  if (field === 'passThroughBody') return { category: 'request', selector: '#channel-body-passthrough' };
  if (field === 'tags' || field.startsWith('tags.')) return { category: 'other', selector: '[data-channel-field="tags"]' };
  if (field === 'remark') return { category: 'other', selector: '[name="remark"]' };
  if (field === 'name') return { category: 'connection', selector: '[data-testid="channel-name-input"]' };
  if (field === 'baseURL') return { category: 'connection', selector: '[data-testid="channel-base-url-input"]' };
  if (field === 'defaultTestModel') return { category: 'connection', selector: '[data-testid="default-test-model-select"]' };
  if (field === 'autoSyncModelPattern') return { category: 'connection', selector: '[name="autoSyncModelPattern"]' };
  if (field === 'selectedFetchedModels') return { category: 'connection', selector: '#channel-confirm-fetched-models' };
  if (field.startsWith('credentials.apiKeys')) return { category: 'connection', selector: '[data-testid="channel-api-key-input"]' };
  if (field.startsWith('credentials')) return { category: 'connection', selector: '[data-channel-field="credentials"]' };
  if (
    ['supportedModels', 'manualModels', 'selectedFetchedModels', 'autoSyncSupportedModels'].some(
      (key) => field === key || field.startsWith(`${key}.`)
    )
  ) {
    return { category: 'connection', selector: '[data-channel-field="models"]' };
  }
  return { category: 'connection', selector: '[data-channel-category="connection"]' };
}

export function getFirstChannelConfigurationErrorPath(errors: unknown, prefix = ''): string | undefined {
  if (!errors || typeof errors !== 'object') return undefined;
  const record = errors as Record<string, unknown>;
  if (typeof record.message === 'string' || typeof record.type === 'string') return prefix;
  for (const [key, value] of Object.entries(record)) {
    // RHF refs can contain DOM nodes and cycles; only traverse validation paths.
    if (['ref', 'types', 'message', 'type'].includes(key)) continue;
    const result = getFirstChannelConfigurationErrorPath(value, prefix ? `${prefix}.${key}` : key);
    if (result !== undefined) return result;
  }
  return undefined;
}
