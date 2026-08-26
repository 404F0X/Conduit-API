import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { getTokenFromStorage } from '@/stores/authStore';
import { DEFAULT_PRODUCT_MODE, type ProductMode } from './mode';

const PRODUCT_EXPERIENCE_SETTINGS_QUERY = `
  query ProductExperienceSettings {
    productExperienceSettings {
      mode
    }
  }
`;

const UPDATE_PRODUCT_EXPERIENCE_SETTINGS_MUTATION = `
  mutation UpdateProductExperienceSettings($input: UpdateProductExperienceSettingsInput!) {
    updateProductExperienceSettings(input: $input) {
      mode
    }
  }
`;

export const productExperienceQueryKey = ['productExperienceSettings'] as const;

export interface ProductExperienceSettings {
  mode: ProductMode;
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
    mutationFn: async (mode: ProductMode) => {
      const data = await graphqlRequest<{ updateProductExperienceSettings: ProductExperienceSettings }>(
        UPDATE_PRODUCT_EXPERIENCE_SETTINGS_MUTATION,
        { input: { mode } }
      );
      return data.updateProductExperienceSettings;
    },
    onSuccess: (settings) => {
      queryClient.setQueryData(productExperienceQueryKey, settings);
    },
  });
}
