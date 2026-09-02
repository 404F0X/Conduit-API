import { useState, type FormEvent } from 'react';
import { IconBan, IconChevronLeft, IconChevronRight, IconCopy, IconPlus, IconRefresh, IconTicket } from '@tabler/icons-react';
import { isAuthError } from '@/gql/graphql';
import { useTranslation } from 'react-i18next';
import { toast } from 'sonner';
import { Alert, AlertDescription, AlertTitle } from '@/components/ui/alert';
import { Badge } from '@/components/ui/badge';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Input } from '@/components/ui/input';
import { Label } from '@/components/ui/label';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { isValidCreditAmount, isValidRedemptionQuantity, isValidRedemptionUseLimit } from '../redemption-code';
import {
  type CreateCreditRedemptionCodesPayload,
  type CreditRedemptionCodeStatus,
  useCreateCreditRedemptionCodes,
  useCreditRedemptionCodes,
  useRevokeCreditRedemptionCode,
} from '../redemption-data';

const PAGE_SIZE = 20;

function displayDate(value: string | null | undefined, locale: string) {
  if (!value) return '—';
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return '—';
  return new Intl.DateTimeFormat(locale, { dateStyle: 'medium', timeStyle: 'short' }).format(parsed);
}

function statusVariant(status: CreditRedemptionCodeStatus): 'default' | 'secondary' | 'destructive' | 'outline' {
  if (status === 'ACTIVE') return 'default';
  if (status === 'REDEEMED') return 'secondary';
  if (status === 'REVOKED') return 'destructive';
  return 'outline';
}

