import { lazy, Suspense } from 'react';
import { useTranslation } from 'react-i18next';
import { Skeleton } from '@/components/ui/skeleton';
import { useChannels } from '../context/channels-context';

const Dialog = lazy(async () => ({ default: (await import('./channels-model-price-dialog')).ChannelsModelPriceDialog }));

export function LazyChannelsModelPriceDialog() {
  const { open } = useChannels();
  const { t } = useTranslation();
  return (
    <Suspense fallback={open === 'price' ? <Skeleton role='status' aria-label={t('price.title')} className='h-10 w-full' /> : null}>
      <Dialog />
    </Suspense>
  );
}
