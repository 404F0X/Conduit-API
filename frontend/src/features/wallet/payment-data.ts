import { z } from 'zod';
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useAuthStore } from '@/stores/authStore';
import { useProjectStore, useSelectedProjectId } from '@/stores/projectStore';

const integer = z.string().regex(/^(0|[1-9]\d*)$/);
const positiveInteger = z.string().regex(/^[1-9]\d*$/);
export const paymentOrderSchema = z.object({
  id: z.string(),
  projectID: z.string(),
  status: z.string(),
  channel: z.enum(['ALIPAY', 'WECHAT']),
  currency: z.literal('CNY'),
  amountMinor: positiveInteger,
  creditsMicros: positiveInteger,
  rechargeMultiplierPPM: positiveInteger,
  accountingVersion: z.string(),
  createdAt: z.string(),
  expiresAt: z.string(),
  paidAt: z.string().nullable(),
  checkoutURL: z.string().nullable(),
  reviewReason: z.string().nullable(),
});
export type PaymentOrder = z.infer<typeof paymentOrderSchema>;
export type PaymentChannel = PaymentOrder['channel'];
export const paymentCapabilitiesSchema = z
  .object({
    enabled: z.boolean(),
    currency: z.literal('CNY'),
    channels: z.array(z.enum(['ALIPAY', 'WECHAT'])),
    minAmountMinor: integer,
    maxAmountMinor: integer,
  })
  .superRefine((value, context) => {
    if (!/^(0|[1-9]\d*)$/.test(value.minAmountMinor) || !/^(0|[1-9]\d*)$/.test(value.maxAmountMinor)) return;
    const lower = BigInt(value.minAmountMinor),
      upper = BigInt(value.maxAmountMinor);
    if (lower < 1n || upper < lower || upper > 9223372036854775807n || (value.enabled && value.channels.length === 0))
      context.addIssue({ code: 'custom', message: 'Invalid payment capability range' });
  });
export type PaymentCapabilities = z.infer<typeof paymentCapabilitiesSchema>;
const ORDER_FIELDS = `id projectID status channel currency amountMinor creditsMicros rechargeMultiplierPPM accountingVersion createdAt expiresAt paidAt checkoutURL reviewReason`;

export class StalePaymentContextError extends Error {
  constructor() {
    super('Payment payer or Project context changed');
    this.name = 'StalePaymentContextError';
  }
}

function usePaymentContext() {
  const projectID = useSelectedProjectId();
  const userID = useAuthStore((state) => state.auth.user?.id);
  const assertCurrent = () => {
    if (
      !userID ||
      !projectID ||
      useAuthStore.getState().auth.user?.id !== userID ||
      useProjectStore.getState().selectedProjectId !== projectID
    )
      throw new StalePaymentContextError();
  };
  const request = async <T>(query: string, variables?: Record<string, unknown>): Promise<T> => {
    assertCurrent();
    try {
      const data = await graphqlRequest<T>(query, variables, { 'X-Project-ID': projectID! });
      assertCurrent();
      return data;
    } catch (error) {
      // A response/error from the previous payer must not populate the new
      // context's UI or be reported as its payment/business failure.
      assertCurrent();
      throw error;
    }
  };
  return { projectID, userID, request };
}

export function usePaymentCapabilities() {
  const { projectID, userID, request } = usePaymentContext();
  return useQuery({
    queryKey: ['payments', userID, projectID, 'capabilities'],
    enabled: !!projectID && !!userID,
    queryFn: async () => {
      const data = await request<{ paymentCapabilities: unknown }>(
        `query PaymentCapabilities { paymentCapabilities { enabled currency channels minAmountMinor maxAmountMinor } }`,
        undefined
      );
      return paymentCapabilitiesSchema.parse(data.paymentCapabilities);
    },
    retry: false,
  });
}

export function usePaymentOrders(offset: number) {
  const { projectID, userID, request } = usePaymentContext();
  return useQuery({
    queryKey: ['payments', userID, projectID, 'orders', offset],
    enabled: !!projectID && !!userID,
    queryFn: async () => {
      const data = await request<{ paymentOrders: unknown }>(
        `query PaymentOrders($limit: Int!, $offset: Int!) { paymentOrders(limit: $limit, offset: $offset) { ${ORDER_FIELDS} } }`,
        { limit: 10, offset }
      );
      return z.array(paymentOrderSchema).parse(data.paymentOrders);
    },
    refetchOnWindowFocus: true,
    retry: false,
  });
}

export function usePaymentOrder(id?: string) {
  const { projectID, userID, request } = usePaymentContext();
  return useQuery({
    queryKey: ['payments', userID, projectID, 'order', id],
    enabled: !!projectID && !!userID && !!id,
    queryFn: async () => {
      const data = await request<{ paymentOrder: unknown }>(`query PaymentOrder($id: ID!) { paymentOrder(id: $id) { ${ORDER_FIELDS} } }`, {
        id,
      });
      return paymentOrderSchema.parse(data.paymentOrder);
    },
    refetchOnWindowFocus: true,
    retry: false,
  });
}

export function useCreatePaymentOrder() {
  const { projectID, userID, request } = usePaymentContext();
  const client = useQueryClient();
  return useMutation({
    mutationFn: async (input: { amountMinor: string; channel: PaymentChannel; idempotencyKey: string }) => {
      const data = await request<{ createPaymentOrder: unknown }>(
        `mutation CreatePaymentOrder($input: CreatePaymentOrderInput!) { createPaymentOrder(input: $input) { ${ORDER_FIELDS} } }`,
        { input }
      );
      return paymentOrderSchema.parse(data.createPaymentOrder);
    },
    retry: false,
    // The mounted caller reports business errors once; stale contexts are silent.
    onError: () => undefined,
    onSuccess: () => client.invalidateQueries({ queryKey: ['payments', userID, projectID] }),
  });
}

export function useRecheckPaymentOrder() {
  const { projectID, userID, request } = usePaymentContext();
  const client = useQueryClient();
  return useMutation({
    mutationFn: async (id: string) => {
      const data = await request<{ recheckPaymentOrder: unknown }>(
        `mutation RecheckPaymentOrder($id: ID!) { recheckPaymentOrder(id: $id) { ${ORDER_FIELDS} } }`,
        { id }
      );
      return paymentOrderSchema.parse(data.recheckPaymentOrder);
    },
    retry: false,
    // The mounted caller reports business errors once; stale contexts are silent.
    onError: () => undefined,
    onSuccess: async () => {
      await client.invalidateQueries({ queryKey: ['payments', userID, projectID] });
      await client.invalidateQueries({ queryKey: ['billing'] });
    },
  });
}
