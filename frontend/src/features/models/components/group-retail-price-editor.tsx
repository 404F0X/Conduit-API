import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Textarea } from '@/components/ui/textarea';
import { LazyNewApiExpressionEditor } from './lazy-new-api-expression-editor';
import {
  isNewApiExpressionPrice,
  newApiExpressionPrice,
  readRetailPriceDocument,
  readSimpleRetailPrice,
  serializeRetailPriceDocument,
  validateEditorDocument,
  type RetailPriceDocument,
  type RetailPriceLeaf,
  type SimpleRetailPrice,
  type StaticRetailPriceLeaf,
} from './simple-retail-price';

type GroupOption = { id: string; name: string; status: string };
const blank = (): RetailPriceLeaf => ({ items: [] });

function StaticLeafEditor({
  value,
  onChange,
  currency,
}: {
  value: StaticRetailPriceLeaf;
  onChange: (value: StaticRetailPriceLeaf) => void;
  currency: string;
}) {
  const { t } = useTranslation();
  const [raw, setRaw] = useState(readSimpleRetailPrice(value) === null);
  const [text, setText] = useState(JSON.stringify(value, null, 2));
  const [error, setError] = useState('');
  const [form, setForm] = useState(() => readSimpleRetailPrice(value));
  const simple = readSimpleRetailPrice(value);
  const update = (next: SimpleRetailPrice) => {
    setForm(next);
    // Incomplete decimal input remains part of this leaf; save validation is
    // performed below and by the canonical backend parser, never by fallback.
    const fields = {
      prompt: 'prompt_tokens',
      completion: 'completion_tokens',
      cacheRead: 'prompt_cached_tokens',
      cacheWrite: 'prompt_write_cached_tokens',
    };
    const items =
      next.mode === 'request'
        ? [{ itemCode: 'request', pricing: { mode: 'flat_fee', flatFee: next.flatFee } }]
        : Object.entries(fields)
            .filter(([field]) => next[field as keyof typeof fields] !== '')
            .map(([field, itemCode]) => ({
              itemCode,
              pricing: { mode: 'usage_per_unit', usagePerUnit: next[field as keyof typeof fields] },
            }));
    onChange({ items });
  };
  return (
    <div className='space-y-3'>
      <Button
        type='button'
        variant='outline'
        size='sm'
        onClick={() => {
          if (!raw) {
            setText(JSON.stringify(value, null, 2));
            setRaw(true);
          } else if (simple !== null && !error) {
            setForm(simple);
            setRaw(false);
          }
        }}
        disabled={raw && (simple === null || !!error)}
      >
        {t(raw ? 'models.groupPrice.visual' : 'models.groupPrice.raw')}
      </Button>
      {raw ? (
        <>
          <Textarea
            aria-label={t('models.groupPrice.raw')}
            className='min-h-36 font-mono text-xs'
            value={text}
            onChange={(event) => {
              setText(event.target.value);
              try {
                const parsed: unknown = JSON.parse(event.target.value);
                if (!parsed || typeof parsed !== 'object' || !Array.isArray((parsed as StaticRetailPriceLeaf).items))
                  throw new Error(t('models.groupPrice.invalidLeaf'));
                onChange(parsed as StaticRetailPriceLeaf);
                setError('');
              } catch (err) {
                setError(err instanceof Error ? err.message : String(err));
                onChange({ items: [] });
              }
            }}
          />
          {error && (
            <p role='alert' className='text-destructive text-xs'>
              {error}
            </p>
          )}
          {simple === null && <p className='text-muted-foreground text-xs'>{t('models.groupPrice.complexPreserved')}</p>}
        </>
      ) : (
        form && (
          <>
            <Label>{t('models.groupPrice.mode')}</Label>
            <select
              className='bg-background h-9 rounded-md border px-2 text-sm'
              value={form.mode}
              onChange={(event) => update({ ...form, mode: event.target.value as SimpleRetailPrice['mode'] })}
            >
              <option value='tokens'>{t('models.groupPrice.tokens')}</option>
              <option value='request'>{t('models.groupPrice.request')}</option>
            </select>
            <div className='grid gap-3 sm:grid-cols-2'>
              {(form.mode === 'request' ? (['flatFee'] as const) : (['prompt', 'completion', 'cacheRead', 'cacheWrite'] as const)).map(
                (field) => (
                  <label key={field} className='grid gap-1 text-xs'>
                    {t(`models.groupPrice.${field}`, { currency })}
                    <Input inputMode='decimal' value={form[field]} onChange={(event) => update({ ...form, [field]: event.target.value })} />
                  </label>
                )
              )}
            </div>
          </>
        )
      )}
    </div>
  );
}

