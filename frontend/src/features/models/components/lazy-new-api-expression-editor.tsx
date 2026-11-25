import { lazy, Suspense, type ComponentProps } from 'react';
import { Skeleton } from '@/components/ui/skeleton';
import type { NewApiExpressionEditor } from './new-api-expression-editor';

const Editor = lazy(async () => ({ default: (await import('./new-api-expression-editor')).NewApiExpressionEditor }));

export function LazyNewApiExpressionEditor(props: ComponentProps<typeof NewApiExpressionEditor>) {
  return (
    <Suspense fallback={<Skeleton className='h-24 w-full' />}>
      <Editor {...props} />
    </Suspense>
  );
}
