import { useId, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { useQueryAllModels } from '@/features/models/data/models';
import { modelTypeSchema, type ModelType } from '@/features/models/data/schema';
import { useChannelPublicModelBindings } from '../data/channels';
import type { ChannelPublicModelBindingsInput } from '../data/schema';

type Props = {
  channelID?: string;
  upstreamModels: string[];
  changes: ChannelPublicModelBindingsInput;
  onChange: (changes: ChannelPublicModelBindingsInput) => void;
};

export function ChannelPublicModelBindings({ channelID, upstreamModels, changes, onChange }: Props) {
  const { t } = useTranslation();
  const id = useId();
  const [expanded, setExpanded] = useState(false);
  const [upstreamModelID, setUpstreamModelID] = useState('');
  const [key, setKey] = useState('');
  const [modelType, setModelType] = useState<ModelType>();
  const [confirmed, setConfirmed] = useState(false);
  const catalog = useQueryAllModels({}, { enabled: expanded });
  const bindings = useChannelPublicModelBindings(channelID, expanded);
  const publicModelKey = key.trim();
  const catalogModels = catalog.data?.edges.map(({ node }) => node) ?? [];
  const existingModel = catalogModels.find((model) => model.modelID === publicModelKey);
  const upserts = changes.upserts ?? [];
  const removals = changes.removeRouteIDs ?? [];
  const ready = catalog.isSuccess && (!channelID || (bindings.isSuccess && !!bindings.data));
  const alreadyBound =
    upserts.some((binding) => binding.upstreamModelID === upstreamModelID && binding.publicModelKey === publicModelKey) ||
    bindings.data?.some((binding) => binding.upstreamModelID === upstreamModelID && binding.publicModelKey === publicModelKey);
  const canAdd =
    ready &&
    upstreamModels.includes(upstreamModelID) &&
    !!publicModelKey &&
    !alreadyBound &&
    (existingModel ? existingModel.status !== 'enabled' || confirmed : !!modelType);

  function queueBinding() {
    if (!canAdd) return;
    onChange({
      ...changes,
      upserts: [
        ...upserts,
        {
          upstreamModelID,
          publicModelKey,
          ...(existingModel ? { confirmCompatibility: confirmed } : { modelType }),
        },
      ],
    });
    setKey('');
    setConfirmed(false);
    setModelType(undefined);
  }

  return (
    <details className='space-y-3 rounded-md border p-4' onToggle={(event) => setExpanded(event.currentTarget.open)}>
      <summary className='cursor-pointer text-sm font-semibold'>{t('channels.publicBindings.title')}</summary>
      <p className='text-muted-foreground text-xs'>{t('channels.publicBindings.description')}</p>
      {expanded && (catalog.isPending || (channelID && bindings.isPending)) && (
        <p role='status' className='text-muted-foreground text-sm'>
          {t('channels.publicBindings.loading')}
        </p>
      )}
      {expanded && (catalog.isError || bindings.isError || (channelID && bindings.isSuccess && bindings.data === null)) && (
        <p role='alert' className='text-destructive text-sm'>
          {t('channels.publicBindings.loadError')}
        </p>
      )}
      {bindings.data?.map((binding) => (
        <div key={binding.routeID} className='flex flex-wrap items-center justify-between gap-2 text-sm'>
          <span className='min-w-0 break-all'>
            {binding.upstreamModelID} → {binding.publicModelKey} · {t(`channels.publicBindings.status.${binding.status}`)}
          </span>
          {binding.status === 'ENABLED' && (
            <Button
              type='button'
              variant='outline'
              size='sm'
              onClick={() =>
                onChange({
                  ...changes,
                  removeRouteIDs: removals.includes(binding.routeID)
                    ? removals.filter((routeID) => routeID !== binding.routeID)
                    : [...removals, binding.routeID],
                })
              }
            >
              {t(removals.includes(binding.routeID) ? 'channels.publicBindings.undoDisable' : 'channels.publicBindings.disable')}
            </Button>
          )}
        </div>
      ))}
      {upserts.map((binding, index) => (
        <div
          key={`${binding.upstreamModelID}\u0000${binding.publicModelKey}`}
          className='flex flex-wrap items-center justify-between gap-2 text-sm'
        >
          <span className='min-w-0 break-all'>
            {binding.upstreamModelID} → {binding.publicModelKey}
          </span>
          <Button
            type='button'
            variant='outline'
            size='sm'
            onClick={() => onChange({ ...changes, upserts: upserts.filter((_, i) => i !== index) })}
          >
            {t('channels.publicBindings.removeDraft')}
          </Button>
        </div>
      ))}
      {ready && (
        <div className='space-y-3'>
          <div className='grid gap-3 sm:grid-cols-2'>
            <div className='min-w-0 space-y-2'>
              <Label htmlFor={`${id}-upstream`}>{t('channels.publicBindings.upstreamModel')}</Label>
              <Select
                value={upstreamModelID}
                onValueChange={(value) => {
                  setUpstreamModelID(value);
                  setConfirmed(false);
                }}
              >
                <SelectTrigger id={`${id}-upstream`} disabled={upstreamModels.length === 0}>
                  <SelectValue placeholder={t('channels.publicBindings.selectUpstream')} />
                </SelectTrigger>
                <SelectContent>
                  {upstreamModels.map((model) => (
                    <SelectItem key={model} value={model}>
                      {model}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className='min-w-0 space-y-2'>
              <Label htmlFor={`${id}-key`}>{t('channels.publicBindings.publicModel')}</Label>
              <Input
                id={`${id}-key`}
                list={`${id}-catalog`}
                value={key}
                onChange={(event) => {
                  setKey(event.target.value);
                  setConfirmed(false);
                  setModelType(undefined);
                }}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') event.preventDefault();
                }}
              />
              <datalist id={`${id}-catalog`}>
                {catalogModels.map((model) => (
                  <option key={model.id} value={model.modelID} />
                ))}
              </datalist>
            </div>
          </div>
          {publicModelKey && !existingModel && (
            <div className='space-y-2'>
              <Label htmlFor={`${id}-type`}>{t('channels.publicBindings.modelType')}</Label>
              <Select value={modelType ?? ''} onValueChange={(value) => setModelType(modelTypeSchema.parse(value))}>
                <SelectTrigger id={`${id}-type`}>
                  <SelectValue placeholder={t('channels.publicBindings.selectType')} />
                </SelectTrigger>
                <SelectContent>
                  {modelTypeSchema.options.map((type) => (
                    <SelectItem key={type} value={type}>
                      {t(`models.types.${type}`)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
              <p className='text-muted-foreground text-xs'>{t('channels.publicBindings.newModel')}</p>
            </div>
          )}
          {existingModel?.status === 'enabled' && !alreadyBound && (
            <Label className='flex items-start gap-2'>
              <Checkbox checked={confirmed} onCheckedChange={(checked) => setConfirmed(checked === true)} />
              {t('channels.publicBindings.confirmCompatibility')}
            </Label>
          )}
          {alreadyBound && <p className='text-muted-foreground text-xs'>{t('channels.publicBindings.alreadyBound')}</p>}
          <Button type='button' variant='outline' size='sm' onClick={queueBinding} disabled={!canAdd}>
            {t('channels.publicBindings.add')}
          </Button>
        </div>
      )}
    </details>
  );
}