function LeafEditor({
  value,
  onChange,
  currency,
}: {
  value: RetailPriceLeaf;
  onChange: (value: RetailPriceLeaf) => void;
  currency: string;
}) {
  const { t } = useTranslation();
  const expression = isNewApiExpressionPrice(value);
  const [previousStatic, setPreviousStatic] = useState<StaticRetailPriceLeaf>({ items: [] });
  const [previousExpression, setPreviousExpression] = useState(newApiExpressionPrice);
  return (
    <div className='space-y-3'>
      <label className='grid gap-1 text-xs'>
        {t('models.expressionPrice.kind')}
        <select
          className='bg-background h-9 rounded-md border px-2 text-sm'
          value={expression ? 'expression' : 'static'}
          onChange={(event) => {
            if (event.target.value === 'expression' && !expression) {
              setPreviousStatic(value);
              onChange(previousExpression);
            } else if (expression) {
              setPreviousExpression(value);
              onChange(previousStatic);
            }
          }}
        >
          <option value='static'>{t('models.expressionPrice.static')}</option>
          <option value='expression'>{t('models.expressionPrice.expression')}</option>
        </select>
      </label>
      {expression ? (
        <LazyNewApiExpressionEditor value={value} onChange={onChange} />
      ) : (
        <StaticLeafEditor value={value} onChange={onChange} currency={currency} />
      )}
    </div>
  );
}

