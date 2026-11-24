import { IconPlus, IconTemplate } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { usePermissions } from '@/hooks/usePermissions';
import { Button } from '@/components/ui/button';
import { useApiKeysContext } from '../context/apikeys-context';

export function ApiKeysPrimaryButtons() {
  const { t } = useTranslation();
  const { openDialog } = useApiKeysContext();
  const { apiKeyPermissions } = usePermissions();

  // 没有写权限时不显示新建入口，避免点了才报错
  if (!apiKeyPermissions.canWrite) return null;

  return (
    <div className='flex gap-2'>
      <Button variant='outline' size='sm' onClick={() => openDialog('profileTemplates')}>
        <IconTemplate className='mr-2 h-4 w-4' />
        {t('apikeys.profileTemplates.button')}
      </Button>
      <Button onClick={() => openDialog('create')} size='sm'>
        <IconPlus className='mr-2 h-4 w-4' />
        {t('apikeys.createApiKey')}
      </Button>
    </div>
  );
}
