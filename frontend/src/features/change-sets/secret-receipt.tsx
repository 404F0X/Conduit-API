import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { claimGovernedSecrets } from './data/governance';

export function SecretReceipt({ id }: { id: string }) {
  const { t } = useTranslation();
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [receipt, setReceipt] = useState<unknown>();
  const [error, setError] = useState('');
  async function claim() {
    setOpen(true);
    setBusy(true);
    setError('');
    try {
      setReceipt(await claimGovernedSecrets(id));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <Button variant='outline' disabled={busy} onClick={() => void claim()}>
        {t('governance.claim', { defaultValue: 'Receive generated keys or codes' })}
      </Button>
      <Dialog
        open={open}
        onOpenChange={(value) => {
          if (!busy) {
            setOpen(value);
            if (!value) setReceipt(undefined);
          }
        }}
      >
        <DialogContent>
          <DialogHeader>
            <DialogTitle>{t('governance.secretTitle', { defaultValue: 'One-time receipt' })}</DialogTitle>
            <DialogDescription>
              {t('governance.secretExplanation', {
                defaultValue: 'Only the proposal creator can receive generated secrets, once. Save them before closing this dialog.',
              })}
            </DialogDescription>
          </DialogHeader>
          {busy && <p role='status'>{t('common.loading')}</p>}
          {error && (
            <p role='alert' className='text-destructive text-sm'>
              {error}
            </p>
          )}
          {receipt !== undefined && (
            <pre className='bg-muted max-h-80 overflow-auto rounded-md p-3 text-xs break-all whitespace-pre-wrap'>
              {JSON.stringify(receipt, null, 2)}
            </pre>
          )}
          <DialogFooter>
            {receipt !== undefined && (
              <Button
                variant='outline'
                onClick={() => {
                  void navigator.clipboard
                    .writeText(JSON.stringify(receipt, null, 2))
                    .catch(() =>
                      setError(t('governance.copyFailed', { defaultValue: 'Copy failed. Select and save the receipt before closing.' }))
                    );
                }}
              >
                {t('common.copy', { defaultValue: 'Copy' })}
              </Button>
            )}
            <Button
              disabled={busy}
              onClick={() => {
                setReceipt(undefined);
                setOpen(false);
              }}
            >
              {t('common.close', { defaultValue: 'Close' })}
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
    </>
  );
}
