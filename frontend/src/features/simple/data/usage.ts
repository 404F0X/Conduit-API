import { useQueries, useQuery } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useTranslation } from 'react-i18next';
import { useSelectedProjectId } from '@/stores/projectStore';
import { useErrorHandler } from '@/hooks/use-error-handler';
import { useRequestPermissions } from '@/hooks/useRequestPermissions';

/**
 * 简单模式的使用日志查询。和完整模式的“请求日志”读的是同一个 requests 接口，
 * 只是字段少很多（不取执行记录、客户端 IP 等），表格固定 6 列。
 */

export type SimpleRequestStatus = 'pending' | 'processing' | 'completed' | 'failed' | 'canceled';

export interface SimpleCostItem {
  itemCode: string;
  quantity: number;
  subtotal: string | number;
}

export interface SimpleRequest {
  id: string;
  createdAt: string;
  modelID: string;
  status: SimpleRequestStatus;
  stream: boolean;
  metricsLatencyMs?: number | null;
  metricsFirstTokenLatencyMs?: number | null;
  apiKey?: { id: string; name: string } | null;
  usageLogs?: {
    edges: Array<{
      node: {
        promptTokens: number;
        completionTokens: number;
        totalTokens: number;
        promptCachedTokens?: number | null;
        promptWriteCachedTokens?: number | null;
        totalCost?: string | number | null;
        costItems?: SimpleCostItem[] | null;
      };
    }>;
  } | null;
}

interface SimpleRequestConnection {
  edges: Array<{ node: SimpleRequest }>;
  pageInfo: { hasNextPage: boolean; endCursor?: string | null };
  totalCount: number;
}

export interface SimpleRequestFilter {
  from?: Date | null;
  to?: Date | null;
  apiKeyId?: string | null;
  modelId?: string | null;
  failedOnly?: boolean;
}

function buildQuery(withApiKey: boolean) {
  return `
    query SimpleRequests($first: Int, $after: Cursor, $orderBy: RequestOrder, $where: RequestWhereInput) {
      requests(first: $first, after: $after, orderBy: $orderBy, where: $where) {
        edges {
          node {
            id
            createdAt
            modelID
            status
            stream
            metricsLatencyMs
            metricsFirstTokenLatencyMs${withApiKey ? '\n            apiKey { id name }' : ''}
            usageLogs(first: 1) {
              edges {
                node {
                  promptTokens
                  completionTokens
                  totalTokens
                  promptCachedTokens
                  promptWriteCachedTokens
                  totalCost
                  costItems { itemCode quantity subtotal }
                }
              }
            }
          }
        }
        pageInfo { hasNextPage endCursor }
        totalCount
      }
    }
  `;
}

const COUNT_QUERY = `
  query SimpleRequestCount($where: RequestWhereInput) {
    requests(first: 1, where: $where) { totalCount }
  }
`;

export function buildRequestWhere(filter: SimpleRequestFilter, projectId: string | null | undefined) {
  return {
    ...(projectId ? { projectID: projectId } : {}),
    sourceIn: ['api'],
    ...(filter.from ? { createdAtGTE: filter.from.toISOString() } : {}),
    ...(filter.to ? { createdAtLTE: filter.to.toISOString() } : {}),
    ...(filter.apiKeyId ? { apiKeyIDIn: [filter.apiKeyId] } : {}),
    ...(filter.modelId ? { modelID: filter.modelId } : {}),
    ...(filter.failedOnly ? { statusIn: ['failed'] } : {}),
  };
}

export function useSimpleRequests(
  filter: SimpleRequestFilter,
  page: { first: number; after?: string },
  options?: { refetchInterval?: number | false }
) {
  const { t } = useTranslation();
  const { handleError } = useErrorHandler();
  const projectId = useSelectedProjectId();
  const permissions = useRequestPermissions();

  return useQuery({
    queryKey: ['requests', 'simple', filter, page, projectId, permissions.canViewApiKeys],
    queryFn: async () => {
      try {
        const data = await graphqlRequest<{ requests: SimpleRequestConnection }>(
          buildQuery(permissions.canViewApiKeys),
          {
            first: page.first,
            after: page.after,
            orderBy: { field: 'CREATED_AT', direction: 'DESC' },
            where: buildRequestWhere(filter, projectId),
          },
          projectId ? { 'X-Project-ID': projectId } : undefined
        );
        return data.requests;
      } catch (error) {
        handleError(error, t('common.errors.internalServerError'));
        throw error;
      }
    },
    enabled: !!projectId,
    refetchInterval: options?.refetchInterval,
  });
}

/** 只取总数（首页每日请求数、失败次数用），不取明细。 */
export function useSimpleRequestCounts(filters: SimpleRequestFilter[]) {
  const projectId = useSelectedProjectId();
  return useQueries({
    queries: filters.map((filter) => ({
      queryKey: ['requests', 'simple-count', filter, projectId],
      queryFn: async () => {
        const data = await graphqlRequest<{ requests: { totalCount: number } }>(
          COUNT_QUERY,
          { where: buildRequestWhere(filter, projectId) },
          projectId ? { 'X-Project-ID': projectId } : undefined
        );
        return data.requests.totalCount;
      },
      enabled: !!projectId,
      staleTime: 30_000,
    })),
  });
}

export function usageOf(request: SimpleRequest) {
  return request.usageLogs?.edges?.[0]?.node ?? null;
}
