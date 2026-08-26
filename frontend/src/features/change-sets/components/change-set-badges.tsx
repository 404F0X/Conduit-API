import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { Badge } from '@/components/ui/badge';
import type { ChangeSetKind, ChangeSetStatus } from '../data/change-sets';

const statusClasses: Record<ChangeSetStatus, string> = {
  DRAFT: 'border-border bg-muted/60 text-foreground',
  PENDING_REVIEW: 'border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300',
  APPLIED: 'border-emerald-500/40 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300',
  REJECTED: 'border-red-500/35 bg-red-500/10 text-red-700 dark:text-red-300',
  SUPERSEDED: 'border-border bg-muted/60 text-muted-foreground',
  INVALID: 'border-red-500/40 bg-red-500/10 text-red-700 dark:text-red-300',
};

export function ChangeSetStatusBadge({ status, className }: { status: ChangeSetStatus; className?: string }) {
  const { t } = useTranslation();

  return (
    <Badge variant='outline' className={cn('max-w-full rounded-md font-medium', statusClasses[status], className)}>
      <span className='truncate'>{t(`changeSets.status.${status}`)}</span>
    </Badge>
  );
}

export function ChangeSetKindBadge({ kind, className }: { kind: ChangeSetKind; className?: string }) {
  const { t } = useTranslation();

  return (
    <Badge variant='secondary' className={cn('max-w-full rounded-md font-medium', className)}>
      <span className='truncate'>{t(`changeSets.kind.${kind}`)}</span>
    </Badge>
  );
}
