import { z } from 'zod';
import { useQuery } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';

const moneyMetricSchema = z.object({
  amount: z.number().nullable(),
  quality: z.enum(['EXACT', 'PARTIAL', 'UNAVAILABLE']),
  coverageRate: z.number().nullable(),
  reason: z.string().nullable(),
});

const errorBucketSchema = z.object({
  category: z.string(),
  count: z.number(),
});

const summarySchema = z.object({
  customerRequests: z.number(),
  upstreamAttempts: z.number(),
  successfulAttempts: z.number(),
  failedAttempts: z.number(),
  successRate: z.number().nullable(),
  retryCount: z.number(),
  averageTtftMs: z.number().nullable(),
  ttftSampleCount: z.number(),
  averageTps: z.number().nullable(),
  tpsSampleCount: z.number(),
  errorBreakdown: z.array(errorBucketSchema),
  inputTokens: z.number(),
  outputTokens: z.number(),
  cachedTokens: z.number(),
  totalTokens: z.number(),
  recordedUpstreamCost: moneyMetricSchema,
  recognizedUsageRevenue: moneyMetricSchema,
  grossProfit: moneyMetricSchema,
  grossMargin: z.number().nullable(),
});

const coverageSchema = z.object({
  usageRows: z.number(),
  costedUsageRows: z.number(),
  settledUsageRows: z.number(),
  pendingChargeRows: z.number(),
  costCoverageRate: z.number().nullable(),
  billingCoverageRate: z.number().nullable(),
  costComplete: z.boolean(),
  billingComplete: z.boolean(),
});

const trendSchema = z.object({
  date: z.string(),
  customerRequests: z.number(),
  failedCustomerRequests: z.number(),
  requestFailureRate: z.number().nullable(),
  upstreamAttempts: z.number(),
  successfulAttempts: z.number(),
  failedAttempts: z.number(),
  failureRate: z.number().nullable(),
  retryCount: z.number(),
  averageTtftMs: z.number().nullable(),
  averageTps: z.number().nullable(),
  recordedUpstreamCost: z.number().nullable(),
  recognizedUsageRevenue: z.number().nullable(),
  grossProfit: z.number().nullable(),
});

const channelSchema = z.object({
  channelId: z.number(),
  channelName: z.string(),
  channelType: z.string(),
  channelStatus: z.string(),
  customerRequests: z.number(),
  upstreamAttempts: z.number(),
  successfulAttempts: z.number(),
  failedAttempts: z.number(),
  successRate: z.number().nullable(),
  retryCount: z.number(),
  inputTokens: z.number(),
  outputTokens: z.number(),
  cachedTokens: z.number(),
  totalTokens: z.number(),
  averageLatencyMs: z.number().nullable(),
  averageTtftMs: z.number().nullable(),
  ttftSampleCount: z.number(),
  averageTps: z.number().nullable(),
  tpsSampleCount: z.number(),
  errorBreakdown: z.array(errorBucketSchema),
  recordedUpstreamCost: moneyMetricSchema,
  recognizedUsageRevenue: moneyMetricSchema,
  grossProfit: moneyMetricSchema,
  grossMargin: z.number().nullable(),
  costPerAttempt: z.number().nullable(),
  usageRows: z.number(),
  costedUsageRows: z.number(),
  settledUsageRows: z.number(),
  pendingChargeRows: z.number(),
  costCoverageRate: z.number().nullable(),
  billingCoverageRate: z.number().nullable(),
  quotaCurrency: z.string().nullable(),
  quotaRemaining: z.string().nullable(),
  actualQuotaUsed: z.string().nullable(),
  quotaSnapshotAt: z.string().nullable(),
  observedPricingSource: z.string().nullable(),
  observedPricingAt: z.string().nullable(),
  observedPriceChangeCount: z.number(),
  lastProbeAt: z.string().nullable(),
  lastActivityAt: z.string().nullable(),
});

