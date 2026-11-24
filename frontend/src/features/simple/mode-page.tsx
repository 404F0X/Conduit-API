import type React from 'react';
import { useIsSimpleMode, useProductExperience } from '@/features/product-experience';

/**
 * 同一个地址，按站点模式渲染不同页面：简单模式是精简页，完整模式是原有的完整页。
 * 两边读写的是同一套接口，简单模式只是把高级选项收起来、用默认值代替。
 */
export function ModePage({ simple, enterprise }: { simple: React.ReactNode; enterprise: React.ReactNode }) {
  const { isLoading } = useProductExperience();
  const isSimple = useIsSimpleMode();
  if (isLoading) return null;
  return <>{isSimple ? simple : enterprise}</>;
}
