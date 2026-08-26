import { z } from 'zod';
import { useQuery } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { channelModelPriceSchema, type ChannelModelPrice } from '@/features/channels/data/schema';

const CHANNEL_CATALOG_PRICES_QUERY = `
  query ChannelCatalogPrices($input: QueryChannelInput!) {
    queryChannels(input: $input) {
      edges {
        node {
          id
          channelModelPrices {
            id
            modelID
            currencyCode
            price {
              items {
                itemCode
                pricing {
                  mode
                  flatFee
                  usagePerUnit
                  usageTiered {
                    tiers {
                      upTo
                      pricePerUnit
                    }
                  }
                }
                promptWriteCacheVariants {
                  variantCode
                  pricing {
                    mode
                    flatFee
                    usagePerUnit
                    usageTiered {
                      tiers {
                        upTo
                        pricePerUnit
                      }
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  }
`;

const responseSchema = z.object({
  queryChannels: z.object({
    edges: z.array(
      z.object({
        node: z.object({
          id: z.string(),
          channelModelPrices: z.array(channelModelPriceSchema).optional().default([]),
        }),
      })
    ),
  }),
});

export function useChannelCatalogPrices(enabled = true) {
  return useQuery({
    queryKey: ['model-catalog', 'channel-prices'],
    queryFn: async () => {
      const response = await graphqlRequest<unknown>(CHANNEL_CATALOG_PRICES_QUERY, {
        input: { first: 10000 },
      });
      const parsed = responseSchema.parse(response);
      return new Map<string, ChannelModelPrice[]>(parsed.queryChannels.edges.map(({ node }) => [node.id, node.channelModelPrices]));
    },
    enabled,
    staleTime: 60_000,
    retry: 1,
  });
}
