import { Check, Send, X } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import type { ChangeSet } from '../data/change-sets';

export type ChangeSetActionType = 'submit' | 'approve' | 'reject';

type Props = {
  changeSet: ChangeSet;
  onAction: (action: ChangeSetActionType, changeSet: ChangeSet) => void;
  className?: string;
};

export function ChangeSetActions({ changeSet, onAction, className }: Props) {
  const { t } = useTranslation();

  if (changeSet.status === 'DRAFT') {
    return (
      <div className={className}>
        <Button size='sm' onClick={() => onAction('submit', changeSet)}>
          <Send className='size-4' />
          {t('changeSets.actions.submit')}
        </Button>
      </div>
    );
  }

  if (changeSet.status === 'PENDING_REVIEW') {
    return (
      <div className={className}>
        <Button size='sm' onClick={() => onAction('approve', changeSet)}>
          <Check className='size-4' />
          {t('changeSets.actions.approve')}
        </Button>
        <Button
          size='sm'
          variant='outline'
          className='border-red-500/35 text-red-700 hover:bg-red-500/10 hover:text-red-700 dark:text-red-300'
          onClick={() => onAction('reject', changeSet)}
        >
          <X className='size-4' />
          {t('changeSets.actions.reject')}
        </Button>
      </div>
    );
  }

  if (changeSet.status === 'INVALID') {
    return (
      <div className={className}>
        <Button
          size='sm'
          variant='outline'
          className='border-red-500/35 text-red-700 hover:bg-red-500/10 hover:text-red-700 dark:text-red-300'
          onClick={() => onAction('reject', changeSet)}
        >
          <X className='size-4' />
          {t('changeSets.actions.reject')}
        </Button>
      </div>
    );
  }

  return null;
}
