import { useEffect, useState } from 'react';
import { useForm } from 'react-hook-form';
import { zodResolver } from '@hookform/resolvers/zod';
import { IconEye, IconEyeOff } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Form, FormControl, FormField, FormItem, FormLabel, FormMessage } from '@/components/ui/form';
import { Input } from '@/components/ui/input';
import { ScopesSelect } from '@/components/scopes-select';
import { useApiKeysContext } from '../context/apikeys-context';
import { useAssignableModelGroups, useAssignApiKeyModelGroup, useUpdateApiKey } from '../data/apikeys';
import { UpdateApiKeyInput, updateApiKeyInputSchemaFactory } from '../data/schema';

export function ApiKeysEditDialog() {
  const { t } = useTranslation();
  const { isDialogOpen, closeDialog, selectedApiKey } = useApiKeysContext();
  const updateApiKey = useUpdateApiKey();
  const groups = useAssignableModelGroups(selectedApiKey?.id);
  const assign = useAssignApiKeyModelGroup();
  const [groupID, setGroupID] = useState('');
  const [isSubmitting, setIsSubmitting] = useState(false);
  const [showApiKey, setShowApiKey] = useState(false);
  const [dialogContent, setDialogContent] = useState<HTMLDivElement | null>(null);

  const form = useForm<UpdateApiKeyInput>({
    resolver: zodResolver(updateApiKeyInputSchemaFactory(t)),
    defaultValues: {
      name: '',
      scopes: [],
    },
  });

  useEffect(() => {
    if (selectedApiKey && isDialogOpen.edit) {
      setGroupID(selectedApiKey.modelGroupID || '');
      form.reset({
        name: selectedApiKey.name,
        scopes: selectedApiKey.scopes || [],
      });
    }
  }, [selectedApiKey, isDialogOpen.edit, form]);

  const onSubmit = async (data: UpdateApiKeyInput) => {
    if (!selectedApiKey) return;

    setIsSubmitting(true);
    try {
      const input: UpdateApiKeyInput = {
        name: data.name,
      };

      if (selectedApiKey.type === 'service_account') {
        input.scopes = data.scopes;
      }

      await updateApiKey.mutateAsync({
        id: selectedApiKey.id,
        input,
      });

      if (groupID && groupID !== selectedApiKey.modelGroupID) await assign.mutateAsync({ id: selectedApiKey.id, modelGroupID: groupID });
      closeDialog('edit');
    } catch (_error) {
      // Error is handled by the mutation.
    } finally {
      setIsSubmitting(false);
    }
  };

  const handleClose = () => {
    form.reset();
    closeDialog('edit');
  };

  const isServiceAccount = selectedApiKey?.type === 'service_account';

  return (
    <Dialog open={isDialogOpen.edit} onOpenChange={handleClose}>
      <DialogContent className='flex max-h-[90vh] flex-col sm:max-w-[600px]' ref={setDialogContent}>
        <DialogHeader>
          <DialogTitle>{t('apikeys.dialogs.edit.title')}</DialogTitle>
          <DialogDescription>{t('apikeys.dialogs.edit.description')}</DialogDescription>
        </DialogHeader>
        <Form {...form}>
          <form onSubmit={form.handleSubmit(onSubmit)} className='space-y-4'>
            <label className='block'>
              所属模型组
              <select className='mt-1 block w-full rounded border p-2' value={groupID} onChange={(e) => setGroupID(e.target.value)}>
                <option value=''>{selectedApiKey?.modelGroupID || '待配置（公开调用不可用）'}</option>
                {groups.data?.map((g) => (
                  <option key={g.id} value={g.id}>
                    {g.name}
                  </option>
                ))}
              </select>
              <span className='text-muted-foreground text-sm'>保存时服务端按 Key 实际计费用户校验授权。</span>
            </label>
            <FormField
              control={form.control}
              name='name'
              render={({ field }) => (
                <FormItem>
                  <FormLabel>{t('apikeys.dialogs.fields.name.label')}</FormLabel>
                  <FormControl>
                    <Input placeholder={t('apikeys.dialogs.fields.name.placeholder')} {...field} />
                  </FormControl>
                  <FormMessage />
                </FormItem>
              )}
            />
            {isServiceAccount && (
              <FormField
                control={form.control}
                name='scopes'
                render={({ field }) => (
                  <FormItem>
                    <FormLabel>{t('apikeys.dialogs.fields.scopes.label')}</FormLabel>
                    <FormControl>
                      <ScopesSelect
                        value={field.value || []}
                        onChange={field.onChange}
                        portalContainer={dialogContent}
                        level='system'
                        enablePermissionFilter
                      />
                    </FormControl>
                    <FormMessage />
                  </FormItem>
                )}
              />
            )}
            <div className='space-y-4'>
              <div>
                <div className='flex items-center justify-between'>
                  <label className='text-muted-foreground text-sm font-medium'>{t('apikeys.dialogs.fields.key.label')}</label>
                  <Button type='button' variant='ghost' size='sm' onClick={() => setShowApiKey(!showApiKey)} className='h-6 px-2'>
                    {showApiKey ? <IconEyeOff className='h-3 w-3' /> : <IconEye className='h-3 w-3' />}
                    <span className='ml-1 text-xs'>{showApiKey ? t('apikeys.actions.hide') : t('apikeys.actions.show')}</span>
                  </Button>
                </div>
                <p className='text-foreground mt-1 font-mono text-sm break-all'>
                  {showApiKey ? selectedApiKey?.key : '••••••••••••••••••••••••••••••••'}
                </p>
              </div>
            </div>
            <DialogFooter className='flex-col items-stretch gap-2 sm:flex-row sm:items-center sm:justify-end'>
              <div className='flex w-full gap-2 sm:w-auto'>
                <Button type='button' variant='outline' onClick={handleClose} disabled={isSubmitting}>
                  {t('common.buttons.cancel')}
                </Button>
                <Button type='submit' disabled={isSubmitting}>
                  {isSubmitting ? t('common.buttons.saving') : t('common.buttons.save')}
                </Button>
              </div>
            </DialogFooter>
          </form>
        </Form>
      </DialogContent>
    </Dialog>
  );
}
