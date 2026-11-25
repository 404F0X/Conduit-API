import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { GraphQLRequestError, graphqlRequest } from '@/gql/graphql';
import type { NewApiExpressionPrice } from '@/features/models/components/simple-retail-price';

export const CHANGE_SET_KINDS = [
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
] as const;
export type ChangeSetKind = (typeof CHANGE_SET_KINDS)[number];
export type ChangeSetStatus = 'DRAFT' | 'PENDING_REVIEW' | 'APPROVED' | 'APPLIED' | 'REJECTED' | 'SUPERSEDED' | 'INVALID';

export type ChangeSetItem = {
  id: string;
  itemKey: string;
  action: 'CREATE' | 'UPDATE' | 'DELETE';
  beforeSnapshot?: unknown | null;
  afterSnapshot?: unknown | null;
  sourceSnapshot?: unknown | null;
  validationError?: string | null;
  createdAt: string;
  updatedAt: string;
};

export type ChangeSetEvent = {
  id: string;
  eventType: string;
  actorType: string;
  actorID?: string | null;
  detail: unknown;
  createdAt: string;
};

export type ChangeSet = {
  id: string;
  kind: ChangeSetKind;
  scopeType: string;
  scopeID: string;
  title: string;
  status: ChangeSetStatus;
  baseRevision: string;
  sourceRevision: string;
  appliedTargetType?: string | null;
  appliedTargetID?: string | null;
  validationError?: string | null;
  createdBy?: string | null;
  submittedBy?: string | null;
  reviewedBy?: string | null;
  reviewNote?: string | null;
  createdAt: string;
  updatedAt: string;
  submittedAt?: string | null;
  reviewedAt?: string | null;
  appliedAt?: string | null;
  items: ChangeSetItem[];
  events: ChangeSetEvent[];
};

