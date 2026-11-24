import type React from 'react';
import { IconAlertTriangle, IconInbox, IconLock } from '@tabler/icons-react';
import { useTranslation } from 'react-i18next';
import { cn } from '@/lib/utils';
import { Button } from '@/components/ui/button';
import { Skeleton } from '@/components/ui/skeleton';

/**
 * 三个公共状态组件（UI 信息规格“空状态、加载、错误”）。
 * 每页只传文案和动作，不再各写各的。
 */

export function LoadingRows({ rows = 5, className }: { rows?: number; className?: string }) {
  return (
    <div className={cn('space-y-2', className)} aria-busy='true'>
      {Array.from({ length: rows }, (_, index) => (
        <Skeleton key={index} className='h-10 w-full' />
      ))}
    </div>
  );
}

export function EmptyState({
  title,
  description,
  action,
  className,
}: {
  title: string;
  description?: string;
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn('flex flex-col items-center justify-center gap-3 px-4 py-12 text-center', className)}>
      <IconInbox className='text-muted-foreground size-8' stroke={1.5} />
      <div className='space-y-1'>
        <p className='text-sm font-medium'>{title}</p>
        {description && <p className='text-muted-foreground max-w-md text-sm'>{description}</p>}
      </div>
      {action}
    </div>
  );
}

export function ErrorState({
  message,
  onRetry,
  forbidden,
  className,
}: {
  message?: string;
  onRetry?: () => void;
  /** 没有权限时显示“你没有查看这部分的权限，可以联系站长”，不显示重试 */
  forbidden?: boolean;
  className?: string;
}) {
  const { t } = useTranslation();
  const Icon = forbidden ? IconLock : IconAlertTriangle;
  return (
    <div className={cn('flex flex-col items-center justify-center gap-3 px-4 py-10 text-center', className)} role='alert'>
      <Icon className={cn('size-7', forbidden ? 'text-muted-foreground' : 'text-amber-500')} stroke={1.5} />
      <p className='text-muted-foreground max-w-md text-sm'>
        {forbidden ? t('simple.state.forbidden') : message || t('simple.state.loadFailed')}
      </p>
      {!forbidden && onRetry && (
        <Button variant='outline' size='sm' onClick={onRetry}>
          {t('simple.state.retry')}
        </Button>
      )}
    </div>
  );
}

/** 页头：标题、一句话说明、右侧一个主按钮（其他操作放进“更多”）。 */
export function PageHeading({ title, description, action }: { title: string; description?: string; action?: React.ReactNode }) {
  return (
    <div className='flex min-w-0 flex-1 items-center justify-between gap-4'>
      <div className='min-w-0'>
        <h2 className='truncate text-xl font-bold tracking-tight'>{title}</h2>
        {description && <p className='text-muted-foreground truncate text-sm'>{description}</p>}
      </div>
      {action && <div className='flex shrink-0 items-center gap-2'>{action}</div>}
    </div>
  );
}
