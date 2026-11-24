import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { getTokenFromStorage } from '@/stores/authStore';
import { DEFAULT_PRODUCT_MODE, type ProductMode } from './mode';

const PRODUCT_EXPERIENCE_SETTINGS_QUERY = `
  query ProductExperienceSettings {
    productExperienceSettings {
      mode
      revision
      features { id visibility execution visible enabled dependencies }
    }
  }
`;

const UPDATE_PRODUCT_EXPERIENCE_SETTINGS_MUTATION = `
  mutation UpdateProductExperienceSettings($input: UpdateProductExperienceSettingsInput!) {
    updateProductExperienceSettings(input: $input) {
      mode
      revision
      features { id visibility execution visible enabled dependencies }
    }
  }
`;

export const productExperienceQueryKey = ['productExperienceSettings'] as const;

export interface ProductExperienceSettings {
  mode: ProductMode;
  revision: string;
  features: ProductFeature[];
}
export type FeatureOverride = 'INHERIT' | 'ON' | 'OFF';
export interface ProductFeature {
  id: string;
  visibility: FeatureOverride;
  execution: FeatureOverride;
  visible: boolean;
  enabled: boolean;
  dependencies: string[];
}
export interface UpdateProductExperienceInput {
  mode: ProductMode;
  expectedRevision?: string;
  overrides?: { id: string; visibility?: FeatureOverride; execution?: FeatureOverride }[];
}

export async function fetchProductExperienceSettings(): Promise<ProductExperienceSettings> {
  const data = await graphqlRequest<{ productExperienceSettings: ProductExperienceSettings }>(PRODUCT_EXPERIENCE_SETTINGS_QUERY);
  return data.productExperienceSettings;
}

export async function fetchProductModeOrDefault(): Promise<ProductMode> {
  try {
    return (await fetchProductExperienceSettings()).mode;
  } catch {
    return DEFAULT_PRODUCT_MODE;
  }
}

export function useProductExperienceSettings() {
  return useQuery({
    queryKey: productExperienceQueryKey,
    queryFn: fetchProductExperienceSettings,
    enabled: !!getTokenFromStorage(),
    staleTime: 60_000,
    retry: 1,
  });
}

export function useUpdateProductExperienceSettings() {
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (input: ProductMode | UpdateProductExperienceInput) => {
      const data = await graphqlRequest<{ updateProductExperienceSettings: ProductExperienceSettings }>(
        UPDATE_PRODUCT_EXPERIENCE_SETTINGS_MUTATION,
        {
          input:
            typeof input === 'string'
              ? { mode: input, expectedRevision: queryClient.getQueryData<ProductExperienceSettings>(productExperienceQueryKey)?.revision }
              : input,
        }
      );
      return data.updateProductExperienceSettings;
    },
    onSuccess: (settings) => {
      queryClient.setQueryData(productExperienceQueryKey, settings);
    },
  });
}
