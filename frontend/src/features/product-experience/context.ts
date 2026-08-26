import { createContext, useContext } from 'react';
import type { ProductMode } from './mode';

export interface ProductExperienceContextValue {
  mode: ProductMode;
  isLoading: boolean;
  homePath: '/' | '/project/dashboard';
}

export const ProductExperienceContext = createContext<ProductExperienceContextValue | null>(null);

export function useProductExperience(): ProductExperienceContextValue {
  const value = useContext(ProductExperienceContext);
  if (!value) {
    throw new Error('useProductExperience must be used inside ProductExperienceProvider');
  }
  return value;
}
