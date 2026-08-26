import { usePricingDisplayStore } from '@/stores/pricingDisplayStore';
import {
  currencyToAccountingFactor,
  DEFAULT_ACCOUNTING_CURRENCY_CODE,
  DEFAULT_CREDIT_DISPLAY_NAME,
  DEFAULT_CREDITS_PER_ACCOUNTING_UNIT,
  scaleDisplayAmount,
} from '@/lib/accounting';
import { usePermissions } from '@/hooks/usePermissions';
import { useGeneralSettings } from '@/features/system/data/system';

export function useAdminPriceDisplay(sourceCurrency?: string, forceCredits = false) {
  const { data: settings } = useGeneralSettings();
  const { hasSystemScope } = usePermissions();
  const selectedMode = usePricingDisplayStore((state) => state.mode);
  const mode = forceCredits || !hasSystemScope('read_commercialization') ? 'credits' : selectedMode;
  const accountingCurrencyCode = settings?.accountingCurrencyCode?.trim() || DEFAULT_ACCOUNTING_CURRENCY_CODE;
  const sourceToAccounting = currencyToAccountingFactor(
    {
      accountingCurrencyCode,
      exchangeRates: settings?.exchangeRates || [],
    },
    sourceCurrency?.trim() || accountingCurrencyCode
  );
  const creditsPerAccountingUnit = Number(settings?.creditsPerAccountingUnit || DEFAULT_CREDITS_PER_ACCOUNTING_UNIT);
  const factor =
    mode === 'credits'
      ? sourceToAccounting != null && Number.isFinite(creditsPerAccountingUnit)
        ? sourceToAccounting * creditsPerAccountingUnit
        : null
      : sourceToAccounting;

  return {
    mode,
    label: mode === 'credits' ? settings?.creditDisplayName?.trim() || DEFAULT_CREDIT_DISPLAY_NAME : accountingCurrencyCode,
    factor,
    amount: (value: string | number | null | undefined) => scaleDisplayAmount(value, factor),
  };
}
