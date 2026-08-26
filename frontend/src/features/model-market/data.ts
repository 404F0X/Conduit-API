import { z } from 'zod';
import { useQuery } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useSelectedProjectId } from '@/stores/projectStore';

const healthSchema = z.object({
  status: z.string(),
  successRate: z.number().nullable(),
  avgTimeToFirstTokenMs: z.number().nullable(),
  avgTokensPerSecond: z.number().nullable(),
  lastUpdatedAt: z.string().nullable(),
});

const routeSchema = z.object({
  id: z.string(),
  channelID: z.string(),
  channelName: z.string(),
  label: z.string(),
  routeType: z.string(),
  health: healthSchema.nullable(),
});
const modelSchema = z.object({
  id: z.string(),
  modelId: z.string(),
  name: z.string(),
  group: z.string(),
  developer: z.string(),
  modelType: z.string(),
  capabilities: z.array(z.string()),
  contextLimit: z.number().nullable(),
  outputLimit: z.number().nullable(),
  price: z.object({
    currency: z.string(),
    displayName: z.string(),
    inputPerMillion: z.string().nullable(),
    outputPerMillion: z.string().nullable(),
    cacheReadPerMillion: z.string().nullable(),
    cacheWritePerMillion: z.string().nullable(),
    effectiveMultiplier: z.string(),
    billable: z.boolean(),
  }),
  routes: z.array(routeSchema),
  health: healthSchema.nullable(),
});

const responseSchema = z.object({ myModelCatalog: z.object({ models: z.array(modelSchema), healthVisible: z.boolean() }) });
export type CatalogModel = z.infer<typeof modelSchema>;
export type CatalogRoute = z.infer<typeof routeSchema>;

const QUERY = `
  query MyModelCatalog {
    myModelCatalog {
      healthVisible
      models {
        id modelId name group developer modelType capabilities contextLimit outputLimit
        price {
          currency displayName inputPerMillion outputPerMillion
          cacheReadPerMillion cacheWritePerMillion effectiveMultiplier billable
        }
        health { status successRate avgTimeToFirstTokenMs avgTokensPerSecond lastUpdatedAt }
        routes {
          id channelID channelName label routeType
          health { status successRate avgTimeToFirstTokenMs avgTokensPerSecond lastUpdatedAt }
        }
      }
    }
  }
`;

export function useMyModelCatalog() {
  const selectedProjectId = useSelectedProjectId();
  return useQuery({
    queryKey: ['myModelCatalog', selectedProjectId],
    queryFn: async () =>
      responseSchema.parse(
        await graphqlRequest<unknown>(QUERY, undefined, {
          'X-Project-ID': selectedProjectId!,
        })
      ).myModelCatalog,
    enabled: !!selectedProjectId,
    staleTime: 0,
    refetchOnWindowFocus: 'always',
    refetchInterval: 15_000,
    retry: 1,
  });
}