export type PackageSummary = Omit<ChangeSet, 'items' | 'events' | 'baseRevision' | 'sourceRevision'> & {
  operationID?: string | null;
  revision: string;
  scopeName?: string | null;
  actorName?: string | null;
  sourceOrigin: string;
  itemCount: number;
  createCount: number;
  updateCount: number;
  deleteCount: number;
  invalidItemCount: number;
  validationState: string;
};
export type PackageIssue = {
  itemKey?: string | null;
  field?: string | null;
  code: string;
  message: string;
  severity: 'BLOCKING' | 'WARNING';
};
export type PackageDetail = {
  summary: PackageSummary;
  baseRevision: string;
  sourceRevision: string;
  currentBaseRevision?: string | null;
  reviewNote?: string | null;
  issues: PackageIssue[];
  availableActions: string[];
  appliedTargets: Array<{ type: string; id: string; itemKey: string }>;
};
export type ItemSummary = {
  id: string;
  itemKey: string;
  action: string;
  modelLabel?: string | null;
  changedFieldCount: number;
  validationState: string;
  issues: PackageIssue[];
};
export type ItemDetail = { item: ChangeSetItem; currentSnapshot?: unknown | null; currentMatchesBefore: boolean; issues: PackageIssue[] };
export type Connection<T> = {
  edges: Array<{ cursor: string; node: T }>;
  pageInfo: { hasNextPage: boolean; hasPreviousPage: boolean; startCursor?: string | null; endCursor?: string | null };
  totalCount: number;
};
export type PackageFilter = {
  kind?: ChangeSetKind;
  statuses?: readonly ChangeSetStatus[];
  scopeType?: string;
  scopeID?: string;
  search?: string;
};
export type ProviderPriceInput = {
  modelId: string;
  currencyCode: string;
  price?: unknown;
  billingExpression?: NewApiExpressionPrice | null;
};
export type ProviderEditBase = {
  channelID: string;
  revision: string;
  accountingCurrency: string;
  accountingSettingsVersion: string;
  prices: Array<{ id: string; modelID: string; currencyCode: string; price: unknown; billingExpression?: NewApiExpressionPrice | null }>;
};
export type RetailEditBase = {
  priceBookID: string;
  revision: string;
  accountingCurrency: string;
  accountingSettingsVersion: string;
  prices: Array<{ publicModelID: string; price: unknown }>;
};
export const SUMMARY_FIELDS = `id operationID kind scopeType scopeID scopeName title status revision sourceOrigin actorName createdBy createdAt updatedAt submittedAt reviewedAt appliedAt itemCount createCount updateCount deleteCount invalidItemCount validationState validationError`;
const ISSUE_FIELDS = `itemKey field code message severity`;
export const DETAIL_FIELDS = `summary { ${SUMMARY_FIELDS} } baseRevision sourceRevision currentBaseRevision reviewNote issues { ${ISSUE_FIELDS} } availableActions appliedTargets { type id itemKey }`;
const PAGE_FIELDS = `hasNextPage hasPreviousPage startCursor endCursor`;
export async function getPackage(id: string) {
  return graphqlRequest<{ changeSet: PackageDetail | null }>(`query ChangeSet($id: ID!) { changeSet(id: $id) { ${DETAIL_FIELDS} } }`, {
    id,
  }).then((d) => d.changeSet);
}
export async function getPackageByOperation(operationID: string, kind: ChangeSetKind, scopeType: string, scopeID: string) {
  return graphqlRequest<{ changeSetByOperation: PackageDetail | null }>(
    `query PackageByOperation($operationID: ID!, $kind: ChangeSetKind!, $scopeType: String!, $scopeID: ID!) { changeSetByOperation(operationID: $operationID, kind: $kind, scopeType: $scopeType, scopeID: $scopeID) { ${DETAIL_FIELDS} } }`,
    { operationID, kind, scopeType, scopeID }
  ).then((d) => d.changeSetByOperation);
}
export function usePackage(id?: string) {
  return useQuery({ queryKey: ['changeSet', id], queryFn: () => getPackage(id!), enabled: Boolean(id), staleTime: 0 });
}
export async function getPackagePage(filter: PackageFilter, after?: string) {
  return graphqlRequest<{ changeSets: Connection<PackageSummary> & { statusCounts: Array<{ status: ChangeSetStatus; count: number }> } }>(
    `query ChangeSets($filter: ChangeSetFilterInput, $after: Cursor, $first: Int!) { changeSets(filter: $filter, after: $after, first: $first) { edges { cursor node { ${SUMMARY_FIELDS} } } pageInfo { ${PAGE_FIELDS} } totalCount statusCounts { status count } } }`,
    { filter, after, first: 20 }
  ).then((d) => d.changeSets);
}
export function usePackagePage(filter: PackageFilter, after?: string) {
  return useQuery({ queryKey: ['changeSets', filter, after], queryFn: () => getPackagePage(filter, after) });
}
export async function getItemPage(id: string, after?: string, search?: string, problemsOnly = false) {
  return graphqlRequest<{ changeSetItems: Connection<ItemSummary> }>(
    `query PackageItems($id: ID!, $after: Cursor, $search: String, $problemsOnly: Boolean!) { changeSetItems(changeSetID: $id, after: $after, first: 50, search: $search, problemsOnly: $problemsOnly) { edges { cursor node { id itemKey action modelLabel changedFieldCount validationState issues { ${ISSUE_FIELDS} } } } pageInfo { ${PAGE_FIELDS} } totalCount } }`,
    { id, after, search, problemsOnly }
  ).then((d) => d.changeSetItems);
}
export async function getItem(id: string, itemID: string) {
  return graphqlRequest<{ changeSetItem: ItemDetail | null }>(
    `query PackageItem($id: ID!, $itemID: ID!) { changeSetItem(changeSetID: $id, itemID: $itemID) { item { id itemKey action beforeSnapshot afterSnapshot sourceSnapshot validationError createdAt updatedAt } currentSnapshot currentMatchesBefore issues { ${ISSUE_FIELDS} } } }`,
    { id, itemID }
  ).then((d) => d.changeSetItem);
}
export async function getEventPage(id: string, after?: string) {
  return graphqlRequest<{ changeSetEvents: Connection<ChangeSetEvent> }>(
    `query PackageEvents($id: ID!, $after: Cursor) { changeSetEvents(changeSetID: $id, after: $after, first: 50) { edges { cursor node { id eventType actorType actorID detail createdAt } } pageInfo { ${PAGE_FIELDS} } totalCount } }`,
    { id, after }
  ).then((d) => d.changeSetEvents);
}
export async function getProviderBase(channelID: string) {
  return graphqlRequest<{ providerPriceEditBase: ProviderEditBase }>(
    `query ProviderEditBase($channelID: ID!) { providerPriceEditBase(channelID: $channelID) { channelID revision accountingCurrency accountingSettingsVersion prices { id modelID currencyCode price { items { itemCode pricing { mode flatFee usagePerUnit usageTiered { tiers { upTo pricePerUnit } } } promptWriteCacheVariants { variantCode pricing { mode flatFee usagePerUnit usageTiered { tiers { upTo pricePerUnit } } } } } } billingExpression } } }`,
    { channelID }
  ).then((d) => d.providerPriceEditBase);
}
export async function getRetailBase(priceBookID: string) {
  return graphqlRequest<{ retailPriceEditBase: RetailEditBase }>(
    `query RetailEditBase($priceBookID: ID!) { retailPriceEditBase(priceBookID: $priceBookID) { priceBookID revision accountingCurrency accountingSettingsVersion prices { publicModelID price } } }`,
    { priceBookID }
  ).then((d) => d.retailPriceEditBase);
}
export type Correction = { sourceChangeSetID: string; expectedSourcePackageRevision: string; excludedItemKeys: string[]; reason: string };
export type CreateProviderResult = { operationID: string; outcome: 'CREATED' | 'NO_CHANGE'; changeSetDetail?: PackageDetail | null };
export async function findProviderOperation(operationID: string) {
  return graphqlRequest<{ providerPriceChangeSetOperation: CreateProviderResult | null }>(
    `query ProviderOperation($operationID: ID!) { providerPriceChangeSetOperation(operationID: $operationID) { operationID outcome changeSetDetail { ${DETAIL_FIELDS} } } }`,
    { operationID }
  ).then((d) => d.providerPriceChangeSetOperation);
}
export async function createProviderPackage(input: {
  channelID: string;
  operationID: string;
  expectedBaseRevision: string;
  input: ProviderPriceInput[];
  correction?: Correction;
}) {
  return graphqlRequest<{ createProviderPriceChangeSet: CreateProviderResult }>(
    `mutation CreateProviderPriceChangeSet($channelID: ID!, $operationID: ID!, $expectedBaseRevision: String!, $input: [SaveChannelModelPriceInput!]!, $correction: ChangeSetCorrectionInput) { createProviderPriceChangeSet(channelID: $channelID, operationID: $operationID, expectedBaseRevision: $expectedBaseRevision, input: $input, correction: $correction) { operationID outcome changeSetDetail { ${DETAIL_FIELDS} } } }`,
    input
  ).then((d) => d.createProviderPriceChangeSet);
}
function requireProviderDraftReceipt(receipt: CreateProviderResult | undefined, operationID: string): CreateProviderResult {
  if (receipt?.operationID === operationID) {
    if (receipt.outcome === 'NO_CHANGE' && receipt.changeSetDetail === null) return receipt;
    const id = receipt.changeSetDetail?.summary?.id;
    if (receipt.outcome === 'CREATED' && typeof id === 'string' && /^[1-9][0-9]{0,18}$/.test(id) && BigInt(id) <= 9223372036854775807n)
      return receipt;
  }
  throw new Error('Provider price draft receipt is incomplete');
}

