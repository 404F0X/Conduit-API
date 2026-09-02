import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { useRouter } from '@tanstack/react-router';
import { toast } from 'sonner';
import { systemApi } from '@/lib/api-client';
import i18n from '@/lib/i18n';

export interface SystemStatus {
  isInitialized: boolean;
}

export interface InitializeSystemInput {
  ownerEmail: string;
  ownerPassword: string;
  ownerFirstName: string;
  ownerLastName: string;
  brandName: string;
  preferLanguage?: string;
  accountingCurrencyCode: string;
  creditDisplayName: string;
  creditsPerAccountingUnit: string;
}

export interface InitializeSystemPayload {
  success: boolean;
  message: string;
}

export const systemStatusQueryKey = ['systemStatus'] as const;

export function useSystemStatus() {
  return useQuery({
    queryKey: systemStatusQueryKey,
    queryFn: async (): Promise<SystemStatus> => {
      return await systemApi.getStatus();
    },
    retry: 1, // Allow one retry to handle network hiccups on first load
    retryDelay: 500, // Wait 500ms before retry
    refetchOnWindowFocus: false,
    staleTime: 5 * 60 * 1000, // Consider data fresh for 5 minutes
  });
}

export function useInitializeSystem() {
  const router = useRouter();
  const queryClient = useQueryClient();

  return useMutation({
    mutationFn: async (input: InitializeSystemInput): Promise<InitializeSystemPayload> => {
      return await systemApi.initialize(input);
    },
    onSuccess: (data) => {
      if (data.success) {
        // Prevent InitializationGuard from treating the first real login as a
        // stale session while its pre-initialization status cache is fresh.
        queryClient.setQueryData<SystemStatus>(systemStatusQueryKey, { isInitialized: true });
        toast.success(data.message);

        // 初始化成功后跳转到登录页面
        router.navigate({ to: '/sign-in' });
      } else {
        toast.error(data.message);
      }
    },
    onError: (error: any) => {
      const errorMessage = error.message || i18n.t('common.errors.initializationFailed');
      toast.error(errorMessage);
    },
  });
}