export function RedemptionCodeSection({
  creditDisplayName,
  canCreate,
  canRevoke,
}: {
  creditDisplayName: string;
  canCreate: boolean;
  canRevoke: boolean;
}) {
  const { t, i18n } = useTranslation();
  const [offset, setOffset] = useState(0);
  const [createOpen, setCreateOpen] = useState(false);
  const query = useCreditRedemptionCodes(PAGE_SIZE, offset);
  const revoke = useRevokeCreditRedemptionCode();
  const page = query.data;
  const currentPage = Math.floor(offset / PAGE_SIZE) + 1;
  const totalPages = Math.max(1, Math.ceil((page?.total || 0) / PAGE_SIZE));

  const revokeCode = async (id: string) => {
    if (!window.confirm(t('billing.redemption.admin.revokeConfirm'))) return;
    try {
      await revoke.mutateAsync(id);
      toast.success(t('billing.redemption.admin.revokeSuccess'));
    } catch (error) {
      if (!isAuthError(error)) {
        toast.error(t('billing.redemption.admin.revokeError'));
      }
    }
  };

  return (
    <>
      <Card className='gap-4 py-5 shadow-none'>
        <CardHeader className='flex-row items-start justify-between gap-4 px-5'>
          <div>
            <CardTitle className='flex items-center gap-2'>
              <IconTicket className='text-emerald-600' size={19} />
              {t('billing.redemption.admin.title')}
            </CardTitle>
            <CardDescription className='mt-1'>{t('billing.redemption.admin.description')}</CardDescription>
          </div>
          {canCreate && (
            <Button size='sm' onClick={() => setCreateOpen(true)}>
              <IconPlus /> {t('billing.redemption.admin.create')}
            </Button>
          )}
        </CardHeader>
        <CardContent className='px-0'>
          {query.isLoading ? (
            <div className='text-muted-foreground flex items-center justify-center gap-2 py-12 text-sm'>
              <IconRefresh className='animate-spin' /> {t('billing.redemption.admin.loading')}
            </div>
          ) : query.error ? (
            <div className='px-5'>
              <Alert variant='destructive'>
                <IconTicket />
                <AlertTitle>{t('billing.redemption.admin.loadErrorTitle')}</AlertTitle>
                <AlertDescription className='flex flex-wrap items-center justify-between gap-3'>
                  <span>{t('billing.redemption.admin.loadError')}</span>
                  <Button size='sm' variant='outline' onClick={() => query.refetch()}>
                    <IconRefresh /> {t('billing.retry')}
                  </Button>
                </AlertDescription>
              </Alert>
            </div>
          ) : !page?.items.length ? (
            <div className='text-muted-foreground py-12 text-center text-sm'>{t('billing.redemption.admin.empty')}</div>
          ) : (
            <div className='overflow-x-auto'>
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead className='pl-5'>{t('billing.redemption.admin.codeHint')}</TableHead>
                    <TableHead>{t('billing.redemption.admin.descriptionColumn')}</TableHead>
                    <TableHead>{t('billing.redemption.admin.amount')}</TableHead>
                    <TableHead>{t('billing.redemption.admin.usage')}</TableHead>
                    <TableHead>{t('billing.redemption.admin.status')}</TableHead>
                    <TableHead>{t('billing.redemption.admin.expiresAt')}</TableHead>
                    <TableHead>{t('billing.redemption.admin.createdAt')}</TableHead>
                    <TableHead className='pr-5 text-right'>{t('billing.redemption.admin.actions')}</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {page.items.map((code) => (
                    <TableRow key={code.id}>
                      <TableCell className='pl-5 font-mono text-xs'>{code.codeHint}</TableCell>
                      <TableCell className='text-muted-foreground max-w-64 truncate text-xs' title={code.description || undefined}>
                        {code.description || '—'}
                      </TableCell>
                      <TableCell className='font-mono tabular-nums'>
                        {creditDisplayName} {code.amount}
                      </TableCell>
                      <TableCell className='font-mono tabular-nums'>
                        {code.redemptionCount} / {code.maxRedemptions}
                      </TableCell>
                      <TableCell>
                        <Badge variant={statusVariant(code.status)}>{t(`billing.redemption.status.${code.status.toLowerCase()}`)}</Badge>
                      </TableCell>
                      <TableCell className='text-muted-foreground text-xs whitespace-nowrap'>
                        {displayDate(code.expiresAt, i18n.language)}
                      </TableCell>
                      <TableCell className='text-muted-foreground text-xs whitespace-nowrap'>
                        {displayDate(code.createdAt, i18n.language)}
                      </TableCell>
                      <TableCell className='pr-5 text-right'>
                        {canRevoke && code.status === 'ACTIVE' ? (
                          <Button
                            size='sm'
                            variant='ghost'
                            className='text-destructive hover:text-destructive'
                            disabled={revoke.isPending}
                            onClick={() => revokeCode(code.id)}
                          >
                            <IconBan /> {t('billing.redemption.admin.revoke')}
                          </Button>
                        ) : (
                          <span className='text-muted-foreground'>—</span>
                        )}
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            </div>
          )}

          {!!page?.total && (
            <div className='flex items-center justify-between border-t px-5 pt-4'>
              <p className='text-muted-foreground text-xs'>
                {t('billing.redemption.admin.pagination', { page: currentPage, pages: totalPages, total: page.total })}
              </p>
              <div className='flex gap-2'>
                <Button
                  size='icon'
                  variant='outline'
                  aria-label={t('billing.redemption.admin.previous')}
                  disabled={offset === 0 || query.isFetching}
                  onClick={() => setOffset((value) => Math.max(0, value - PAGE_SIZE))}
                >
                  <IconChevronLeft />
                </Button>
                <Button
                  size='icon'
                  variant='outline'
                  aria-label={t('billing.redemption.admin.next')}
                  disabled={offset + PAGE_SIZE >= page.total || query.isFetching}
                  onClick={() => setOffset((value) => value + PAGE_SIZE)}
                >
                  <IconChevronRight />
                </Button>
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      <CreateRedemptionCodesDialog open={createOpen} onOpenChange={setCreateOpen} creditDisplayName={creditDisplayName} />
    </>
  );
}

function CreateRedemptionCodesDialog({
  open,
  onOpenChange,
  creditDisplayName,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  creditDisplayName: string;
}) {
  const { t } = useTranslation();
  const create = useCreateCreditRedemptionCodes();
  const [amount, setAmount] = useState('');
  const [quantity, setQuantity] = useState(1);
  const [maxRedemptions, setMaxRedemptions] = useState(1);
  const [expiresAt, setExpiresAt] = useState('');
  const [description, setDescription] = useState('');
  const [created, setCreated] = useState<CreateCreditRedemptionCodesPayload>();

  const reset = () => {
    setAmount('');
    setQuantity(1);
    setMaxRedemptions(1);
    setExpiresAt('');
    setDescription('');
    setCreated(undefined);
    create.reset();
  };

  const close = () => {
    if (create.isPending) return;
    reset();
    onOpenChange(false);
  };

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    if (!isValidCreditAmount(amount) || !isValidRedemptionQuantity(quantity) || !isValidRedemptionUseLimit(maxRedemptions)) {
      return;
    }

    let expiresAtISO: string | undefined;
    if (expiresAt) {
      const parsed = new Date(expiresAt);
      if (Number.isNaN(parsed.getTime())) {
        toast.error(t('billing.redemption.admin.invalidExpiry'));
        return;
      }
      expiresAtISO = parsed.toISOString();
    }

    try {
      const result = await create.mutateAsync({
        amount: amount.trim(),
        quantity,
        maxRedemptions,
        expiresAt: expiresAtISO,
        description: description.trim() || undefined,
      });
      setCreated(result);
      create.reset();
      toast.success(t('billing.redemption.admin.createSuccess', { count: result.quantity }));
    } catch (error) {
      if (!isAuthError(error)) {
        toast.error(t('billing.redemption.admin.createError'));
      }
    }
  };

  const copyCodes = async () => {
    if (!created) return;
    try {
      await navigator.clipboard.writeText(created.codes.map((item) => item.code).join('\n'));
      toast.success(t('billing.redemption.admin.copySuccess'));
    } catch {
      toast.error(t('billing.redemption.admin.copyError'));
    }
  };

  const valid = isValidCreditAmount(amount) && isValidRedemptionQuantity(quantity) && isValidRedemptionUseLimit(maxRedemptions);

  return (
    <Dialog open={open} onOpenChange={(nextOpen) => (nextOpen ? onOpenChange(true) : close())}>
      <DialogContent className='max-h-[calc(100dvh-2rem)] overflow-y-auto sm:max-w-lg'>
        {created ? (
          <>
            <DialogHeader>
              <DialogTitle>{t('billing.redemption.admin.createdTitle')}</DialogTitle>
              <DialogDescription>{t('billing.redemption.admin.createdDescription')}</DialogDescription>
            </DialogHeader>
            <Alert className='my-4 border-amber-500/40 bg-amber-500/5'>
              <IconTicket className='text-amber-700 dark:text-amber-400' />
              <AlertTitle>{t('billing.redemption.admin.showOnceTitle')}</AlertTitle>
              <AlertDescription>{t('billing.redemption.admin.showOnceDescription')}</AlertDescription>
            </Alert>
            <div className='bg-muted/40 max-h-72 overflow-auto rounded-md border p-3'>
              <pre className='font-mono text-sm leading-7 whitespace-pre-wrap' data-testid='generated-redemption-codes'>
                {created.codes.map((item) => item.code).join('\n')}
              </pre>
            </div>
            <p className='text-muted-foreground mt-2 text-xs'>
              {t('billing.redemption.admin.createdSummary', {
                count: created.quantity,
                amount: `${creditDisplayName} ${created.amount}`,
                uses: created.maxRedemptions,
              })}
            </p>
            <DialogFooter className='mt-5'>
              <Button type='button' variant='outline' onClick={copyCodes}>
                <IconCopy /> {t('billing.redemption.admin.copyAll')}
              </Button>
              <Button type='button' onClick={close}>
                {t('billing.redemption.admin.done')}
              </Button>
            </DialogFooter>
          </>
        ) : (
          <form onSubmit={submit}>
            <DialogHeader>
              <DialogTitle>{t('billing.redemption.admin.createTitle')}</DialogTitle>
              <DialogDescription>{t('billing.redemption.admin.createDescription')}</DialogDescription>
            </DialogHeader>
            <div className='space-y-4 py-5'>
              <div className='space-y-2'>
                <Label htmlFor='redemption-code-amount'>{t('billing.redemption.admin.amount')}</Label>
                <div className='flex'>
                  <span className='bg-muted text-muted-foreground flex items-center rounded-l-md border border-r-0 px-3 font-mono text-xs'>
                    {creditDisplayName}
                  </span>
                  <Input
                    id='redemption-code-amount'
                    value={amount}
                    onChange={(event) => setAmount(event.target.value)}
                    inputMode='decimal'
                    autoComplete='off'
                    className='rounded-l-none font-mono tabular-nums'
                    placeholder='100.00'
                    disabled={create.isPending}
                  />
                </div>
                <p className='text-muted-foreground text-xs'>{t('billing.redemption.admin.amountHint')}</p>
              </div>
              <div className='space-y-2'>
                <Label htmlFor='redemption-code-quantity'>{t('billing.redemption.admin.quantity')}</Label>
                <Input
                  id='redemption-code-quantity'
                  type='number'
                  min={1}
                  max={1000}
                  step={1}
                  value={quantity}
                  onChange={(event) => setQuantity(Number(event.target.value))}
                  disabled={create.isPending}
                />
                <p className='text-muted-foreground text-xs'>{t('billing.redemption.admin.quantityHint')}</p>
              </div>
              <div className='space-y-2'>
                <Label htmlFor='redemption-code-use-limit'>{t('billing.redemption.admin.maxRedemptions')}</Label>
                <Input
                  id='redemption-code-use-limit'
                  type='number'
                  min={1}
                  max={100_000}
                  step={1}
                  value={maxRedemptions}
                  onChange={(event) => setMaxRedemptions(Number(event.target.value))}
                  disabled={create.isPending}
                />
                <p className='text-muted-foreground text-xs'>{t('billing.redemption.admin.maxRedemptionsHint')}</p>
              </div>
              <div className='space-y-2'>
                <Label htmlFor='redemption-code-expiry'>{t('billing.redemption.admin.expiry')}</Label>
                <Input
                  id='redemption-code-expiry'
                  type='datetime-local'
                  value={expiresAt}
                  onChange={(event) => setExpiresAt(event.target.value)}
                  disabled={create.isPending}
                />
                <p className='text-muted-foreground text-xs'>{t('billing.redemption.admin.expiryHint')}</p>
              </div>
              <div className='space-y-2'>
                <Label htmlFor='redemption-code-description'>{t('billing.redemption.admin.note')}</Label>
                <Input
                  id='redemption-code-description'
                  value={description}
                  onChange={(event) => setDescription(event.target.value)}
                  placeholder={t('billing.redemption.admin.notePlaceholder')}
                  maxLength={500}
                  disabled={create.isPending}
                />
              </div>
            </div>
            <DialogFooter>
              <Button type='button' variant='outline' onClick={close} disabled={create.isPending}>
                {t('billing.cancel')}
              </Button>
              <Button type='submit' disabled={create.isPending || !valid}>
                {create.isPending ? <IconRefresh className='animate-spin' /> : <IconPlus />}
                {t('billing.redemption.admin.createSubmit')}
              </Button>
            </DialogFooter>
          </form>
        )}
      </DialogContent>
    </Dialog>
  );
}
