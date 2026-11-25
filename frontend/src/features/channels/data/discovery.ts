import { graphqlRequest } from '@/gql/graphql';

export type DiscoveryStage = 'MODELS' | 'PRICES';
export type StageResult = {
  status: string;
  discoveredCount: number;
  changedCount: number;
  unsupportedCount: number;
  errorCode?: string | null;
  message?: string | null;
  retryable: boolean;
  snapshotID?: string | null;
  completedAt?: string | null;
};
export type DiscoveryOperation = {
  operationID: string;
  channelID: string;
  origin: string;
  attempt: number;
  state: string;
  channelCreated: boolean;
  createdAt: string;
  updatedAt: string;
  leaseExpiresAt?: string | null;
  models: StageResult;
  prices: StageResult;
  packageValidation: { status: string; totalItems: number; validItems: number; invalidItems: number; warningItems: number };
  packages: Array<{ changeSetID: string; kind: string; status: string; itemCount: number; reused: boolean }>;
  supersededSourceChangeSetIDs: string[];
  recurringModelSyncEnabled: boolean;
  recurringPriceDiscoveryState: string;
};
export type RebuildSources = { sources: Array<{ id: string; expectedRevision: string }>; supersedeSources: boolean };
const STAGE = `status discoveredCount changedCount unsupportedCount errorCode message retryable snapshotID completedAt`;
const FIELDS = `operationID channelID origin attempt state channelCreated createdAt updatedAt leaseExpiresAt models { ${STAGE} } prices { ${STAGE} } packageValidation { status totalItems validItems invalidItems warningItems } packages { changeSetID kind status itemCount reused } supersededSourceChangeSetIDs recurringModelSyncEnabled recurringPriceDiscoveryState`;
export async function getDiscovery(operationID: string) {
  return graphqlRequest<{ channelDiscoveryOperation: DiscoveryOperation | null }>(
    `query DiscoveryOperation($operationID: ID!) { channelDiscoveryOperation(operationID: $operationID) { ${FIELDS} } }`,
    { operationID }
  ).then((d) => d.channelDiscoveryOperation);
}
export async function initializeDiscovery(input: {
  channelID: string;
  operationID: string;
  stages?: DiscoveryStage[];
  rebuild?: RebuildSources;
}) {
  return graphqlRequest<{ initializeNewApiChannel: DiscoveryOperation }>(
    `mutation InitializeNewApiChannel($channelID: ID!, $operationID: ID!, $stages: [ChannelDiscoveryStage!]!, $rebuild: RebuildObservedPricePackagesInput) { initializeNewApiChannel(channelID: $channelID, operationID: $operationID, stages: $stages, rebuild: $rebuild) { ${FIELDS} } }`,
    { ...input, stages: input.stages ?? ['MODELS', 'PRICES'] }
  ).then((d) => d.initializeNewApiChannel);
}
export async function retryDiscovery(input: {
  operationID: string;
  expectedAttempt: number;
  stages: DiscoveryStage[];
  credentials?: { pat: string; userID?: string };
}) {
  return graphqlRequest<{ retryNewApiChannelDiscovery: DiscoveryOperation }>(
    `mutation RetryNewApiChannelDiscovery($operationID: ID!, $expectedAttempt: Int!, $stages: [ChannelDiscoveryStage!]!, $credentials: NewApiAccountCredentialsInput) { retryNewApiChannelDiscovery(operationID: $operationID, expectedAttempt: $expectedAttempt, stages: $stages, credentials: $credentials) { ${FIELDS} } }`,
    input
  ).then((d) => d.retryNewApiChannelDiscovery);
}
export function rememberDiscovery(channelID: string, operationID: string) {
  localStorage.setItem(`conduit:channel-discovery:${channelID.split('/').at(-1)}`, operationID);
}
export function recalledDiscovery(channelID: string) {
  return localStorage.getItem(`conduit:channel-discovery:${channelID.split('/').at(-1)}`) ?? undefined;
}
export function retryableStages(operation: DiscoveryOperation): DiscoveryStage[] {
  return (['MODELS', 'PRICES'] as const).filter((stage) => operation[stage === 'MODELS' ? 'models' : 'prices'].retryable);
}
export function canonicalNewApiURL(raw: string) {
  const url = new URL(raw.trim());
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password)
    throw new Error('Use an HTTP(S) URL without embedded credentials');
  url.search = '';
  url.hash = '';
  url.pathname = url.pathname.replace(/\/+$/, '').replace(/\/v1$/, '');
  return url.toString().replace(/\/+$/, '');
}

export function isNewApiAnthropicControlOnly(channelType: string, managementAdapter: string | null | undefined) {
  return channelType === 'anthropic' && managementAdapter === 'new_api';
}