const routeHealthSchema = z.object({
  channelId: z.number(),
  channelName: z.string(),
  actualModel: z.string(),
  credentialIdentity: z.string().nullable(),
  healthStatus: z.enum(['healthy', 'degraded', 'unhealthy', 'unknown']),
  upstreamAttempts: z.number(),
  successfulAttempts: z.number(),
  failedAttempts: z.number(),
  successRate: z.number().nullable(),
  errorBreakdown: z.array(errorBucketSchema),
  lastActivityAt: z.string().nullable(),
});

const riskSchema = z.object({
  code: z.string(),
  severity: z.string(),
  channelId: z.number().nullable(),
  channelName: z.string().nullable(),
  affectedCount: z.number().nullable(),
  totalCount: z.number().nullable(),
  observedValue: z.number().nullable(),
  thresholdValue: z.number().nullable(),
  periodDays: z.number().nullable(),
});

export const operationsLedgerSchema = z.object({
  generatedAt: z.string(),
  periodStart: z.string(),
  periodEnd: z.string(),
  periodDays: z.number(),
  summary: summarySchema,
  coverage: coverageSchema,
  trend: z.array(trendSchema),
  channels: z.array(channelSchema),
  routeHealth: z.array(routeHealthSchema),
  risks: z.array(riskSchema),
  accountingScopeNote: z.string(),
});

export type OperationsLedger = z.infer<typeof operationsLedgerSchema>;
export type OperationsMoneyMetric = z.infer<typeof moneyMetricSchema>;
export type OperationsChannel = z.infer<typeof channelSchema>;
export type OperationsRouteHealth = z.infer<typeof routeHealthSchema>;

const operationsFlowRowSchema = z.object({
  userId: z.number().nullable(),
  userEmail: z.string(),
  projectId: z.number().nullable(),
  projectName: z.string(),
  apiKeyId: z.number().nullable(),
  apiKeyName: z.string(),
  requestedModel: z.string(),
  actualModel: z.string(),
  channelId: z.number().nullable(),
  channelName: z.string(),
  meteredRequests: z.number(),
  totalTokens: z.number(),
  recordedUpstreamCost: z.number().nullable(),
  recognizedUsageRevenue: z.number().nullable(),
  settledRequests: z.number(),
  lastActivityAt: z.string().nullable(),
});

export const operationsFlowSchema = z.object({
  generatedAt: z.string(),
  periodStart: z.string(),
  periodEnd: z.string(),
  periodDays: z.number(),
  usageRows: z.number(),
  settledUsageRows: z.number(),
  attributionNote: z.string(),
  rows: z.array(operationsFlowRowSchema),
});

export type OperationsFlow = z.infer<typeof operationsFlowSchema>;
export type OperationsFlowRow = z.infer<typeof operationsFlowRowSchema>;

const operationsModelSeriesPointSchema = z.object({
  bucketStart: z.string(),
  requestedModel: z.string(),
  meteredRequests: z.number(),
  totalTokens: z.number(),
  recordedUpstreamCost: z.number().nullable(),
  recognizedUsageRevenue: z.number().nullable(),
});

export const operationsModelSeriesSchema = z.object({
  generatedAt: z.string(),
  periodStart: z.string(),
  periodEnd: z.string(),
  periodDays: z.number(),
  granularity: z.enum(['hour', 'day', 'week']),
  points: z.array(operationsModelSeriesPointSchema),
});

export type OperationsModelSeries = z.infer<typeof operationsModelSeriesSchema>;

