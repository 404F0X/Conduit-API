import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import type { CreateModelInput } from './schema';

export type UpstreamModelDeployment = {
  id: string;
  channelID: string;
  channelName: string;
  upstreamModelID: string;
  internalName: string;
  variant: string;
  status: 'ENABLED' | 'DISABLED' | 'ARCHIVED';
  source: string;
};

export type ModelRoute = {
  id: string;
  publicModelID: string;
  publicModelKey: string;
  deploymentID: string;
  deploymentName: string;
  channelID: string;
  channelName: string;
  upstreamModelID: string;
  status: 'ENABLED' | 'DISABLED' | 'ARCHIVED';
};

export type PriceBookItem = {
  id: string;
  publicModelID: string;
  publicModelKey: string;
  price: { items: Array<{ itemCode: string; pricing: { mode: string; usagePerUnit?: string; flatFee?: string } }> };
};

export type PriceBookVersion = {
  id: string;
  version: number;
  status: string;
  referenceID: string;
  effectiveStartAt?: string | null;
  items: PriceBookItem[];
};

export type PriceBook = {
  id: string;
  name: string;
  currency: string;
  status: 'ENABLED' | 'DISABLED' | 'ARCHIVED';
  isDefault: boolean;
  versions: PriceBookVersion[];
};

const QUERY = `
  query CommercializationCatalog {
    upstreamModelDeployments {
      id channelID channelName upstreamModelID internalName variant status source
    }
    modelRoutes {
      id publicModelID publicModelKey deploymentID deploymentName
      channelID channelName upstreamModelID status
    }
    priceBooks {
      id name currency status isDefault
      versions {
        id version status referenceID effectiveStartAt
        items { id publicModelID publicModelKey price }
      }
    }
  }
`;

const SUPPLY_QUERY = `
  query UpstreamSupplyCatalog {
    upstreamModelDeployments {
      id channelID channelName upstreamModelID internalName variant status source
    }
    modelRoutes {
      id publicModelID publicModelKey deploymentID deploymentName
      channelID channelName upstreamModelID status
    }
  }
`;

const UPSERT_ROUTE = `
  mutation UpsertModelRoute($input: UpsertModelRouteInput!) {
    upsertModelRoute(input: $input) { id }
  }
`;

const CREATE_PUBLIC_MODEL_WITH_ROUTES = `
  mutation CreatePublicModelWithRoutes($input: CreatePublicModelWithRoutesInput!) {
    createPublicModelWithRoutes(input: $input) {
      model {
        id modelID status
      }
      routes {
        id publicModelID publicModelKey deploymentID deploymentName
        channelID channelName upstreamModelID status
      }
    }
  }
`;

const CREATE_BOOK = `
  mutation CreatePriceBook($input: CreatePriceBookInput!) {
    createPriceBook(input: $input) { id }
  }
`;

export function useCommercializationCatalog(enabled = true) {
  return useQuery({
    queryKey: ['commercialization-catalog'],
    queryFn: () =>
      graphqlRequest<{
        upstreamModelDeployments: UpstreamModelDeployment[];
        modelRoutes: ModelRoute[];
        priceBooks: PriceBook[];
      }>(QUERY),
    enabled,
  });
}

export function useUpstreamSupplyCatalog(enabled = true) {
  return useQuery({
    queryKey: ['upstream-supply-catalog'],
    queryFn: () =>
      graphqlRequest<{
        upstreamModelDeployments: UpstreamModelDeployment[];
        modelRoutes: ModelRoute[];
      }>(SUPPLY_QUERY),
    enabled,
  });
}

function useCommercialMutation<T>(mutationFn: (input: T) => Promise<unknown>, invalidatesAccountingCurrency = false) {
  const client = useQueryClient();
  return useMutation({
    mutationFn,
    onSuccess: () =>
      Promise.all([
        client.invalidateQueries({ queryKey: ['commercialization-catalog'] }),
        client.invalidateQueries({ queryKey: ['upstream-supply-catalog'] }),
        ...(invalidatesAccountingCurrency ? [client.invalidateQueries({ queryKey: ['generalSettings'] })] : []),
      ]),
  });
}

export function useUpsertModelRoute() {
  return useCommercialMutation(
    (input: { id?: string; publicModelID: string; deploymentID: string; status: 'ENABLED' | 'DISABLED'; confirmCompatibility?: boolean }) =>
      graphqlRequest(UPSERT_ROUTE, { input })
  );
}

export type CreatePublicModelWithRoutesInput = {
  model: CreateModelInput;
  deploymentIDs: string[];
  enabled: boolean;
  confirmCompatibility?: boolean;
};

export function useCreatePublicModelWithRoutes() {
  const client = useQueryClient();
  return useMutation({
    mutationFn: (input: CreatePublicModelWithRoutesInput) =>
      graphqlRequest<{
        createPublicModelWithRoutes: {
          model: { id: string; modelID: string; status: string };
          routes: ModelRoute[];
        };
      }>(CREATE_PUBLIC_MODEL_WITH_ROUTES, { input }).then((data) => data.createPublicModelWithRoutes),
    onSuccess: async () => {
      await Promise.all([
        client.invalidateQueries({ queryKey: ['models'] }),
        client.invalidateQueries({ queryKey: ['commercialization-catalog'] }),
        client.invalidateQueries({ queryKey: ['upstream-supply-catalog'] }),
      ]);
    },
  });
}

export function useCreatePriceBook() {
  return useCommercialMutation(
    (input: { name: string; currency: string; isDefault: boolean }) =>
      graphqlRequest<{ createPriceBook: { id: string } }>(CREATE_BOOK, { input }),
    true
  );
}
