import { useCallback, useEffect, useState, type ReactNode } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { IconRocket } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { DEFAULT_ACCOUNTING_CURRENCY_CODE } from '@/lib/accounting';
import { usePermissions } from '@/hooks/usePermissions';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import {
  useChangeSets,
  useCreateRetailPriceChangeSet,
  useSaveRetailPriceChangeSetItem,
  useSubmitChangeSet,
} from '@/features/change-sets/data/change-sets';
import { useGeneralSettings } from '@/features/system/data/system';
import { type ModelRoute, useCommercializationCatalog, useCreatePriceBook, useUpsertModelRoute } from '../data/commercialization';
import type { Model } from '../data/schema';

export type CommercializationAction =
  { kind: 'add-route'; publicModelID: string } | { kind: 'edit-route'; route: ModelRoute } | { kind: 'price'; publicModelID: string };

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
type PriceForm = {
  publicModelID: string;
  mode: 'tokens' | 'request';
  prompt: string;
  completion: string;
  cacheRead: string;
  cacheWrite: string;
  flatFee: string;
};

const emptyRoute = (): RouteForm => ({
  publicModelID: '',
  deploymentID: '',
  status: 'ENABLED',
  confirmCompatibility: false,
});
const emptyPrice = (): PriceForm => ({
  publicModelID: '',
  mode: 'tokens',
  prompt: '',
  completion: '',
  cacheRead: '',
  cacheWrite: '',
  flatFee: '',
});
const DECIMAL_PATTERN = /^\d+(?:\.\d{1,12})?$/;

function itemPrices(item?: {
  price: { items: Array<{ itemCode: string; pricing: { mode: string; usagePerUnit?: string; flatFee?: string } }> };
}) {
  const find = (code: string) => item?.price.items.find((entry) => entry.itemCode === code)?.pricing.usagePerUnit || '';
  const flatFee = item?.price.items.find((entry) => entry.pricing.mode === 'flat_fee')?.pricing.flatFee || '';
  return {
    mode: flatFee ? ('request' as const) : ('tokens' as const),
    prompt: find('prompt_tokens'),
    completion: find('completion_tokens'),
    cacheRead: find('prompt_cached_tokens'),
    cacheWrite: find('prompt_write_cached_tokens'),
    flatFee,
  };
}