const QUERY = `
  query OperationsLedger($periodDays: Int) {
    operationsLedger(periodDays: $periodDays) {
      generatedAt periodStart periodEnd periodDays accountingScopeNote
      summary {
        customerRequests upstreamAttempts successfulAttempts failedAttempts successRate
        retryCount averageTtftMs ttftSampleCount averageTps tpsSampleCount
        errorBreakdown { category count }
        inputTokens outputTokens cachedTokens totalTokens grossMargin
        recordedUpstreamCost { amount quality coverageRate reason }
        recognizedUsageRevenue { amount quality coverageRate reason }
        grossProfit { amount quality coverageRate reason }
      }
      coverage {
        usageRows costedUsageRows settledUsageRows pendingChargeRows
        costCoverageRate billingCoverageRate costComplete billingComplete
      }
      trend {
        date customerRequests failedCustomerRequests requestFailureRate
        upstreamAttempts successfulAttempts failedAttempts failureRate recordedUpstreamCost
        retryCount averageTtftMs averageTps recognizedUsageRevenue grossProfit
      }
      channels {
        channelId channelName channelType channelStatus
        customerRequests upstreamAttempts successfulAttempts failedAttempts successRate
        retryCount inputTokens outputTokens cachedTokens totalTokens averageLatencyMs averageTtftMs
        ttftSampleCount averageTps tpsSampleCount errorBreakdown { category count }
        recordedUpstreamCost { amount quality coverageRate reason }
        recognizedUsageRevenue { amount quality coverageRate reason }
        grossProfit { amount quality coverageRate reason }
        grossMargin costPerAttempt usageRows costedUsageRows settledUsageRows pendingChargeRows
        costCoverageRate billingCoverageRate quotaCurrency quotaRemaining actualQuotaUsed
        quotaSnapshotAt observedPricingSource observedPricingAt observedPriceChangeCount lastProbeAt lastActivityAt
      }
      routeHealth {
        channelId channelName actualModel credentialIdentity healthStatus
        upstreamAttempts successfulAttempts failedAttempts successRate lastActivityAt
        errorBreakdown { category count }
      }
      risks { code severity channelId channelName affectedCount totalCount observedValue thresholdValue periodDays }
    }
  }
`;

const FLOW_QUERY = `
  query OperationsFlow($periodDays: Int) {
    operationsFlow(periodDays: $periodDays, limit: 250) {
      generatedAt periodStart periodEnd periodDays usageRows settledUsageRows attributionNote
      rows {
        userId userEmail projectId projectName apiKeyId apiKeyName requestedModel actualModel
        channelId channelName meteredRequests totalTokens recordedUpstreamCost
        recognizedUsageRevenue settledRequests lastActivityAt
      }
    }
  }
`;

const MODEL_SERIES_QUERY = `
  query OperationsModelSeries($periodDays: Int) {
    operationsModelSeries(periodDays: $periodDays) {
      generatedAt periodStart periodEnd periodDays granularity
      points {
        bucketStart requestedModel meteredRequests totalTokens
        recordedUpstreamCost recognizedUsageRevenue
      }
    }
  }
`;

export function useOperationsLedger(periodDays: 1 | 7 | 14 | 29 | 30, enabled = true) {
  return useQuery({
    queryKey: ['operations-ledger', periodDays],
    queryFn: async () => {
      const response = await graphqlRequest<{ operationsLedger: unknown }>(QUERY, { periodDays });
      return operationsLedgerSchema.parse(response.operationsLedger);
    },
    enabled,
  });
}

export function useOperationsFlow(periodDays: 1 | 7 | 14 | 29 | 30) {
  return useQuery({
    queryKey: ['operations-flow', periodDays],
    queryFn: async () => {
      const response = await graphqlRequest<{ operationsFlow: unknown }>(FLOW_QUERY, { periodDays });
      return operationsFlowSchema.parse(response.operationsFlow);
    },
  });
}

export function useOperationsModelSeries(periodDays: 1 | 7 | 14 | 29) {
  return useQuery({
    queryKey: ['operations-model-series', periodDays],
    queryFn: async () => {
      const response = await graphqlRequest<{ operationsModelSeries: unknown }>(MODEL_SERIES_QUERY, { periodDays });
      return operationsModelSeriesSchema.parse(response.operationsModelSeries);
    },
  });
}
