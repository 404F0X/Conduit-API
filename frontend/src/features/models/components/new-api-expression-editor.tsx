import { useState } from 'react';
import { useMutation } from '@tanstack/react-query';
import { graphqlRequest } from '@/gql/graphql';
import { useTranslation } from 'react-i18next';
import { usePermissions } from '@/hooks/usePermissions';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Textarea } from '@/components/ui/textarea';
import { validNewApiExpressionDraft, type NewApiExpressionPrice } from './simple-retail-price';

const PREVIEW = `
  query PreviewNewApiBillingExpression($input: NewApiBillingExpressionPreviewInput!) {
    previewNewApiBillingExpression(input: $input) { amountUSD }
  }
`;

const initialTokens = {
  promptTokens: '0',
  completionTokens: '0',
  cacheReadTokens: '0',
  cacheWriteTokens: '0',
  cacheWrite1hTokens: '0',
};
type TokenInput = typeof initialTokens;

function ExpressionPreview({ pricing }: { pricing: NewApiExpressionPrice }) {
  const { t } = useTranslation();
  const [tokens, setTokens] = useState(initialTokens);
  const [inputIncludesCache, setInputIncludesCache] = useState(true);
  const input = { pricing, ...tokens, inputIncludesCache };
  const preview = useMutation({
    mutationFn: async (current: typeof input) => {
      const result = await graphqlRequest<{ previewNewApiBillingExpression: { amountUSD: string } }>(PREVIEW, { input: current });
      return result.previewNewApiBillingExpression.amountUSD;
    },
  });
  const currentResult = JSON.stringify(preview.variables) === JSON.stringify(input);
  return (
    <details className='space-y-3 rounded border p-3'>
      <summary className='cursor-pointer text-sm font-medium'>{t('models.expressionPrice.preview')}</summary>
      <p className='text-muted-foreground text-xs'>{t('models.expressionPrice.previewScope')}</p>
      <div className='grid gap-2 sm:grid-cols-2'>
        {(Object.keys(initialTokens) as Array<keyof TokenInput>).map((name) => (
          <label key={name} className='grid gap-1 text-xs'>
            {t(`models.expressionPrice.${name}`)}
            <Input inputMode='numeric' value={tokens[name]} onChange={(event) => setTokens({ ...tokens, [name]: event.target.value })} />
          </label>
        ))}
      </div>
      <label className='flex items-center gap-2 text-xs'>
        <input type='checkbox' checked={inputIncludesCache} onChange={(event) => setInputIncludesCache(event.target.checked)} />
        {t('models.expressionPrice.inputIncludesCache')}
      </label>
      <Button
        type='button'
        variant='outline'
        size='sm'
        disabled={preview.isPending || !validNewApiExpressionDraft(pricing) || !Object.values(tokens).every((value) => /^\d+$/.test(value))}
        onClick={() => preview.mutate(input)}
      >
        {t('models.expressionPrice.evaluate')}
      </Button>
      {currentResult && preview.isSuccess && (
        <p role='status' className='text-sm'>
          {t('models.expressionPrice.previewResult', { amount: preview.data })}
        </p>
      )}
      {currentResult && preview.isError && (
        <p role='alert' className='text-destructive text-sm'>
          {preview.error.message}
        </p>
      )}
    </details>
  );
}

export function NewApiExpressionEditor({
  value,
  onChange,
}: {
  value: NewApiExpressionPrice;
  onChange: (value: NewApiExpressionPrice) => void;
}) {
  const { t } = useTranslation();
  const { hasSystemScope } = usePermissions();
  return (
    <div className='space-y-3'>
      <p className='text-muted-foreground text-xs'>{t('models.expressionPrice.profile', value)}</p>
      <p className='text-muted-foreground text-xs'>{t('models.expressionPrice.support')}</p>
      <label className='grid gap-1 text-xs'>
        {t('models.expressionPrice.source')}
        <Textarea
          className='min-h-24 font-mono text-sm'
          value={value.expression}
          onChange={(event) => onChange({ ...value, expression: event.target.value })}
        />
      </label>
      <label className='grid gap-1 text-xs'>
        {t('models.expressionPrice.multiplier')}
        <Input inputMode='decimal' value={value.multiplier} onChange={(event) => onChange({ ...value, multiplier: event.target.value })} />
      </label>
      {hasSystemScope('read_commercialization') && <ExpressionPreview pricing={value} />}
    </div>
  );
}
