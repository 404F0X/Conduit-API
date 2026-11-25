import { z } from 'zod';
import { useQuery } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useSelectedProjectId } from '@/stores/projectStore';
import { isNewApiExpressionPrice, type NewApiExpressionPrice } from '@/features/models/components/simple-retail-price';

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
const pricingSchema = z.object({
  mode: z.enum(['flat_fee', 'usage_per_unit', 'usage_tiered', 'usage_volume']),
  amount: z.string().nullable(),
  tiers: z.array(z.object({ above: z.string(), upTo: z.string().nullable(), pricePerMillion: z.string() })),
});
const priceItemSchema = z.object({
  itemCode: z.string(),
  pricing: pricingSchema,
  promptWriteCacheVariants: z.array(z.object({ variantCode: z.string(), pricing: pricingSchema })),
});
const expressionSchema = z.custom<NewApiExpressionPrice>(isNewApiExpressionPrice).nullish();
const priceLeafSchema = z
  .object({
    items: z.array(priceItemSchema).nullable(),
    billingExpression: expressionSchema,
  })
  .refine((leaf) => !leaf.billingExpression || leaf.items === null, 'Expression price has incompatible static items');
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
  price: z
    .object({
      ...priceLeafSchema.shape,
      status: z.enum(['PRICED', 'UNPRICED', 'UNAVAILABLE']),
      branches: z.array(priceLeafSchema.safeExtend({ source: z.string(), label: z.string(), condition: z.string().nullable() })),
      source: z.string(),
      priceBookVersion: z.string().nullable(),
      currency: z.string(),
      displayName: z.string(),
      inputPerMillion: z.string().nullable(),
      outputPerMillion: z.string().nullable(),
      cacheReadPerMillion: z.string().nullable(),
      cacheWritePerMillion: z.string().nullable(),
      effectiveMultiplier: z.string(),
      billable: z.boolean(),
    })
    .refine((leaf) => !leaf.billingExpression || leaf.items === null, 'Expression price has incompatible static items'),
  routes: z.array(routeSchema),
  health: healthSchema.nullable(),
});

const responseSchema = z.object({
  myModelCatalog: z.object({
    models: z.array(modelSchema),
    healthVisible: z.boolean(),
    priceScopes: z.array(
      z.object({
        id: z.string(),
        label: z.string(),
        modelGroupID: z.string(),
        modelGroupRevision: z.number(),
        apiKeyID: z.string().nullable(),
      })
    ),
    selectedPriceScope: z.string().nullable(),
  }),
});
export type CatalogModel = z.infer<typeof modelSchema>;
export type CatalogRoute = z.infer<typeof routeSchema>;

const QUERY = `
  query MyModelCatalog($modelGroupID: ID, $apiKeyID: ID) {
    myModelCatalog(modelGroupID: $modelGroupID, apiKeyID: $apiKeyID) {
      healthVisible selectedPriceScope
      priceScopes { id label modelGroupID modelGroupRevision apiKeyID }
      models {
        id modelId name group developer modelType capabilities contextLimit outputLimit
        price {
          status source priceBookVersion billingExpression
          branches { source label condition billingExpression items { itemCode pricing { mode amount tiers { above upTo pricePerMillion } } promptWriteCacheVariants { variantCode pricing { mode amount tiers { above upTo pricePerMillion } } } } }
          items {
            itemCode
            pricing { mode amount tiers { above upTo pricePerMillion } }
            promptWriteCacheVariants { variantCode pricing { mode amount tiers { above upTo pricePerMillion } } }
          }
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

export function useMyModelCatalog(scope?: { modelGroupID?: string; apiKeyID?: string }) {
  const selectedProjectId = useSelectedProjectId();
  return useQuery({
    queryKey: ['myModelCatalog', selectedProjectId, scope],
    queryFn: async () =>
      responseSchema.parse(
        await graphqlRequest<unknown>(QUERY, scope, {
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
