import { useEffect, useState } from 'react';
import { useNavigate } from '@tanstack/react-router';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Textarea } from '@/components/ui/textarea';
import { proposeCommand, type GovernanceCommand } from './data/governance';
import { GOVERNANCE_REQUEST_EVENT } from './mutation-adapter';

export function GovernanceProposalDialog() {
  const { t } = useTranslation();
  const navigate = useNavigate();
  const [command, setCommand] = useState<GovernanceCommand>();
  const [reason, setReason] = useState('');
  const [operationID, setOperationID] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    const receive = (event: Event) => {
      setCommand((event as CustomEvent<GovernanceCommand>).detail);
      setReason('');
      setError('');
      setOperationID(crypto.randomUUID());
    };
    window.addEventListener(GOVERNANCE_REQUEST_EVENT, receive);
    return () => window.removeEventListener(GOVERNANCE_REQUEST_EVENT, receive);
  }, []);
  async function save() {
    if (!command) return;
    setBusy(true);
    setError('');
    try {
      const detail = await proposeCommand(command, reason, operationID);
      setCommand(undefined);
      await navigate({ to: '/change-sets/$changeSetId', params: { changeSetId: detail.summary.id } });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Dialog
      open={!!command}
      onOpenChange={(open) => {
        if (!busy && !open) setCommand(undefined);
      }}
    >
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{t('governance.proposalRequired', { defaultValue: 'This change requires approval' })}</DialogTitle>
          <DialogDescription>
            {t('governance.proposalRequiredBody', {
              defaultValue:
                'Create a proposal from your saved form. Review and publication happen separately; the live configuration changes only when published.',
            })}
          </DialogDescription>
        </DialogHeader>
        <Textarea
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          placeholder={t('governance.reason', { defaultValue: 'Reason for this change' })}
          aria-label={t('governance.reason', { defaultValue: 'Reason for this change' })}
          disabled={busy}
        />
        {error && (
          <p role='alert' className='text-destructive text-sm'>
            {error}
          </p>
        )}
        <DialogFooter>
          <Button variant='outline' disabled={busy} onClick={() => setCommand(undefined)}>
            {t('governance.cancel')}
          </Button>
          <Button disabled={busy || !reason.trim()} onClick={() => void save()}>
            {t('governance.propose', { defaultValue: 'Create proposal' })}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
