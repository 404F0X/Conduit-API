import { IconAlertTriangle, IconTemplate } from '@tabler/icons-react';
import { useAllScopes } from '@/gql/scopes';
import { useTranslation } from 'react-i18next';
import { useAuthStore } from '@/stores/authStore';
import { filterGrantableScopes } from '@/lib/permission-utils';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { systemRoleTemplates } from '../data/templates';

type Props = {
  onApply: (name: string, scopes: string[]) => void;
};

export function SystemRoleTemplatePicker({ onApply }: Props) {
  const { t } = useTranslation();
  const currentUser = useAuthStore((state) => state.auth.user);
  const scopesQuery = useAllScopes('system');
  const catalogScopes = scopesQuery.data?.map((scope) => scope.scope) || [];
  // A selected Project's owner flag must never widen system-level grants.
  const grantableScopes = new Set(filterGrantableScopes(currentUser, catalogScopes));

  return (
    <div className='space-y-3'>
      <div className='flex items-start gap-2'>
        <IconTemplate className='text-muted-foreground mt-0.5 size-4 shrink-0' />
        <div>
          <div className='text-sm font-medium'>{t('roles.templates.title')}</div>
          <p className='text-muted-foreground text-xs'>{t('roles.templates.description')}</p>
        </div>
      </div>
      <div className='grid gap-2 sm:grid-cols-2'>
        {systemRoleTemplates.map((template) => {
          const canApply = !scopesQuery.isLoading && template.scopes.every((scope) => grantableScopes.has(scope));
          return (
            <Button
              key={template.id}
              type='button'
              variant='outline'
              className='h-auto min-h-24 items-start justify-start px-3 py-3 text-left whitespace-normal'
              disabled={!canApply}
              title={!canApply ? t('roles.templates.insufficientPermissions') : undefined}
              onClick={() => onApply(t(template.nameKey), [...template.scopes])}
            >
              <span className='flex w-full flex-col gap-1.5'>
                <span className='flex w-full items-center justify-between gap-2'>
                  <span className='font-medium'>{t(template.nameKey)}</span>
                  {template.highRisk && (
                    <Badge variant='destructive' className='shrink-0 gap-1'>
                      <IconAlertTriangle className='size-3' />
                      {t('roles.templates.highRisk')}
                    </Badge>
                  )}
                </span>
                <span className='text-muted-foreground text-xs leading-relaxed'>{t(template.descriptionKey)}</span>
                <span className='text-muted-foreground text-[11px]'>
                  {t('roles.templates.scopeCount', { count: template.scopes.length })}
                </span>
              </span>
            </Button>
          );
        })}
      </div>
      <p className='text-muted-foreground text-xs'>{t('roles.templates.noAutomaticChanges')}</p>
    </div>
  );
}
