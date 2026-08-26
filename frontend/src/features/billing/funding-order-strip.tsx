import { useTranslation } from 'react-i18next';
import { Badge } from '@/components/ui/badge';

export function FundingOrderStrip({ creditDisplayName }: { creditDisplayName: string }) {
  const { t } = useTranslation();
  const steps = [
    { label: t('billing.fundingOrder.dedicated'), variant: 'secondary' as const },
    { label: t('billing.fundingOrder.general'), variant: 'outline' as const },
    { label: t('billing.summary.stationCredit', { name: creditDisplayName }), variant: 'outline' as const },
  ];

  return (
    <aside className='bg-muted/30 flex flex-col gap-2 rounded-md border border-dashed px-3 py-2.5 sm:flex-row sm:items-center'>
      <span className='text-muted-foreground shrink-0 text-[11px] font-medium tracking-wide uppercase'>
        {t('billing.fundingOrder.label')}
      </span>
      <ol className='flex min-w-0 flex-wrap items-center gap-1.5' aria-label={t('billing.fundingOrder.description')}>
        {steps.map((step, index) => (
          <li key={step.label} className='flex items-center gap-1.5'>
            <Badge variant={step.variant} className='max-w-48 truncate font-normal' title={step.label}>
              <span className='mr-1 font-mono text-[10px] tabular-nums'>{index + 1}</span>
              {step.label}
            </Badge>
            {index < steps.length - 1 && (
              <span className='text-muted-foreground' aria-hidden='true'>
                →
              </span>
            )}
          </li>
        ))}
      </ol>
      <p className='text-muted-foreground text-xs text-pretty sm:ml-auto sm:text-right'>{t('billing.fundingOrder.description')}</p>
    </aside>
  );
}
