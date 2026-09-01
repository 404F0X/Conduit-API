import { useState, type FormEvent } from 'react';
import { IconRefresh, IconTicket } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { Button } from '@/components/ui/button';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { canRedeemCode, MAX_REDEMPTION_CODE_LENGTH, normalizeRedemptionCode } from '@/features/billing/redemption-code';
import { useRedeemCreditCode } from '@/features/billing/redemption-data';

export function RedeemCodeDialog({
  open,
  onOpenChange,
  creditDisplayName,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const redeem = useRedeemCreditCode();
  const [code, setCode] = useState('');
  const [showRequired, setShowRequired] = useState(false);

  const close = () => {
    if (redeem.isPending) return;
    setCode('');
    setShowRequired(false);
    redeem.reset();
    onOpenChange(false);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    const normalizedCode = normalizeRedemptionCode(code);
    if (!canRedeemCode(normalizedCode)) {
      setShowRequired(true);
      return;
    }

    try {
      const receipt = await redeem.mutateAsync(normalizedCode);
      toast.success(
        t('wallet.redeem.success', {
          amount: `${creditDisplayName} ${receipt.amount}`,
        })
      );
      setCode('');
      setShowRequired(false);
      redeem.reset();
      onOpenChange(false);
    } catch {
      toast.error(t('wallet.redeem.errors.generic'));
    }
  };

  return (
    <Dialog open={open} onOpenChange={(nextOpen) => (nextOpen ? onOpenChange(true) : close())}>
      <DialogContent className='sm:max-w-md'>
        <form onSubmit={submit}>
          <DialogHeader>
            <DialogTitle className='flex items-center gap-2'>
              <IconTicket className='text-emerald-600' size={20} />
              {t('wallet.redeem.title')}
            </DialogTitle>
            <DialogDescription>{t('wallet.redeem.description')}</DialogDescription>
          </DialogHeader>

          <div className='space-y-2 py-5'>
            <Label htmlFor='wallet-redemption-code'>{t('wallet.redeem.code')}</Label>
            <Input
              id='wallet-redemption-code'
              value={code}
              onChange={(event) => {
                setCode(event.target.value);
                setShowRequired(false);
              }}
              autoComplete='off'
              autoCapitalize='none'
              spellCheck={false}
              maxLength={MAX_REDEMPTION_CODE_LENGTH}
              className='font-mono tracking-wide'
              placeholder={t('wallet.redeem.placeholder')}
              aria-invalid={showRequired}
              aria-describedby={showRequired ? 'wallet-redemption-code-error' : 'wallet-redemption-code-hint'}
              disabled={redeem.isPending}
              autoFocus
            />
            <p id='wallet-redemption-code-hint' className='text-muted-foreground text-xs'>
              {t('wallet.redeem.hint')}
            </p>
            {showRequired && (
              <p id='wallet-redemption-code-error' className='text-destructive text-xs' role='alert'>
                {t('wallet.redeem.required')}
              </p>
            )}
          </div>

          <DialogFooter>
            <Button type='button' variant='outline' onClick={close} disabled={redeem.isPending}>
              {t('billing.cancel')}
            </Button>
            <Button type='submit' disabled={redeem.isPending || !canRedeemCode(code)}>
              {redeem.isPending ? <IconRefresh className='animate-spin' /> : <IconTicket />}
              {t('wallet.redeem.submit')}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
