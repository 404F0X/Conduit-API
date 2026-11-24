import { createContext, useContext } from 'react';
import type { ProductFeature } from './data';
import type { ProductLandingPath, ProductMode } from './mode';

export interface ProductExperienceContextValue {
  mode: ProductMode;
  features: ProductFeature[];
  isLoading: boolean;
  homePath: ProductLandingPath;
}

export const ProductExperienceContext = createContext<ProductExperienceContextValue | null>(null);

export function useProductExperience(): ProductExperienceContextValue {
  const value = useContext(ProductExperienceContext);
  if (!value) {
    throw new Error('useProductExperience must be used inside ProductExperienceProvider');
  }
  return value;
}

export type UiMode = 'simple' | 'enterprise';

/**
 * 页面里判断模式用这个：返回 'simple' 或 'enterprise'。
 * 简单模式是用企业能力加默认值封装出来的，页面通常是“同一个组件，简单模式少渲染一些字段”，
 * 只有用户侧六页（首页、令牌、模型与价格、使用日志、钱包、设置）有单独的简化视图。
 */
export function useMode(): UiMode {
  return useProductExperience().mode === 'SIMPLE' ? 'simple' : 'enterprise';
}

export function useIsSimpleMode(): boolean {
  return useMode() === 'simple';
}
