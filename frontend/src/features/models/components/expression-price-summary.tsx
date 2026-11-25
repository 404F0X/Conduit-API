import { useTranslation } from 'react-i18next';
import type { NewApiExpressionPrice } from './simple-retail-price';

export function ExpressionPriceSummary({ price }: { price: NewApiExpressionPrice }) {
  const { t } = useTranslation();
  return (
    <div className='min-w-0 space-y-1 text-xs'>
      <p className='font-medium'>{t('models.expressionPrice.expression')}</p>
      <p className='text-muted-foreground'>{t('models.expressionPrice.profile', price)}</p>
      <pre className='text-muted-foreground font-mono break-all whitespace-pre-wrap'>{price.expression}</pre>
      <p className='text-muted-foreground'>{t('models.expressionPrice.multiplierDisplay', price)}</p>
    </div>
  );
}