export async function lookupProviderDraftReceipt(operationID: string) {
  const receipt = await findProviderOperation(operationID);
  return receipt === null ? null : requireProviderDraftReceipt(receipt, operationID);
}

export async function createProviderDraftFromDialog(
  intent: Parameters<typeof createProviderPackage>[0],
  create: typeof createProviderPackage,
  lookup = lookupProviderDraftReceipt
): Promise<{ result: CreateProviderResult; recovered?: true } | { validationError: GraphQLRequestError }> {
  try {
    return { result: requireProviderDraftReceipt(await create(intent), intent.operationID) };
  } catch (error) {
    if (!(error instanceof GraphQLRequestError) || !error.providerCreateValidationRejected) throw error;
    const receipt = await lookup(intent.operationID);
    if (receipt === null) return { validationError: error };
    return { result: receipt, recovered: true };
  }
}

export async function saveProviderPackage(input: { id: string; expectedRevision: string; input: ProviderPriceInput[] }) {
  return graphqlRequest<{ saveProviderPriceChangeSet: PackageDetail }>(
    `mutation SaveProviderPackage($id: ID!, $expectedRevision: String!, $input: [SaveChannelModelPriceInput!]!) { saveProviderPriceChangeSet(id: $id, expectedRevision: $expectedRevision, input: $input) { ${DETAIL_FIELDS} } }`,
    input
  ).then((d) => d.saveProviderPriceChangeSet);
}
export async function transitionPackage(
  action: 'submit' | 'approve' | 'publish' | 'reject',
  input: { id: string; expectedRevision: string; reviewNote?: string }
) {
  const field = action === 'approve' ? 'reviewChangeSet' : `${action}ChangeSet`;
  const withNote = action === 'approve' || action === 'reject';
  return graphqlRequest<Record<string, PackageDetail>>(
    `mutation PackageTransition($id: ID!, $expectedRevision: String!${withNote ? ', $reviewNote: String' : ''}) { ${field}(id: $id, expectedRevision: $expectedRevision${withNote ? ', reviewNote: $reviewNote' : ''}) { ${DETAIL_FIELDS} } }`,
    input
  ).then((d) => d[field]);
}
export async function createRetailPackage(input: { priceBookID: string; operationID: string; expectedBaseRevision: string }) {
  return graphqlRequest<{ createRetailPriceChangeSet: PackageDetail }>(
    `mutation CreateRetailPriceChangeSet($priceBookID: ID!, $operationID: ID!, $expectedBaseRevision: String!) { createRetailPriceChangeSet(priceBookID: $priceBookID, operationID: $operationID, expectedBaseRevision: $expectedBaseRevision) { ${DETAIL_FIELDS} } }`,
    input
  ).then((d) => d.createRetailPriceChangeSet);
}
export async function saveRetailItem(input: { changeSetID: string; expectedRevision: string; publicModelID: string; price: unknown }) {
  return graphqlRequest<{ saveRetailPriceChangeSetItem: { item: ChangeSetItem; changeSetRevision: string } }>(
    `mutation SaveRetailPriceChangeSetItem($input: SaveRetailPriceChangeSetItemInput!) { saveRetailPriceChangeSetItem(input: $input) { item { id itemKey action beforeSnapshot afterSnapshot sourceSnapshot validationError createdAt updatedAt } changeSetRevision } }`,
    { input }
  ).then((d) => d.saveRetailPriceChangeSetItem);
}
export function useChangeSets(
  filters: {
    kind?: ChangeSetKind;
    status?: ChangeSetStatus;
    statuses?: readonly ChangeSetStatus[];
    scopeType?: string;
    scopeID?: string;
    limit?: number;
    enabled?: boolean;
  } = {}
) {
  const { enabled = true, status, statuses, kind, scopeType, scopeID } = filters;
  return useQuery({
    queryKey: ['changeSets', kind, statuses, status, scopeType, scopeID],
    queryFn: () =>
      getPackagePage({ kind, statuses: statuses ?? (status ? [status] : undefined), scopeType, scopeID }).then((d) =>
        d.edges.map((e) => e.node)
      ),
    enabled,
  });
}
function useChangeSetMutation<T, R>(mutationFn: (input: T) => Promise<R>) {
  const client = useQueryClient();
  return useMutation({
    mutationFn,
    onSettled: () =>
      Promise.all(
        [
          'changeSets',
          'changeSet',
          'packageItems',
          'packageItem',
          'packageEvents',
          'commercialization-catalog',
          'upstream-supply-catalog',
          'models',
          'channels',
          'generalSettings',
        ].map((key) => client.invalidateQueries({ queryKey: [key] }))
      ),
  });
}
export function useCreateRetailPriceChangeSet() {
  return useChangeSetMutation(createRetailPackage);
}
export function useSaveRetailPriceChangeSetItem() {
  return useChangeSetMutation(saveRetailItem);
}
export function useSubmitChangeSet() {
  return useChangeSetMutation((input: { id: string; expectedRevision: string }) => transitionPackage('submit', input));
}
export function useApproveChangeSet() {
  return useChangeSetMutation((input: { id: string; expectedRevision: string; reviewNote?: string }) =>
    transitionPackage('approve', input)
  );
}
export function useRejectChangeSet() {
  return useChangeSetMutation((input: { id: string; expectedRevision: string; reviewNote?: string }) => transitionPackage('reject', input));
}
