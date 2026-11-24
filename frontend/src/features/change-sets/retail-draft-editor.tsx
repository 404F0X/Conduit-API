import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { getItem, getItemPage, getRetailBase, saveRetailItem, type PackageDetail } from './data/change-sets';
import { displayAmount, priceRows, updatePriceAmount } from './price-view';

export function RetailDraftEditor({ detail, onSaved, onClose }: { detail: PackageDetail; onSaved: () => void; onClose: () => void }) {
  const { t } = useTranslation();
  const [model, setModel] = useState('');
  const [price, setPrice] = useState<unknown>({ items: [] });
  const [choices, setChoices] = useState<string[]>([]);
  const [busy, setBusy] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string>();
  const load = async () => {
    setBusy(true);
    try {
      const base = await getRetailBase(detail.summary.scopeID);
      const keys = new Set(base.prices.map((p) => p.publicModelID));
      let after: string | undefined;
      do {
        const page = await getItemPage(detail.summary.id, after);
        page.edges.forEach((e) => keys.add(e.node.itemKey));
        after = page.pageInfo.hasNextPage ? page.pageInfo.endCursor! : undefined;
      } while (after);
      setChoices([...keys]);
      setLoaded(true);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const select = async (value: string) => {
    setModel(value);
    setBusy(true);
    setError(undefined);
    try {
      const base = await getRetailBase(detail.summary.scopeID);
      let proposed: unknown;
      let after: string | undefined;
      do {
        const page = await getItemPage(detail.summary.id, after, value);
        const exact = page.edges.find((e) => e.node.itemKey === value);
        if (exact) {
          proposed = (await getItem(detail.summary.id, exact.node.id))?.item.afterSnapshot;
          break;
        }
        after = page.pageInfo.hasNextPage ? page.pageInfo.endCursor! : undefined;
      } while (after);
      setPrice(proposed ?? base.prices.find((p) => p.publicModelID === value)?.price ?? { items: [] });
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };
  const save = async () => {
    setBusy(true);
    setError(undefined);
    try {
      await saveRetailItem({ changeSetID: detail.summary.id, expectedRevision: detail.summary.revision, publicModelID: model, price });
      onSaved();
      onClose();
    } catch (e) {
      setError(e instanceof Error ? e.message : t('eventPackages.error'));
    } finally {
      setBusy(false);
    }
  };
  return (
    <Dialog open onOpenChange={(open) => !open && !busy && onClose()}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('eventPackages.edit')}</DialogTitle>
          <DialogDescription>{t('eventPackages.priceUnit')}</DialogDescription>
        </DialogHeader>
        {!loaded ? (
          <Button disabled={busy} onClick={() => void load()}>
            {t('eventPackages.refresh')}
          </Button>
        ) : (
          <>
            <Label htmlFor='retail-model'>{t('eventPackages.model')}</Label>
            <select
              id='retail-model'
              className='border-input bg-background h-10 rounded-md border px-3 text-sm'
              value={model}
              onChange={(e) => void select(e.target.value)}
            >
              <option value=''>{t('eventPackages.changes')}</option>
              {choices.map((value) => (
                <option key={value} value={value}>
                  {value}
                </option>
              ))}
            </select>
            {['prompt_tokens', 'completion_tokens', 'prompt_cached_tokens', 'prompt_write_cached_tokens', 'request'].map((code) => {
              const field = priceRows(price).find((p) => p.code === code);
              return (
                <div key={code}>
                  <Label htmlFor={code}>{t(`eventPackages.fields.${code}`)}</Label>
                  <Input
                    id={code}
                    inputMode='decimal'
                    value={displayAmount(field?.amount) ?? ''}
                    disabled={busy || (field != null && !['usage_per_unit', 'flat_fee'].includes(field.mode))}
                    onChange={(e) => setPrice(updatePriceAmount(price, code, e.target.value))}
                  />
                </div>
              );
            })}
          </>
        )}
        {error && (
          <p role='alert' className='text-destructive text-sm'>
            {error}
          </p>
        )}
        <DialogFooter>
          <Button variant='outline' disabled={busy} onClick={onClose}>
            {t('eventPackages.cancel')}
          </Button>
          <Button disabled={busy || !loaded || !model} onClick={() => void save()}>
            {t('eventPackages.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
