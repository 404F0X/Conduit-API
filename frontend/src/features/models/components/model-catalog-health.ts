export interface ProbePointLike {
  totalRequestCount: number;
  successRequestCount: number;
}

export interface RouteHealthLike {
  status: string;
  channelID: string;
}

export interface ChannelHealthLike {
  id: string;
  status: string;
}

export interface OperationsRouteHealthLike {
  channelId: number;
  actualModel: string;
  upstreamAttempts: number;
  successfulAttempts: number;
  failedAttempts: number;
  errorBreakdown?: Array<{ category: string; count: number }>;
}

export function getHealth(points: ProbePointLike[]) {
  const withRequests = points.filter((point) => point.totalRequestCount > 0);
  const total = withRequests.reduce((sum, point) => sum + point.totalRequestCount, 0);
  const success = withRequests.reduce((sum, point) => sum + point.successRequestCount, 0);
  if (!total) return { state: 'empty' as const, rate: null };
  const rate = success / total;
  if (rate >= 0.9) return { state: 'healthy' as const, rate };
  if (rate >= 0.5) return { state: 'warning' as const, rate };
  return { state: 'error' as const, rate };
}

export function aggregatePublicModelHealth(
  routes: RouteHealthLike[],
  channels: ChannelHealthLike[],
  probeMap: Map<string, ProbePointLike[]>
) {
  const channelsById = new Map(channels.map((channel) => [channel.id, channel]));
  const eligibleChannelIds = new Set(
    routes
      .filter((route) => route.status === 'ENABLED' && channelsById.get(route.channelID)?.status === 'enabled')
      .map((route) => route.channelID)
  );
  const points = [...eligibleChannelIds].flatMap((channelID) => probeMap.get(channelID) || []);
  return getHealth(points);
}

export function channelNumericID(id: string) {
  const value = id.match(/\/(\d+)$/)?.[1] ?? id;
  const parsed = Number(value);
  return Number.isSafeInteger(parsed) ? parsed : null;
}

export function aggregateUpstreamModelHealth(rows: OperationsRouteHealthLike[], channelID: string, actualModel: string) {
  const numericChannelID = channelNumericID(channelID);
  const matching = rows.filter((row) => row.channelId === numericChannelID && row.actualModel === actualModel);
  const attempts = matching.reduce((sum, row) => sum + row.upstreamAttempts, 0);
  const successes = matching.reduce((sum, row) => sum + row.successfulAttempts, 0);
  const failures = matching.reduce((sum, row) => sum + row.failedAttempts, 0);
  if (!attempts) return { state: 'empty' as const, rate: null, attempts, successes, failures, credentialCount: matching.length };

  const errorCounts = new Map<string, number>();
  for (const row of matching) {
    for (const error of row.errorBreakdown || []) {
      errorCounts.set(error.category, (errorCounts.get(error.category) || 0) + error.count);
    }
  }
  const rate = successes / attempts;
  const hasPermanentFailure = (errorCounts.get('auth') || 0) > 0 || (errorCounts.get('configuration') || 0) > 0;
  const hasTransientFailure = ['rate_limit', 'timeout', 'upstream_5xx', 'connection', 'canceled'].some(
    (category) => (errorCounts.get(category) || 0) > 0
  );
  const state = hasPermanentFailure || (attempts >= 3 && rate < 0.5) ? 'error' : hasTransientFailure || rate < 0.95 ? 'warning' : 'healthy';
  return { state, rate, attempts, successes, failures, credentialCount: matching.length };
}
