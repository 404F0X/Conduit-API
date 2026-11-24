import { useEffect, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select';
import { Textarea } from '@/components/ui/textarea';
import { useCreateChannel, useFetchModels } from '../data/channels';
import { getDefaultBaseURL, getDefaultModels } from '../data/config_channels';
import { channelTypeSchema, type ChannelType } from '../data/schema';

/** 需要 OAuth、云厂商凭据或测试用的类型不进快速新建，走完整表单。 */
const FULL_FORM_ONLY: ChannelType[] = [
  'codex',
  'claudecode',
  'antigravity',
  'github_copilot',
  'anthropic_aws',
  'anthropic_gcp',
  'gemini_vertex',
  'anthropic_fake',
  'openai_fake',
];
const QUICK_TYPES = channelTypeSchema.options.filter((type) => !FULL_FORM_ONLY.includes(type));

/**
 * 新建渠道（快速）：只问四件事——类型、名称、Base URL、API Key。
 * 支持的模型自动从上游拉取，拉不到就用这个类型的默认模型；默认测试模型取第一个。
 * 其余配置（标签、权重、代理、覆盖参数…）建好后在渠道详情页里改。
 */
export function ChannelQuickCreateDialog({
  open,
  onOpenChange,
  onUseFullForm,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  onUseFullForm: () => void;
}) {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const createChannel = useCreateChannel();
  const fetchModels = useFetchModels();
  const [type, setType] = useState<ChannelType>('openai');
  const [name, setName] = useState('');
  const [baseURL, setBaseURL] = useState(getDefaultBaseURL('openai'));
  const [apiKeys, setApiKeys] = useState('');
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    setType('openai');
    setName('');
    setBaseURL(getDefaultBaseURL('openai'));
    setApiKeys('');
    setError(null);
  }, [open]);

  const changeType = (next: ChannelType) => {
    // 只在用户没改过地址时跟着类型换默认地址
    if (!baseURL || baseURL === getDefaultBaseURL(type)) setBaseURL(getDefaultBaseURL(next));
    setType(next);
  };

  const keys = apiKeys
    .split('\n')
    .map((key) => key.trim())
    .filter(Boolean);
  const urlValid = /^https?:\/\/\S+$/i.test(baseURL.trim());
  const busy = createChannel.isPending || fetchModels.isPending;
  const canSubmit = !!name.trim() && urlValid && keys.length > 0 && !busy;

  const submit = async () => {
    if (!canSubmit) return;
    setError(null);
    let models: string[] = [];
    try {
      const fetched = await fetchModels.mutateAsync({ channelType: type, baseURL: baseURL.trim(), apiKey: keys[0] });
      if (!fetched.error) models = fetched.models.map((model) => model.id);
    } catch {
      // 拉不到上游模型列表时退回到默认模型，下面统一处理
    }
    if (models.length === 0) models = getDefaultModels(type);
    if (models.length === 0) {
      setError(t('channels.quickCreate.noModels'));
      return;
    }
    try {
      const channel = await createChannel.mutateAsync({
        type,
        name: name.trim(),
        baseURL: baseURL.trim(),
        quotaCurrency: 'USD',
        supportedModels: models,
        autoSyncSupportedModels: false,
        autoSyncModelPattern: '',
        tags: [],
        defaultTestModel: models[0],
        credentials: { apiKeys: keys },
      });
      onOpenChange(false);
      navigate({ to: '/channels/$channelId', params: { channelId: channel.id } });
    } catch {
      // useCreateChannel 已经提示了错误
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='sm:max-w-lg'>
        <DialogHeader>
          <DialogTitle>{t('channels.quickCreate.title')}</DialogTitle>
          <DialogDescription>{t('channels.quickCreate.description')}</DialogDescription>
        </DialogHeader>
        <div className='space-y-4'>
          <div className='space-y-2'>
            <Label>{t('channels.quickCreate.type')}</Label>
            <Select value={type} onValueChange={(value) => changeType(value as ChannelType)}>
              <SelectTrigger>
                <SelectValue />
              </SelectTrigger>
              <SelectContent className='max-h-72'>
                {QUICK_TYPES.map((option) => (
                  <SelectItem key={option} value={option}>
                    {t(`channels.types.${option}`)}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
          <div className='space-y-2'>
            <Label htmlFor='quick-channel-name'>{t('channels.quickCreate.name')}</Label>
            <Input
              id='quick-channel-name'
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder={t('channels.quickCreate.namePlaceholder')}
            />
          </div>
          <div className='space-y-2'>
            <Label htmlFor='quick-channel-url'>Base URL</Label>
            <Input id='quick-channel-url' value={baseURL} onChange={(e) => setBaseURL(e.target.value)} className='font-mono text-sm' />
            {!urlValid && baseURL && <p className='text-destructive text-xs'>{t('channels.quickCreate.urlInvalid')}</p>}
          </div>
          <div className='space-y-2'>
            <Label htmlFor='quick-channel-keys'>API Key</Label>
            <Textarea
              id='quick-channel-keys'
              value={apiKeys}
              onChange={(e) => setApiKeys(e.target.value)}
              rows={3}
              className='font-mono text-sm'
              placeholder={t('channels.quickCreate.keysPlaceholder')}
            />
            <p className='text-muted-foreground text-xs'>{t('channels.quickCreate.keysHint')}</p>
          </div>
          {error && <p className='text-destructive text-sm'>{error}</p>}
        </div>
        <DialogFooter className='gap-2 sm:justify-between'>
          <Button variant='link' className='px-0' onClick={onUseFullForm}>
            {t('channels.quickCreate.useFullForm')}
          </Button>
          <div className='flex gap-2'>
            <Button variant='outline' onClick={() => onOpenChange(false)}>
              {t('common.buttons.cancel')}
            </Button>
            <Button onClick={() => void submit()} disabled={!canSubmit}>
              {busy ? t('channels.quickCreate.creating') : t('common.buttons.create')}
            </Button>
          </div>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
