import { useEffect } from 'react';
import { useState } from 'react';
import { useRouter } from '@tanstack/react-router';
import { useAuthStore } from '@/stores/authStore';
import { Skeleton } from '@/components/ui/skeleton';
import { useSystemStatus } from '@/features/auth/data/initialization';

interface InitializationGuardProps {
  children: React.ReactNode;
}

export function InitializationGuard({ children }: InitializationGuardProps) {
  const router = useRouter();
  const { data: systemStatus, isLoading, error } = useSystemStatus();
  const accessToken = useAuthStore((state) => state.auth.accessToken);
  const user = useAuthStore((state) => state.auth.user);
  const resetAuth = useAuthStore((state) => state.auth.reset);
  const [isNavigating, setIsNavigating] = useState(false);
  const hasStoredSession = Boolean(accessToken || user);

  useEffect(() => {
    // A freshly initialized database cannot recognize a session issued by a
    // previous database. Clear that stale session before mounting anything
    // that may issue authenticated GraphQL requests.
    if (systemStatus && !systemStatus.isInitialized && hasStoredSession) {
      resetAuth();
      return;
    }

    // Only redirect if we have data and system is not initialized
    if (systemStatus && !systemStatus.isInitialized) {
      // Check if we're not already on the initialization page
      const currentPath = window.location.pathname;
      if (currentPath !== '/initialization') {
        setIsNavigating(true);
        router.navigate({ to: '/initialization' }).finally(() => {
          setIsNavigating(false);
        });
      }
    }
  }, [systemStatus, hasStoredSession, resetAuth, router]);

  // Show loading skeleton while checking system status
  if (isLoading || (systemStatus && !systemStatus.isInitialized && hasStoredSession)) {
    return (
      <div className='flex h-screen items-center justify-center'>
        <div className='space-y-4'>
          <Skeleton className='h-8 w-48' />
          <Skeleton className='h-4 w-32' />
        </div>
      </div>
    );
  }

  // Show error if failed to check system status
  if (error) {
    return (
      <div className='flex h-screen items-center justify-center'>
        <div className='text-center'>
          <h1 className='text-2xl font-bold text-red-600'>System Error</h1>
          <p className='text-muted-foreground'>Failed to check system status</p>
        </div>
      </div>
    );
  }

  // If system is not initialized and we're not on initialization page, don't render children
  // But allow navigation to complete naturally
  if ((systemStatus && !systemStatus.isInitialized && window.location.pathname !== '/initialization') || isNavigating) {
    // Don't return null immediately - let the navigation complete
    // The useEffect will handle the redirect
    return (
      <div className='flex h-screen items-center justify-center'>
        <div className='space-y-4'>
          <Skeleton className='h-8 w-48' />
          <Skeleton className='h-4 w-32' />
        </div>
      </div>
    );
  }

  return <>{children}</>;
}
