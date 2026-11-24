import { useCallback } from 'react';
import { useTranslation } from 'react-i18next';
import { useAdminPriceDisplay } from '@/hooks/use-admin-price-display';
import { formatMoney } from './format';

/**
 * 简单模式只有一种金额单位：站点展示单位（站长在系统设置里起的名字，默认“积分”）。
 * 请求里的 totalCost 是记账币种，要先换算；钱包余额本身就是展示单位，直接用 label 拼。
 */
export function useMoney() {
  const { t } = useTranslation();
  const display = useAdminPriceDisplay(undefined, true);
  const { label, amount } = display;
  /** 记账币种的金额（如请求消费） */
  const cost = useCallback((value: string | number | null | undefined) => formatMoney(label, amount(value), t), [label, amount, t]);
  /** 已经是展示单位的金额（如钱包余额、令牌额度） */
  const credits = useCallback((value: string | number | null | undefined) => formatMoney(label, value, t), [label, t]);
  return { label, cost, credits };
}
