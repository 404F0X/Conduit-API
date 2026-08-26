'use client';

import { useEffect, useState } from 'react';
import { Plus } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Label } from '@/components/ui/label';
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { Textarea } from '@/components/ui/textarea';
import { useUpdateChannel } from '../data/channels';
import {
  autoModelMappingRuleSchema,
  type AutoModelMappingRule,
  type Channel,
  errorResponseRewriteRuleSchema,
  type ErrorResponseRewriteRule,
} from '../data/schema';
import { mergeChannelSettingsForUpdate } from '../utils/merge';

interface Props {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  currentRow: Channel;
}

function formatted(value: unknown[] | null | undefined): string {
  return JSON.stringify(value ?? [], null, 2);
}

function appendRule(raw: string, rule: unknown): string | null {
  try {
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return null;
    return formatted([...parsed, rule]);
  } catch {
    return null;
  }
}

export function ChannelsAutomationDialog({ open, onOpenChange, currentRow }: Props) {
  const { t } = useTranslation();
  const updateChannel = useUpdateChannel();
  const [modelRules, setModelRules] = useState('[]');
  const [errorRules, setErrorRules] = useState('[]');

  useEffect(() => {
    if (!open) return;
    setModelRules(formatted(currentRow.settings?.autoModelMappingRules));
    setErrorRules(formatted(currentRow.settings?.errorResponseRewriteRules));
  }, [currentRow, open]);

  const addModelRule = () => {
    const next = appendRule(modelRules, {
      pattern: '^provider/(.+)$',
      publicModelIdTemplate: '$1',
      createDraft: false,
      developerTemplate: '',
      nameTemplate: '$1',
      groupTemplate: '',
      modelType: 'chat',
    });
    if (next === null) {
      toast.error(t('channels.dialogs.automation.invalidJson'));
      return;
    }
    setModelRules(next);
  };

  const addErrorRule = () => {
    const next = appendRule(errorRules, {
      statusCodes: [429],
      bodyPattern: '',
      httpStatus: 503,
      message: 'Upstream service is temporarily unavailable',
      errorType: 'service_unavailable',
      code: 'channel_unavailable',
    });
    if (next === null) {
      toast.error(t('channels.dialogs.automation.invalidJson'));
      return;
    }
    setErrorRules(next);
  };

  const save = async () => {
    let parsedModelRules: AutoModelMappingRule[];
    let parsedErrorRules: ErrorResponseRewriteRule[];
    try {
      parsedModelRules = autoModelMappingRuleSchema.array().parse(JSON.parse(modelRules));
      parsedErrorRules = errorResponseRewriteRuleSchema.array().parse(JSON.parse(errorRules));
    } catch {
      toast.error(t('channels.dialogs.automation.invalidJson'));
      return;
    }

    try {
      await updateChannel.mutateAsync({
        id: currentRow.id,
        input: {
          settings: mergeChannelSettingsForUpdate(currentRow.settings, {
            autoModelMappingRules: parsedModelRules,
            errorResponseRewriteRules: parsedErrorRules,
          }),
        },
      });
      toast.success(t('channels.messages.updateSuccess'));
      onOpenChange(false);
    } catch {
      toast.error(t('common.errors.internalServerError'));
    }
  };

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='max-h-[90vh] overflow-y-auto sm:max-w-3xl'>
        <DialogHeader className='text-left'>
          <DialogTitle>{t('channels.dialogs.automation.title')}</DialogTitle>
          <DialogDescription>{t('channels.dialogs.automation.description', { name: currentRow.name })}</DialogDescription>
        </DialogHeader>

        <Tabs defaultValue='models'>
          <TabsList className='grid w-full grid-cols-2'>
            <TabsTrigger value='models'>{t('channels.dialogs.automation.modelsTab')}</TabsTrigger>
            <TabsTrigger value='errors'>{t('channels.dialogs.automation.errorsTab')}</TabsTrigger>
          </TabsList>
          <TabsContent value='models' className='space-y-2 pt-3'>
            <div className='flex items-center justify-between gap-3'>
              <Label htmlFor='auto-model-mapping-rules'>{t('channels.dialogs.automation.modelsLabel')}</Label>
              <Button type='button' variant='outline' size='sm' onClick={addModelRule}>
                <Plus className='size-4' />
                {t('channels.dialogs.automation.addRule')}
              </Button>
            </div>
            <Textarea
              id='auto-model-mapping-rules'
              className='min-h-[360px] resize-y font-mono text-xs leading-5'
              value={modelRules}
              onChange={(event) => setModelRules(event.target.value)}
              spellCheck={false}
            />
          </TabsContent>
          <TabsContent value='errors' className='space-y-2 pt-3'>
            <div className='flex items-center justify-between gap-3'>
              <Label htmlFor='error-response-rewrite-rules'>{t('channels.dialogs.automation.errorsLabel')}</Label>
              <Button type='button' variant='outline' size='sm' onClick={addErrorRule}>
                <Plus className='size-4' />
                {t('channels.dialogs.automation.addRule')}
              </Button>
            </div>
            <Textarea
              id='error-response-rewrite-rules'
              className='min-h-[360px] resize-y font-mono text-xs leading-5'
              value={errorRules}
              onChange={(event) => setErrorRules(event.target.value)}
              spellCheck={false}
            />
          </TabsContent>
        </Tabs>

        <DialogFooter>
          <Button type='button' variant='outline' onClick={() => onOpenChange(false)}>
            {t('common.buttons.cancel')}
          </Button>
          <Button type='button' onClick={save} disabled={updateChannel.isPending}>
            {updateChannel.isPending ? t('common.buttons.saving') : t('common.buttons.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
