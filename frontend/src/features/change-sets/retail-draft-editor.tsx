import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Label } from '@/components/ui/label';
import { GroupRetailPriceEditor } from '@/features/models/components/group-retail-price-editor';
import { validateEditorDocument } from '@/features/models/components/simple-retail-price';
import { usePricingModelGroups } from '@/features/models/data/commercialization';
import { getItem, getItemPage, getRetailBase, saveRetailItem, type PackageDetail } from './data/change-sets';

export function RetailDraftEditor({ detail, onSaved, onClose }: { detail: PackageDetail; onSaved: () => void; onClose: () => void }) {
  const { t } = useTranslation();
  const groups = usePricingModelGroups(true);
  const [currency, setCurrency] = useState('');
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
      setCurrency(base.accountingCurrency);
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
      const result = await saveRetailItem({
        changeSetID: detail.summary.id,
        expectedRevision: detail.summary.revision,
        publicModelID: model,
        price,
      });
      if (result.item.validationError) throw new Error(result.item.validationError);
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
      <DialogContent className='max-h-[85svh] max-w-3xl overflow-y-auto'>
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
            {model && !busy && (
              <GroupRetailPriceEditor key={model} value={price} onChange={setPrice} currency={currency} groups={groups.data ?? []} />
            )}
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
          <Button disabled={busy || !loaded || !model || !!validateEditorDocument(price)} onClick={() => void save()}>
            {t('eventPackages.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
