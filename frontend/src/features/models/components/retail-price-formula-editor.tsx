import { useState } from 'react';
import { Link } from '@tanstack/react-router';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Checkbox } from '@/components/ui/checkbox';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Textarea } from '@/components/ui/textarea';
import { priceRows } from '@/features/change-sets/price-view';
import type { ModelRoute } from '../data/commercialization';
import {
  type FormulaPreview,
  type RetailFormula,
  previewRetailFormula,
  useRetailFormulas,
  useSaveRetailFormula,
} from '../data/price-formulas';
import type { Model } from '../data/schema';

type Props = { bookID: string; model: Model; routes: ModelRoute[]; canWrite: boolean };
export function RetailPriceFormulaEditor(props: Props) {
  const formulas = useRetailFormulas(props.bookID);
  if (formulas.isPending) return <p>加载已保存公式…</p>;
  if (formulas.isError) return <p role='alert'>{formulas.error.message}</p>;
  const saved = formulas.data.find((item) => item.publicModelID === props.model.id);
  return <FormulaForm key={`${props.model.id}:${saved?.revision ?? '0'}`} {...props} saved={saved} />;
}
function FormulaForm({ bookID, model, routes, canWrite, saved }: Props & { saved?: RetailFormula }) {
  const save = useSaveRetailFormula();
  const [deploymentID, setDeploymentID] = useState(saved?.deploymentID ?? '');
  const [expression, setExpression] = useState(saved?.expression ?? 'cost * markup');
  const [parameters, setParameters] = useState(JSON.stringify(saved?.parameters ?? { markup: '1.2' }, null, 2));
  const [enabled, setEnabled] = useState(saved?.enabled ?? true);
  const [preview, setPreview] = useState<FormulaPreview | null>(null);
  const [previewing, setPreviewing] = useState(false);
  const available = routes.filter((route) => route.publicModelID === model.id && route.status === 'ENABLED');
  const clearPreview = () => setPreview(null);
  const input = () => {
    if (!deploymentID) throw new Error('请选择明确的采购价来源');
    const values: unknown = JSON.parse(parameters);
    if (!values || typeof values !== 'object' || Array.isArray(values) || Object.values(values).some((value) => typeof value !== 'string'))
      throw new Error('常量参数必须是 JSON 对象，数值使用十进制字符串');
    return {
      priceBookID: bookID,
      publicModelID: model.id,
      deploymentID,
      expression,
      parameters: values as Record<string, string>,
      enabled,
      expectedRevision: saved?.revision ?? '0',
    };
  };
  const previewPrice = async () => {
    try {
      setPreviewing(true);
      setPreview(await previewRetailFormula(input()));
    } catch (error) {
      setPreview(null);
      toast.error(error instanceof Error ? error.message : '预览失败');
    } finally {
      setPreviewing(false);
    }
  };
  const saveFormula = async () => {
    try {
      const result = await save.mutateAsync(input());
      toast.success(result.enabled ? '公式已保存；自动生成的零售价草稿需发布后生效' : '公式已停用；最后有效价格保持不变');
      setPreview(null);
    } catch (error) {
      toast.error(error instanceof Error ? error.message : '保存失败');
    }
  };
  return (
    <div className='grid gap-4'>
      <div className='rounded-md border p-3 text-sm'>
        <div className='font-medium'>{model.name}</div>
        <div className='text-muted-foreground font-mono text-xs'>{model.modelID}</div>
        <p className='text-muted-foreground mt-2'>
          公式生成模型基础零售价，项目有效倍率在收费时再乘一次。正式采购价变化自动生成草稿，沿用现有审核和发布规则。
        </p>
      </div>
      <div className='grid gap-2'>
        <Label htmlFor='formula-source'>采购价来源</Label>
        <Select
          value={deploymentID}
          onValueChange={(value) => {
            setDeploymentID(value);
            clearPreview();
          }}
          disabled={!canWrite || previewing || save.isPending}
        >
          <SelectTrigger id='formula-source'>
            <SelectValue placeholder='选择已启用映射的来源渠道' />
          </SelectTrigger>
          <SelectContent>
            {deploymentID && !available.some((route) => route.deploymentID === deploymentID) && (
              <SelectItem value={deploymentID}>原来源不可用 · {deploymentID}</SelectItem>
            )}
            {available.map((route) => (
              <SelectItem key={route.id} value={route.deploymentID}>
                {route.channelName} / {route.upstreamModelID}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
      <div className='grid gap-2'>
        <Label htmlFor='formula-expression'>价格公式</Label>
        <Input
          id='formula-expression'
          value={expression}
          onChange={(event) => {
            setExpression(event.target.value);
            clearPreview();
          }}
          disabled={!canWrite || previewing || save.isPending}
          maxLength={1024}
        />
        <p className='text-muted-foreground text-xs'>
          支持 cost、命名常量、括号、+ - * /。cost 是该项正式采购价：Token 项按每百万
          Tokens，按次项按每次请求；缓存变体和阶梯分别计算。结果最多 12 位小数，四舍五入。
        </p>
      </div>
      <div className='grid gap-2'>
        <Label htmlFor='formula-parameters'>常量参数（十进制字符串）</Label>
        <Textarea
          id='formula-parameters'
          value={parameters}
          onChange={(event) => {
            setParameters(event.target.value);
            clearPreview();
          }}
          className='font-mono'
          disabled={!canWrite || previewing || save.isPending}
        />
      </div>
      <label className='flex items-center gap-2 text-sm'>
        <Checkbox
          checked={enabled}
          onCheckedChange={(value) => {
            setEnabled(value === true);
            clearPreview();
          }}
          disabled={!canWrite || previewing || save.isPending}
        />
        启用自动生成草稿
      </label>
      {saved && (
        <div className='rounded-md border p-3 text-sm' role='status'>
          <div>
            状态：{saved.status} · 版本 {saved.revision}
          </div>
          {saved.lastError && <p className='text-destructive mt-1'>{saved.lastError}</p>}
          {saved.lastChangeSetID && (
            <Link to='/change-sets/$changeSetId' params={{ changeSetId: saved.lastChangeSetID }} className='mt-2 inline-block underline'>
              查看最近生成的价格包
            </Link>
          )}
        </div>
      )}
      {preview && (
        <div className='max-h-56 overflow-auto rounded-md border p-3 text-sm' aria-label='服务器价格预览'>
          <div className='mb-2 font-medium'>服务器预览 · {preview.currency}</div>
          {priceRows(preview.price).map((row, index) => (
            <div key={index} className='mb-2'>
              <span className='font-mono'>
                {row.code}
                {row.variantCode ? ` / ${row.variantCode}` : ''}
              </span>
              ：
              {row.tiers.length
                ? row.tiers.map((tier, tierIndex) => (
                    <span key={tierIndex} className='mr-2'>
                      ≤ {String(tier.upTo ?? '∞')}：{String(tier.amount)}
                    </span>
                  ))
                : String(row.amount ?? '缺价')}
            </div>
          ))}
          <p className='text-muted-foreground text-xs'>保存和发布会重新核对来源；预览与生成使用相同服务器算法。</p>
        </div>
      )}
      <div className='flex justify-end gap-2'>
        <Button variant='outline' onClick={() => void previewPrice()} disabled={previewing || !deploymentID}>
          预览
        </Button>
        <Button onClick={() => void saveFormula()} disabled={!canWrite || previewing || save.isPending}>
          保存公式
        </Button>
      </div>
    </div>
  );
}
