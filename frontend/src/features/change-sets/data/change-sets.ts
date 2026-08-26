import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';

export type ChangeSetKind = 'PROVIDER_PRICE' | 'MODEL_MAPPING' | 'RETAIL_PRICE';
export type ChangeSetStatus = 'DRAFT' | 'PENDING_REVIEW' | 'APPLIED' | 'REJECTED' | 'SUPERSEDED' | 'INVALID';

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

const CHANGE_SETS = `
  query ChangeSets($kind: ChangeSetKind, $status: ChangeSetStatus, $scopeType: String, $scopeID: ID, $limit: Int) {
    changeSets(kind: $kind, status: $status, scopeType: $scopeType, scopeID: $scopeID, limit: $limit) {
      id kind scopeType scopeID title status baseRevision sourceRevision
      appliedTargetType appliedTargetID validationError reviewNote
      createdBy submittedBy reviewedBy
      createdAt updatedAt submittedAt reviewedAt appliedAt
      items { id itemKey action beforeSnapshot afterSnapshot sourceSnapshot validationError createdAt updatedAt }
      events { id eventType actorType actorID detail createdAt }
    }
  }
`;

const CREATE_RETAIL = `
  mutation CreateRetailPriceChangeSet($priceBookID: ID!) {
    createRetailPriceChangeSet(priceBookID: $priceBookID) {
      id kind scopeType scopeID title status baseRevision sourceRevision
      createdAt updatedAt
      items { id itemKey action beforeSnapshot afterSnapshot sourceSnapshot validationError createdAt updatedAt }
      events { id eventType actorType actorID detail createdAt }
    }
  }
`;

const SAVE_RETAIL_ITEM = `
  mutation SaveRetailPriceChangeSetItem($input: SaveRetailPriceChangeSetItemInput!) {
    saveRetailPriceChangeSetItem(input: $input) { id itemKey action afterSnapshot }
  }
`;

const SUBMIT = `mutation SubmitChangeSet($id: ID!) { submitChangeSet(id: $id) { id status } }`;
const APPROVE = `mutation ApproveChangeSet($id: ID!, $reviewNote: String) { approveChangeSet(id: $id, reviewNote: $reviewNote) { id status } }`;
const REJECT = `mutation RejectChangeSet($id: ID!, $reviewNote: String) { rejectChangeSet(id: $id, reviewNote: $reviewNote) { id status } }`;

type ChangeSetFilters = {
  kind?: ChangeSetKind;
  status?: ChangeSetStatus;
  statuses?: readonly ChangeSetStatus[];
  scopeType?: string;
  scopeID?: string;
  limit?: number;
  enabled?: boolean;
};

export function useChangeSets(filters: ChangeSetFilters = {}) {
  const { enabled = true, statuses, ...variables } = filters;
  return useQuery({
    queryKey: ['changeSets', { ...variables, statuses }],
    queryFn: async () => {
      if (!statuses?.length) {
        return graphqlRequest<{ changeSets: ChangeSet[] }>(CHANGE_SETS, variables).then((data) => data.changeSets);
      }

      const batches = await Promise.all(
        [...new Set(statuses)].map((status) =>
          graphqlRequest<{ changeSets: ChangeSet[] }>(CHANGE_SETS, { ...variables, status }).then((data) => data.changeSets)
        )
      );
      return [...new Map(batches.flat().map((changeSet) => [changeSet.id, changeSet])).values()];
    },
    enabled,
  });
}

function useChangeSetMutation<T>(mutationFn: (input: T) => Promise<unknown>) {
  const client = useQueryClient();
  return useMutation({
    mutationFn,
    // Approval may atomically mark a stale change set as superseded and still
    // return an operation error. Refresh on both success and failure so the
    // review queue always reflects the committed state.
    onSettled: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: ['changeSets'] }),
        client.invalidateQueries({ queryKey: ['commercialization-catalog'] }),
        client.invalidateQueries({ queryKey: ['upstream-supply-catalog'] }),
        client.invalidateQueries({ queryKey: ['models'] }),
        client.invalidateQueries({ queryKey: ['channels'] }),
        client.invalidateQueries({ queryKey: ['generalSettings'] }),
      ]),
  });
}

export function useCreateRetailPriceChangeSet() {
  return useChangeSetMutation((priceBookID: string) =>
    graphqlRequest<{ createRetailPriceChangeSet: ChangeSet }>(CREATE_RETAIL, { priceBookID }).then(
      (data) => data.createRetailPriceChangeSet
    )
  );
}

export function useSaveRetailPriceChangeSetItem() {
  return useChangeSetMutation((input: { changeSetID: string; publicModelID: string; price: unknown }) =>
    graphqlRequest(SAVE_RETAIL_ITEM, { input })
  );
}

export function useSubmitChangeSet() {
  return useChangeSetMutation((id: string) => graphqlRequest(SUBMIT, { id }));
}

export function useApproveChangeSet() {
  return useChangeSetMutation((input: { id: string; reviewNote?: string }) => graphqlRequest(APPROVE, input));
}

export function useRejectChangeSet() {
  return useChangeSetMutation((input: { id: string; reviewNote?: string }) => graphqlRequest(REJECT, input));
}
