import { useCallback, useEffect, useRef, useState, type ReactNode } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { IconRocket } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { DEFAULT_ACCOUNTING_CURRENCY_CODE } from '@/lib/accounting';
import { extractNumberID } from '@/lib/utils';
import { usePermissions } from '@/hooks/usePermissions';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import {
  getRetailBase,
  getItemPage,
  getItem,
  getPackageByOperation,
  getPackage,
  type PackageDetail,
  useCreateRetailPriceChangeSet,
  useSaveRetailPriceChangeSetItem,
  useSubmitChangeSet,
} from '@/features/change-sets/data/change-sets';
import { useGeneralSettings } from '@/features/system/data/system';
import {
  type ModelRoute,
  useCommercializationCatalog,
  useCreatePriceBook,
  useUpsertModelRoute,
  usePricingModelGroups,
} from '../data/commercialization';
import type { Model } from '../data/schema';
import { GroupRetailPriceEditor } from './group-retail-price-editor';
import { RetailPriceFormulaEditor } from './retail-price-formula-editor';
import { validateEditorDocument } from './simple-retail-price';

export type CommercializationAction =
  | { kind: 'add-route'; publicModelID: string }
  | { kind: 'edit-route'; route: ModelRoute }
  | { kind: 'price' | 'formula'; publicModelID: string };

type Props = {
  models: Model[];
  action: CommercializationAction | null;
  onActionHandled: () => void;
};
type RouteForm = {
  id?: string;
  publicModelID: string;
  deploymentID: string;
  status: 'ENABLED' | 'DISABLED';
  confirmCompatibility: boolean;
};
type PriceForm = { publicModelID: string };

