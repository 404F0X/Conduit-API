import { useEffect, useRef, useState } from 'react';
import { useTranslation } from 'react-i18next';
import { useAuthStore } from '@/stores/authStore';
import { useProjectStore } from '@/stores/projectStore';
import { Alert, AlertDescription } from '@/components/ui/alert';
import { Button } from '@/components/ui/button';
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card';
import { Input } from '@/components/ui/input';
import {
  StalePaymentContextError,
  useCreatePaymentOrder,
  usePaymentCapabilities,
  usePaymentOrder,
  usePaymentOrders,
  useRecheckPaymentOrder,
  type PaymentChannel,
  type PaymentOrder,
} from '../payment-data';
import { cnyToMinor, formatPaymentUnits, paymentCheckoutHref } from '../payment-money';

type Intent = { amountMinor: string; channel: PaymentChannel; idempotencyKey: string };
function readIntent(key: string): Intent | null {
  try {
    const value: unknown = JSON.parse(sessionStorage.getItem(key) ?? 'null');
    if (!value || typeof value !== 'object') return null;
    const intent = value as Intent;
    return typeof intent.amountMinor === 'string' &&
      /^[1-9]\d*$/.test(intent.amountMinor) &&
      ['ALIPAY', 'WECHAT'].includes(intent.channel) &&
      typeof intent.idempotencyKey === 'string' &&
      /^[\da-f-]{36}$/i.test(intent.idempotencyKey)
      ? intent
      : null;
  } catch {
    return null;
  }
}
function storeIntent(key: string, intent: Intent | null) {
  try {
    if (intent) sessionStorage.setItem(key, JSON.stringify(intent));
    else sessionStorage.removeItem(key);
  } catch {
    /* The in-memory intent still makes explicit retries idempotent. */
  }
}

function serverDate(value: string, locale: string) {
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? '—' : new Intl.DateTimeFormat(locale, { dateStyle: 'short', timeStyle: 'short' }).format(parsed);
}