export function CommercializationPanel({ models, action, onActionHandled }: Props) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const { hasSystemScope } = usePermissions();
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
  const [editingDraftID, setEditingDraftID] = useState('');
  const [priceModelLocked, setPriceModelLocked] = useState(false);
  const [route, setRoute] = useState<RouteForm>(emptyRoute());
  const [price, setPrice] = useState<PriceForm>(emptyPrice());

  // Settlement only reads the explicitly designated default book. Falling
  // back to an arbitrary book here would let the UI edit prices that runtime
  // billing can never see.
  const defaultBook = catalog.data?.priceBooks.find((book) => book.isDefault);
  const retailChangeSets = useChangeSets({
    kind: 'RETAIL_PRICE',
    scopeType: 'price_book',
    scopeID: defaultBook?.id,
    enabled: Boolean(defaultBook),
  });
  const retailChangeSet = retailChangeSets.data?.find((item) => item.status === 'DRAFT' || item.status === 'PENDING_REVIEW');
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
        const editable = await createRetailChangeSet.mutateAsync(book.id);
        const changed = editable.items.find((item) => item.itemKey === publicModelID)?.afterSnapshot as
          { items: Array<{ itemCode: string; pricing: { mode: string; usagePerUnit?: string; flatFee?: string } }> } | undefined;
        const published = book.versions
          .find((version) => version.status === 'published')
          ?.items.find((item) => item.publicModelID === publicModelID);
        const existing = changed ? { price: changed } : published;
        setEditingDraftID(editable.id);
        setPriceModelLocked(Boolean(publicModelID));
        setPrice({ publicModelID, ...itemPrices(existing) });
        setPriceOpen(true);
      } catch (error) {
        toast.error(error instanceof Error ? error.message : '无法准备价格编辑器');
      }
    },
    [accountingCurrencyCode, catalog, createBook, createRetailChangeSet, defaultBook]
  );

  useEffect(() => {
    if (!action) return;
    if (action.kind === 'add-route') {
      setRoute({ ...emptyRoute(), publicModelID: action.publicModelID });
      setRouteOpen(true);
    } else if (action.kind === 'edit-route') {
      editRoute(action.route);
    } else {
      void openPriceEditor(action.publicModelID);
    }
    onActionHandled();
  }, [action, editRoute, onActionHandled, openPriceEditor]);

  const savePrice = async () => {
    if (!editingDraftID || !price.publicModelID) return toast.error('请选择模型');
    const values =
      price.mode === 'request' ? [price.flatFee] : [price.prompt, price.completion, price.cacheRead, price.cacheWrite].filter(Boolean);
    if (!values.length || values.some((value) => !DECIMAL_PATTERN.test(value)))
      return toast.error(price.mode === 'request' ? '请填写有效的单次价格，最多 12 位小数' : '至少填写一项有效非负价格，最多 12 位小数');
    const items =
      price.mode === 'request'
        ? [{ itemCode: 'request', pricing: { mode: 'flat_fee', flatFee: price.flatFee } }]
        : [
            ...(price.prompt ? [{ itemCode: 'prompt_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: price.prompt } }] : []),
            ...(price.completion
              ? [{ itemCode: 'completion_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: price.completion } }]
              : []),
            ...(price.cacheRead
              ? [{ itemCode: 'prompt_cached_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: price.cacheRead } }]
              : []),
            ...(price.cacheWrite
              ? [{ itemCode: 'prompt_write_cached_tokens', pricing: { mode: 'usage_per_unit', usagePerUnit: price.cacheWrite } }]
              : []),
          ];
    try {
      await saveItem.mutateAsync({ changeSetID: editingDraftID, publicModelID: price.publicModelID, price: { items } });
      toast.success('零售价变更已保存');
      setPriceOpen(false);
      setPrice(emptyPrice());
    } catch (error) {
      toast.error(error instanceof Error ? error.message : '保存价格失败');
    }
  };

  const submitRetailChangeSet = async () => {
    if (!retailChangeSet || retailChangeSet.status !== 'DRAFT') return;
    try {
      await submitChangeSet.mutateAsync(retailChangeSet.id);
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
      {retailChangeSet && (
        <div className='bg-card flex flex-col gap-2 rounded-lg border border-dashed p-3 sm:flex-row sm:items-center sm:justify-between'>
          <div>
            <div className='text-sm font-medium'>零售价变更 · {retailChangeSet.status === 'DRAFT' ? '编辑中' : '待审核'}</div>
            <div className='text-muted-foreground text-xs'>{retailChangeSet.items.length} 个模型已定价；批准前不会用于应收计算。</div>
          </div>
          <Button
            onClick={() => (retailChangeSet.status === 'DRAFT' ? void submitRetailChangeSet() : openRetailReview())}
            disabled={
              retailChangeSet.status === 'DRAFT' &&
              (!canWriteCommercialization || !retailChangeSet.items.length || submitChangeSet.isPending)
            }
          >
            <IconRocket className='size-4' />
            {retailChangeSet.status === 'DRAFT' ? '提交审核' : '前往审批工作台'}
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
                      {model.modelID}
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
        <DialogContent>
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
                <Select value={price.publicModelID} onValueChange={(value) => setPrice({ ...price, publicModelID: value })}>
                  <SelectTrigger>
                    <SelectValue placeholder='选择模型' />
                  </SelectTrigger>
                  <SelectContent>
                    {models.map((model) => (
                      <SelectItem key={model.id} value={model.id}>
                        {model.modelID}
                      </SelectItem>
                    ))}
                  </SelectContent>
                </Select>
              )}
            </Field>
            <Field label='计费方式'>
              <Select value={price.mode} onValueChange={(mode: 'tokens' | 'request') => setPrice({ ...price, mode })}>
                <SelectTrigger>
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value='tokens'>按 Token 用量</SelectItem>
                  <SelectItem value='request'>按成功请求次数</SelectItem>
                </SelectContent>
              </Select>
            </Field>
            {price.mode === 'tokens' ? (
              <div className='grid gap-4 sm:grid-cols-2'>
                <Field label={t('models.catalog.retailEditorInput', { currency: accountingCurrencyCode })}>
                  <Input
                    value={price.prompt}
                    onChange={(event) => setPrice({ ...price, prompt: event.target.value })}
                    placeholder='例如 2.50'
                    inputMode='decimal'
                  />
                </Field>
                <Field label={t('models.catalog.retailEditorOutput', { currency: accountingCurrencyCode })}>
                  <Input
                    value={price.completion}
                    onChange={(event) => setPrice({ ...price, completion: event.target.value })}
                    placeholder='例如 10.00'
                    inputMode='decimal'
                  />
                </Field>
                <Field label={t('models.catalog.retailEditorCacheRead', { currency: accountingCurrencyCode })}>
                  <Input
                    value={price.cacheRead}
                    onChange={(event) => setPrice({ ...price, cacheRead: event.target.value })}
                    placeholder='例如 0.25'
                    inputMode='decimal'
                  />
                </Field>
                <Field label={t('models.catalog.retailEditorCacheWrite', { currency: accountingCurrencyCode })}>
                  <Input
                    value={price.cacheWrite}
                    onChange={(event) => setPrice({ ...price, cacheWrite: event.target.value })}
                    placeholder='例如 3.00'
                    inputMode='decimal'
                  />
                </Field>
              </div>
            ) : (
              <Field label={`单次价格 (${accountingCurrencyCode} / 次)`}>
                <Input
                  value={price.flatFee}
                  onChange={(event) => setPrice({ ...price, flatFee: event.target.value })}
                  placeholder='例如 0.05'
                />
              </Field>
            )}
          </div>
          <DialogFooter>
            <Button variant='outline' onClick={() => setPriceOpen(false)}>
              取消
            </Button>
            <Button onClick={() => void savePrice()} disabled={saveItem.isPending}>
              保存变更
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className='grid gap-1.5'>
      <Label>{label}</Label>
      {children}
    </div>
  );
}
