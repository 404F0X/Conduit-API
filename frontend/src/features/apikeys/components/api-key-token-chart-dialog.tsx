import { useState } from 'react';
import { useTranslation } from 'react-i18next';
import { extractNumberIDAsNumber } from '@/lib/utils';
import { formatNumber } from '@/utils/format-number';
import { Dialog, DialogContent, DialogHeader, DialogTitle } from '@/components/ui/dialog';
import { Skeleton } from '@/components/ui/skeleton';
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from '@/components/ui/table';
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs';
import { useApiKeyTokenUsageStats } from '../data/apikeys';
import type { ApiKey } from '../data/schema';

type TimeRange = 'today' | 'last7days' | 'all';

const pct = (value: number, total: number) => (total > 0 ? ((value / total) * 100).toFixed(1) : '0.0');

interface ApiKeyTokenChartDialogProps {
  apiKey: ApiKey | null;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

export function ApiKeyTokenChartDialog({ apiKey, open, onOpenChange }: ApiKeyTokenChartDialogProps) {
  const { t } = useTranslation();
  const [timeRange, setTimeRange] = useState<TimeRange>('today');

  const apiKeyId = apiKey ? extractNumberIDAsNumber(apiKey.id) : null;
  const timeWindow = timeRange === 'today' ? 'day' : timeRange === 'last7days' ? 'week' : 'allTime';

  const {
    data: usageStats,
    isLoading,
    isFetching,
  } = useApiKeyTokenUsageStats(
    apiKeyId !== null && Number.isSafeInteger(apiKeyId)
      ? {
          apiKeyIds: [apiKeyId],
          timeWindow,
        }
      : undefined,
    {
      enabled: open && apiKeyId !== null && Number.isSafeInteger(apiKeyId),
    }
  );

  const stat = usageStats?.[0];
  const totalTokens = stat ? stat.totalInputTokens + stat.totalOutputTokens + stat.totalCachedTokens : 0;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className='flex max-h-[90vh] flex-col sm:max-w-2xl'>
        <DialogHeader className='flex flex-col space-y-3 sm:flex-row sm:items-center sm:justify-between sm:space-y-0'>
          <DialogTitle className='text-base sm:text-lg'>
            {t('apikeys.tokenUsageChart.title')} - {apiKey?.name}
          </DialogTitle>
          <Tabs value={timeRange} onValueChange={(value) => setTimeRange(value as TimeRange)}>
            <TabsList className='grid w-full grid-cols-3 sm:mr-6 sm:w-auto'>
              <TabsTrigger value='today'>{t('apikeys.tokenUsageChart.today')}</TabsTrigger>
              <TabsTrigger value='last7days'>{t('apikeys.tokenUsageChart.last7days')}</TabsTrigger>
              <TabsTrigger value='all'>{t('apikeys.tokenUsageChart.all')}</TabsTrigger>
            </TabsList>
          </Tabs>
        </DialogHeader>
        <div className='-ml-6 min-h-0 flex-1 scrollbar-thin space-y-2 overflow-y-auto pl-6'>
          {isLoading ? (
            <Skeleton className='h-[200px] w-full' />
          ) : !stat || totalTokens === 0 ? (
            <div className='text-muted-foreground flex h-[200px] items-center justify-center'>{t('apikeys.tokenUsageChart.noData')}</div>
          ) : (
            <div className='space-y-4' style={{ opacity: isFetching ? 0.6 : 1, transition: 'opacity 0.2s' }}>
              <div>
                <h3 className='mb-2 text-sm font-medium'>{t('apikeys.tokenUsageChart.overallUsage')}</h3>
                <div className='overflow-x-auto rounded-lg border'>
                  <Table>
                    <TableHeader>
                      <TableRow>
                        <TableHead className='w-2/5 whitespace-nowrap'>{t('apikeys.tokenUsageChart.tokenType')}</TableHead>
                        <TableHead className='w-[30%] text-center whitespace-nowrap'>{t('apikeys.tokenUsageChart.count')}</TableHead>
                        <TableHead className='w-[30%] text-center whitespace-nowrap'>{t('apikeys.tokenUsageChart.percentage')}</TableHead>
                      </TableRow>
                    </TableHeader>
                    <TableBody>
                      <TableRow>
                        <TableCell className='font-medium'>{t('apikeys.columns.inputTokens')}</TableCell>
                        <TableCell className='text-center tabular-nums'>{formatNumber(stat.totalInputTokens)}</TableCell>
                        <TableCell className='text-center tabular-nums'>{pct(stat.totalInputTokens, totalTokens)}%</TableCell>
                      </TableRow>
                      <TableRow>
                        <TableCell className='font-medium'>{t('apikeys.columns.outputTokens')}</TableCell>
                        <TableCell className='text-center tabular-nums'>{formatNumber(stat.totalOutputTokens)}</TableCell>
                        <TableCell className='text-center tabular-nums'>{pct(stat.totalOutputTokens, totalTokens)}%</TableCell>
                      </TableRow>
                      <TableRow>
                        <TableCell className='font-medium'>{t('apikeys.columns.cachedTokens')}</TableCell>
                        <TableCell className='text-center tabular-nums'>{formatNumber(stat.totalCachedTokens)}</TableCell>
                        <TableCell className='text-center tabular-nums'>{pct(stat.totalCachedTokens, totalTokens)}%</TableCell>
                      </TableRow>
                      <TableRow className='bg-muted/50 font-semibold'>
                        <TableCell>{t('apikeys.tokenUsageChart.total')}</TableCell>
                        <TableCell className='text-center tabular-nums'>{formatNumber(totalTokens)}</TableCell>
                        <TableCell className='text-center tabular-nums'>100%</TableCell>
                      </TableRow>
                    </TableBody>
                  </Table>
                </div>
              </div>
            </div>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
