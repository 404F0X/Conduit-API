import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useSelectedProjectId } from '@/stores/projectStore';

export type CreditRedemptionCodeStatus = 'ACTIVE' | 'REDEEMED' | 'REVOKED' | 'EXPIRED';

export type CreditRedemptionCode = {
  id: string;
  batchID: string;
  codeHint: string;
  description?: string | null;
  amount: string;
  currency: string;
  maxRedemptions: number;
  redemptionCount: number;
  remainingRedemptions: number;
  status: CreditRedemptionCodeStatus;
  expiresAt?: string | null;
  redeemedAt?: string | null;
  revokedAt?: string | null;
  createdAt: string;
};

export type CreditRedemptionCodePage = {
  items: CreditRedemptionCode[];
  total: number;
  limit: number;
  offset: number;
};

export type GeneratedCreditRedemptionCode = {
  id: string;
  code: string;
  codeHint: string;
};

export type CreateCreditRedemptionCodesInput = {
  amount: string;
  quantity: number;
  maxRedemptions: number;
  expiresAt?: string;
  description?: string;
};

export type CreateCreditRedemptionCodesPayload = {
  batchID: string;
  amount: string;
  currency: string;
  quantity: number;
  maxRedemptions: number;
  expiresAt?: string | null;
  codes: GeneratedCreditRedemptionCode[];
};

export type CreditRedemptionReceipt = {
  id: string;
  codeID: string;
  projectID: string;
  userID: string;
  amount: string;
  currency: string;
  redeemedAt: string;
};

const CODE_FIELDS = `id batchID codeHint description amount currency maxRedemptions redemptionCount remainingRedemptions status expiresAt redeemedAt revokedAt createdAt`;

const REDEMPTION_CODES_QUERY = `
  query CreditRedemptionCodes($limit: Int!, $offset: Int!) {
    creditRedemptionCodes(limit: $limit, offset: $offset) {
      items { ${CODE_FIELDS} }
      total
      limit
      offset
    }
  }
`;

const CREATE_REDEMPTION_CODES_MUTATION = `
  mutation CreateCreditRedemptionCodes($input: CreateCreditRedemptionCodesInput!) {
    createCreditRedemptionCodes(input: $input) {
      batchID
      amount
      currency
      quantity
      maxRedemptions
      expiresAt
      codes { id code codeHint }
    }
  }
`;

const REVOKE_REDEMPTION_CODE_MUTATION = `
  mutation RevokeCreditRedemptionCode($id: ID!) {
    revokeCreditRedemptionCode(id: $id) { ${CODE_FIELDS} }
  }
`;

const REDEEM_CREDIT_CODE_MUTATION = `
  mutation RedeemCreditCode($code: String!) {
    redeemCreditCode(code: $code) {
      id
      codeID
      projectID
      userID
      amount
      currency
      redeemedAt
    }
  }
`;

export function useCreditRedemptionCodes(limit = 50, offset = 0, enabled = true) {
  return useQuery({
    queryKey: ['billing', 'redemption-codes', { limit, offset }],
    queryFn: async () => {
      const data = await graphqlRequest<{ creditRedemptionCodes: CreditRedemptionCodePage }>(REDEMPTION_CODES_QUERY, {
        limit,
        offset,
      });
      return data.creditRedemptionCodes;
    },
    enabled,
  });
}

export function useCreateCreditRedemptionCodes() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: async (input: CreateCreditRedemptionCodesInput) => {
      const data = await graphqlRequest<{ createCreditRedemptionCodes: CreateCreditRedemptionCodesPayload }>(
        CREATE_REDEMPTION_CODES_MUTATION,
        { input }
      );
      return data.createCreditRedemptionCodes;
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ['billing', 'redemption-codes'] }).catch(() => undefined);
    },
    // The dialog owns the operation-specific error message. Supplying a local
    // handler replaces the QueryClient default and prevents a second generic
    // mutation toast for the same failure.
    onError: () => undefined,
    // Generated plaintext codes must disappear as soon as their observer is gone.
    gcTime: 0,
  });
}

export function useRevokeCreditRedemptionCode() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: async (id: string) => {
      const data = await graphqlRequest<{ revokeCreditRedemptionCode: CreditRedemptionCode }>(REVOKE_REDEMPTION_CODE_MUTATION, { id });
      return data.revokeCreditRedemptionCode;
    },
    onSuccess: () => {
      void client.invalidateQueries({ queryKey: ['billing', 'redemption-codes'] }).catch(() => undefined);
    },
    // The table action renders its own revoke error; suppress the global
    // mutation handler so one rejection produces one notification.
    onError: () => undefined,
  });
}

export function useRedeemCreditCode() {
  const client = useQueryClient();
  const selectedProjectID = useSelectedProjectId();

  return useMutation({
    mutationFn: async (code: string) => {
      if (!selectedProjectID) throw new Error('No Project is selected');
      const data = await graphqlRequest<{ redeemCreditCode: CreditRedemptionReceipt }>(
        REDEEM_CREDIT_CODE_MUTATION,
        { code },
        { 'X-Project-ID': selectedProjectID }
      );
      return data.redeemCreditCode;
    },
    onSuccess: (receipt) => {
      // A balance refresh failure must not turn a committed redemption into a
      // false failure that encourages the user to submit the one-time code again.
      void Promise.allSettled([
        client.invalidateQueries({ queryKey: ['billing', 'my-project-balance'] }),
        client.invalidateQueries({ queryKey: ['billing', 'project-balance', receipt.projectID] }),
        client.invalidateQueries({ queryKey: ['billing', 'my-project-wallet-comparison'] }),
        client.invalidateQueries({ queryKey: ['billing', 'project-wallet-comparison', receipt.projectID] }),
      ]);
    },
    // The redemption dialog deliberately collapses every invalid, expired,
    // revoked, or exhausted code into one non-enumerating message. Providing
    // a mutation-level handler overrides the QueryClient's generic error
    // toast; mutateAsync still rejects so the dialog's catch block can show
    // the single purpose-built message and keep the entered code visible.
    onError: () => undefined,
  });
}
