import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import type { SimpleRequestStatus } from '../data/usage';

const TONE: Record<SimpleRequestStatus, string> = {
  completed: 'bg-emerald-500',
  failed: 'bg-red-500',
  canceled: 'bg-muted-foreground/50',
  pending: 'bg-amber-500',
  processing: 'bg-sky-500',
};

/** 请求状态：中文词 + 颜色圆点（成功 / 失败 / 已取消 / 排队中 / 进行中）。 */
export function StatusLabel({ status }: { status: SimpleRequestStatus }) {
  const { t } = useTranslation();
  return (
    <span className='inline-flex items-center gap-1.5 text-sm whitespace-nowrap'>
      <span className={cn('size-2 rounded-full', TONE[status] ?? 'bg-muted-foreground/50')} />
      {t(`simple.status.${status}`)}
    </span>
  );
}