export function PaymentTopupPanel({
  payerID,
  projectID,
  creditDisplayName,
}: {
  payerID: string;
  projectID: string;
  creditDisplayName: string;
}) {
  const { t, i18n } = useTranslation();
  const active = useRef(true);
  useEffect(() => {
    active.current = true;
    return () => {
      active.current = false;
    };
  }, []);
  const isCurrent = () =>
    active.current && useAuthStore.getState().auth.user?.id === payerID && useProjectStore.getState().selectedProjectId === projectID;
  const storageKey = `payment-create-intent:${payerID}:${projectID}`;
  const intent = useRef<Intent | null>(readIntent(storageKey));
  const [input, setInput] = useState(() => (intent.current ? formatPaymentUnits(intent.current.amountMinor, 2, true) : ''));
  const [channel, setChannel] = useState<PaymentChannel>(intent.current?.channel ?? 'ALIPAY');
  const [error, setError] = useState('');
  const [created, setCreated] = useState<PaymentOrder | null>(null);
  const [selectedID, setSelectedID] = useState<string>();
  const [offset, setOffset] = useState(0);
  const capabilities = usePaymentCapabilities();
  const orders = usePaymentOrders(offset);
  const selected = usePaymentOrder(selectedID);
  const create = useCreatePaymentOrder();
  const recheck = useRecheckPaymentOrder();
  const order = selected.data ?? (created?.id === selectedID ? created : null);
  const amountMinor = capabilities.data ? cnyToMinor(input, capabilities.data.minAmountMinor, capabilities.data.maxAmountMinor) : null;
  const enabled = capabilities.data?.enabled === true;
  const checkout = order && Date.parse(order.expiresAt) > Date.now() ? paymentCheckoutHref(order.checkoutURL) : null;
  const pending = create.isPending || recheck.isPending;
  const selectOrder = (value: PaymentOrder) => {
    if (!isCurrent()) return;
    setCreated(value);
    setSelectedID(value.id);
    setError('');
  };

  if (!isCurrent()) return null;

  return (
    <Card className='gap-3 py-4 shadow-none'>
      <CardHeader className='px-4'>
        <CardTitle className='text-base'>{t('wallet.payment.title')}</CardTitle>
        <CardDescription>{t('wallet.payment.description')}</CardDescription>
      </CardHeader>
      <CardContent className='space-y-4 px-4'>
        {(!enabled || capabilities.isError) && (
          <Alert>
            <AlertDescription>{t(capabilities.isError ? 'wallet.payment.unavailable' : 'wallet.payment.disabled')}</AlertDescription>
          </Alert>
        )}
        {!selectedID && (
          <form
            className='space-y-3'
            onSubmit={async (event) => {
              event.preventDefault();
              if (!isCurrent() || create.isPending || !enabled || !amountMinor || !capabilities.data?.channels.includes(channel)) return;
              setError('');
              if (!intent.current) {
                intent.current = { amountMinor, channel, idempotencyKey: crypto.randomUUID() };
                storeIntent(storageKey, intent.current);
              }
              try {
                const receipt = await create.mutateAsync(intent.current);
                if (!isCurrent()) return;
                selectOrder(receipt);
                intent.current = null;
                storeIntent(storageKey, null);
              } catch (error) {
                if (isCurrent() && !(error instanceof StalePaymentContextError)) setError(t('wallet.payment.createFailed'));
              }
            }}
          >
            <p className='text-sm'>{t('wallet.payment.target', { project: projectID })}</p>
            <div className='grid gap-3 sm:grid-cols-2'>
              <label className='grid gap-1.5 text-sm'>
                {t('wallet.payment.amount')}
                <Input
                  inputMode='decimal'
                  value={input}
                  onChange={(event) => setInput(event.target.value)}
                  disabled={pending || !!intent.current || !enabled}
                  placeholder='10.00'
                />
              </label>
              <label className='grid gap-1.5 text-sm'>
                {t('wallet.payment.channel')}
                <select
                  className='bg-background h-9 rounded-md border px-2'
                  value={channel}
                  onChange={(event) => setChannel(event.target.value as PaymentChannel)}
                  disabled={pending || !!intent.current || !enabled}
                >
                  <option value='ALIPAY' disabled={!capabilities.data?.channels.includes('ALIPAY')}>
                    {t('wallet.payment.alipay')}
                  </option>
                  <option value='WECHAT' disabled={!capabilities.data?.channels.includes('WECHAT')}>
                    {t('wallet.payment.wechat')}
                  </option>
                </select>
              </label>
            </div>
            {capabilities.data && enabled && (
              <p className='text-muted-foreground text-xs'>
                {t('wallet.payment.range', {
                  min: formatPaymentUnits(capabilities.data.minAmountMinor, 2, true),
                  max: formatPaymentUnits(capabilities.data.maxAmountMinor, 2, true),
                })}
              </p>
            )}
            {!!input && !amountMinor && enabled && (
              <p role='alert' className='text-destructive text-sm'>
                {t('wallet.payment.invalidAmount')}
              </p>
            )}
            {intent.current && <p className='text-muted-foreground text-sm'>{t('wallet.payment.retrySameOrder')}</p>}
            <Button type='submit' disabled={!enabled || !amountMinor || pending || !capabilities.data?.channels.includes(channel)}>
              {t(intent.current ? 'wallet.payment.retryCreate' : 'wallet.payment.create')}
            </Button>
          </form>
        )}
        {error && (
          <p role='alert' className='text-destructive text-sm'>
            {error}
          </p>
        )}
        {selected.isError && (
          <p role='alert' className='text-destructive text-sm'>
            {t('wallet.payment.statusFailed')}
          </p>
        )}
        {order && (
          <section className='space-y-3 rounded-md border p-4' aria-label={t('wallet.payment.orderDetail')}>
            <h3 className='text-sm font-semibold'>{t('wallet.payment.orderDetail')}</h3>
            <dl className='grid gap-3 text-sm sm:grid-cols-2'>
              {[
                [t('wallet.payment.orderID'), order.id],
                [t('wallet.payment.targetLabel'), order.projectID],
                [t('wallet.payment.pay'), `${order.currency} ${formatPaymentUnits(order.amountMinor, 2, true)}`],
                [t('wallet.payment.receive'), `${creditDisplayName} ${formatPaymentUnits(order.creditsMicros, 6)}`],
                [t('wallet.payment.multiplier'), `${formatPaymentUnits(order.rechargeMultiplierPPM, 6)}×`],
                [t('wallet.payment.accountingVersion'), order.accountingVersion],
                [
                  t('wallet.payment.statusLabel'),
                  t(`wallet.payment.status.${order.status}`, { defaultValue: t('wallet.payment.status.unknown') }),
                ],
                [t('wallet.payment.expires'), serverDate(order.expiresAt, i18n.language)],
              ].map(([label, value]) => (
                <div key={label}>
                  <dt className='text-muted-foreground'>{label}</dt>
                  <dd className='mt-1 font-mono break-all tabular-nums'>{value}</dd>
                </div>
              ))}
            </dl>
            <p className='text-muted-foreground text-xs'>{t('wallet.payment.frozen')}</p>
            <p className='text-muted-foreground text-xs'>{t('wallet.payment.returnHint')}</p>
            {order.status === 'review_required' && (
              <Alert>
                <AlertDescription>{t('wallet.payment.review')}</AlertDescription>
              </Alert>
            )}
            <div className='flex flex-wrap gap-2'>
              {order.status === 'pending' && !selected.isError && checkout && (
                <Button asChild>
                  <a
                    href={checkout!}
                    target='_blank'
                    rel='noopener noreferrer'
                    onClick={(event) => {
                      if (!isCurrent() || Date.parse(order.expiresAt) <= Date.now()) {
                        event.preventDefault();
                        setError(t('wallet.payment.checkoutExpired'));
                      }
                    }}
                  >
                    {t('wallet.payment.cashier')}
                  </a>
                </Button>
              )}
              <Button
                type='button'
                variant='outline'
                disabled={pending || !enabled}
                onClick={async () => {
                  if (!isCurrent()) return;
                  setError('');
                  try {
                    selectOrder(await recheck.mutateAsync(order.id));
                  } catch (error) {
                    if (isCurrent() && !(error instanceof StalePaymentContextError)) setError(t('wallet.payment.recheckFailed'));
                  }
                }}
              >
                {t('wallet.payment.recheck')}
              </Button>
              <Button
                type='button'
                variant='ghost'
                disabled={pending}
                onClick={() => {
                  setSelectedID(undefined);
                  setCreated(null);
                  setError('');
                }}
              >
                {t('wallet.payment.back')}
              </Button>
            </div>
          </section>
        )}
        <section className='space-y-2'>
          <div className='flex items-center justify-between gap-2'>
            <h3 className='text-sm font-medium'>{t('wallet.payment.history')}</h3>
            <Button
              type='button'
              variant='ghost'
              size='sm'
              disabled={orders.isFetching}
              onClick={() => {
                void orders.refetch();
                if (selectedID) void selected.refetch();
              }}
            >
              {t('wallet.payment.refresh')}
            </Button>
          </div>
          {orders.isError ? (
            <p role='alert' className='text-destructive text-sm'>
              {t('wallet.payment.historyFailed')}
            </p>
          ) : orders.isPending ? (
            <p className='text-muted-foreground text-sm'>{t('wallet.payment.loading')}</p>
          ) : orders.data?.length ? (
            <ul className='divide-y'>
              {orders.data.map((item) => (
                <li key={item.id}>
                  <button
                    type='button'
                    className='hover:bg-muted/40 flex w-full flex-wrap justify-between gap-2 rounded p-2 text-left text-sm'
                    onClick={() => selectOrder(item)}
                  >
                    <span>
                      {serverDate(item.createdAt, i18n.language)} ·{' '}
                      {t(item.channel === 'ALIPAY' ? 'wallet.payment.alipay' : 'wallet.payment.wechat')}
                    </span>
                    <span className='font-mono tabular-nums'>
                      CNY {formatPaymentUnits(item.amountMinor, 2, true)} ·{' '}
                      {t(`wallet.payment.status.${item.status}`, { defaultValue: t('wallet.payment.status.unknown') })}
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          ) : (
            <p className='text-muted-foreground text-sm'>{t('wallet.payment.empty')}</p>
          )}
          <div className='flex gap-2'>
            <Button
              type='button'
              variant='outline'
              size='sm'
              disabled={offset === 0 || orders.isFetching}
              onClick={() => setOffset(Math.max(0, offset - 10))}
            >
              {t('wallet.payment.previous')}
            </Button>
            <Button
              type='button'
              variant='outline'
              size='sm'
              disabled={orders.data?.length !== 10 || orders.isFetching}
              onClick={() => setOffset(offset + 10)}
            >
              {t('wallet.payment.next')}
            </Button>
          </div>
        </section>
      </CardContent>
    </Card>
  );
}
