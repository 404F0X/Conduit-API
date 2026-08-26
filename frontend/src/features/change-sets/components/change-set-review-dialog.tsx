import { useState } from 'react';
import { Loader2 } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { Textarea } from '@/components/ui/textarea';
import { ConfirmDialog } from '@/components/confirm-dialog';
import { type ChangeSet, useApproveChangeSet, useRejectChangeSet, useSubmitChangeSet } from '../data/change-sets';
import type { ChangeSetActionType } from './change-set-actions';

export type ChangeSetActionSelection = {
  action: ChangeSetActionType;
  changeSet: ChangeSet;
};

type Props = {
  selection: ChangeSetActionSelection | null;
  onClose: () => void;
};

export function ChangeSetReviewDialog({ selection, onClose }: Props) {
  const { t } = useTranslation();
  const submit = useSubmitChangeSet();
  const approve = useApproveChangeSet();
  const reject = useRejectChangeSet();
  const [reviewNote, setReviewNote] = useState('');

  const close = () => {
    if (submit.isPending || approve.isPending || reject.isPending) return;
    setReviewNote('');
    onClose();
  };

  const handleConfirm = async () => {
    if (!selection) return;

    try {
      if (selection.action === 'submit') {
        await submit.mutateAsync(selection.changeSet.id);
      } else if (selection.action === 'approve') {
        await approve.mutateAsync({ id: selection.changeSet.id, reviewNote: reviewNote.trim() || undefined });
      } else {
        await reject.mutateAsync({ id: selection.changeSet.id, reviewNote: reviewNote.trim() || undefined });
      }
      toast.success(t(`changeSets.toast.${selection.action}`));
      setReviewNote('');
      onClose();
    } catch {
      // graphqlRequest owns the shared error presentation.
    }
  };

  const action = selection?.action ?? 'submit';
  const pending = submit.isPending || approve.isPending || reject.isPending;

  return (
    <ConfirmDialog
      open={selection !== null}
      onOpenChange={(open) => !open && close()}
      title={t(`changeSets.confirm.${action}.title`)}
      desc={t(`changeSets.confirm.${action}.description`, { title: selection?.changeSet.title ?? '' })}
      confirmText={
        <span className='inline-flex items-center gap-2'>
          {pending && <Loader2 className='size-4 animate-spin' />}
          {t(`changeSets.actions.${action}`)}
        </span>
      }
      cancelBtnText={t('common.buttons.cancel')}
      destructive={action === 'reject'}
      isLoading={pending}
      handleConfirm={handleConfirm}
      className='rounded-md'
    >
      {action !== 'submit' && (
        <Textarea
          value={reviewNote}
          onChange={(event) => setReviewNote(event.target.value)}
          placeholder={t('changeSets.confirm.reviewNotePlaceholder')}
          aria-label={t('changeSets.confirm.reviewNoteLabel')}
          maxLength={2000}
          className='min-h-24'
        />
      )}
    </ConfirmDialog>
  );
}