export function GroupRetailPriceEditor({
  value,
  onChange,
  currency,
  groups,
}: {
  value: unknown;
  onChange: (value: unknown) => void;
  currency: string;
  groups: GroupOption[];
}) {
  const { t } = useTranslation();
  const [raw, setRaw] = useState(readRetailPriceDocument(value) === null);
  const [text, setText] = useState(JSON.stringify(value, null, 2));
  const [error, setError] = useState('');
  const [selectedGroup, setSelectedGroup] = useState('');
  const doc = readRetailPriceDocument(value);
  const change = (next: RetailPriceDocument) => onChange(serializeRetailPriceDocument(next));
  const errorMessage = error || (validateEditorDocument(value) ? t(validateEditorDocument(value)!) : '');
  return (
    <div className='space-y-4'>
      <p className='text-muted-foreground text-xs'>{t('models.groupPrice.hint')}</p>
      <Button
        type='button'
        variant='outline'
        size='sm'
        onClick={() => {
          if (!raw) {
            setText(JSON.stringify(value, null, 2));
            setRaw(true);
          } else if (doc && !error) setRaw(false);
        }}
        disabled={raw && (!doc || !!error)}
      >
        {t(raw ? 'models.groupPrice.visual' : 'models.groupPrice.rawDocument')}
      </Button>
      {raw ? (
        <Textarea
          aria-label={t('models.groupPrice.rawDocument')}
          className='min-h-64 font-mono text-xs'
          value={text}
          onChange={(event) => {
            setText(event.target.value);
            try {
              const next: unknown = JSON.parse(event.target.value);
              onChange(next);
              setError('');
            } catch (err) {
              setError(err instanceof Error ? err.message : String(err));
              onChange(null);
            }
          }}
        />
      ) : (
        doc && (
          <>
            <details open className='rounded-lg border p-3'>
              <summary className='cursor-pointer text-sm font-semibold'>{t('models.groupPrice.global')}</summary>
              <div className='mt-3 space-y-3'>
                {doc.global ? (
                  <>
                    <LeafEditor value={doc.global} onChange={(global) => change({ ...doc, global })} currency={currency} />
                    <Button type='button' variant='ghost' onClick={() => change({ ...doc, global: null })}>
                      {t('models.groupPrice.remove')}
                    </Button>
                  </>
                ) : (
                  <Button type='button' variant='outline' onClick={() => change({ ...doc, global: blank() })}>
                    {t('models.groupPrice.addBase')}
                  </Button>
                )}
              </div>
            </details>
            <div className='flex flex-wrap gap-2'>
              <select
                aria-label={t('models.groupPrice.selectGroup')}
                className='bg-background h-9 max-w-full rounded-md border px-2 text-sm'
                value={selectedGroup}
                onChange={(event) => setSelectedGroup(event.target.value)}
              >
                <option value=''>{t('models.groupPrice.selectGroup')}</option>
                {groups
                  .filter((group) => group.status === 'ENABLED' && !doc.groups.some((branch) => branch.modelGroupID === group.id))
                  .map((group) => (
                    <option key={group.id} value={group.id}>
                      {group.name}
                    </option>
                  ))}
              </select>
              <Button
                type='button'
                variant='outline'
                disabled={!selectedGroup || doc.groups.length >= 128}
                onClick={() => {
                  change({ ...doc, groups: [...doc.groups, { modelGroupID: selectedGroup, base: blank(), rules: [] }] });
                  setSelectedGroup('');
                }}
              >
                {t('models.groupPrice.addGroup')}
              </Button>
            </div>
            {doc.groups.map((group, index) => {
              const update = (next: typeof group) => change({ ...doc, groups: doc.groups.map((old, i) => (i === index ? next : old)) });
              return (
                <details key={group.modelGroupID} open className='rounded-lg border p-3'>
                  <summary className='cursor-pointer text-sm font-semibold'>
                    {groups.find((option) => option.id === group.modelGroupID)?.name || group.modelGroupID}
                  </summary>
                  <div className='mt-3 space-y-4'>
                    <p className='text-muted-foreground text-xs'>{t('models.groupPrice.orderHint')}</p>
                    {group.rules.map((rule, ruleIndex) => {
                      const updateRule = (next: typeof rule) =>
                        update({ ...group, rules: group.rules.map((old, i) => (i === ruleIndex ? next : old)) });
                      return (
                        <details key={rule.id} open className='rounded-md border p-3'>
                          <summary className='cursor-pointer text-sm font-semibold'>
                            {rule.label || `${t('models.groupPrice.ruleLabel')} ${ruleIndex + 1}`}
                          </summary>
                          <div className='mt-3 space-y-3'>
                            <Input
                              aria-label={t('models.groupPrice.ruleLabel')}
                              value={rule.label}
                              maxLength={256}
                              placeholder={t('models.groupPrice.ruleLabel')}
                              onChange={(event) => updateRule({ ...rule, label: event.target.value })}
                            />
                            <label className='grid gap-1 text-xs'>
                              {t('models.groupPrice.condition')}
                              <Textarea
                                className='font-mono text-xs'
                                value={typeof rule.condition === 'string' ? rule.condition : JSON.stringify(rule.condition)}
                                onChange={(event) => {
                                  let condition: typeof rule.condition = event.target.value;
                                  if (condition.trimStart().startsWith('{')) {
                                    try {
                                      const parsed: unknown = JSON.parse(condition);
                                      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed))
                                        condition = parsed as Record<string, unknown>;
                                    } catch {
                                      /* Preserve invalid input for the canonical validation error. */
                                    }
                                  }
                                  updateRule({ ...rule, condition });
                                }}
                              />
                            </label>
                            <LeafEditor value={rule.price} onChange={(price) => updateRule({ ...rule, price })} currency={currency} />
                            <div className='flex gap-2'>
                              {[-1, 1].map((direction) => (
                                <Button
                                  key={direction}
                                  type='button'
                                  variant='outline'
                                  size='sm'
                                  disabled={ruleIndex + direction < 0 || ruleIndex + direction >= group.rules.length}
                                  onClick={() => {
                                    const rules = [...group.rules];
                                    [rules[ruleIndex], rules[ruleIndex + direction]] = [rules[ruleIndex + direction], rules[ruleIndex]];
                                    update({ ...group, rules });
                                  }}
                                >
                                  {t(direction < 0 ? 'models.groupPrice.up' : 'models.groupPrice.down')}
                                </Button>
                              ))}
                              <Button
                                type='button'
                                variant='ghost'
                                size='sm'
                                onClick={() => update({ ...group, rules: group.rules.filter((_, i) => i !== ruleIndex) })}
                              >
                                {t('models.groupPrice.remove')}
                              </Button>
                            </div>
                          </div>
                        </details>
                      );
                    })}
                    <Button
                      type='button'
                      variant='outline'
                      disabled={group.rules.length >= 128}
                      onClick={() =>
                        update({
                          ...group,
                          rules: [
                            ...group.rules,
                            { id: crypto.randomUUID(), label: '', condition: 'prompt_tokens >= 1000 && hour < 18', price: blank() },
                          ],
                        })
                      }
                    >
                      {t('models.groupPrice.addRule')}
                    </Button>
                    <p className='text-muted-foreground text-xs'>{t('models.groupPrice.facts')}</p>
                    <div className='space-y-3 border-t pt-3'>
                      <h4 className='text-sm font-medium'>{t('models.groupPrice.base')}</h4>
                      {group.base ? (
                        <>
                          <LeafEditor value={group.base} onChange={(base) => update({ ...group, base })} currency={currency} />
                          <Button type='button' variant='ghost' size='sm' onClick={() => update({ ...group, base: null })}>
                            {t('models.groupPrice.remove')}
                          </Button>
                        </>
                      ) : (
                        <Button type='button' variant='outline' onClick={() => update({ ...group, base: blank() })}>
                          {t('models.groupPrice.addBase')}
                        </Button>
                      )}
                    </div>
                    <Button
                      type='button'
                      variant='destructive'
                      size='sm'
                      onClick={() => change({ ...doc, groups: doc.groups.filter((_, i) => i !== index) })}
                    >
                      {t('models.groupPrice.removeGroup')}
                    </Button>
                  </div>
                </details>
              );
            })}
          </>
        )
      )}
      {errorMessage && (
        <p role='alert' className='text-destructive text-sm'>
          {errorMessage}
        </p>
      )}
    </div>
  );
}