const emptyRoute = (): RouteForm => ({
  publicModelID: '',
  deploymentID: '',
  status: 'ENABLED',
  confirmCompatibility: false,
});
const emptyPrice = (): PriceForm => ({ publicModelID: '' });
export function CommercializationPanel({ models, action, onActionHandled }: Props) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { hasSystemScope, user } = usePermissions();
  const { data: generalSettings } = useGeneralSettings();
  const accountingCurrencyCode = generalSettings?.accountingCurrencyCode?.trim() || DEFAULT_ACCOUNTING_CURRENCY_CODE;
  const canWriteCommercialization = hasSystemScope('write_commercialization');
  const catalog = useCommercializationCatalog();
  const upsertRoute = useUpsertModelRoute();
  const createBook = useCreatePriceBook();
  const createRetailChangeSet = useCreateRetailPriceChangeSet();
  const saveItem = useSaveRetailPriceChangeSetItem();
  const submitChangeSet = useSubmitChangeSet();
  const [routeOpen, setRouteOpen] = useState(false);
  const [priceOpen, setPriceOpen] = useState(false);
  const [formulaModelID, setFormulaModelID] = useState('');
  const [editingDraftID, setEditingDraftID] = useState('');
  const [retailChangeSet, setRetailChangeSet] = useState<PackageDetail | null>(null);
  const [priceModelLocked, setPriceModelLocked] = useState(false);
  const [route, setRoute] = useState<RouteForm>(emptyRoute());
  const [basePrices, setBasePrices] = useState<Array<{ publicModelID: string; price: unknown }>>([]);
  const [priceDocument, setPriceDocument] = useState<unknown>({ items: [] });
  const pricingGroups = usePricingModelGroups(canWriteCommercialization && priceOpen);
  const [price, setPrice] = useState<PriceForm>(emptyPrice());
  const pendingRetailIntent = useRef<{ modelID: string; bookID: string; operationID: string; baseRevision: string } | null>(null);

  // Settlement only reads the explicitly designated default book. Falling
  // back to an arbitrary book here would let the UI edit prices that runtime
  // billing can never see.
  const defaultBook = catalog.data?.priceBooks.find((book) => book.isDefault);
  const selectedPublicModelHasRoutes = (catalog.data?.modelRoutes || []).some(
    (item) => item.publicModelID === route.publicModelID && item.id !== route.id && item.status !== 'ARCHIVED'
  );

  const saveRoute = async () => {
    if (!route.publicModelID || !route.deploymentID) return toast.error('请选择对外模型和上游模型实例');
    if (selectedPublicModelHasRoutes && !route.confirmCompatibility) {
      return toast.error('请确认该上游实例与现有实例能力和质量等价；否则应创建另一个对外模型');
    }
    try {
      await upsertRoute.mutateAsync({
        ...(route.id ? { id: route.id } : {}),
        publicModelID: route.publicModelID,
        deploymentID: route.deploymentID,
        status: route.status,
        confirmCompatibility: route.confirmCompatibility,
      });
      toast.success('上游模型映射已保存');
      setRouteOpen(false);
      setRoute(emptyRoute());
    } catch (error) {
      toast.error(error instanceof Error ? error.message : '保存上游模型映射失败');
    }
  };

  const editRoute = useCallback((item: ModelRoute) => {
    setRoute({
      id: item.id,
      publicModelID: item.publicModelID,
      deploymentID: item.deploymentID,
      status: item.status === 'DISABLED' ? 'DISABLED' : 'ENABLED',
      confirmCompatibility: false,
    });
    setRouteOpen(true);
  }, []);

  const openPriceEditor = useCallback(
    async (publicModelID = '') => {
      try {
        if (!defaultBook) {
          await createBook.mutateAsync({ name: 'Default retail pricing', currency: accountingCurrencyCode, isDefault: true });
        }
        const fresh = await catalog.refetch();
        const book = fresh.data?.priceBooks.find((item) => item.isDefault);
        if (!book) throw new Error('无法创建默认价格簿');
        if (pendingRetailIntent.current && pendingRetailIntent.current.modelID !== publicModelID)
          throw new Error(t('eventPackages.pendingIntent'));
        const base = await getRetailBase(book.id);
        setBasePrices(base.prices);
        const existing = base.prices.find((item) => extractNumberID(item.publicModelID) === extractNumberID(publicModelID));
        setPriceDocument(existing ? existing.price : { items: [] });
        setPriceModelLocked(Boolean(publicModelID));
        setPrice({ ...emptyPrice(), publicModelID });
        const storageKey = `retail-price-draft:${user?.id ?? 'current'}:${book.id}:${publicModelID}`;
        const storedOperation = typeof window === 'undefined' ? null : window.sessionStorage.getItem(storageKey);
        let intent = pendingRetailIntent.current ?? {
          modelID: publicModelID,
          bookID: book.id,
          operationID: storedOperation || crypto.randomUUID(),
          baseRevision: base.revision,
        };
        pendingRetailIntent.current = intent;
        let editable = await getPackageByOperation(intent.operationID, 'RETAIL_PRICE', 'price_book', intent.bookID);
        if (editable && editable.summary.status !== 'DRAFT') {
          intent = { ...intent, operationID: crypto.randomUUID(), baseRevision: base.revision };
          pendingRetailIntent.current = intent;
          editable = null;
        }
        if (!editable)
          editable = await createRetailChangeSet.mutateAsync({
            priceBookID: intent.bookID,
            operationID: intent.operationID,
            expectedBaseRevision: intent.baseRevision,
          });
        if (!editable.currentBaseRevision || editable.currentBaseRevision !== base.revision) throw new Error(t('eventPackages.conflict'));
        if (typeof window !== 'undefined') window.sessionStorage.setItem(storageKey, intent.operationID);
        if (publicModelID) {
          const savedPrice = await readDraftModelPrice(editable.summary.id, publicModelID);
          if (savedPrice !== undefined) setPriceDocument(savedPrice);
        }
        pendingRetailIntent.current = null;
        setRetailChangeSet(editable);
        setEditingDraftID(editable.summary.id);
        setPriceModelLocked(Boolean(publicModelID));

        setPriceOpen(true);
      } catch (error) {
        toast.error(error instanceof Error ? error.message : '无法准备价格编辑器');
      }
    },
    [accountingCurrencyCode, catalog, createBook, createRetailChangeSet, defaultBook, t, user?.id]
  );

  useEffect(() => {
    if (!action) return;
    if (action.kind === 'add-route') {
      setRoute({ ...emptyRoute(), publicModelID: action.publicModelID });
      setRouteOpen(true);
    } else if (action.kind === 'edit-route') {
      editRoute(action.route);
    } else if (action.kind === 'formula') {
      setFormulaModelID(action.publicModelID);
    } else {
      void openPriceEditor(action.publicModelID);
    }
    onActionHandled();
  }, [action, editRoute, onActionHandled, openPriceEditor]);

  const savePrice = async () => {
    const invalid = validateEditorDocument(priceDocument);
    if (invalid) return toast.error(t(invalid));
    if (!editingDraftID || !price.publicModelID) return toast.error(t('models.groupPrice.selectModel'));
    try {
      const fresh = await getPackage(editingDraftID);
      if (!fresh || fresh.summary.status !== 'DRAFT') throw new Error(t('eventPackages.conflict'));
      const saved = await saveItem.mutateAsync({
        changeSetID: editingDraftID,
        expectedRevision: fresh.summary.revision,
        publicModelID: price.publicModelID,
        price: priceDocument,
      });
      setRetailChangeSet(await getPackage(editingDraftID));
      if (saved.item.validationError) throw new Error(saved.item.validationError);
      toast.success('零售价变更已保存');
      setPriceOpen(false);
      setPrice(emptyPrice());
    } catch (error) {
      toast.error(error instanceof Error ? error.message : '保存价格失败');
    }
  };

  const submitRetailChangeSet = async () => {
    if (!retailChangeSet || retailChangeSet.summary.status !== 'DRAFT') return;
    try {
      const updated = await submitChangeSet.mutateAsync({
        id: retailChangeSet.summary.id,
        expectedRevision: retailChangeSet.summary.revision,
      });
      setRetailChangeSet(updated);
      toast.success('零售价变更已提交审核');
    } catch (error) {
      toast.error(error instanceof Error ? error.message : '提交零售价变更失败');
    }
  };

  const openRetailReview = () => {
    if (!defaultBook) return;
    void navigate({
      to: '/change-sets',
      search: {
        kind: 'RETAIL_PRICE',
        status: 'PENDING_REVIEW',
        scopeType: 'price_book',
        scopeID: defaultBook.id,
      },
    });
  };

  if (catalog.isError) {
    return (
      <Alert variant='destructive'>
        <AlertTitle>商业化配置加载失败</AlertTitle>
        <AlertDescription>{catalog.error.message}</AlertDescription>
      </Alert>
    );
  }

  return (
    <>
      <Dialog
        open={Boolean(formulaModelID)}
        onOpenChange={(open) => {
          if (!open) setFormulaModelID('');
        }}
      >
        <DialogContent className='max-h-[90vh] overflow-y-auto sm:max-w-2xl'>
          <DialogHeader>
            <DialogTitle>自动零售价公式</DialogTitle>
            <DialogDescription>来源保持固定，价格变化经过治理发布后才用于客户收费。</DialogDescription>
          </DialogHeader>
          {defaultBook && models.find((model) => model.id === formulaModelID) ? (
            <RetailPriceFormulaEditor
              bookID={defaultBook.id}
              model={models.find((model) => model.id === formulaModelID)!}
              routes={catalog.data?.modelRoutes ?? []}
              canWrite={canWriteCommercialization}
            />
          ) : (
            <Button
              onClick={() =>
                void createBook.mutateAsync({ name: 'Default retail pricing', currency: accountingCurrencyCode, isDefault: true })
              }
              disabled={createBook.isPending || !canWriteCommercialization}
            >
              创建默认价格簿
            </Button>
          )}
        </DialogContent>
      </Dialog>
      {retailChangeSet && (
        <div className='bg-card flex flex-col gap-2 rounded-lg border border-dashed p-3 sm:flex-row sm:items-center sm:justify-between'>
          <div>
            <div className='text-sm font-medium'>零售价变更 · {retailChangeSet.summary.status === 'DRAFT' ? '编辑中' : '待审核'}</div>
            <div className='text-muted-foreground text-xs'>{retailChangeSet.summary.itemCount} 个模型已定价；批准前不会用于应收计算。</div>
          </div>
          <Button
            onClick={() => (retailChangeSet.summary.status === 'DRAFT' ? void submitRetailChangeSet() : openRetailReview())}
            disabled={
              retailChangeSet.summary.status === 'DRAFT' &&
              (!canWriteCommercialization || !retailChangeSet.summary.itemCount || submitChangeSet.isPending)
            }
          >
            <IconRocket className='size-4' />
            {retailChangeSet.summary.status === 'DRAFT' ? '提交审核' : '前往审批工作台'}
          </Button>
        </div>
      )}

      <Dialog
        open={routeOpen}
        onOpenChange={(open) => {
          setRouteOpen(open);
          if (!open) setRoute(emptyRoute());
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{route.id ? '编辑上游映射' : '连接上游模型'}</DialogTitle>
            <DialogDescription>为一个用户可见的对外模型选择实际执行请求的上游模型实例。</DialogDescription>
          </DialogHeader>
          <div className='grid gap-4'>
            <Field label='对外模型'>
              <Select value={route.publicModelID} onValueChange={(value) => setRoute({ ...route, publicModelID: value })}>
                <SelectTrigger>
                  <SelectValue placeholder='选择用户请求使用的模型 ID' />
                </SelectTrigger>
                <SelectContent>
                  {models.map((model) => (
                    <SelectItem key={model.id} value={model.id}>
                      {model.name} · {model.modelID}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label='上游模型实例'>
              <Select value={route.deploymentID} onValueChange={(value) => setRoute({ ...route, deploymentID: value })}>
                <SelectTrigger>
                  <SelectValue placeholder='选择上游渠道发现的真实模型' />
                </SelectTrigger>
                <SelectContent>
                  {(catalog.data?.upstreamModelDeployments || [])
                    .filter((deployment) => deployment.status === 'ENABLED')
                    .map((deployment) => (
                      <SelectItem key={deployment.id} value={deployment.id}>
                        {deployment.channelName} / {deployment.upstreamModelID}
                      </SelectItem>
                    ))}
                </SelectContent>
              </Select>
            </Field>
            <Field label='路由状态'>
              <Select value={route.status} onValueChange={(value: 'ENABLED' | 'DISABLED') => setRoute({ ...route, status: value })}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value='ENABLED'>启用</SelectItem>
                  <SelectItem value='DISABLED'>停用</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            {selectedPublicModelHasRoutes && (
              <label className='flex items-start gap-3 rounded-md border border-amber-500/40 bg-amber-500/5 p-3 text-sm'>
                <Checkbox
                  checked={route.confirmCompatibility}
                  onCheckedChange={(checked) => setRoute({ ...route, confirmCompatibility: checked === true })}
                />
                <span>
                  <span className='block font-medium'>确认这些上游实例可以承载同一个对外模型</span>
                  <span className='text-muted-foreground mt-1 block text-xs'>
                    上游模型名相同不代表智力、上下文、工具调用或输出质量相同。若无法确认，请创建另一个对外模型 ID。
                  </span>
                </span>
              </label>
            )}
          </div>
          <DialogFooter>
            <Button variant='outline' onClick={() => setRouteOpen(false)}>
              取消
            </Button>
            <Button onClick={() => void saveRoute()} disabled={upsertRoute.isPending}>
              保存
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <Dialog open={priceOpen} onOpenChange={setPriceOpen}>
        <DialogContent className='max-h-[90vh] overflow-y-auto sm:max-w-3xl'>
          <DialogHeader>
            <DialogTitle>编辑零售价变更</DialogTitle>
            <DialogDescription>Token 模型按每 1,000,000 Tokens 计价；图片、音频、视频等也可按每次成功请求收取固定价格。</DialogDescription>
          </DialogHeader>
          <div className='grid gap-4'>
            <Field label='对外模型'>
              {priceModelLocked ? (
                <div className='bg-muted/30 rounded-md border px-3 py-2'>
                  <div className='font-mono text-sm font-medium'>
                    {models.find((model) => model.id === price.publicModelID)?.modelID || price.publicModelID}
                  </div>
                  <div className='text-muted-foreground mt-0.5 text-xs'>正在编辑此对外模型的零售价</div>
                </div>
              ) : (
                <Select
                  value={price.publicModelID}
                  onValueChange={async (value) => {
                    try {
                      const existing = basePrices.find((item) => extractNumberID(item.publicModelID) === extractNumberID(value));
                      const savedPrice = editingDraftID ? await readDraftModelPrice(editingDraftID, value) : undefined;
                      setPriceDocument(savedPrice ?? (existing ? existing.price : { items: [] }));
                      setPrice({ ...emptyPrice(), publicModelID: value });
                    } catch (error) {
                      toast.error(error instanceof Error ? error.message : t('eventPackages.conflict'));
                    }
                  }}
                >
                  <SelectTrigger>
                    <SelectValue placeholder='选择模型' />
                  </SelectTrigger>
                  <SelectContent>
                    {models.map((model) => (
                      <SelectItem key={model.id} value={model.id}>
                        {model.name} · {model.modelID}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </Field>
            {pricingGroups.isError && (
              <Alert variant='destructive'>
                <AlertDescription>{pricingGroups.error.message}</AlertDescription>
              </Alert>
            )}
            <GroupRetailPriceEditor
              key={price.publicModelID}
              value={priceDocument}
              onChange={setPriceDocument}
              currency={accountingCurrencyCode}
              groups={pricingGroups.data ?? []}
            />
          </div>
          <DialogFooter>
            <Button variant='outline' onClick={() => setPriceOpen(false)}>
              取消
            </Button>
            <Button
              onClick={() => void savePrice()}
              disabled={!canWriteCommercialization || !!validateEditorDocument(priceDocument) || saveItem.isPending}
            >
              保存变更
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
async function readDraftModelPrice(draftID: string, modelID: string): Promise<unknown | undefined> {
  const key = modelID.split('/').at(-1)!;
  let after: string | undefined;
  do {
    const page = await getItemPage(draftID, after, key);
    const item = page.edges.find(({ node }) => node.itemKey === key)?.node;
    if (item) return (await getItem(draftID, item.id))?.item.afterSnapshot ?? undefined;
    if (!page.pageInfo.hasNextPage) return undefined;
    const cursor = page.pageInfo.endCursor;
    if (!cursor || cursor === after) throw new Error('Invalid draft item pagination');
    after = cursor;
  } while (after);
  return undefined;
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className='grid gap-1.5'>
      <Label>{label}</Label>
      {children}
    </div>
  );
}
