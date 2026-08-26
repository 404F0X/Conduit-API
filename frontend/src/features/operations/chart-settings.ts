export const OPERATIONS_TREND_STORAGE_KEY = 'operations-trend-visible-series-v1';

export const OPERATIONS_TREND_SERIES = [
  'customerRequests',
  'upstreamAttempts',
  'retryCount',
  'recordedUpstreamCost',
  'recognizedUsageRevenue',
  'grossProfit',
  'requestFailureRate',
  'failureRate',
] as const;

export type OperationsTrendSeriesId = (typeof OPERATIONS_TREND_SERIES)[number];

export const DEFAULT_OPERATIONS_TREND_SERIES: OperationsTrendSeriesId[] = [...OPERATIONS_TREND_SERIES];

type StorageReader = Pick<Storage, 'getItem'>;

const knownSeries = new Set<string>(OPERATIONS_TREND_SERIES);

export function parseOperationsTrendSeries(rawValue: string | null): OperationsTrendSeriesId[] {
  if (rawValue == null) return [...DEFAULT_OPERATIONS_TREND_SERIES];

  try {
    const parsed: unknown = JSON.parse(rawValue);
    if (!Array.isArray(parsed) || parsed.some((value) => typeof value !== 'string')) {
      return [...DEFAULT_OPERATIONS_TREND_SERIES];
    }

    const filtered = [...new Set(parsed.filter((value): value is OperationsTrendSeriesId => knownSeries.has(value)))];
    if (parsed.length > 0 && filtered.length === 0) return [...DEFAULT_OPERATIONS_TREND_SERIES];
    return filtered;
  } catch {
    return [...DEFAULT_OPERATIONS_TREND_SERIES];
  }
}

export function loadOperationsTrendSeries(storage?: StorageReader): OperationsTrendSeriesId[] {
  if (!storage) return [...DEFAULT_OPERATIONS_TREND_SERIES];

  try {
    return parseOperationsTrendSeries(storage.getItem(OPERATIONS_TREND_STORAGE_KEY));
  } catch {
    return [...DEFAULT_OPERATIONS_TREND_SERIES];
  }
}

export function hasVisibleTrendAxis(
  visibleSeries: ReadonlySet<OperationsTrendSeriesId>,
  axisSeries: readonly OperationsTrendSeriesId[]
): boolean {
  return axisSeries.some((series) => visibleSeries.has(series));
}
